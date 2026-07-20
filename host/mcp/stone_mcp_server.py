#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Small MCP stdio server for host-side Stone evaluation."""

from __future__ import annotations

import abc
import csv
import json
import os
import re
import select
import shutil
import struct
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, BinaryIO, TextIO


MAX_OUTPUT_BYTES = 64 * 1024
MAX_PREVIEW_BYTES = 16 * 1024
LARGE_RESULT_BYTES = 12 * 1024
LARGE_RESULT_LIST_ITEMS = 20
LARGE_RESULT_PEEK_ITEMS = 5
DEFAULT_TIMEOUT_SECONDS = 180.0


HELP_TABLE: dict[str, dict[str, Any]] = {
    "cat": {
        "name": "cat",
        "signature": "cat(path: str | file_record) -> str",
        "effects": ["read_file"],
        "example": 'text = cat("README.md")',
    },
    "edit": {
        "name": "edit",
        "signature": "edit(path: str, old: str, new: str, all: bool = False) -> record",
        "effects": ["read_file", "write_file"],
        "example": 'edit("fix.py", "pass\\n", "return 1\\n")',
    },
    "edit_file": {
        "name": "edit_file",
        "signature": "edit_file(path: str, old: str, new: str, all: bool = False) -> record",
        "effects": ["read_file", "write_file"],
        "example": 'edit_file("fix.py", "pass\\n", "return 1\\n")',
        "alias_for": "edit",
    },
    "find": {
        "name": "find",
        "signature": "find(root: str, name_glob: str = '*', max_depth: int? = None) -> list[record]",
        "effects": ["read_dir"],
        "example": 'files = find(path=".", name="*.jsonl")',
    },
    "first": {
        "name": "first",
        "signature": "first(values: list, count: int? = None) -> Any | list",
        "effects": [],
        "example": "sample = first(rows, 5)",
    },
    "head": {
        "name": "head",
        "signature": "head(values: list, count: int? = None) -> Any | list",
        "effects": [],
        "example": "sample = head(rows, 5)",
        "alias_for": "first",
    },
    "open": {
        "name": "open",
        "signature": "open(path: str, mode: str = 'r') -> file",
        "effects": ["read_file", "write_file", "append_file", "create_parent_dirs"],
        "example": 'open("out.txt", "w").write("hello\\n")',
    },
    "read_csv": {
        "name": "read_csv",
        "signature": "read_csv(path: str | file_record, limit: int? = None) -> list[record]",
        "effects": ["read_file"],
        "example": 'rows = read_csv("input.csv", limit=5)',
    },
    "read_file": {
        "name": "read_file",
        "signature": "read_file(path: str, max_bytes: int? = None, start_line: int? = None, end_line: int? = None) -> str",
        "effects": ["read_file"],
        "example": 'text = read_file("README.md")',
        "alias_for": "read_text",
    },
    "read_json": {
        "name": "read_json",
        "signature": "read_json(path: str | file_record) -> Any",
        "effects": ["read_file"],
        "example": 'config = read_json("package.json")',
    },
    "read_jsonl": {
        "name": "read_jsonl",
        "signature": "read_jsonl(path: str | file_record, limit: int? = None) -> list[Any]",
        "effects": ["read_file"],
        "example": 'rows = read_jsonl("events.jsonl", limit=10)',
    },
    "read_text": {
        "name": "read_text",
        "signature": "read_text(path: str, max_bytes: int? = None, start_line: int? = None, end_line: int? = None) -> str",
        "effects": ["read_file"],
        "example": 'text = read_text("README.md")',
    },
    "write_file": {
        "name": "write_file",
        "signature": "write_file(path: str, text: str, append: bool = False) -> record",
        "effects": ["write_file", "create_parent_dirs"],
        "example": 'write_file("out.txt", "hello\\n")',
        "alias_for": "write_text",
    },
    "write_json": {
        "name": "write_json",
        "signature": "write_json(path: str, value: Any) -> int",
        "effects": ["write_file", "create_parent_dirs"],
        "example": 'write_json("out/report.json", {"ok": True})',
    },
    "write_jsonl": {
        "name": "write_jsonl",
        "signature": "write_jsonl(path: str, rows: list[Any]) -> int",
        "effects": ["write_file", "create_parent_dirs"],
        "example": 'write_jsonl("out/rows.jsonl", rows)',
    },
    "write_text": {
        "name": "write_text",
        "signature": "write_text(path: str, text: str, append: bool = False) -> record",
        "effects": ["write_file", "create_parent_dirs"],
        "example": 'write_text("out.txt", "hello\\n")',
    },
    "json_loads": {
        "name": "json_loads",
        "signature": "json_loads(text: str) -> Any",
        "effects": [],
        "example": 'value = json_loads("{\\"ok\\": true}")',
    },
    "json_dumps": {
        "name": "json_dumps",
        "signature": "json_dumps(value: Any) -> str",
        "effects": [],
        "example": 'text = json_dumps({"ok": True})',
    },
    "run": {
        "name": "run",
        "signature": 'run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, background: bool = False, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576) -> record',
        "effects": ["process", "unknown"],
        "example": 'job = run(["long_running_command", "arg1", "arg2"], cwd="/app", background=True); while job.still_running: status = run_status(job.run_id); job = run_wait(job.run_id, timeout_ms=30000)',
        "use_when": "Use for POSIX programs. For task commands that may run more than a few seconds but should eventually exit, pass background=True and manage the returned run_id with run_status/run_wait/run_terminate.",
        "avoid": [
            "Do not pass shell strings; pass argv lists.",
            "Use background=True for long-running task commands that should eventually exit, such as builds, tests, installs, downloads, benchmarks, or data processing.",
            "Do not use shell backgrounding, nohup, or `&`; use background=True for long task commands, or start_daemon() for servers/services that must stay running while tests execute.",
        ],
        "call_form": "stone_call",
        "args": [
            {"name": "argv", "type": "list[str]", "required": True, "example": ["cargo", "test"]},
            {"name": "cwd", "type": "str?", "required": False, "example": "."},
            {"name": "stdin", "type": "str?", "required": False},
            {"name": "timeout_ms", "type": "int?", "required": False, "example": 5000},
            {"name": "env", "type": "record?", "required": False},
            {"name": "background", "type": "bool", "required": False, "example": True},
            {"name": "stdout", "type": "str", "required": False, "example": "capture"},
            {"name": "stderr", "type": "str", "required": False, "example": "capture"},
            {"name": "max_stdout_bytes", "type": "int", "required": False, "example": 1048576},
            {"name": "max_stderr_bytes", "type": "int", "required": False, "example": 1048576},
        ],
        "returns": {
            "type": "RunResult",
            "fields": [
                "argv",
                "cwd",
                "stdout",
                "stderr",
                "stdout_tail",
                "stderr_tail",
                "exit_code",
                "ok",
                "timed_out",
                "still_running",
                "done",
                "next_action",
                "suggested_actions",
                "partial_output_hint",
                "run_id",
                "background",
                "duration_ms",
                "explanation",
            ],
        },
        "example_call": {"name": "run", "args": [["cargo", "test"]]},
        "also_accepts": [
            {"name": "run", "args": {"argv": ["cargo", "test"]}},
            {"name": "run", "args": ["cargo", "test"]},
        ],
    },
    "run_wait": {
        "name": "run_wait",
        "signature": "run_wait(run_id: str, timeout_ms: int = 30000) -> record",
        "effects": ["process", "unknown"],
        "example": 'while "still_running" in result and result.still_running: status = run_status(result.run_id); result = run_wait(result.run_id, timeout_ms=30000)',
    },
    "run_status": {
        "name": "run_status",
        "signature": "run_status(run_id: str) -> record",
        "effects": ["process"],
        "example": 'if "still_running" in result and result.still_running: status = run_status(result.run_id)',
    },
    "run_terminate": {
        "name": "run_terminate",
        "signature": "run_terminate(run_id: str) -> record",
        "effects": ["process", "unknown"],
        "example": 'if "still_running" in result and result.still_running: stopped = run_terminate(result.run_id)',
    },
    "must_run": {
        "name": "must_run",
        "signature": 'must_run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576) -> record',
        "effects": ["process", "unknown"],
        "example": 'result = must_run(["printf", "ok"], timeout_ms=5000)',
        "call_form": "stone_call",
        "args": [
            {"name": "argv", "type": "list[str]", "required": True, "example": ["printf", "ok"]},
            {"name": "cwd", "type": "str?", "required": False, "example": "."},
            {"name": "stdin", "type": "str?", "required": False},
            {"name": "timeout_ms", "type": "int?", "required": False, "example": 120000},
            {"name": "env", "type": "record?", "required": False},
            {"name": "stdout", "type": "str", "required": False, "example": "capture"},
            {"name": "stderr", "type": "str", "required": False, "example": "capture"},
            {"name": "max_stdout_bytes", "type": "int", "required": False, "example": 1048576},
            {"name": "max_stderr_bytes", "type": "int", "required": False, "example": 1048576},
        ],
        "returns": {
            "type": "RunResult",
            "fields": [
                "argv",
                "cwd",
                "stdout",
                "stderr",
                "stdout_tail",
                "stderr_tail",
                "exit_code",
                "ok",
                "timed_out",
                "suggested_actions",
                "partial_output_hint",
                "duration_ms",
                "explanation",
            ],
        },
        "example_call": {"name": "must_run", "args": [["printf", "ok"]]},
        "also_accepts": [
            {"name": "must_run", "args": {"argv": ["printf", "ok"]}},
            {"name": "must_run", "args": ["printf", "ok"]},
        ],
    },
    "resolve_command": {
        "name": "resolve_command",
        "signature": "resolve_command(name: str) -> record",
        "effects": ["read_env", "read_file"],
        "example": 'info = resolve_command("python3")',
    },
    "state": {
        "name": "state",
        "signature": "state() -> record",
        "effects": ["read_env", "read_file", "process"],
        "example": "snapshot = state()",
    },
    "current_program": {
        "name": "current_program",
        "signature": "current_program(entrypoint: str = current_entrypoint) -> record",
        "effects": [],
        "example": 'worker_program = current_program(entrypoint="worker")',
    },
    "attempt_info": {
        "name": "attempt_info",
        "signature": "attempt_info(attempt: str = '') -> record",
        "effects": ["read_env"],
        "example": "me = attempt_info()",
    },
    "attempt_state": {
        "name": "attempt_state",
        "signature": "attempt_state(attempt: str = '', sample_limit: int = 100) -> record",
        "effects": ["read_env", "read_file"],
        "example": "state = attempt_state(sample_limit=50)",
    },
    "attempt_logs": {
        "name": "attempt_logs",
        "signature": "attempt_logs(attempt: str = '', stream: str = 'both', tail: int = 0, max_bytes: int = 65536) -> record",
        "effects": ["read_env"],
        "example": 'logs = attempt_logs(child.attempt, stream="stderr", tail=20)',
    },
    "attempts": {
        "name": "attempts",
        "signature": "attempts(task: str = '', workspace: str = '', state: str = '') -> list[record]",
        "effects": ["read_env"],
        "example": 'active = attempts(state="active")',
    },
    "attempt_list": {
        "name": "attempt_list",
        "signature": "attempt_list(task: str = '', workspace: str = '', state: str = '') -> list[record]",
        "effects": ["read_env"],
        "example": 'active = attempt_list(state="active")',
        "alias_for": "attempts",
    },
    "attempt_spawn": {
        "name": "attempt_spawn",
        "signature": "attempt_spawn(task: str = '', workspace: str = '', task_spec: record = {}, task_input: Any = None, program: record = {}, entrypoint: str = '', workspace_source: record = {}, context_source: record = {}, capabilities: record = {}, start: bool = false, scope: attempt_scope? = None, controller: str = '', capability_profile: str = '', container: str = '', workspace_mount: str = '', parent_attempt: str = '', resource_limits: record = {}, metadata: record = {}) -> attempt_handle",
        "effects": ["write_file"],
        "example": 'child = attempt_spawn(task_spec={"id": "task-debug", "objective": "write hello.txt"}, workspace_source={"workspace": "repo"}, program={"kind": "stone", "source": "write_file(\\"hello.txt\\", \\"hello\\")"})',
    },
    "attempt_start": {
        "name": "attempt_start",
        "signature": "attempt_start(attempt: str = '', wait: bool = false, timeout_ms: int = 0) -> record",
        "effects": ["write_file", "process"],
        "example": "started = attempt_start(child.attempt)",
    },
    "attempt_wait": {
        "name": "attempt_wait",
        "signature": "attempt_wait(attempt: str = '', timeout_ms: int = 0) -> record",
        "effects": ["read_env", "process"],
        "example": "done = attempt_wait(child.attempt, timeout_ms=30000)",
    },
    "attempt_join": {
        "name": "attempt_join",
        "signature": "attempt_join(attempt: attempt_handle | str | record, timeout_ms: int? = None) -> attempt_outcome",
        "effects": ["read_env", "process"],
        "example": "outcome = attempt_join(child, timeout_ms=30000)",
    },
    "attempt_wait_any": {
        "name": "attempt_wait_any",
        "signature": "attempt_wait_any(children: attempt_scope | list[attempt_handle | str | record], timeout_ms: int? = None) -> attempt_outcome",
        "effects": ["read_env", "process"],
        "example": "first = attempt_wait_any(scope, timeout_ms=30000)",
    },
    "attempt_wait_all": {
        "name": "attempt_wait_all",
        "signature": "attempt_wait_all(children: attempt_scope | list[attempt_handle | str | record], timeout_ms: int? = None) -> record",
        "effects": ["read_env", "process"],
        "example": "batch = attempt_wait_all(scope, timeout_ms=30000)",
    },
    "attempt_terminate": {
        "name": "attempt_terminate",
        "signature": "attempt_terminate(attempt: str | record) -> record",
        "effects": ["process"],
        "example": "attempt_terminate(child.attempt)",
    },
    "attempt_scope": {
        "name": "attempt_scope",
        "signature": "attempt_scope(exit_policy: 'cancel_then_join' = 'cancel_then_join', join_timeout_ms: int = 5000) -> attempt_scope",
        "effects": ["process", "write_file", "remove_file"],
        "example": "scope = attempt_scope(join_timeout_ms=5000)",
    },
    "attempt_scope_add": {
        "name": "attempt_scope_add",
        "signature": "attempt_scope_add(scope: attempt_scope, child: str | record) -> attempt_scope",
        "effects": [],
        "example": "scope = attempt_scope_add(scope, child)",
    },
    "attempt_scope_close": {
        "name": "attempt_scope_close",
        "signature": "attempt_scope_close(scope: attempt_scope, reason: str = 'attempt scope closed') -> record",
        "effects": ["process", "write_file", "remove_file"],
        "example": "cleanup = attempt_scope_close(scope)",
    },
    "attempt_fork": {
        "name": "attempt_fork",
        "signature": "attempt_fork(parent_attempt: attempt_handle | str | record = '', task: str = '', program: record? = None, entrypoint: str = '', start: bool = False, scope: attempt_scope? = None, controller: str = '', capability_profile: str = '', container: str = '', workspace_mount: str = '', resource_limits: record = {}, metadata: record = {}) -> attempt_handle",
        "effects": ["write_file"],
        "example": 'branch = attempt_fork(task="try-alt-fix", controller="codex")',
    },
    "attempt_finish": {
        "name": "attempt_finish",
        "signature": "attempt_finish(action: str, attempt: str = '', message: str = '', reason: str = '', allow_risky: bool = False) -> record",
        "effects": ["write_file", "remove_file"],
        "example": 'attempt_finish("rollback", reason="debug branch done")',
    },
    "attempt_run_process": {
        "name": "attempt_run_process",
        "signature": "attempt_run_process(attempt: str = '', argv: list[str], env: record = {}) -> record",
        "effects": ["process", "write_file", "unknown"],
        "example": 'run = attempt_run_process(child.attempt, ["/path/to/controller"], env={"HELIX_ROOT": "/path/to/helix"})',
    },
    "env_state": {
        "name": "env_state",
        "signature": "env_state(sample_limit: int = 100) -> record",
        "effects": ["read_env", "read_file"],
        "example": "changes = env_state(sample_limit=50)",
    },
    "env_diff": {
        "name": "env_diff",
        "signature": "env_diff(sample_limit: int = 100) -> record",
        "effects": ["read_env", "read_file"],
        "example": "changes = env_diff(sample_limit=50)",
        "alias_for": "env_state",
    },
    "env_tx_info": {
        "name": "env_tx_info",
        "signature": "env_tx_info(tx: str = '') -> record",
        "effects": ["read_env"],
        "example": "info = env_tx_info()",
    },
    "env_txs": {
        "name": "env_txs",
        "signature": "env_txs(workspace: str = '', purpose: str = '') -> list[record]",
        "effects": ["read_env"],
        "example": 'debug_branches = env_txs(purpose="checkpoint-run")',
    },
    "env_finish": {
        "name": "env_finish",
        "signature": "env_finish() -> record",
        "effects": ["read_env", "read_file"],
        "example": "finish = env_finish()",
    },
    "env_restore": {
        "name": "env_restore",
        "signature": "env_restore(paths: list[str] | str = []) -> record",
        "effects": ["write_file", "remove_file"],
        "example": 'env_restore(["tmp.txt", "build.log"])',
    },
    "env_checkpoint": {
        "name": "env_checkpoint",
        "signature": "env_checkpoint(reason: str = '') -> record",
        "effects": ["read_file", "write_file"],
        "example": 'cp = env_checkpoint(reason="before verifier attempt")',
    },
    "env_fork": {
        "name": "env_fork",
        "signature": "env_fork(checkpoint: str) -> record",
        "effects": ["write_file"],
        "example": "branch = env_fork(cp.checkpoint)",
    },
    "env_restore_checkpoint": {
        "name": "env_restore_checkpoint",
        "signature": "env_restore_checkpoint(checkpoint: str) -> record",
        "effects": ["write_file", "remove_file", "process"],
        "example": "env_restore_checkpoint(cp.checkpoint)",
    },
    "env_checkpoints": {
        "name": "env_checkpoints",
        "signature": "env_checkpoints(workspace: str = '', include_discarded: bool = False) -> list[record]",
        "effects": ["read_env", "read_file"],
        "example": "checkpoints = env_checkpoints()",
    },
    "env_checkpoint_gc": {
        "name": "env_checkpoint_gc",
        "signature": "env_checkpoint_gc(apply: bool = False) -> record",
        "effects": ["read_env", "read_file", "remove_file"],
        "example": "gc = env_checkpoint_gc()",
    },
    "env_discard_checkpoint": {
        "name": "env_discard_checkpoint",
        "signature": "env_discard_checkpoint(checkpoint: str, force: bool = False) -> record",
        "effects": ["remove_file"],
        "example": "env_discard_checkpoint(cp.checkpoint)",
    },
    "env_run_checkpoint": {
        "name": "env_run_checkpoint",
        "signature": "env_run_checkpoint(checkpoint: str, image: str, argv: list[str], workspace_mount: str = '/app', workdir: str = '/app', timeout_ms: int = 300000, env: record? = None, stdin: str = '', user: str = '', keep_tx: bool = False) -> record",
        "effects": ["process", "read_file", "write_file", "remove_file"],
        "example": 'result = env_run_checkpoint(cp.checkpoint, "python:3.12-slim", ["python", "-c", "print(\'ok\')"])',
    },
    "env_commit": {
        "name": "env_commit",
        "signature": "env_commit(message: str = 'agent commit', allow_risky: bool = False) -> record",
        "effects": ["write_file"],
        "example": 'env_commit(message="solve task")',
    },
    "env_rollback": {
        "name": "env_rollback",
        "signature": "env_rollback() -> record",
        "effects": ["write_file", "remove_file"],
        "example": "env_rollback()",
    },
    "last_result": {
        "name": "last_result",
        "signature": "last_result() -> record | None",
        "effects": ["read_env"],
        "example": "previous = last_result()",
    },
    "last": {
        "name": "last",
        "signature": "last(values: list, count: int? = None) -> Any | list",
        "effects": [],
        "example": "tail_sample = last(rows, 5)",
    },
    "start_daemon": {
        "name": "start_daemon",
        "signature": "start_daemon(argv: list[str], cwd: str? = None, env: record? = None, stdout: str? = None, stderr: str? = None) -> record",
        "effects": ["process", "unknown"],
        "example": 'daemon = start_daemon(["python3", "-m", "http.server", "8888"], cwd="/app", stderr="server.err")',
    },
    "daemon_status": {
        "name": "daemon_status",
        "signature": 'daemon_status(daemon: record | int, port: int? = None, host: str = "127.0.0.1", log: str? = None, max_log_bytes: int = 4000) -> record',
        "effects": ["process", "read_file"],
        "example": 'status = daemon_status(daemon, port=8888, log="server.err")',
    },
    "stop_daemon": {
        "name": "stop_daemon",
        "signature": "stop_daemon(daemon: record | int, timeout_ms: int = 5000) -> record",
        "effects": ["process"],
        "example": "stop = stop_daemon(daemon)",
    },
    "wait_port": {
        "name": "wait_port",
        "signature": 'wait_port(port: int, host: str = "127.0.0.1", timeout_ms: int = 30000, protocol: str = "tcp") -> record',
        "effects": ["network"],
        "example": 'ready = wait_port(8888, protocol="tcp", timeout_ms=30000)',
    },
    "wait_for": {
        "name": "wait_for",
        "signature": "wait_for(predicate: lambda, timeout_ms: int = 30000, interval_ms: int = 100, ignore_errors: bool = False) -> record",
        "effects": ["read_file", "process", "network"],
        "example": 'ready = wait_for(lambda: read_file("server.log").find("READY") >= 0, timeout_ms=30000, ignore_errors=True)',
        "call_form": "stone_eval",
        "args": [
            {
                "name": "source",
                "type": "Stone source",
                "required": True,
                "example": 'ready = wait_for(lambda: True, timeout_ms=1000)',
            }
        ],
        "returns": {
            "type": "WaitForResult",
            "fields": ["ok", "kind", "attempts", "duration_ms", "value", "error"],
        },
        "example_call": {
            "tool": "stone_eval",
            "source": 'ready = wait_for(lambda: read_file("server.log").find("READY") >= 0, timeout_ms=30000, ignore_errors=True)\nemit(ready)',
        },
    },
    "ps": {
        "name": "ps",
        "signature": "ps(interval_ms: int = 0) -> list[record]",
        "effects": ["process"],
        "example": "procs = ps()",
        "args": [
            {"name": "interval_ms", "type": "int", "required": False, "example": 0},
        ],
        "returns": {
            "type": "list[ProcessRecord]",
            "fields": [
                "pid",
                "ppid",
                "name",
                "command",
                "status",
                "cwd",
                "cpu_percent",
                "memory_bytes",
                "virtual_bytes",
                "owner_uid",
                "owner_kind",
                "owner_id",
                "listen_addrs",
                "open_files",
            ],
        },
    },
    "sysinfo": {
        "name": "sysinfo",
        "signature": 'sysinfo(section: "os" | "cpu" | "cpu_long" | "mem" | "disks" | "net" | "temp" | "users" | "all" = "all") -> record | list',
        "effects": ["read_env", "read_file", "process", "network"],
        "example": 'host = sysinfo("os")',
        "args": [
            {"name": "section", "type": "str", "required": False, "example": "os"},
        ],
        "returns": {"type": "record | list"},
    },
    "stat": {
        "name": "stat",
        "signature": "stat(path: str, follow_symlinks: bool = False) -> record",
        "effects": ["read_file"],
        "example": 'info = stat("results.txt")',
    },
    "search": {
        "name": "search",
        "signature": "search(root: str, needle: str, regex: bool = False) -> list[record]",
        "effects": ["read_dir", "read_file"],
        "example": 'matches = search(path=".", query="needle")',
    },
    "tail": {
        "name": "tail",
        "signature": "tail(values: list, count: int? = None) -> Any | list",
        "effects": [],
        "example": "tail_sample = tail(rows, 5)",
        "alias_for": "last",
    },
    "rm": {
        "name": "rm",
        "signature": "rm(path: str | list[str], force: bool = False, recursive: bool = True) -> None",
        "effects": ["remove_file"],
        "example": 'rm(["tmp.txt", "out"], force=True)',
    },
}

