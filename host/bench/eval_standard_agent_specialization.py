#!/usr/bin/env python3
"""Test whether an outer model can specialize the standard Stone control adapters."""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
GATEWAY_ROOT = SANDBOX / "waymark-gateway"
DEFAULT_LIBRARY = ROOT / "examples/scripts/standard_attempt_agent.stone"
DEFAULT_SMOKE = GATEWAY_ROOT / "host/runner/smoke_waymark_libos_gateway_model_call.py"
INVOCATION_MARKER = "\nsession = agent_session()"
EXPECTED_ANSWER = "specialization-ready"
EXPECTED_LABEL = "adapter-canary"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--codex", default="codex")
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
        default=ROOT / "target/runs/stone-standard-specialization-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--warm-build", choices=("auto", "0", "1"), default="0")
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def library_prefix(source: str) -> str:
    marker = source.find(INVOCATION_MARKER)
    if marker < 0:
        raise ValueError(f"standard source is missing marker {INVOCATION_MARKER!r}")
    return source[:marker].rstrip() + "\n"


def authorship_prompt() -> str:
    return """You are the outer coding agent. Return only one JSON object with a
non-empty `source` string containing a small Stone specialization. Do not call
tools or inspect files.

Stone is Python-shaped but has no imports, classes, decorators, or Python
standard library. Record fields may be read with dot access or get(...), but
record mutation requires item assignment such as checked["specialized"] = True;
attribute assignment such as checked.specialized = True is unsupported. The
execution harness prepends the complete visible standard agent-control library
to your source.

Available library contract:
- session = agent_session()
- options = standard_agent_options(session.input)
- standard_agent_control(session, options, dispatch_action, verify_finish,
  record_progress) -> result
- standard_shell_dispatch(action, options)
- standard_verify_finish(candidate, session, state) -> candidate
- standard_record_progress(event)
- emit(value)

Write only the specialization suffix:
1. Define a named three-argument verifier adapter. It must delegate first to
   standard_verify_finish(candidate, session, state).
2. Annotate the checked candidate with:
   - specialized = True
   - specialization_label = get(session.input, "specialization_label", "missing")
   - verified_round = state.rounds
3. Construct session and options, call standard_agent_control with the standard
   dispatcher, your named verifier passed directly (not a lambda), and the
   standard progress adapter, then emit the result.
4. Do not redefine or copy standard_agent_control. Do not call model_call,
   model_infer, context operations, tools, or attempt operations directly.
5. Include no Markdown fence. The source must be admitted Stone, not Python.
"""


def source_features(source: str) -> dict[str, bool]:
    return {
        "named_verifier": "def " in source and "verify" in source,
        "delegates_validation": "standard_verify_finish(" in source,
        "annotates_specialized": '"specialized"' in source,
        "annotates_label": '"specialization_label"' in source,
        "annotates_round": '"verified_round"' in source,
        "uses_control": "standard_agent_control(" in source,
        "uses_standard_dispatch": "standard_shell_dispatch" in source,
        "uses_standard_progress": "standard_record_progress" in source,
        "top_level_emit": "emit(" in source,
        "no_lambda": "lambda" not in source,
        "no_model_effect": "model_call(" not in source and "model_infer(" not in source,
        "no_library_redefinition": "def standard_agent_control(" not in source,
        "no_import": "\nimport " not in "\n" + source
        and "\nfrom " not in "\n" + source,
    }


def required_features_ok(features: dict[str, bool]) -> bool:
    return all(features.values())


def preflight(waymark_bin: Path, source: str, cwd: Path) -> dict[str, Any]:
    completed = base.run_capture(
        [str(waymark_bin), "eval", "-c", source],
        cwd=cwd,
        timeout=30,
    )
    payload = base.response_payload(completed)
    error = payload.get("error") if isinstance(payload, dict) else None
    detail = error.get("detail") if isinstance(error, dict) else None
    reached_context = detail == "Gateway task context is not active in this Stone runtime"
    return {
        "ok": reached_context,
        "reached_agent_session": reached_context,
        "exit_code": completed.returncode,
        "response": payload,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }


