#!/usr/bin/env python3
"""Evaluate whether an outer model can author a small Stone agent program."""

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


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
DEFAULT_MODELS = ["gpt-5.5", "gpt-5.6-terra", "gpt-5.6-luna"]
FIXTURE_RESPONSE = {"kind": "finish", "answer": "ready"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", action="append", dest="models")
    parser.add_argument("--codex", default="codex")
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=ROOT / "target" / "debug" / "waymark",
    )
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway" / "target" / "debug" / "waymark-gateway",
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target" / "runs" / "stone-agent-authorship-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument(
        "--reuse-existing",
        action="store_true",
        help="Reuse an existing model directory's last-message.json without another model call.",
    )
    return parser.parse_args()


def run_capture(
    command: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        env=env,
        timeout=timeout,
        text=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def help_topic(waymark_bin: Path, topic: str) -> dict[str, Any]:
    completed = run_capture(
        [str(waymark_bin), "eval", "-c", f'emit(help({json.dumps(topic)}))'],
        cwd=ROOT,
        timeout=30,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"help({topic!r}) failed: {completed.stderr}")
    response = json.loads(completed.stdout)
    value = response.get("value")
    if not isinstance(value, dict):
        raise RuntimeError(f"help({topic!r}) returned no record: {completed.stdout}")
    return value


def compact_help(topic: dict[str, Any]) -> dict[str, Any]:
    return {
        "signature": topic.get("signature"),
        "use_when": topic.get("use_when"),
        "avoid": topic.get("avoid"),
        "examples": topic.get("examples"),
    }


def authorship_prompt(model_help: dict[str, Any]) -> str:
    help_json = json.dumps(compact_help(model_help), separators=(",", ":"), sort_keys=True)
    return f"""You are the outer coding agent. Write one complete ad-hoc Stone program that acts as a bounded inner agent. Do not call tools and do not inspect the filesystem. Return only the required JSON object.

Stone is an LLM-oriented, Python-shaped structured shell language. It supports assignments, def/return, for/range, if/elif/else, try/except, lists, records, record field access, and ordinary function calls. It does not provide Python imports, classes, asyncio, or the Python standard library.

Relevant structured builtins:
- json_loads(text: str) -> Any
- json_dumps(value: Any) -> str
- run(argv: list[str], timeout_ms: int = ...) -> record with ok, exit_code, stdout, stderr
- fail(message: str, code: str = ...) -> never
- emit(value: Any) -> Any
- model_call help: {help_json}

Program contract:
1. Embed the task: ask the inner model to return exactly one JSON object with either {{"kind":"run","argv":[...]}} or {{"kind":"finish","answer":"..."}}. The task is to produce the final answer `ready`; a run action is optional.
2. Keep conversation messages as a Stone list of role/content records.
3. Use a for loop with at most 4 iterations and call model_call exactly once per iteration. The first externally visible effect in the program must be model_call.
4. Parse response.content with json_loads. On kind `run`, call run(action.argv, timeout_ms=5000), append the assistant response and a structured JSON observation, then continue.
5. On kind `finish`, return or emit a record containing answer and turns.
6. If the loop is exhausted, call fail with code `agent_turn_limit`.
7. Keep all prompts, retry/stopping behavior, and dispatch visible in Stone source. Do not use task_spec(), model_infer(), native tool calling, hidden helpers, imports, or comments claiming unavailable behavior.
8. Include a top-level call so the program executes, and emit its structured result.

The evaluator's deterministic model returns {{"kind":"finish","answer":"ready"}} on the first call. The program must therefore complete successfully without executing run in that fixture case.
"""


def output_schema() -> dict[str, Any]:
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"source": {"type": "string", "minLength": 1}},
        "required": ["source"],
        "additionalProperties": False,
    }


