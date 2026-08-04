#!/usr/bin/env python3
"""Evaluate a model-authored verifier adapter against hidden executable tests."""

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
EXPECTED_ANSWER = "tests-pass"
VISIBLE_TEST_COMMAND = "python3 /app/test_solution.py"
HIDDEN_TEST_COMMAND = "PYTHONPATH=/app python3 /tests/verify_solution.py"
SOURCE_INPUT = "opaque executable verifier task\n"


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
        default=ROOT / "target/runs/stone-standard-executable-verifier-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def authorship_prompt() -> str:
    return f"""You are the outer coding agent. Return only one JSON object with a
non-empty `source` string containing a small Stone specialization. Do not call
tools or inspect files. The task's implementation and hidden tests are not
available to you.

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
- run(argv, cwd=..., timeout_ms=..., max_stdout_bytes=...,
  max_stderr_bytes=...) -> record with ok, exit_code, stdout, stderr
- fail(message, code=...) -> never
- emit(value)

The repository-owned visible verifier command is `python3 /app/test_solution.py`.

Write only the specialization suffix:
1. Define a named three-argument verifier adapter.
2. Delegate first to standard_verify_finish(candidate, session, state).
3. Execute the repository-owned visible verifier exactly once with:
   run(["python3", "/app/test_solution.py"], cwd="/app", timeout_ms=10000,
       max_stdout_bytes=4096, max_stderr_bytes=4096)
4. If result.ok is false, call fail with code `task_verification_failed`.
5. On success annotate the checked candidate with task_verified=True,
   verifier_status=result.exit_code, verifier_transition_id=result.transition_id,
   and verified_round=state.rounds, then return it.
6. Construct session/options, invoke standard_agent_control with the standard
   dispatcher, your verifier passed directly as a named function, and standard
   progress, then emit the result.
7. Do not embed an expected implementation or output. Do not redefine the
   library. Do not call model_call, model_infer, context, attempt, file-write,
   or other resource operations directly. The verifier's one bounded run is
   the only direct resource effect.
8. Include no Markdown fence. The source must be admitted Stone, not Python.

The host will independently run hidden read-only tests in a rolled-back
checkpoint branch. Your visible verifier is early feedback, not the authority.
"""


def source_features(source: str) -> dict[str, bool]:
    forbidden = (
        "model_call(",
        "model_infer(",
        "context_write(",
        "context_read(",
        "attempt_",
        "read_file(",
        "write_text(",
        "write_file(",
    )
    return {
        "named_verifier": "def " in source and "verify" in source,
        "delegates_validation": "standard_verify_finish(" in source,
        "one_verifier_run": source.count("run(") == 1,
        "visible_verifier_argv": '["python3", "/app/test_solution.py"]' in source,
        "bounded_timeout": "timeout_ms=10000" in source,
        "bounded_stdout": "max_stdout_bytes=4096" in source,
        "bounded_stderr": "max_stderr_bytes=4096" in source,
        "checks_result": ".ok" in source,
        "verification_error": "task_verification_failed" in source,
        "annotates_verified": '"task_verified"' in source,
        "annotates_status": '"verifier_status"' in source,
        "annotates_transition": '"verifier_transition_id"' in source,
        "annotates_round": '"verified_round"' in source,
        "uses_control": "standard_agent_control(" in source,
        "uses_standard_dispatch": "standard_shell_dispatch" in source,
        "uses_standard_progress": "standard_record_progress" in source,
        "top_level_emit": "emit(" in source,
        "no_expected_solution": "normalize_words" not in source,
        "no_lambda": "lambda" not in source,
        "no_library_redefinition": "def standard_agent_control(" not in source,
        "no_forbidden_effect": not any(effect in source for effect in forbidden),
        "no_import": "\nimport " not in "\n" + source
        and "\nfrom " not in "\n" + source,
    }


def required_features_ok(features: dict[str, bool]) -> bool:
    return all(features.values())


def prepare_repository(run_dir: Path) -> tuple[Path, Path]:
    repository = run_dir / "repository"
    hidden_tests = run_dir / "hidden-tests"
    repository.mkdir(parents=True, exist_ok=True)
    hidden_tests.mkdir(parents=True, exist_ok=True)
    (repository / "input.txt").write_text(SOURCE_INPUT, encoding="utf-8")
    (repository / "README.md").write_text(
        """# Word normalization

Implement `normalize_words(words)` in `solution.py`.

- Strip surrounding whitespace from each string.
- Discard empty results.
- Normalize with `casefold()`.
- Return a sorted list of unique normalized strings.
- Do not mutate the input collection.

Run `python3 test_solution.py` for the visible checks. Do not modify tests.
""",
        encoding="utf-8",
    )
    (repository / "solution.py").write_text(
        """def normalize_words(words):
    raise NotImplementedError("implement normalize_words")
""",
        encoding="utf-8",
    )
    (repository / "test_solution.py").write_text(
        """from solution import normalize_words

assert normalize_words([" Pear ", "apple", "PEAR", ""]) == ["apple", "pear"]
assert normalize_words([]) == []
print("visible verifier passed")
""",
        encoding="utf-8",
    )
    (hidden_tests / "verify_solution.py").write_text(
        """from solution import normalize_words

cases = [
    (["  Zeta", "alpha", "ALPHA", "beta ", ""], ["alpha", "beta", "zeta"]),
    (["10", "2", " 10 "], ["10", "2"]),
    (["Straße", "STRASSE"], ["strasse"]),
    ([], []),
]
for values, expected in cases:
    before = list(values)
    actual = normalize_words(values)
    assert actual == expected, (values, expected, actual)
    assert values == before, "normalize_words mutated its input"
print("hidden verifier passed")
""",
        encoding="utf-8",
    )
    return repository, hidden_tests


