#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
import json
import io
import os
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import stone_mcp_server as server


class FakeBackend(server.StoneBackend):
    def __init__(self, result: dict | None = None) -> None:
        self.calls: list[tuple[str, str | None]] = []
        self.result = result or {"ok": True, "value": {"name": "waymark"}}

    def eval(self, source: str, cwd: str | None = None) -> dict:
        self.calls.append((source, cwd))
        return dict(self.result)


class StoneMcpServerTests(unittest.TestCase):
    def test_stone_eval_tool_description_mentions_session_bindings(self) -> None:
        description = next(
            tool["description"] for tool in server.TOOLS if tool["name"] == "stone_eval"
        )

        self.assertIn("long-lived Stone session", description)
        self.assertIn("bindings persist across stone_eval calls", description)
        self.assertIn("multi-line script", description)
        self.assertIn("allow_large_output=true", description)
        self.assertIn("Open file handles do not persist", description)

    def test_normalizes_successful_stone_json_response(self) -> None:
        proc = subprocess.CompletedProcess(
            ["waymark"],
            0,
            stdout='{"cwd":"/workspace","ok":true,"output":{"stderr":"","stdout":"hi\\n"},"value":"hi"}\n',
            stderr="",
        )

        result = server.normalize_stone_process_result(proc, 1024, 7)

        self.assertEqual(
            result,
            {
                "ok": True,
                "value": "hi",
                "stdout": "hi\n",
                "diagnostics": {"exit_code": 0, "duration_ms": 7, "backend": "subprocess"},
            },
        )

    def test_normalizes_subprocess_session_diagnostics(self) -> None:
        proc = subprocess.CompletedProcess(
            ["waymark"],
            0,
            stdout='{"cwd":"/workspace","ok":true,"diagnostics":{"session":{"bound":["rows"]}},"output":{"stderr":"","stdout":""},"value":null}\n',
            stderr="",
        )

        result = server.normalize_stone_process_result(proc, 1024, 7)

        self.assertEqual(result["ok"], True)
        self.assertEqual(result["diagnostics"]["session"], {"bound": ["rows"]})

    def test_normalizes_stone_error_from_stderr(self) -> None:
        proc = subprocess.CompletedProcess(
            ["waymark"],
            1,
            stdout="",
            stderr='{"ok":false,"error":{"kind":"generic","code":"stone_script_error","detail":"bad"}}\n',
        )

        result = server.normalize_stone_process_result(proc, 1024, 3)

        self.assertFalse(result["ok"])
        self.assertEqual(result["error"]["kind"], "generic")
        self.assertEqual(result["error"]["message"], "bad")

    def test_normalizes_warm_task_frame(self) -> None:
        frame = {
            "version": 0,
            "type": "result",
            "id": "stone-mcp-1",
            "result": {
                "ok": True,
                "value": 3,
                "output": {"stdout": "3\n", "stderr": ""},
                "diagnostics": {
                    "hot_loop": {"jsonl_fused_traces_executed": 1},
                    "session": {"bound": ["rows"]},
                },
            },
            "reset": {"ok": True},
        }

        result = server.normalize_task_frame_result(frame, 1024, 11)

        self.assertEqual(result["ok"], True)
        self.assertEqual(result["value"], 3)
        self.assertEqual(result["stdout"], "3\n")
        self.assertEqual(result["diagnostics"]["backend"], "warm_stdio")
        self.assertEqual(
            result["diagnostics"]["hot_loop"], {"jsonl_fused_traces_executed": 1}
        )
        self.assertEqual(result["diagnostics"]["session"], {"bound": ["rows"]})

    def test_large_eval_result_is_replaced_with_peek(self) -> None:
        backend = FakeBackend({"ok": True, "value": list(range(30)), "diagnostics": {"backend": "fake"}})
        mcp = server.McpServer(backend, io.BytesIO(), io.BytesIO(), io.StringIO())

        response = mcp.call_tool(
            {"name": "stone_eval", "arguments": {"source": "emit(rows)"}}
        )
        result = response["structuredContent"]

        self.assertEqual(result["ok"], True)
        self.assertEqual(result["value"]["type"], "list")
        self.assertEqual(result["value"]["len"], 30)
        self.assertEqual(result["value"]["head"], [0, 1, 2, 3, 4])
        self.assertEqual(result["value"]["tail"], [25, 26, 27, 28, 29])
        self.assertEqual(result["diagnostics"]["large_output"]["policy"], "preview")
        self.assertIn("allow_large_output=true", result["diagnostics"]["large_output"]["message"])

    def test_large_eval_result_can_be_forced(self) -> None:
        values = list(range(30))
        backend = FakeBackend({"ok": True, "value": values})
        mcp = server.McpServer(backend, io.BytesIO(), io.BytesIO(), io.StringIO())

        response = mcp.call_tool(
            {
                "name": "stone_eval",
                "arguments": {"source": "emit(rows)", "allow_large_output": True},
            }
        )

        self.assertEqual(response["structuredContent"]["value"], values)
        self.assertNotIn("large_output", response["structuredContent"].get("diagnostics", {}))

    def test_large_stone_call_result_is_replaced_with_peek(self) -> None:
        backend = FakeBackend({"ok": True, "value": list(range(30))})
        mcp = server.McpServer(backend, io.BytesIO(), io.BytesIO(), io.StringIO())

        response = mcp.call_tool(
            {"name": "stone_call", "arguments": {"name": "read_csv", "args": {"path": "x.csv"}}}
        )

        self.assertEqual(response["structuredContent"]["value"]["type"], "list")
        self.assertEqual(response["structuredContent"]["value"]["len"], 30)
        self.assertIn("large_output", response["structuredContent"]["diagnostics"])

    def test_read_frame_times_out_without_complete_frame(self) -> None:
        read_fd, write_fd = os.pipe()
        try:
            with os.fdopen(read_fd, "rb", closefd=False) as reader:
                with self.assertRaises(TimeoutError):
                    server.read_frame(reader, timeout_seconds=0.001)
        finally:
            os.close(read_fd)
            os.close(write_fd)

    def test_transport_timeout_error_explains_scope_and_partial_state(self) -> None:
        result = server.stone_transport_timeout_error(
            timeout_seconds=12.5,
            duration_ms=12501,
            backend="warm_stdio",
            stdout="partial",
            stderr="still cloning",
            max_output_bytes=1024,
        )

        self.assertFalse(result["ok"])
        self.assertEqual(result["error"]["code"], "stone_timeout")
        self.assertIn("Stone transport timed out", result["error"]["message"])
        self.assertIn("MCP/host timeout", result["error"]["detail"])
        self.assertEqual(result["error"]["timeout_ms"], 12500)
        self.assertIn("partial checkout", result["error"]["next_steps"][0])
        self.assertEqual(result["stdout"], "partial")
        self.assertEqual(result["stderr"], "still cloning")
        self.assertEqual(result["diagnostics"]["backend"], "warm_stdio")

    def test_stone_describe_reports_symlink_target(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "target.txt").write_text("hello\n", encoding="utf-8")
            os.symlink("target.txt", root / "link.txt")

            result = server.stone_describe("link.txt", cwd=str(root))

        self.assertTrue(result["ok"])
        self.assertEqual(result["value"]["symlink_target"], "target.txt")
        self.assertEqual(result["value"]["preview"], "hello\n")

    def test_jsonl_infer_schema_is_always_list_shaped(self) -> None:
        one_line = server.infer_schema("jsonl", '{"name": "waymark"}\n')
        two_lines = server.infer_schema(
            "jsonl", '{"name": "waymark"}\n{"count": 2}\n'
        )

        self.assertEqual(one_line, [{"name": "str"}])
        self.assertEqual(two_lines, [{"name": "str"}, {"count": "int"}])

    def test_compact_run_explanation_preserves_timeout_fields(self) -> None:
        compact = server.compact_run_explanation(
            {
                "kind": "timeout",
                "summary": "Stone stopped waiting",
                "scope": "external process",
                "timeout_ms": 20,
                "duration_ms": 21,
                "argv": ["sleep", "1"],
                "command": "sleep 1",
                "next_steps": ["Inspect partial output"],
            }
        )

        self.assertEqual(compact["kind"], "timeout")
        self.assertEqual(compact["timeout_ms"], 20)
        self.assertEqual(compact["duration_ms"], 21)
        self.assertEqual(compact["argv"], ["sleep", "1"])
        self.assertEqual(compact["command"], "sleep 1")

    def test_compact_run_explanation_preserves_python_package_fields(self) -> None:
        compact = server.compact_run_explanation(
            {
                "kind": "python_module_attribute_missing",
                "summary": "Python package API mismatch",
                "module": "transformers",
                "attribute": "AutoModelForVision2Seq",
                "package": "transformers",
                "inspect_argv": [
                    ["python3", "-m", "pip", "show", "transformers"],
                    ["python3", "-m", "pip", "check"],
                ],
                "next_steps": ["Inspect installed package metadata"],
            }
        )

        self.assertEqual(compact["module"], "transformers")
        self.assertEqual(compact["attribute"], "AutoModelForVision2Seq")
        self.assertEqual(compact["package"], "transformers")
        self.assertEqual(
            compact["inspect_argv"][0],
            ["python3", "-m", "pip", "show", "transformers"],
        )

    def test_compact_run_explanation_preserves_pip_conflict_fields(self) -> None:
        compact = server.compact_run_explanation(
            {
                "kind": "python_dependency_conflict",
                "summary": "dependency conflict",
                "dependent": "demo 1.0",
                "requirement": "dep<2",
                "installed": "dep 3.0",
                "requested": ["alpha==1"],
                "evidence": "ERROR: ResolutionImpossible",
                "inspect_argv": [["python3", "-m", "pip", "check"]],
            }
        )

        self.assertEqual(compact["dependent"], "demo 1.0")
        self.assertEqual(compact["requirement"], "dep<2")
        self.assertEqual(compact["installed"], "dep 3.0")
        self.assertEqual(compact["requested"], ["alpha==1"])
        self.assertEqual(compact["evidence"], "ERROR: ResolutionImpossible")

    def test_timeout_seconds_from_env_parses_positive_value(self) -> None:
        old = os.environ.get("WAYMARK_STONE_TIMEOUT_SECONDS")
        os.environ["WAYMARK_STONE_TIMEOUT_SECONDS"] = "1.5"
        try:
            self.assertEqual(server.timeout_seconds_from_env(), 1.5)
            os.environ["WAYMARK_STONE_TIMEOUT_SECONDS"] = "0"
            self.assertEqual(server.timeout_seconds_from_env(), server.DEFAULT_TIMEOUT_SECONDS)
        finally:
            if old is None:
                os.environ.pop("WAYMARK_STONE_TIMEOUT_SECONDS", None)
            else:
                os.environ["WAYMARK_STONE_TIMEOUT_SECONDS"] = old

    def test_subprocess_backend_sets_start_dir_for_durable_writes(self) -> None:
        waymark_bin = (
            Path(__file__).resolve().parents[2]
            / "target"
            / "x86_64-unknown-linux-musl"
            / "release"
            / "waymark"
        )
        if not waymark_bin.is_file():
            self.skipTest("musl waymark binary is not built")

        with tempfile.TemporaryDirectory() as tmp:
            backend = server.SubprocessBackend(str(waymark_bin), timeout_seconds=10.0)
            result = backend.eval('open("hello.txt", "w").write("Hello, world!\\n")\n', tmp)

            self.assertTrue(result["ok"], result)
            self.assertEqual((Path(tmp) / "hello.txt").read_text(), "Hello, world!\n")

    def test_warm_backend_preserves_work_dir_between_evals(self) -> None:
        waymark_bin = (
            Path(__file__).resolve().parents[2]
            / "target"
            / "x86_64-unknown-linux-musl"
            / "release"
            / "waymark"
        )
        if not waymark_bin.is_file():
            self.skipTest("musl waymark binary is not built")

        with tempfile.TemporaryDirectory() as tmp:
            backend = server.WarmStdioBackend(str(waymark_bin), cwd=tmp, timeout_seconds=10.0)
            try:
                write = backend.eval('open("hello.txt", "w").write("Hello, world!\\n")\n', tmp)
                read = backend.eval('emit(cat("hello.txt"))\n', tmp)
            finally:
                backend.close()

            self.assertTrue(write["ok"], write)
            self.assertTrue(read["ok"], read)
            self.assertEqual(read["value"], "Hello, world!\n")
            self.assertEqual((Path(tmp) / "hello.txt").read_text(), "Hello, world!\n")
            self.assertFalse(write["diagnostics"]["reset"]["work"])

    def test_help_delegates_to_shell_help(self) -> None:
        backend = FakeBackend({"ok": True, "value": {"language": "Stone"}})

        all_help = server.stone_help(backend)
        read_file = server.stone_help(backend, "read_file")

        self.assertTrue(all_help["ok"])
        self.assertEqual(all_help["value"]["language"], "Stone")
        self.assertEqual(
            backend.calls,
            [
                ("emit(help())\n", None),
                ('emit(help("read_file"))\n', None),
            ],
        )
        self.assertTrue(read_file["ok"])

    def test_help_enriches_builtin_with_stone_call_signature(self) -> None:
        backend = FakeBackend({"ok": True, "value": {"name": "run", "found": True}})

        result = server.stone_help(backend, "run")

        self.assertTrue(result["ok"], result)
        self.assertEqual(result["value"]["call_form"], "stone_call")
        self.assertEqual(result["value"]["args"][0]["name"], "argv")
        self.assertEqual(result["value"]["args"][0]["type"], "list[str]")
        self.assertEqual(result["value"]["example_call"], {"name": "run", "args": [["cargo", "test"]]})

    def test_stone_signature_returns_stone_call_example(self) -> None:
        result = server.stone_signature("run")

        self.assertTrue(result["ok"], result)
        self.assertEqual(result["value"]["call_form"], "stone_call")
        self.assertEqual(result["value"]["example_call"], {"name": "run", "args": [["cargo", "test"]]})
        self.assertIn({"name": "run", "args": ["cargo", "test"]}, result["value"]["also_accepts"])

    def test_stone_signature_includes_must_run(self) -> None:
        result = server.stone_signature("must_run")

        self.assertTrue(result["ok"], result)
        self.assertEqual(result["value"]["args"][0]["name"], "argv")
        self.assertEqual(result["value"]["example_call"], {"name": "must_run", "args": [["printf", "ok"]]})

    def test_stone_signature_includes_recent_system_helpers(self) -> None:
        ps = server.stone_signature("ps")
        sysinfo = server.stone_signature("sys")
        wait_for = server.stone_signature("wait_for")

        self.assertTrue(ps["ok"], ps)
        self.assertEqual(ps["value"]["name"], "ps")
        self.assertEqual(ps["value"]["args"][0]["name"], "interval_ms")
        self.assertTrue(sysinfo["ok"], sysinfo)
        self.assertEqual(sysinfo["value"]["name"], "sysinfo")
        self.assertEqual(sysinfo["value"]["alias_for"], "sysinfo")
        self.assertEqual(sysinfo["value"]["args"][0]["name"], "section")
        self.assertTrue(wait_for["ok"], wait_for)
        self.assertEqual(wait_for["value"]["call_form"], "stone_eval")

    def test_stone_call_generates_emit_wrapped_builtin_call(self) -> None:
        backend = FakeBackend()

        result = server.stone_call(backend, "read_json", {"path": "package.json"}, "/repo")

        self.assertTrue(result["ok"])
        self.assertEqual(result["effects"], {"reads": ["package.json"]})
        self.assertEqual(backend.calls, [('emit(read_json("/repo/package.json"))\n', None)])

    def test_stone_call_supports_file_aliases(self) -> None:
        backend = FakeBackend()

        write = server.stone_call(
            backend,
            "write_file",
            {"path": "hello.txt", "text": "Hello\n", "append": False},
            "/repo",
        )
        read = server.stone_call(backend, "read_file", {"path": "hello.txt"}, "/repo")
        alias_write = server.stone_call(backend, "write", {"path": "alias.txt", "content": "ok"}, "/repo")

        self.assertTrue(write["ok"])
        self.assertTrue(read["ok"])
        self.assertTrue(alias_write["ok"])
        self.assertEqual(write["effects"], {"writes": ["hello.txt"], "creates_parent_dirs": True})
        self.assertEqual(read["effects"], {"reads": ["hello.txt"]})
        self.assertEqual(alias_write["effects"], {"writes": ["alias.txt"], "creates_parent_dirs": True})
        self.assertEqual(
            backend.calls,
            [
                ('emit(write_file("/repo/hello.txt", "Hello\\n", False))\n', None),
                ('emit(read_file("/repo/hello.txt"))\n', None),
                ('emit(write_file("/repo/alias.txt", "ok"))\n', None),
            ],
        )

    def test_stone_call_supports_run_builtin(self) -> None:
        backend = FakeBackend({"ok": True, "value": {"exit_code": 0, "stdout": "ok\n"}})

        result = server.stone_call(
            backend,
            "run",
            {"argv": ["python3", "-c", "print('ok')"], "cwd": "/app", "timeout_ms": 1000},
            "/app",
        )

        self.assertTrue(result["ok"], result)
        self.assertEqual(result["effects"], {"unknown": True})
        self.assertEqual(
            backend.calls,
            [
                (
                    'emit(run(["python3", "-c", "print(\'ok\')"], cwd="/app", timeout_ms=1000))\n',
                    None,
                )
            ],
        )

    def test_stone_call_supports_run_output_controls(self) -> None:
        source = server.stone_call_source(
            "run",
            {
                "argv": ["bash", "-lc", "make noisy"],
                "cwd": "/app",
                "stdout": "suppress",
                "stderr": "capture",
                "max_stderr_bytes": 12000,
            },
        )

        self.assertEqual(
            source,
            'emit(run(["bash", "-lc", "make noisy"], cwd="/app", stdout="suppress", stderr="capture", max_stderr_bytes=12000))\n',
        )

    def test_stone_call_supports_positional_run_convenience(self) -> None:
        source = server.stone_call_source("run", [["bash", "-lc", "cat"], "/app", "input", 1000])

        self.assertEqual(
            source,
            'emit(run(["bash", "-lc", "cat"], cwd="/app", stdin="input", timeout_ms=1000))\n',
        )

    def test_stone_call_treats_direct_run_string_array_as_argv(self) -> None:
        backend = FakeBackend()
        argv = [
            "aws",
            "s3api",
            "create-bucket",
            "--bucket",
            "sample-bucket",
            "--region",
            "us-west-2",
            "--create-bucket-configuration",
            "LocationConstraint=us-west-2",
            "--debug",
        ]

        result = server.stone_call(backend, "run", argv)

        self.assertTrue(result["ok"], result)
        self.assertEqual(
            backend.calls,
            [
                (
                    'emit(run(["aws", "s3api", "create-bucket", "--bucket", "sample-bucket", "--region", "us-west-2", "--create-bucket-configuration", "LocationConstraint=us-west-2", "--debug"]))\n',
                    None,
                )
            ],
        )

    def test_stone_call_supports_must_run_direct_argv(self) -> None:
        backend = FakeBackend()

        result = server.stone_call(backend, "must_run", ["cargo", "check"])

        self.assertTrue(result["ok"], result)
        self.assertEqual(backend.calls, [('emit(must_run(["cargo", "check"]))\n', None)])

    def test_stone_call_supports_daemon_helpers(self) -> None:
        daemon = server.stone_call_source(
            "start_daemon",
            {
                "argv": ["python3", "-m", "http.server", "8888"],
                "cwd": "/app",
                "stderr": "server.err",
            },
        )
        status = server.stone_call_source(
            "daemon_status",
            {"daemon": {"pid": 123}, "port": 8888, "log": "server.err"},
        )
        wait = server.stone_call_source(
            "wait_port",
            {"port": 8888, "protocol": "udp", "timeout_ms": 30000},
        )
        stop = server.stone_call_source("stop_daemon", {"daemon": 123, "timeout_ms": 1000})

        self.assertEqual(
            daemon,
            'emit(start_daemon(["python3", "-m", "http.server", "8888"], cwd="/app", stderr="server.err"))\n',
        )
        self.assertEqual(
            status,
            'emit(daemon_status({"pid": 123}, port=8888, log="server.err"))\n',
        )
        self.assertEqual(wait, 'emit(wait_port(8888, protocol="udp", timeout_ms=30000))\n')
        self.assertEqual(stop, "emit(stop_daemon(123, timeout_ms=1000))\n")

    def test_stone_call_supports_resolve_command(self) -> None:
        source = server.stone_call_source("resolve_command", {"name": "python3"})

        self.assertEqual(source, 'emit(resolve_command("python3"))\n')

    def test_stone_call_supports_recent_system_helpers(self) -> None:
        ps = server.stone_call_source("ps", {"interval_ms": 0})
        sysinfo = server.stone_call_source("sysinfo", {"section": "os"})
        backend = FakeBackend()
        sys_alias = server.stone_call(backend, "sys", {"section": "mem"})

        self.assertEqual(ps, "emit(ps(0))\n")
        self.assertEqual(sysinfo, 'emit(sysinfo("os"))\n')
        self.assertTrue(sys_alias["ok"], sys_alias)
        self.assertEqual(backend.calls, [('emit(sysinfo("mem"))\n', None)])

    def test_stone_call_accepts_json_string_args(self) -> None:
        backend = FakeBackend()

        result = server.stone_call(backend, "find", '{"root": ".", "name_glob": "*.jsonl"}', "/repo")

        self.assertTrue(result["ok"], result)
        self.assertEqual(backend.calls, [('emit(find("/repo", "*.jsonl"))\n', None)])

    def test_stone_call_accepts_common_agent_alias_args(self) -> None:
        backend = FakeBackend()

        found = server.stone_call(
            backend,
            "find",
            {"path": "/app", "name": "*.py", "glob": "src/**/*.py", "max_depth": 3},
            "/app",
        )
        listed = server.stone_call(backend, "list", '{"path":"/app"}', "/app")
        searched = server.stone_call(backend, "search", {"path": "/app", "query": "foo"}, "/app")
        alias_read = server.stone_call(backend, "read", {"path": "/app/input.txt"}, "/app")
        read = server.stone_call(
            backend,
            "read_file",
            {"path": "/app/big.txt", "limit": 500, "start_line": 2, "end_line": 4},
            "/app",
        )
        written = server.stone_call(
            backend,
            "write_file",
            {"path": "/app/out.txt", "content": "ok"},
            "/app",
        )
        made_dir = server.stone_call(backend, "mkdir", {"path": "/app/out"}, "/app")
        edited = server.stone_call(
            backend,
            "edit_file",
            {"path": "/app/fix.py", "old": "pass\n", "new": "return 1\n"},
            "/app",
        )
        removed = server.stone_call(backend, "delete_file", {"path": "/app/tmp.txt"}, "/app")
        removed_dir = server.stone_call(backend, "delete_dir", {"path": "/app/tmp-dir"}, "/app")
        removed_paths = server.stone_call(
            backend,
            "rm",
            {"paths": ["/app/a.tmp", "/app/b.tmp"], "force": True},
            "/app",
        )
        statted = server.stone_call(backend, "stat", {"path": "/app/results.txt"}, "/app")

        self.assertTrue(found["ok"], found)
        self.assertTrue(listed["ok"], listed)
        self.assertTrue(searched["ok"], searched)
        self.assertTrue(alias_read["ok"], alias_read)
        self.assertTrue(read["ok"], read)
        self.assertTrue(written["ok"], written)
        self.assertTrue(made_dir["ok"], made_dir)
        self.assertTrue(edited["ok"], edited)
        self.assertTrue(removed["ok"], removed)
        self.assertTrue(removed_dir["ok"], removed_dir)
        self.assertTrue(removed_paths["ok"], removed_paths)
        self.assertTrue(statted["ok"], statted)
        self.assertEqual(edited["effects"], {"reads": ["/app/fix.py"], "writes": ["/app/fix.py"]})
        self.assertEqual(removed["effects"], {"removes": ["/app/tmp.txt"]})
        self.assertEqual(removed_dir["effects"], {"removes": ["/app/tmp-dir"]})
        self.assertEqual(removed_paths["effects"], {"removes": ["/app/a.tmp", "/app/b.tmp"]})
        self.assertEqual(statted["effects"], {"reads": ["/app/results.txt"]})
        self.assertEqual(
            backend.calls,
            [
                ('emit(find("/app", "*.py", max_depth=3, path_glob="src/**/*.py"))\n', None),
                ('emit(ls("/app"))\n', None),
                ('emit(search("/app", "foo"))\n', None),
                ('emit(read_file("/app/input.txt"))\n', None),
                ('emit(read_file("/app/big.txt", 500, start_line=2, end_line=4))\n', None),
                ('emit(write_file("/app/out.txt", "ok"))\n', None),
                ('emit(mkdir("/app/out"))\n', None),
                ('emit(edit("/app/fix.py", "pass\\n", "return 1\\n"))\n', None),
                ('emit(rm("/app/tmp.txt"))\n', None),
                ('emit(rm("/app/tmp-dir"))\n', None),
                ('emit(rm(["/app/a.tmp", "/app/b.tmp"], force=True))\n', None),
                ('emit(stat("/app/results.txt"))\n', None),
            ],
        )

    def test_stone_call_normalizes_common_run_agent_args(self) -> None:
        backend = FakeBackend()

        result = server.stone_call(
            backend,
            "run",
            {
                "command": ["pytest", "-q"],
                "max_output_bytes": 12000,
            },
        )

        self.assertTrue(result["ok"], result)
        self.assertEqual(
            backend.calls,
            [('emit(run(["pytest", "-q"], max_stdout_bytes=12000, max_stderr_bytes=12000))\n', None)],
        )

    def test_stone_call_rejects_string_run_argv_with_hint(self) -> None:
        result = server.stone_call(FakeBackend(), "run", {"argv": "pytest"}, "/app")

        self.assertFalse(result["ok"])
        self.assertEqual(result["error"]["code"], "stone_call_invalid_args")
        self.assertIn('run argv must be a list of strings', result["error"]["message"])
        self.assertIn('run(["pytest"])', result["error"]["message"])

    def test_stone_call_help_delegates_to_stone_help(self) -> None:
        backend = FakeBackend()

        result = server.stone_call(backend, "help", {"name": "save"})
        alias_result = server.stone_call(backend, "stone_help", "{}")

        self.assertTrue(result["ok"], result)
        self.assertTrue(alias_result["ok"], alias_result)
        self.assertEqual(
            backend.calls,
            [('emit(help("save"))\n', None), ("emit(help())\n", None)],
        )

    def test_stone_call_maps_late_ordered_args_to_keywords(self) -> None:
        source = server.stone_call_source("read_csv", {"limit": 5, "path": "input.csv"})

        self.assertEqual(source, 'emit(read_csv("input.csv", 5))\n')

    def test_stone_call_renders_nested_stone_literals(self) -> None:
        source = server.stone_call_source(
            "write_json",
            {"path": "out.json", "value": {"ok": True, "items": [1, None, "x"]}},
        )

        self.assertEqual(
            source,
            'emit(write_json("out.json", {"ok": True, "items": [1, None, "x"]}))\n',
        )

    def test_stone_call_rejects_unknown_builtin(self) -> None:
        result = server.stone_call(FakeBackend(), "eval", {})

        self.assertFalse(result["ok"])
        self.assertEqual(result["error"]["code"], "stone_call_unknown")

    def test_stone_call_supports_help_table_builtins_that_are_simple_calls(self) -> None:
        unsupported_by_design = {"open", "wait_for"}
        missing = set(server.HELP_TABLE) - set(server.Stone_CALL_ARG_ORDER) - unsupported_by_design

        self.assertEqual(missing, set())

    def test_stone_call_resolves_path_args_for_warm_reset_backend(self) -> None:
        args = server.stone_call_resolved_args("read_jsonl", {"path": "events.jsonl", "limit": 2}, "/repo")

        self.assertEqual(args, {"path": "/repo/events.jsonl", "limit": 2})

    def test_describe_json_file_infers_schema(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "package.json").write_text('{"name":"waymark","scripts":{"test":"true"}}\n')

            result = server.stone_describe("package.json", str(root))

        self.assertTrue(result["ok"])
        self.assertEqual(result["value"]["kind"], "json")
        self.assertEqual(result["value"]["schema"]["name"], "str")
        self.assertEqual(result["effects"]["reads"], ["package.json"])

    def test_escape_linux_requires_reason(self) -> None:
        result = server.escape_linux("", "true")

        self.assertFalse(result["ok"])
        self.assertEqual(result["error"]["code"], "escape_linux_reason_required")

    def test_escape_linux_runs_command_with_bounded_result(self) -> None:
        result = server.escape_linux("need direct shell for a smoke check", "printf hello")

        self.assertTrue(result["ok"])
        self.assertEqual(result["stdout"], "hello")
        self.assertEqual(result["gap"], "unsupported/out_of_scope")
        self.assertEqual(result["effects"], {"unknown": True})

    def test_trace_recorder_writes_escape_accounting_record(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            trace_path = Path(tmp) / "trace.jsonl"
            recorder = server.TraceRecorder(str(trace_path))

            recorder.record_tool_call(
                "escape_linux",
                {
                    "reason": "need cargo test; Stone has no test runner",
                    "command": "cargo test",
                    "cwd": "/repo",
                },
                {"ok": True, "gap": "process/test_runner", "stdout": "ok\n"},
                12,
            )

            records = [json.loads(line) for line in trace_path.read_text().splitlines()]

        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["tool"], "escape_linux")
        self.assertEqual(records[0]["cwd"], "/repo")
        self.assertEqual(records[0]["gap"], "process/test_runner")
        self.assertEqual(records[0]["escape"]["reason"], "need cargo test; Stone has no test runner")
        self.assertEqual(records[0]["escape"]["command"], "cargo test")

    def test_trace_recorder_writes_stone_call_record(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            trace_path = Path(tmp) / "trace.jsonl"
            recorder = server.TraceRecorder(str(trace_path))

            recorder.record_tool_call(
                "stone_call",
                {"name": "read_json", "args": {"path": "package.json"}},
                {
                    "ok": True,
                    "effects": {"reads": ["package.json"]},
                    "diagnostics": {
                        "backend": "warm_stdio",
                        "hot_loop": {"jsonl_fused_traces_executed": 1},
                    },
                },
                4,
            )

            record = json.loads(trace_path.read_text())

        self.assertEqual(record["tool"], "stone_call")
        self.assertEqual(record["effects"], {"reads": ["package.json"]})
        self.assertEqual(
            record["stone"],
            {
                "backend": "warm_stdio",
                "call": "read_json",
                "hot_loop": {"jsonl_fused_traces_executed": 1},
            },
        )

    def test_trace_recorder_writes_bounded_run_result_details(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            trace_path = Path(tmp) / "trace.jsonl"
            recorder = server.TraceRecorder(str(trace_path))

            recorder.record_tool_call(
                "stone_call",
                {"name": "run", "args": {"argv": ["python", "-m", "pip", "install", "x"]}},
                {
                    "ok": True,
                    "diagnostics": {"backend": "warm_stdio"},
                    "value": {
                        "ok": False,
                        "kind": "exec_failed",
                        "exit_code": 7,
                        "stdout": "",
                        "stderr": "connection failed\n",
                        "runtime": {
                            "kind": "python",
                            "command_name": "python",
                            "resolved_executable": "/app/.venv/bin/python",
                            "python_executable": "/app/.venv/bin/python",
                            "pip_available": False,
                            "cwd_project_markers": ["/app/pyproject.toml"],
                        },
                        "explanation": {
                            "kind": "external_process_exit",
                            "summary": "Stone ran the process but it exited with code 7.",
                            "scope": "external process; Stone transport succeeded",
                        },
                        "helpers": [
                            {
                                "helper": "python.after_failure",
                                "event": "run.after_failure",
                                "family": "python/pip",
                                "kind": "python_package_resolution_failed",
                                "summary": "pip could not resolve packages",
                                "source": "/app/.stone/helpers/python.stone",
                                "evidence": {"stderr_excerpt": "connection failed\n"},
                                "next_checks": [["python", "-m", "pip", "check"]],
                            }
                        ],
                    },
                },
                15,
            )

            record = json.loads(trace_path.read_text())

        self.assertEqual(record["stone"]["call"], "run")
        self.assertEqual(record["stone"]["run"]["argv"], ["python", "-m", "pip", "install", "x"])
        self.assertEqual(record["stone"]["run"]["exit_code"], 7)
        self.assertEqual(record["stone"]["run"]["stderr"], "connection failed\n")
        self.assertEqual(
            record["stone"]["run"]["runtime"]["resolved_executable"], "/app/.venv/bin/python"
        )
        self.assertFalse(record["stone"]["run"]["runtime"]["pip_available"])
        self.assertEqual(record["stone"]["run"]["explanation"]["kind"], "external_process_exit")
        self.assertEqual(record["stone"]["run"]["helpers"][0]["helper"], "python.after_failure")
        self.assertEqual(
            record["stone"]["run"]["helpers"][0]["kind"], "python_package_resolution_failed"
        )
        self.assertEqual(
            record["stone"]["run"]["helpers"][0]["next_checks"][0],
            ["python", "-m", "pip", "check"],
        )

    def test_parse_hot_loop_diagnostics_from_stderr(self) -> None:
        stderr = (
            'noise\nWAYMARK_STONE_HOT_LOOP_DIAGNOSTICS {"jsonl_fused_traces_executed":1,'
            '"loop_candidates":2}\n'
        )

        self.assertEqual(
            server.parse_hot_loop_diagnostics(stderr),
            {"jsonl_fused_traces_executed": 1, "loop_candidates": 2},
        )

    def test_stone_call_supports_runtime_state_builtins(self) -> None:
        backend = FakeBackend({"ok": True, "value": {"cwd": "/repo"}})

        result = server.stone_call(backend, "state", {})

        self.assertTrue(result["ok"])
        self.assertEqual(backend.calls[0], ("emit(state())\n", None))

        server.stone_call(backend, "last_result", {})
        self.assertEqual(backend.calls[1], ("emit(last_result())\n", None))


if __name__ == "__main__":
    unittest.main()
