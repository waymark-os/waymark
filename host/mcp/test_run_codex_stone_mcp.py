#!/usr/bin/env python3

from __future__ import annotations

import argparse
import contextlib
import io
import json
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import run_codex_stone_mcp as runner


class RunCodexStoneMcpTests(unittest.TestCase):
    def test_default_prompt_mentions_session_bindings(self) -> None:
        self.assertIn("bindings across stone_eval calls", runner.DEFAULT_PROMPT)
        self.assertIn("reuse names later", runner.DEFAULT_PROMPT)
        self.assertIn("multi-line script", runner.DEFAULT_PROMPT)
        self.assertIn("allow_large_output=true", runner.DEFAULT_PROMPT)
        self.assertIn("Open file handles do not persist", runner.DEFAULT_PROMPT)

    def test_codex_exec_command_includes_workspace_and_prompt(self) -> None:
        args = argparse.Namespace(
            codex="codex",
            workspace=Path("/repo"),
            model="gpt-5.5",
            json_events=True,
            codex_sandbox="bypass",
            prompt="inspect",
        )

        command = runner.codex_exec_command(args)

        self.assertEqual(command[:4], ["codex", "exec", "--cd", "/repo"])
        self.assertIn("--skip-git-repo-check", command)
        self.assertIn("--dangerously-bypass-approvals-and-sandbox", command)
        self.assertNotIn("--ask-for-approval", command)
        self.assertIn("--json", command)
        self.assertIn("--model", command)
        self.assertEqual(command[-1], "inspect")

    def test_codex_exec_command_can_use_read_only_sandbox(self) -> None:
        args = argparse.Namespace(
            codex="codex",
            workspace=Path("/repo"),
            model=None,
            json_events=False,
            codex_sandbox="read-only",
            prompt="inspect",
        )

        command = runner.codex_exec_command(args)

        self.assertIn("--sandbox", command)
        self.assertEqual(command[command.index("--sandbox") + 1], "read-only")
        self.assertNotIn("--ask-for-approval", command)
        self.assertNotIn("--dangerously-bypass-approvals-and-sandbox", command)

    def test_prepare_config_uses_workspace_as_default_stone_cwd_compat(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            waymark_bin = tmp_path / "waymark"
            waymark_bin.write_text("binary")
            workspace = tmp_path / "workspace"
            workspace.mkdir()
            args = argparse.Namespace(
                workspace=workspace,
                trace=None,
                model=None,
                reasoning_effort=None,
                server=Path("/waymark/host/mcp/stone_mcp_server.py"),
                waymark_bin=waymark_bin,
                stone_cwd_compat=None,
                timeout_seconds=7.0,
            )

            config = runner.prepare_config(args, tmp_path / "codex-home").read_text()

        self.assertIn(f'WAYMARK_STONE_CWD = "{workspace}"', config)
        self.assertIn('WAYMARK_STONE_TIMEOUT_SECONDS = "7.0"', config)
        self.assertIn('WAYMARK_STONE_HOT_LOOP = "1"', config)
        self.assertIn('WAYMARK_STONE_HOT_LOOP_VM = "1"', config)
        self.assertIn("stone-mcp-trace.jsonl", config)

    def test_cli_dry_run_prints_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            waymark_bin = tmp_path / "waymark"
            waymark_bin.write_text("binary")
            workspace = tmp_path / "workspace"
            workspace.mkdir()
            codex_home = tmp_path / "codex-home"

            completed = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).resolve().parent / "run_codex_stone_mcp.py"),
                    "--workspace",
                    str(workspace),
                    "--codex-home",
                    str(codex_home),
                    "--waymark-bin",
                    str(waymark_bin),
                    "--server",
                    "/waymark/host/mcp/stone_mcp_server.py",
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )

            summary = json.loads(completed.stdout)
            config_exists = (codex_home / "config.toml").exists()

        self.assertTrue(summary["ok"])
        self.assertEqual(summary["codex_home"], str(codex_home))
        self.assertTrue(config_exists)

    def test_check_mode_captures_mcp_list_output(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            waymark_bin = tmp_path / "waymark"
            waymark_bin.write_text("binary")
            workspace = tmp_path / "workspace"
            workspace.mkdir()
            args = argparse.Namespace(
                workspace=workspace,
                codex="codex",
                codex_home=tmp_path / "codex-home",
                server=Path("/waymark/host/mcp/stone_mcp_server.py"),
                waymark_bin=waymark_bin,
                stone_cwd_compat=None,
                trace=None,
                timeout_seconds=180.0,
                model=None,
                reasoning_effort=None,
                check=True,
                exec=False,
                json_events=False,
                codex_sandbox="bypass",
                prompt="inspect",
            )

            with mock.patch.object(
                runner,
                "run_checked",
                return_value=subprocess.CompletedProcess(["codex"], 0, "stone enabled\n", "warn\n"),
            ):
                stdout = io.StringIO()
                with contextlib.redirect_stdout(stdout):
                    exit_code = runner.run_with_home(args, args.codex_home)

        self.assertEqual(exit_code, 0)
        summary = json.loads(stdout.getvalue())
        self.assertTrue(summary["ok"])
        self.assertEqual(summary["mcp_list_stdout"], "stone enabled\n")
        self.assertEqual(summary["mcp_list_stderr"], "warn\n")


if __name__ == "__main__":
    unittest.main()
