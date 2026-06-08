#!/usr/bin/env python3
"""Run randomized repeated Nushell prompt experiments."""

from __future__ import annotations

import argparse
import json
import random
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import run_nu_tbench20 as nu


ROOT = Path(__file__).resolve().parents[2]


def main() -> int:
    args = parse_args()
    out_root = args.out_root or default_out_root()
    out_root = out_root.resolve()
    out_root.mkdir(parents=True, exist_ok=True)

    jobs = [{"repeat": repeat} for repeat in range(1, args.repeats + 1)]
    rng = random.Random(args.seed)
    rng.shuffle(jobs)
    for index, job in enumerate(jobs, start=1):
        job["index"] = index
        job["out_dir"] = str(out_root / f"{index:02d}-r{job['repeat']:02d}-nu")
    write_json(out_root / "manifest.json", {"seed": args.seed, "jobs": jobs})

    records = []
    started = time.monotonic()
    for job in jobs:
        out_dir = Path(job["out_dir"])
        summary = read_summary(out_dir / "summary.json")
        complete = summary.get("tasks") == len(args.tasks)
        if complete and args.resume:
            result = {"returncode": 0, "skipped": True}
        else:
            cmd = [
                sys.executable,
                str(ROOT / "host" / "bench" / "run_nu_tbench20.py"),
                "--provider",
                args.provider,
                "--model",
                args.model,
                "--timeout",
                str(args.timeout),
                "--verify-mode",
                args.verify_mode,
                "--max-turns",
                str(args.max_turns),
                "--max-tokens",
                str(args.max_tokens),
                "--nu-bin",
                str(args.nu_bin),
                "--out-dir",
                str(out_dir),
                "--tasks",
                *args.tasks,
            ]
            if args.server:
                cmd.extend(["--server", args.server])
            if args.api_key_env:
                cmd.extend(["--api-key-env", args.api_key_env])
            if args.omit_sampling:
                cmd.append("--omit-sampling")
            print(f"running {job['index']:02d}/{len(jobs)} repeat={job['repeat']}", flush=True)
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
            "discovery_overhead": summary.get("discovery_overhead"),
            "elapsed_sec": summary.get("elapsed_sec"),
        }
        records.append(record)
        write_json(
            out_root / "repeat-summary.json",
            {"elapsed_sec": round(time.monotonic() - started, 3), "records": records},
        )

    return 0 if all((record.get("resolved") == record.get("tasks")) for record in records) else 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--tasks", nargs="+", default=nu.DEFAULT_TASKS)
    parser.add_argument("--seed", type=int, default=20260515)
    parser.add_argument("--out-root", type=Path)
    parser.add_argument("--nu-bin", type=Path, default=nu.DEFAULT_NU_BIN)
    parser.add_argument("--server")
    parser.add_argument("--provider", choices=("openai-compatible", "openai"), default="openai-compatible")
    parser.add_argument("--model", default="Qwen/Qwen3.6-27B-FP8")
    parser.add_argument("--api-key-env", default="OPENAI_API_KEY")
    parser.add_argument("--omit-sampling", action="store_true")
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--verify-mode", choices=("exact", "shape"), default="shape")
    parser.add_argument("--max-turns", type=int, default=10)
    parser.add_argument("--max-tokens", type=int, default=4096)
    parser.add_argument("--resume", action="store_true")
    args = parser.parse_args()
    if args.server is None:
        args.server = "https://api.openai.com" if args.provider == "openai" else "http://192.168.1.11:8000"
    return args


def default_out_root() -> Path:
    stamp = time.strftime("%Y%m%dT%H%M%S")
    return ROOT / "target" / "runs" / f"nu-tbench20-randomized-{stamp}"


def read_summary(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    return json.loads(path.read_text())


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    raise SystemExit(main())
