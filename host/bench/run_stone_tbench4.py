#!/usr/bin/env python3
"""Run a small Stone-first Terminal-Bench syntax experiment.

This harness intentionally avoids Terminal-Bench and containers for the first
iteration. It builds local /app and /work directories for four starter tasks,
executes Stone through `waymark eval`, and can drive a simple ReAct loop against
an OpenAI-compatible local model server.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
import zipfile
from datetime import datetime, timedelta
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_TASKS_ROOT = ROOT.parent / "terminal-bench" / "original-tasks"
DEFAULT_TASKS = ("hello-world", "bank-trans-filter", "jsonl-aggregator", "log-summary")
METADATA_FILES = {
    "Dockerfile",
    "docker-compose.yaml",
    "run-tests.sh",
    "solution.sh",
    "task.yaml",
    "tests",
    "task-deps",
    "task_deps",
}

EXPECTED_BANK_TRANSACTIONS = [
    {
        "Company Name": "North West Capital Group Incorporated",
        "Account Number": "BUS-30001",
        "Date": "2024-01-22",
        "Amount": "-$197887.00",
        "Transaction Type": "Payment",
        "Description": "Professional Services",
    },
    {
        "Company Name": "NWCG Inc. Pty. Ltd.",
        "Account Number": "BUS-30g01",
        "Date": "2024-02-02",
        "Amount": "-$56760.00",
        "Transaction Type": "Payment",
        "Description": "Consulting Fee",
    },
    {
        "Company Name": "NWCG Inc. Pty. Ltd.",
        "Account Number": "BUS-30001",
        "Date": "2024-03-01",
        "Amount": "-$84377.00",
        "Transaction Type": "Purchase",
        "Description": "Office Supplies",
    },
    {
        "Company Name": "North West Capital Group Incoporated",
        "Account Number": "BUS-30999",
        "Date": "2024-03-15",
        "Amount": "-$22413.00",
        "Transaction Type": "Withdrawal",
        "Description": "Office Supplies",
    },
    {
        "Company Name": "NWCG Inc. Pty. Ltd.",
        "Account Number": "BUS-30001",
        "Date": "2024-03-16",
        "Amount": "-$205706.00",
        "Transaction Type": "Purchase",
        "Description": "Software License",
    },
    {
        "Company Name": "North West Capital Group Incorporated",
        "Account Number": "BUS-30001",
        "Date": "2024-03-20",
        "Amount": "$232005.00",
        "Transaction Type": "Deposit",
        "Description": "Quarterly Earnings",
    },
    {
        "Company Name": "North West Capital Group Incorporated",
        "Account Number": "BUS-30001",
        "Date": "2024-04-04",
        "Amount": "$279375.00",
        "Transaction Type": "Transfer",
        "Description": "Inter-company Transfer",
    },
    {
        "Company Name": "North West Capital Group Incorporated",
        "Account Number": "BUS-30001",
        "Date": "2024-04-10",
        "Amount": "-$125251.00",
        "Transaction Type": "Withdrawal",
        "Description": "Payroll",
    },
    {
        "Company Name": "North West Capital Group Incorporated",
        "Account Number": "BUS-35001",
        "Date": "2024-04-13",
        "Amount": "$121274.00",
        "Transaction Type": "Transfer",
        "Description": "Partner Distribution",
    },
]

EXPECTED_JSONL_AGGREGATES = {
    "top_5_users_by_amount": {
        "user211": {"total_amount": 2131992.47, "total_items": 149035},
        "user220_dev": {"total_amount": 2084232.55, "total_items": 151865},
        "chloe_test": {"total_amount": 2054175.28, "total_items": 147752},
        "user260_123": {"total_amount": 2035431.62, "total_items": 149640},
        "user271": {"total_amount": 2034668.79, "total_items": 131998},
    },
    "top_5_tags_by_count": {
        "lime": {"count": 28874},
        "type_a": {"count": 28793},
        "reporting": {"count": 28781},
        "frontend": {"count": 28694},
        "approved": {"count": 28665},
    },
}

EXPECTED_LOG_SUMMARY = {"ERROR": 4, "WARNING": 3, "INFO": 8}

EXPECTED_REPORT = (
    "Total requests: 2000\n"
    "Unique IP addresses: 273\n"
    "Top 3 URLs:\n"
    "  /order-confirmation: 54\n"
    "  /product/456: 53\n"
    "  /about.html: 52\n"
    "404 errors: 83\n"
)

EXPECTED_RECOVERED_FILES = {
    "project_alpha.log": (
        "Log entry 1: System initialized.\n"
        "Log entry 2: User 'admin' logged in.\n"
        "Log entry 3: Critical operation started.\n"
    ),
    "credentials.txt": "user: test_user\npass: P0$$wOrd123!\nserver: 192.168.1.100\n",
}

EXPECTED_MAHJONG = {
    "hand_001.json": {"Winning hand - Seven Pairs", "Winning hand - Standard"},
    "hand_002.json": {"Winning hand - Thirteen Orphans"},
    "hand_003.json": {"Not a winning hand"},
    "hand_004.json": {"Winning hand - Seven Pairs"},
    "hand_005.json": {"Winning hand - Seven Pairs", "Winning hand - Standard"},
    "hand_006.json": {"Winning hand - Standard"},
    "hand_007.json": {"Winning hand - Seven Pairs", "Winning hand - Standard"},
    "hand_008.json": {"Winning hand - Seven Pairs"},
}

EXPECTED_LOG_DATE_ROWS = [
    ["today", "ERROR", "370"],
    ["today", "WARNING", "463"],
    ["today", "INFO", "1433"],
    ["last_7_days", "ERROR", "2969"],
    ["last_7_days", "WARNING", "4020"],
    ["last_7_days", "INFO", "12203"],
    ["last_30_days", "ERROR", "9594"],
    ["last_30_days", "WARNING", "12847"],
    ["last_30_days", "INFO", "38702"],
    ["month_to_date", "ERROR", "4682"],
    ["month_to_date", "WARNING", "6327"],
    ["month_to_date", "INFO", "19420"],
    ["total", "ERROR", "14160"],
    ["total", "WARNING", "18772"],
    ["total", "INFO", "56621"],
]

GOLD_STONE = {
    "hello-world": """\
open("/work/hello.txt", "w").write("Hello, world!\\n")
emit({"path": "/work/hello.txt"})
""",
    "bank-trans-filter": """\
mkdir("/work/data")

target_accounts = ["BUS-30001", "BUS-30g01", "BUS-30999", "BUS-35001"]
matches = []

for row in read_csv("/app/data/bank_transactions.csv"):
    if row["Account Number"] in target_accounts:
        matches.append(row)

matches = sort(matches, key="Date")
write_json("/work/data/output.json", matches)
emit({"count": len(matches), "path": "/work/data/output.json"})
""",
    "jsonl-aggregator": """\
user_amounts = {}
user_items = {}
users = []
tag_counts = {}
tags = []

for file in find("/app", name_glob="records_*.jsonl"):
    for record in read_jsonl(file["path"]):
        user = record["user"]
        if user in user_amounts:
            user_amounts[user] = user_amounts[user] + record["amount"]
            user_items[user] = user_items[user] + record["items"]
        else:
            user_amounts[user] = record["amount"]
            user_items[user] = record["items"]
            users.append(user)

        for tag in record["tags"]:
            if tag in tag_counts:
                tag_counts[tag] = tag_counts[tag] + 1
            else:
                tag_counts[tag] = 1
                tags.append(tag)

selected_users = []
top_users = {}
for i in range(5):
    best_user = ""
    best_amount = -1.0
    for user in users:
        amount = user_amounts[user]
        if user not in selected_users and amount > best_amount:
            best_user = user
            best_amount = amount
    selected_users.append(best_user)
    top_users[best_user] = {
        "total_amount": round(best_amount, 2),
        "total_items": user_items[best_user],
    }

selected_tags = []
top_tags = {}
for i in range(5):
    best_tag = ""
    best_count = -1
    for tag in tags:
        count = tag_counts[tag]
        if tag not in selected_tags and count > best_count:
            best_tag = tag
            best_count = count
    selected_tags.append(best_tag)
    top_tags[best_tag] = {"count": best_count}

result = {
    "top_5_users_by_amount": top_users,
    "top_5_tags_by_count": top_tags,
}
write_json("/work/aggregates.json", result)
emit({"path": "/work/aggregates.json", "users": len(users), "tags": len(tags)})
""",
    "log-summary": """\
error_count = 0
warning_count = 0
info_count = 0

for file in find("/app/logs", name_glob="*.log"):
    for line in open(file["path"]):
        if line.find("ERROR") != -1:
            error_count = error_count + 1
        if line.find("WARNING") != -1:
            warning_count = warning_count + 1
        if line.find("INFO") != -1:
            info_count = info_count + 1

