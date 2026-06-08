#!/usr/bin/env python3
"""Summarize Stone Terminal-Bench experiment run directories."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("runs", nargs="+", type=Path)
    args = parser.parse_args()

    summaries = []
    for run in args.runs:
        summary_path = run / "summary.json" if run.is_dir() else run
        if not summary_path.exists():
            print(f"missing {summary_path}")
            continue
        summary = json.loads(summary_path.read_text())
        records = summary.get("records", [])
        if not records:
            continue
        summaries.append((summary_path.parent, summary))

    print("| run | resolved | syntax | parse | unsupported | runtime | escapes | help | turns | evals |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for run, summary in summaries:
        records = summary["records"]
        tasks = len(records)
        resolved = sum(1 for record in records if record.get("resolved"))
        syntax = sum_int(records, "syntax_errors")
        parse = sum_int(records, "parse_errors")
        unsupported = sum_int(records, "unsupported_feature_errors")
        runtime = sum_int(records, "runtime_errors")
        escapes = sum_int(records, "escape_linux_calls")
        help_calls = sum_int(records, "stone_help_calls")
        turns = sum_int(records, "turns")
        evals = sum_int(records, "stone_eval_calls")
        print(
            f"| {run.name} | {resolved}/{tasks} | {syntax} | {parse} | {unsupported} | "
            f"{runtime} | {escapes} | {help_calls} | {turns} | {evals} |"
        )

    failures = []
    for run, summary in summaries:
        for record in summary["records"]:
            if not record.get("resolved"):
                failures.append((run.name, record))
    if failures:
        print("\nFailures:")
        for run_name, record in failures:
            print(f"- {run_name}/{record.get('task_id')}: {record.get('failure_class')}; {record.get('failures')}")
    return 0


def sum_int(records: list[dict[str, Any]], key: str) -> int:
    return sum(int(record.get(key) or 0) for record in records)


if __name__ == "__main__":
    raise SystemExit(main())