def codex_command(args: argparse.Namespace, model: str, run_dir: Path, prompt: str) -> list[str]:
    return [
        args.codex,
        "exec",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--json",
        "--model",
        model,
        "--cd",
        str(run_dir),
        "--output-schema",
        str(run_dir / "output-schema.json"),
        "--output-last-message",
        str(run_dir / "last-message.json"),
        prompt,
    ]


def parse_last_message(path: Path) -> tuple[str | None, str | None]:
    if not path.is_file():
        return None, "missing last-message.json"
    text = path.read_text(encoding="utf-8")
    try:
        value = json.loads(text)
    except json.JSONDecodeError as error:
        return None, f"invalid final JSON: {error}"
    if not isinstance(value, dict) or not isinstance(value.get("source"), str):
        return None, "final JSON does not contain string source"
    return value["source"], None


def response_payload(completed: subprocess.CompletedProcess[str]) -> dict[str, Any] | None:
    for text in (completed.stdout, completed.stderr):
        for line in reversed(text.splitlines()):
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict) and "ok" in value:
                return value
    return None


def preflight_source(waymark_bin: Path, source: str, cwd: Path) -> dict[str, Any]:
    completed = run_capture(
        [str(waymark_bin), "eval", "-c", source],
        cwd=cwd,
        timeout=30,
    )
    payload = response_payload(completed)
    error = payload.get("error", {}) if isinstance(payload, dict) else {}
    reached_model = isinstance(error, dict) and error.get("code") == "model_unavailable"
    return {
        "ok": reached_model,
        "reached_model_call": reached_model,
        "exit_code": completed.returncode,
        "response": payload,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }


def gateway_command(gateway_bin: Path, data_root: Path, *args: str) -> str:
    completed = run_capture(
        [str(gateway_bin), "--data-root", str(data_root), *args],
        timeout=30,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"Gateway command failed: {args}\nstdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed.stdout.strip()


def wait_for_socket(path: Path, process: subprocess.Popen[str]) -> None:
    deadline = time.time() + 10
    while time.time() < deadline:
        if process.poll() is not None:
            stderr = process.stderr.read() if process.stderr else ""
            raise RuntimeError(f"Gateway exited with {process.returncode}: {stderr}")
        if path.exists():
            return
        time.sleep(0.05)
    raise TimeoutError(f"Gateway socket did not appear: {path}")


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
    # AF_UNIX paths are capped at roughly 108 bytes on Linux. Experiment run
    # directories are intentionally descriptive, so keep only the socket in a
    # short-lived /tmp directory while all durable evidence stays in run_dir.
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-authorship-", dir="/tmp"))
    socket_path = socket_root / "gateway.sock"
    source.mkdir(parents=True)
    work.mkdir()
    (source / "README.md").write_text("agent authorship fixture\n", encoding="utf-8")
    gateway_command(gateway_bin, data_root, "repo", "snapshot", "--name", "repo", "--path", str(source))
    tx = gateway_command(gateway_bin, data_root, "env", "snapshot", "--workspace", "repo")

    server_env = dict(os.environ)
    server_env.update(
        {
            "WAYMARK_MODEL_PROVIDER": "fixture",
            "WAYMARK_MODEL_FIXTURE_TEXT": json.dumps(FIXTURE_RESPONSE, separators=(",", ":")),
        }
    )
    server = subprocess.Popen(
        [
            str(gateway_bin),
            "--data-root",
            str(data_root),
            "rpc",
            "serve",
            "--socket",
            str(socket_path),
        ],
        env=server_env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        wait_for_socket(socket_path, server)
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
        completed = run_capture(
            [str(waymark_bin), "eval", "-c", source_text],
            cwd=work,
            env=env,
            timeout=60,
        )
        payload = response_payload(completed)
        value = payload.get("value") if isinstance(payload, dict) else None
        answer = value.get("answer") if isinstance(value, dict) else None
        return {
            "ok": completed.returncode == 0
            and isinstance(payload, dict)
            and payload.get("ok") is True
            and answer == "ready",
            "exit_code": completed.returncode,
            "answer": answer,
            "response": payload,
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


def source_features(source: str) -> dict[str, bool]:
    return {
        "model_call": "model_call(" in source,
        "bounded_loop": bool(re.search(r"for\s+\w+\s+in\s+range\([1-4]\)", source)),
        "json_loads": "json_loads(" in source,
        "run_dispatch": "run(" in source,
        "finish_branch": "finish" in source,
        "turn_limit": "agent_turn_limit" in source,
        "top_level_emit": "emit(" in source,
        "forbidden_import": bool(re.search(r"(?m)^\s*(import|from)\s+", source)),
    }


def tool_call_count(events_text: str) -> int:
    count = 0
    for line in events_text.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        event_type = str(event.get("type", ""))
        if "tool" in event_type and "call" in event_type:
            count += 1
    return count


def invocation_unavailable_reason(events_text: str) -> str | None:
    for line in events_text.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        message = json.dumps(event, sort_keys=True)
        if "model is not supported" in message or "model_not_found" in message:
            return message
    return None


def evaluate_model(
    args: argparse.Namespace,
    model: str,
    prompt: str,
    run_dir: Path,
) -> dict[str, Any]:
    run_dir.mkdir(parents=True, exist_ok=args.overwrite or args.reuse_existing)
    (run_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_dir / "output-schema.json").write_text(
        json.dumps(output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = codex_command(args, model, run_dir, prompt)
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
            completed = run_capture(command, cwd=run_dir, timeout=args.timeout)
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
    source, output_error = parse_last_message(run_dir / "last-message.json")
    if source is not None:
        (run_dir / "agent.stone").write_text(source.rstrip() + "\n", encoding="utf-8")
    features = source_features(source or "")
    preflight = preflight_source(args.waymark_bin, source, run_dir) if source else None
    fixture = (
        execute_with_fixture(args.waymark_bin, args.gateway_bin, source, run_dir)
        if source and preflight and preflight.get("ok")
        else None
    )
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
    unavailable_reason = invocation_unavailable_reason(completed.stdout)
    passed = completed.returncode == 0 \
        and output_error is None \
        and required_features \
        and bool(preflight and preflight.get("ok")) \
        and bool(fixture and fixture.get("ok"))
    summary = {
        "ok": passed,
        "status": "pass" if passed else "unavailable" if unavailable_reason else "fail",
        "unavailable_reason": unavailable_reason,
        "model": model,
        "command": command,
        "codex_exit_code": completed.returncode,
        "timed_out": timed_out,
        "reused_existing": args.reuse_existing,
        "duration_seconds": duration,
        "output_error": output_error,
        "source_bytes": len((source or "").encode("utf-8")),
        "features": features,
        "required_features_ok": required_features,
        "preflight": preflight,
        "fixture_execution": fixture,
        "codex_tool_calls": tool_call_count(completed.stdout),
    }
    (run_dir / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return summary


def safe_model_dir(model: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", model)


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    for label, path in (("Waymark", args.waymark_bin), ("Gateway", args.gateway_bin)):
        if not path.is_file():
            raise SystemExit(f"{label} binary not found: {path}")
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not args.overwrite:
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    run_root.mkdir(parents=True, exist_ok=True)
    prompt = authorship_prompt(help_topic(args.waymark_bin, "model_call"))
    models = args.models or DEFAULT_MODELS
    results = [
        evaluate_model(args, model, prompt, run_root / safe_model_dir(model))
        for model in models
    ]
    aggregate = {
        "ok": all(result["ok"] for result in results),
        "run_root": str(run_root),
        "fixture_response": FIXTURE_RESPONSE,
        "results": results,
        "passed": sum(bool(result["ok"]) for result in results),
        "failed": sum(result["status"] == "fail" for result in results),
        "unavailable": sum(result["status"] == "unavailable" for result in results),
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