out = "severity,count\\n"
out = out + "ERROR," + str(error_count) + "\\n"
out = out + "WARNING," + str(warning_count) + "\\n"
out = out + "INFO," + str(info_count) + "\\n"
open("/work/summary.csv", "w").write(out)
emit({"path": "/work/summary.csv", "ERROR": error_count, "WARNING": warning_count, "INFO": info_count})
""",
}


@dataclass
class Metrics:
    tool_calls: int = 0
    stone_eval_calls: int = 0
    stone_help_calls: int = 0
    escape_linux_calls: int = 0
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
    waymark_bin: Path
    verify_mode: str
    metrics: Metrics = field(default_factory=Metrics)


def main() -> int:
    args = parse_args()
    waymark_bin = Path(args.waymark_bin)
    if not waymark_bin.is_absolute():
        waymark_bin = ROOT / waymark_bin
    if not waymark_bin.exists():
        print(f"missing waymark binary: {waymark_bin}", file=sys.stderr)
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
            waymark_bin,
            args.fixture_cache_dir,
            args.verify_mode,
        )
        task_started = time.monotonic()
        try:
            if args.mode == "gold":
                record = run_gold(ctx, timeout_sec=args.timeout)
            else:
                record = run_agent(ctx, args)
        except Exception as err:
            record = record_harness_failure(
                ctx,
                args.mode if args.mode == "gold" else f"agent:{args.prompt_style}",
                task_started,
                classify_harness_exception(err),
                f"{type(err).__name__}: {err}",
            )
        records.append(record)
        write_json_file(out_dir / "summary.json", summarize(records, started))
        write_summary_csv(out_dir / "summary.csv", records)
        print(json.dumps(record, sort_keys=True))

    print(f"wrote {out_dir / 'summary.json'}")
    return 0 if all(record["resolved"] for record in records) else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("gold", "agent"), default="gold")
    parser.add_argument("--tasks-root", default=str(DEFAULT_TASKS_ROOT))
    parser.add_argument("--tasks", nargs="+", default=list(DEFAULT_TASKS))
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--waymark-bin", default="target/debug/waymark")
    parser.add_argument("--fixture-cache-dir", type=Path, default=ROOT / "target" / "runs" / "stone-tbench4-fixtures")
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--verify-mode", choices=("exact", "shape"), default="exact")
    parser.add_argument("--max-turns", type=int, default=8)
    parser.add_argument("--max-tokens", type=int, default=4096)
    parser.add_argument("--temperature", type=float, default=0.0)
    parser.add_argument("--top-p", type=float, default=0.95)
    parser.add_argument("--server", default=None)
    parser.add_argument("--model", default=os.environ.get("LLAMACPP_MODEL", "local"))
    parser.add_argument(
        "--provider",
        choices=("openai-compatible", "openai"),
        default="openai-compatible",
        help="Use openai for api.openai.com auth and Chat Completions token parameter shape.",
    )
    parser.add_argument(
        "--api-key-env",
        default="OPENAI_API_KEY",
        help="Environment variable containing the bearer token when --provider openai is used.",
    )
    parser.add_argument(
        "--omit-sampling",
        action="store_true",
        help="Do not send temperature/top_p; useful for models that reject sampling controls.",
    )
    parser.add_argument(
        "--prompt-style",
        choices=("full", "no-negatives", "no-examples", "minimal"),
        default="full",
    )
    args = parser.parse_args()
    if args.server is None:
        if args.provider == "openai":
            args.server = os.environ.get("OPENAI_SERVER", "https://api.openai.com")
        else:
            args.server = os.environ.get("LLAMACPP_SERVER", "http://127.0.0.1:8080")
    return args


def default_out_dir() -> Path:
    stamp = time.strftime("%Y%m%dT%H%M%S")
    return ROOT / "target" / "runs" / f"stone-tbench4-{stamp}"


def prepare_task(
    task_id: str,
    tasks_root: Path,
    out_dir: Path,
    waymark_bin: Path,
    fixture_cache_dir: Path,
    verify_mode: str,
) -> RunContext:
    task_dir = tasks_root / task_id
    if not task_dir.exists():
        raise FileNotFoundError(task_dir)
    run_dir = out_dir / task_id
    app_dir = run_dir / "app"
    work_dir = run_dir / "work"
    log_dir = run_dir / "logs"
    if run_dir.exists():
        shutil.rmtree(run_dir)
    app_dir.mkdir(parents=True)
    work_dir.mkdir()
    log_dir.mkdir()

    cached_app = fixture_cache_dir.resolve() / task_id / "app"
    if task_id == "jsonl-aggregator" and (cached_app / "records_5.jsonl").exists():
        shutil.copytree(cached_app, app_dir, dirs_exist_ok=True)
    else:
        for item in task_dir.iterdir():
            if item.name in METADATA_FILES:
                continue
            dest = app_dir / item.name
            if item.is_dir():
                shutil.copytree(item, dest)
            else:
                shutil.copy2(item, dest)
    if task_id == "jsonl-aggregator":
        if not (app_dir / "records_5.jsonl").exists():
            generator = task_dir / "task-deps" / "generate_records.py"
            run_checked(["python3", str(generator)], cwd=app_dir, timeout_sec=120.0)
        if not (cached_app / "records_5.jsonl").exists():
            cached_app.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(app_dir, cached_app, dirs_exist_ok=True)
    elif task_id == "heterogeneous-dates":
        task_deps = task_dir / "task-deps"
        for name in ("daily_temp_sf_high.csv", "daily_temp_sf_low.csv"):
            shutil.copy2(task_deps / name, app_dir / name)
    elif task_id == "recover-accuracy-log":
        task_deps = task_dir / "task_deps"
        raw_logs = app_dir / "raw_logs"
        raw_logs.mkdir()
        for name in ("generator.log", "judge.log"):
            shutil.copy2(task_deps / name, raw_logs / name)
    elif task_id == "recover-obfuscated-files":
        sensitive = app_dir / "sensitive_data"
        sensitive.mkdir()
        for item in (task_dir / "setup_files").iterdir():
            shutil.copy2(item, sensitive / item.name)
    elif task_id == "mahjong-winninghand":
        protected = app_dir / "protected"
        protected.mkdir(exist_ok=True)
        for item in (task_dir / "protected").glob("*.json"):
            shutil.copy2(item, protected / item.name)
    elif task_id == "count-call-stack":
        with zipfile.ZipFile(task_dir / "log.stack.zip") as archive:
            archive.extractall(app_dir)
    elif task_id == "flood-monitoring-basic":
        data_dir = app_dir / "data"
        data_dir.mkdir()
        materialize_flood_data(task_dir / "Dockerfile", data_dir)
    elif task_id == "log-summary-date-ranges":
        generator = (task_dir / "log_generator_deterministic.py").read_text()
        generator = generator.replace('output_dir = "/app/logs"', f"output_dir = {str(app_dir / 'logs')!r}")
        run_checked([sys.executable, "-c", generator], cwd=ROOT, timeout_sec=120.0)
    elif task_id == "pandas-etl":
        shutil.copy2(task_dir / "task-deps" / "data.csv", app_dir / "data.csv")
    elif task_id == "constraints-scheduling":
        for item in (task_dir / "inputs").glob("*.ics"):
            shutil.copy2(item, app_dir / item.name)

    instruction = adapted_prompt(task_id, read_task_instruction(task_dir / "task.yaml"))
    (run_dir / "instruction.txt").write_text(instruction)
    return RunContext(task_id, task_dir, app_dir, work_dir, log_dir, instruction, waymark_bin, verify_mode)


def read_task_instruction(task_yaml: Path) -> str:
    lines = task_yaml.read_text().splitlines()
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped in ("instruction: |", "instruction: |-", "instruction: |+"):
            block = []
            indent = None
            for raw in lines[index + 1 :]:
                if not raw.strip():
                    block.append("")
                    continue
                raw_indent = len(raw) - len(raw.lstrip(" "))
                if indent is None:
                    indent = raw_indent
                if raw_indent < indent:
                    break
                block.append(raw[indent:])
            return "\n".join(block).strip()
        if stripped.startswith("instruction: "):
            return stripped.split(":", 1)[1].strip().strip("'\"")
    raise ValueError(f"could not find instruction in {task_yaml}")


def materialize_flood_data(dockerfile: Path, data_dir: Path) -> None:
    current: Path | None = None
    for raw in dockerfile.read_text().splitlines():
        match = re.search(r'echo "([^"]*)" (>|>>) /data/(station_[ABC]\.csv)', raw)
        if not match:
            continue
        text, op, name = match.groups()
        path = data_dir / name
        if op == ">" or current != path:
            path.write_text(text + "\n")
        else:
            with path.open("a") as file:
                file.write(text + "\n")
        current = path


def adapted_prompt(task_id: str, instruction: str) -> str:
    mapping = {
        "hello-world": "Create /work/hello.txt instead of /app/hello.txt.",
        "bank-trans-filter": "Read /app/data/bank_transactions.csv, but write /work/data/output.json instead of /app/data/output.json.",
        "jsonl-aggregator": "Read /app/records_*.jsonl, but write /work/aggregates.json instead of /app/aggregates.json.",
        "log-summary": "Read /app/logs/*.log, but write /work/summary.csv instead of /app/summary.csv.",
        "analyze-access-logs": "Read /app/access_log, but write /work/report.txt instead of /app/report.txt.",
        "heterogeneous-dates": "Read /app/daily_temp_sf_high.csv and /app/daily_temp_sf_low.csv, but write /work/avg_temp.txt instead of /app/avg_temp.txt.",
        "recover-accuracy-log": "Read /app/raw_logs/generator.log and /app/raw_logs/judge.log, but write recovered outputs under /work/recovered_logs instead of /app/recovered_logs.",
        "recover-obfuscated-files": "Read /app/sensitive_data and /app/NOTE_FROM_ATTACKER.txt, but write recovered files under /work/recovered instead of /app/recovered.",
        "schedule-vacation": "Read and run helper scripts under /app, but write generated files under /work: schedules, all_availability.json, overlap_results.json, and vacation_dates.txt.",
        "grid-pattern-transform": "Create /work/grid_transform.py instead of /app/grid_transform.py.",
        "mahjong-winninghand": "Read JSON hands under /app/protected, but write /work/result.txt instead of /app/result.txt.",
        "ilp-solver": "Write /work/ilp_output.txt instead of /app/ilp_output.txt.",
        "sha-puzzle": "Write /work/solution.txt instead of /app/solution.txt.",
        "count-call-stack": "Read /app/log.stack, but write /work/output.txt instead of /app/output.txt.",
        "flood-monitoring-basic": "Read and update CSV files under /app/data instead of /data, and write /work/flood_summary.json instead of /app/flood_summary.json.",
        "log-summary-date-ranges": "Read /app/logs, but write /work/summary.csv instead of /app/summary.csv.",
        "pandas-etl": "Read /app/data.csv, but write /work/result.csv instead of /app/result.csv.",
        "organization-json-generator": "Read /app/departments.csv, /app/employees.csv, /app/projects.csv, and /app/schema.json, but write /work/organization.json instead of /app/organization.json.",
        "regex-log": "Write /work/regex.txt instead of /app/regex.txt.",
        "constraints-scheduling": "Read the three /app/*_calendar.ics files without modifying them, but write /work/meeting_scheduled.ics instead of /app/meeting_scheduled.ics.",
    }
    harness_note = "/app is task input; /work is writable output."
    if task_id != "flood-monitoring-basic":
        harness_note = "/app is read-only task input; /work is writable output."
    return (
        f"Terminal-Bench task: {task_id}\n\n"
        f"Original instruction:\n{instruction}\n\n"
        f"Local harness adaptation: {harness_note} "
        f"{mapping[task_id]}"
    )


def run_gold(ctx: RunContext, timeout_sec: float) -> dict[str, Any]:
    started = time.monotonic()
    source = GOLD_STONE.get(ctx.task_id)
    if source is None:
        return record_harness_failure(
            ctx,
            "gold",
            started,
            "harness_error",
            f"gold mode has no built-in Stone solution for task {ctx.task_id!r}",
        )
    result = stone_eval(ctx, source, timeout_sec=timeout_sec)
    verification = verify_context(ctx)
    return record_result(ctx, "gold", started, result, verification)


def run_agent(ctx: RunContext, args: argparse.Namespace) -> dict[str, Any]:
    started = time.monotonic()
    guide = build_prompt_guide(ctx.waymark_bin, args.prompt_style)
    messages = [
        {
            "role": "system",
            "content": (
                "You are a Stone Terminal-Bench agent. Return only JSON action objects. "
                "Use Stone for file, CSV, JSON, JSONL, text, find/glob, and simple transform work."
            ),
        },
        {
            "role": "user",
            "content": (
                guide
                + "\n\nTask instruction:\n"
                + ctx.instruction
                + "\n\nHarness execution constraints:\n"
                + "- Each stone_eval call runs in a fresh Waymark CLI process; variables do not persist between tool calls.\n"
                + "- Repeat needed reads and setup in the same stone_eval source that uses them.\n"
                + "- Use the verification details in observations to repair missing or extra output rows.\n"
                + "\n\nAction schema:\n"
                + '{"tool":"stone_eval","input":{"source":"..."} }\n'
                + '{"tool":"stone_help","input":{"name":"optional_builtin_or_topic"} }\n'
                + '{"tool":"escape_linux","input":{"command":"...","cwd":"/app","timeout_ms":30000} }\n'
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
                f"agent:{args.prompt_style}",
                started,
                {"ok": False, "failure_class": failure_class, "failures": [observation["error"]]},
                observation["verification"],
            )
        ctx.metrics.input_tokens += usage.get("prompt_tokens", 0)
        ctx.metrics.output_tokens += usage.get("completion_tokens", 0)
        action, parse_error = parse_action_json(text)
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
                + truncate(json.dumps(observation, indent=2, sort_keys=True), 12000)
                + "\nIf the task-required /work output exists and is correct, return finish. Otherwise continue.",
            }
        )
        if observation.get("finished"):
            break

    verification = verify_context(ctx)
    result = {"ok": bool(final_observation and final_observation.get("finished"))}
    return record_result(ctx, f"agent:{args.prompt_style}", started, result, verification)


def dispatch_action(ctx: RunContext, action: dict[str, Any], timeout_sec: float) -> dict[str, Any]:
    ctx.metrics.tool_calls += 1
    tool = action.get("tool")
    input_value = action.get("input") if isinstance(action.get("input"), dict) else {}
    if tool == "finish":
        verification = verify_context(ctx)
        return {"ok": verification["ok"], "finished": True, "verification": verification}
    if tool == "stone_help":
        ctx.metrics.stone_help_calls += 1
        name = input_value.get("name")
        return stone_help(ctx, name if isinstance(name, str) and name else None, timeout_sec)
    if tool == "stone_eval":
        source = input_value.get("source")
        if not isinstance(source, str):
            return {"ok": False, "error": "stone_eval requires input.source"}
        return stone_eval(ctx, source, timeout_sec=timeout_sec)
    if tool == "escape_linux":
        command = input_value.get("command")
        cwd = input_value.get("cwd", "/app")
        timeout_ms = input_value.get("timeout_ms", int(timeout_sec * 1000))
        if not isinstance(command, str):
            return {"ok": False, "error": "escape_linux requires input.command"}
        if not isinstance(cwd, str):
            return {"ok": False, "error": "escape_linux input.cwd must be a string"}
        if not isinstance(timeout_ms, int):
            return {"ok": False, "error": "escape_linux input.timeout_ms must be an integer"}
        return escape_linux(ctx, command, cwd, timeout_ms)
    return {"ok": False, "error": f"unsupported tool {tool!r}"}


def stone_eval(ctx: RunContext, source: str, timeout_sec: float) -> dict[str, Any]:
    ctx.metrics.stone_eval_calls += 1
    rewritten = rewrite_logical_paths(source, ctx)
    try:
        result = run_waymark(ctx.waymark_bin, ["eval", "-c", rewritten], cwd=ROOT, timeout_sec=timeout_sec)
    except subprocess.TimeoutExpired as err:
        ctx.metrics.runtime_errors += 1
        return timeout_observation("stone_eval", err, ctx)
    response = sanitize_waymark_response(parse_waymark_response(result.stdout) or parse_waymark_response(result.stderr))
    stdout = sanitize_waymark_output(result.stdout)
    stderr = sanitize_waymark_output(result.stderr)
    error_class = classify_stone_error(response)
    observation = {
        "ok": result.returncode == 0 and bool(response and response.get("ok")),
        "kind": "stone_eval",
        "returncode": result.returncode,
        "error_class": error_class,
        "response": response,
        "stdout_tail": tail(stdout),
        "stderr_tail": tail(stderr),
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


def stone_help(ctx: RunContext, name: str | None, timeout_sec: float) -> dict[str, Any]:
    source = "emit(help())" if name is None else f"emit(help({json.dumps(name)}))"
    try:
        result = run_waymark(ctx.waymark_bin, ["eval", "-c", source], cwd=ROOT, timeout_sec=timeout_sec)
    except subprocess.TimeoutExpired as err:
        return timeout_observation("stone_help", err, ctx)
    response = parse_waymark_response(result.stdout)
    return {
        "ok": result.returncode == 0 and bool(response and response.get("ok")),
        "kind": "stone_help",
        "response": response,
        "stderr_tail": tail(result.stderr),
    }


def escape_linux(ctx: RunContext, command: str, cwd: str, timeout_ms: int) -> dict[str, Any]:
    ctx.metrics.escape_linux_calls += 1
    host_cwd = logical_path_to_host(cwd, ctx)
    try:
        result = run_checked_result(
            ["bash", "-lc", rewrite_logical_paths(command, ctx)],
            cwd=host_cwd,
            timeout_sec=max(1.0, timeout_ms / 1000.0),
        )
    except subprocess.TimeoutExpired as err:
        return timeout_observation("escape_linux", err, ctx)
    return {
        "ok": result.returncode == 0,
        "kind": "escape_linux",
        "returncode": result.returncode,
        "stdout_tail": tail(result.stdout),
        "stderr_tail": tail(result.stderr),
        "verification": verify_context(ctx),
    }


def build_prompt_guide(waymark_bin: Path, prompt_style: str) -> str:
    result = run_waymark(waymark_bin, ["eval", "-c", "emit(help())"], cwd=ROOT, timeout_sec=30.0)
    response = parse_waymark_response(result.stdout)
    if not response or not response.get("ok"):
        raise RuntimeError("could not read Stone help()")
    guide = response["value"]
    if prompt_style in ("full", "no-negatives", "no-examples"):
        guide = dict(guide)
        details = []
        for item in guide.get("builtins", []):
            name = item.get("name")
            if not isinstance(name, str):
                continue
            detail_result = run_waymark(
                waymark_bin,
                ["eval", "-c", f"emit(help({json.dumps(name)}))"],
                cwd=ROOT,
                timeout_sec=30.0,
            )
            detail_response = parse_waymark_response(detail_result.stdout)
            if detail_response and detail_response.get("ok"):
                details.append(detail_response["value"])
        guide["builtin_details"] = details
    if prompt_style == "no-negatives":
        guide = {key: value for key, value in guide.items() if key != "unsupported"}
    elif prompt_style == "no-examples":
        guide = strip_examples(guide)
    elif prompt_style == "minimal":
        guide = {
            "language": "Stone",
            "builtins": [
                {"name": item["name"], "signature": item["signature"]}
                for item in guide.get("builtins", [])
            ],
        }
    return "Stone language guide:\n" + json.dumps(guide, indent=2, sort_keys=True)


def strip_examples(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: strip_examples(inner) for key, inner in value.items() if key != "examples"}
    if isinstance(value, list):
        return [strip_examples(item) for item in value]
    return value


def chat(args: argparse.Namespace, messages: list[dict[str, str]], log_dir: Path, turn: int) -> tuple[str, dict[str, int]]:
    body = {
        "model": args.model,
        "messages": messages,
    }
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


def parse_action_json(text: str) -> tuple[dict[str, Any], str | None]:
    stripped = text.strip()
    try:
        value = json.loads(stripped)
    except json.JSONDecodeError:
        match = re.search(r"\{.*\}", stripped, re.DOTALL)
        if not match:
            return {}, "response did not contain a JSON object"
        try:
            value = json.loads(match.group(0))
        except json.JSONDecodeError as err:
            return {}, f"invalid JSON action: {err}"
    if isinstance(value, dict) and isinstance(value.get("actions"), list):
        actions = value["actions"]
        if not actions:
            return {}, "actions list is empty"
        value = actions[0]
    if not isinstance(value, dict):
        return {}, "JSON action must be an object"
    return value, None


def run_waymark(waymark_bin: Path, args: list[str], cwd: Path, timeout_sec: float) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(waymark_bin), *args],
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout_sec,
        check=False,
    )


def run_checked(argv: list[str], cwd: Path, timeout_sec: float) -> None:
    result = run_checked_result(argv, cwd, timeout_sec)
    if result.returncode != 0:
        raise RuntimeError(f"{argv} failed:\n{result.stderr}")


def run_checked_result(argv: list[str], cwd: Path, timeout_sec: float) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout_sec,
        check=False,
    )


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
        "stdout_tail": tail(stdout),
        "stderr_tail": tail(stderr),
        "verification": verify_context(ctx),
    }


def parse_waymark_response(stdout: str) -> dict[str, Any] | None:
    for line in reversed(stdout.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and "ok" in value:
            return value
    return None


def sanitize_waymark_output(text: str) -> str:
    lines = []
    for line in text.splitlines():
        try:
            value = json.loads(line.strip())
        except json.JSONDecodeError:
            lines.append(line)
            continue
        if isinstance(value, dict) and "ok" in value:
            value = sanitize_waymark_response(value)
            lines.append(json.dumps(value, sort_keys=True))
        else:
            lines.append(line)
    if not lines:
        return ""
    suffix = "\n" if text.endswith("\n") else ""
    return "\n".join(lines) + suffix


def sanitize_waymark_response(response: dict[str, Any] | None) -> dict[str, Any] | None:
    if not isinstance(response, dict):
        return response
    diagnostics = response.get("diagnostics")
    if not isinstance(diagnostics, dict) or "session" not in diagnostics:
        return response
    sanitized = dict(response)
    sanitized_diagnostics = dict(diagnostics)
    sanitized_diagnostics.pop("session", None)
    if sanitized_diagnostics:
        sanitized["diagnostics"] = sanitized_diagnostics
    else:
        sanitized.pop("diagnostics", None)
    return sanitized


def rewrite_logical_paths(source: str, ctx: RunContext) -> str:
    replacements = {
        "/app": str(ctx.app_dir),
        "/work": str(ctx.work_dir),
        "/data": str(ctx.app_dir / "data"),
    }

    def replace_root(match: re.Match[str]) -> str:
        return replacements[match.group(0)]

    return re.sub(r"(?<![A-Za-z0-9_.-])/(?:app|work|data)(?=/|[\s'\"`)\]}]|$)", replace_root, source)


def logical_path_to_host(path: str, ctx: RunContext) -> Path:
    rewritten = rewrite_logical_paths(path, ctx)
    resolved = Path(rewritten).resolve()
    allowed = (ctx.app_dir.resolve(), ctx.work_dir.resolve(), (ctx.app_dir / "data").resolve())
    if not any(resolved == root or root in resolved.parents for root in allowed):
        raise ValueError(f"cwd must stay under /app or /work: {path}")
    return resolved


def classify_stone_error(response: dict[str, Any] | None) -> str | None:
    if not isinstance(response, dict) or response.get("ok") is not False:
        return None
    error = response.get("error")
    if not isinstance(error, dict):
        return "runtime"
    code = error.get("code")
    detail = error.get("detail")
    if code == "stone_parse_error":
        return "parse"
    if code == "stone_script_unsupported":
        return "unsupported_feature"
    if isinstance(detail, str) and "unsupported statement" in detail:
        return "unsupported_feature"
    return "runtime"


def verify_context(ctx: RunContext) -> dict[str, Any]:
    if ctx.verify_mode == "shape":
        return verify_task_shape(ctx.task_id, ctx.app_dir, ctx.work_dir)
    return verify_task(ctx.task_id, ctx.app_dir, ctx.work_dir)


def verify_task(task_id: str, app_dir: Path, work_dir: Path) -> dict[str, Any]:
    if task_id == "hello-world":
        return verify_text_file(work_dir / "hello.txt", "Hello, world!", app_dir / "hello.txt")
    if task_id == "bank-trans-filter":
        return verify_json_file(work_dir / "data" / "output.json", EXPECTED_BANK_TRANSACTIONS, app_dir / "data" / "output.json")
    if task_id == "jsonl-aggregator":
        return verify_json_file(work_dir / "aggregates.json", EXPECTED_JSONL_AGGREGATES, app_dir / "aggregates.json")
    if task_id == "log-summary":
        return verify_log_summary_csv(work_dir / "summary.csv", app_dir / "summary.csv")
    if task_id == "analyze-access-logs":
        return verify_access_log_report(work_dir / "report.txt", app_dir / "report.txt")
    if task_id == "heterogeneous-dates":
        return verify_float_file(work_dir / "avg_temp.txt", 11.428571428571429, app_dir / "avg_temp.txt")
    if task_id == "recover-accuracy-log":
        return verify_recover_accuracy_log(
            work_dir / "recovered_logs",
            app_dir / "recovered_logs",
            ROOT.parent / "terminal-bench" / "original-tasks" / "recover-accuracy-log" / "tests" / "test_data",
        )
    if task_id == "recover-obfuscated-files":
        return verify_recovered_files(work_dir / "recovered", app_dir / "recovered")
    if task_id == "schedule-vacation":
        return verify_schedule_vacation(work_dir, app_dir)
    if task_id == "grid-pattern-transform":
        return verify_grid_transform(work_dir / "grid_transform.py", app_dir / "grid_transform.py")
    if task_id == "mahjong-winninghand":
        return verify_mahjong_result(work_dir / "result.txt", app_dir / "result.txt")
    if task_id == "ilp-solver":
        return verify_contains_lines(
            work_dir / "ilp_output.txt",
            app_dir / "ilp_output.txt",
            ["Invest A: y", "Invest B: n", "Defer B: n", "Invest C: y", "Maximized NPV: 299.00"],
        )
    if task_id == "sha-puzzle":
        return verify_sha_puzzle(work_dir / "solution.txt", app_dir / "solution.txt")
    if task_id == "count-call-stack":
        expected = ROOT.parent / "terminal-bench" / "original-tasks" / "count-call-stack" / "tests" / "expected_output.txt"
        return verify_text_file(work_dir / "output.txt", expected.read_text().strip(), app_dir / "output.txt")
    if task_id == "flood-monitoring-basic":
        return verify_flood_monitoring(app_dir / "data", work_dir / "flood_summary.json", app_dir / "flood_summary.json")
    if task_id == "log-summary-date-ranges":
        return verify_csv_exact(work_dir / "summary.csv", app_dir / "summary.csv", ["period", "severity", "count"], EXPECTED_LOG_DATE_ROWS)
    if task_id == "pandas-etl":
        return verify_pandas_etl(work_dir / "result.csv", app_dir / "result.csv")
    if task_id == "organization-json-generator":
        return verify_organization_json(work_dir / "organization.json", app_dir / "organization.json")
    if task_id == "regex-log":
        return verify_regex_log(work_dir / "regex.txt", app_dir / "regex.txt")
    if task_id == "constraints-scheduling":
        return verify_constraints_scheduling(work_dir / "meeting_scheduled.ics", app_dir / "meeting_scheduled.ics", app_dir)
    return {"ok": False, "failure_class": "harness_error", "failures": [f"no verifier for {task_id}"]}


def verify_task_shape(task_id: str, app_dir: Path, work_dir: Path) -> dict[str, Any]:
    if task_id == "hello-world":
        return verify_text_shape(work_dir / "hello.txt", app_dir / "hello.txt")
    if task_id == "bank-trans-filter":
        return verify_json_shape(work_dir / "data" / "output.json", app_dir / "data" / "output.json", list, required_item_keys=[
            "Company Name",
            "Account Number",
            "Date",
            "Amount",
            "Transaction Type",
            "Description",
        ])
    if task_id == "jsonl-aggregator":
        return verify_json_shape(work_dir / "aggregates.json", app_dir / "aggregates.json", dict, required_keys=[
            "top_5_users_by_amount",
            "top_5_tags_by_count",
        ])
    if task_id == "log-summary":
        return verify_csv_shape(work_dir / "summary.csv", app_dir / "summary.csv", ["severity", "count"], min_rows=2)
    if task_id == "analyze-access-logs":
        return verify_text_shape(work_dir / "report.txt", app_dir / "report.txt", required=["Total", "404"])
    if task_id == "heterogeneous-dates":
        return verify_float_shape(work_dir / "avg_temp.txt", app_dir / "avg_temp.txt")
    if task_id == "recover-accuracy-log":
        return verify_directory_shape(
            work_dir / "recovered_logs",
            app_dir / "recovered_logs",
            ["results.json", "run_1_generator.jsonl", "run_1_judge.jsonl"],
        )
    if task_id == "recover-obfuscated-files":
        return verify_directory_shape(work_dir / "recovered", app_dir / "recovered", min_files=1)
    if task_id == "schedule-vacation":
        failures = []
        for path in (
            work_dir / "vacation_dates.txt",
            work_dir / "all_availability.json",
            work_dir / "overlap_results.json",
        ):
            if not path.exists():
                failures.append(f"missing expected file {path}")
        for path in (work_dir / "all_availability.json", work_dir / "overlap_results.json"):
            if path.exists():
                failures.extend(json_parse_failures(path))
        return shape_result(failures, app_dir / "vacation_dates.txt")
    if task_id == "grid-pattern-transform":
        return verify_text_shape(work_dir / "grid_transform.py", app_dir / "grid_transform.py", required=["def solve"])
    if task_id == "mahjong-winninghand":
        return verify_text_shape(work_dir / "result.txt", app_dir / "result.txt")
    if task_id == "ilp-solver":
        return verify_text_shape(work_dir / "ilp_output.txt", app_dir / "ilp_output.txt")
    if task_id == "sha-puzzle":
        return verify_text_shape(work_dir / "solution.txt", app_dir / "solution.txt")
    if task_id == "count-call-stack":
        return verify_text_shape(work_dir / "output.txt", app_dir / "output.txt", required=["stack"])
    if task_id == "flood-monitoring-basic":
        return verify_json_shape(work_dir / "flood_summary.json", app_dir / "flood_summary.json", dict)
    if task_id == "log-summary-date-ranges":
        return verify_csv_shape(work_dir / "summary.csv", app_dir / "summary.csv", ["period", "severity", "count"], min_rows=2)
    if task_id == "pandas-etl":
        return verify_csv_shape(work_dir / "result.csv", app_dir / "result.csv", [
            "participant_id",
            "first_name",
            "last_name",
            "birth_date",
            "address",
            "phone_number",
            "country",
            "institute",
            "occupation",
            "register_time",
            "postal_code",
            "city",
            "team_name",
        ], min_rows=1)
    if task_id == "organization-json-generator":
        return verify_json_shape(work_dir / "organization.json", app_dir / "organization.json", dict, required_keys=[
            "organization",
            "statistics",
        ])
    if task_id == "regex-log":
        path = work_dir / "regex.txt"
        if not path.exists():
            return missing_result(path, app_dir / "regex.txt")
        try:
            re.compile(path.read_text().strip())
        except re.error as err:
            return {"ok": False, "failure_class": "bad_result", "failures": [f"invalid regex: {err}"]}
        return {"ok": True, "failure_class": None, "failures": []}
    if task_id == "constraints-scheduling":
        return verify_text_shape(
            work_dir / "meeting_scheduled.ics",
            app_dir / "meeting_scheduled.ics",
            required=["BEGIN:VCALENDAR", "BEGIN:VEVENT", "DTSTART", "DTEND", "END:VCALENDAR"],
        )
    return {"ok": False, "failure_class": "harness_error", "failures": [f"no shape verifier for {task_id}"]}


def verify_text_shape(path: Path, bad_path_probe: Path, required: list[str] | None = None) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    text = path.read_text()
    failures = []
    if not text.strip():
        failures.append(f"{path.name} is empty")
    for needle in required or []:
        if needle not in text:
            failures.append(f"missing {needle!r}")
    return shape_result(failures, bad_path_probe)


def verify_float_shape(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    raw = path.read_text().strip()
    try:
        float(raw)
    except ValueError:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"{path.name} did not contain a float: {raw!r}"]}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_json_shape(
    path: Path,
    bad_path_probe: Path,
    expected_type: type,
    required_keys: list[str] | None = None,
    required_item_keys: list[str] | None = None,
) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    try:
        data = json.loads(path.read_text())
    except json.JSONDecodeError as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"invalid JSON: {err}"]}
    failures = []
    if not isinstance(data, expected_type):
        failures.append(f"expected {expected_type.__name__}, got {type(data).__name__}")
    if required_keys and isinstance(data, dict):
        for key in required_keys:
            if key not in data:
                failures.append(f"missing key {key!r}")
    if required_item_keys and isinstance(data, list):
        if not data:
            failures.append("JSON list is empty")
        for index, item in enumerate(data[:3]):
            if not isinstance(item, dict):
                failures.append(f"row {index} is not an object")
                continue
            missing = [key for key in required_item_keys if key not in item]
            if missing:
                failures.append(f"row {index} missing keys {missing!r}")
    return shape_result(failures, bad_path_probe)


def verify_csv_shape(path: Path, bad_path_probe: Path, header: list[str], min_rows: int = 0) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    try:
        with path.open(newline="") as file:
            rows = list(csv.reader(file))
    except csv.Error as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"invalid CSV: {err}"]}
    failures = []
    if not rows:
        failures.append("CSV is empty")
    elif rows[0] != header:
        failures.append(f"header mismatch: expected {header!r}, got {rows[0]!r}")
    if len(rows) - 1 < min_rows:
        failures.append(f"expected at least {min_rows} data row(s), got {max(0, len(rows) - 1)}")
    return shape_result(failures, bad_path_probe)


def verify_directory_shape(
    path: Path,
    bad_path_probe: Path,
    required_files: list[str] | None = None,
    min_files: int = 0,
) -> dict[str, Any]:
    if not path.exists():
        failure_class = "bad_path" if bad_path_probe.exists() else "bad_result"
        return {"ok": False, "failure_class": failure_class, "failures": [f"missing expected directory {path}"]}
    failures = []
    files = [item for item in path.iterdir() if item.is_file()]
    if len(files) < min_files:
        failures.append(f"expected at least {min_files} file(s), got {len(files)}")
    for name in required_files or []:
        if not (path / name).exists():
            failures.append(f"missing expected file {name}")
    return shape_result(failures, bad_path_probe)


def json_parse_failures(path: Path) -> list[str]:
    try:
        json.loads(path.read_text())
    except json.JSONDecodeError as err:
        return [f"invalid JSON in {path.name}: {err}"]
    return []


def shape_result(failures: list[str], bad_path_probe: Path) -> dict[str, Any]:
    if failures:
        failure_class = "bad_path" if bad_path_probe.exists() else "bad_result"
        return {"ok": False, "failure_class": failure_class, "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_text_file(path: Path, expected: str, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    actual = path.read_text().strip()
    if actual == expected:
        return {"ok": True, "failure_class": None, "failures": []}
    return {"ok": False, "failure_class": "bad_result", "failures": [f"expected {expected!r}, got {actual!r}"]}


def verify_access_log_report(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    lines = path.read_text().strip().splitlines()
    failures = []
    expected_lines = {
        "Total requests: 2000",
        "Unique IP addresses: 273",
        "Top 3 URLs:",
        "404 errors: 83",
    }
    actual_set = set(lines)
    for line in sorted(expected_lines):
        if line not in actual_set:
            failures.append(f"missing report line {line!r}")
    url_lines = {line.strip() for line in lines if line.startswith("  /")}
    expected_urls = {
        "/order-confirmation: 54",
        "/product/456: 53",
        "/about.html: 52",
    }
    if url_lines != expected_urls:
        failures.append(f"top URL lines mismatch: expected {expected_urls!r}, got {url_lines!r}")
    if len(url_lines) != 3:
        failures.append(f"expected exactly 3 top URL lines, got {len(url_lines)}")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_recovered_files(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    failures = []
    actual_files = {item.name for item in path.iterdir() if item.is_file()}
    if actual_files != set(EXPECTED_RECOVERED_FILES):
        failures.append(f"expected files {sorted(EXPECTED_RECOVERED_FILES)}, got {sorted(actual_files)}")
    for name, expected in EXPECTED_RECOVERED_FILES.items():
        file = path / name
        if not file.exists():
            continue
        if file.read_text().replace("\r\n", "\n") != expected.replace("\r\n", "\n"):
            failures.append(f"{name} content mismatch")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_schedule_vacation(work_dir: Path, app_dir: Path) -> dict[str, Any]:
    failures = []
    vacation = work_dir / "vacation_dates.txt"
    if not vacation.exists():
        return missing_result(vacation, app_dir / "vacation_dates.txt")
    if vacation.read_text().strip() != "2025-07-28 to 2025-08-03":
        failures.append(f"wrong vacation window: {vacation.read_text().strip()!r}")
    for friend in ("alice", "bob", "charlie", "dana"):
        path = work_dir / f"{friend}_schedule.txt"
        if not path.exists():
            failures.append(f"missing {path.name}")
            continue
        result = run_checked_result([sys.executable, str(app_dir / "get_schedule.py"), friend], cwd=app_dir, timeout_sec=10)
        if path.read_text().strip() != result.stdout.strip():
            failures.append(f"{path.name} does not match get_schedule.py output")
    availability = work_dir / "all_availability.json"
    overlap = work_dir / "overlap_results.json"
    for path in (availability, overlap):
        if not path.exists():
            failures.append(f"missing {path.name}")
    if availability.exists():
        try:
            data = json.loads(availability.read_text())
            if set(data) != {"alice", "bob", "charlie", "dana"}:
                failures.append("all_availability.json missing friends")
        except json.JSONDecodeError as err:
            failures.append(f"bad all_availability.json: {err}")
    if overlap.exists():
        try:
            data = json.loads(overlap.read_text())
            if data.get("vacation_duration") != 7 or data.get("total_windows_found", 0) <= 0:
                failures.append("overlap_results.json has wrong shape/count")
        except json.JSONDecodeError as err:
            failures.append(f"bad overlap_results.json: {err}")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_grid_transform(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    namespace: dict[str, Any] = {}
    try:
        exec(path.read_text(), namespace)
        solve = namespace["solve"]
        cases = [
            ([[8, 6], [6, 4]], [[8, 6, 8, 6, 8, 6], [6, 4, 6, 4, 6, 4], [6, 8, 6, 8, 6, 8], [4, 6, 4, 6, 4, 6], [8, 6, 8, 6, 8, 6], [6, 4, 6, 4, 6, 4]]),
            ([[7, 9], [4, 3]], [[7, 9, 7, 9, 7, 9], [4, 3, 4, 3, 4, 3], [9, 7, 9, 7, 9, 7], [3, 4, 3, 4, 3, 4], [7, 9, 7, 9, 7, 9], [4, 3, 4, 3, 4, 3]]),
            ([[3, 2], [7, 8]], [[3, 2, 3, 2, 3, 2], [7, 8, 7, 8, 7, 8], [2, 3, 2, 3, 2, 3], [8, 7, 8, 7, 8, 7], [3, 2, 3, 2, 3, 2], [7, 8, 7, 8, 7, 8]]),
        ]
        for input_grid, expected in cases:
            if solve(input_grid) != expected:
                return {"ok": False, "failure_class": "bad_result", "failures": [f"wrong output for {input_grid}"]}
    except Exception as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"grid_transform.py failed: {err}"]}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_mahjong_result(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    results: dict[str, set[str]] = {}
    for line in path.read_text().strip().splitlines():
        if ":" not in line:
            continue
        filename, right = line.split(":", 1)
        matches = re.findall(r'"([^"]+)"', right)
        results[filename.strip()] = set(matches) if matches else {right.strip()}
    failures = []
    if set(results) != set(EXPECTED_MAHJONG):
        failures.append(f"expected result files {sorted(EXPECTED_MAHJONG)}, got {sorted(results)}")
    for name, expected in EXPECTED_MAHJONG.items():
        if results.get(name) != expected:
            failures.append(f"{name}: expected {expected}, got {results.get(name)}")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures[:8]}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_contains_lines(path: Path, bad_path_probe: Path, expected_lines: list[str]) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    text = path.read_text()
    failures = [f"missing {line!r}" for line in expected_lines if line not in text]
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_sha_puzzle(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    content = path.read_text().strip()
    first_letters = "".join(word[0] for word in content.split() if word)
    count = sum(1 for char in hashlib.sha1(content.encode()).hexdigest() if char.isalpha())
    try:
        predicted = number_words_to_int(first_letters)
    except ValueError as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [str(err)]}
    if count != predicted:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"sha letter count {count}, first letters mean {predicted}"]}
    return {"ok": True, "failure_class": None, "failures": []}


def number_words_to_int(text: str) -> int:
    words = {
        "zero": 0,
        "one": 1,
        "two": 2,
        "three": 3,
        "four": 4,
        "five": 5,
        "six": 6,
        "seven": 7,
        "eight": 8,
        "nine": 9,
        "ten": 10,
        "eleven": 11,
        "twelve": 12,
        "thirteen": 13,
        "fourteen": 14,
        "fifteen": 15,
        "sixteen": 16,
        "seventeen": 17,
        "eighteen": 18,
        "nineteen": 19,
        "twenty": 20,
        "thirty": 30,
        "forty": 40,
    }
    if text in words:
        return words[text]
    for prefix, value in (("twenty", 20), ("thirty", 30), ("forty", 40)):
        if text.startswith(prefix) and text[len(prefix) :] in words:
            return value + words[text[len(prefix) :]]
    raise ValueError(f"could not parse number words from first letters {text!r}")


def verify_float_file(path: Path, expected: float, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    raw = path.read_text().strip()
    try:
        actual = float(raw)
    except ValueError:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"{path.name} did not contain a float: {raw!r}"]}
    if round(actual, 3) == round(expected, 3):
        return {"ok": True, "failure_class": None, "failures": []}
    return {"ok": False, "failure_class": "bad_result", "failures": [f"{path.name}: expected {expected}, got {actual}"]}


def verify_json_file(path: Path, expected: Any, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    try:
        actual = json.loads(path.read_text())
    except json.JSONDecodeError as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"invalid JSON: {err}"]}
    if actual == expected:
        return {"ok": True, "failure_class": None, "failures": []}
    failures = ["JSON output mismatch"]
    if isinstance(actual, list) and isinstance(expected, list):
        failures.append(f"expected {len(expected)} rows, got {len(actual)}")
        actual_keys = {json.dumps(row, sort_keys=True) for row in actual}
        expected_keys = {json.dumps(row, sort_keys=True) for row in expected}
        missing = [json.loads(row) for row in sorted(expected_keys - actual_keys)]
        extra = [json.loads(row) for row in sorted(actual_keys - expected_keys)]
        if missing:
            failures.append("missing rows: " + json.dumps(missing[:3], sort_keys=True))
        if extra:
            failures.append("extra rows: " + json.dumps(extra[:3], sort_keys=True))
    elif isinstance(actual, dict) and isinstance(expected, dict):
        failures.append(f"expected keys {sorted(expected)}, got {sorted(actual)}")
    return {"ok": False, "failure_class": "bad_result", "failures": failures}


def verify_recover_accuracy_log(path: Path, bad_path_probe: Path, expected_dir: Path) -> dict[str, Any]:
    expected_files = [
        "results.json",
        "run_1_generator.jsonl",
        "run_1_judge.jsonl",
        "run_2_generator.jsonl",
        "run_2_judge.jsonl",
        "run_3_generator.jsonl",
        "run_3_judge.jsonl",
    ]
    if not path.exists():
        failure_class = "bad_path" if bad_path_probe.exists() else "bad_result"
        return {"ok": False, "failure_class": failure_class, "failures": [f"missing expected directory {path}"]}
    failures = []
    for name in expected_files:
        actual = path / name
        expected = expected_dir / name
        if not actual.exists():
            failures.append(f"missing expected file {name}")
            continue
        if name.endswith(".json"):
            try:
                actual_json = json.loads(actual.read_text())
                expected_json = json.loads(expected.read_text())
            except json.JSONDecodeError as err:
                failures.append(f"{name} is not valid JSON: {err}")
                continue
            if actual_json != expected_json:
                failures.append(f"{name} JSON mismatch")
        else:
            actual_lines = actual.read_text().splitlines()
            expected_lines = expected.read_text().splitlines()
            if actual_lines != expected_lines:
                failures.append(f"{name} JSONL lines mismatch")
                failures.append(f"expected {len(expected_lines)} line(s), got {len(actual_lines)}")
                for index, (expected_line, actual_line) in enumerate(zip(expected_lines, actual_lines)):
                    if expected_line != actual_line:
                        failures.append(
                            f"{name} first mismatch at line {index + 1}: expected {expected_line!r}, got {actual_line!r}"
                        )
                        break
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_regex_log(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    pattern = path.read_text().strip()
    try:
        re.compile(pattern)
    except re.error as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"invalid regex: {err}"]}
    sample_logs = [
        "2025-01-09 User login from 192.168.0.1",
        "192.168.1.100 accessed on 2023-12-31 and 2024-11-01",
        "No IP here but 2022-05-05 appears",
        "Multiple IPs 10.0.0.1 10.0.0.2 with 2020-01-01",
        "Date before IP 2019-06-15 from 172.16.0.5",
        "IP with date before and after 2018-05-05 8.8.8.8 info 2018-06-06",
        "Invalid IP 256.100.50.25 with date 2021-01-01",
        "Another invalid 123.456.78.90 2020-12-12",
        "Edge case 0.0.0.0 and date 2017-03-03",
        "Broadcast 255.255.255.255 on 2016-12-31",
        "Broadcast 255.255.255.255 for user 1234-56-7890",
        "Error for user with code abc192.168.1.1 on 2023-01-01",
        "Connection from 192.168.0.10 on 2023-13-05",
        "Access from 10.0.0.5 at 2022-00-15",
        "Server 172.16.5.4 logged event on 1995-06-32",
        "Client 8.8.8.8 reported on 2001-09-00",
        "User 192.0.2.1 performed action 2020-07-15abc",
        "Backup abc2021-08-20 from 203.0.113.5 completed",
        "2024-11-11 login from 198.51.100.23 at start",
        "Routine check by 203.0.113.10 ended on 1999-03-05",
        "User login 192.168.1.1abc on 2021-12-01",
        "abc10.0.0.5 accessed system on 1999-07-20",
        "Login attempt 172.16.0.256 on 2000-05-10",
        "Login attempt 172.16.0.256 for user 2000-02-30",
        "Login attempt 172.16.0.256 for user 2000-04-31",
    ]
    expected = [
        "2025-01-09",
        "2024-11-01",
        "2020-01-01",
        "2019-06-15",
        "2018-06-06",
        "2017-03-03",
        "2016-12-31",
        "2024-11-11",
        "1999-03-05",
    ]
    matches = re.findall(pattern, "\n".join(sample_logs), re.MULTILINE)
    if matches != expected:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"expected {expected}, got {matches}"]}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_constraints_scheduling(path: Path, bad_path_probe: Path, app_dir: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    text = path.read_text()
    failures = []
    if not text.strip().startswith("BEGIN:VCALENDAR") or not text.strip().endswith("END:VCALENDAR"):
        failures.append("bad VCALENDAR wrapper")
    for needle in ("VERSION:2.0", "PRODID:", "BEGIN:VEVENT", "END:VEVENT", "SUMMARY:Team Planning Meeting"):
        if needle not in text:
            failures.append(f"missing {needle}")
    for email in ("alice@example.com", "bob@example.com", "carol@example.com"):
        if email not in text:
            failures.append(f"missing attendee {email}")
    try:
        start = parse_ics_dt(r"DTSTART:(\d{8}T\d{6}Z)", text)
        end = parse_ics_dt(r"DTEND:(\d{8}T\d{6}Z)", text)
    except AssertionError as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [str(err)]}
    if end - start != timedelta(hours=1):
        failures.append("meeting must last 1 hour")
    if not (start.year == 2024 and start.month == 1 and 15 <= start.day <= 19 and start.weekday() < 5):
        failures.append(f"meeting outside Jan 15-19 weekday window: {start}")
    if not constraints_hard_ok(start, end):
        failures.append(f"meeting violates hard constraints: {start} to {end}")
    calendars = [(app_dir / name).read_text() for name in ("alice_calendar.ics", "bob_calendar.ics", "carol_calendar.ics")]
    if any(ics_overlap(start, end, cal) for cal in calendars):
        failures.append("meeting conflicts with input calendar")
    if not (9 <= start.hour < 12):
        failures.append("should prefer Alice morning slot")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def parse_ics_dt(pattern: str, text: str) -> datetime:
    match = re.search(pattern, text)
    assert match, f"missing timestamp {pattern}"
    return datetime.strptime(match.group(1), "%Y%m%dT%H%M%SZ")


def ics_overlap(a0: datetime, a1: datetime, cal: str) -> bool:
    for start, end in zip(re.findall(r"DTSTART:(\d{8}T\d{6}Z)", cal), re.findall(r"DTEND:(\d{8}T\d{6}Z)", cal)):
        b0 = datetime.strptime(start, "%Y%m%dT%H%M%SZ")
        b1 = datetime.strptime(end, "%Y%m%dT%H%M%SZ")
        if a0 < b1 and a1 > b0:
            return True
    return False


def constraints_hard_ok(t0: datetime, t1: datetime) -> bool:
    if t0.hour < 9:
        return False
    if not (t1.hour < 14 or (t1.hour == 14 and t1.minute <= 0)):
        return False
    if t0.hour < 10:
        return False
    if t0.weekday() in (1, 3) and not (t1.hour < 16 or (t1.hour == 16 and t1.minute <= 30)):
        return False
    if not (t1.hour < 17 or (t1.hour == 17 and t1.minute <= 0)):
        return False
    lunch0 = t0.replace(hour=12, minute=0)
    lunch1 = t0.replace(hour=12, minute=30)
    if t0 < lunch1 and t1 > lunch0:
        return False
    return t1.date() == t0.date() and t0.weekday() < 5


def verify_log_summary_csv(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    with path.open(newline="") as file:
        rows = list(csv.reader(file))
    failures = []
    if len(rows) != 4:
        failures.append(f"summary.csv must have 4 rows, got {len(rows)}")
    if not rows or rows[0] != ["severity", "count"]:
        failures.append("summary.csv header must be severity,count")
    actual = {}
    for row in rows[1:]:
        if len(row) != 2:
            failures.append(f"bad row: {row!r}")
            continue
        try:
            actual[row[0]] = int(row[1])
        except ValueError:
            failures.append(f"bad count: {row!r}")
    for key, value in EXPECTED_LOG_SUMMARY.items():
        if actual.get(key) != value:
            failures.append(f"{key} expected {value}, got {actual.get(key)}")
    if set(actual) != set(EXPECTED_LOG_SUMMARY):
        failures.append(f"severities mismatch: {sorted(actual)}")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_csv_exact(path: Path, bad_path_probe: Path, header: list[str], rows: list[list[str]]) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    with path.open(newline="") as file:
        actual = list(csv.reader(file))
    expected = [header, *rows]
    if actual != expected:
        failures = [f"CSV mismatch: expected {len(expected)} rows, got {len(actual)}"]
        for index, (expected_row, actual_row) in enumerate(zip(expected, actual)):
            if expected_row != actual_row:
                failures.append(f"first mismatch at row {index}: expected {expected_row!r}, got {actual_row!r}")
                break
        if len(actual) > len(expected):
            failures.append(f"first extra row: {actual[len(expected)]!r}")
        elif len(actual) < len(expected):
            failures.append(f"first missing row: {expected[len(actual)]!r}")
        return {"ok": False, "failure_class": "bad_result", "failures": failures}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_flood_monitoring(data_dir: Path, summary_path: Path, bad_path_probe: Path) -> dict[str, Any]:
    failures = []
    for station in ("station_A", "station_B", "station_C"):
        path = data_dir / f"{station}.csv"
        if not path.exists():
            failures.append(f"missing {path}")
            continue
        with path.open(newline="") as file:
            rows = list(csv.DictReader(file))
        if len(rows) != 48:
            failures.append(f"{station}.csv should have 48 rows, got {len(rows)}")
        for index, row in enumerate(rows):
            if row.get("water_level_m") == "":
                failures.append(f"{station}.csv has missing value at row {index}")
                break
    checks = {
        ("station_A", 0): 2.3,
        ("station_A", 5): 3.45,
        ("station_A", 20): 3.15,
        ("station_A", 36): 2.05,
        ("station_A", 47): 2.1,
        ("station_B", 9): 3.6,
        ("station_B", 30): 2.6,
    }
    for (station, index), expected in checks.items():
        with (data_dir / f"{station}.csv").open(newline="") as file:
            rows = list(csv.DictReader(file))
        try:
            actual = float(rows[index]["water_level_m"])
        except Exception:
            failures.append(f"{station}.csv row {index} is not numeric")
            continue
        if abs(actual - expected) >= 0.01:
            failures.append(f"{station}.csv row {index}: expected {expected}, got {actual}")
    if not summary_path.exists():
        return missing_result(summary_path, bad_path_probe)
    try:
        summary = json.loads(summary_path.read_text())
    except json.JSONDecodeError as err:
        failures.append(f"bad flood_summary.json: {err}")
        summary = {}
    expected_summary = {"station_A": 2, "station_B": 1, "station_C": 0}
    if summary != expected_summary:
        failures.append(f"expected flood summary {expected_summary}, got {summary}")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures[:8]}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_pandas_etl(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    with path.open(newline="") as file:
        rows = list(csv.DictReader(file))
    required = {
        "participant_id",
        "first_name",
        "last_name",
        "birth_date",
        "address",
        "phone_number",
        "country",
        "institute",
        "occupation",
        "register_time",
        "postal_code",
        "city",
        "team_name",
    }
    if set(rows[0]) != required:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"columns mismatch: {sorted(rows[0])}"]}
    by_id = {row["participant_id"]: row for row in rows}
    expected = {
        "bd9b6f88-b84f-4c4d-90f8-b67fe2f1a29a": ("80734", "Medan", "1991-02-05", "CN-Georgia-UPM"),
        "7dfe3391-6f40-47b6-b4db-0c76ebaf5fc3": ("09434", "Prabumulih", "1993-01-11", "AS-Korea Utara-UD"),
        "19582d7f-b824-4fe5-a517-d5bf573fc768": ("57739", "Ambon", "1993-07-31", "CH-Komoro-UHP"),
        "aeb6d817-25f3-4867-8a74-8d92e0a0f633": ("76156", "Lubuklinggau", "1991-11-04", "SR-Eritrea-PTY"),
        "1fdabdd9-5444-4c97-87b2-fe8833ad0d27": ("70074", "Kediri", "2003-01-22", "VA-Aljazair-PHR"),
    }
    failures = []
    for key, values in expected.items():
        row = by_id.get(key)
        if row is None:
            failures.append(f"missing participant {key}")
            continue
        for field, expected_value in zip(("postal_code", "city", "birth_date", "team_name"), values):
            if row[field] != expected_value:
                failures.append(f"{key} {field}: expected {expected_value}, got {row[field]}")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures[:8]}
    return {"ok": True, "failure_class": None, "failures": []}


def verify_organization_json(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    if not path.exists():
        return missing_result(path, bad_path_probe)
    try:
        data = json.loads(path.read_text())
        departments = data["organization"]["departments"]
        stats = data["statistics"]
    except Exception as err:
        return {"ok": False, "failure_class": "bad_result", "failures": [f"bad organization.json: {err}"]}
    failures = []
    if len(departments) < 3:
        failures.append("Organization must have at least 3 departments")
    dept_ids = set()
    statuses = []
    for dept in departments:
        dept_ids.add(dept["id"])
        if len(dept["employees"]) < 2:
            failures.append(f"Department {dept['id']} needs 2 employees")
        emp_ids = {emp["id"] for emp in dept["employees"]}
        if len(emp_ids) != len(dept["employees"]):
            failures.append(f"duplicate employees in {dept['id']}")
        for project in dept["projects"]:
            statuses.append(project["status"])
            for member_id in project["members"]:
                if member_id not in emp_ids:
                    failures.append(f"{member_id} not in {dept['name']}")
    if len(dept_ids) != len(departments):
        failures.append("Department IDs must be unique")
    total_budget = sum(dept["budget"] for dept in departments)
    if abs(stats["averageDepartmentBudget"] - total_budget / len(departments)) >= 0.01:
        failures.append("averageDepartmentBudget mismatch")
    total_employees = sum(len(dept["employees"]) for dept in departments)
    if stats["totalEmployees"] != total_employees:
        failures.append("totalEmployees mismatch")
    for dept in departments:
        if stats["departmentSizes"][dept["name"]] != len(dept["employees"]):
            failures.append(f"department size mismatch for {dept['name']}")
    for status in set(statuses):
        if stats["projectStatusDistribution"][status] != statuses.count(status):
            failures.append(f"status count mismatch for {status}")
    if failures:
        return {"ok": False, "failure_class": "bad_result", "failures": failures[:8]}
    return {"ok": True, "failure_class": None, "failures": []}


def missing_result(path: Path, bad_path_probe: Path) -> dict[str, Any]:
    failure_class = "bad_path" if bad_path_probe.exists() else "bad_result"
    return {"ok": False, "failure_class": failure_class, "failures": [f"missing expected file {path}"]}


def record_result(
    ctx: RunContext,
    mode: str,
    started: float,
    result: dict[str, Any],
    verification: dict[str, Any],
) -> dict[str, Any]:
    metrics = ctx.metrics
    result_failures = result.get("failures") if isinstance(result.get("failures"), list) else []
    verification_failures = verification.get("failures", [])
    failure_class = verification.get("failure_class")
    if not verification.get("ok") and result.get("failure_class"):
        failure_class = result.get("failure_class")
    return {
        "task_id": ctx.task_id,
        "mode": mode,
        "verify_mode": ctx.verify_mode,
        "resolved": bool(verification["ok"]),
        "failure_class": failure_class,
        "failures": [*result_failures, *verification_failures],
        "elapsed_sec": round(time.monotonic() - started, 3),
        "result_ok": bool(result.get("ok")),
        "turns": metrics.turns,
        "tool_calls": metrics.tool_calls,
        "stone_eval_calls": metrics.stone_eval_calls,
        "stone_help_calls": metrics.stone_help_calls,
        "escape_linux_calls": metrics.escape_linux_calls,
        "syntax_errors": metrics.syntax_errors,
        "parse_errors": metrics.parse_errors,
        "unsupported_feature_errors": metrics.unsupported_feature_errors,
        "runtime_errors": metrics.runtime_errors,
        "input_tokens": metrics.input_tokens,
        "output_tokens": metrics.output_tokens,
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
    stone_eval_calls = sum(record["stone_eval_calls"] for record in records)
    syntax_errors = sum(record["syntax_errors"] for record in records)
    parse_errors = sum(record["parse_errors"] for record in records)
    unsupported_feature_errors = sum(record["unsupported_feature_errors"] for record in records)
    runtime_errors = sum(record["runtime_errors"] for record in records)
    return {
        "elapsed_sec": round(time.monotonic() - started, 3),
        "verify_mode": records[0].get("verify_mode") if records else None,
        "tasks": len(records),
        "resolved": sum(1 for record in records if record["resolved"]),
        "syntax_error_rate": syntax_errors / stone_eval_calls if stone_eval_calls else 0.0,
        "parse_error_rate": parse_errors / stone_eval_calls if stone_eval_calls else 0.0,
        "unsupported_feature_error_rate": unsupported_feature_errors / stone_eval_calls if stone_eval_calls else 0.0,
        "runtime_error_rate": runtime_errors / stone_eval_calls if stone_eval_calls else 0.0,
        "escape_rate": sum(record["escape_linux_calls"] for record in records) / tool_calls if tool_calls else 0.0,
        "discovery_overhead": sum(record["stone_help_calls"] for record in records) / len(records) if records else 0.0,
        "records": records,
    }


def write_json_file(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def write_summary_csv(path: Path, records: list[dict[str, Any]]) -> None:
    fields = [
        "task_id",
        "mode",
        "resolved",
        "failure_class",
        "elapsed_sec",
        "turns",
        "tool_calls",
        "stone_eval_calls",
        "stone_help_calls",
        "escape_linux_calls",
        "syntax_errors",
        "parse_errors",
        "unsupported_feature_errors",
        "runtime_errors",
        "verify_mode",
        "input_tokens",
        "output_tokens",
    ]
    with path.open("w", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=fields)
        writer.writeheader()
        for record in records:
            writer.writerow({field: record.get(field) for field in fields})


def truncate(text: str, max_chars: int) -> str:
    if len(text) <= max_chars:
        return text
    return text[:max_chars] + "\n...[truncated]..."


def tail(text: str, max_chars: int = 4000) -> str:
    return text[-max_chars:] if len(text) > max_chars else text


if __name__ == "__main__":
    raise SystemExit(main())