Stone_CALL_ARG_ORDER: dict[str, tuple[str, ...]] = {
    "cat": ("path",),
    "edit": ("path", "old", "new", "all"),
    "edit_file": ("path", "old", "new", "all"),
    "find": ("root", "name_glob"),
    "first": ("values", "count"),
    "head": ("values", "count"),
    "json_dumps": ("value",),
    "json_loads": ("text",),
    "last": ("values", "count"),
    "list": ("path",),
    "list_dir": ("path",),
    "ls": ("path",),
    "mkdir": ("path",),
    "must_run": (
        "argv",
        "cwd",
        "stdin",
        "timeout_ms",
        "env",
        "stdout",
        "stderr",
        "max_stdout_bytes",
        "max_stderr_bytes",
    ),
    "read_csv": ("path", "limit"),
    "read_file": ("path", "max_bytes"),
    "read_json": ("path",),
    "read_jsonl": ("path", "limit"),
    "read_text": ("path", "max_bytes"),
    "run": (
        "argv",
        "cwd",
        "stdin",
        "timeout_ms",
        "env",
        "stdout",
        "stderr",
        "max_stdout_bytes",
        "max_stderr_bytes",
    ),
    "run_wait": ("run_id", "timeout_ms"),
    "run_status": ("run_id",),
    "run_terminate": ("run_id",),
    "resolve_command": ("name",),
    "state": (),
    "current_program": ("entrypoint",),
    "attempt_info": ("attempt",),
    "attempt_state": ("attempt", "sample_limit"),
    "attempt_logs": ("attempt", "stream", "tail", "max_bytes"),
    "attempts": ("task", "workspace", "state"),
    "attempt_list": ("task", "workspace", "state"),
    "attempt_spawn": (
        "task",
        "workspace",
        "task_spec",
        "task_input",
        "program",
        "entrypoint",
        "workspace_source",
        "context_source",
        "capabilities",
        "start",
        "scope",
        "controller",
        "capability_profile",
        "container",
        "workspace_mount",
        "parent_attempt",
        "resource_limits",
        "metadata",
    ),
    "attempt_start": ("attempt", "wait", "timeout_ms"),
    "attempt_wait": ("attempt", "timeout_ms"),
    "attempt_join": ("attempt", "timeout_ms"),
    "attempt_wait_any": ("children", "timeout_ms"),
    "attempt_wait_all": ("children", "timeout_ms"),
    "attempt_terminate": ("attempt",),
    "attempt_scope": ("exit_policy", "join_timeout_ms"),
    "attempt_scope_add": ("scope", "child"),
    "attempt_scope_close": ("scope", "reason"),
    "attempt_fork": (
        "parent_attempt",
        "task",
        "program",
        "entrypoint",
        "start",
        "scope",
        "controller",
        "capability_profile",
        "container",
        "workspace_mount",
        "resource_limits",
        "metadata",
    ),
    "attempt_finish": ("action", "attempt", "message", "reason", "allow_risky"),
    "attempt_run_process": ("attempt", "argv", "env"),
    "env_state": ("sample_limit",),
    "env_diff": ("sample_limit",),
    "env_tx_info": ("tx",),
    "env_txs": ("workspace", "purpose"),
    "env_finish": (),
    "env_restore": ("paths",),
    "env_checkpoint": ("reason",),
    "env_fork": ("checkpoint",),
    "env_restore_checkpoint": ("checkpoint",),
    "env_checkpoints": ("workspace", "include_discarded"),
    "env_checkpoint_gc": ("apply",),
    "env_discard_checkpoint": ("checkpoint", "force"),
    "env_run_checkpoint": (
        "checkpoint",
        "image",
        "argv",
        "workspace_mount",
        "workdir",
        "timeout_ms",
        "env",
        "stdin",
        "user",
        "keep_tx",
    ),
    "env_commit": ("message", "allow_risky"),
    "env_rollback": (),
    "last_result": (),
    "rm": ("path",),
    "start_daemon": ("argv", "cwd", "env", "stdout", "stderr"),
    "daemon_status": ("daemon", "port", "host", "log", "max_log_bytes"),
    "stop_daemon": ("daemon", "timeout_ms"),
    "wait_port": ("port", "host", "timeout_ms", "protocol"),
    "ps": ("interval_ms",),
    "sysinfo": ("section",),
    "search": ("root", "needle"),
    "stat": ("path", "follow_symlinks"),
    "tail": ("values", "count"),
    "write_file": ("path", "text", "append"),
    "write_json": ("path", "value"),
    "write_jsonl": ("path", "rows"),
    "write_text": ("path", "text", "append"),
}

