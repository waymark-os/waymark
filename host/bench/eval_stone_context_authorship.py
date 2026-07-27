#!/usr/bin/env python3
"""Evaluate whether an outer model can author a context-aware Stone agent loop."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
DEFAULT_MODELS = ["gpt-5.5", "gpt-5.6-terra", "gpt-5.6-luna"]
PROMPT_VERSION = "v2"
FIXTURE_VERSION = "v2"
FIXTURE_SEQUENCE = [
    json.dumps(
        {
            "actions": [
                {
                    "tool": "context_write",
                    "input": {
                        "key": "requirement.output",
                        "kind": "requirement",
                        "content": {"text": "the final answer must be ready"},
                        "status": "verified",
                        "evidence": ["fixture:task"],
                    },
                }
            ]
        },
        separators=(",", ":"),
    ),
    json.dumps(
        {
            "actions": [
                {
                    "tool": "context_write",
                    "input": {
                        "key": "outcome.probe",
                        "kind": "outcome",
                        "content": {
                            "action": "probe",
                            "ok": False,
                            "lesson": "do not retry the failed probe",
                        },
                        "evidence": ["fixture:probe"],
                    },
                }
            ]
        },
        separators=(",", ":"),
    ),
    json.dumps(
        {
            "actions": [
                {
                    "tool": "context_read",
                    "input": {
                        "query": "ready failed probe",
                        "keys": [],
                        "kinds": [],
                        "limit": 8,
                    },
                }
            ]
        },
        separators=(",", ":"),
    ),
    json.dumps({"actions": [{"final": {"answer": "ready"}}]}, separators=(",", ":")),
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", action="append", dest="models")
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-context-authorship-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    parser.add_argument(
        "--canary-source",
        type=Path,
        help="Run one pre-authored Stone source through the deterministic fixture without an author model call.",
    )
    parser.add_argument(
        "--repair-source",
        type=Path,
        help="Give an author model one response to repair this previously generated source.",
    )
    parser.add_argument(
        "--repair-summary",
        type=Path,
        help="Prior failed summary.json supplying the bounded runtime diagnostic for --repair-source.",
    )
    return parser.parse_args()


def help_bundle(waymark_bin: Path) -> dict[str, dict[str, Any]]:
    names = [
        "model_call",
        "context_write",
        "context_read",
        "context_project",
    ]
    return {name: base.compact_help(base.help_topic(waymark_bin, name)) for name in names}


def authorship_prompt(help_topics: dict[str, dict[str, Any]]) -> str:
    help_json = json.dumps(help_topics, separators=(",", ":"), sort_keys=True)
    return f"""You are the outer coding agent. Write one complete ad-hoc Stone program that acts as a bounded context-aware inner agent. Do not call tools and do not inspect the filesystem. Return only the required JSON object.

Stone is an LLM-oriented, Python-shaped structured shell language. It supports assignments, def/return, for/range, if/elif/else, try/except, lists, records, record field access, and ordinary function calls. It does not provide Python imports, classes, asyncio, or the Python standard library.

Relevant builtins:
- json_loads(text: str) -> Any
- json_dumps(value: Any) -> str
- get(record, key, default) -> Any
- fail(message: str, code: str = ...) -> never
- emit(value: Any) -> Any
- live model/context help: {help_json}

Program contract:
1. Keep all control flow in Stone. Use a for loop with at most 6 iterations and call model_call exactly once per iteration.
2. Before each model_call, call context_project with a focus describing the current task/turn and max_tokens no greater than 256. Put projection.text in a user message visible to that call.
3. Ask the inner model for JSON only as {{"actions":[...]}}. An action is one of:
   - {{"tool":"context_write","input":{{"key":"...","kind":"...","content":...,"status":"...","evidence":[...]}}}}
   - {{"tool":"context_read","input":{{"query":"...","keys":[...],"kinds":[...],"limit":8}}}}
   - {{"final":{{"answer":"..."}}}}
