#!/usr/bin/env python3
"""Run a model-authored standard-control verifier on a simple repository task."""

from __future__ import annotations

import argparse
import json
import shutil
from collections import Counter
from pathlib import Path
from typing import Any

import eval_standard_agent_specialization as specialization
import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
GATEWAY_ROOT = SANDBOX / "waymark-gateway"
DEFAULT_LIBRARY = ROOT / "examples/scripts/standard_attempt_agent.stone"
DEFAULT_SMOKE = GATEWAY_ROOT / "host/runner/smoke_waymark_libos_gateway_model_call.py"
INPUT_TEXT = "pear\napple\npear\nbanana\n"
EXPECTED_OUTPUT = "APPLE\nBANANA\nPEAR\n"
EXPECTED_ANSWER = "verified-transform"
OUTPUT_PATH = "output.txt"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-terra", help="Outer author model")
    parser.add_argument("--inner-model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--codex", default="codex")
    parser.add_argument(
        "--codex-auth-json",
        type=Path,
        default=Path.home() / ".codex/auth.json",
    )
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=ROOT / "target/debug/waymark",
    )
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=GATEWAY_ROOT / "target/debug/waymark-gateway",
    )
    parser.add_argument("--standard-source", type=Path, default=DEFAULT_LIBRARY)
    parser.add_argument("--smoke", type=Path, default=DEFAULT_SMOKE)
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-standard-task-specialization-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def authorship_prompt() -> str:
    expected = json.dumps(EXPECTED_OUTPUT)
    return f"""You are the outer coding agent. Return only one JSON object with a
non-empty `source` string containing a small Stone specialization. Do not call
tools or inspect files.

Stone is Python-shaped but has no imports, classes, decorators, or Python
standard library. Record fields may be read with dot access or get(...), but
record mutation requires item assignment such as checked["task_verified"] =
True; attribute assignment is unsupported. The execution harness prepends the
complete visible standard agent-control library.

Available library contract:
- session = agent_session()
- options = standard_agent_options(session.input)
- standard_agent_control(session, options, dispatch_action, verify_finish,
  record_progress) -> result
- standard_shell_dispatch(action, options)
- standard_verify_finish(candidate, session, state) -> candidate
- standard_record_progress(event)
- read_file(path, max_bytes=...) -> text
- fail(message, code=...) -> never
- emit(value)

Frozen repository task:
- `/app/input.txt` contains duplicate lowercase fruit names.
- The inner agent must create `/app/{OUTPUT_PATH}` containing sorted unique
  uppercase names with a trailing newline.
- For this frozen input the exact required bytes are {expected}.
- A valid final answer is `{EXPECTED_ANSWER}` only after those bytes exist.
- The harness independently checks the exact workspace bytes and effect trace;
  the authored verifier is defense in depth, not the authority.

Write only the specialization suffix:
1. Define a named three-argument verifier adapter.
2. Delegate first to standard_verify_finish(candidate, session, state).
3. Read `/app/{OUTPUT_PATH}` exactly once with read_file(..., max_bytes=256).
   If its bytes are not exactly {expected}, call fail with code
   `task_verification_failed`.
4. Annotate the checked candidate with task_verified=True,
   verified_bytes=len(content), and verified_round=state.rounds, then return it.
5. Construct session/options, invoke standard_agent_control with the standard
   dispatcher, your verifier passed directly as a named function, and standard
   progress, then emit the result.
6. Do not redefine the library. Do not call model_call, model_infer, context,
   attempt, run, write, or other tool operations directly. The verifier's one
   bounded read_file is the only direct resource effect.
7. Include no Markdown fence. The source must be admitted Stone, not Python.
"""


