#!/usr/bin/env python3
"""Exercise Stone fork inheritance, branch isolation, and reference-only accept."""

from __future__ import annotations

import argparse
import json
import os
import re
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
        "--source",
        type=Path,
        default=ROOT / "examples/scripts/attempt_memory_fork_canary.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=120.0)
    return parser.parse_args()


def assert_result(
    payload: dict[str, Any],
    parent_info: dict[str, Any],
    child_info: dict[str, Any],
) -> None:
    if payload.get("ok") is not True:
        raise AssertionError(f"Stone controller failed: {payload}")
    value = payload.get("value") or {}
    child = str(value.get("child") or "")
    parent_memory_ref = str(value.get("parent_memory_ref") or "")
    child_memory_ref = f"attempt-memory:{child}"
    if not child:
        raise AssertionError(f"controller returned no child: {value}")
    if value.get("fork_memory_ref") != parent_memory_ref:
        raise AssertionError(f"fork did not record the parent memory ref: {value}")
    if value.get("fork_memory_revision") != 1:
        raise AssertionError(f"child did not fork from parent revision 1: {value}")
    if value.get("parent_memory_revision") != 3:
        raise AssertionError(f"unexpected parent memory revision: {value}")
    if value.get("parent_keys_before_promotion") != [
        "parent.after",
        "shared.parent",
    ]:
        raise AssertionError(f"child memory leaked into parent: {value}")
    if value.get("parent_keys_after_promotion") != [
        "candidate.accepted",
        "parent.after",
        "shared.parent",
    ]:
        raise AssertionError(f"explicit parent promotion failed: {value}")
    observation = value.get("child_observation") or {}
    if observation.get("inherited_value") != 1:
        raise AssertionError(f"child did not observe inherited memory: {value}")
    if observation.get("child_keys") != ["candidate.child", "shared.parent"]:
        raise AssertionError(f"child branch shape is wrong: {value}")
    if value.get("accepted_child_memory_ref") != child_memory_ref:
        raise AssertionError(f"accepted child memory ref was not retained: {value}")
    if value.get("accepted_child_memory_revision") != "2":
        raise AssertionError(f"accepted child revision was not retained: {value}")
    if value.get("accept_memory_policy") != "reference_only":
        raise AssertionError(f"accept policy is not explicit: {value}")

    if parent_info.get("memory_revision") != "3":
        raise AssertionError(f"parent info changed memory unexpectedly: {parent_info}")
    if child_info.get("memory_revision") != "2":
        raise AssertionError(f"child info lost its memory revision: {child_info}")
    if child_info.get("fork_memory_ref") != parent_memory_ref:
        raise AssertionError(f"typed fork origin lost memory ref: {child_info}")
    if child_info.get("fork_memory_revision") != "1":
        raise AssertionError(f"typed fork origin lost memory revision: {child_info}")
    if child_info.get("state") != "rolled_back":
        raise AssertionError(f"accepted child was not closed: {child_info}")


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

    with tempfile.TemporaryDirectory(prefix="waymark-memory-fork-", dir="/tmp") as root_text:
        root = Path(root_text)
        data_root = root / "gateway-data"
        workspace_source = root / "workspace"
        socket_path = root / "gateway.sock"
        workspace_source.mkdir()
        (workspace_source / "README.md").write_text(
            "attempt memory fork smoke fixture\n", encoding="utf-8"
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

        server_env = dict(os.environ)
        server_env.update(
            {
                "WAYMARK_STONE_BIN": str(args.waymark_bin),
                "WAYMARK_GATEWAY_SOCKET": str(socket_path),
                "WAYMARK_GATEWAY_IMAGE": args.image,
                "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
            }
        )
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
        attempt = ""
        client_env = dict(os.environ)
        client_env.update(
            {
                "WAYMARK_STONE_BIN": str(args.waymark_bin),
                "WAYMARK_GATEWAY_SOCKET": str(socket_path),
                "WAYMARK_GATEWAY_TX": bootstrap_tx,
                "WAYMARK_GATEWAY_IMAGE": args.image,
                "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
            }
        )
        try:
            base.wait_for_socket(socket_path, server)
            spawn_source = "emit(attempt_spawn(" + ",".join(
                [
                    'task_spec={"id":"attempt-memory-fork-smoke","objective":"verify fork memory isolation"}',
                    "task_input={}",
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
                raise AssertionError(f"attempt_spawn failed: {spawned.stdout}{spawned.stderr}")
            attempt = str((spawn_payload.get("value") or {}).get("attempt") or "")
            if not attempt:
                raise AssertionError(f"attempt_spawn returned no attempt id: {spawn_payload}")

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
                raise AssertionError(f"controller logs contained no Stone response: {logs}")
            value = payload.get("value") or {}
            child = str(value.get("child") or "")
            if not child:
                related_attempts = [
                    candidate
                    for candidate in dict.fromkeys(
                        re.findall(r"attempt-\d+-\d+", json.dumps(payload))
                    )
                    if candidate != attempt
                ]
                child_logs = {}
                for candidate in related_attempts[:3]:
                    completed = smoke.gateway(
                        args.gateway_bin,
                        data_root,
                        "attempt",
                        "logs",
                        candidate,
                        "--max-bytes",
                        "131072",
                        env=client_env,
                        check=False,
                    )
                    child_logs[candidate] = completed.stdout + completed.stderr
                raise AssertionError(
                    f"controller returned no child: {payload}; child logs: {child_logs}"
                )
            parent_info = smoke.parse_info(
                smoke.gateway(
                    args.gateway_bin,
                    data_root,
                    "attempt",
                    "info",
                    attempt,
                    env=client_env,
                ).stdout
            )
            child_info = smoke.parse_info(
                smoke.gateway(
                    args.gateway_bin,
                    data_root,
                    "attempt",
                    "info",
                    child,
                    env=client_env,
                ).stdout
            )
            assert_result(payload, parent_info, child_info)
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
                    "attempt memory fork smoke cleanup",
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
            args.gateway_bin, data_root, "env", "list-tx", env=client_env
        ).stdout.strip()
        if open_transactions:
            raise AssertionError(f"smoke left open transactions: {open_transactions}")

    print(json.dumps({"ok": True, "source": str(args.source)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