4. Initialize one bounded messages list before the loop and pass that same list to every model_call. After each call, append response.content to that list as an assistant message. After each context action, append its bounded structured observation as a user message. Parse response.content with json_loads, dispatch every context action to its matching Stone builtin using the structured input fields, and reject unknown tools. The deterministic fixture advances by counting assistant messages visible in this list.
5. On final, set ledger to a fresh context_read(keys=["requirement.output"]) finish check and fail if it is empty. Return exactly {{"answer":..., "turns":..., "ledger":ledger, "projection":projection}}, where projection is the latest context_project result.
6. If the loop is exhausted, call fail with code context_turn_limit. Keep the program bounded; do not replay an unbounded shadow transcript or use files as hidden memory.
7. Include a top-level call and emit the structured result. Do not use imports, native model tool calling, hidden agent helpers, react_control, task_spec, or task_input.

The evaluator returns a deterministic four-response sequence that writes a requirement, writes a failed outcome, reads retained state, and then finishes. A correct program finishes with answer `ready` after four model calls.
"""


def repair_prompt(
    source: str,
    prior_summary: dict[str, Any],
    help_topics: dict[str, dict[str, Any]],
) -> str:
    diagnostic = (((prior_summary.get("fixture_execution") or {}).get("response") or {}).get("error") or {})
    bounded_diagnostic = {
        "code": diagnostic.get("code"),
        "message": diagnostic.get("message"),
        "detail": diagnostic.get("detail"),
        "help": diagnostic.get("help"),
    }
    help_json = json.dumps(help_topics, separators=(",", ":"), sort_keys=True)
    diagnostic_json = json.dumps(bounded_diagnostic, separators=(",", ":"), sort_keys=True)
    return f"""You are repairing one generated Stone context-aware agent. Do not call tools and do not inspect the filesystem. Return only the required JSON object containing the complete repaired source.

Preserve the visible bounded loop, one model_call per iteration, shared bounded messages list, context_write/context_read dispatch, context_project before each model call, finish-time requirement.output read, exact answer/turns/ledger/projection result, turn-limit failure, and top-level emit. Do not use imports, react_control, hidden helpers, files as memory, or native model tool calling.

Stone model messages require string role/content fields. Serialize structured observations with json_dumps before placing them in message content.

Live help:
{help_json}

Bounded runtime diagnostic:
{diagnostic_json}

