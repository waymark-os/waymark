#!/usr/bin/env python3
"""Compare raw fork/repair ABI with Stone's typed semantic-frontier interface."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base
import eval_stone_stage_syntax_ab as stage_ab


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
ARMS = ("raw", "typed")
DISPATCHER = r'''
control = attempt_info()
entrypoint = get(control.metadata, "program_entrypoint", "main")
if entrypoint == "worker":
    result = worker(task_input())
elif entrypoint == "setup_owner":
    result = setup_owner(task_input())
else:
    result = main(task_input())
attempt_report(
    status="succeeded",
    result=result,
    metadata={"control": "semantic_frontier_authorship_ab_v1"},
)
emit(result)
'''


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--max-repairs", type=int, choices=(0, 1), default=1)
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--fixture",
        type=Path,
        default=(
            ROOT
            / "examples/references/semantic_frontier_authorship_fixture.stone"
        ),
    )
    parser.add_argument(
        "--raw-reference",
        type=Path,
        default=(
            ROOT
            / "examples/references/semantic_frontier_authorship_raw_reference.stone"
        ),
    )
    parser.add_argument(
        "--typed-reference",
        type=Path,
        default=(
            ROOT
            / "examples/references/semantic_frontier_authorship_typed_reference.stone"
        ),
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/semantic-frontier-authorship-ab-v1",
    )
    parser.add_argument("--author-timeout", type=float, default=600.0)
    parser.add_argument("--execution-timeout", type=float, default=180.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    parser.add_argument("--reference-smoke", action="store_true")
    return parser.parse_args()


def compact_help(topic: dict[str, Any]) -> dict[str, Any]:
    return {
        "signature": topic.get("signature"),
        "use_when": topic.get("use_when"),
        "avoid": topic.get("avoid"),
        "examples": topic.get("examples"),
    }


def common_prompt() -> str:
    return """You are testing an LLM-oriented programming language. Write only one
complete Stone function named main(input). Do not call tools or inspect files.
Return only {"source":"..."}. Do not define worker/setup functions and do not
emit top-level code.

Stone is Python-shaped. The evaluator supplies:

- fixture_prepare_checkpoint() -> one full forkable workflow checkpoint record.
- worker(input) -> child entrypoint; input has {label, path, expected}.
- setup_owner(input) -> child entrypoint returning
  {checkpoint: one full repairable workflow checkpoint record}.
- fixture_finalize(interface, parent_source, retained_source, parent_outcome,
  repaired_outcome, parent_child, owner, repaired, cleanup) -> final verified
  result. For the raw arm, sources are checkpoint records. For the typed arm,
  sources are semantic_frontier values. parent_child, owner, and repaired must
  remain attempt_handle values; attempt_accept(...) returns a distinct
  attempt_acceptance transition result.

main must:

1. Set checkpoint = fixture_prepare_checkpoint() exactly once and
   root = attempt_info() exactly once. checkpoint is a workflow checkpoint;
   root is the current attempt record with attempt, task, and workspace fields.
2. Create one attempt_scope(join_timeout_ms=30000).
3. Branch from the root checkpoint to worker with
   {label:"parent-branch", path:"root.txt", expected:"root"}, join it with a
   30000 ms timeout, require joined and succeeded, then discard it.
4. Branch from the same root checkpoint to setup_owner, join it, require joined
   and succeeded, save its returned repairable checkpoint, then call
   attempt_finish("fail", attempt=owner, reason=...) to inject a late failure.
5. Branch from that retained checkpoint to worker with
   {label:"retained-branch", path:"owner.txt", expected:"owner"}, join it, and
   require joined and succeeded.
6. Accept the repaired child into the root, close the scope, and return
   fixture_finalize(...) with every argument in its documented order.

All three started children must use program=current_program(), the named
entrypoint, start=True, and scope=scope. Pass scope only while creating the
child, never to attempt_join. Infrastructure failure must call fail with a
short code. Do not create workflows/checkpoints, call the preparation twice,
parallelize, retry, or hide lifecycle in another function.
"""


def arm_prompt(arm: str, help_topics: dict[str, dict[str, Any]]) -> str:
    if arm == "typed":
        interface = """Use only the typed frontier interface for all three branches.

