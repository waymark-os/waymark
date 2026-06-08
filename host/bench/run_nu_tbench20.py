#!/usr/bin/env python3
"""Run the 20-task syntax experiment with Nushell as the agent language."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import run_stone_tbench4 as stone


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_NU_BIN = ROOT.parent / "nushell" / "target" / "debug" / "nu"
DEFAULT_TASKS = [
    "hello-world",
    "bank-trans-filter",
    "jsonl-aggregator",
    "log-summary",
    "recover-obfuscated-files",
    "schedule-vacation",
    "grid-pattern-transform",
    "mahjong-winninghand",
    "ilp-solver",
    "sha-puzzle",
    "analyze-access-logs",
    "heterogeneous-dates",
    "count-call-stack",
    "flood-monitoring-basic",
    "log-summary-date-ranges",
    "pandas-etl",
    "organization-json-generator",
    "recover-accuracy-log",
    "regex-log",
    "constraints-scheduling",
]


@dataclass
class Metrics:
    tool_calls: int = 0
    nu_eval_calls: int = 0
    nu_help_calls: int = 0
    syntax_errors: int = 0
    parse_errors: int = 0
    unsupported_feature_errors: int = 0
    runtime_errors: int = 0
    turns: int = 0
    input_tokens: int = 0
    output_tokens: int = 0


@dataclass
class RunContext:
    task_id: str
    task_dir: Path
    app_dir: Path
    work_dir: Path
    log_dir: Path
    instruction: str
    nu_bin: Path
    verify_mode: str
    metrics: Metrics = field(default_factory=Metrics)


def main() -> int:
    args = parse_args()
    nu_bin = args.nu_bin.resolve()
    if not nu_bin.exists():
        print(f"missing nu binary: {nu_bin}", file=sys.stderr)
        return 2

    out_dir = args.out_dir or default_out_dir()
    out_dir = out_dir.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    records = []
    started = time.monotonic()
    for task_id in args.tasks:
        ctx = prepare_task(
            task_id,
            Path(args.tasks_root),
            out_dir,
            nu_bin,
            args.fixture_cache_dir,
            args.verify_mode,
        )
        task_started = time.monotonic()
        try:
            record = run_agent(ctx, args)
        except Exception as err:
            record = record_harness_failure(
                ctx,
                "agent:nu",
                task_started,
                classify_harness_exception(err),
                f"{type(err).__name__}: {err}",
            )
        records.append(record)
        write_json_file(out_dir / "summary.json", summarize(records, started))
        write_summary_csv(out_dir, records)
        print(json.dumps(record, sort_keys=True), flush=True)

    print(f"wrote {out_dir / 'summary.json'}")
    return 0 if all(record["resolved"] for record in records) else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tasks-root", default=str(stone.DEFAULT_TASKS_ROOT))
    parser.add_argument("--tasks", nargs="+", default=DEFAULT_TASKS)
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--nu-bin", type=Path, default=DEFAULT_NU_BIN)
    parser.add_argument(
        "--fixture-cache-dir",
        type=Path,
        default=ROOT / "target" / "runs" / "stone-tbench4-fixtures",
    )
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--verify-mode", choices=("exact", "shape"), default="shape")
    parser.add_argument("--max-turns", type=int, default=10)
    parser.add_argument("--max-tokens", type=int, default=4096)
    parser.add_argument("--temperature", type=float, default=0.0)
    parser.add_argument("--top-p", type=float, default=0.95)
    parser.add_argument("--server", default=None)
    parser.add_argument("--model", default=os.environ.get("LLAMACPP_MODEL", "local"))
    parser.add_argument("--provider", choices=("openai-compatible", "openai"), default="openai-compatible")
    parser.add_argument("--api-key-env", default="OPENAI_API_KEY")
    parser.add_argument("--omit-sampling", action="store_true")
    args = parser.parse_args()
    if args.server is None:
        if args.provider == "openai":
            args.server = os.environ.get("OPENAI_SERVER", "https://api.openai.com")
        else:
            args.server = os.environ.get("LLAMACPP_SERVER", "http://127.0.0.1:8080")
    return args


def default_out_dir() -> Path:
    stamp = time.strftime("%Y%m%dT%H%M%S")
    return ROOT / "target" / "runs" / f"nu-tbench20-{stamp}"


def prepare_task(
    task_id: str,
    tasks_root: Path,
    out_dir: Path,
    nu_bin: Path,
    fixture_cache_dir: Path,
    verify_mode: str,
) -> RunContext:
    stone_ctx = stone.prepare_task(
        task_id,
        tasks_root,
        out_dir,
        Path("target/debug/waymark"),
        fixture_cache_dir,
        verify_mode,
    )
    return RunContext(
        task_id=stone_ctx.task_id,
        task_dir=stone_ctx.task_dir,
        app_dir=stone_ctx.app_dir,
        work_dir=stone_ctx.work_dir,
        log_dir=stone_ctx.log_dir,
        instruction=stone_ctx.instruction,
        nu_bin=nu_bin,
        verify_mode=verify_mode,
    )


def run_agent(ctx: RunContext, args: argparse.Namespace) -> dict[str, Any]:
    started = time.monotonic()
    messages = [
        {
            "role": "system",
            "content": (
                "You are a Nushell Terminal-Bench agent. Return only JSON action objects. "
                "Use Nushell for file, CSV, JSON, JSONL, text, glob/listing, and simple transform work."
            ),
        },
        {
            "role": "user",
            "content": (
                build_nu_prompt_guide()
                + "\n\nTask instruction:\n"
                + ctx.instruction
                + "\n\nHarness execution constraints:\n"
                + "- Each nu_eval call runs in a fresh Nushell process; variables do not persist between tool calls.\n"
                + "- Repeat needed reads and setup in the same nu_eval source that uses them.\n"
                + "- Use /app for task input and /work for output; these logical paths are rewritten by the harness.\n"
                + "- Use the verification details in observations to repair missing or extra output rows.\n"
                + "\n\nAction schema:\n"
                + '{"tool":"nu_eval","input":{"source":"..."} }\n'
                + '{"tool":"nu_help","input":{}} \n'
                + '{"tool":"finish","input":{}}\n'
                + "Use one action per turn. Do not finish until the required /work output exists."
            ),
        },
    ]
    final_observation: dict[str, Any] | None = None
    for turn in range(1, args.max_turns + 1):
        ctx.metrics.turns = turn
        try:
            text, usage = chat(args, messages, ctx.log_dir, turn)
        except Exception as err:
            failure_class = classify_harness_exception(err)
            observation = {
                "ok": False,
                "kind": failure_class,
                "error": f"{type(err).__name__}: {err}",
                "verification": verify_context(ctx),
            }
            write_json_file(ctx.log_dir / f"observation-{turn:03d}.json", observation)
            return record_result(
                ctx,
                "agent:nu",
                started,
                {"ok": False, "failure_class": failure_class, "failures": [observation["error"]]},
                observation["verification"],
            )
        ctx.metrics.input_tokens += usage.get("prompt_tokens", 0)
        ctx.metrics.output_tokens += usage.get("completion_tokens", 0)
        action, parse_error = stone.parse_action_json(text)
        write_json_file(ctx.log_dir / f"action-{turn:03d}.json", {"raw": text, "action": action, "error": parse_error})
        if parse_error:
            observation = {"ok": False, "error": parse_error, "guidance": "Return one JSON action object only."}
        else:
            observation = dispatch_action(ctx, action, args.timeout)
        write_json_file(ctx.log_dir / f"observation-{turn:03d}.json", observation)
        final_observation = observation
        messages.append({"role": "assistant", "content": text})
        messages.append(
            {
                "role": "user",
                "content": "Observation:\n"
                + stone.truncate(json.dumps(observation, indent=2, sort_keys=True), 12000)
                + "\nIf the task-required /work output exists and is correct, return finish. Otherwise continue.",
            }
        )
        if observation.get("finished"):
            break

    verification = verify_context(ctx)
    result = {"ok": bool(final_observation and final_observation.get("finished"))}
    return record_result(ctx, "agent:nu", started, result, verification)


def dispatch_action(ctx: RunContext, action: dict[str, Any], timeout_sec: float) -> dict[str, Any]:
    ctx.metrics.tool_calls += 1
    tool = action.get("tool")
    input_value = action.get("input") if isinstance(action.get("input"), dict) else {}
    if tool == "finish":
        verification = verify_context(ctx)
        return {"ok": verification["ok"], "finished": True, "verification": verification}
    if tool == "nu_help":
        ctx.metrics.nu_help_calls += 1
        return {"ok": True, "kind": "nu_help", "guide": build_nu_prompt_guide()}
    if tool == "nu_eval":
        source = input_value.get("source")
        if not isinstance(source, str):
            return {"ok": False, "error": "nu_eval requires input.source"}
        return nu_eval(ctx, source, timeout_sec=timeout_sec)
    return {"ok": False, "error": f"unsupported tool {tool!r}; use nu_eval, nu_help, or finish"}


def nu_eval(ctx: RunContext, source: str, timeout_sec: float) -> dict[str, Any]:
    ctx.metrics.nu_eval_calls += 1
    rewritten = stone.rewrite_logical_paths(source, ctx)
    try:
        result = subprocess.run(
            [str(ctx.nu_bin), "--no-config-file", "-c", rewritten],
            cwd=ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout_sec,
            check=False,
        )
    except subprocess.TimeoutExpired as err:
        ctx.metrics.runtime_errors += 1
        return timeout_observation("nu_eval", err, ctx)
    error_class = classify_nu_error(result)
    observation = {
        "ok": result.returncode == 0,
        "kind": "nu_eval",
        "returncode": result.returncode,
        "error_class": error_class,
        "stdout_tail": stone.tail(result.stdout),
        "stderr_tail": stone.tail(result.stderr),
        "verification": verify_context(ctx),
    }
    if error_class in ("parse", "unsupported_feature"):
        ctx.metrics.syntax_errors += 1
    if error_class == "parse":
        ctx.metrics.parse_errors += 1
    elif error_class == "unsupported_feature":
        ctx.metrics.unsupported_feature_errors += 1
    elif error_class == "runtime":
        ctx.metrics.runtime_errors += 1
    return observation


def classify_nu_error(result: subprocess.CompletedProcess[str]) -> str | None:
    if result.returncode == 0:
        return None
    text = (result.stderr + "\n" + result.stdout).lower()
    if any(needle in text for needle in ("parse error", "expected", "unexpected", "unclosed", "missing")):
        return "parse"
    if any(needle in text for needle in ("unknown command", "command not found", "external commands", "not found")):
        return "unsupported_feature"
    return "runtime"


def build_nu_prompt_guide() -> str:
    guide = {
        "language": "Nushell 0.112",
        "core_rules": [
            "Write Nushell source, not Python, Bash, or Stone.",
            "Pipelines pass typed values. Prefer structured commands over text parsing.",
            "Assignments use `let name = expr` and variables are referenced as `$name`.",
            "Blocks use braces: `if cond { ... } else { ... }`, `for row in $rows { ... }`.",
            "Records use `{name: value}`. Lists use `[a b c]` or multi-line entries.",
            "Save text with `'text' | save -f path`; save structured JSON with `$value | to json --raw | save -f path`.",
        ],
        "common_examples": [
            "'Hello, world!' | save -f /work/hello.txt",
            "let rows = open /app/data/input.csv | from csv",
            "let rows = open /app/input.jsonl | lines | each {|line| $line | from json }",
            "let filtered = $rows | where account in [BUS-30001 BUS-30g01]",
            "$rows | to json --raw | save -f /work/output.json",
            "$rows | to csv | save -f /work/summary.csv",
            "ls /app/**/*.log | each {|file| open $file.name }",
            "let parts = $line | split row ','",
            "mkdir /work/data",
        ],
        "avoid": [
            "Do not use Python imports, def syntax, colons for blocks, or Python list/dict comprehensions.",
            "Do not use Bash loops, awk, sed, grep, or shell redirection.",
            "Do not finish before writing the required /work outputs.",
        ],
    }
    return "Nushell guide:\n" + json.dumps(guide, indent=2, sort_keys=True)


def chat(args: argparse.Namespace, messages: list[dict[str, str]], log_dir: Path, turn: int) -> tuple[str, dict[str, int]]:
    body = {"model": args.model, "messages": messages}
    if args.provider == "openai":
        body["max_completion_tokens"] = args.max_tokens
    else:
        body["max_tokens"] = args.max_tokens
        body["chat_template_kwargs"] = {"enable_thinking": False}
    if not args.omit_sampling:
        body["temperature"] = args.temperature
        body["top_p"] = args.top_p
    write_json_file(log_dir / f"request-{turn:03d}.json", body)
    headers = {"Content-Type": "application/json"}
    if args.provider == "openai":
        token = os.environ.get(args.api_key_env)
        if not token:
            raise RuntimeError(f"--provider openai requires ${args.api_key_env}")
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(
        args.server.rstrip("/") + "/v1/chat/completions",
        data=json.dumps(body).encode("utf-8"),
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=args.timeout) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as err:
        detail = err.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"model HTTP {err.code}: {detail}") from err
    write_json_file(log_dir / f"response-{turn:03d}.json", payload)
    message = payload["choices"][0].get("message") or {}
    text = message.get("content") if isinstance(message.get("content"), str) else ""
    if not text:
        text = message.get("reasoning_content") or message.get("reasoning") or ""
    (log_dir / f"response-{turn:03d}.text").write_text(text)
    usage = payload.get("usage") if isinstance(payload.get("usage"), dict) else {}
    return text, {
        "prompt_tokens": int(usage.get("prompt_tokens") or 0),
        "completion_tokens": int(usage.get("completion_tokens") or 0),
    }


def verify_context(ctx: RunContext) -> dict[str, Any]:
    if ctx.verify_mode == "shape":
        return stone.verify_task_shape(ctx.task_id, ctx.app_dir, ctx.work_dir)
    return stone.verify_task(ctx.task_id, ctx.app_dir, ctx.work_dir)


def record_result(
    ctx: RunContext,
    mode: str,
    started: float,
    result: dict[str, Any],
    verification: dict[str, Any],
) -> dict[str, Any]:
    failures = []
    if isinstance(result.get("failures"), list):
        failures.extend(result["failures"])
    failures.extend(verification.get("failures", []))
    metrics = ctx.metrics
    return {
        "task_id": ctx.task_id,
        "mode": mode,
        "elapsed_sec": round(time.monotonic() - started, 3),
        "turns": metrics.turns,
        "tool_calls": metrics.tool_calls,
        "nu_eval_calls": metrics.nu_eval_calls,
        "nu_help_calls": metrics.nu_help_calls,
        "syntax_errors": metrics.syntax_errors,
        "parse_errors": metrics.parse_errors,
        "unsupported_feature_errors": metrics.unsupported_feature_errors,
        "runtime_errors": metrics.runtime_errors,
        "input_tokens": metrics.input_tokens,
        "output_tokens": metrics.output_tokens,
        "result_ok": bool(result.get("ok")),
        "resolved": bool(verification.get("ok")),
        "failure_class": result.get("failure_class") or verification.get("failure_class"),
        "failures": failures,
        "verify_mode": ctx.verify_mode,
    }


def record_harness_failure(
    ctx: RunContext,
    mode: str,
    started: float,
    failure_class: str,
    message: str,
) -> dict[str, Any]:
    verification = verify_context(ctx)
    return record_result(
        ctx,
        mode,
        started,
        {"ok": False, "failure_class": failure_class, "failures": [message]},
        verification,
    )


def summarize(records: list[dict[str, Any]], started: float) -> dict[str, Any]:
    tool_calls = sum(record["tool_calls"] for record in records)
    nu_eval_calls = sum(record["nu_eval_calls"] for record in records)
    return {
        "elapsed_sec": round(time.monotonic() - started, 3),
        "tasks": len(records),
        "resolved": sum(1 for record in records if record["resolved"]),
        "tool_calls": tool_calls,
        "nu_eval_calls": nu_eval_calls,
        "nu_help_calls": sum(record["nu_help_calls"] for record in records),
        "syntax_errors": sum(record["syntax_errors"] for record in records),
        "parse_errors": sum(record["parse_errors"] for record in records),
        "unsupported_feature_errors": sum(record["unsupported_feature_errors"] for record in records),
        "runtime_errors": sum(record["runtime_errors"] for record in records),
        "input_tokens": sum(record["input_tokens"] for record in records),
        "output_tokens": sum(record["output_tokens"] for record in records),
        "syntax_error_rate": sum(record["syntax_errors"] for record in records) / nu_eval_calls if nu_eval_calls else 0.0,
        "parse_error_rate": sum(record["parse_errors"] for record in records) / nu_eval_calls if nu_eval_calls else 0.0,
        "unsupported_feature_error_rate": sum(record["unsupported_feature_errors"] for record in records) / nu_eval_calls if nu_eval_calls else 0.0,
        "runtime_error_rate": sum(record["runtime_errors"] for record in records) / nu_eval_calls if nu_eval_calls else 0.0,
        "discovery_overhead": sum(record["nu_help_calls"] for record in records) / len(records) if records else 0.0,
        "records": records,
    }


def write_summary_csv(out_dir: Path, records: list[dict[str, Any]]) -> None:
    fields = [
        "task_id",
        "mode",
        "resolved",
        "failure_class",
        "turns",
        "tool_calls",
        "nu_eval_calls",
        "nu_help_calls",
        "syntax_errors",
        "parse_errors",
        "unsupported_feature_errors",
        "runtime_errors",
        "input_tokens",
        "output_tokens",
        "elapsed_sec",
    ]
    with (out_dir / "summary.csv").open("w", newline="") as file:
        import csv

        writer = csv.DictWriter(file, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(records)


def write_json_file(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def classify_harness_exception(err: Exception) -> str:
    if isinstance(err, subprocess.TimeoutExpired):
        return "harness_timeout"
    if isinstance(err, (urllib.error.URLError, TimeoutError)):
        return "model_connection_error"
    if isinstance(err, RuntimeError) and str(err).startswith("model HTTP "):
        return "model_http_error"
    return "harness_error"


def timeout_observation(kind: str, err: subprocess.TimeoutExpired, ctx: RunContext) -> dict[str, Any]:
    stdout = err.stdout if isinstance(err.stdout, str) else ""
    stderr = err.stderr if isinstance(err.stderr, str) else ""
    return {
        "ok": False,
        "kind": kind,
        "error_class": "timeout",
        "error": f"{kind} timed out after {err.timeout} seconds",
        "stdout_tail": stone.tail(stdout),
        "stderr_tail": stone.tail(stderr),
        "verification": verify_context(ctx),
    }


if __name__ == "__main__":
    raise SystemExit(main())
