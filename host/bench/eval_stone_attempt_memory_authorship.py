#!/usr/bin/env python3
"""Test whether an outer model can author the restart-aware Stone memory loop."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base
import eval_stone_attempt_memory_restart as restart


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
DEFAULT_AUTHOR_MODELS = ["gpt-5.6-terra"]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", action="append", dest="models")
    parser.add_argument("--inner-model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--author-timeout", type=float, default=600.0)
    parser.add_argument("--execution-timeout", type=float, default=900.0)
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-attempt-memory-authorship-v1",
    )
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def help_bundle(waymark_bin: Path) -> dict[str, dict[str, Any]]:
    names = [
        "model_call",
        "run",
        "context_write",
        "context_read",
        "context_project",
        "read_file",
        "write_file",
        "emit",
        "attempt_info",
    ]
    return {name: base.compact_help(base.help_topic(waymark_bin, name)) for name in names}


def authorship_prompt(help_topics: dict[str, dict[str, Any]]) -> str:
    help_json = json.dumps(help_topics, separators=(",", ":"), sort_keys=True)
    return f"""You are the outer coding agent. Write one complete Stone program for a restart-aware attempt agent. Do not call tools, inspect files, or execute code. Return only the required JSON object containing the source.

Stone is a Python-shaped structured shell language with assignments, functions, return, if/elif/else, try/except, for/range, lists, records, field/index access, and ordinary calls. It has no imports, classes, Python standard library, or native model tool calling. `json_loads` parses text, `json_dumps` serializes values, and `fail(message, code=...)` aborts. `emit` publishes a result but does not terminate top-level execution; use mutually exclusive branches or return from a function when later effects must not run. Model message role/content fields must be strings.

Live help:
{help_json}

The evaluator starts this exact source twice in each of three independent Gateway attempts. It never edits or repairs the source. Relative file operations run at the transactional workspace root; `run` sees that workspace at `/app`.

Gateway lifecycle and workspace fixtures:
- `attempt_info()` returns the authoritative typed `controller_run_count`, `controller_restarted`, and `controller_phase`; run count 1 is the initial controller and run count 2 is its restart.
- `mode.txt` is exactly N, T, or M plus a newline.
- `early_requirement.json` contains an opaque `action_token` unknown while authoring.

Program contract:
1. Define named transition-hook functions. Every dynamic effect after restart must use both a pre and post hook. Hooks receive one transition record. Do not invoke `model_call` or `run` inside a hook.
2. Call `attempt_info()` once and use its typed lifecycle fields to choose two mutually exclusive branches. Do not create or infer restart state from a workspace marker. The initial branch must not fall through into restart effects; remember that `emit` does not terminate execution.
3. First controller: call `run(["cat", "/app/early_requirement.json"])` with a post hook that parses the successful stdout. In M, write exactly one verified item under `requirement.pivot_action`; in T, persist a raw JSON transcript containing the requirement followed by at least 40 irrelevant historical messages; in N, persist neither semantic memory nor the requirement. T and N must make no context writes. Emit exactly a record with phase="seed", mode, early_transition_id, and retained. `retained` is the one requirement read in M and [] otherwise. Make no model call in this controller.
4. Restarted controller: build a bounded recent message list with a system message and at least 12 distractors that do not contain the opaque token. Use exactly `for turn in range(2)` and exactly one `model_call` per turn. Require JSON with one string `selected_action`. Turn 0 must request `diagnostic_probe`. Turn 1 must request the verified opaque action or `insufficient_evidence` if absent.
5. The model pre hook is mode-aware: M calls `context_project` with at most 256 tokens and appends projection.text to this call; T reads and injects the entire persisted raw transcript; N injects nothing. The model post hook writes `decision.last` only in M, replacing it on turn 1. Use the same pre/post hooks in all modes so every model transition exposes start/pre/effect/post.
6. After turn 0, execute exactly one deliberately failed tool action with argv `["sh", "-c", "printf 'diagnostic failed; do not retry\\n' >&2; exit 7"]`. Append its bounded outcome to recent messages. After turn 1, execute `["printf", selected_action]`. Both calls use one action pre hook and one outcome post hook. The pre hook rejects empty argv only. The post hook writes `outcome.last_tool` only in M, replacing it after the final action. T and N make no context writes.
7. In M there must therefore be exactly five memory revisions and three current keys: `requirement.pivot_action`, `decision.last`, and `outcome.last_tool`. Do not make any other context write. At finish, set retained to `context_read(limit=8)` in M and [] otherwise.
8. Emit exactly these result fields after turn 1: phase="restore", mode, decisions, selected_action, materialized_action, failed_probe={{ok,exit_code,stderr}}, model_transition_ids, action_transition_ids, provider, model, usage, retained. The two transition-ID lists each contain the corresponding two IDs. `usage` contains both model usage records.
9. Keep all behavior visible and bounded. Do not hard-code an opaque action value, use files as memory in M, replay a transcript in M, call task_spec/task_input/react_control, report a final attempt result, or use global hooks. Include a top-level execution path and `emit` in both phases.

