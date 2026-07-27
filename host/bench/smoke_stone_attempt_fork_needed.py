#!/usr/bin/env python3
"""Exercise a task whose in-progress frontier makes attempt_fork necessary."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base
import smoke_stone_default_attempt_agent as smoke


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
PROBLEM = "stressed"
EXPECTED = "desserts"
CANDIDATES = ("deserts", EXPECTED)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--source",
        type=Path,
        default=ROOT / "examples/scripts/attempt_fork_needed_canary.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=120.0)
    return parser.parse_args()


def assert_result(payload: dict[str, Any]) -> tuple[str, str]:
    if payload.get("ok") is not True:
        raise AssertionError(f"fork-required controller failed: {payload}")
    value = payload.get("value") or {}
    if value.get("answer") != EXPECTED or value.get("accepted") != value.get("winner"):
        raise AssertionError(f"winning branch was not accepted: {value}")
    if value.get("clean") is not True:
        raise AssertionError(f"fork scope did not close cleanly: {value}")
    if value.get("parent_keys") != ["requirement.fork_target"]:
        raise AssertionError(f"child memory leaked into the parent: {value}")
    results = value.get("results") or []
    if len(results) != 2:
        raise AssertionError(f"expected two isolated candidate results: {value}")
    by_candidate = {
        (entry.get("result") or {}).get("candidate"): entry
        for entry in results
        if isinstance(entry, dict)
    }
    winner = by_candidate.get(EXPECTED) or {}
    loser = by_candidate.get(CANDIDATES[0]) or {}
    if (winner.get("result") or {}).get("passed") is not True:
        raise AssertionError(f"expected candidate did not pass: {value}")
    if (loser.get("result") or {}).get("passed") is not False:
        raise AssertionError(f"losing candidate did not remain isolated: {value}")
    for entry in results:
        result = entry.get("result") or {}
        if result.get("problem") != PROBLEM:
            raise AssertionError(f"child missed the uncommitted problem file: {entry}")
        if result.get("inherited_target") != EXPECTED:
            raise AssertionError(f"child missed inherited hot memory: {entry}")
        if result.get("memory_keys") != [
            "candidate.result",
            "requirement.fork_target",
        ]:
            raise AssertionError(f"child memory branch is malformed: {entry}")
        if entry.get("fork_memory_revision") != 1:
            raise AssertionError(f"child forked from the wrong memory frontier: {entry}")
    return str(value.get("winner") or ""), str(value.get("loser") or "")


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.source = args.source.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("Stone source", args.source),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")

    with tempfile.TemporaryDirectory(prefix="waymark-fork-needed-", dir="/tmp") as root_text:
        root = Path(root_text)
        data_root = root / "gateway-data"
        workspace_source = root / "workspace"
        socket_path = root / "gateway.sock"
        workspace_source.mkdir()
        (workspace_source / "README.md").write_text(
            "fork-required fixture base\n", encoding="utf-8"
        )
        smoke.gateway(
            args.gateway_bin,
            data_root,
            "repo",
            "snapshot",
            "--name",
            "repo",
            "--path",
            str(workspace_source),
        )
        bootstrap_tx = smoke.gateway(
            args.gateway_bin,
            data_root,
            "env",
            "snapshot",
            "--workspace",
            "repo",
        ).stdout.strip()

        server_env = {
            **os.environ,
            "WAYMARK_STONE_BIN": str(args.waymark_bin),
            "WAYMARK_GATEWAY_SOCKET": str(socket_path),
            "WAYMARK_GATEWAY_IMAGE": args.image,
            "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
        }
        server = subprocess.Popen(
            [
                str(args.gateway_bin),
                "--data-root",
                str(data_root),
                "rpc",
                "serve",
                "--socket",
                str(socket_path),
            ],
            env=server_env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        client_env = {
            **server_env,
            "WAYMARK_GATEWAY_TX": bootstrap_tx,
        }
        attempt = ""
        try:
            base.wait_for_socket(socket_path, server)
            task_input = {
                "problem": PROBLEM,
                "expected": EXPECTED,
                "candidates": list(CANDIDATES),
            }
            spawn_source = "emit(attempt_spawn(" + ",".join(
                [
                    'task_spec={"id":"fork-needed-smoke","objective":"select one isolated candidate from an in-progress frontier"}',
                    "task_input=" + json.dumps(task_input, separators=(",", ":")),
                    'workspace_source={"workspace":"repo"}',
                    "program="
                    + json.dumps(
                        {
                            "kind": "stone",
                            "source": args.source.read_text(encoding="utf-8"),
                        },
                        separators=(",", ":"),
                    ),
                    'entrypoint="main"',
                    'controller="stone"',
                    'workspace_mount="/app"',
                ]
            ) + "))"
            spawned = base.run_capture(
                [str(args.waymark_bin), "eval", "-c", spawn_source],
                env=client_env,
                timeout=30,
            )
            spawn_payload = base.response_payload(spawned)
            if not isinstance(spawn_payload, dict) or spawn_payload.get("ok") is not True:
                raise AssertionError(
                    f"attempt_spawn failed: {spawned.stdout}{spawned.stderr}"
                )
            attempt = str((spawn_payload.get("value") or {}).get("attempt") or "")
            if not attempt:
                raise AssertionError(f"attempt_spawn returned no attempt: {spawn_payload}")

            smoke.gateway(
                args.gateway_bin,
                data_root,
                "attempt",
                "start",
                attempt,
                "--wait",
                "--timeout-ms",
                str(int(args.timeout * 1000)),
                env=client_env,
                timeout=args.timeout + 30,
            )
            logs = smoke.gateway(
                args.gateway_bin,
                data_root,
                "attempt",
                "logs",
                attempt,
                "--max-bytes",
                "524288",
                env=client_env,
            ).stdout
            payload = base.response_payload(subprocess.CompletedProcess([], 0, logs, ""))
            if not isinstance(payload, dict):
                raise AssertionError(f"controller logs had no Stone response: {logs}")
            winner, loser = assert_result(payload)
            winner_info = smoke.parse_info(
                smoke.gateway(
                    args.gateway_bin,
                    data_root,
                    "attempt",
                    "info",
                    winner,
                    env=client_env,
                ).stdout
            )
            loser_info = smoke.parse_info(
                smoke.gateway(
                    args.gateway_bin,
                    data_root,
                    "attempt",
                    "info",
                    loser,
                    env=client_env,
                ).stdout
            )
            if winner_info.get("state") != "rolled_back":
                raise AssertionError(f"accepted child was not reclaimed: {winner_info}")
            if loser_info.get("state") != "rolled_back":
                raise AssertionError(f"discarded child was not reclaimed: {loser_info}")
            trace = (
                data_root / "traces" / "operations.jsonl"
            ).read_text(encoding="utf-8")
            if trace.count('"op":"attempt.fork"') != 2:
                raise AssertionError("trace does not contain exactly two forks")
            if '"op":"attempt.accept"' not in trace:
                raise AssertionError("trace lacks attempt.accept")
            if (
                '"op":"attempt.finish"' not in trace
                or '"reason":"fork canary losing candidate"' not in trace
            ):
                raise AssertionError("trace lacks the losing-branch discard reason")
        finally:
            if attempt:
                smoke.gateway(
                    args.gateway_bin,
                    data_root,
                    "attempt",
                    "finish",
                    attempt,
                    "--rollback",
                    "--reason",
                    "fork-required smoke cleanup",
                    env=client_env,
                )
            smoke.gateway(
                args.gateway_bin,
                data_root,
                "env",
                "rollback",
                bootstrap_tx,
                env=client_env,
            )
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)

        open_transactions = smoke.gateway(
            args.gateway_bin,
            data_root,
            "env",
            "list-tx",
            env=client_env,
        ).stdout.strip()
        if open_transactions:
            raise AssertionError(f"fork-required smoke left transactions: {open_transactions}")

    print(
        json.dumps(
            {
                "ok": True,
                "source": str(args.source),
                "problem": PROBLEM,
                "expected": EXPECTED,
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