def source_features(source: str) -> dict[str, bool]:
    forbidden_effects = (
        "model_call(",
        "model_infer(",
        "context_write(",
        "context_read(",
        "attempt_",
        "run(",
        "write_text(",
        "write_file(",
    )
    return {
        "named_verifier": "def " in source and "verify" in source,
        "delegates_validation": "standard_verify_finish(" in source,
        "one_bounded_read": source.count("read_file(") == 1
        and "max_bytes=256" in source,
        "checks_output_path": f'"/app/{OUTPUT_PATH}"' in source,
        "checks_expected_bytes": '"APPLE\\nBANANA\\nPEAR\\n"' in source,
        "verification_error": "task_verification_failed" in source,
        "annotates_verified": '"task_verified"' in source,
        "annotates_bytes": '"verified_bytes"' in source,
        "annotates_round": '"verified_round"' in source,
        "uses_control": "standard_agent_control(" in source,
        "uses_standard_dispatch": "standard_shell_dispatch" in source,
        "uses_standard_progress": "standard_record_progress" in source,
        "top_level_emit": "emit(" in source,
        "no_lambda": "lambda" not in source,
        "no_library_redefinition": "def standard_agent_control(" not in source,
        "no_forbidden_effect": not any(effect in source for effect in forbidden_effects),
        "no_import": "\nimport " not in "\n" + source
        and "\nfrom " not in "\n" + source,
    }


def required_features_ok(features: dict[str, bool]) -> bool:
    return all(features.values())


def inner_task_objective() -> str:
    return (
        "Use exactly one action per model response and wait for every observation. "
        "Read /app/input.txt with the read tool. Create /app/output.txt with the "
        "unique input lines uppercased, sorted alphabetically, and terminated by "
        "one newline using the write tool. Read /app/output.txt back. Only after "
        "observing the exact required bytes, return one final action with "
        '{"answer":"verified-transform"}. Do not use run_linux.'
    )


