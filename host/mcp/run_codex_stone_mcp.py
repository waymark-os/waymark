#!/usr/bin/env python3
"""Prepare and optionally run Codex with the Stone MCP server enabled."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

import write_codex_stone_mcp_config


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PROMPT = (
    "Use the available Stone MCP tools to inspect the workspace, then summarize what you found. "
    "The warm Stone session keeps top-level value and function bindings across stone_eval calls; "
    "bind intermediate data once and reuse names later instead of rereading files. "
    "A stone_eval source can be a multi-line script like python -c or bash -c. "
    "For large values, emit compact len/head/tail summaries; stone_eval returns a peek "
    "by default and allow_large_output=true forces the full emitted value. "
    "Open file handles do not persist across calls."
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--workspace",
        type=Path,
        default=Path.cwd(),
        help="Workspace passed to `codex exec --cd`. Default: current directory.",
    )
    parser.add_argument(
        "--codex",
        default="codex",
        help="Codex executable. Default: %(default)s",
    )
    parser.add_argument(
        "--codex-home",
        type=Path,
        default=None,
        help="CODEX_HOME to use. When omitted, a temporary directory is created.",
    )
    parser.add_argument(
        "--server",
        type=Path,
        default=ROOT / "host" / "mcp" / "stone_mcp_server.py",
        help="Stone MCP server script path.",
    )
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=None,
        help="waymark binary visible to the MCP server.",
    )
    parser.add_argument(
        "--stone-cwd",
        default=None,
        help="WAYMARK_STONE_CWD for the MCP server. Default: --workspace.",
    )
    parser.add_argument(
        "--trace",
        type=Path,
        default=None,
        help="Stone MCP JSONL trace path. Default: <codex-home>/stone-mcp-trace.jsonl.",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=180.0,
        help="Per-call Stone timeout. Default: %(default)s",
    )
    parser.add_argument(
        "--model",
        default=None,
        help="Optional model passed to config.toml and codex exec.",
    )
    parser.add_argument(
        "--reasoning-effort",
        default=None,
        help="Optional model_reasoning_effort in config.toml.",
    )
    parser.add_argument(
        "--prompt",
        default=DEFAULT_PROMPT,
        help="Prompt for `codex exec`. Default: %(default)r",
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
        help="Pass --json to `codex exec`.",
    )
    parser.add_argument(
        "--codex-sandbox",
        choices=["bypass", "read-only", "workspace-write", "danger-full-access"],
        default="bypass",
        help=(
            "Sandbox mode for `codex exec`. `bypass` uses Codex's explicit "
            "approval/sandbox bypass flag. Default: %(default)s"
        ),
    )
    parser.add_argument(
        "--keep-temp",
        action="store_true",
        help="Keep temporary CODEX_HOME when --codex-home is omitted.",
    )
    return parser.parse_args()


def resolve_waymark_bin(path: Path | None) -> Path:
    if path is not None:
        return path.resolve()
    candidates = [
        ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "waymark",
        ROOT / "target" / "release" / "waymark",
        ROOT / "target" / "debug" / "waymark",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    found = shutil.which("waymark")
    if found:
        return Path(found).resolve()
    raise SystemExit(
        "waymark binary not found; build with "
        "`cargo build --release -p waymark --target x86_64-unknown-linux-musl`"
    )


def prepare_config(args: argparse.Namespace, codex_home: Path) -> Path:
    workspace = args.workspace.resolve()
    trace = args.trace.resolve() if args.trace else codex_home / "stone-mcp-trace.jsonl"
    config_args = argparse.Namespace(
        model=args.model,
        reasoning_effort=args.reasoning_effort,
        server_name="stone",
        python="python3",
        server=args.server.resolve(),
        waymark_bin=resolve_waymark_bin(args.waymark_bin),
        cwd=getattr(args, "stone_cwd", None) or getattr(args, "stone_cwd_compat", None) or str(workspace),
        backend="warm-stdio",
        timeout_seconds=args.timeout_seconds,
        trace=str(trace),
        helper_dirs=os.environ.get("WAYMARK_STONE_HELPER_DIRS"),
    )
    config_path = codex_home / "config.toml"
    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(write_codex_stone_mcp_config.codex_stone_mcp_config(config_args))
    return config_path


def codex_exec_command(args: argparse.Namespace) -> list[str]:
    command = [
        args.codex,
        "exec",
        "--cd",
        str(args.workspace.resolve()),
        "--skip-git-repo-check",
    ]
    if args.codex_sandbox == "bypass":
        command.append("--dangerously-bypass-approvals-and-sandbox")
    else:
        command.extend(["--sandbox", args.codex_sandbox])
    if args.model:
        command.extend(["--model", args.model])
    if args.json_events:
        command.append("--json")
    command.append(args.prompt)
    return command


def codex_mcp_list_command(args: argparse.Namespace) -> list[str]:
    return [args.codex, "mcp", "list"]


def run_checked(
    command: list[str], env: dict[str, str], capture: bool = False
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        env=env,
        text=True,
        check=False,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )


def run_with_home(args: argparse.Namespace, codex_home: Path) -> int:
    config_path = prepare_config(args, codex_home)
    env = {**os.environ, "CODEX_HOME": str(codex_home)}
    summary: dict[str, Any] = {
        "ok": True,
        "codex_home": str(codex_home),
        "config": str(config_path),
        "workspace": str(args.workspace.resolve()),
        "trace": str(args.trace.resolve() if args.trace else codex_home / "stone-mcp-trace.jsonl"),
        "mcp_list_command": codex_mcp_list_command(args),
        "codex_exec_command": codex_exec_command(args),
    }

    if args.check:
        completed = run_checked(codex_mcp_list_command(args), env, capture=True)
        summary["mcp_list_exit_code"] = completed.returncode
        summary["mcp_list_stdout"] = completed.stdout
        summary["mcp_list_stderr"] = completed.stderr
        if completed.returncode != 0:
            summary["ok"] = False
            print(json.dumps(summary, indent=2, sort_keys=True))
            return completed.returncode

    if not args.exec:
        print(json.dumps(summary, indent=2, sort_keys=True))
        return 0

    print(json.dumps(summary, indent=2, sort_keys=True), file=sys.stderr)
    completed = run_checked(codex_exec_command(args), env)
    return completed.returncode


def main() -> int:
    args = parse_args()
    if args.codex_home is not None:
        return run_with_home(args, args.codex_home.resolve())

    tmp = tempfile.TemporaryDirectory(prefix="codex-stone-mcp-")
    try:
        code = run_with_home(args, Path(tmp.name))
        if args.keep_temp:
            print(f"kept CODEX_HOME={tmp.name}", file=sys.stderr)
            tmp.cleanup = lambda: None  # type: ignore[method-assign]
        return code
    finally:
        tmp.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
