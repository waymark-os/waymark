#!/usr/bin/env python3
"""Run randomized repeated Stone prompt-arm experiments."""

from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ARMS = ("full", "no-negatives", "no-examples", "minimal")


def main() -> int:
    args = parse_args()
    out_root = args.out_root or default_out_root()
    out_root = out_root.resolve()
    out_root.mkdir(parents=True, exist_ok=True)

    jobs = [
        {"repeat": repeat, "arm": arm}
        for repeat in range(1, args.repeats + 1)
        for arm in args.arms
    ]
    rng = random.Random(args.seed)
    rng.shuffle(jobs)
    for index, job in enumerate(jobs, start=1):
        job["index"] = index
        job["out_dir"] = str(out_root / f"{index:02d}-r{job['repeat']:02d}-{job['arm']}")
    write_json(out_root / "manifest.json", {"seed": args.seed, "jobs": jobs})

    records = []
    started = time.monotonic()
    for job in jobs:
        out_dir = Path(job["out_dir"])
        summary = read_summary(out_dir / "summary.json")
        complete = summary.get("tasks") == len(args.tasks)
        rerun_failure_class = summary_has_failure_class(summary, set(args.rerun_failure_class))
        if (out_dir / "summary.json").exists() and args.resume and not args.resume_incomplete:
            result = {"returncode": 0, "skipped": True}
        elif complete and args.resume_incomplete and not rerun_failure_class:
            result = {"returncode": 0, "skipped": True}
        else:
            cmd = [
                sys.executable,
                str(ROOT / "host" / "bench" / "run_stone_tbench4.py"),
                "--mode",
                "agent",
                "--prompt-style",
                job["arm"],
                "--server",
                args.server,
                "--model",
                args.model,
                "--provider",
                args.provider,
                "--api-key-env",
                args.api_key_env,
                "--timeout",
                str(args.timeout),
                "--verify-mode",
                args.verify_mode,
                "--max-turns",
                str(args.max_turns),
                "--out-dir",
                str(out_dir),
                "--tasks",
                *args.tasks,
            ]
            if args.omit_sampling:
                cmd.insert(cmd.index("--out-dir"), "--omit-sampling")
            print(f"running {job['index']:02d}/{len(jobs)} repeat={job['repeat']} arm={job['arm']}", flush=True)
            completed = subprocess.run(cmd, cwd=ROOT, text=True, check=False)
            result = {"returncode": completed.returncode, "skipped": False}
            summary = read_summary(out_dir / "summary.json")
        record = {
            **job,
            **result,
            "resolved": summary.get("resolved"),
            "tasks": summary.get("tasks"),
            "syntax_error_rate": summary.get("syntax_error_rate"),
            "parse_error_rate": summary.get("parse_error_rate"),
            "unsupported_feature_error_rate": summary.get("unsupported_feature_error_rate"),
            "runtime_error_rate": summary.get("runtime_error_rate"),
            "escape_rate": summary.get("escape_rate"),
            "discovery_overhead": summary.get("discovery_overhead"),
            "elapsed_sec": summary.get("elapsed_sec"),
        }
        records.append(record)
        write_json(out_root / "repeat-summary.json", {"elapsed_sec": round(time.monotonic() - started, 3), "records": records})

    return 0 if all((record.get("resolved") == record.get("tasks")) for record in records) else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--arms", nargs="+", default=list(DEFAULT_ARMS))
    parser.add_argument(
        "--tasks",
        nargs="+",
        default=["hello-world", "bank-trans-filter", "jsonl-aggregator", "log-summary"],
    )
    parser.add_argument("--seed", type=int, default=20260515)
    parser.add_argument("--out-root", type=Path)
    parser.add_argument("--server", default=None)
    parser.add_argument("--model", default="Qwen/Qwen3.6-27B-FP8")
    parser.add_argument("--provider", choices=("openai-compatible", "openai"), default="openai-compatible")
    parser.add_argument("--api-key-env", default="OPENAI_API_KEY")
    parser.add_argument("--omit-sampling", action="store_true")
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--verify-mode", choices=("exact", "shape"), default="exact")
    parser.add_argument("--max-turns", type=int, default=10)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument(
        "--resume-incomplete",
        action="store_true",
        help="With --resume, rerun arms whose summary has fewer tasks than the requested task list.",
    )
    parser.add_argument(
        "--rerun-failure-class",
        nargs="*",
        default=[],
        help="With --resume-incomplete, rerun complete summaries containing any listed task failure_class.",
    )
    args = parser.parse_args()
    if args.server is None:
        if args.provider == "openai":
            args.server = "https://api.openai.com"
        else:
            args.server = "http://192.168.1.11:8000"
    return args


def default_out_root() -> Path:
    stamp = time.strftime("%Y%m%dT%H%M%S")
    return ROOT / "target" / "runs" / f"stone-tbench4-vllm-randomized-{stamp}"


def read_summary(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    return json.loads(path.read_text())


def summary_has_failure_class(summary: dict[str, Any], failure_classes: set[str]) -> bool:
    if not failure_classes:
        return False
    records = summary.get("records")
    if not isinstance(records, list):
        return False
    return any(isinstance(record, dict) and record.get("failure_class") in failure_classes for record in records)


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    raise SystemExit(main())