Stone_CALL_ALIASES: dict[str, str] = {
    "attempt_list": "attempts",
    "delete_dir": "rm",
    "delete_directory": "rm",
    "delete_file": "rm",
    "edit_file": "edit",
    "stone_help": "help",
    "process_list": "ps",
    "list": "ls",
    "list_builtins": "help",
    "remove_dir": "rm",
    "remove_directory": "rm",
    "read": "read_file",
    "remove_file": "rm",
    "sys": "sysinfo",
    "sys_info": "sysinfo",
    "write": "write_file",
}

Stone_ONE_POSITIONAL_THEN_KEYWORDS = {
    "env_checkpoint",
    "env_checkpoint_gc",
    "env_checkpoints",
    "env_commit",
    "env_discard_checkpoint",
    "env_fork",
    "env_run_checkpoint",
    "env_restore_checkpoint",
    "must_run",
    "run",
    "run_status",
    "run_wait",
    "start_daemon",
    "daemon_status",
    "stop_daemon",
    "wait_port",
}
Stone_TWO_POSITIONAL_THEN_KEYWORDS = {
    "attempt_finish",
}


class StoneBackend(abc.ABC):
    @abc.abstractmethod
    def eval(self, source: str, cwd: str | None = None) -> dict[str, Any]:
        raise NotImplementedError


class TraceRecorder:
    def __init__(self, path: str | None = None) -> None:
        self.path = Path(path).expanduser() if path else None
        self.next_seq = 0
        if self.path is not None:
            self.path.parent.mkdir(parents=True, exist_ok=True)

    def record_tool_call(
        self, name: str, args: dict[str, Any], result: dict[str, Any], duration_ms: int
    ) -> None:
        if self.path is None:
            return
        self.next_seq += 1
        record = sparse(
            {
                "seq": self.next_seq,
                "ts_ms": int(time.time() * 1000),
                "tool": name,
                "ok": result.get("ok"),
                "duration_ms": duration_ms,
                "cwd": args.get("cwd") if isinstance(args, dict) else None,
                "error": compact_error(result.get("error")),
                "effects": result.get("effects"),
                "gap": result.get("gap"),
                "escape": escape_trace(args, result) if name == "escape_linux" else None,
                "stone": stone_trace(args, result) if name in {"stone_eval", "stone_call"} else None,
            }
        )
        with self.path.open("a", encoding="utf-8") as file:
            file.write(json.dumps(record, separators=(",", ":"), sort_keys=True))
            file.write("\n")


def compact_error(error: Any) -> dict[str, Any] | None:
    if not isinstance(error, dict):
        return None
    return sparse(
        {
            "kind": error.get("kind"),
            "code": error.get("code"),
            "message": bound_text(error.get("message"), 512),
            "detail": bound_text(error.get("detail"), 512),
        }
    )


def escape_trace(args: dict[str, Any], result: dict[str, Any]) -> dict[str, Any]:
    return sparse(
        {
            "reason": bound_text(args.get("reason"), 512),
            "command": bound_text(args.get("command"), 1024),
            "gap": result.get("gap"),
        }
    )


def stone_trace(args: dict[str, Any], result: dict[str, Any]) -> dict[str, Any]:
    diagnostics = result.get("diagnostics") if isinstance(result.get("diagnostics"), dict) else {}
    trace: dict[str, Any] = {
        "backend": diagnostics.get("backend"),
        "hot_loop": diagnostics.get("hot_loop"),
    }
    if "source" in args:
        trace["source_preview"] = bound_text(args.get("source"), 512)
    if "name" in args:
        trace["call"] = args.get("name")
        if args.get("name") == "run":
            trace["run"] = stone_run_trace(args, result)
    return sparse(trace)


def stone_run_trace(args: dict[str, Any], result: dict[str, Any]) -> dict[str, Any] | None:
    call_args = args.get("args")
    if isinstance(call_args, str):
        try:
            call_args = json.loads(call_args)
        except json.JSONDecodeError:
            call_args = None
    value = result.get("value") if isinstance(result.get("value"), dict) else {}
    trace: dict[str, Any] = {}
    argv = None
    if isinstance(call_args, dict):
        argv = call_args.get("argv")
    elif isinstance(call_args, list) and call_args:
        argv = call_args[0]
    if argv is not None:
        trace["argv"] = argv
    if isinstance(value, dict):
        control = value.get("control") if isinstance(value.get("control"), dict) else {}
        run_id = value.get("run_id", control.get("run_id"))
        still_running = value.get("still_running", control.get("still_running"))
        trace.update(
            {
                "ok": value.get("ok", control.get("ok")),
                "kind": value.get("kind", control.get("kind")),
                "exit_code": value.get("exit_code", control.get("exit_code")),
                "duration_ms": value.get("duration_ms", control.get("duration_ms")),
                "stdout": bound_text(value.get("stdout"), 2048),
                "stderr": bound_text(value.get("stderr"), 2048),
                "timed_out": value.get("timed_out", control.get("timed_out")),
                "still_running": still_running,
                "done": value.get("done", control.get("done")),
                "next_action": value.get("next_action", control.get("next_action")),
                "run_id": run_id,
                "runtime": compact_runtime_context(value.get("runtime")),
                "explanation": compact_run_explanation(value.get("explanation")),
                "helpers": compact_helper_observations(value.get("helpers")),
            }
        )
    return sparse(trace)


def compact_runtime_context(runtime: Any) -> dict[str, Any] | None:
    if not isinstance(runtime, dict):
        return None
    markers = runtime.get("cwd_project_markers")
    if isinstance(markers, list):
        compact_markers = [item for item in markers[:8] if isinstance(item, str)]
    else:
        compact_markers = None
    return sparse(
        {
            "kind": runtime.get("kind"),
            "command_name": runtime.get("command_name"),
            "resolved_executable": runtime.get("resolved_executable"),
            "python_executable": runtime.get("python_executable"),
            "python_version": runtime.get("python_version"),
            "sys_prefix": runtime.get("sys_prefix"),
            "sys_base_prefix": runtime.get("sys_base_prefix"),
            "pip_available": runtime.get("pip_available"),
            "env_virtual_env": runtime.get("env_virtual_env"),
            "uv_project_environment": runtime.get("uv_project_environment"),
            "cwd_project_markers": compact_markers,
            "note": bound_text(runtime.get("note"), 300),
            "python_probe_error": bound_text(runtime.get("python_probe_error"), 300),
        }
    )


def compact_run_explanation(explanation: Any) -> dict[str, Any] | None:
    if not isinstance(explanation, dict):
        return None
    next_steps = explanation.get("next_steps")
    if isinstance(next_steps, list):
        compact_next_steps = [
            bound_text(step, 300)
            for step in next_steps[:5]
            if isinstance(step, str) and step
        ]
    else:
        compact_next_steps = None
    return sparse(
        {
            "kind": explanation.get("kind"),
            "summary": bound_text(explanation.get("summary"), 512),
            "scope": explanation.get("scope"),
            "timeout_ms": explanation.get("timeout_ms"),
            "duration_ms": explanation.get("duration_ms"),
            "argv": explanation.get("argv"),
            "command": bound_text(explanation.get("command"), 512),
            "module": explanation.get("module"),
            "attribute": explanation.get("attribute"),
            "package": explanation.get("package"),
            "dependent": explanation.get("dependent"),
            "requirement": explanation.get("requirement"),
            "installed": explanation.get("installed"),
            "requested": explanation.get("requested"),
            "evidence": bound_text(explanation.get("evidence"), 512),
            "inspect_argv": explanation.get("inspect_argv"),
            "next_steps": compact_next_steps,
        }
    )


def compact_helper_observations(helpers: Any) -> list[dict[str, Any]] | None:
    if not isinstance(helpers, list):
        return None
    compact: list[dict[str, Any]] = []
    for helper in helpers[:8]:
        if not isinstance(helper, dict):
            continue
        next_checks = helper.get("next_checks")
        if isinstance(next_checks, list):
            compact_next_checks = [
                check for check in next_checks[:4] if isinstance(check, list)
            ]
        else:
            compact_next_checks = None
        evidence = helper.get("evidence")
        if isinstance(evidence, dict):
            compact_evidence = {
                key: bound_text(value, 512) if isinstance(value, str) else value
                for key, value in evidence.items()
                if value not in (None, "", [], {})
            }
        else:
            compact_evidence = None
        compact.append(
            sparse(
                {
                    "helper": helper.get("helper"),
                    "event": helper.get("event"),
                    "family": helper.get("family"),
                    "kind": helper.get("kind"),
                    "summary": bound_text(helper.get("summary"), 512),
                    "source": helper.get("source"),
                    "evidence": compact_evidence,
                    "next_checks": compact_next_checks,
                }
            )
        )
    return compact or None


def stone_transport_timeout_error(
    timeout_seconds: float,
    duration_ms: int,
    backend: str,
    stdout: Any = "",
    stderr: Any = "",
    max_output_bytes: int = MAX_OUTPUT_BYTES,
) -> dict[str, Any]:
    timeout_ms = int(timeout_seconds * 1000)
    return sparse(
        {
            "ok": False,
            "error": {
                "kind": "timeout",
                "code": "stone_timeout",
                "message": (
                    f"Stone transport timed out after {timeout_seconds:g}s while waiting for "
                    "waymark to return a result."
                ),
                "detail": (
                    "This is the MCP/host timeout for one Stone call, not necessarily the "
                    "inner command's own timeout. The guest process was killed, so a "
                    "long-running command such as git clone may have left partial files behind."
                ),
                "timeout_seconds": timeout_seconds,
                "timeout_ms": timeout_ms,
                "next_steps": [
                    "Inspect the working directory before retrying; the interrupted command may have left a partial checkout or partial output files.",
                    "If the operation is expected to take longer, rerun with a larger Stone MCP timeout or pass a larger timeout_ms to run() when using the run builtin.",
                    "For large downloads or clones, prefer a narrow command, shallow clone, or resumable follow-up command when the task allows it.",
                ],
            },
            "stdout": bound_text(stdout or "", max_output_bytes),
            "stderr": bound_text(stderr or "", max_output_bytes),
            "diagnostics": {
                "duration_ms": duration_ms,
                "backend": backend,
                "timeout_seconds": timeout_seconds,
            },
        }
    )


