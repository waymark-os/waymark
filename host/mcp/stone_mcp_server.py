#!/usr/bin/env python3
"""Small MCP stdio server for host-side Stone evaluation."""

from __future__ import annotations

import abc
import csv
import json
import os
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
        "signature": "find(root: str, name_glob: str = '*') -> list[record]",
        "effects": ["read_dir"],
        "example": 'files = find(".", "*.jsonl")',
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
        "signature": "read_file(path: str, max_bytes: int? = None) -> str",
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
        "signature": "read_text(path: str, max_bytes: int? = None) -> str",
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
        "signature": 'run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576) -> record',
        "effects": ["process", "unknown"],
        "example": 'result = run(["cargo", "test"], cwd=".", stdout="suppress", stderr="capture", max_stderr_bytes=12000)',
    },
    "resolve_command": {
        "name": "resolve_command",
        "signature": "resolve_command(name: str) -> record",
        "effects": ["read_env", "read_file"],
        "example": 'info = resolve_command("python3")',
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
        "signature": 'wait_port(port: int, host: str = "127.0.0.1", timeout_ms: int = 30000) -> record',
        "effects": ["network"],
        "example": "ready = wait_port(8888, timeout_ms=30000)",
    },
    "stat": {
        "name": "stat",
        "signature": "stat(path: str, follow_symlinks: bool = False) -> record",
        "effects": ["read_file"],
        "example": 'info = stat("results.txt")',
    },
    "rm": {
        "name": "rm",
        "signature": "rm(path: str, ...paths: str) -> None",
        "effects": ["remove_file"],
        "example": 'rm("tmp.txt")',
    },
}

Stone_CALL_ARG_ORDER: dict[str, tuple[str, ...]] = {
    "cat": ("path",),
    "edit": ("path", "old", "new", "all"),
    "edit_file": ("path", "old", "new", "all"),
    "find": ("root", "name_glob"),
    "json_dumps": ("value",),
    "json_loads": ("text",),
    "list": ("path",),
    "list_dir": ("path",),
    "ls": ("path",),
    "mkdir": ("path",),
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
    "resolve_command": ("name",),
    "rm": ("path",),
    "start_daemon": ("argv", "cwd", "env", "stdout", "stderr"),
    "daemon_status": ("daemon", "port", "host", "log", "max_log_bytes"),
    "stop_daemon": ("daemon", "timeout_ms"),
    "wait_port": ("port", "host", "timeout_ms"),
    "search": ("root", "needle"),
    "stat": ("path", "follow_symlinks"),
    "write_file": ("path", "text", "append"),
    "write_json": ("path", "value"),
    "write_jsonl": ("path", "rows"),
    "write_text": ("path", "text", "append"),
}

Stone_CALL_ALIASES: dict[str, str] = {
    "delete_dir": "rm",
    "delete_directory": "rm",
    "delete_file": "rm",
    "edit_file": "edit",
    "stone_help": "help",
    "list": "ls",
    "list_builtins": "help",
    "remove_dir": "rm",
    "remove_directory": "rm",
    "read": "read_file",
    "remove_file": "rm",
    "write": "write_file",
}