def inner_task_objective() -> str:
    return (
        "Inspect /app/README.md and /app/solution.py. Implement the requested "
        "normalize_words function without changing README.md or test_solution.py. "
        "Run python3 /app/test_solution.py with run_linux. Fix any failure. Only "
        "after the visible tests pass, return one final action with "
        '{"answer":"tests-pass"}.'
    )


def positive_command(
    args: argparse.Namespace,
    source: Path,
    repository: Path,
    hidden_tests: Path,
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
        str(source.resolve()),
        "--workspace-source-dir",
        str(repository.resolve()),
        "--task-objective",
        inner_task_objective(),
        "--task-input-json",
        '{"max_turns":8,"max_rounds":8,"completion_critique":false}',
        "--source-input-text",
        SOURCE_INPUT,
        "--expected-answer",
        EXPECTED_ANSWER,
        "--expected-model-calls",
        "4",
        "--max-turns",
        "8",
        "--max-rounds",
        "8",
        "--checkpoint-verifier-command",
        HIDDEN_TEST_COMMAND,
        "--checkpoint-verifier-mount",
        f"{hidden_tests.resolve()}:/tests",
        "--checkpoint-verifier-timeout-ms",
        "30000",
        "--expected-checkpoint-verifier-status",
        "0",
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def negative_command(
    args: argparse.Namespace,
    source: Path,
    repository: Path,
    hidden_tests: Path,
    out_dir: Path,
) -> list[str]:
    def response(action: dict[str, Any]) -> str:
        return json.dumps({"actions": [action]}, separators=(",", ":"))

    wrong_solution = "def normalize_words(words):\n    return list(words)\n"
    sequence = [
        response({"tool": "read", "input": {"path": "/app/README.md"}}),
        response(
            {
                "tool": "write",
                "input": {
                    "path": "/app/solution.py",
                    "content": wrong_solution,
                },
            }
        ),
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
        str(source.resolve()),
        "--workspace-source-dir",
        str(repository.resolve()),
        "--task-objective",
        inner_task_objective(),
        "--task-input-json",
        '{"max_turns":8,"max_rounds":8,"completion_critique":false}',
        "--source-input-text",
        SOURCE_INPUT,
        "--fixture-sequence-json",
        json.dumps(sequence, separators=(",", ":")),
        "--expected-error-code",
        "task_verification_failed",
        "--expected-model-calls",
        "3",
        "--max-turns",
        "8",
        "--max-rounds",
        "8",
        "--checkpoint-verifier-command",
        HIDDEN_TEST_COMMAND,
        "--checkpoint-verifier-mount",
        f"{hidden_tests.resolve()}:/tests",
        "--checkpoint-verifier-timeout-ms",
        "30000",
        "--expected-checkpoint-verifier-status",
        "1",
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def trace_metrics(out_dir: Path) -> dict[str, Any]:
    path = out_dir / "gateway-data/traces/operations.jsonl"
    counts: Counter[str] = Counter()
    usage = Counter()
    if path.is_file():
        for line in path.read_text(encoding="utf-8").splitlines():
            value = json.loads(line)
            operation = value.get("op")
            if isinstance(operation, str):
                counts[operation] += 1
            if operation == "attempt.rpc.model.call":
                for key in ("input_tokens", "output_tokens", "total_tokens"):
                    amount = value.get(key)
                    if isinstance(amount, int):
                        usage[key] += amount
    return {
        "operation_counts": dict(sorted(counts.items())),
        "token_usage": dict(usage),
    }


def checkpoint_gate(
    summary: dict[str, Any],
    expected_status: int,
) -> list[str]:
    verifier = summary.get("checkpoint_verifier")
    if not isinstance(verifier, dict):
        return ["checkpoint verifier result is missing"]
    violations: list[str] = []
    if verifier.get("status") != expected_status:
        violations.append(
            f"checkpoint verifier status={verifier.get('status')!r}, "
            f"expected {expected_status}"
        )
    if verifier.get("rolled_back") is not True:
        violations.append("checkpoint verifier branch did not roll back")
    if HIDDEN_TEST_COMMAND != verifier.get("command"):
        violations.append("checkpoint verifier command changed")
    return violations


def positive_gate(
    summary: dict[str, Any] | None,
    metrics: dict[str, Any],
) -> tuple[bool, list[str]]:
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return False, ["positive summary is missing or failed"]
    violations = checkpoint_gate(summary, 0)
    report = summary.get("controller_report")
    if not isinstance(report, dict):
        violations.append("controller_report is missing")
    else:
        expected = {
            "answer": EXPECTED_ANSWER,
            "task_verified": True,
            "verifier_status": 0,
        }
        for key, value in expected.items():
            if report.get(key) != value:
                violations.append(
                    f"controller_report.{key}={report.get(key)!r}, expected {value!r}"
                )
        if not isinstance(report.get("verifier_transition_id"), str):
            violations.append("verifier transition id is missing")
        control = report.get("_control")
        if not isinstance(control, dict) or control.get("name") != "stone.standard_action_v13":
            violations.append("standard control provenance is missing")
    counts = metrics.get("operation_counts", {})
    model_calls = counts.get("attempt.rpc.model.call", 0)
    if not 4 <= model_calls <= 8:
        violations.append(f"model calls={model_calls}, expected 4..8")
    if counts.get("attempt.rpc.workspace_tx.write", 0) < 1:
        violations.append("inner agent did not write the workspace")
    if counts.get("attempt.rpc.linux.exec", 0) < 1:
        violations.append("visible executable verifier did not run")
    context = (
        summary.get("controller_result", {})
        .get("diagnostics", {})
        .get("context", {})
    )
    if context.get("item_count") != 1:
        violations.append("progress hot store did not retain exactly one item")
    if summary.get("rolled_back") is not True:
        violations.append("positive attempt did not roll back")
    return not violations, violations


def negative_gate(
    summary: dict[str, Any] | None,
    metrics: dict[str, Any],
) -> tuple[bool, list[str]]:
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return False, ["negative summary is missing or failed"]
    violations = checkpoint_gate(summary, 1)
    if summary.get("controller_error_code") != "task_verification_failed":
        violations.append(
            f"controller error={summary.get('controller_error_code')!r}, "
            "expected task_verification_failed"
        )
    counts = metrics.get("operation_counts", {})
    if counts.get("attempt.rpc.model.call") != 3:
        violations.append(
            f"negative model calls={counts.get('attempt.rpc.model.call')!r}, expected 3"
        )
    if counts.get("attempt.rpc.workspace_tx.write") != 1:
        violations.append("negative trajectory did not make exactly one write")
    if counts.get("attempt.rpc.linux.exec", 0) < 1:
        violations.append("authored verifier did not execute visible tests")
    if summary.get("rolled_back") is not True:
        violations.append("negative attempt did not roll back")
    return not violations, violations


def execute_cell(command: list[str], cwd: Path, out_dir: Path, timeout: int) -> dict[str, Any]:
    completed = base.run_capture(command, cwd=cwd, timeout=timeout)
    summary_path = out_dir / "summary.json"
    return {
        "exit_code": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
        "summary": (
            json.loads(summary_path.read_text(encoding="utf-8"))
            if summary_path.is_file()
            else None
        ),
        "metrics": trace_metrics(out_dir),
    }


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
    preflight = None
    positive = None
    negative = None
    positive_ok = False
    negative_ok = False
    positive_violations = ["authored source unavailable"]
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
        composed = prefix + "\n# Model-authored executable verifier.\n" + source.rstrip() + "\n"
        composed_path = run_dir / "composed-agent.stone"
        composed_path.write_text(composed, encoding="utf-8")
        preflight = specialization.preflight(args.waymark_bin, composed, run_dir)
        if required_features_ok(features) and preflight["ok"]:
            repository, hidden_tests = prepare_repository(run_dir)
            positive_out = run_dir / "positive-exec"
            positive = execute_cell(
                positive_command(
                    args,
                    composed_path,
                    repository,
                    hidden_tests,
                    positive_out,
                ),
                GATEWAY_ROOT,
                positive_out,
                300,
            )
            positive_ok, positive_violations = positive_gate(
                positive["summary"],
                positive["metrics"],
            )
            negative_out = run_dir / "negative-exec"
            negative = execute_cell(
                negative_command(
                    args,
                    composed_path,
                    repository,
                    hidden_tests,
                    negative_out,
                ),
                GATEWAY_ROOT,
                negative_out,
                240,
            )
            negative_ok, negative_violations = negative_gate(
                negative["summary"],
                negative["metrics"],
            )
        else:
            positive_violations = ["source feature or preflight gate failed"]
            negative_violations = ["source feature or preflight gate failed"]

    passed = (
        completed.returncode == 0
        and not timed_out
        and output_error is None
        and required_features_ok(features)
        and bool(preflight and preflight["ok"])
        and bool(positive and positive["exit_code"] == 0)
        and positive_ok
        and bool(negative and negative["exit_code"] == 0)
        and negative_ok
    )
    result = {
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
        "preflight": preflight,
        "positive": positive,
        "positive_violations": positive_violations,
        "negative": negative,
        "negative_violations": negative_violations,
    }
    (run_dir / "summary.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    aggregate = {
        "ok": passed,
        "run_root": str(run_root),
        "result": result,
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