class SubprocessBackend(StoneBackend):
    def __init__(
        self,
        waymark_bin: str | None = None,
        timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
        max_output_bytes: int = MAX_OUTPUT_BYTES,
    ) -> None:
        self.waymark_bin = waymark_bin or resolve_waymark_bin()
        self.timeout_seconds = timeout_seconds
        self.max_output_bytes = max_output_bytes

    def eval(self, source: str, cwd: str | None = None) -> dict[str, Any]:
        run_cwd = str(resolve_cwd(cwd))
        start = time.monotonic()
        try:
            proc = subprocess.run(
                [self.waymark_bin, "--stone", "--stdin-script"],
                input=source,
                cwd=run_cwd,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=self.timeout_seconds,
                env={
                    **os.environ,
                    "RUST_BACKTRACE": os.environ.get("RUST_BACKTRACE", "0"),
                    "WAYMARK_START_DIR": run_cwd,
                },
            )
        except subprocess.TimeoutExpired as err:
            return stone_transport_timeout_error(
                self.timeout_seconds,
                int((time.monotonic() - start) * 1000),
                "subprocess",
                err.stdout,
                err.stderr,
                self.max_output_bytes,
            )
        except OSError as err:
            return {
                "ok": False,
                "error": {
                    "kind": "backend_error",
                    "code": "stone_backend_spawn_failed",
                    "message": str(err),
                },
            }

        duration_ms = int((time.monotonic() - start) * 1000)
        return normalize_stone_process_result(proc, self.max_output_bytes, duration_ms)


class WarmStdioBackend(StoneBackend):
    def __init__(
        self,
        waymark_bin: str | None = None,
        cwd: str | None = None,
        timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
        max_output_bytes: int = MAX_OUTPUT_BYTES,
    ) -> None:
        self.waymark_bin = waymark_bin or resolve_waymark_bin()
        self.cwd = resolve_cwd(cwd)
        self.timeout_seconds = timeout_seconds
        self.max_output_bytes = max_output_bytes
        self.proc: subprocess.Popen[bytes] | None = None
        self.next_id = 0

    def eval(self, source: str, cwd: str | None = None) -> dict[str, Any]:
        requested_cwd = resolve_cwd(cwd) if cwd else self.cwd
        if requested_cwd != self.cwd:
            return {
                "ok": False,
                "error": {
                    "kind": "backend_error",
                    "code": "stone_warm_cwd_fixed",
                    "message": "warm Stone backend uses one fixed cwd per server process",
                    "hint": f"start the MCP server from {requested_cwd} or omit cwd",
                },
            }

        started = time.monotonic()
        try:
            self.ensure_started()
            task_id = self.next_task_id()
            write_frame(
                self.proc_stdin(),
                {
                    "version": 0,
                    "type": "task",
                    "id": task_id,
                    "payload": {
                        "encoding": "json-inline",
                        "task": {
                            "version": 0,
                            "id": task_id,
                            "runtime": {"guest": "waymark", "frontend": "stone"},
                            "script": {"source": source},
                        },
                    },
                },
            )
            frame = read_frame(self.proc_stdout(), self.timeout_seconds)
        except TimeoutError:
            self.close(kill=True)
            return stone_transport_timeout_error(
                self.timeout_seconds,
                int((time.monotonic() - started) * 1000),
                "warm_stdio",
            )
        except (OSError, EOFError, RuntimeError) as err:
            self.close(kill=True)
            return {
                "ok": False,
                "error": {
                    "kind": "backend_error",
                    "code": "stone_warm_stdio_failed",
                    "message": str(err),
                },
            }

        duration_ms = int((time.monotonic() - started) * 1000)
        return normalize_task_frame_result(frame, self.max_output_bytes, duration_ms)

    def ensure_started(self) -> None:
        if self.proc is not None and self.proc.poll() is None:
            return
        env = {
            **os.environ,
            "RUST_BACKTRACE": os.environ.get("RUST_BACKTRACE", "0"),
            "WAYMARK_START_DIR": str(self.cwd),
        }
        self.proc = subprocess.Popen(
            [self.waymark_bin, "--task-server-stream"],
            cwd=str(self.cwd),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env=env,
        )

    def next_task_id(self) -> str:
        self.next_id += 1
        return f"stone-mcp-{self.next_id}"

    def proc_stdin(self) -> BinaryIO:
        if self.proc is None or self.proc.stdin is None:
            raise RuntimeError("waymark stdin is unavailable")
        return self.proc.stdin

    def proc_stdout(self) -> BinaryIO:
        if self.proc is None or self.proc.stdout is None:
            raise RuntimeError("waymark stdout is unavailable")
        return self.proc.stdout

    def close(self, kill: bool = False) -> None:
        proc = self.proc
        self.proc = None
        if proc is None:
            return
        try:
            if kill:
                proc.kill()
                proc.wait(timeout=5)
                return
            if proc.poll() is None and proc.stdin is not None:
                try:
                    write_frame(proc.stdin, {"version": 0, "type": "shutdown"})
                except OSError:
                    pass
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
        finally:
            for pipe in (proc.stdin, proc.stdout):
                if pipe is not None:
                    try:
                        pipe.close()
                    except OSError:
                        pass


def write_frame(stream: BinaryIO, frame: dict[str, Any]) -> None:
    payload = json.dumps(frame, separators=(",", ":")).encode("utf-8")
    stream.write(struct.pack(">I", len(payload)))
    stream.write(payload)
    stream.flush()


def read_frame(stream: BinaryIO, timeout_seconds: float | None = None) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds if timeout_seconds is not None else None
    raw_len = read_exact(stream, 4, deadline)
    size = struct.unpack(">I", raw_len)[0]
    payload = read_exact(stream, size, deadline)
    value = json.loads(payload.decode("utf-8"))
    if not isinstance(value, dict):
        raise RuntimeError(f"waymark returned non-object frame: {value!r}")
    return value


