#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Waymark-side MCP server backed by Waymark Gateway RPC.

This server is intended to run next to the agent, including inside an agent
container. It does not own workspace state; each tool delegates to the host
gateway through the Gateway RPC Unix socket.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, TextIO


TOOLS: list[dict[str, Any]] = [
    {
        "name": "env_snapshot",
        "description": "Open a Gateway transaction for a workspace.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace": {"type": "string"},
                "container": {"type": "string"},
                "workspace_mount": {"type": "string"},
            },
            "required": ["workspace"],
        },
    },
    {
        "name": "workspace_list",
        "description": "List files in a Gateway workspace generation.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace": {"type": "string"},
                "generation": {"type": "string"},
                "path": {"type": "string"},
            },
            "required": ["workspace"],
        },
    },
    {
        "name": "workspace_stat",
        "description": "Stat one file in a Gateway workspace generation.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace": {"type": "string"},
                "generation": {"type": "string"},
                "path": {"type": "string"},
            },
            "required": ["workspace", "path"],
        },
    },
    {
        "name": "workspace_read",
        "description": "Read one text file from a Gateway workspace generation.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace": {"type": "string"},
                "generation": {"type": "string"},
                "path": {"type": "string"},
            },
            "required": ["workspace", "path"],
        },
    },
    {
        "name": "workspace_grep",
        "description": "Search files in a Gateway workspace generation.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace": {"type": "string"},
                "generation": {"type": "string"},
                "pattern": {"type": "string"},
                "path": {"type": "string"},
                "regex": {"type": "boolean"},
                "limit": {"type": "integer"},
            },
            "required": ["workspace", "pattern"],
        },
    },
    {
        "name": "state",
        "description": "Show whether a Gateway transaction has uncommitted changes.",
        "inputSchema": {
            "type": "object",
            "properties": {"tx": {"type": "string"}, "sample_limit": {"type": "integer"}},
            "required": ["tx"],
        },
    },
    {
        "name": "tx_info",
        "description": "Return Gateway transaction metadata, including retained checkpoint-run purpose.",
        "inputSchema": {
            "type": "object",
            "properties": {"tx": {"type": "string"}},
            "required": ["tx"],
        },
    },
    {
        "name": "tx_list",
        "description": "List open Gateway transactions with metadata.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace": {"type": "string"},
                "purpose": {"type": "string"},
            },
        },
    },
    {
        "name": "attempt_spawn",
        "description": "Create a Gateway task attempt and its transaction.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task": {"type": "string"},
                "workspace": {"type": "string"},
                "controller": {"type": "string"},
                "capability_profile": {"type": "string"},
                "container": {"type": "string"},
                "workspace_mount": {"type": "string"},
                "resource_limits": {"type": "object", "additionalProperties": {"type": "string"}},
                "metadata": {"type": "object", "additionalProperties": {"type": "string"}},
                "task_spec": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "objective": {"type": "string"},
                    },
                    "additionalProperties": True,
                },
                "program": {
                    "type": "object",
                    "additionalProperties": True,
                },
                "workspace_source": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string"},
                        "workspace": {"type": "string"},
                        "generation": {"type": "string"},
                        "attempt": {"type": "string"},
                        "checkpoint": {"type": "string"},
                    },
                },
                "context_source": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string"},
                        "attempt": {"type": "string"},
                        "context": {"type": "string"},
                        "include_last_turns": {"type": "integer"},
                    },
                },
                "capabilities": {"type": "object", "additionalProperties": {"type": "string"}},
                "start": {"type": "boolean"},
            },
        },
    },
    {
        "name": "attempt_fork",
        "description": "Fork a child Gateway task attempt from a parent attempt checkpoint.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "parent_attempt": {"type": "string"},
                "task": {"type": "string"},
                "controller": {"type": "string"},
                "capability_profile": {"type": "string"},
                "container": {"type": "string"},
                "workspace_mount": {"type": "string"},
                "resource_limits": {"type": "object", "additionalProperties": {"type": "string"}},
                "metadata": {"type": "object", "additionalProperties": {"type": "string"}},
            },
            "required": ["parent_attempt"],
        },
    },
    {
        "name": "attempt_list",
        "description": "List Gateway task attempts with optional filters.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task": {"type": "string"},
                "workspace": {"type": "string"},
                "state": {"type": "string"},
            },
        },
    },
    {
        "name": "attempt_info",
        "description": "Return Gateway task attempt metadata.",
        "inputSchema": {
            "type": "object",
            "properties": {"attempt": {"type": "string"}},
            "required": ["attempt"],
        },
    },
    {
        "name": "attempt_start",
        "description": "Start an existing Gateway task attempt's recorded controller program.",
        "inputSchema": {
            "type": "object",
            "properties": {"attempt": {"type": "string"}},
            "required": ["attempt"],
        },
    },
    {
        "name": "attempt_logs",
        "description": "Read bounded stdout/stderr from an attempt's recorded controller run.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "attempt": {"type": "string"},
                "stream": {
                    "type": "string",
                    "enum": ["stdout", "stderr", "both"],
                    "default": "both",
                },
                "tail": {"type": "integer", "minimum": 1},
                "max_bytes": {"type": "integer", "minimum": 1},
            },
            "required": ["attempt"],
        },
    },
    {
        "name": "attempt_state",
        "description": "Return Gateway task attempt metadata plus transaction diff state.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "attempt": {"type": "string"},
                "sample_limit": {"type": "integer"},
            },
            "required": ["attempt"],
        },
    },
    {
        "name": "attempt_finish",
        "description": "Close a Gateway task attempt by commit, rollback, fail, or kill.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "attempt": {"type": "string"},
                "action": {"type": "string"},
                "message": {"type": "string"},
                "reason": {"type": "string"},
                "allow_risky": {"type": "boolean"},
            },
            "required": ["attempt", "action"],
        },
    },
    {
        "name": "attempt_run_process",
        "description": "Run a host-authority controller process attached to a Gateway task attempt.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "attempt": {"type": "string"},
                "argv": {"type": "array", "items": {"type": "string"}},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
            },
            "required": ["attempt", "argv"],
        },
    },
    {
        "name": "stone_call",
        "description": "Run a Linux command in the Gateway transaction view.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tx": {"type": "string"},
                "checkpoint": {"type": "string"},
                "workspace": {"type": "string"},
                "dry_run": {"type": "boolean"},
                "keep_tx": {"type": "boolean"},
                "image": {"type": "string"},
                "container": {"type": "string"},
                "argv": {"type": "array", "items": {"type": "string"}},
                "workspace_mount": {"type": "string"},
                "read_only_mounts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "host_path": {"type": "string"},
                            "container_path": {"type": "string"},
                        },
                        "required": ["host_path", "container_path"],
                    },
                },
                "workdir": {"type": "string"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
                "user": {"type": "string"},
                "stdin": {"type": "string"},
                "timeout_ms": {"type": "integer"},
            },
            "required": ["argv"],
        },
    },
    {
        "name": "restore",
        "description": "Restore paths in a Gateway transaction.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tx": {"type": "string"},
                "paths": {"type": "array", "items": {"type": "string"}},
            },
            "required": ["tx", "paths"],
        },
    },
    {
        "name": "checkpoint",
        "description": "Persist the current Gateway transaction state as a named checkpoint.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tx": {"type": "string"},
                "reason": {"type": "string"},
            },
            "required": ["tx"],
        },
    },
    {
        "name": "fork",
        "description": "Open an independent Gateway transaction from a checkpoint.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "checkpoint": {"type": "string"},
                "container": {"type": "string"},
                "workspace_mount": {"type": "string"},
            },
            "required": ["checkpoint"],
        },
    },
    {
        "name": "restore_checkpoint",
        "description": "Restore an open Gateway transaction to a named checkpoint.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tx": {"type": "string"},
                "checkpoint": {"type": "string"},
            },
            "required": ["tx", "checkpoint"],
        },
    },
    {
        "name": "checkpoint_list",
        "description": "List Gateway checkpoints.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "workspace": {"type": "string"},
                "include_discarded": {"type": "boolean"},
            },
        },
    },
    {
        "name": "checkpoint_discard",
        "description": "Discard a Gateway checkpoint.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "checkpoint": {"type": "string"},
                "force": {"type": "boolean"},
            },
            "required": ["checkpoint"],
        },
    },
    {
        "name": "checkpoint_gc",
        "description": "Report checkpoint storage reachability and reclaimable orphan payloads without deleting anything.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "apply": {"type": "boolean"},
            },
        },
    },
    {
        "name": "run_checkpoint",
        "description": "Fork a checkpoint, run a Linux command in the fork, return output plus diff, then roll the fork back.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "checkpoint": {"type": "string"},
                "image": {"type": "string"},
                "argv": {"type": "array", "items": {"type": "string"}},
                "keep_tx": {
                    "type": "boolean",
                    "description": "Keep the forked transaction open instead of rolling it back.",
                },
                "workspace_mount": {"type": "string"},
                "read_only_mounts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "host_path": {"type": "string"},
                            "container_path": {"type": "string"},
                        },
                        "required": ["host_path", "container_path"],
                    },
                },
                "workdir": {"type": "string"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
                "user": {"type": "string"},
                "stdin": {"type": "string"},
                "timeout_ms": {"type": "integer"},
            },
            "required": ["checkpoint", "image", "argv"],
        },
    },
    {
        "name": "rollback",
        "description": "Discard a Gateway transaction.",
        "inputSchema": {
            "type": "object",
            "properties": {"tx": {"type": "string"}},
            "required": ["tx"],
        },
    },
    {
        "name": "commit",
        "description": "Publish a Gateway transaction as a new generation.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tx": {"type": "string"},
                "message": {"type": "string"},
                "allow_risky": {"type": "boolean"},
            },
            "required": ["tx"],
        },
    },
    {
        "name": "finish",
        "description": "Check that a Gateway transaction is clean or explicitly resolved.",
        "inputSchema": {
            "type": "object",
            "properties": {"tx": {"type": "string"}},
            "required": ["tx"],
        },
    },
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--gateway-bin",
        default=os.environ.get("WAYMARK_GATEWAY_BIN", "waymark-gateway"),
        help="Gateway CLI with the protobuf RPC client.",
    )
    parser.add_argument(
        "--socket",
        type=Path,
        default=Path(os.environ.get("WAYMARK_GATEWAY_SOCKET", "/run/waymark/gateway.sock")),
    )
    parser.add_argument("--trace", type=Path, default=None)
    return parser.parse_args()


