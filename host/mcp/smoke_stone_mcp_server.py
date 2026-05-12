#!/usr/bin/env python3
"""Protocol smoke test for the Stone MCP stdio server."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, BinaryIO


ROOT = Path(__file__).resolve().parents[2]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--server",
        type=Path,
        default=ROOT / "host" / "mcp" / "stone_mcp_server.py",
        help="Stone MCP server script to launch.",
    )
    parser.add_argument(
        "--waymark-bin",
        type=Path,
        default=None,
        help="waymark binary for the MCP server to use.",
    )
    parser.add_argument(
        "--keep-temp",
        action="store_true",
        help="Keep temporary app/work directories for debugging.",
    )
    return parser.parse_args()


def resolve_waymark_bin(path: Path | None) -> Path:
    if path is not None:
        return path.resolve()
    candidates = [
        ROOT / "target" / "debug" / "waymark",
        ROOT / "target" / "release" / "waymark",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    found = shutil.which("waymark")
    if found:
        return Path(found)
    raise SystemExit("waymark binary not found; build with `cargo build -p waymark`")


def write_message(stream: BinaryIO, message: dict[str, Any]) -> None:
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    stream.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
    stream.write(body)
    stream.flush()


def read_message(stream: BinaryIO) -> dict[str, Any]:
    headers: dict[str, str] = {}
    while True:
        line = stream.readline()
        if line == b"":
            raise EOFError("MCP server closed stdout")
        if line in (b"\r\n", b"\n"):
            break
        decoded = line.decode("ascii", errors="replace").strip()
        if ":" in decoded:
            key, value = decoded.split(":", 1)
            headers[key.lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        raise RuntimeError("MCP response missing Content-Length")
    body = stream.read(length)
    return json.loads(body.decode("utf-8"))


class McpClient:
    def __init__(self, proc: subprocess.Popen[bytes]) -> None:
        if proc.stdin is None or proc.stdout is None:
            raise RuntimeError("MCP server pipes are unavailable")
        self.proc = proc
        self.stdin = proc.stdin
        self.stdout = proc.stdout
        self.next_id = 0

    def request(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self.next_id += 1
        request_id = self.next_id
        message: dict[str, Any] = {"jsonrpc": "2.0", "id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        write_message(self.stdin, message)
        response = read_message(self.stdout)
        if response.get("id") != request_id:
            raise RuntimeError(f"response id mismatch: {response!r}")
        if "error" in response:
            raise RuntimeError(f"MCP error response: {response['error']!r}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise RuntimeError(f"MCP response result is not an object: {response!r}")
        return result

    def call_tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        result = self.request("tools/call", {"name": name, "arguments": arguments})
        structured = result.get("structuredContent")
        if not isinstance(structured, dict):
            raise RuntimeError(f"tool {name} returned no structuredContent: {result!r}")
        return structured


def run_smoke(args: argparse.Namespace, app_dir: Path, work_dir: Path) -> dict[str, Any]:
    waymark_bin = resolve_waymark_bin(args.waymark_bin)
    (app_dir / "package.json").write_text(json.dumps({"name": "waymark", "scripts": {"test": "true"}}))
    (app_dir / "events.jsonl").write_text('{"kind":"open"}\n{"kind":"closed"}\n')

    env = {
        **os.environ,
        "WAYMARK_STONE_BIN": str(waymark_bin),
        "WAYMARK_STONE_CWD": str(app_dir),
    }
    proc = subprocess.Popen(
        [sys.executable, str(args.server)],
        cwd=str(ROOT),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
    )
    client = McpClient(proc)
    try:
        initialized = client.request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "stone-mcp-smoke", "version": "0"},
            },
        )
        tools = client.request("tools/list")
        tool_names = sorted(tool["name"] for tool in tools.get("tools", []))
        expected_tools = {"stone_eval", "stone_help", "stone_call", "stone_describe", "escape_linux"}
        missing_tools = sorted(expected_tools.difference(tool_names))
        if missing_tools:
            raise RuntimeError(f"missing MCP tools: {missing_tools}")

        eval_result = client.call_tool("stone_eval", {"source": "print(7)"})
        require_ok("stone_eval", eval_result)
        if eval_result.get("value") != 7:
            raise RuntimeError(f"stone_eval returned unexpected value: {eval_result!r}")

        call_result = client.call_tool(
            "stone_call",
            {"name": "read_json", "args": {"path": "package.json"}, "cwd": str(app_dir)},
        )
        require_ok("stone_call", call_result)
        if call_result.get("value", {}).get("name") != "waymark":
            raise RuntimeError(f"stone_call returned unexpected value: {call_result!r}")

        describe_result = client.call_tool(
            "stone_describe", {"path": "package.json", "cwd": str(app_dir)}
        )
        require_ok("stone_describe", describe_result)
        if describe_result.get("value", {}).get("kind") != "json":
            raise RuntimeError(f"stone_describe returned unexpected value: {describe_result!r}")

        escape_result = client.call_tool(
            "escape_linux",
            {
                "reason": "smoke harness needs direct host command",
                "command": "printf smoke",
                "cwd": str(app_dir),
            },
        )
        require_ok("escape_linux", escape_result)
        if escape_result.get("stdout") != "smoke":
            raise RuntimeError(f"escape_linux returned unexpected output: {escape_result!r}")

        return {
            "ok": True,
            "server": initialized.get("serverInfo"),
            "tools": tool_names,
            "stone_eval": {
                "value": eval_result.get("value"),
                "backend": eval_result.get("diagnostics", {}).get("backend"),
            },
            "stone_call": {"value": call_result.get("value"), "effects": call_result.get("effects")},
            "stone_describe": {"kind": describe_result.get("value", {}).get("kind")},
            "escape_linux": {"gap": escape_result.get("gap")},
        }
    finally:
        close_server(proc)


def require_ok(label: str, result: dict[str, Any]) -> None:
    if result.get("ok") is not True:
        raise RuntimeError(f"{label} failed: {result!r}")


def close_server(proc: subprocess.Popen[bytes]) -> None:
    if proc.stdin is not None:
        proc.stdin.close()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    if proc.returncode not in (0, None):
        stderr = ""
        if proc.stderr is not None:
            stderr = proc.stderr.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"MCP server exited {proc.returncode}: {stderr}")


def main() -> int:
    args = parse_args()
    if args.keep_temp:
        root = Path(tempfile.mkdtemp(prefix="stone-mcp-smoke-"))
        app_dir = root / "app"
        work_dir = root / "work"
        app_dir.mkdir()
        work_dir.mkdir()
        summary = run_smoke(args, app_dir, work_dir)
        summary["temp_dir"] = str(root)
    else:
        with tempfile.TemporaryDirectory(prefix="stone-mcp-smoke-") as tmp:
            root = Path(tmp)
            app_dir = root / "app"
            work_dir = root / "work"
            app_dir.mkdir()
            work_dir.mkdir()
            summary = run_smoke(args, app_dir, work_dir)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
