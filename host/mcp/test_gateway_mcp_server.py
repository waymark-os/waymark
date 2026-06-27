#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import gateway_mcp_server as server


class FakeRpc:
    def __init__(self) -> None:
        self.calls: list[tuple[str, tuple[str, ...]]] = []

    def call(self, method: str, args: list[str]) -> dict[str, object]:
        self.calls.append((method, tuple(args)))
        if method == "env.snapshot":
            return {"ok": True, "stdout": "tx\ttx-source\n", "stderr": "", "exit_code": 0}
        if method == "env.checkpoint":
            return {"ok": True, "stdout": "checkpoint\tcp-dry\n", "stderr": "", "exit_code": 0}
        if method == "env.run_checkpoint":
            rolled_back = "--keep-tx" not in args
            return {
                "ok": True,
                "stdout": f"tx\ttx-branch\nrolled_back\t{str(rolled_back).lower()}\n",
                "stderr": "",
                "exit_code": 0,
            }
        if method == "env.rollback":
            return {"ok": True, "stdout": "rolled_back\n", "stderr": "", "exit_code": 0}
        if method == "env.checkpoint_discard":
            return {"ok": True, "stdout": "discarded\n", "stderr": "", "exit_code": 0}
        raise AssertionError(f"unexpected RPC call: {method} {args!r}")


class GatewayMcpTests(unittest.TestCase):
    def test_stone_call_workspace_dry_run_uses_checkpoint_branch(self) -> None:
        rpc = FakeRpc()
        mcp = server.GatewayMcp(rpc)  # type: ignore[arg-type]

        result = mcp.stone_call(
            {
                "workspace": "repo",
                "dry_run": True,
                "image": "alpine:latest",
                "argv": ["sh", "-c", "true"],
            }
        )

        self.assertTrue(result["ok"], result)
        self.assertTrue(result["dry_run"])
        self.assertEqual(result["checkpoint"], "cp-dry")
        self.assertEqual(result["source_tx"], "tx-source")
        self.assertEqual(result["branch_tx"], "tx-branch")
        self.assertTrue(result["rolled_back"])
        self.assertFalse(result["retained"])
        self.assertEqual(
            rpc.calls,
            [
                ("env.snapshot", ("--workspace", "repo")),
                ("env.checkpoint", ("--tx", "tx-source", "--reason", "stone_call dry-run baseline")),
                (
                    "env.run_checkpoint",
                    (
                        "--checkpoint",
                        "cp-dry",
                        "--image",
                        "alpine:latest",
                        "--workspace-mount",
                        "/app",
                        "--",
                        "sh",
                        "-c",
                        "true",
                    ),
                ),
                ("env.rollback", ("--tx", "tx-source")),
                ("env.checkpoint_discard", ("--checkpoint", "cp-dry", "--force")),
            ],
        )

    def test_stone_call_workspace_dry_run_can_retain_branch(self) -> None:
        rpc = FakeRpc()
        mcp = server.GatewayMcp(rpc)  # type: ignore[arg-type]

        result = mcp.stone_call(
            {
                "workspace": "repo",
                "dry_run": True,
                "keep_tx": True,
                "image": "alpine:latest",
                "argv": ["sh", "-c", "true"],
            }
        )

        self.assertTrue(result["ok"], result)
        self.assertEqual(result["branch_tx"], "tx-branch")
        self.assertFalse(result["rolled_back"])
        self.assertTrue(result["retained"])
        self.assertIn("--keep-tx", rpc.calls[2][1])
        self.assertNotIn(("env.checkpoint_discard", ("--checkpoint", "cp-dry", "--force")), rpc.calls)

    def test_trace_call_records_checkpoint_branch_lifecycle(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp) / "trace.jsonl"
            mcp = server.GatewayMcp(FakeRpc(), trace)  # type: ignore[arg-type]

            mcp.trace_call(
                "stone_call",
                {"workspace": "repo", "dry_run": True, "argv": ["true"]},
                {
                    "ok": True,
                    "exit_code": 0,
                    "dry_run": True,
                    "checkpoint": "cp-dry",
                    "source_tx": "tx-source",
                    "branch_tx": "tx-branch",
                    "rolled_back": True,
                    "retained": False,
                    "source_rollback": {"ok": True, "stdout": "rolled_back\n"},
                    "checkpoint_cleanup": {"ok": True, "stdout": "discarded\n"},
                },
                17,
            )

            record = json.loads(trace.read_text().strip())

        self.assertEqual(record["tool"], "stone_call")
        self.assertTrue(record["dry_run"])
        self.assertEqual(record["checkpoint"], "cp-dry")
        self.assertEqual(record["source_tx"], "tx-source")
        self.assertEqual(record["branch_tx"], "tx-branch")
        self.assertTrue(record["rolled_back"])
        self.assertFalse(record["retained"])
        self.assertTrue(record["source_rollback_ok"])
        self.assertTrue(record["checkpoint_cleanup_ok"])


if __name__ == "__main__":
    unittest.main()