Stone_ONE_POSITIONAL_THEN_KEYWORDS = {
    "run",
    "start_daemon",
    "daemon_status",
    "stop_daemon",
    "wait_port",
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
        trace.update(
            {
                "ok": value.get("ok"),
                "kind": value.get("kind"),
                "exit_code": value.get("exit_code"),
                "stdout": bound_text(value.get("stdout"), 2048),
                "stderr": bound_text(value.get("stderr"), 2048),
                "timed_out": value.get("timed_out"),
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
        result["diagnostics"] = {
            "exit_code": proc.returncode,
            "duration_ms": duration_ms,
            "backend": "subprocess",
            "hot_loop": parse_hot_loop_diagnostics(stderr),
        }
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
    result: dict[str, Any] = {
        "ok": bool(payload.get("ok")),
        "stdout": bound_text(output.get("stdout") if isinstance(output, dict) else "", max_output_bytes),
        "stderr": bound_text(output.get("stderr") if isinstance(output, dict) else "", max_output_bytes),
        "diagnostics": {
            "duration_ms": duration_ms,
            "backend": "warm_stdio",
            "reset": compact_reset(frame.get("reset")),
            "hot_loop": hot_loop,
        },
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
    source = "emit(help())\n" if not name else f"emit(help({stone_literal(name)}))\n"
    result = backend.eval(source)
    if result.get("ok") is True:
        return result
    return {
        **result,
        "error": {
            **(result.get("error") if isinstance(result.get("error"), dict) else {}),
            "hint": "stone_help delegates to shell help(); try stone_eval with help() for details.",
        },
    }


def stone_call(backend: StoneBackend, name: str, args: Any, cwd: str | None = None) -> dict[str, Any]:
    name = Stone_CALL_ALIASES.get(name, name)
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
        if name in {"run", "start_daemon"} and "argv" not in normalized and "command" in normalized:
            normalized["argv"] = normalized.pop("command")
        if name == "run":
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
        return normalized
    return args


def stone_call_source(name: str, args: Any) -> str:
    positional, named = stone_call_arguments(name, args)
    rendered_args = [stone_literal(value) for value in positional]
    rendered_args.extend(f"{key}={stone_literal(value)}" for key, value in named.items())
    return f"emit({name}({', '.join(rendered_args)}))\n"


def stone_call_resolved_args(name: str, args: Any, cwd: str | None) -> Any:
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
    return args


def stone_call_arguments(name: str, args: Any) -> tuple[list[Any], dict[str, Any]]:
    order = Stone_CALL_ARG_ORDER[name]
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
        if name in {"run", "start_daemon"} and isinstance(args[first_key], str):
            raise ValueError(
                f"{name} argv must be a list of strings, not a string; "
                f'use {{"argv": ["{args[first_key]}"]}} or use stone_eval with {name}(["{args[first_key]}"])'
            )
        return [args[first_key]], named

    if isinstance(args, list):
        if len(args) > len(order):
            raise ValueError(f"{name} accepts at most {len(order)} positional arguments")
        if name in {"run", "start_daemon"} and args and isinstance(args[0], str):
            raise ValueError(
                f"{name} argv must be a list of strings, not a string; "
                f'use [["{args[0]}"]] for positional stone_call args or {name}(["{args[0]}"]) in stone_eval'
            )
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


def stone_describe(path: str, cwd: str | None = None) -> dict[str, Any]:
    target = resolve_workspace_path(path, cwd)
    display_path = path
    if not target.exists():
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
    if target.is_dir():
        entries = sorted(child.name for child in target.iterdir())[:100]
        value.update({"kind": "directory", "entries": entries})
        return {"ok": True, "value": value, "effects": effects}
    if target.is_symlink():
        value["symlink_target"] = os.readlink(target)

    data = target.read_bytes()[:MAX_PREVIEW_BYTES]
    text = data.decode("utf-8", errors="replace")
    kind = infer_kind(target, data)
    value["kind"] = kind
    if kind != "binary":
        value["preview"] = text
    if stat.st_size > len(data):
        value["preview_truncated"] = True

    schema = infer_schema(kind, text)
    if schema:
        value["schema"] = schema
    return sparse({"ok": True, "value": value, "effects": effects})


def resolve_workspace_path(path: str, cwd: str | None = None) -> Path:
    raw = Path(path).expanduser()
    if raw.is_absolute():
        return raw.resolve()
    return (resolve_cwd(cwd) / raw).resolve()


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
        return schemas[0] if len(schemas) == 1 else schemas or None
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
    start = time.monotonic()
    try:
        proc = subprocess.run(
            command,
            shell=True,
            cwd=str(run_cwd),
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
        "description": "Run a short Stone source string through waymark --stdin-script.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "source": {"type": "string"},
                "cwd": {"type": "string"},
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
        "name": "stone_call",
        "description": "Call a supported Stone builtin with JSON arguments.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "args": {
                    "oneOf": [
                        {"type": "object"},
                        {"type": "array"},
                    ]
                },
                "cwd": {"type": "string"},
            },
            "required": ["name", "args"],
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
                result = {"tools": TOOLS}
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
            result = self.backend.eval(str(args.get("source", "")), args.get("cwd"))
        elif name == "stone_help":
            result = stone_help(self.backend, args.get("name"))
        elif name == "stone_call":
            result = stone_call(self.backend, str(args.get("name", "")), args.get("args"), args.get("cwd"))
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
