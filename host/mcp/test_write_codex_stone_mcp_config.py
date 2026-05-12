#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import write_codex_stone_mcp_config as config_writer


class WriteCodexStoneMcpConfigTests(unittest.TestCase):
    def test_writes_codex_mcp_server_config(self) -> None:
        args = Namespace(
            model="gpt-5.5",
            reasoning_effort="medium",
            server_name="stone",
            python="python3",
            server=Path("/workspace/waymark/host/mcp/stone_mcp_server.py"),
            waymark_bin=Path("/workspace/waymark/target/debug/waymark"),
            cwd="/app",
            backend="warm-stdio",
            timeout_seconds=42.0,
            trace="/trace/stone.jsonl",
            helper_dirs="/tmp/waymark/helpers",
        )

        text = config_writer.codex_stone_mcp_config(args)

        self.assertIn('model = "gpt-5.5"', text)
        self.assertIn("[mcp_servers.stone]", text)
        self.assertIn('command = "python3"', text)
        self.assertIn('args = ["/workspace/waymark/host/mcp/stone_mcp_server.py"]', text)
        self.assertIn("[mcp_servers.stone.env]", text)
        self.assertIn('WAYMARK_STONE_BIN = "/workspace/waymark/target/debug/waymark"', text)
        self.assertIn('WAYMARK_STONE_CWD = "/app"', text)
        self.assertIn('WAYMARK_STONE_MCP_BACKEND = "warm-stdio"', text)
        self.assertIn('WAYMARK_STONE_TIMEOUT_SECONDS = "42.0"', text)
        self.assertIn('WAYMARK_STONE_HOT_LOOP = "1"', text)
        self.assertIn('WAYMARK_STONE_HOT_LOOP_VM = "1"', text)
        self.assertIn('WAYMARK_STONE_MCP_TRACE = "/trace/stone.jsonl"', text)
        self.assertIn('WAYMARK_STONE_HELPER_DIRS = "/tmp/waymark/helpers"', text)

    def test_toml_string_escapes_quotes_and_backslashes(self) -> None:
        self.assertEqual(
            config_writer.toml_string('a"b\\c'),
            '"a\\"b\\\\c"',
        )

    def test_cli_writes_config_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "codex" / "config.toml"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).resolve().parent / "write_codex_stone_mcp_config.py"),
                    "--out",
                    str(out),
                    "--waymark-bin",
                    "/waymark/waymark",
                    "--server",
                    "/waymark/host/mcp/stone_mcp_server.py",
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=True,
            )

            self.assertEqual(completed.stdout, f"{out}\n")
            self.assertIn("[mcp_servers.stone]", out.read_text())


if __name__ == "__main__":
    unittest.main()