def json_rpc_result(request_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def json_rpc_error(request_id: Any, code: int, message: str, data: Any = None) -> dict[str, Any]:
    error: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": request_id, "error": error}


def content_result(value: dict[str, Any]) -> dict[str, Any]:
    return {
        "content": [{"type": "text", "text": json.dumps(value, indent=2, sort_keys=True)}],
        "structuredContent": value,
    }


class GatewayCliRpc:
    def __init__(self, gateway_bin: str, socket_path: Path) -> None:
        self.gateway_bin = gateway_bin
        self.socket_path = socket_path

    def call(self, method: str, args: list[str]) -> dict[str, Any]:
        command = [self.gateway_bin, "rpc", "call", "--socket", str(self.socket_path), method, *args]
        completed = subprocess.run(
            command,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        result = {
            "ok": completed.returncode == 0,
            "method": method,
            "command": command,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
            "exit_code": completed.returncode,
        }
        return decorate_gateway_result(method, result)


class GatewayMcp:
    def __init__(self, rpc: GatewayCliRpc, trace: Path | None = None) -> None:
        self.rpc = rpc
        self.trace = trace
        if self.trace is not None:
            self.trace.parent.mkdir(parents=True, exist_ok=True)

    def trace_call(self, name: str, args: dict[str, Any], result: dict[str, Any], duration_ms: int) -> None:
        if self.trace is None:
            return
        record = {
            "ts_ms": int(time.time() * 1000),
            "tool": name,
            "args": args,
            "ok": result.get("ok"),
            "duration_ms": duration_ms,
            "exit_code": result.get("exit_code"),
            "stderr": bound_text(result.get("stderr"), 600),
            "stdout": bound_text(result.get("stdout"), 600),
        }
        for field in (
            "attempt",
            "parent_attempt",
            "checkpoint",
            "source_tx",
            "branch_tx",
            "dry_run",
            "rolled_back",
            "retained",
        ):
            if field in result:
                record[field] = result[field]
        for result_field, trace_field in (
            ("source_rollback", "source_rollback_ok"),
            ("branch_rollback", "branch_rollback_ok"),
            ("checkpoint_cleanup", "checkpoint_cleanup_ok"),
            ("dry_run_cleanup", "dry_run_cleanup_ok"),
        ):
            nested = result.get(result_field)
            if isinstance(nested, dict):
                record[trace_field] = bool(nested.get("ok"))
        with self.trace.open("a", encoding="utf-8") as file:
            file.write(json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n")

    def tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        start = time.monotonic()
        try:
            result = self.run_tool(name, args)
            return result
        finally:
            duration_ms = int((time.monotonic() - start) * 1000)
            if "result" in locals():
                self.trace_call(name, args, result, duration_ms)

    def run_tool(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        if name == "env_snapshot":
            call_args = ["--workspace", required(args, "workspace")]
            if args.get("container"):
                call_args.extend(["--container", str(args["container"])])
                call_args.extend(["--workspace-mount", str(args.get("workspace_mount", "/app"))])
            return self.rpc.call("env.snapshot", call_args)
        if name == "workspace_list":
            call_args = ["--workspace", required(args, "workspace")]
            add_optional(call_args, args, "generation", "--generation")
            add_optional(call_args, args, "path", "--path")
            return self.rpc.call("workspace.list", call_args)
        if name == "workspace_stat":
            call_args = ["--workspace", required(args, "workspace"), "--path", required(args, "path")]
            add_optional(call_args, args, "generation", "--generation")
            return self.rpc.call("workspace.stat", call_args)
        if name == "workspace_read":
            call_args = ["--workspace", required(args, "workspace"), "--path", required(args, "path")]
            add_optional(call_args, args, "generation", "--generation")
            return self.rpc.call("workspace.read", call_args)
        if name == "workspace_grep":
            call_args = ["--workspace", required(args, "workspace"), "--pattern", required(args, "pattern")]
            add_optional(call_args, args, "generation", "--generation")
            add_optional(call_args, args, "path", "--path")
            if args.get("regex"):
                call_args.append("--regex")
            if args.get("limit") is not None:
                call_args.extend(["--limit", str(args["limit"])])
            return self.rpc.call("workspace.grep", call_args)
        if name == "state":
            sample_limit = str(args.get("sample_limit", 50))
            return self.rpc.call("env.diff", ["--tx", required(args, "tx"), "--sample-limit", sample_limit])
        if name in {"tx_info", "env_tx_info"}:
            return self.rpc.call("env.tx_info", ["--tx", required(args, "tx")])
        if name in {"tx_list", "env_tx_list"}:
            call_args: list[str] = []
            add_optional(call_args, args, "workspace", "--workspace")
            add_optional(call_args, args, "purpose", "--purpose")
            return self.rpc.call("env.tx_list", call_args)
        if name == "attempt_spawn":
            task_spec = dict(args.get("task_spec") or {})
            workspace_source = dict(args.get("workspace_source") or {})
            task = args.get("task") or task_spec.get("id")
            workspace = args.get("workspace") or workspace_source.get("workspace")
            if not task or not workspace:
                raise ValueError("attempt_spawn requires task/workspace or task_spec.id/workspace_source.workspace")
            call_args = ["--task", str(task), "--workspace", str(workspace)]
            add_attempt_options(call_args, args)
            add_spawn_options(call_args, args)
            return self.rpc.call("attempt.spawn", call_args)
        if name == "attempt_fork":
            call_args = ["--parent-attempt", required(args, "parent_attempt")]
            if args.get("task"):
                call_args.extend(["--task", str(args["task"])])
            add_attempt_options(call_args, args)
            return self.rpc.call("attempt.fork", call_args)
        if name == "attempt_list":
            call_args: list[str] = []
            add_optional(call_args, args, "task", "--task")
            add_optional(call_args, args, "workspace", "--workspace")
            add_optional(call_args, args, "state", "--state")
            return self.rpc.call("attempt.list", call_args)
        if name == "attempt_info":
            return self.rpc.call("attempt.info", ["--attempt", required(args, "attempt")])
        if name == "attempt_start":
            return self.rpc.call("attempt.start", ["--attempt", required(args, "attempt")])
        if name == "attempt_logs":
            call_args = ["--attempt", required(args, "attempt")]
            add_optional(call_args, args, "stream", "--stream")
            add_optional(call_args, args, "tail", "--tail")
            add_optional(call_args, args, "max_bytes", "--max-bytes")
            return self.rpc.call("attempt.logs", call_args)
        if name == "attempt_state":
            call_args = ["--attempt", required(args, "attempt")]
            if args.get("sample_limit") is not None:
                call_args.extend(["--sample-limit", str(args["sample_limit"])])
            return self.rpc.call("attempt.state", call_args)
        if name == "attempt_finish":
            action = required(args, "action")
            if action not in {"commit", "rollback", "fail", "kill"}:
                return {
                    "ok": False,
                    "stdout": "",
                    "stderr": f"unknown attempt finish action: {action}",
                    "exit_code": 2,
                }
            call_args = ["--attempt", required(args, "attempt"), "--action", action]
            add_optional(call_args, args, "message", "--message")
            add_optional(call_args, args, "reason", "--reason")
            if args.get("allow_risky"):
                call_args.append("--allow-risky")
            return self.rpc.call("attempt.finish", call_args)
        if name == "attempt_run_process":
            argv = args.get("argv")
            if not isinstance(argv, list) or not all(isinstance(part, str) for part in argv):
                raise ValueError("attempt_run_process requires argv: list[str]")
            call_args = ["--attempt", required(args, "attempt")]
            for key, value in sorted((args.get("env") or {}).items()):
                call_args.extend(["--env", f"{key}={value}"])
            call_args.append("--")
            call_args.extend(argv)
            return self.rpc.call("attempt.run_process", call_args)
        if name in {"finish", "env_finish"}:
            return self.rpc.call("env.finish", ["--tx", required(args, "tx")])
        if name in {"restore", "env_restore"}:
            return self.rpc.call("env.restore", ["--tx", required(args, "tx"), *string_list(args.get("paths"))])
        if name in {"checkpoint", "env_checkpoint"}:
            call_args = ["--tx", required(args, "tx")]
            if args.get("reason"):
                call_args.extend(["--reason", str(args["reason"])])
            return self.rpc.call("env.checkpoint", call_args)
        if name in {"fork", "env_fork"}:
            call_args = ["--checkpoint", required(args, "checkpoint")]
            if args.get("container"):
                call_args.extend(["--container", str(args["container"])])
                call_args.extend(["--workspace-mount", str(args.get("workspace_mount", "/app"))])
            return self.rpc.call("env.fork", call_args)
        if name in {"restore_checkpoint", "env_restore_checkpoint"}:
            return self.rpc.call(
                "env.restore_checkpoint",
                ["--tx", required(args, "tx"), "--checkpoint", required(args, "checkpoint")],
            )
        if name in {"checkpoint_list", "env_checkpoint_list"}:
            call_args: list[str] = []
            add_optional(call_args, args, "workspace", "--workspace")
            if args.get("include_discarded"):
                call_args.append("--include-discarded")
            return self.rpc.call("env.checkpoint_list", call_args)
        if name in {"checkpoint_discard", "env_checkpoint_discard"}:
            call_args = ["--checkpoint", required(args, "checkpoint")]
            if args.get("force"):
                call_args.append("--force")
            return self.rpc.call("env.checkpoint_discard", call_args)
        if name in {"checkpoint_gc", "env_checkpoint_gc"}:
            call_args: list[str] = []
            if args.get("apply"):
                call_args.append("--apply")
            return self.rpc.call("env.checkpoint_gc", call_args)
        if name in {"run_checkpoint", "env_run_checkpoint"}:
            call_args = [
                "--checkpoint",
                required(args, "checkpoint"),
                "--image",
                required(args, "image"),
                "--workspace-mount",
                str(args.get("workspace_mount", "/app")),
            ]
            add_optional(call_args, args, "workdir", "--workdir")
            add_optional(call_args, args, "user", "--user")
            add_optional(call_args, args, "stdin", "--stdin")
            if args.get("timeout_ms") is not None:
                call_args.extend(["--timeout-ms", str(args["timeout_ms"])])
            if args.get("keep_tx"):
                call_args.append("--keep-tx")
            add_read_only_mounts(call_args, args)
            for key, value in sorted(dict(args.get("env", {})).items()):
                call_args.extend(["--env", f"{key}={value}"])
            call_args.append("--")
            call_args.extend(string_list(args.get("argv")))
            return self.rpc.call("env.run_checkpoint", call_args)
        if name in {"rollback", "env_rollback"}:
            return self.rpc.call("env.rollback", ["--tx", required(args, "tx")])
        if name in {"commit", "env_commit"}:
            call_args = ["--tx", required(args, "tx")]
            if args.get("message"):
                call_args.extend(["--message", str(args["message"])])
            if args.get("allow_risky"):
                call_args.append("--allow-risky")
            return self.rpc.call("env.commit", call_args)
        if name == "stone_call":
            return self.stone_call(args)
        raise ValueError(f"unknown tool: {name}")

    def stone_call(self, args: dict[str, Any]) -> dict[str, Any]:
        dry_run = bool(args.get("dry_run"))
        tx = args.get("tx")
        cleanup: dict[str, Any] | None = None
        if dry_run:
            if args.get("checkpoint"):
                if args.get("container"):
                    return {
                        "ok": False,
                        "stdout": "",
                        "stderr": "checkpoint dry-run does not support attached containers yet",
                        "exit_code": 2,
                        "dry_run": True,
                        "checkpoint": args["checkpoint"],
                    }
                call_args = [
                    "--checkpoint",
                    required(args, "checkpoint"),
                    "--image",
                    required(args, "image"),
                    "--workspace-mount",
                    str(args.get("workspace_mount", "/app")),
                ]
                add_optional(call_args, args, "workdir", "--workdir")
                add_optional(call_args, args, "user", "--user")
                add_optional(call_args, args, "stdin", "--stdin")
                if args.get("timeout_ms") is not None:
                    call_args.extend(["--timeout-ms", str(args["timeout_ms"])])
                if args.get("keep_tx"):
                    call_args.append("--keep-tx")
                add_read_only_mounts(call_args, args)
                env = args.get("env")
                if isinstance(env, dict):
                    for key, value in sorted(env.items()):
                        call_args.extend(["--env", f"{key}={value}"])
                call_args.append("--")
                call_args.extend(string_list(args.get("argv")))
                result = self.rpc.call("env.run_checkpoint", call_args)
                return {
                    **result,
                    "dry_run": True,
                    "checkpoint": args["checkpoint"],
                    "branch_tx": field_from_stdout(result.get("stdout", ""), "tx"),
                    "rolled_back": result.get("ok") and "rolled_back\ttrue" in result.get("stdout", ""),
                    "retained": result.get("ok") and "rolled_back\ttrue" not in result.get("stdout", ""),
                }
            workspace = required(args, "workspace")
            snapshot = self.rpc.call("env.snapshot", ["--workspace", workspace])
            tx = parse_tx(snapshot["stdout"])
            if not tx:
                return {**snapshot, "error": "dry-run env.snapshot did not return a tx"}
            checkpoint_result = self.rpc.call(
                "env.checkpoint",
                ["--tx", tx, "--reason", "stone_call dry-run baseline"],
            )
            checkpoint = field_from_stdout(checkpoint_result.get("stdout", ""), "checkpoint")
            if not checkpoint:
                checkpoint = checkpoint_result.get("stdout", "").strip()
            if not checkpoint_result.get("ok") or not checkpoint:
                cleanup = self.rpc.call("env.rollback", ["--tx", tx])
                return {
                    **checkpoint_result,
                    "dry_run": True,
                    "workspace": workspace,
                    "source_tx": tx,
                    "rolled_back": False,
                    "retained": False,
                    "source_rollback": cleanup,
                    "error": "dry-run env.checkpoint did not return a checkpoint",
                }
            args = {**args, "checkpoint": checkpoint}
            result = self.stone_call(args)
            cleanup = self.rpc.call("env.rollback", ["--tx", tx])
            result = {
                **result,
                "workspace": workspace,
                "source_tx": tx,
                "checkpoint": checkpoint,
                "source_rollback": cleanup,
            }
            if not args.get("keep_tx"):
                checkpoint_cleanup = self.rpc.call(
                    "env.checkpoint_discard",
                    ["--checkpoint", checkpoint, "--force"],
                )
                result["checkpoint_cleanup"] = checkpoint_cleanup
                if not checkpoint_cleanup.get("ok"):
                    result["ok"] = False
            if not cleanup.get("ok"):
                result["ok"] = False
            return result
        elif not tx:
            raise ValueError("stone_call requires tx unless dry_run is true")

        call_args = [
            "--tx",
            str(tx),
            "--workspace-mount",
            str(args.get("workspace_mount", "/app")),
        ]
        if args.get("container"):
            call_args.extend(["--container", str(args["container"])])
        else:
            call_args.extend(["--image", required(args, "image")])
        if args.get("workdir"):
            call_args.extend(["--workdir", str(args["workdir"])])
        if args.get("user"):
            call_args.extend(["--user", str(args["user"])])
        if args.get("stdin") is not None:
            call_args.extend(["--stdin", str(args["stdin"])])
        if args.get("timeout_ms") is not None:
            call_args.extend(["--timeout-ms", str(args["timeout_ms"])])
        if not args.get("container"):
            add_read_only_mounts(call_args, args)
        env = args.get("env")
        if isinstance(env, dict):
            for key, value in sorted(env.items()):
                call_args.extend(["--env", f"{key}={value}"])
        call_args.append("--")
        call_args.extend(string_list(args.get("argv")))
        result = self.rpc.call("linux.exec", call_args)
        if dry_run and tx:
            cleanup = self.rpc.call("env.rollback", ["--tx", str(tx)])
            result = {**result, "dry_run": True, "dry_run_tx": tx, "dry_run_cleanup": cleanup}
        return result


def required(args: dict[str, Any], name: str) -> str:
    value = args.get(name)
    if not isinstance(value, str) or not value:
        raise ValueError(f"missing required string argument: {name}")
    return value


def string_list(value: Any) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError("expected list[str]")
    return value


def add_optional(call_args: list[str], args: dict[str, Any], name: str, flag: str) -> None:
    value = args.get(name)
    if isinstance(value, str) and value:
        call_args.extend([flag, value])


def add_attempt_options(call_args: list[str], args: dict[str, Any]) -> None:
    add_optional(call_args, args, "controller", "--controller")
    add_optional(call_args, args, "capability_profile", "--capability-profile")
    if args.get("container"):
        call_args.extend(["--container", str(args["container"])])
        call_args.extend(["--workspace-mount", str(args.get("workspace_mount", "/app"))])
    elif args.get("workspace_mount"):
        call_args.extend(["--workspace-mount", str(args["workspace_mount"])])
    for key, value in sorted(dict(args.get("resource_limits") or {}).items()):
        call_args.extend(["--limit", f"{key}={value}"])
    for key, value in sorted(dict(args.get("metadata") or {}).items()):
        call_args.extend(["--meta", f"{key}={value}"])


def add_spawn_options(call_args: list[str], args: dict[str, Any]) -> None:
    task_spec = dict(args.get("task_spec") or {})
    if task_spec.get("id"):
        call_args.extend(["--task-spec-id", str(task_spec["id"])])
    if task_spec.get("objective"):
        call_args.extend(["--task-objective", str(task_spec["objective"])])

    program = dict(args.get("program") or {})
    nested_stone = dict(program.get("stone") or {})
    nested_builtin = dict(program.get("builtin") or {})
    nested_artifact = dict(program.get("artifact") or {})
    kind = str(program.get("kind") or "").lower()
    if nested_stone or kind == "stone" or program.get("source"):
        source = nested_stone.get("source", program.get("source", ""))
        call_args.extend(["--program-stone-source", str(source)])
        entrypoint = nested_stone.get("entrypoint", program.get("entrypoint"))
        if entrypoint:
            call_args.extend(["--program-entrypoint", str(entrypoint)])
    elif nested_builtin or kind == "builtin" or program.get("name"):
        name = nested_builtin.get("name", program.get("name", ""))
        call_args.extend(["--program-builtin", str(name)])
        args_json = nested_builtin.get("args_json", program.get("args_json"))
        if args_json:
            call_args.extend(["--program-args-json", str(args_json)])
    elif nested_artifact or kind == "artifact" or program.get("artifact"):
        artifact = nested_artifact.get("artifact", program.get("artifact", ""))
        call_args.extend(["--program-artifact", str(artifact)])
        entrypoint = nested_artifact.get("entrypoint", program.get("entrypoint"))
        if entrypoint:
            call_args.extend(["--program-entrypoint", str(entrypoint)])
        args_json = nested_artifact.get("args_json", program.get("args_json"))
        if args_json:
            call_args.extend(["--program-args-json", str(args_json)])

    workspace_source = dict(args.get("workspace_source") or {})
    for key, flag in [
        ("kind", "--workspace-source-kind"),
        ("workspace", "--workspace-source-workspace"),
        ("generation", "--workspace-source-generation"),
        ("attempt", "--workspace-source-attempt"),
        ("checkpoint", "--workspace-source-checkpoint"),
    ]:
        if workspace_source.get(key):
            call_args.extend([flag, str(workspace_source[key])])

    context_source = dict(args.get("context_source") or {})
    for key, flag in [
        ("kind", "--context-source-kind"),
        ("attempt", "--context-source-attempt"),
        ("context", "--context-source-context"),
    ]:
        if context_source.get(key):
            call_args.extend([flag, str(context_source[key])])
    if context_source.get("include_last_turns") is not None:
        call_args.extend(["--context-include-last-turns", str(context_source["include_last_turns"])])

    for key, value in sorted(dict(args.get("capabilities") or {}).items()):
        call_args.extend(["--capability", f"{key}={value}"])
    if args.get("start"):
        call_args.append("--start")


def add_read_only_mounts(call_args: list[str], args: dict[str, Any]) -> None:
    for mount in args.get("read_only_mounts", []) or []:
        call_args.extend(
            [
                "--read-only-mount",
                f"{mount['host_path']}:{mount['container_path']}",
            ]
        )


def parse_tx(stdout: str) -> str | None:
    for line in stdout.splitlines():
        parts = line.split("\t", 1)
        if len(parts) == 2 and parts[0] == "tx":
            return parts[1]
    return None


def field_from_stdout(stdout: str, field: str) -> str:
    prefix = f"{field}\t"
    for line in stdout.splitlines():
        if line.startswith(prefix):
            return line[len(prefix) :]
    return ""


def decorate_gateway_result(method: str, result: dict[str, Any]) -> dict[str, Any]:
    stdout = result.get("stdout") if isinstance(result.get("stdout"), str) else ""
    if method == "linux.exec":
        env_diff = stdout.split("\ndiff\n", 1)[1] if "\ndiff\n" in stdout else ""
        result["env_diff"] = env_diff
        result["env_warnings"] = warning_lines(env_diff)
        result["env_clean"] = preview_clean(env_diff)
        return result
    if method in {"env.diff", "env.finish", "env.restore"}:
        result["env_diff"] = stdout
        result["env_warnings"] = warning_lines(stdout)
        result["env_clean"] = preview_clean(stdout)
        if method == "env.finish":
            result["next_actions"] = next_actions(stdout)
            result["ok"] = result["ok"] and (result["env_clean"] or "transaction closed" in stdout)
        return result
    return result


def warning_lines(text: str) -> list[str]:
    return [line for line in text.splitlines() if line.startswith("warning\t")]


def next_actions(text: str) -> list[str]:
    for line in text.splitlines():
        if line.startswith("next_actions\t"):
            raw = line.split("\t", 1)[1]
            return [item for item in raw.split(",") if item]
    return []


def preview_clean(text: str) -> bool:
    stripped = text.strip()
    if stripped in {"", "clean", "transaction closed", "rolled_back", "No changes"}:
        return True
    for line in text.splitlines():
        if line == "changes\tcreated=0 modified=0 deleted=0 type_changed=0 env=0":
            return True
        if line.startswith(("Created\t", "Modified\t", "Deleted\t", "TypeChanged\t", "warning\t")):
            return False
    if "next_actions\t" in text:
        return False
    return False


def bound_text(value: Any, max_bytes: int) -> str | None:
    if not isinstance(value, str):
        return None
    encoded = value.encode("utf-8", errors="replace")
    if len(encoded) <= max_bytes:
        return value
    return encoded[:max_bytes].decode("utf-8", errors="replace") + "\n...[truncated]"


def serve(server: GatewayMcp, stdin: Any = sys.stdin.buffer, stdout: Any = sys.stdout.buffer) -> None:
    framing = "headers"
    while True:
        request = read_message(stdin)
        if request is None:
            return
        if request.get("_framing") == "jsonl":
            framing = "jsonl"
            request.pop("_framing", None)
        try:
            method = request.get("method")
            request_id = request.get("id")
            if method == "initialize":
                response = json_rpc_result(
                    request_id,
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "waymark-gateway-mcp", "version": "0.1.0"},
                    },
                )
            elif method == "notifications/initialized":
                continue
            elif method == "tools/list":
                response = json_rpc_result(request_id, {"tools": TOOLS})
            elif method == "tools/call":
                params = request.get("params") if isinstance(request.get("params"), dict) else {}
                name = params.get("name")
                args = params.get("arguments") if isinstance(params.get("arguments"), dict) else {}
                if not isinstance(name, str):
                    raise ValueError("tools/call requires string params.name")
                response = json_rpc_result(request_id, content_result(server.tool(name, args)))
            else:
                response = json_rpc_error(request_id, -32601, f"unknown method: {method}")
        except Exception as err:
            response = json_rpc_error(request.get("id") if isinstance(request, dict) else None, -32000, str(err))
        write_message(stdout, response, framing)


def read_message(stdin: Any) -> dict[str, Any] | None:
    headers: dict[str, str] = {}
    while True:
        line = stdin.readline()
        if line == b"":
            return None
        stripped = line.strip()
        if not stripped:
            break
        if stripped.startswith(b"{"):
            value = json.loads(stripped.decode("utf-8"))
            if isinstance(value, dict):
                value["_framing"] = "jsonl"
            return value
        decoded = line.decode("ascii", errors="replace").strip()
        if ":" in decoded:
            key, value = decoded.split(":", 1)
            headers[key.lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        return None
    return json.loads(stdin.read(length).decode("utf-8"))


def write_message(stdout: Any, message: dict[str, Any], framing: str) -> None:
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    if framing == "jsonl":
        stdout.write(body + b"\n")
    else:
        stdout.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
        stdout.write(body)
    stdout.flush()


def main() -> int:
    args = parse_args()
    serve(GatewayMcp(GatewayCliRpc(args.gateway_bin, args.socket), args.trace))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
