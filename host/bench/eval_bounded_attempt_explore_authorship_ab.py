#!/usr/bin/env python3
"""Compare explicit attempt lifecycle with the visible Stone explore library."""

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
ARMS = ("explicit", "library")
DISPATCHER = r'''
control = attempt_info()
entrypoint = get(control.metadata, "program_entrypoint", "main")
if entrypoint == "worker":
    result = worker(task_input())
else:
    result = main(task_input())
attempt_report(
    status="succeeded",
    result=result,
    metadata={"control": "bounded_explore_authorship_ab_v1"},
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
        "--library",
        type=Path,
        default=ROOT / "examples/scripts/bounded_attempt_explore.stone",
    )
    parser.add_argument(
        "--fixture",
        type=Path,
        default=(
            ROOT
            / "examples/references/bounded_attempt_explore_authorship_fixture.stone"
        ),
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/bounded-attempt-explore-authorship-ab-v1",
    )
    parser.add_argument("--author-timeout", type=float, default=600.0)
    parser.add_argument("--execution-timeout", type=float, default=180.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def common_prompt() -> str:
    return """You are testing an LLM-oriented programming language. Write only one
complete Stone function named main(input). Do not call tools or inspect files.
Return only {"source":"..."}. Do not define worker or emit top-level code.

Stone is Python-shaped and ordinary user functions accept positional or keyword
arguments. The evaluator supplies these functions:

- fixture_prepare_checkpoint() -> opaque semantic checkpoint string. It writes
  an uncommitted problem and requirement before sealing the frontier.
- fixture_candidates() -> two admitted records ordered as deserts, desserts.
- propose_lexical_first(candidates) -> {"candidate":"deserts", ...}.
- evaluate_candidate(item, outcome) -> {accepted, summary, evidence}.
- worker(input) -> a child entrypoint. It expects input={candidate, proposal,
  ordinal}, writes answer.txt, independently checks the reversal, and returns a
  successful result value even when the candidate is rejected.
- fixture_finalize(result) -> externally checks exact reject-then-accept order,
  imported answer bytes, checkpoint memory, and clean lifecycle, then returns
  the final task result.

main must prepare one checkpoint, honor the proposal, try deserts first, reject
it as an ordinary outcome, then try desserts from the same checkpoint and
accept it. It must explore sequentially, never replay preparation, and return
fixture_finalize(result). Infrastructure/controller failure must raise fail;
unsatisfied candidate evidence must not abort exploration.
"""


def arm_prompt(arm: str) -> str:
    if arm == "library":
        interface = """Use the supplied visible library exactly once:

explore(
    checkpoint,
    candidates,
    worker_entrypoint,
    evaluate,
    propose=None,
    max_candidates=4,
    timeout_ms=60000,
    context_prompt_view=None,
)

Call it with readable keyword arguments, worker_entrypoint="worker",
evaluate=evaluate_candidate, propose=propose_lexical_first, max_candidates=2,
and timeout_ms=60000. Pass its result to fixture_finalize. Do not directly call
attempt_scope, attempt_fork, attempt_join, attempt_accept, attempt_discard, or
attempt_scope_close."""
    elif arm == "explicit":
        interface = """Do not call explore or candidate. Write the explicit control:

1. Read root=attempt_info(), checkpoint=fixture_prepare_checkpoint(),
   candidates=fixture_candidates(), and proposal=propose_lexical_first(...).
2. Create attempt_scope(join_timeout_ms=60000).
3. Sequentially attempt at most two candidates, proposed name first. For each,
   call attempt_fork(checkpoint=checkpoint, program=current_program(),
   entrypoint="worker", input={candidate:item, proposal:proposal,
   ordinal:index}, start=True, scope=scope), then exactly
   attempt_join(child, timeout_ms=60000). Do not pass scope to attempt_join;
   attempt_fork already registered the child.
4. A child for which not outcome.joined or
   outcome.result.status != "succeeded" is infrastructure failure. Otherwise
   call evaluate_candidate(item, outcome). Candidate identity is item.name.
5. On rejected evidence, attempt_discard(outcome, reason=...) and append a
   bounded outcome record. On accepted evidence, call
   attempt_accept(root.attempt, outcome), append the accepted record, close the
   scope, and construct the result below. Close the scope on exhaustion too.

The result passed to fixture_finalize must contain:
{ok, status, checkpoint, proposal, winner, accepted_attempt, outcomes, tried,
clean}. Each outcomes item contains {candidate, attempt, status, summary,
evidence, result}, where result is outcome.result.value. Set tried to the
integer len(outcomes), and clean to the returned scope-cleanup clean field. Do
not parallelize or hide lifecycle in another function."""
    else:
        raise ValueError(f"unknown arm: {arm}")
    return common_prompt() + "\nExperiment arm:\n" + interface


def source_features(source: str) -> dict[str, Any]:
    return {
        "main_defs": len(re.findall(r"(?m)^def\s+main\s*\(", source)),
        "all_defs": len(re.findall(r"(?m)^def\s+", source)),
        "explore": "explore(" in source,
        "keyword_checkpoint": "checkpoint=" in source,
        "keyword_evaluate": "evaluate=" in source,
        "keyword_propose": "propose=" in source,
        "attempt_scope": "attempt_scope(" in source,
        "attempt_fork": "attempt_fork(" in source,
        "attempt_join": "attempt_join(" in source,
        "attempt_accept": "attempt_accept(" in source,
        "attempt_discard": "attempt_discard(" in source,
        "attempt_scope_close": "attempt_scope_close(" in source,
        "fixture_prepare": "fixture_prepare_checkpoint(" in source,
        "fixture_finalize": "fixture_finalize(" in source,
        "top_level_emit": bool(re.search(r"(?m)^emit\s*\(", source)),
        "lines": len(source.splitlines()),
        "bytes": len(source.encode("utf-8")),
    }


def structural_gate(arm: str, features: dict[str, Any]) -> tuple[bool, list[str]]:
    violations = []
    if features["main_defs"] != 1 or features["all_defs"] != 1:
        violations.append("source must define exactly one main function")
    for field in ("fixture_prepare", "fixture_finalize"):
        if not features[field]:
            violations.append(f"missing {field}")
    if features["top_level_emit"]:
        violations.append("source emitted top-level code")
    lifecycle = (
        "attempt_scope",
        "attempt_fork",
        "attempt_join",
        "attempt_accept",
        "attempt_discard",
        "attempt_scope_close",
    )
    if arm == "library":
        if not features["explore"]:
            violations.append("library arm did not call explore")
        for field in ("keyword_checkpoint", "keyword_evaluate", "keyword_propose"):
            if not features[field]:
                violations.append(f"library arm missing {field}")
        for field in lifecycle:
            if features[field]:
                violations.append(f"library arm used explicit {field}")
    else:
        if features["explore"]:
            violations.append("explicit arm called explore")
        for field in lifecycle:
            if not features[field]:
                violations.append(f"explicit arm missing {field}")
    return not violations, violations


def compose_source(
    arm: str,
    authored: str,
    library_source: str,
    fixture_source: str,
) -> str:
    parts = []
    if arm == "library":
        parts.append(library_source.rstrip())
    parts.extend((fixture_source.rstrip(), authored.rstrip(), DISPATCHER.strip()))
    return "\n\n".join(parts) + "\n"


def execute_source(
    args: argparse.Namespace,
    arm: str,
    authored: str,
    run_dir: Path,
) -> dict[str, Any]:
    full_source = compose_source(
        arm,
        authored,
        args.library.read_text(encoding="utf-8"),
        args.fixture.read_text(encoding="utf-8"),
    )
    full_path = run_dir / "composed.stone"
    full_path.write_text(full_source, encoding="utf-8")
    empty_library = run_dir / "empty-library.stone"
    empty_library.write_text("", encoding="utf-8")
    command = [
        sys.executable,
        str(ROOT / "host/bench/smoke_bounded_attempt_explore.py"),
        "--waymark-bin",
        str(args.waymark_bin),
        "--gateway-bin",
        str(args.gateway_bin),
        "--library",
        str(empty_library),
        "--specialization",
        str(full_path),
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
        "ok": completed.returncode == 0 and isinstance(payload, dict) and payload.get("ok"),
        "exit_code": completed.returncode,
        "timed_out": timed_out,
        "duration_seconds": duration,
        "payload": payload,
        "stderr": completed.stderr[-8192:],
    }


def repair_prompt(original: str, source: str, result: dict[str, Any]) -> str:
    diagnostic = {
        "structural_violations": result.get("structural_violations") or [],
        "execution": result.get("execution"),
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
    for field in ("waymark_bin", "gateway_bin", "library", "fixture"):
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

    prompts = {arm: arm_prompt(arm) for arm in ARMS}
    results: dict[str, list[dict[str, Any]]] = {arm: [] for arm in ARMS}
    for trial in range(1, args.trials + 1):
        for arm in ARMS:
            run_dir = run_root / f"trial-{trial}" / arm
            result = evaluate_once(args, arm=arm, prompt=prompts[arm], run_dir=run_dir)
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
    explicit = arms["explicit"]
    library = arms["library"]
    success_noninferior = (
        library["eventual_passes"] >= explicit["eventual_passes"]
        and library["first_response_passes"] >= explicit["first_response_passes"]
    )
    source_reduction = (
        library["mean_source_bytes"] is not None
        and explicit["mean_source_bytes"] is not None
        and library["mean_source_bytes"] <= explicit["mean_source_bytes"] * 0.7
    )
    repair_noninferior = library["repair_attempts"] <= explicit["repair_attempts"]
    complete = all(
        cell.get("codex_exit_code") == 0
        and cell.get("output_error") is None
        and cell.get("codex_tool_calls") == 0
        for arm_results in results.values()
        for original in arm_results
        for cell in [original, *(original.get("repairs") or [])]
    )
    aggregate = {
        "schema": "waymark.bounded-attempt-explore-authorship-ab.v1",
        "complete": complete,
        "model": args.model,
        "reasoning_effort": args.reasoning_effort,
        "trials": args.trials,
        "max_repairs": args.max_repairs,
        "arms": arms,
        "success_noninferior": success_noninferior,
        "source_reduction_at_least_30_percent": source_reduction,
        "repair_noninferior": repair_noninferior,
        "hypothesis_supported": (
            complete and success_noninferior and source_reduction and repair_noninferior
        ),
        "artifacts": {
            "driver": {
                "path": str(Path(__file__).resolve()),
                "sha256": sha256_file(Path(__file__).resolve()),
            },
            "library": {
                "path": str(args.library),
                "sha256": sha256_file(args.library),
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