Broken source:
---
{source}
---
"""


def source_features(source: str) -> dict[str, bool]:
    return {
        "model_call": "model_call(" in source,
        "context_write": "context_write(" in source,
        "context_read": "context_read(" in source,
        "context_project": "context_project(" in source,
        "bounded_loop": bool(re.search(r"for\s+\w+\s+in\s+range\([1-6]\)", source)),
        "json_loads": "json_loads(" in source,
        "finish_branch": "final" in source and "requirement.output" in source,
        "turn_limit": "context_turn_limit" in source,
        "top_level_emit": "emit(" in source,
        "forbidden_import": bool(re.search(r"(?m)^\s*(import|from)\s+", source)),
        "hidden_control": "react_control(" in source,
    }


def context_gate(payload: dict[str, Any] | None) -> tuple[bool, list[str]]:
    violations: list[str] = []
    if not isinstance(payload, dict) or payload.get("ok") is not True:
        return False, ["Stone execution did not return ok=true"]
    value = payload.get("value")
    if not isinstance(value, dict):
        return False, ["Stone result is not a record"]
    if value.get("answer") != "ready":
        violations.append("result answer is not ready")
    if value.get("turns") != 4:
        violations.append("result turns is not 4")
    ledger = value.get("ledger")
    if not isinstance(ledger, list) or not ledger:
        violations.append("finish result lacks a non-empty ledger")
    elif ledger[0].get("key") != "requirement.output":
        violations.append("finish ledger does not contain requirement.output")
    projection = value.get("projection")
    if not isinstance(projection, dict) or not projection.get("items"):
        violations.append("result lacks a non-empty latest projection")

    events = (((payload.get("diagnostics") or {}).get("context") or {}).get("events") or [])
    ops = [event.get("op") for event in events if isinstance(event, dict)]
    for required in ("write", "read", "project"):
        if required not in ops:
            violations.append(f"context diagnostics lack {required}")
    write_keys = [
        event.get("key")
        for event in events
        if isinstance(event, dict) and event.get("op") == "write"
    ]
    if "requirement.output" not in write_keys or "outcome.probe" not in write_keys:
        violations.append("context diagnostics lack the requirement or outcome write")
    elif not (
        write_keys.index("requirement.output") < write_keys.index("outcome.probe")
    ):
        violations.append("requirement/outcome write causal order is wrong")
    if not any(event.get("op") == "project" and event.get("selected") for event in events):
        violations.append("no projection activated a retained item")
    if not any(
        event.get("op") == "read" and event.get("query") and event.get("selected")
        for event in events
    ):
        violations.append("explicit context query retrieved no retained item")
    if not ops or ops[-1] != "read":
        violations.append("finish did not end with a fresh context read")
    return not violations, violations


def execute_with_fixture(
    waymark_bin: Path,
    gateway_bin: Path,
    source_text: str,
    run_dir: Path,
) -> dict[str, Any]:
    fixture_dir = run_dir / "fixture-exec"
    if fixture_dir.exists():
        shutil.rmtree(fixture_dir)
    data_root = fixture_dir / "gateway-data"
    source = fixture_dir / "source"
    work = fixture_dir / "work"
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-context-author-", dir="/tmp"))
    socket_path = socket_root / "gateway.sock"
    source.mkdir(parents=True)
    work.mkdir()
    (source / "README.md").write_text("context authorship fixture\n", encoding="utf-8")
    base.gateway_command(gateway_bin, data_root, "repo", "snapshot", "--name", "repo", "--path", str(source))
    tx = base.gateway_command(gateway_bin, data_root, "env", "snapshot", "--workspace", "repo")

    server_env = dict(os.environ)
    server_env.update(
        {
            "WAYMARK_MODEL_PROVIDER": "fixture",
            "WAYMARK_MODEL_FIXTURE_SEQUENCE_JSON": json.dumps(FIXTURE_SEQUENCE),
        }
    )
    server = subprocess.Popen(
        [str(gateway_bin), "--data-root", str(data_root), "rpc", "serve", "--socket", str(socket_path)],
        env=server_env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        base.wait_for_socket(socket_path, server)
        env = dict(os.environ)
        env.update(
            {
                "WAYMARK_START_DIR": str(work),
                "WAYMARK_GATEWAY_SOCKET": str(socket_path),
                "WAYMARK_GATEWAY_TX": tx,
                "WAYMARK_GATEWAY_IMAGE": "python:3.12",
                "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
                "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
            }
        )
        completed = base.run_capture(
            [str(waymark_bin), "eval", "-c", source_text],
            cwd=work,
            env=env,
            timeout=60,
        )
        payload = base.response_payload(completed)
        gate_ok, violations = context_gate(payload)
        return {
            "ok": completed.returncode == 0 and gate_ok,
            "exit_code": completed.returncode,
            "response": payload,
            "context_gate_ok": gate_ok,
            "context_gate_violations": violations,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
        }
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=5)
        shutil.rmtree(socket_root, ignore_errors=True)


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
            completed = base.run_capture(command, cwd=run_dir, timeout=args.timeout)
            timed_out = False
        except subprocess.TimeoutExpired as error:
            completed = subprocess.CompletedProcess(command, 124, error.stdout or "", error.stderr or "")
            timed_out = True
    duration = time.monotonic() - started
    (run_dir / "codex.stdout.jsonl").write_text(completed.stdout, encoding="utf-8")
    (run_dir / "codex.stderr").write_text(completed.stderr, encoding="utf-8")
    source, output_error = base.parse_last_message(run_dir / "last-message.json")
    if source is not None:
        (run_dir / "agent.stone").write_text(source.rstrip() + "\n", encoding="utf-8")
    features = source_features(source or "")
    preflight = base.preflight_source(args.waymark_bin, source, run_dir) if source else None
    fixture = (
        execute_with_fixture(args.waymark_bin, args.gateway_bin, source, run_dir)
        if source and preflight and preflight.get("ok")
        else None
    )
    required = all(
        features[name]
        for name in (
            "model_call",
            "context_write",
            "context_read",
            "context_project",
            "bounded_loop",
            "json_loads",
            "finish_branch",
            "turn_limit",
            "top_level_emit",
        )
    ) and not features["forbidden_import"] and not features["hidden_control"]
    passed = (
        completed.returncode == 0
        and output_error is None
        and required
        and bool(preflight and preflight.get("ok"))
        and bool(fixture and fixture.get("ok"))
    )
    summary = {
        "ok": passed,
        "status": "pass" if passed else "fail",
        "model": model,
        "command": command,
        "codex_exit_code": completed.returncode,
        "timed_out": timed_out,
        "reused_existing": args.reuse_existing,
        "duration_seconds": duration,
        "output_error": output_error,
        "source_bytes": len((source or "").encode("utf-8")),
        "features": features,
        "required_features_ok": required,
        "preflight": preflight,
        "fixture_execution": fixture,
        "codex_tool_calls": base.tool_call_count(completed.stdout),
    }
    (run_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return summary


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    for label, path in (("Waymark", args.waymark_bin), ("Gateway", args.gateway_bin)):
        if not path.is_file():
            raise SystemExit(f"{label} binary not found: {path}")
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not (args.overwrite or args.reuse_existing):
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    run_root.mkdir(parents=True, exist_ok=True)
    if args.canary_source is not None:
        source_path = args.canary_source.resolve()
        if not source_path.is_file():
            raise SystemExit(f"canary source not found: {source_path}")
        source = source_path.read_text(encoding="utf-8")
        preflight = base.preflight_source(args.waymark_bin, source, run_root)
        fixture = (
            execute_with_fixture(args.waymark_bin, args.gateway_bin, source, run_root)
            if preflight.get("ok")
            else None
        )
        result = {
            "ok": bool(preflight.get("ok") and fixture and fixture.get("ok")),
            "source": str(source_path),
            "preflight": preflight,
            "fixture_execution": fixture,
        }
        (run_root / "canary.json").write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0 if result["ok"] else 1
    if args.repair_source is not None:
        source_path = args.repair_source.resolve()
        summary_path = args.repair_summary.resolve() if args.repair_summary else None
        if not source_path.is_file():
            raise SystemExit(f"repair source not found: {source_path}")
        if summary_path is None or not summary_path.is_file():
            raise SystemExit("--repair-source requires an existing --repair-summary")
        original = source_path.read_text(encoding="utf-8")
        prior_summary = json.loads(summary_path.read_text(encoding="utf-8"))
        prompt = repair_prompt(original, prior_summary, help_bundle(args.waymark_bin))
        models = args.models or DEFAULT_MODELS
        results = []
        for model in models:
            model_dir = run_root / base.safe_model_dir(model)
            result = evaluate_model(args, model, prompt, model_dir)
            repaired_path = model_dir / "agent.stone"
            repaired = repaired_path.read_text(encoding="utf-8") if repaired_path.is_file() else ""
            result["repair_changed_source"] = bool(repaired and repaired != original)
            result["ok"] = bool(result["ok"] and result["repair_changed_source"])
            result["status"] = "pass" if result["ok"] else "fail"
            (model_dir / "summary.json").write_text(
                json.dumps(result, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            results.append(result)
        aggregate = {
            "ok": all(result["ok"] for result in results),
            "kind": "one_diagnostic_repair",
            "prompt_version": PROMPT_VERSION,
            "fixture_version": FIXTURE_VERSION,
            "source": str(source_path),
            "prior_summary": str(summary_path),
            "results": results,
            "passed": sum(bool(result["ok"]) for result in results),
            "failed": sum(not bool(result["ok"]) for result in results),
            "total": len(results),
        }
        (run_root / "aggregate.json").write_text(
            json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        print(json.dumps(aggregate, indent=2, sort_keys=True))
        return 0 if aggregate["ok"] else 1
    prompt = authorship_prompt(help_bundle(args.waymark_bin))
    models = args.models or DEFAULT_MODELS
    results = [
        evaluate_model(args, model, prompt, run_root / base.safe_model_dir(model))
        for model in models
    ]
    aggregate = {
        "ok": all(result["ok"] for result in results),
        "prompt_version": PROMPT_VERSION,
        "fixture_version": FIXTURE_VERSION,
        "run_root": str(run_root),
        "fixture_sequence": FIXTURE_SEQUENCE,
        "results": results,
        "passed": sum(bool(result["ok"]) for result in results),
        "failed": sum(not bool(result["ok"]) for result in results),
        "total": len(results),
    }
    (run_root / "aggregate.json").write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(aggregate, indent=2, sort_keys=True))
    return 0 if aggregate["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