def inner_command(
    args: argparse.Namespace,
    composed_source: Path,
    out_dir: Path,
) -> list[str]:
    return [
        "python3",
        str(args.smoke.resolve()),
        "--gateway-bin",
        str(args.gateway_bin.resolve()),
        "--provider",
        "codex-chatgpt",
        "--model",
        args.inner_model,
        "--reasoning-effort",
        args.reasoning_effort,
        "--codex-auth-json",
        str(args.codex_auth_json.resolve()),
        "--program-mode",
        "stone",
        "--stone-source-file",
        str(composed_source.resolve()),
        "--task-objective",
        inner_task_objective(),
        "--task-input-json",
        '{"max_turns":6,"max_rounds":6,"completion_critique":false}',
        "--source-input-text",
        INPUT_TEXT,
        "--expected-answer",
        EXPECTED_ANSWER,
        "--expected-model-calls",
        "4",
        "--expected-workspace-path",
        OUTPUT_PATH,
        "--expected-workspace-content",
        EXPECTED_OUTPUT,
        "--max-turns",
        "6",
        "--max-rounds",
        "6",
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def negative_command(
    args: argparse.Namespace,
    composed_source: Path,
    out_dir: Path,
) -> list[str]:
    def response(action: dict[str, Any]) -> str:
        return json.dumps({"actions": [action]}, separators=(",", ":"))

    wrong_output = "APPLE\nPEAR\n"
    sequence = [
        response({"tool": "read", "input": {"path": "/app/input.txt"}}),
        response(
            {
                "tool": "write",
                "input": {
                    "path": "/app/output.txt",
                    "content": wrong_output,
                },
            }
        ),
        response({"tool": "read", "input": {"path": "/app/output.txt"}}),
        response({"final": {"answer": EXPECTED_ANSWER}}),
    ]
    return [
        "python3",
        str(args.smoke.resolve()),
        "--gateway-bin",
        str(args.gateway_bin.resolve()),
        "--provider",
        "fixture",
        "--program-mode",
        "stone",
        "--stone-source-file",
        str(composed_source.resolve()),
        "--task-objective",
        inner_task_objective(),
        "--task-input-json",
        '{"max_turns":6,"max_rounds":6,"completion_critique":false}',
        "--source-input-text",
        INPUT_TEXT,
        "--fixture-sequence-json",
        json.dumps(sequence, separators=(",", ":")),
        "--expected-error-code",
        "task_verification_failed",
        "--expected-model-calls",
        "4",
        "--max-turns",
        "6",
        "--max-rounds",
        "6",
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def trace_metrics(out_dir: Path) -> dict[str, Any]:
    trace_path = out_dir / "gateway-data/traces/operations.jsonl"
    operations: Counter[str] = Counter()
    usage = Counter()
    if trace_path.is_file():
        for line in trace_path.read_text(encoding="utf-8").splitlines():
            value = json.loads(line)
            operation = value.get("op")
            if isinstance(operation, str):
                operations[operation] += 1
            if operation == "attempt.rpc.model.call":
                for key in ("input_tokens", "output_tokens", "total_tokens"):
                    amount = value.get(key)
                    if isinstance(amount, int):
                        usage[key] += amount
    return {
        "operation_counts": dict(sorted(operations.items())),
        "token_usage": dict(usage),
    }


def result_gate(
    summary: dict[str, Any] | None,
    metrics: dict[str, Any],
) -> tuple[bool, list[str]]:
    violations: list[str] = []
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return False, ["inner summary is missing or failed"]
    report = summary.get("controller_report")
    if not isinstance(report, dict):
        return False, ["controller_report is missing"]
    expected_report = {
        "answer": EXPECTED_ANSWER,
        "task_verified": True,
        "verified_bytes": len(EXPECTED_OUTPUT.encode("utf-8")),
        "verified_round": 4,
    }
    for key, expected in expected_report.items():
        if report.get(key) != expected:
            violations.append(
                f"controller_report.{key}={report.get(key)!r}, expected {expected!r}"
            )
    control = report.get("_control")
    expected_control = {
        "name": "stone.standard_action_v11",
        "rounds": 4,
        "actions": 4,
        "tool_calls": 3,
        "failed_tools": 0,
        "validation_retries": 0,
    }
    if not isinstance(control, dict):
        violations.append("standard control provenance is missing")
    else:
        for key, expected in expected_control.items():
            if control.get(key) != expected:
                violations.append(
                    f"_control.{key}={control.get(key)!r}, expected {expected!r}"
                )
    counts = metrics.get("operation_counts", {})
    expected_counts = {
        "attempt.rpc.model.call": 4,
        "attempt.rpc.workspace_tx.read": 3,
        "attempt.rpc.workspace_tx.write": 1,
        "attempt.memory.write": 13,
    }
    for operation, expected in expected_counts.items():
        if counts.get(operation) != expected:
            violations.append(
                f"{operation}={counts.get(operation)!r}, expected {expected}"
            )
    if counts.get("attempt.rpc.linux.exec", 0) != 0:
        violations.append("inner agent unexpectedly used Linux")
    if summary.get("expected_workspace_content") != EXPECTED_OUTPUT:
        violations.append("external workspace verifier did not observe exact bytes")
    if summary.get("rolled_back") is not True:
        violations.append("attempt did not roll back")
    return not violations, violations


def negative_gate(
    summary: dict[str, Any] | None,
    metrics: dict[str, Any],
) -> tuple[bool, list[str]]:
    violations: list[str] = []
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return False, ["negative summary is missing or failed"]
    if summary.get("controller_error_code") != "task_verification_failed":
        violations.append(
            "wrong verifier error: "
            f"{summary.get('controller_error_code')!r}"
        )
    counts = metrics.get("operation_counts", {})
    expected_counts = {
        "attempt.rpc.model.call": 4,
        "attempt.rpc.workspace_tx.read": 3,
        "attempt.rpc.workspace_tx.write": 1,
        "attempt.memory.write": 12,
    }
    for operation, expected in expected_counts.items():
        if counts.get(operation) != expected:
            violations.append(
                f"negative {operation}={counts.get(operation)!r}, expected {expected}"
            )
    if summary.get("rolled_back") is not True:
        violations.append("negative attempt did not roll back")
    return not violations, violations


def evaluate(args: argparse.Namespace) -> dict[str, Any]:
    run_root = args.run_root.resolve()
    run_dir = run_root / base.safe_model_dir(args.model)
    if run_dir.exists() and any(run_dir.iterdir()) and not (
        args.overwrite or args.reuse_existing
    ):
        raise SystemExit(f"refusing to overwrite non-empty model directory: {run_dir}")
    if args.overwrite and run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)

    completed, timed_out, duration, source, output_error = specialization.author_source(
        args,
        authorship_prompt(),
        run_dir,
    )
    features = source_features(source or "")
    preflight_result = None
    inner_exit_code = None
    inner_summary = None
    metrics: dict[str, Any] = {"operation_counts": {}, "token_usage": {}}
    violations = ["authored source unavailable"]
    negative_exit_code = None
    negative_summary = None
    negative_metrics: dict[str, Any] = {
        "operation_counts": {},
        "token_usage": {},
    }
    negative_violations = ["authored source unavailable"]
    composed = None
    if source is not None:
        (run_dir / "specialization.stone").write_text(
            source.rstrip() + "\n",
            encoding="utf-8",
        )
        prefix = specialization.library_prefix(
            args.standard_source.read_text(encoding="utf-8")
        )
        composed = prefix + "\n# Model-authored task verifier.\n" + source.rstrip() + "\n"
        composed_path = run_dir / "composed-agent.stone"
        composed_path.write_text(composed, encoding="utf-8")
        preflight_result = specialization.preflight(
            args.waymark_bin,
            composed,
            run_dir,
        )
        if required_features_ok(features) and preflight_result["ok"]:
            inner_out = run_dir / "inner-exec"
            inner_completed = base.run_capture(
                inner_command(args, composed_path, inner_out),
                cwd=GATEWAY_ROOT,
                timeout=240,
            )
            inner_exit_code = inner_completed.returncode
            (run_dir / "inner.stdout").write_text(
                inner_completed.stdout,
                encoding="utf-8",
            )
            (run_dir / "inner.stderr").write_text(
                inner_completed.stderr,
                encoding="utf-8",
            )
            summary_path = inner_out / "summary.json"
            inner_summary = (
                json.loads(summary_path.read_text(encoding="utf-8"))
                if summary_path.is_file()
                else None
            )
            metrics = trace_metrics(inner_out)
            inner_ok, violations = result_gate(inner_summary, metrics)

            negative_out = run_dir / "negative-exec"
            negative_completed = base.run_capture(
                negative_command(args, composed_path, negative_out),
                cwd=GATEWAY_ROOT,
                timeout=180,
            )
            negative_exit_code = negative_completed.returncode
            (run_dir / "negative.stdout").write_text(
                negative_completed.stdout,
                encoding="utf-8",
            )
            (run_dir / "negative.stderr").write_text(
                negative_completed.stderr,
                encoding="utf-8",
            )
            negative_summary_path = negative_out / "summary.json"
            negative_summary = (
                json.loads(negative_summary_path.read_text(encoding="utf-8"))
                if negative_summary_path.is_file()
                else None
            )
            negative_metrics = trace_metrics(negative_out)
            negative_ok, negative_violations = negative_gate(
                negative_summary,
                negative_metrics,
            )
        else:
            inner_ok = False
            negative_ok = False
            violations = ["source feature or preflight gate failed"]
            negative_violations = ["source feature or preflight gate failed"]
    else:
        inner_ok = False
        negative_ok = False

    passed = (
        completed.returncode == 0
        and not timed_out
        and output_error is None
        and required_features_ok(features)
        and bool(preflight_result and preflight_result["ok"])
        and inner_exit_code == 0
        and inner_ok
        and negative_exit_code == 0
        and negative_ok
    )
    summary = {
        "ok": passed,
        "author_model": args.model,
        "inner_model": args.inner_model,
        "codex_exit_code": completed.returncode,
        "codex_tool_calls": base.tool_call_count(completed.stdout),
        "timed_out": timed_out,
        "duration_seconds": duration,
        "output_error": output_error,
        "source_bytes": len((source or "").encode("utf-8")),
        "composed_source_bytes": len((composed or "").encode("utf-8")),
        "features": features,
        "required_features_ok": required_features_ok(features),
        "preflight": preflight_result,
        "inner_exit_code": inner_exit_code,
        "inner_metrics": metrics,
        "inner_violations": violations,
        "inner_summary": inner_summary,
        "negative_exit_code": negative_exit_code,
        "negative_metrics": negative_metrics,
        "negative_violations": negative_violations,
        "negative_summary": negative_summary,
    }
    (run_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    aggregate = {
        "ok": passed,
        "run_root": str(run_root),
        "result": summary,
    }
    (run_root / "aggregate.json").write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return aggregate


def main() -> int:
    args = parse_args()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("Codex auth", args.codex_auth_json),
        ("standard source", args.standard_source),
        ("smoke", args.smoke),
    ):
        path = path.resolve()
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    result = evaluate(args)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
