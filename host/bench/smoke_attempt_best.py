#!/usr/bin/env python3
"""Exercise Stone's runtime-owned best-candidate lifecycle against Gateway."""

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


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--program",
        type=Path,
        default=ROOT / "examples/references/attempt_best_canary.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=180.0)
    return parser.parse_args()


def assert_result(payload: dict[str, Any]) -> tuple[str, list[str]]:
    if payload.get("ok") is not True:
        raise AssertionError(f"best-candidate controller failed: {payload}")
    value = payload.get("value") or {}
    children = [str(item) for item in value.get("children") or []]
    decisions = value.get("decisions") or []
    if (
        value.get("answer") != "beta"
        or value.get("score") != 0.9
        or value.get("status") != "accepted"
        or value.get("considered") != 3
        or value.get("replacements") != 1
        or value.get("released_outcome") is not True
        or value.get("clean") is not True
        or len(children) != 3
        or len(decisions) != 3
    ):
        raise AssertionError(f"best-candidate result has the wrong shape: {value}")
    if value.get("winner") != children[1]:
        raise AssertionError(f"middle candidate was not retained: {value}")
    expected = [
        (True, children[0], None),
        (True, children[1], children[0]),
        (False, children[1], children[2]),
    ]
    observed = [
        (
            decision.get("selected"),
            decision.get("best_attempt"),
            decision.get("discarded_attempt"),
        )
        for decision in decisions
    ]
    if observed != expected:
        raise AssertionError(f"replacement/rejection decisions changed: {value}")
    return children[1], children


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.program = args.program.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("Stone program", args.program),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")

    source = args.program.read_text(encoding="utf-8")
    with tempfile.TemporaryDirectory(prefix="waymark-attempt-best-", dir="/tmp") as root_text:
        root = Path(root_text)
        data_root = root / "gateway-data"
        workspace_source = root / "workspace"
        socket_path = root / "gateway.sock"
        workspace_source.mkdir()
        (workspace_source / "README.md").write_text(
            "attempt best fixture base\n", encoding="utf-8"
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
        client_env = {**server_env, "WAYMARK_GATEWAY_TX": bootstrap_tx}
        attempt = ""
        winner = ""
        children: list[str] = []
        try:
            base.wait_for_socket(socket_path, server)
            spawn_source = "emit(attempt_spawn(" + ",".join(
                [
                    'task_spec={"id":"attempt-best-smoke","objective":"retain the highest-scored isolated candidate"}',
                    'task_input={}',
                    'workspace_source={"workspace":"repo"}',
                    "program=" + json.dumps(
                        {"kind": "stone", "source": source}, separators=(",", ":")
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
            winner, children = assert_result(payload)
            for child in children:
                info = smoke.parse_info(
                    smoke.gateway(
                        args.gateway_bin,
                        data_root,
                        "attempt",
                        "info",
                        child,
                        env=client_env,
                    ).stdout
                )
                if info.get("state") != "rolled_back":
                    raise AssertionError(f"candidate resources were not reclaimed: {info}")
            trace = (data_root / "traces" / "operations.jsonl").read_text(
                encoding="utf-8"
            )
            if trace.count('"op":"attempt.fork"') != 3:
                raise AssertionError("trace does not contain exactly three candidate forks")
            if trace.count('"op":"attempt.accept"') != 1:
                raise AssertionError("trace does not contain exactly one candidate acceptance")
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
                    "attempt best smoke cleanup",
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
            raise AssertionError(f"attempt best smoke left transactions: {open_transactions}")

    print(
        json.dumps(
            {
                "ok": True,
                "program": str(args.program),
                "winner": winner,
                "children": children,
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
