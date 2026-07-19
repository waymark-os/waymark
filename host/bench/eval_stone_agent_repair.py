#!/usr/bin/env python3
"""Test one-shot, diagnostic-guided repair of an admitted Stone agent."""

from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
AUTHORSHIP_PATH = Path(__file__).with_name("eval_stone_agent_authorship.py")
AUTHORSHIP_SPEC = importlib.util.spec_from_file_location(
    "eval_stone_agent_authorship", AUTHORSHIP_PATH
)
assert AUTHORSHIP_SPEC and AUTHORSHIP_SPEC.loader
AUTHORSHIP = importlib.util.module_from_spec(AUTHORSHIP_SPEC)
AUTHORSHIP_SPEC.loader.exec_module(AUTHORSHIP)

DEFAULT_SOURCE = (
    ROOT / "examples" / "generated" / "gpt-5.5" / "bounded_react_agent.stone"
)
CASES = ("syntax", "type", "model-effect")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.5")
    parser.add_argument("--case", action="append", choices=CASES, dest="cases")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument(
        "--waymark-bin", type=Path, default=ROOT / "target" / "debug" / "waymark"
    )
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway" / "target" / "debug" / "waymark-gateway",
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target" / "runs" / "stone-agent-repair-v1-gpt55",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument(
        "--reuse-existing",
        action="store_true",
        help="Reuse completed case outputs and run only missing repair calls.",
    )
    return parser.parse_args()


def corrupt_source(source: str, case: str) -> str:
    if case == "syntax":
        old, new = "def bounded_inner_agent():", "def bounded_inner_agent()"
    elif case == "type":
        old, new = "def bounded_inner_agent():", "def bounded_inner_agent() -> None:"
    elif case == "model-effect":
        old = 'response_format={"type": "json_object"})'
        new = 'response_format={"type": "json_object"}, seed=0)'
    else:
        raise ValueError(f"unknown repair case: {case}")
    if source.count(old) != 1:
        raise ValueError(f"repair case {case!r} expected one source anchor {old!r}")
    return source.replace(old, new, 1)


def error_code(result: dict[str, Any] | None) -> str | None:
    response = result.get("response") if isinstance(result, dict) else None
    error = response.get("error") if isinstance(response, dict) else None
    return error.get("code") if isinstance(error, dict) else None


def bounded_result(result: dict[str, Any]) -> dict[str, Any]:
    """Keep only the structured diagnostic that an outer agent should need."""
    return {
        "exit_code": result.get("exit_code"),
        "response": result.get("response"),
    }


def diagnose(
    case: str,
    broken: str,
    waymark_bin: Path,
    gateway_bin: Path,
    case_dir: Path,
) -> dict[str, Any]:
    preflight = AUTHORSHIP.preflight_source(waymark_bin, broken, case_dir)
    if case == "type" and preflight.get("reached_model_call"):
        result = AUTHORSHIP.execute_with_fixture(
            waymark_bin, gateway_bin, broken, case_dir / "diagnostic"
        )
        phase = "fixture_execution"
    else:
        result = preflight
        phase = "preflight"
    diagnostic = {"phase": phase, **bounded_result(result)}
    expected = {
        "syntax": "stone_parse_error",
        "type": "stone_script_error",
        "model-effect": "model_invalid_request",
    }[case]
    diagnostic["expected_code"] = expected
    diagnostic["observed_code"] = error_code(result)
    diagnostic["matches_expected"] = diagnostic["observed_code"] == expected
    return diagnostic


def repair_prompt(
    case: str,
    broken: str,
    diagnostic: dict[str, Any],
    model_help: dict[str, Any],
) -> str:
    help_json = json.dumps(
        AUTHORSHIP.compact_help(model_help), separators=(",", ":"), sort_keys=True
    )
    diagnostic_json = json.dumps(diagnostic, separators=(",", ":"), sort_keys=True)
    return f'''You are repairing one complete Stone program written by another coding agent. Do not call tools and do not inspect the filesystem. Make exactly one repair attempt and return only the required JSON object.

Stone is an LLM-oriented, Python-shaped structured shell language, not Python. Preserve the visible bounded ReAct control flow, prompts, JSON action dispatch, maximum of four model turns, structured observations, and final emit. Do not add imports, hidden helpers, native tool calling, task_spec(), or model_infer(). The repaired source must still call model_call before any other externally visible effect.

Failure class: {case}
Structured runtime diagnostic: {diagnostic_json}
Relevant live model_call help: {help_json}

Broken Stone source:
---
{broken.rstrip()}
---

Return {{"source":"<the complete repaired Stone program>"}}. Do not return a patch or explanation.
'''


def codex_command(args: argparse.Namespace, case_dir: Path, prompt: str) -> list[str]:
    compatible = SimpleNamespace(codex=args.codex)
    return AUTHORSHIP.codex_command(compatible, args.model, case_dir, prompt)