- parent_source = semantic_frontier(checkpoint)
- attempt_branch(parent_source, ...)
- retained_source = semantic_frontier(repair_checkpoint, owner=owner)
- attempt_branch(retained_source, ...)

Do not call attempt_fork, attempt_spawn, or construct workspace_source records.
Pass "typed" as fixture_finalize's interface argument."""
        help_names = ("semantic_frontier", "attempt_branch")
    elif arm == "raw":
        interface = """Use only the raw kernel-shaped interface.

- Both root children use attempt_fork(checkpoint=checkpoint.reference, ...).
- The retained child uses attempt_spawn(
    task=root.task,
    workspace=root.workspace,
    workspace_source={
        "kind":"repair-checkpoint",
        "workspace":root.workspace,
        "attempt":owner.attempt,
        "checkpoint":repair_checkpoint.reference,
    },
    ...,
  ).

Do not call semantic_frontier or attempt_branch. Pass "raw" as
fixture_finalize's interface argument."""
        help_names = ("attempt_fork", "attempt_spawn")
    else:
        raise ValueError(f"unknown arm: {arm}")
    help_view = {
        name: compact_help(help_topics[name])
        for name in (
            *help_names,
            "attempt_scope",
            "attempt_info",
            "attempt_join",
            "attempt_accept",
            "attempt_discard",
            "attempt_finish",
            "attempt_scope_close",
        )
    }
    return (
        common_prompt()
        + "\nExperiment arm:\n"
        + interface
        + "\n\nLive Stone help:\n"
        + json.dumps(help_view, separators=(",", ":"), sort_keys=True)
    )


def source_features(source: str) -> dict[str, Any]:
    count = lambda pattern: len(re.findall(pattern, source))
    checkpoint_declarations = count(r"""checkpoint\s*=\s*["']""")
    prepare_calls = source.count("fixture_prepare_checkpoint(")
    return {
        "main_defs": count(r"(?m)^def\s+main\s*\("),
        "all_defs": count(r"(?m)^def\s+"),
        "fixture_prepare_calls": prepare_calls,
        "redundant_seal_requests": max(0, prepare_calls - 1) + checkpoint_declarations,
        "checkpoint_declarations": checkpoint_declarations,
        "semantic_frontier": source.count("semantic_frontier("),
        "attempt_branch": source.count("attempt_branch("),
        "attempt_fork": source.count("attempt_fork("),
        "attempt_spawn": source.count("attempt_spawn("),
        "workspace_source": source.count("workspace_source"),
        "repair_checkpoint_literal": source.count('"repair-checkpoint"')
        + source.count("'repair-checkpoint'"),
        "attempt_scope": source.count("attempt_scope("),
        "attempt_join": source.count("attempt_join("),
        "attempt_accept": source.count("attempt_accept("),
        "attempt_discard": source.count("attempt_discard("),
        "attempt_finish": source.count("attempt_finish("),
        "attempt_info": source.count("attempt_info("),
        "attempt_scope_close": source.count("attempt_scope_close("),
        "fixture_finalize": source.count("fixture_finalize("),
        "current_program": source.count("current_program()"),
        "top_level_emit": bool(re.search(r"(?m)^emit\s*\(", source)),
        "lines": len(source.splitlines()),
        "bytes": len(source.encode("utf-8")),
    }