def read_exact(stream: BinaryIO, size: int, deadline: float | None = None) -> bytes:
    chunks = []
    remaining = size
    fd = stream.fileno()
    while remaining:
        if deadline is not None:
            timeout = deadline - time.monotonic()
            if timeout <= 0:
                raise TimeoutError(f"timed out while reading {size} bytes")
            readable, _, _ = select.select([fd], [], [], timeout)
            if not readable:
                raise TimeoutError(f"timed out while reading {size} bytes")
        chunk = os.read(fd, remaining)
        if not chunk:
            raise EOFError(f"stream closed while reading {size} bytes")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def resolve_waymark_bin() -> str:
    env_path = os.environ.get("WAYMARK_STONE_BIN")
    if env_path:
        return env_path
    root = repo_root()
    candidates = [
        root / "target" / "x86_64-unknown-linux-musl" / "release" / "waymark",
        root / "target" / "release" / "waymark",
        root / "target" / "debug" / "waymark",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    found = shutil.which("waymark")
    if found:
        return found
    return "waymark"


def timeout_seconds_from_env() -> float:
    raw = os.environ.get("WAYMARK_STONE_TIMEOUT_SECONDS")
    if raw is None:
        return DEFAULT_TIMEOUT_SECONDS
    try:
        value = float(raw)
    except ValueError:
        return DEFAULT_TIMEOUT_SECONDS
    if value <= 0:
        return DEFAULT_TIMEOUT_SECONDS
    return value


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def resolve_cwd(cwd: str | None) -> Path:
    if not cwd:
        return Path.cwd()
    path = Path(cwd).expanduser()
    if not path.is_absolute():
        path = Path.cwd() / path
    return path.resolve()


def normalize_stone_process_result(
    proc: subprocess.CompletedProcess[str], max_output_bytes: int, duration_ms: int
) -> dict[str, Any]:
    payload = parse_json_response(proc.stdout) or parse_json_response(proc.stderr)
    stdout = bound_text(proc.stdout, max_output_bytes)
    stderr = bound_text(proc.stderr, max_output_bytes)

    if isinstance(payload, dict):
        result: dict[str, Any] = {"ok": bool(payload.get("ok"))}
        output = payload.get("output") if isinstance(payload.get("output"), dict) else {}
        payload_stdout = output.get("stdout") if isinstance(output, dict) else None
        payload_stderr = output.get("stderr") if isinstance(output, dict) else None
        if payload.get("ok"):
            if "value" in payload:
                result["value"] = payload["value"]
            result["stdout"] = bound_text(payload_stdout or "", max_output_bytes)
            result["stderr"] = bound_text(payload_stderr or stderr, max_output_bytes)
        else:
            result["error"] = normalize_stone_error(payload.get("error"))
            result["stdout"] = bound_text(payload_stdout or stdout, max_output_bytes)
            result["stderr"] = bound_text(payload_stderr or stderr, max_output_bytes)
        diagnostics = {
            "exit_code": proc.returncode,
            "duration_ms": duration_ms,
            "backend": "subprocess",
            "hot_loop": parse_hot_loop_diagnostics(stderr),
        }
        payload_diagnostics = (
            payload.get("diagnostics") if isinstance(payload.get("diagnostics"), dict) else {}
        )
        if isinstance(payload_diagnostics.get("session"), dict):
            diagnostics["session"] = payload_diagnostics["session"]
        result["diagnostics"] = diagnostics
        return sparse(result)

    return sparse(
        {
            "ok": proc.returncode == 0,
            "stdout": stdout,
            "stderr": stderr,
            "error": None
            if proc.returncode == 0
            else {
                "kind": "process_exit",
                "code": "stone_process_exit",
                "message": f"waymark exited {proc.returncode} without a JSON response",
            },
            "diagnostics": {
                "exit_code": proc.returncode,
                "duration_ms": duration_ms,
                "backend": "subprocess",
            },
        }
    )


def normalize_task_frame_result(
    frame: dict[str, Any], max_output_bytes: int, duration_ms: int
) -> dict[str, Any]:
    if frame.get("type") == "error":
        return sparse(
            {
                "ok": False,
                "error": normalize_stone_error(frame.get("error")),
                "diagnostics": {"duration_ms": duration_ms, "backend": "warm_stdio"},
            }
        )
    if frame.get("type") != "result":
        return {
            "ok": False,
            "error": {
                "kind": "backend_error",
                "code": "stone_unexpected_frame",
                "message": f"unexpected waymark frame type: {frame.get('type')!r}",
            },
        }

    payload = frame.get("result")
    if not isinstance(payload, dict):
        return {
            "ok": False,
            "error": {
                "kind": "backend_error",
                "code": "stone_malformed_result",
                "message": "waymark result frame did not contain an object result",
            },
        }

    output = payload.get("output") if isinstance(payload.get("output"), dict) else {}
    payload_diagnostics = (
        payload.get("diagnostics") if isinstance(payload.get("diagnostics"), dict) else {}
    )
    hot_loop = payload_diagnostics.get("hot_loop")
    if not isinstance(hot_loop, dict):
        hot_loop = parse_hot_loop_diagnostics(
            output.get("stderr") if isinstance(output, dict) else ""
        )
    diagnostics = {
        "duration_ms": duration_ms,
        "backend": "warm_stdio",
        "reset": compact_reset(frame.get("reset")),
        "hot_loop": hot_loop,
    }
    if isinstance(payload_diagnostics.get("session"), dict):
        diagnostics["session"] = payload_diagnostics["session"]
    result: dict[str, Any] = {
        "ok": bool(payload.get("ok")),
        "stdout": bound_text(output.get("stdout") if isinstance(output, dict) else "", max_output_bytes),
        "stderr": bound_text(output.get("stderr") if isinstance(output, dict) else "", max_output_bytes),
        "diagnostics": diagnostics,
    }
    if payload.get("ok"):
        if "value" in payload and payload.get("value") is not None:
            result["value"] = payload["value"]
    else:
        result["error"] = normalize_stone_error(payload.get("error"))
    return sparse(result)


def compact_reset(reset: Any) -> dict[str, Any] | None:
    if not isinstance(reset, dict):
        return None
    compact: dict[str, Any] = {
        "ok": reset.get("ok"),
        "task_state": reset.get("task_state"),
        "work": reset.get("work"),
    }
    memory = reset.get("memory")
    if isinstance(memory, dict):
        after_reset = memory.get("after_reset")
        if isinstance(after_reset, dict):
            compact["memory_after_reset"] = {
                key: after_reset.get(key)
                for key in ("source", "active_file_bytes", "stale_file_bytes", "stale_files")
                if after_reset.get(key) not in (None, "", [], {})
            }
    return sparse(compact)


def normalize_stone_error(error: Any) -> dict[str, Any]:
    if not isinstance(error, dict):
        return {"kind": "stone_error", "code": "stone_error", "message": str(error)}
    normalized: dict[str, Any] = {}
    for key in ("kind", "code", "message", "detail", "span", "hint", "location"):
        value = error.get(key)
        if value not in (None, "", [], {}):
            normalized[key] = value
    if "message" not in normalized and "detail" in normalized:
        normalized["message"] = normalized["detail"]
    if "kind" not in normalized:
        normalized["kind"] = "stone_error"
    if "code" not in normalized:
        normalized["code"] = "stone_error"
    return normalized


def parse_json_response(text: str) -> dict[str, Any] | None:
    for line in reversed(text.splitlines()):
        line = line.strip()
        if not line:
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and "ok" in value:
            return value
    return None


def bound_text(value: str | bytes | None, max_bytes: int = MAX_OUTPUT_BYTES) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        value = value.decode("utf-8", errors="replace")
    encoded = value.encode("utf-8")
    if len(encoded) <= max_bytes:
        return value
    suffix = f"\n[truncated to {max_bytes} bytes]\n"
    keep = max(0, max_bytes - len(suffix.encode("utf-8")))
    return encoded[:keep].decode("utf-8", errors="replace") + suffix


def apply_large_result_policy(
    result: dict[str, Any], *, allow_large_output: bool = False, force_allowed: bool = True
) -> dict[str, Any]:
    if (allow_large_output and force_allowed) or not result.get("ok") or "value" not in result:
        return result
    preview = large_result_preview(result.get("value"))
    if preview is None:
        return result

    policy_result = dict(result)
    policy_result["value"] = preview
    diagnostics = dict(policy_result.get("diagnostics") or {})
    large_output = {
        "policy": "preview",
        "message": (
            "Large structured result was replaced with a peek. Bind large values and "
            "emit len/head/tail summaries, or call stone_eval with allow_large_output=true "
            "when the full value is required."
            if force_allowed
            else "Large control-plane feedback was replaced with a bounded peek. Use its "
            "summary and samples, then request a narrower state or diff view if needed."
        ),
    }
    if force_allowed:
        large_output["force"] = "set allow_large_output=true on stone_eval"
    diagnostics["large_output"] = large_output
    policy_result["diagnostics"] = diagnostics
    return sparse(policy_result)


def large_result_preview(value: Any) -> dict[str, Any] | None:
    kind = "json"
    count: int | None = None
    if isinstance(value, list):
        kind = "list"
        count = len(value)
        if count <= LARGE_RESULT_LIST_ITEMS and json_value_size(value) <= LARGE_RESULT_BYTES:
            return None
        preview: dict[str, Any] = {
            "__waymark_large_output__": True,
            "type": "list",
            "len": count,
            "head": value[:LARGE_RESULT_PEEK_ITEMS],
        }
        tail = value[-LARGE_RESULT_PEEK_ITEMS:] if count > LARGE_RESULT_PEEK_ITEMS else []
        if tail:
            preview["tail"] = tail
        preview["hint"] = (
            "The full value is still live if you bound it in the Stone session. "
            "Use head(rows, n), tail(rows, n), len(rows), write_json(...), or "
            "allow_large_output=true to force the full emitted value."
        )
        return preview

    if json_value_size(value) <= LARGE_RESULT_BYTES:
        return None
    if isinstance(value, dict):
        kind = "record"
        count = len(value)
        control = preview_record_control_fields(value)
        return {
            "__waymark_large_output__": True,
            "type": kind,
            "keys": list(value.keys())[:LARGE_RESULT_PEEK_ITEMS],
            "len": count,
            **control,
            **({"control": control} if control else {}),
            "head": preview_record(value),
            "hint": (
                "Large record output was replaced with a peek. Emit selected fields "
                "or use allow_large_output=true to force the full value."
            ),
        }
    return {
        "__waymark_large_output__": True,
        "type": kind,
        "bytes": json_value_size(value),
        "hint": "Large output was replaced with a peek; use allow_large_output=true to force it.",
    }


RECORD_CONTROL_PREVIEW_KEYS = (
    "ok",
    "kind",
    "exit_code",
    "duration_ms",
    "timed_out",
    "still_running",
    "done",
    "next_action",
    "run_id",
    "cwd",
    "argv",
)


def preview_record_control_fields(value: dict[str, Any]) -> dict[str, Any]:
    return {
        key: value[key]
        for key in RECORD_CONTROL_PREVIEW_KEYS
        if key in value
    }


def preview_record(value: dict[str, Any]) -> dict[str, Any]:
    preview: dict[str, Any] = {}
    for key, item in list(value.items())[:LARGE_RESULT_PEEK_ITEMS]:
        if isinstance(item, list):
            preview[key] = {
                "type": "list",
                "len": len(item),
                "head": item[: min(len(item), LARGE_RESULT_PEEK_ITEMS)],
            }
        elif isinstance(item, dict):
            preview[key] = {
                "type": "record",
                "len": len(item),
                "keys": list(item.keys())[:LARGE_RESULT_PEEK_ITEMS],
            }
        elif isinstance(item, (str, bytes)):
            preview[key] = bound_text(item, max_bytes=2 * 1024)
        else:
            preview[key] = item
    return preview


def json_value_size(value: Any) -> int:
    try:
        return len(json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8"))
    except (TypeError, ValueError):
        return len(str(value).encode("utf-8", errors="replace"))


def parse_hot_loop_diagnostics(text: str | bytes | None) -> dict[str, Any] | None:
    if isinstance(text, bytes):
        text = text.decode("utf-8", errors="replace")
    if not isinstance(text, str) or "WAYMARK_STONE_HOT_LOOP_DIAGNOSTICS" not in text:
        return None
    for line in text.splitlines():
        _, sep, payload = line.partition("WAYMARK_STONE_HOT_LOOP_DIAGNOSTICS ")
        if not sep:
            continue
        try:
            value = json.loads(payload)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
    return None


def sparse(value: dict[str, Any]) -> dict[str, Any]:
    sparse_value: dict[str, Any] = {}
    for key, item in value.items():
        if item in (None, "", [], {}):
            continue
        if isinstance(item, dict):
            item = sparse(item)
            if not item:
                continue
        sparse_value[key] = item
    return sparse_value


def stone_help(backend: StoneBackend, name: str | None = None) -> dict[str, Any]:
    if blind_surface_enabled() or explicit_tx_surface_enabled() or attempt_surface_enabled():
        if name is None:
            return {"ok": True, "value": visible_help_table()}
        normalized = Stone_CALL_ALIASES.get(str(name), str(name))
        if blind_surface_enabled() and normalized in BLIND_HELP_NAMES:
            return {
                "ok": False,
                "error": {
                    "kind": "hidden_call",
                    "code": "stone_call_hidden_blind_surface",
                    "message": f"{normalized} is hidden in blind agent surface mode",
                },
            }
        if explicit_tx_surface_enabled() and normalized in EXPLICIT_TX_HIDDEN_HELP_NAMES:
            return {
                "ok": False,
                "error": {
                    "kind": "hidden_call",
                    "code": "stone_call_hidden_explicit_tx_surface",
                    "message": f"{normalized} is hidden in explicit transaction agent surface mode",
                },
            }
        if attempt_surface_enabled() and normalized not in ATTEMPT_HELP_NAMES:
            return {
                "ok": False,
                "error": {
                    "kind": "hidden_call",
                    "code": "stone_call_hidden_attempt_surface",
                    "message": f"{normalized} is hidden in attempt agent surface mode",
                },
            }
    source = "emit(help())\n" if not name else f"emit(help({stone_literal(name)}))\n"
    result = backend.eval(source)
    if result.get("ok") is True:
        return enrich_stone_help_result(result, name)
    return {
        **result,
        "error": {
            **(result.get("error") if isinstance(result.get("error"), dict) else {}),
            "hint": "stone_help delegates to shell help(); try stone_eval with help() for details.",
        },
    }


def enrich_stone_help_result(result: dict[str, Any], name: str | None) -> dict[str, Any]:
    if not name:
        return result
    signature = stone_signature_value(name)
    if signature is None:
        return result
    value = result.get("value")
    if not isinstance(value, dict):
        return result
    return {**result, "value": {**value, **signature}}


def stone_signature_value(name: str) -> dict[str, Any] | None:
    normalized = Stone_CALL_ALIASES.get(name, name)
    entry = HELP_TABLE.get(normalized)
    if entry is None:
        return None
    order = Stone_CALL_ARG_ORDER.get(normalized, ())
    args = entry.get("args")
    if args is None:
        args = [
            {"name": arg_name, "type": "Any", "required": index == 0}
            for index, arg_name in enumerate(order)
        ]
    example_call = entry.get("example_call")
    if example_call is None:
        example_args = []
        for item in args:
            if not isinstance(item, dict) or "example" not in item:
                break
            example_args.append(item["example"])
            if not item.get("required"):
                break
        example_call = {"name": normalized, "args": example_args}
    value = {
        "name": normalized,
        "call_form": entry.get("call_form", "stone_call"),
        "signature": entry.get("signature"),
        "args": args,
        "returns": entry.get("returns", {"type": "Any"}),
        "example_call": example_call,
    }
    if "also_accepts" in entry:
        value["also_accepts"] = entry["also_accepts"]
    if normalized != name:
        value["alias_for"] = normalized
    return value


def stone_signature(name: str) -> dict[str, Any]:
    normalized = Stone_CALL_ALIASES.get(name, name)
    if explicit_tx_surface_enabled() and normalized in EXPLICIT_TX_HIDDEN_HELP_NAMES:
        return {
            "ok": False,
            "error": {
                "kind": "hidden_call",
                "code": "stone_call_hidden_explicit_tx_surface",
                "message": f"{normalized} is hidden in explicit transaction agent surface mode",
            },
        }
    if attempt_surface_enabled() and normalized not in ATTEMPT_HELP_NAMES:
        return {
            "ok": False,
            "error": {
                "kind": "hidden_call",
                "code": "stone_call_hidden_attempt_surface",
                "message": f"{normalized} is hidden in attempt agent surface mode",
            },
        }
    value = stone_signature_value(name)
    if value is None:
        return {
            "ok": False,
            "error": {
                "kind": "unknown_call",
                "code": "stone_signature_unknown",
                "message": f"stone_signature does not know {name!r}",
                "hint": "Use stone_help without a name to inspect supported operations.",
            },
        }
    return {"ok": True, "value": value}


def stone_call(backend: StoneBackend, name: str, args: Any, cwd: str | None = None) -> dict[str, Any]:
    name = Stone_CALL_ALIASES.get(name, name)
    if blind_surface_enabled() and name in BLIND_STONE_CALLS:
        return {
            "ok": False,
            "error": {
                "kind": "hidden_call",
                "code": "stone_call_hidden_blind_surface",
                "message": f"{name} is hidden in blind agent surface mode",
            },
        }
    if explicit_tx_surface_enabled() and name in EXPLICIT_TX_HIDDEN_STONE_CALLS:
        return {
            "ok": False,
            "error": {
                "kind": "hidden_call",
                "code": "stone_call_hidden_explicit_tx_surface",
                "message": f"{name} is hidden in explicit transaction agent surface mode",
            },
        }
    if attempt_surface_enabled() and name not in ATTEMPT_STONE_CALLS:
        return {
            "ok": False,
            "error": {
                "kind": "hidden_call",
                "code": "stone_call_hidden_attempt_surface",
                "message": f"{name} is hidden in attempt agent surface mode",
            },
        }
    try:
        args = parse_stone_call_args(args)
    except ValueError as err:
        return {
            "ok": False,
            "error": {
                "kind": "invalid_request",
                "code": "stone_call_invalid_args",
                "message": str(err),
            },
        }
    args = normalize_stone_call_args(name, args)

    if name == "help":
        help_name = None
        if isinstance(args, list) and args:
            help_name = str(args[0])
        elif isinstance(args, dict):
            help_name = args.get("name", args.get("topic"))
            if help_name is not None:
                help_name = str(help_name)
        return stone_help(backend, help_name)

    if name not in Stone_CALL_ARG_ORDER:
        return {
            "ok": False,
            "error": {
                "kind": "unknown_call",
                "code": "stone_call_unknown",
                "message": f"stone_call does not support {name!r}",
                "hint": "Use stone_help to inspect supported operations, or stone_eval for custom source.",
            },
        }

    try:
        source_args = stone_call_resolved_args(name, args, cwd)
        source = stone_call_source(name, source_args)
    except ValueError as err:
        return {
            "ok": False,
            "error": {
                "kind": "invalid_request",
                "code": "stone_call_invalid_args",
                "message": str(err),
            },
        }

    result = backend.eval(source)
    effects = stone_call_effects(name, args)
    if result.get("ok") and effects:
        result = {**result, "effects": effects}
    return result


def parse_stone_call_args(args: Any) -> Any:
    if isinstance(args, str):
        try:
            return json.loads(args)
        except json.JSONDecodeError as err:
            raise ValueError(f"args string must contain a JSON object or array: {err.msg}") from err
    return args


def normalize_stone_call_args(name: str, args: Any) -> Any:
    if isinstance(args, dict):
        normalized = dict(args)
        if name in {"run", "must_run", "start_daemon"} and "argv" not in normalized and "command" in normalized:
            normalized["argv"] = normalized.pop("command")
        if name in {"run", "must_run"}:
            if "max_output_bytes" in normalized:
                value = normalized.pop("max_output_bytes")
                normalized.setdefault("max_stdout_bytes", value)
                normalized.setdefault("max_stderr_bytes", value)
            if "max_output_chars" in normalized:
                value = normalized.pop("max_output_chars")
                normalized.setdefault("max_stdout_bytes", value)
                normalized.setdefault("max_stderr_bytes", value)
        if name in {"write_file", "write_text"} and "text" not in normalized and "content" in normalized:
            normalized["text"] = normalized.pop("content")
        if name in {"read_file", "read_text"} and "max_bytes" not in normalized and "limit" in normalized:
            normalized["max_bytes"] = normalized.pop("limit")
        if name in {"read_csv", "read_json", "read_jsonl"} and "path" not in normalized:
            if "path_or_file" in normalized:
                normalized["path"] = normalized.pop("path_or_file")
        if name in {"find", "search"} and "root" not in normalized and "path" in normalized:
            normalized["root"] = normalized.pop("path")
        if name == "find":
            if "name_glob" not in normalized and "name" in normalized:
                normalized["name_glob"] = normalized.pop("name")
            if "path_glob" not in normalized and "glob" in normalized:
                normalized["path_glob"] = normalized.pop("glob")
        if name == "search" and "needle" not in normalized:
            if "query" in normalized:
                normalized["needle"] = normalized.pop("query")
            elif "pattern" in normalized:
                normalized["needle"] = normalized.pop("pattern")
        if name == "rm" and "path" not in normalized and "paths" in normalized:
            normalized["path"] = normalized.pop("paths")
        return normalized
    return args


def stone_call_source(name: str, args: Any) -> str:
    positional, named = stone_call_arguments(name, args)
    rendered_args = [stone_literal(value) for value in positional]
    rendered_args.extend(f"{key}={stone_literal(value)}" for key, value in named.items())
    return f"emit({name}({', '.join(rendered_args)}))\n"


def stone_call_resolved_args(name: str, args: Any, cwd: str | None) -> Any:
    if name == "env_restore":
        return resolve_env_restore_args(args, cwd)
    if name not in {
        "cat",
        "find",
        "read_csv",
        "read_file",
        "read_json",
        "read_jsonl",
        "read_text",
        "list",
        "list_dir",
        "ls",
        "mkdir",
        "edit",
        "edit_file",
        "rm",
        "search",
        "stat",
        "write_file",
        "write_json",
        "write_jsonl",
        "write_text",
    }:
        return args
    if isinstance(args, list) and args and isinstance(args[0], str):
        resolved = list(args)
        resolved[0] = str(resolve_workspace_path(resolved[0], cwd))
        return resolved
    if isinstance(args, dict):
        key = "root" if name in {"find", "search"} else "path"
        if isinstance(args.get(key), str):
            resolved = dict(args)
            resolved[key] = str(resolve_workspace_path(resolved[key], cwd))
            return resolved
        if name == "rm" and isinstance(args.get(key), list):
            resolved = dict(args)
            resolved[key] = [
                str(resolve_workspace_path(path, cwd)) if isinstance(path, str) else path
                for path in resolved[key]
            ]
            return resolved
    return args


def resolve_env_restore_args(args: Any, cwd: str | None) -> Any:
    def resolve(path: Any) -> Any:
        if not isinstance(path, str) or not Path(path).is_absolute():
            return path
        workspace = os.environ.get("WAYMARK_GATEWAY_WORKSPACE_MOUNT") or cwd
        if not workspace:
            return path
        try:
            relative = Path(path).resolve().relative_to(Path(workspace).resolve())
        except ValueError:
            return path
        return relative.as_posix() if relative.parts else path

    if isinstance(args, list):
        return [resolve(path) for path in args]
    if isinstance(args, dict):
        paths = args.get("paths")
        resolved = dict(args)
        if isinstance(paths, list):
            resolved["paths"] = [resolve(path) for path in paths]
        elif paths is not None:
            resolved["paths"] = resolve(paths)
        return resolved
    if isinstance(args, str):
        return resolve(args)
    return args


def stone_call_arguments(name: str, args: Any) -> tuple[list[Any], dict[str, Any]]:
    order = Stone_CALL_ARG_ORDER[name]
    if name in Stone_TWO_POSITIONAL_THEN_KEYWORDS and isinstance(args, dict):
        missing = [key for key in order[:2] if key not in args]
        if missing:
            raise ValueError(f"{name} requires a {missing[0]} argument")
        positional = [args[key] for key in order[:2]]
        named = {
            key: value
            for key, value in args.items()
            if key not in order[:2] and value is not None
        }
        return positional, named

    if name in Stone_ONE_POSITIONAL_THEN_KEYWORDS and isinstance(args, dict):
        if "argv" not in args:
            first_key = order[0]
            if first_key not in args:
                raise ValueError(f"{name} requires a {first_key} argument")
        else:
            first_key = "argv"
        named = {
            key: value
            for key, value in args.items()
            if key != first_key and value is not None
        }
        if name in {"run", "must_run", "start_daemon"} and isinstance(args[first_key], str):
            raise ValueError(
                f"{name} argv must be a list of strings, not a string; "
                f'use {{"argv": ["{args[first_key]}"]}} or use stone_eval with {name}(["{args[first_key]}"])'
            )
        return [args[first_key]], named

    if isinstance(args, list):
        if name in {"run", "must_run", "start_daemon"} and args and isinstance(args[0], str):
            if all(isinstance(item, str) for item in args):
                return [args], {}
            raise ValueError(
                f"{name} argv must be a list of strings, not a mixed positional array; "
                f'use {{"argv": ["{args[0]}"]}}, [["{args[0]}"]], '
                f'or {name}(["{args[0]}"]) in stone_eval'
            )
        if len(args) > len(order):
            raise ValueError(f"{name} accepts at most {len(order)} positional arguments")
        if name in Stone_ONE_POSITIONAL_THEN_KEYWORDS and len(args) > 1:
            named = {
                key: value
                for key, value in zip(order[1:], args[1:])
                if value is not None
            }
            return [args[0]], named
        return list(args), {}
    if not isinstance(args, dict):
        raise ValueError("args must be an object or array")

    positional: list[Any] = []
    named: dict[str, Any] = {}
    for index, key in enumerate(order):
        if key not in args:
            continue
        if index == len(positional):
            positional.append(args[key])
        else:
            named[key] = args[key]

    for key, value in args.items():
        if key in order:
            continue
        if not key.isidentifier():
            raise ValueError(f"invalid Stone keyword argument name: {key!r}")
        named[key] = value
    return positional, named


def stone_literal(value: Any) -> str:
    if value is None:
        return "None"
    if value is True:
        return "True"
    if value is False:
        return "False"
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if value != value or value in (float("inf"), float("-inf")):
            raise ValueError("non-finite float values are not valid Stone literals")
        return repr(value)
    if isinstance(value, list):
        return "[" + ", ".join(stone_literal(item) for item in value) + "]"
    if isinstance(value, dict):
        items = []
        for key, item in value.items():
            if not isinstance(key, str):
                raise ValueError("record keys must be strings")
            items.append(f"{json.dumps(key)}: {stone_literal(item)}")
        return "{" + ", ".join(items) + "}"
    raise ValueError(f"unsupported argument value type: {type(value).__name__}")


def stone_call_effects(name: str, args: Any) -> dict[str, Any]:
    effects = HELP_TABLE.get(name, {}).get("effects") or []
    result: dict[str, Any] = {}
    path = stone_call_path_arg(name, args)
    if "read_file" in effects and path:
        result["reads"] = [path]
    if "write_file" in effects and path:
        result["writes"] = [path]
    if "remove_file" in effects and path:
        result["removes"] = [path]
    elif "remove_file" in effects:
        paths = stone_call_path_args(name, args)
        if paths:
            result["removes"] = paths
    if "create_parent_dirs" in effects:
        result["creates_parent_dirs"] = True
    if effects and not result:
        result["unknown"] = True
    return result


def stone_call_path_arg(name: str, args: Any) -> str | None:
    if isinstance(args, dict):
        value = args.get("path") or args.get("root")
    elif isinstance(args, list) and args:
        value = args[0]
    else:
        value = None
    return value if isinstance(value, str) else None


def stone_call_path_args(name: str, args: Any) -> list[str]:
    if isinstance(args, dict):
        value = args.get("path") or args.get("root")
    elif isinstance(args, list) and args:
        value = args[0]
    else:
        value = None
    if isinstance(value, list):
        return [item for item in value if isinstance(item, str)]
    return [value] if isinstance(value, str) else []


def stone_describe(path: str, cwd: str | None = None) -> dict[str, Any]:
    target = workspace_path(path, cwd)
    resolved_target = resolve_workspace_path(path, cwd)
    display_path = path
    if not target.exists() and not target.is_symlink():
        return {
            "ok": False,
            "error": {
                "kind": "not_found",
                "code": "stone_describe_not_found",
                "message": f"path does not exist: {display_path}",
            },
        }

    stat = target.lstat()
    value: dict[str, Any] = {"path": display_path, "size": stat.st_size}
    effects = {"reads": [display_path]}
    if target.is_symlink():
        value["symlink_target"] = os.readlink(target)
        if not resolved_target.exists():
            value.update({"kind": "symlink", "broken": True})
            return {"ok": True, "value": value, "effects": effects}
    if resolved_target.is_dir():
        entries = sorted(child.name for child in resolved_target.iterdir())[:100]
        value.update({"kind": "directory", "entries": entries})
        return {"ok": True, "value": value, "effects": effects}

    data = resolved_target.read_bytes()[:MAX_PREVIEW_BYTES]
    text = data.decode("utf-8", errors="replace")
    kind = infer_kind(resolved_target, data)
    value["kind"] = kind
    if kind != "binary":
        value["preview"] = text
    if stat.st_size > len(data):
        value["preview_truncated"] = True

    schema = infer_schema(kind, text)
    if schema:
        value["schema"] = schema
    return sparse({"ok": True, "value": value, "effects": effects})


def workspace_path(path: str, cwd: str | None = None) -> Path:
    raw = Path(path).expanduser()
    if raw.is_absolute():
        return raw
    return resolve_cwd(cwd) / raw


def resolve_workspace_path(path: str, cwd: str | None = None) -> Path:
    return workspace_path(path, cwd).resolve()


def infer_kind(path: Path, data: bytes) -> str:
    if b"\0" in data:
        return "binary"
    suffix = path.suffix.lower()
    if suffix == ".json":
        return "json"
    if suffix == ".jsonl":
        return "jsonl"
    if suffix == ".csv":
        return "csv"
    return "text"


def infer_schema(kind: str, text: str) -> Any:
    if kind == "json":
        try:
            return schema_for_value(json.loads(text))
        except json.JSONDecodeError:
            return None
    if kind == "jsonl":
        schemas = []
        for line in text.splitlines():
            if not line.strip():
                continue
            try:
                schemas.append(schema_for_value(json.loads(line)))
            except json.JSONDecodeError:
                break
            if len(schemas) >= 5:
                break
        return schemas or None
    if kind == "csv":
        rows = list(csv.reader(text.splitlines()))
        if not rows:
            return None
        headers = rows[0]
        return {header: "str" for header in headers}
    return None


def schema_for_value(value: Any) -> Any:
    if isinstance(value, dict):
        return {str(key): schema_for_value(item) for key, item in list(value.items())[:50]}
    if isinstance(value, list):
        return [schema_for_value(value[0])] if value else ["unknown"]
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "int"
    if isinstance(value, float):
        return "float"
    if isinstance(value, str):
        return "str"
    return type(value).__name__


def escape_linux(reason: str, command: str, cwd: str | None = None) -> dict[str, Any]:
    if not reason.strip():
        return {
            "ok": False,
            "error": {
                "kind": "invalid_request",
                "code": "escape_linux_reason_required",
                "message": "escape_linux requires a non-empty reason",
            },
        }
    run_cwd = resolve_cwd(cwd)
    gateway_container = os.environ.get("WAYMARK_GATEWAY_CONTAINER", "").strip()
    gateway_workspace_mount = os.environ.get("WAYMARK_GATEWAY_WORKSPACE_MOUNT", "").strip()
    if gateway_container:
        if gateway_workspace_mount and str(run_cwd).startswith(gateway_workspace_mount):
            exec_cwd = str(run_cwd)
        elif gateway_workspace_mount:
            exec_cwd = gateway_workspace_mount
        else:
            exec_cwd = "/"
        argv = ["docker", "exec", "-w", exec_cwd, gateway_container, "sh", "-lc", command]
        execution_target = {
            "kind": "gateway_container",
            "container": gateway_container,
            "cwd": exec_cwd,
        }
    else:
        argv = command
        execution_target = {"kind": "mcp_process", "cwd": str(run_cwd)}
    start = time.monotonic()
    try:
        proc = subprocess.run(
            argv,
            shell=not gateway_container,
            cwd=str(run_cwd) if not gateway_container else None,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=DEFAULT_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as err:
        return sparse(
            {
                "ok": False,
                "error": {
                    "kind": "timeout",
                    "code": "escape_linux_timeout",
                    "message": f"command timed out after {DEFAULT_TIMEOUT_SECONDS:g}s",
                },
                "stdout": bound_text(err.stdout or ""),
                "stderr": bound_text(err.stderr or ""),
                "gap": classify_escape_gap(reason, command),
            }
        )

    return sparse(
        {
            "ok": proc.returncode == 0,
            "stdout": bound_text(proc.stdout),
            "stderr": bound_text(proc.stderr),
            "error": None
            if proc.returncode == 0
            else {
                "kind": "process_exit",
                "code": "escape_linux_process_exit",
                "message": f"command exited {proc.returncode}",
            },
            "gap": classify_escape_gap(reason, command),
            "diagnostics": {
                "exit_code": proc.returncode,
                "duration_ms": int((time.monotonic() - start) * 1000),
                "execution_target": execution_target,
            },
            "effects": {"unknown": True},
        }
    )


def classify_escape_gap(reason: str, command: str) -> str:
    text = f"{reason} {command}".lower()
    if any(token in text for token in ("cargo test", "pytest", "test ", " test")):
        return "process/test_runner"
    if any(token in text for token in ("npm install", "pip install", "cargo fetch", "apt ")):
        return "process/package_manager"
    if any(token in text for token in ("cargo build", "gcc", "clang", "rustc", "make")):
        return "process/compiler"
    if "git " in f" {text}":
        return "git/basic"
    if any(token in text for token in ("tar ", "zip", "unzip", ".tar", ".gz")):
        return "filesystem/archive"
    if any(token in text for token in ("patch", "diff")):
        return "filesystem/patch"
    return "unsupported/out_of_scope"


TOOLS = [
    {
        "name": "stone_eval",
        "description": (
            "Run Stone source in the long-lived Stone session. In warm-stdio mode, "
            "top-level value and function bindings persist across stone_eval calls, "
            "so bind intermediate data once and reuse names later. Source may be a "
            "multi-line script like python -c or bash -c, not only a single expression. "
            "Large emitted values are replaced with a head/tail peek by default; bind "
            "large values and emit len/head/tail summaries, or set allow_large_output=true "
            "only when the full value is required. Open file handles do not persist."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": {"type": "string"},
                "cwd": {"type": "string"},
                "allow_large_output": {
                    "type": "boolean",
                    "description": "Bypass the MCP large-result peek and return the full emitted value.",
                },
            },
            "required": ["source"],
            "additionalProperties": False,
        },
    },
    {
        "name": "stone_help",
        "description": "Return concise structured help for Stone builtins.",
        "inputSchema": {
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "additionalProperties": False,
        },
    },
    {
        "name": "stone_signature",
        "description": (
            "Return the JSON call convention for a supported Stone builtin. "
            "Use this before stone_call when argument nesting is ambiguous."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"],
            "additionalProperties": False,
        },
    },
    {
        "name": "stone_call",
        "description": (
            "Call a supported Stone builtin with JSON arguments. Large returned values "
            "are replaced with a head/tail peek by default; set allow_large_output=true "
            "only when the full value is required. For run/must_run, args may be "
            '{"argv":["cmd","arg"]}, [["cmd","arg"]] for positional call-form, '
            'or the direct argv convenience ["cmd","arg"].'
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "args": {
                    "description": (
                        "Builtin arguments as a JSON object or positional array. "
                        'For run/must_run, ["cmd","arg"] is treated as argv.'
                    ),
                    "oneOf": [
                        {"type": "object"},
                        {"type": "array"},
                    ]
                },
                "cwd": {"type": "string"},
                "allow_large_output": {
                    "type": "boolean",
                    "description": "Bypass the MCP large-result peek and return the full value.",
                },
            },
            "required": ["name", "args"],
            "additionalProperties": False,
        },
    },
    {
        "name": "attempt_spawn",
        "description": (
            "Spawn a child Waymark attempt. Prefer this typed wrapper over stone_call "
            "for attempt control; set program.kind to \"stone\" and program.source "
            "to the Stone control script."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "task": {"type": "string"},
                "workspace": {
                    "type": "string",
                    "description": "Logical Gateway workspace name, for example harbor-crack-7z-hash.",
                },
                "task_spec": {"type": "object"},
                "task_input": {},
                "program": {"type": "object"},
                "workspace_source": {"type": "object"},
                "context_source": {"type": "object"},
                "capabilities": {"type": "object"},
                "start": {"type": "boolean"},
                "controller": {"type": "string"},
                "capability_profile": {"type": "string"},
                "container": {"type": "string"},
                "workspace_mount": {"type": "string"},
                "parent_attempt": {"type": "string"},
                "resource_limits": {"type": "object"},
                "metadata": {"type": "object"},
                "cwd": {"type": "string"},
                "allow_large_output": {
                    "type": "boolean",
                    "description": "Bypass the MCP large-result peek and return the full value.",
                },
            },
            "required": ["task", "workspace", "program"],
            "additionalProperties": False,
        },
    },
    {
        "name": "attempt_start",
        "description": "Start a spawned Waymark attempt controller.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "attempt": {"type": "string"},
                "wait": {"type": "boolean"},
                "timeout_ms": {"type": "integer"},
                "cwd": {"type": "string"},
                "allow_large_output": {"type": "boolean"},
            },
            "required": ["attempt"],
            "additionalProperties": False,
        },
    },
    {
        "name": "attempt_state",
        "description": "Inspect an attempt state and bounded workspace diff sample.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "attempt": {"type": "string"},
                "sample_limit": {"type": "integer"},
                "cwd": {"type": "string"},
                "allow_large_output": {"type": "boolean"},
            },
            "required": ["attempt"],
            "additionalProperties": False,
        },
    },
    {
        "name": "attempt_info",
        "description": "Inspect metadata for one attempt, defaulting to the current attempt when omitted.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "attempt": {"type": "string"},
                "cwd": {"type": "string"},
                "allow_large_output": {"type": "boolean"},
            },
            "additionalProperties": False,
        },
    },
    {
        "name": "attempts",
        "description": "List Waymark attempts, optionally filtered by task, workspace, or state.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task": {"type": "string"},
                "workspace": {"type": "string"},
                "state": {"type": "string"},
                "cwd": {"type": "string"},
                "allow_large_output": {"type": "boolean"},
            },
            "additionalProperties": False,
        },
    },
    {
        "name": "attempt_finish",
        "description": "Commit or roll back a Waymark attempt.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["commit", "rollback"]},
                "attempt": {"type": "string"},
                "message": {"type": "string"},
                "reason": {"type": "string"},
                "allow_risky": {"type": "boolean"},
                "cwd": {"type": "string"},
                "allow_large_output": {"type": "boolean"},
            },
            "required": ["action", "attempt"],
            "additionalProperties": False,
        },
    },
    {
        "name": "stone_describe",
        "description": "Describe a workspace path with stat data, bounded preview, and cheap schema inference.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "cwd": {"type": "string"},
            },
            "required": ["path"],
            "additionalProperties": False,
        },
    },
    {
        "name": "escape_linux",
        "description": "Explicit Linux shell fallback for Stone-primary evaluation mode.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "reason": {"type": "string"},
                "command": {"type": "string"},
                "cwd": {"type": "string"},
            },
            "required": ["reason", "command"],
            "additionalProperties": False,
        },
    },
]

