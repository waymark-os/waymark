#!/usr/bin/env python3
"""Prepare and optionally run a persistent Codex+Stone dogfood environment."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
from pathlib import Path

import run_codex_stone_mcp


ROOT = Path(__file__).resolve().parents[2]
DOGFOOD_ROOT = ROOT / "target" / "dogfood"
DEFAULT_TASK = (
    "Inspect the Waymark repository through the Stone MCP tools. Identify one "
    "small, concrete shell or MCP improvement, make the change if it is clearly "
    "safe, run focused checks, and summarize the result."
)
PROMPT_PREFIX = (
    "You are dogfooding Waymark Shell on the Waymark repository itself. "
    "Use the Stone MCP tools as the primary shell surface. Start with state() "
    "and help() when useful. In warm Stone sessions, top-level value and "
    "function bindings persist across stone_eval calls; bind reusable data once "
    "and inspect it with len/head/tail summaries. stone_eval source may be a "
    "multi-line script like python -c or bash -c. Large emitted values are "
    "returned as peeks unless allow_large_output=true is set. Use escape_linux "
    "only when Stone lacks the needed capability, and give a specific reason."
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workspace",
        type=Path,
        default=ROOT,
        help="Repository workspace for Codex and Stone. Default: repo root.",
    )
    parser.add_argument(
        "--codex-home",
        type=Path,
        default=DOGFOOD_ROOT / "codex-home",
        help="Persistent CODEX_HOME for dogfood runs. Default: target/dogfood/codex-home.",
    )
    parser.add_argument(
        "--trace",
        type=Path,
        default=DOGFOOD_ROOT / "stone-mcp-trace.jsonl",
        help="Persistent Stone MCP trace path. Default: target/dogfood/stone-mcp-trace.jsonl.",
    )
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=None,
        help="waymark binary used by the MCP server. Default: existing release/debug binary.",
    )
    parser.add_argument(
        "--build",
        action="store_true",
        help="Run `cargo build -p waymark` before preparing the environment.",
    )
    parser.add_argument(
        "--seed-auth-from",
        type=Path,
        default=Path(os.environ.get("CODEX_HOME", Path.home() / ".codex")),
        help=(
            "Codex home to copy auth files from before writing dogfood config. "
            "Default: $CODEX_HOME or ~/.codex."
        ),
    )
    parser.add_argument(
        "--no-seed-auth",
        action="store_true",
        help="Do not copy Codex auth files into the dogfood CODEX_HOME.",
    )
    parser.add_argument("--codex", default="codex", help="Codex executable. Default: codex.")
    parser.add_argument("--model", default=None, help="Optional model for Codex.")
    parser.add_argument(
        "--reasoning-effort",
        default=None,
        help="Optional model_reasoning_effort in config.toml.",
    )
    parser.add_argument(
        "--codex-sandbox",
        choices=["bypass", "read-only", "workspace-write", "danger-full-access"],
        default="workspace-write",
        help="Sandbox mode for codex exec. Default: workspace-write.",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=180.0,
        help="Per-call Stone timeout. Default: 180.",
    )
    parser.add_argument(
        "--task",
        default=DEFAULT_TASK,
        help="Dogfood task prompt appended after the Waymark MCP guidance.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Run `codex mcp list` against the generated config.",
    )
    parser.add_argument(
        "--exec",
        action="store_true",
        help="Run `codex exec`. Without this flag, only prepare config and print a summary.",
    )
    parser.add_argument(
        "--json-events",
        action="store_true",
        help="Pass --json to codex exec.",
    )
    return parser.parse_args()


def prompt_for_task(task: str) -> str:
    return f"{PROMPT_PREFIX}\n\nTask:\n{task.strip()}"


def build_waymark() -> None:
    completed = subprocess.run(["cargo", "build", "-p", "waymark"], cwd=ROOT, check=False)
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)


def seed_codex_auth(source_home: Path, target_home: Path) -> list[str]:
    copied: list[str] = []
    target_home.mkdir(parents=True, exist_ok=True)
    for name in ("auth.json", "installation_id", "version.json", "models_cache.json"):
        source = source_home.expanduser() / name
        if not source.is_file():
            continue
        shutil.copy2(source, target_home / name)
        copied.append(name)
    return copied


def runner_args(args: argparse.Namespace) -> argparse.Namespace:
    return argparse.Namespace(
        workspace=args.workspace.resolve(),
        codex=args.codex,
        codex_home=args.codex_home.resolve(),
        server=ROOT / "host" / "mcp" / "stone_mcp_server.py",
        waymark_bin=args.waymark_bin,
        stone_cwd=str(args.workspace.resolve()),
        stone_cwd_compat=None,
        trace=args.trace.resolve(),
        timeout_seconds=args.timeout_seconds,
        model=args.model,
        reasoning_effort=args.reasoning_effort,
        check=args.check,
        exec=args.exec,
        json_events=args.json_events,
        codex_sandbox=args.codex_sandbox,
        prompt=prompt_for_task(args.task),
    )


def main() -> int:
    args = parse_args()
    if args.build:
        build_waymark()
    if not args.no_seed_auth:
        seed_codex_auth(args.seed_auth_from, args.codex_home)
    dogfood_args = runner_args(args)
    return run_codex_stone_mcp.run_with_home(dogfood_args, dogfood_args.codex_home)


if __name__ == "__main__":
    raise SystemExit(main())
