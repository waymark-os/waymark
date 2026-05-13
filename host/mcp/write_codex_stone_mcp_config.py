#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Write a Codex config.toml that registers the Stone MCP server."""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        required=True,
        help="Path to write, usually $CODEX_HOME/config.toml for an eval run.",
    )
    parser.add_argument(
        "--server-name",
        default="stone",
        help="Codex MCP server name. Default: %(default)s",
    )
    parser.add_argument(
        "--python",
        default="python3",
        help="Python executable used to launch the MCP server. Default: %(default)s",
    )
    parser.add_argument(
        "--server",
        type=Path,
        default=ROOT / "host" / "mcp" / "stone_mcp_server.py",
        help="Stone MCP server script path. Default: %(default)s",
    )
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        required=True,
        help="waymark binary visible from the Codex/MCP environment.",
    )
    parser.add_argument(
        "--cwd",
        default="/app",
        help="Stone working directory. Default: %(default)s",
    )
    parser.add_argument(
        "--trace",
        default=None,
        help="Optional JSONL trace path for Stone MCP tool calls.",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=float,
        default=180.0,
        help="Per-call Stone timeout. Default: %(default)s",
    )
    parser.add_argument(
        "--backend",
        choices=("warm-stdio", "subprocess"),
        default="warm-stdio",
        help="Stone MCP backend. Default: %(default)s",
    )
    parser.add_argument(
        "--model",
        default=None,
        help="Optional Codex model setting to include in the generated config.",
    )
    parser.add_argument(
        "--reasoning-effort",
        default=None,
        help="Optional Codex model_reasoning_effort setting to include.",
    )
    return parser.parse_args()


def toml_string(value: str | Path) -> str:
    text = str(value)
    escaped = (
        text.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")
    )
    return f'"{escaped}"'


def codex_stone_mcp_config(args: argparse.Namespace) -> str:
    lines: list[str] = []
    if args.model:
        lines.append(f"model = {toml_string(args.model)}")
    if args.reasoning_effort:
        lines.append(f"model_reasoning_effort = {toml_string(args.reasoning_effort)}")
    if lines:
        lines.append("")

    server = args.server_name
    lines.extend(
        [
            f"[mcp_servers.{server}]",
            f"command = {toml_string(args.python)}",
            f"args = [{toml_string(args.server)}]",
            'default_tools_approval_mode = "approve"',
            "",
            f"[mcp_servers.{server}.env]",
            f"WAYMARK_STONE_BIN = {toml_string(args.waymark_bin)}",
            f"WAYMARK_STONE_CWD = {toml_string(args.cwd)}",
            f"WAYMARK_STONE_MCP_BACKEND = {toml_string(args.backend)}",
            f"WAYMARK_STONE_TIMEOUT_SECONDS = {toml_string(str(args.timeout_seconds))}",
            'WAYMARK_STONE_HOT_LOOP = "1"',
            'WAYMARK_STONE_HOT_LOOP_VM = "1"',
        ]
    )
    if args.trace:
        lines.append(f"WAYMARK_STONE_MCP_TRACE = {toml_string(args.trace)}")
    helper_dirs = getattr(args, "helper_dirs", None) or os.environ.get("WAYMARK_STONE_HELPER_DIRS")
    if helper_dirs:
        lines.append(f"WAYMARK_STONE_HELPER_DIRS = {toml_string(helper_dirs)}")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    config = codex_stone_mcp_config(args)
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(config)
    sys.stdout.write(str(args.out))
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