BLIND_STONE_CALLS = {
    "attempt_finish",
    "attempt_fork",
    "attempt_info",
    "attempt_list",
    "attempt_logs",
    "attempt_run_process",
    "attempt_spawn",
    "attempt_start",
    "attempt_wait",
    "attempt_terminate",
    "attempt_state",
    "attempts",
    "env_state",
    "env_diff",
    "env_tx_info",
    "env_txs",
    "env_restore",
    "env_checkpoint",
    "env_checkpoint_gc",
    "env_fork",
    "env_restore_checkpoint",
    "env_checkpoints",
    "env_discard_checkpoint",
    "env_run_checkpoint",
    "env_rollback",
    "env_commit",
    "env_finish",
}
BLIND_HELP_NAMES = BLIND_STONE_CALLS
BLIND_RESULT_KEYS = {"effects", "env_state", "env_diff", "env_warnings", "next_actions"}
ATTEMPT_STONE_CALLS = {
    "attempt_finish",
    "attempt_fork",
    "attempt_info",
    "attempt_list",
    "attempt_spawn",
    "attempt_start",
    "attempt_state",
    "attempts",
    "help",
}
ATTEMPT_HELP_NAMES = ATTEMPT_STONE_CALLS
EXPLICIT_TX_HIDDEN_STONE_CALLS = {
    "attempt_finish",
    "attempt_fork",
    "attempt_info",
    "attempt_list",
    "attempt_logs",
    "attempt_run_process",
    "attempt_spawn",
    "attempt_start",
    "attempt_wait",
    "attempt_terminate",
    "attempt_state",
    "attempts",
}
EXPLICIT_TX_HIDDEN_HELP_NAMES = EXPLICIT_TX_HIDDEN_STONE_CALLS
CONTROL_RESULTS_ALWAYS_BOUNDED = {
    "attempt_info",
    "attempt_logs",
    "attempt_state",
    "attempts",
    "env_diff",
    "env_finish",
    "env_state",
    "env_tx_info",
    "env_txs",
}
ATTEMPT_MCP_TOOLS = {
    "attempt_finish",
    "attempt_info",
    "attempt_spawn",
    "attempt_start",
    "attempt_state",
    "attempts",
    "stone_call",
    "stone_help",
    "stone_signature",
}
DIRECT_ATTEMPT_TOOLS = {
    "attempt_finish",
    "attempt_info",
    "attempt_spawn",
    "attempt_start",
    "attempt_state",
    "attempts",
}