def fixture_command(
    args: argparse.Namespace,
    composed_source: Path,
    out_dir: Path,
) -> list[str]:
    response = json.dumps(
        {"actions": [{"final": {"answer": EXPECTED_ANSWER}}]},
        separators=(",", ":"),
    )
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
        "Return one final action with answer specialization-ready.",
        "--task-input-json",
        json.dumps(
            {
                "max_turns": 4,
                "max_rounds": 4,
                "completion_critique": False,
                "specialization_label": EXPECTED_LABEL,
            },
            separators=(",", ":"),
        ),
        "--fixture-sequence-json",
        json.dumps([response], separators=(",", ":")),
        "--expected-answer",
        EXPECTED_ANSWER,
        "--expected-model-calls",
        "1",
        "--warm-build",
        args.warm_build,
        "--out-dir",
        str(out_dir.resolve()),
    ]


def fixture_gate(summary: dict[str, Any] | None) -> tuple[bool, list[str]]:
    violations: list[str] = []
    if not isinstance(summary, dict) or summary.get("ok") is not True:
        return False, ["fixture summary is missing or failed"]
    report = summary.get("controller_report")
    if not isinstance(report, dict):
        return False, ["controller_report is missing"]
    expected = {
        "answer": EXPECTED_ANSWER,
        "specialized": True,
        "specialization_label": EXPECTED_LABEL,
        "verified_round": 1,
    }
    for key, value in expected.items():
        if report.get(key) != value:
            violations.append(
                f"controller_report.{key}={report.get(key)!r}, expected {value!r}"
            )
    control = report.get("_control")
    if not isinstance(control, dict) or control.get("name") != "stone.standard_action_v14":
        violations.append("standard control provenance is missing")
    if summary.get("rolled_back") is not True:
        violations.append("attempt did not roll back")
    return not violations, violations


def author_source(
    args: argparse.Namespace,
    prompt: str,
    run_dir: Path,
) -> tuple[subprocess.CompletedProcess[str], bool, float, str | None, str | None]:
    (run_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_dir / "output-schema.json").write_text(
        json.dumps(base.output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = base.codex_command(args, args.model, run_dir, prompt)
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
            completed = base.run_capture(
                command,
                cwd=run_dir,
                timeout=args.timeout,
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
    return completed, timed_out, duration, source, output_error


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

    completed, timed_out, duration, source, output_error = author_source(
        args,
        authorship_prompt(),
        run_dir,
    )
    features = source_features(source or "")
    composed = None
    preflight_result = None
    fixture_result = None
    fixture_violations = ["authored source unavailable"]
    fixture_exit_code = None
    if source is not None:
        (run_dir / "specialization.stone").write_text(
            source.rstrip() + "\n",
            encoding="utf-8",
        )
        prefix = library_prefix(args.standard_source.read_text(encoding="utf-8"))
        composed = prefix + "\n# Model-authored specialization.\n" + source.rstrip() + "\n"
        composed_path = run_dir / "composed-agent.stone"
        composed_path.write_text(composed, encoding="utf-8")
        preflight_result = preflight(args.waymark_bin, composed, run_dir)
        if required_features_ok(features) and preflight_result["ok"]:
            fixture_out = run_dir / "fixture-exec"
            fixture_completed = base.run_capture(
                fixture_command(args, composed_path, fixture_out),
                cwd=GATEWAY_ROOT,
                timeout=180,
            )
            fixture_exit_code = fixture_completed.returncode
            (run_dir / "fixture.stdout").write_text(
                fixture_completed.stdout,
                encoding="utf-8",
            )
            (run_dir / "fixture.stderr").write_text(
                fixture_completed.stderr,
                encoding="utf-8",
            )
            summary_path = fixture_out / "summary.json"
            fixture_result = (
                json.loads(summary_path.read_text(encoding="utf-8"))
                if summary_path.is_file()
                else None
            )
            fixture_ok, fixture_violations = fixture_gate(fixture_result)
        else:
            fixture_ok = False
            fixture_violations = ["source feature or preflight gate failed"]
    else:
        fixture_ok = False

    passed = (
        completed.returncode == 0
        and not timed_out
        and output_error is None
        and required_features_ok(features)
        and bool(preflight_result and preflight_result["ok"])
        and fixture_exit_code == 0
        and fixture_ok
    )
    summary = {
        "ok": passed,
        "model": args.model,
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
        "fixture_exit_code": fixture_exit_code,
        "fixture_violations": fixture_violations,
        "fixture_summary": fixture_result,
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