def structural_gate(arm: str, features: dict[str, Any]) -> tuple[bool, list[str]]:
    violations = []
    if features["main_defs"] != 1 or features["all_defs"] != 1:
        violations.append("source must define exactly one main function")
    if features["fixture_prepare_calls"] != 1:
        violations.append("fixture_prepare_checkpoint must be called exactly once")
    if features["checkpoint_declarations"] != 0:
        violations.append("main declared an unauthorized redundant checkpoint")
    if features["top_level_emit"]:
        violations.append("source emitted top-level code")
    for field, expected in (
        ("attempt_scope", 1),
        ("attempt_join", 3),
        ("attempt_accept", 1),
        ("attempt_discard", 1),
        ("attempt_finish", 1),
        ("attempt_info", 1),
        ("attempt_scope_close", 1),
        ("fixture_finalize", 1),
        ("current_program", 3),
    ):
        if features[field] != expected:
            violations.append(f"{field} count={features[field]}, expected {expected}")
    if arm == "typed":
        for field, expected in (("semantic_frontier", 2), ("attempt_branch", 3)):
            if features[field] != expected:
                violations.append(f"typed arm {field} count={features[field]}, expected {expected}")
        for field in ("attempt_fork", "attempt_spawn", "workspace_source"):
            if features[field] != 0:
                violations.append(f"typed arm used raw {field}")
    else:
        for field, expected in (
            ("attempt_fork", 2),
            ("attempt_spawn", 1),
            ("workspace_source", 1),
            ("repair_checkpoint_literal", 1),
        ):
            if features[field] != expected:
                violations.append(f"raw arm {field} count={features[field]}, expected {expected}")
        for field in ("semantic_frontier", "attempt_branch"):
            if features[field] != 0:
                violations.append(f"raw arm used typed {field}")
    return not violations, violations


def compose_source(authored: str, fixture_source: str) -> str:
    return "\n\n".join(
        (fixture_source.rstrip(), authored.rstrip(), DISPATCHER.strip())
    ) + "\n"


def execute_source(
    args: argparse.Namespace,
    arm: str,
    authored: str,
    run_dir: Path,
) -> dict[str, Any]:
    full_source = compose_source(
        authored,
        args.fixture.read_text(encoding="utf-8"),
    )
    full_path = run_dir / "composed.stone"
    full_path.write_text(full_source, encoding="utf-8")
    command = [
        sys.executable,
        str(ROOT / "host/bench/smoke_semantic_frontier.py"),
        "--waymark-bin",
        str(args.waymark_bin),
        "--gateway-bin",
        str(args.gateway_bin),
        "--source",
        str(full_path),
        "--expected-interface",
        arm,
        "--timeout",
        str(args.execution_timeout),
    ]
    started = time.monotonic()
    try:
        completed = base.run_capture(
            command,
            cwd=run_dir,
            timeout=args.execution_timeout + 60,
        )
        timed_out = False
    except subprocess.TimeoutExpired as error:
        completed = subprocess.CompletedProcess(
            command,
            124,
            error.stdout or "",
            error.stderr or "",
        )
        timed_out = True
    duration = time.monotonic() - started
    (run_dir / "execution.stdout").write_text(completed.stdout, encoding="utf-8")
    (run_dir / "execution.stderr").write_text(completed.stderr, encoding="utf-8")
    payload = None
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError:
        pass
    return {
        "ok": completed.returncode == 0
        and isinstance(payload, dict)
        and payload.get("ok"),
        "exit_code": completed.returncode,
        "timed_out": timed_out,
        "duration_seconds": duration,
        "payload": payload,
        "stderr": completed.stderr[-8192:],
    }


def repair_prompt(original: str, source: str, result: dict[str, Any]) -> str:
    execution = result.get("execution") or {}
    diagnostic = {
        "structural_violations": result.get("structural_violations") or [],
        "execution": {
            "ok": execution.get("ok"),
            "exit_code": execution.get("exit_code"),
            "timed_out": execution.get("timed_out"),
            "stderr": execution.get("stderr"),
        }
        if execution
        else None,
    }
    return original + f"""

One bounded repair is allowed. Return the complete corrected main(input) in the
same experiment arm. Do not call tools. Preserve every requirement.

Diagnostic:
{json.dumps(diagnostic, separators=(",", ":"), sort_keys=True)}

Previous source:
```stone
{source}
```
"""