def agent_surface_mode() -> str:
    mode = os.environ.get("WAYMARK_GATEWAY_AGENT_SURFACE", "full").strip().lower()
    return mode if mode in {"full", "blind", "explicit-tx", "attempt"} else "full"


def blind_surface_enabled() -> bool:
    return agent_surface_mode() == "blind"


def explicit_tx_surface_enabled() -> bool:
    return agent_surface_mode() == "explicit-tx"


def attempt_surface_enabled() -> bool:
    return agent_surface_mode() == "attempt"


def visible_tools() -> list[dict[str, Any]]:
    if attempt_surface_enabled():
        return [tool for tool in TOOLS if tool.get("name") in ATTEMPT_MCP_TOOLS]
    filtered = [tool for tool in TOOLS if tool.get("name") not in DIRECT_ATTEMPT_TOOLS]
    if not blind_surface_enabled():
        return filtered
    return filtered


def visible_help_table() -> dict[str, dict[str, Any]]:
    table = HELP_TABLE
    if blind_surface_enabled():
        table = {name: value for name, value in HELP_TABLE.items() if name not in BLIND_HELP_NAMES}
    elif explicit_tx_surface_enabled():
        table = {
            name: value
            for name, value in HELP_TABLE.items()
            if name not in EXPLICIT_TX_HIDDEN_HELP_NAMES
        }
    elif attempt_surface_enabled():
        table = {name: value for name, value in HELP_TABLE.items() if name in ATTEMPT_HELP_NAMES}
    return {
        name: {**value, **(stone_signature_value(name) or {})}
        for name, value in table.items()
    }


def sanitize_for_agent(value: Any) -> Any:
    if not blind_surface_enabled():
        return value
    if isinstance(value, dict):
        return {
            key: sanitize_for_agent(item)
            for key, item in value.items()
            if key not in BLIND_RESULT_KEYS
        }
    if isinstance(value, list):
        return [sanitize_for_agent(item) for item in value]
    return value


def hidden_blind_source(source: str) -> bool:
    hidden = (
        "env_state",
        "env_diff",
        "env_tx_info",
        "env_txs",
        "env_restore",
        "env_checkpoint",
        "env_checkpoint_gc",
        "env_fork",
        "env_restore_checkpoint",
        "env_checkpoints",
        "env_discard_checkpoint",
        "env_run_checkpoint",
        "env_rollback",
    )
    return any(name in source for name in hidden)


def hidden_explicit_tx_source(source: str) -> bool:
    names = "|".join(re.escape(name) for name in sorted(EXPLICIT_TX_HIDDEN_STONE_CALLS))
    return re.search(rf"\b(?:{names})\b", source) is not None


def optional_int_env(name: str) -> int | None:
    value = os.environ.get(name)
    if not value:
        return None
    try:
        return int(float(value))
    except ValueError:
        return None