The evaluator requires N to refuse the hidden action after restart, while T and M must recover it. It checks every transition phase, the failed outcome, exact memory revision/current keys, prompt tokens, controller run count, rollback, and absence of leaked transactions.
"""


def source_features(source: str) -> dict[str, bool]:
    return {
        "model_call": "model_call(" in source,
        "run": "run(" in source,
        "context_write": "context_write(" in source,
        "context_read": "context_read(" in source,
        "context_project": "context_project(" in source,
        "model_and_action_hooks": source.count("hooks=") >= 3
        and '"pre"' in source
        and '"post"' in source,
        "two_turn_loop": bool(re.search(r"for\s+\w+\s+in\s+range\(2\)", source)),
        "attempt_lifecycle": "attempt_info(" in source
        and (
            "controller_run_count" in source
            or "controller_restarted" in source
            or "controller_phase" in source
        ),
        "workspace_restart_marker": "restart.marker" in source,
        # The behavioral gate verifies actual transcript recovery. Do not make
        # this cheap source screen depend on one filename such as
        # raw_transcript.json; raw_requirement_transcript.json is equivalent.
        "raw_transcript": "transcript" in source.lower(),
        "mode_fixture": "mode.txt" in source,
        "early_fixture": "early_requirement.json" in source,
        "required_keys": all(
            key in source
            for key in (
                "requirement.pivot_action",
                "decision.last",
                "outcome.last_tool",
            )
        ),
        "result_shape": all(
            key in source
            for key in (
                "early_transition_id",
                "model_transition_ids",
                "action_transition_ids",
                "materialized_action",
                "failed_probe",
            )
        ),
        "forbidden_import": bool(re.search(r"(?m)^\s*(import|from)\s+", source)),
        "hidden_control": any(
            name in source for name in ("react_control(", "task_spec(", "task_input(")
        ),
        "leaked_action_token": restart.ACTION_TOKEN in source,
    }


def required_features_ok(features: dict[str, bool]) -> bool:
    required = (
        "model_call",
        "run",
        "context_write",
        "context_read",
        "context_project",
        "model_and_action_hooks",
        "two_turn_loop",
        "attempt_lifecycle",
        "raw_transcript",
        "mode_fixture",
        "early_fixture",
        "required_keys",
        "result_shape",
    )
    return (
        all(features.get(name) for name in required)
        and not features.get("forbidden_import")
        and not features.get("hidden_control")
        and not features.get("leaked_action_token")
        and not features.get("workspace_restart_marker")
    )


def preflight_source(waymark_bin: Path, source: str, run_dir: Path) -> dict[str, Any]:
    preflight_dir = run_dir / "parse-preflight"
    preflight_dir.mkdir(parents=True, exist_ok=True)
    (preflight_dir / "mode.txt").write_text("N\n", encoding="utf-8")
    env = dict(os.environ)
    for key in list(env):
        if key.startswith("WAYMARK_GATEWAY_") or key in ("WM_GW", "WAYMARK_BOOT_TOKEN"):
            env.pop(key)
    env["WAYMARK_START_DIR"] = str(preflight_dir)
    completed = base.run_capture(
        [str(waymark_bin), "eval", "-c", source],
        cwd=preflight_dir,
        env=env,
        timeout=30,
    )
    payload = base.response_payload(completed)
    error = payload.get("error") if isinstance(payload, dict) else None
    code = error.get("code") if isinstance(error, dict) else None
    detail = error.get("detail", "") if isinstance(error, dict) else ""
    reached_model = code == "model_unavailable"
    reached_gateway_boundary = code == "stone_script_error" and (
        "Gateway runtime config is not active" in detail
    )
    return {
        "ok": reached_model or reached_gateway_boundary,
        "reached_model_call": reached_model,
        "reached_gateway_boundary": reached_gateway_boundary,
        "error_code": code,
        "exit_code": completed.returncode,
        "response": payload,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }


def result_gate(result: dict[str, Any]) -> tuple[bool, list[str]]:
    violations: list[str] = []
    if result.get("codex_exit_code") != 0:
        violations.append("author model invocation failed")
    if result.get("output_error") is not None:
        violations.append("author response did not contain a source string")
    if result.get("codex_tool_calls") != 0:
        violations.append("author model used tools")
    if not result.get("required_features_ok"):
        violations.append("generated source lacks required visible features")
    if not (result.get("preflight") or {}).get("ok"):
        violations.append("generated source did not parse and reach model_call")
    behavior = result.get("restart_execution")
    if not isinstance(behavior, dict) or behavior.get("ok") is not True:
        violations.append("unedited source failed the restart behavioral gate")
    return not violations, violations


def evaluate_model(
    args: argparse.Namespace,
    model: str,
    prompt: str,
    run_dir: Path,
) -> dict[str, Any]:
    run_dir.mkdir(parents=True, exist_ok=args.overwrite or args.reuse_existing)
    (run_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_dir / "output-schema.json").write_text(
        json.dumps(base.output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = base.codex_command(args, model, run_dir, prompt)
    started = time.monotonic()
    if args.reuse_existing and (run_dir / "last-message.json").is_file():
        completed = subprocess.CompletedProcess(
            command,
            0,
            (run_dir / "codex.stdout.jsonl").read_text(encoding="utf-8")
            if (run_dir / "codex.stdout.jsonl").is_file()
            else "",
            (run_dir / "codex.stderr").read_text(encoding="utf-8")
            if (run_dir / "codex.stderr").is_file()
            else "",
        )
        timed_out = False
    else:
        try:
            completed = base.run_capture(command, cwd=run_dir, timeout=args.author_timeout)
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
    source_path = run_dir / "agent.stone"
    if source is not None:
        source_path.write_text(source, encoding="utf-8")
    features = source_features(source or "")
    features_ok = required_features_ok(features)
    preflight = preflight_source(args.waymark_bin, source, run_dir) if source else None
    execution = None
    execution_error = None
    if source and features_ok and preflight and preflight.get("ok"):
        behavior_args = argparse.Namespace(
            waymark_bin=args.waymark_bin,
            gateway_bin=args.gateway_bin,
            source=source_path,
            auth_json=args.auth_json,
            model=args.inner_model,
            reasoning_effort=args.reasoning_effort,
            image=args.image,
            timeout=args.execution_timeout,
            run_dir=run_dir / "restart-exec",
            overwrite=args.overwrite,
        )
        try:
            execution = restart.run_experiment(
                behavior_args,
                enforce_frozen_source=False,
            )
        except Exception as error:  # The summary must preserve an unedited-source failure.
            execution_error = str(error)
    result = {
        "schema": "waymark.stone-attempt-memory-authorship-result.v1",
        "model": model,
        "inner_model": args.inner_model,
        "command": command,
        "codex_exit_code": completed.returncode,
        "codex_tool_calls": base.tool_call_count(completed.stdout),
        "timed_out": timed_out,
        "reused_existing": args.reuse_existing,
        "duration_seconds": duration,
        "output_error": output_error,
        "source_bytes": len((source or "").encode("utf-8")),
        "features": features,
        "required_features_ok": features_ok,
        "preflight": preflight,
        "restart_execution": execution,
        "restart_execution_error": execution_error,
        "unedited": True,
    }
    ok, violations = result_gate(result)
    result["ok"] = ok
    result["status"] = "pass" if ok else "fail"
    result["gate_violations"] = violations
    (run_dir / "summary.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return result


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.auth_json = args.auth_json.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("Codex auth", args.auth_json),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not (
        args.overwrite or args.reuse_existing
    ):
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    if run_root.exists() and args.overwrite:
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)
    prompt = authorship_prompt(help_bundle(args.waymark_bin))
    models = args.models or DEFAULT_AUTHOR_MODELS
    results = [
        evaluate_model(args, model, prompt, run_root / base.safe_model_dir(model))
        for model in models
    ]
    aggregate = {
        "schema": "waymark.stone-attempt-memory-authorship-aggregate.v1",
        "ok": all(result["ok"] for result in results),
        "run_root": str(run_root),
        "author_models": models,
        "inner_model": args.inner_model,
        "results": results,
        "passed": sum(bool(result["ok"]) for result in results),
        "failed": sum(not bool(result["ok"]) for result in results),
        "total": len(results),
    }
    (run_root / "aggregate.json").write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        json.dumps(
            {
                "ok": aggregate["ok"],
                "passed": aggregate["passed"],
                "failed": aggregate["failed"],
                "total": aggregate["total"],
                "results": [
                    {
                        "model": result["model"],
                        "ok": result["ok"],
                        "source_bytes": result["source_bytes"],
                        "gate_violations": result["gate_violations"],
                        "restart_execution_error": result["restart_execution_error"],
                        "metrics": (result.get("restart_execution") or {}).get("metrics"),
                    }
                    for result in results
                ],
                "aggregate": str(run_root / "aggregate.json"),
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0 if aggregate["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