def evaluate_once(
    args: argparse.Namespace,
    *,
    arm: str,
    prompt: str,
    run_dir: Path,
) -> dict[str, Any]:
    run_dir.mkdir(parents=True, exist_ok=args.overwrite or args.reuse_existing)
    summary_path = run_dir / "summary.json"
    if args.reuse_existing and summary_path.is_file():
        return json.loads(summary_path.read_text(encoding="utf-8"))
    (run_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_dir / "output-schema.json").write_text(
        json.dumps(stage_ab.output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = stage_ab.codex_command(args, run_dir, prompt)
    started = time.monotonic()
    try:
        completed = base.run_capture(
            command,
            cwd=run_dir,
            timeout=args.author_timeout,
        )
        timed_out = False
    except subprocess.TimeoutExpired as error:
        completed = subprocess.CompletedProcess(
            command,
            124,
            error.stdout or "",
            error.stderr or "",
        )
        timed_out = True
    duration = time.monotonic() - started
    (run_dir / "codex.stdout.jsonl").write_text(completed.stdout, encoding="utf-8")
    (run_dir / "codex.stderr").write_text(completed.stderr, encoding="utf-8")
    source, output_error = base.parse_last_message(run_dir / "last-message.json")
    source_path = run_dir / "main.stone"
    if source is not None:
        source_path.write_text(source.rstrip() + "\n", encoding="utf-8")
    features = source_features(source or "")
    structural_ok, structural_violations = structural_gate(arm, features)
    execution = (
        execute_source(args, arm, source, run_dir)
        if source is not None and structural_ok
        else None
    )
    ok = (
        completed.returncode == 0
        and output_error is None
        and base.tool_call_count(completed.stdout) == 0
        and structural_ok
        and bool(execution and execution.get("ok"))
    )
    result = {
        "ok": ok,
        "arm": arm,
        "model": args.model,
        "command": command,
        "codex_exit_code": completed.returncode,
        "codex_tool_calls": base.tool_call_count(completed.stdout),
        "timed_out": timed_out,
        "duration_seconds": duration,
        "output_error": output_error,
        "features": features,
        "structural_ok": structural_ok,
        "structural_violations": structural_violations,
        "execution": execution,
        "usage": stage_ab.codex_usage(completed.stdout),
    }
    summary_path.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return result


def final_result(result: dict[str, Any]) -> dict[str, Any]:
    repairs = result.get("repairs") or []
    if result.get("ok") is not True and repairs:
        return repairs[-1]
    return result


def summarize_arm(results: list[dict[str, Any]]) -> dict[str, Any]:
    finals = [final_result(result) for result in results]
    passing = [result for result in finals if result.get("ok")]
    return {
        "trials": len(results),
        "first_response_passes": sum(result.get("ok") is True for result in results),
        "eventual_passes": len(passing),
        "repair_attempts": sum(len(result.get("repairs") or []) for result in results),
        "redundant_seal_requests": sum(
            result["features"]["redundant_seal_requests"] for result in finals
        ),
        "mean_source_bytes": (
            sum(result["features"]["bytes"] for result in passing) / len(passing)
            if passing
            else None
        ),
        "mean_source_lines": (
            sum(result["features"]["lines"] for result in passing) / len(passing)
            if passing
            else None
        ),
    }


def main() -> int:
    args = parse_args()
    if args.trials < 1 or args.trials > 10:
        raise SystemExit("--trials must be between 1 and 10")
    for field in (
        "waymark_bin",
        "gateway_bin",
        "fixture",
        "raw_reference",
        "typed_reference",
    ):
        path = getattr(args, field).resolve()
        setattr(args, field, path)
        if not path.is_file():
            raise SystemExit(f"{field.replace('_', ' ')} not found: {path}")
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not (
        args.overwrite or args.reuse_existing
    ):
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    if run_root.exists() and args.overwrite:
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)

    if args.reference_smoke:
        references = {
            "raw": args.raw_reference,
            "typed": args.typed_reference,
        }
        results = {}
        for arm, path in references.items():
            source = path.read_text(encoding="utf-8")
            features = source_features(source)
            structural_ok, violations = structural_gate(arm, features)
            run_dir = run_root / "reference" / arm
            run_dir.mkdir(parents=True, exist_ok=True)
            execution = (
                execute_source(args, arm, source, run_dir)
                if structural_ok
                else None
            )
            results[arm] = {
                "ok": structural_ok and bool(execution and execution.get("ok")),
                "features": features,
                "structural_violations": violations,
                "execution": execution,
            }
        aggregate = {
            "schema": "waymark.semantic-frontier-authorship-reference-smoke.v1",
            "complete": all(result["ok"] for result in results.values()),
            "arms": results,
            "run_root": str(run_root),
        }
        print(json.dumps(aggregate, indent=2, sort_keys=True))
        return 0 if aggregate["complete"] else 1

    help_names = (
        "semantic_frontier",
        "attempt_branch",
        "attempt_fork",
        "attempt_spawn",
        "attempt_scope",
        "attempt_info",
        "attempt_join",
        "attempt_accept",
        "attempt_discard",
        "attempt_finish",
        "attempt_scope_close",
    )
    help_topics = {
        name: base.help_topic(args.waymark_bin, name) for name in help_names
    }
    prompts = {arm: arm_prompt(arm, help_topics) for arm in ARMS}
    results: dict[str, list[dict[str, Any]]] = {arm: [] for arm in ARMS}
    for trial in range(1, args.trials + 1):
        for arm in ARMS:
            run_dir = run_root / f"trial-{trial}" / arm
            result = evaluate_once(
                args,
                arm=arm,
                prompt=prompts[arm],
                run_dir=run_dir,
            )
            result["repairs"] = []
            if result.get("ok") is not True and args.max_repairs == 1:
                source_path = run_dir / "main.stone"
                if source_path.is_file():
                    repaired = evaluate_once(
                        args,
                        arm=arm,
                        prompt=repair_prompt(
                            prompts[arm],
                            source_path.read_text(encoding="utf-8"),
                            result,
                        ),
                        run_dir=run_dir / "repair-1",
                    )
                    result["repairs"].append(repaired)
                    (run_dir / "summary.json").write_text(
                        json.dumps(result, indent=2, sort_keys=True) + "\n",
                        encoding="utf-8",
                    )
            results[arm].append(result)

    arms = {arm: summarize_arm(results[arm]) for arm in ARMS}
    raw = arms["raw"]
    typed = arms["typed"]
    complete = all(
        cell.get("codex_exit_code") == 0
        and cell.get("output_error") is None
        and cell.get("codex_tool_calls") == 0
        for arm_results in results.values()
        for original in arm_results
        for cell in [original, *(original.get("repairs") or [])]
    )
    success_noninferior = (
        typed["eventual_passes"] >= raw["eventual_passes"]
        and typed["first_response_passes"] >= raw["first_response_passes"]
    )
    repair_noninferior = typed["repair_attempts"] <= raw["repair_attempts"]
    seal_noninferior = (
        typed["redundant_seal_requests"] <= raw["redundant_seal_requests"]
    )
    source_reduction = (
        typed["mean_source_bytes"] is not None
        and raw["mean_source_bytes"] is not None
        and typed["mean_source_bytes"] <= raw["mean_source_bytes"] * 0.85
    )
    aggregate = {
        "schema": "waymark.semantic-frontier-authorship-ab.v1",
        "complete": complete,
        "model": args.model,
        "reasoning_effort": args.reasoning_effort,
        "trials": args.trials,
        "max_repairs": args.max_repairs,
        "arms": arms,
        "success_noninferior": success_noninferior,
        "repair_noninferior": repair_noninferior,
        "redundant_seals_noninferior": seal_noninferior,
        "source_reduction_at_least_15_percent": source_reduction,
        "hypothesis_supported": (
            complete
            and success_noninferior
            and repair_noninferior
            and seal_noninferior
            and source_reduction
        ),
        "artifacts": {
            "driver": {
                "path": str(Path(__file__).resolve()),
                "sha256": sha256_file(Path(__file__).resolve()),
            },
            "fixture": {
                "path": str(args.fixture),
                "sha256": sha256_file(args.fixture),
            },
            "waymark_binary": {
                "path": str(args.waymark_bin),
                "sha256": sha256_file(args.waymark_bin),
            },
            "gateway_binary": {
                "path": str(args.gateway_bin),
                "sha256": sha256_file(args.gateway_bin),
            },
        },
        "run_root": str(run_root),
    }
    aggregate_path = run_root / "aggregate.json"
    aggregate_path.write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        json.dumps(
            {**aggregate, "aggregate": str(aggregate_path)},
            indent=2,
            sort_keys=True,
        )
    )
    return 0 if complete else 1


if __name__ == "__main__":
    raise SystemExit(main())