def runtime_status(cwd: str | None = None) -> dict[str, Any]:
    now_ms = int(time.time() * 1000)
    started_at_ms = optional_int_env("WAYMARK_AGENT_STARTED_AT_MS")
    deadline_ms = optional_int_env("WAYMARK_AGENT_DEADLINE_MS")
    time_budget = None
    if started_at_ms is not None or deadline_ms is not None:
        time_budget = sparse(
            {
                "kind": "advisory",
                "source": os.environ.get("WAYMARK_AGENT_TIME_BUDGET_SOURCE"),
                "elapsed_sec": max(0, (now_ms - started_at_ms) // 1000)
                if started_at_ms is not None
                else None,
                "remaining_sec": max(0, (deadline_ms - now_ms) // 1000)
                if deadline_ms is not None
                else None,
            }
        )
    return sparse(
        {
            "cwd": cwd or os.environ.get("WAYMARK_STONE_CWD") or os.getcwd(),
            "virtual_env": os.environ.get("VIRTUAL_ENV"),
            "conda_env": os.environ.get("CONDA_DEFAULT_ENV"),
            "gateway": sparse(
                {
                    "tx": os.environ.get("WAYMARK_GATEWAY_TX"),
                    "container": os.environ.get("WAYMARK_GATEWAY_CONTAINER"),
                    "image": os.environ.get("WAYMARK_GATEWAY_IMAGE"),
                    "workspace_mount": os.environ.get("WAYMARK_GATEWAY_WORKSPACE_MOUNT"),
                    "surface": os.environ.get("WAYMARK_GATEWAY_AGENT_SURFACE"),
                }
            ),
            "time_budget": time_budget,
        }
    )


def advisory_time_budget_warnings(
    tool_name: str | None,
    args: dict[str, Any],
    result: dict[str, Any],
    status: dict[str, Any],
) -> list[dict[str, Any]]:
    time_budget = status.get("time_budget")
    if not isinstance(time_budget, dict):
        return []

    remaining_sec = time_budget.get("remaining_sec")
    if not isinstance(remaining_sec, int):
        return []

    low_threshold_sec = optional_int_env("WAYMARK_AGENT_LOW_TIME_WARNING_SEC") or 120
    wrap_up_threshold_sec = optional_int_env("WAYMARK_AGENT_WRAP_UP_WARNING_SEC") or 30
    warnings: list[dict[str, Any]] = []
    call_name = str(args.get("name", "")) if tool_name == "stone_call" else str(tool_name or "")
    call_args = args.get("args") if tool_name == "stone_call" else args

    process_call = call_name in {"run", "must_run", "run_wait", "run_status", "run_terminate"}
    if not process_call:
        return warnings

    if remaining_sec <= 0:
        warnings.append(
            {
                "code": "agent_time_budget_expired",
                "severity": "critical",
                "message": (
                    "Advisory agent time budget is exhausted. Do not start or wait on "
                    "long-running work; finalize with the best available artifact or stop."
                ),
                "suggested_action": "wrap_up_now",
                "suggested_actions": [
                    "Stop starting or waiting on long-running work.",
                    "Use the best existing artifact or current state.",
                    "Run only short validation or file checks if absolutely necessary.",
                    "Finalize the task now.",
                ],
                "remaining_sec": remaining_sec,
            }
        )
    elif remaining_sec <= wrap_up_threshold_sec:
        warnings.append(
            {
                "code": "agent_time_budget_wrap_up",
                "severity": "critical",
                "message": (
                    "Advisory agent time budget is almost exhausted. Wrap up now with "
                    "the best available artifact; do not start or continue long-running work."
                ),
                "suggested_action": "wrap_up_now",
                "suggested_actions": [
                    "Finalize with the best existing artifact or current state.",
                    "Do not start a replacement long-running command.",
                    "If a process is still running, call run_status once and terminate it unless it is already complete.",
                    "Use only quick checks that fit comfortably inside remaining time.",
                ],
                "remaining_sec": remaining_sec,
                "threshold_sec": wrap_up_threshold_sec,
            }
        )
    elif remaining_sec <= low_threshold_sec:
        warnings.append(
            {
                "code": "agent_time_budget_low",
                "severity": "warning",
                "message": (
                    "Advisory agent time budget is low. Avoid long-running work and prefer "
                    "checking status, terminating stale runs, or finalizing existing results."
                ),
                "suggested_action": "prefer_finalize_or_short_checks",
                "suggested_actions": [
                    "Prefer short validation commands over new long-running commands.",
                    "If a process is running, inspect it with run_status.",
                    "Terminate work that cannot finish comfortably before the budget expires.",
                    "Finalize with an existing artifact instead of starting over.",
                ],
                "remaining_sec": remaining_sec,
                "threshold_sec": low_threshold_sec,
            }
        )

    timeout_ms = stone_call_timeout_ms(call_name, call_args)
    if timeout_ms is not None and timeout_ms / 1000 > max(0, remaining_sec):
        warnings.append(
            {
                "code": "tool_wait_exceeds_remaining_time",
                "severity": "warning",
                "message": (
                    "Requested process wait timeout exceeds the advisory remaining agent "
                    "time. Use a shorter wait, run_status, run_terminate, or finalize."
                ),
                "suggested_action": "shorten_wait_or_wrap_up",
                "suggested_actions": [
                    "Use run_status instead of a long run_wait.",
                    "If waiting is necessary, choose timeout_ms below the remaining time.",
                    "If remaining time is low, terminate stale work and wrap up.",
                ],
                "remaining_sec": remaining_sec,
                "requested_timeout_ms": timeout_ms,
            }
        )

    value = result.get("value")
    if isinstance(value, dict) and value.get("still_running") and remaining_sec <= low_threshold_sec:
        warnings.append(
            {
                "code": "process_still_running_with_low_time",
                "severity": "warning",
                "message": (
                    "A process is still running while advisory agent time is low. Prefer "
                    "run_status or run_terminate before another long wait."
                ),
                "suggested_action": "inspect_or_terminate_then_wrap_up",
                "suggested_actions": [
                    "Call run_status to check whether the process is already complete or nearly complete.",
                    "If it cannot finish comfortably within remaining time, call run_terminate.",
                    "Do not start another long-running process.",
                    "Finalize with any usable artifact already present.",
                ],
                "remaining_sec": remaining_sec,
                "run_id": value.get("run_id"),
            }
        )

    return warnings


def stone_call_timeout_ms(call_name: str, args: Any) -> int | None:
    if call_name in {"run", "must_run"}:
        if isinstance(args, dict):
            value = args.get("timeout_ms")
        else:
            return None
    elif call_name == "run_wait":
        if isinstance(args, dict):
            value = args.get("timeout_ms")
        elif isinstance(args, list) and len(args) >= 2:
            value = args[1]
        else:
            return None
    else:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def attach_runtime_status(
    result: dict[str, Any],
    cwd: str | None = None,
    tool_name: str | None = None,
    args: dict[str, Any] | None = None,
) -> dict[str, Any]:
    diagnostics = result.get("diagnostics")
    if not isinstance(diagnostics, dict):
        diagnostics = {}
    status = runtime_status(cwd)
    diagnostics["runtime_status"] = status
    warnings = advisory_time_budget_warnings(tool_name, args or {}, result, status)
    if warnings:
        existing = diagnostics.get("warnings")
        if isinstance(existing, list):
            diagnostics["warnings"] = [*existing, *warnings]
        else:
            diagnostics["warnings"] = warnings
    result["diagnostics"] = diagnostics
    return result


def direct_attempt_call_args(tool_name: str, args: dict[str, Any]) -> dict[str, Any]:
    call_args = {
        key: value
        for key, value in args.items()
        if key not in {"allow_large_output", "cwd"} and value not in (None, "", [], {})
    }
    if tool_name == "attempt_spawn":
        workspace = call_args.get("workspace")
        workspace_source = call_args.get("workspace_source")
        if isinstance(workspace, str) and workspace:
            if isinstance(workspace_source, dict):
                workspace_source = {"workspace": workspace, **workspace_source}
            else:
                workspace_source = {"workspace": workspace}
            call_args["workspace_source"] = workspace_source
    return call_args


class McpServer:
    def __init__(
        self,
        backend: StoneBackend,
        stdin: BinaryIO,
        stdout: BinaryIO,
        log: TextIO,
        trace: TraceRecorder | None = None,
    ) -> None:
        self.backend = backend
        self.stdin = stdin
        self.stdout = stdout
        self.log = log
        self.trace = trace or TraceRecorder()
        self.framing = "headers"

    def serve(self) -> None:
        while True:
            message = self.read_message()
            if message is None:
                return
            if "id" not in message:
                continue
            response = self.handle_request(message)
            self.write_message(response)

    def read_message(self) -> dict[str, Any] | None:
        headers: dict[str, str] = {}
        while True:
            line = self.stdin.readline()
            if line == b"":
                return None
            stripped = line.strip()
            if stripped.startswith(b"{"):
                self.framing = "jsonl"
                return json.loads(stripped.decode("utf-8"))
            if line in (b"\r\n", b"\n"):
                break
            decoded = line.decode("ascii", errors="replace").strip()
            if ":" in decoded:
                key, value = decoded.split(":", 1)
                headers[key.lower()] = value.strip()
        length = int(headers.get("content-length", "0"))
        if length <= 0:
            return None
        body = self.stdin.read(length)
        return json.loads(body.decode("utf-8"))

    def write_message(self, message: dict[str, Any]) -> None:
        body = json.dumps(message, separators=(",", ":")).encode("utf-8")
        if self.framing == "jsonl":
            self.stdout.write(body)
            self.stdout.write(b"\n")
            self.stdout.flush()
            return
        self.stdout.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
        self.stdout.write(body)
        self.stdout.flush()

    def handle_request(self, request: dict[str, Any]) -> dict[str, Any]:
        request_id = request.get("id")
        method = request.get("method")
        try:
            if method == "initialize":
                result = {
                    "protocolVersion": request.get("params", {}).get("protocolVersion", "2024-11-05"),
                    "serverInfo": {"name": "waymark-stone", "version": "0.1.0"},
                    "capabilities": {"tools": {}},
                }
            elif method == "tools/list":
                result = {"tools": visible_tools()}
            elif method == "tools/call":
                result = self.call_tool(request.get("params", {}))
            else:
                return json_rpc_error(request_id, -32601, f"method not found: {method}")
            return {"jsonrpc": "2.0", "id": request_id, "result": result}
        except Exception as err:  # Keep the stdio server alive on tool bugs.
            print(f"stone MCP server error: {err}", file=self.log)
            return json_rpc_error(request_id, -32603, str(err))

    def call_tool(self, params: dict[str, Any]) -> dict[str, Any]:
        name = params.get("name")
        args = params.get("arguments") or {}
        started = time.monotonic()
        if name == "stone_eval":
            if attempt_surface_enabled():
                result = {
                    "ok": False,
                    "error": {
                        "kind": "hidden_call",
                        "code": "stone_eval_hidden_attempt_surface",
                        "message": "stone_eval is hidden in attempt agent surface mode; spawn a child attempt with program.kind='stone' instead",
                    },
                }
            elif blind_surface_enabled() and hidden_blind_source(str(args.get("source", ""))):
                result = {
                    "ok": False,
                    "error": {
                        "kind": "hidden_call",
                        "code": "stone_eval_hidden_blind_surface",
                        "message": "restore, rollback, and change-inspection builtins are hidden in blind agent surface mode",
                    },
                }
            elif explicit_tx_surface_enabled() and hidden_explicit_tx_source(
                str(args.get("source", ""))
            ):
                result = {
                    "ok": False,
                    "error": {
                        "kind": "hidden_call",
                        "code": "stone_eval_hidden_explicit_tx_surface",
                        "message": "attempt builtins are hidden in explicit transaction agent surface mode",
                    },
                }
            else:
                result = self.backend.eval(str(args.get("source", "")), args.get("cwd"))
                result = apply_large_result_policy(
                    result, allow_large_output=bool(args.get("allow_large_output"))
                )
        elif name == "stone_help":
            result = stone_help(self.backend, args.get("name"))
            result = apply_large_result_policy(result)
        elif name == "stone_signature":
            result = stone_signature(str(args.get("name", "")))
        elif name == "stone_call":
            call_name = Stone_CALL_ALIASES.get(
                str(args.get("name", "")), str(args.get("name", ""))
            )
            result = stone_call(self.backend, call_name, args.get("args"), args.get("cwd"))
            result = apply_large_result_policy(
                result,
                allow_large_output=bool(args.get("allow_large_output")),
                force_allowed=call_name not in CONTROL_RESULTS_ALWAYS_BOUNDED,
            )
        elif isinstance(name, str) and name in DIRECT_ATTEMPT_TOOLS:
            if not attempt_surface_enabled():
                result = {
                    "ok": False,
                    "error": {
                        "kind": "hidden_call",
                        "code": "attempt_tool_hidden_surface",
                        "message": f"{name} is only available in attempt agent surface mode",
                    },
                }
            else:
                result = stone_call(
                    self.backend,
                    name,
                    direct_attempt_call_args(name, args if isinstance(args, dict) else {}),
                    args.get("cwd") if isinstance(args, dict) else None,
                )
                result = apply_large_result_policy(
                    result, allow_large_output=bool(args.get("allow_large_output"))
                )
        elif name == "stone_describe":
            result = stone_describe(str(args.get("path", "")), args.get("cwd"))
        elif name == "escape_linux":
            result = escape_linux(str(args.get("reason", "")), str(args.get("command", "")), args.get("cwd"))
        else:
            result = {
                "ok": False,
                "error": {
                    "kind": "unknown_tool",
                    "code": "mcp_unknown_tool",
                    "message": f"unknown tool: {name}",
                },
            }
        duration_ms = int((time.monotonic() - started) * 1000)
        result = attach_runtime_status(
            result,
            args.get("cwd") if isinstance(args, dict) else None,
            str(name) if name is not None else None,
            args if isinstance(args, dict) else {},
        )
        result = sanitize_for_agent(result)
        self.trace.record_tool_call(str(name), args if isinstance(args, dict) else {}, result, duration_ms)
        text = json.dumps(result, indent=2, sort_keys=True)
        return {"content": [{"type": "text", "text": text}], "structuredContent": result}


def json_rpc_error(request_id: Any, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def main() -> int:
    backend_name = os.environ.get("WAYMARK_STONE_MCP_BACKEND", "warm-stdio")
    if backend_name == "subprocess":
        backend: StoneBackend = SubprocessBackend(timeout_seconds=timeout_seconds_from_env())
    else:
        backend = WarmStdioBackend(
            cwd=os.environ.get("WAYMARK_STONE_CWD"),
            timeout_seconds=timeout_seconds_from_env(),
        )
    trace = TraceRecorder(os.environ.get("WAYMARK_STONE_MCP_TRACE"))
    try:
        McpServer(backend, sys.stdin.buffer, sys.stdout.buffer, sys.stderr, trace).serve()
    finally:
        if isinstance(backend, WarmStdioBackend):
            backend.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