def validate_repair(
    repaired: str,
    args: argparse.Namespace,
    case_dir: Path,
) -> dict[str, Any]:
    features = AUTHORSHIP.source_features(repaired)
    required_features = all(
        features[name]
        for name in (
            "model_call",
            "bounded_loop",
            "json_loads",
            "run_dispatch",
            "finish_branch",
            "turn_limit",
            "top_level_emit",
        )
    ) and not features["forbidden_import"]
    preflight = AUTHORSHIP.preflight_source(args.waymark_bin, repaired, case_dir)
    fixture = (
        AUTHORSHIP.execute_with_fixture(
            args.waymark_bin, args.gateway_bin, repaired, case_dir / "validation"
        )
        if preflight.get("ok")
        else None
    )
    return {
        "ok": required_features
        and bool(preflight.get("ok"))
        and bool(fixture and fixture.get("ok")),
        "features": features,
        "required_features_ok": required_features,
        "preflight": bounded_result(preflight),
        "fixture_execution": bounded_result(fixture) if fixture else None,
    }


def evaluate_case(
    args: argparse.Namespace,
    case: str,
    admitted: str,
    model_help: dict[str, Any],
) -> dict[str, Any]:
    case_dir = args.run_root / case
    case_dir.mkdir(parents=True, exist_ok=args.overwrite)
    broken = corrupt_source(admitted, case)
    (case_dir / "broken.stone").write_text(broken, encoding="utf-8")
    diagnostic = diagnose(
        case, broken, args.waymark_bin, args.gateway_bin, case_dir
    )
    (case_dir / "diagnostic.json").write_text(
        json.dumps(diagnostic, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    prompt = repair_prompt(case, broken, diagnostic, model_help)
    (case_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (case_dir / "output-schema.json").write_text(
        json.dumps(AUTHORSHIP.output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = codex_command(args, case_dir, prompt)
    started = time.monotonic()
    reused_existing = args.reuse_existing and (case_dir / "last-message.json").is_file()
    if reused_existing:
        completed = subprocess.CompletedProcess(
            command,
            0,
            (case_dir / "codex.stdout.jsonl").read_text(encoding="utf-8")
            if (case_dir / "codex.stdout.jsonl").is_file()
            else "",
            (case_dir / "codex.stderr").read_text(encoding="utf-8")
            if (case_dir / "codex.stderr").is_file()
            else "",
        )
        timed_out = False
    else:
        try:
            completed = AUTHORSHIP.run_capture(
                command, cwd=case_dir, timeout=args.timeout
            )
            timed_out = False
        except subprocess.TimeoutExpired as error:
            completed = subprocess.CompletedProcess(
                command, 124, error.stdout or "", error.stderr or ""
            )
            timed_out = True
    duration = time.monotonic() - started
    (case_dir / "codex.stdout.jsonl").write_text(
        completed.stdout, encoding="utf-8"
    )
    (case_dir / "codex.stderr").write_text(completed.stderr, encoding="utf-8")
    repaired, output_error = AUTHORSHIP.parse_last_message(
        case_dir / "last-message.json"
    )
    if repaired is not None:
        (case_dir / "repaired.stone").write_text(
            repaired.rstrip() + "\n", encoding="utf-8"
        )
    validation = validate_repair(repaired, args, case_dir) if repaired else None
    tool_calls = AUTHORSHIP.tool_call_count(completed.stdout)
    passed = (
        completed.returncode == 0
        and not timed_out
        and output_error is None
        and repaired != broken
        and bool(diagnostic["matches_expected"])
        and tool_calls == 0
        and bool(validation and validation["ok"])
    )
    summary = {
        "ok": passed,
        "case": case,
        "model": args.model,
        "command": command,
        "codex_exit_code": completed.returncode,
        "codex_tool_calls": tool_calls,
        "timed_out": timed_out,
        "reused_existing": reused_existing,
        "duration_seconds": duration,
        "diagnostic": diagnostic,
        "output_error": output_error,
        "source_changed": repaired is not None and repaired != broken,
        "validation": validation,
    }
    (case_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return summary


def main() -> int:
    args = parse_args()
    args.source = args.source.resolve()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.run_root = args.run_root.resolve()
    for label, path in (
        ("source", args.source),
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    if args.run_root.exists() and any(args.run_root.iterdir()) and not args.overwrite:
        raise SystemExit(f"refusing to overwrite non-empty run root: {args.run_root}")
    args.run_root.mkdir(parents=True, exist_ok=True)
    admitted = args.source.read_text(encoding="utf-8")
    model_help = AUTHORSHIP.help_topic(args.waymark_bin, "model_call")
    results = [
        evaluate_case(args, case, admitted, model_help)
        for case in (args.cases or CASES)
    ]
    aggregate = {
        "ok": all(result["ok"] for result in results),
        "model": args.model,
        "source": str(args.source),
        "one_repair_attempt_per_case": True,
        "passed": sum(bool(result["ok"]) for result in results),
        "total": len(results),
        "results": results,
    }
    (args.run_root / "aggregate.json").write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(aggregate, indent=2, sort_keys=True))
    return 0 if aggregate["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
