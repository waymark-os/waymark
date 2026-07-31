#!/usr/bin/env python3
"""Exercise one typed Stone branch API across both checkpoint owner modes."""

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
        "--source",
        type=Path,
        default=ROOT / "examples/scripts/semantic_frontier_canary.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument(
        "--expected-interface",
        choices=("typed", "raw", "scoped", "async"),
        default="typed",
    )
    return parser.parse_args()


def assert_result(
    payload: dict[str, Any], expected_interface: str
) -> tuple[list[str], dict[str, Any]]:
    if payload.get("ok") is not True:
        raise AssertionError(f"semantic-frontier controller failed: {payload}")
    value = payload.get("value") or {}
    if expected_interface in ("scoped", "async"):
        if value.get("frontier_status") != "released":
            raise AssertionError(f"lexical frontier was not released: {value}")
        if value.get("scope_closed") is not True:
            raise AssertionError(f"lexical attempt scope was not closed: {value}")
        if value.get("child_resource_state") != "reclaimed":
            raise AssertionError(f"scoped child resources were not reclaimed: {value}")
        child = str(value.get("child") or "")
        if not child:
            raise AssertionError(f"scoped child identity is missing: {value}")
        diagnostics = (payload.get("diagnostics") or {}).get("semantic_frontiers") or {}
        frontiers = diagnostics.get("frontiers") or []
        if (
            diagnostics.get("created") != 1
            or diagnostics.get("unused") != 0
            or len(frontiers) != 1
            or frontiers[0].get("release_origin") != "scope_exit"
        ):
            raise AssertionError(f"lexical release diagnostics are wrong: {diagnostics}")
        result = {"diagnostics": diagnostics}
        if expected_interface == "async":
            async_diagnostics = (payload.get("diagnostics") or {}).get("async") or {}
            if (
                async_diagnostics.get("lowering") != "blocking_attempt_effects"
                or async_diagnostics.get("functions_entered") != 1
                or async_diagnostics.get("awaits") != 1
            ):
                raise AssertionError(
                    f"async lowering diagnostics are wrong: {async_diagnostics}"
                )
            result["async"] = async_diagnostics
        return [child], result
    if value.get("accepted") != "retained-branch" or value.get("clean") is not True:
        raise AssertionError(f"retained branch was not accepted cleanly: {value}")
    if value.get("owner_resource_state") != "reclaimed":
        raise AssertionError(f"failed owner resources were not reclaimed: {value}")
    parent = value.get("parent") or {}
    retained = value.get("retained") or {}
    expected_type = (
        "semantic_frontier" if expected_interface == "typed" else "workflow_checkpoint"
    )
    if parent.get("type") != expected_type:
        raise AssertionError(f"parent frontier was not nominally typed: {parent}")
    if retained.get("type") != expected_type:
        raise AssertionError(f"retained frontier was not nominally typed: {retained}")
    if parent.get("availability") != "parent":
        raise AssertionError(f"parent ownership was not classified: {parent}")
    if retained.get("availability") != "retained":
        raise AssertionError(f"retained ownership was not classified: {retained}")
    for label, frontier in (("parent", parent), ("retained", retained)):
        cost = frontier.get("cost") or {}
        guidance = frontier.get("guidance") or {}
        if not isinstance(cost.get("create_duration_ms"), int):
            raise AssertionError(f"{label} frontier omitted measured seal cost: {frontier}")
        if guidance.get("level") not in {"low", "medium", "high"}:
            raise AssertionError(f"{label} frontier omitted cost guidance: {frontier}")
        if not guidance.get("code") or not guidance.get("message"):
            raise AssertionError(f"{label} frontier guidance is incomplete: {frontier}")
    attempts = value.get("attempts") or {}
    children = [str(attempts.get(name) or "") for name in ("parent", "owner", "retained")]
    if any(not attempt for attempt in children):
        raise AssertionError(f"child attempt identities are missing: {value}")

    diagnostics = (payload.get("diagnostics") or {}).get("semantic_frontiers") or {}
    if expected_interface == "typed":
        if diagnostics.get("created") != 2 or diagnostics.get("unused") != 0:
            raise AssertionError(f"frontier use diagnostics are wrong: {diagnostics}")
        frontier_diagnostics = diagnostics.get("frontiers") or []
        if sorted(item.get("availability") for item in frontier_diagnostics) != [
            "parent",
            "retained",
        ]:
            raise AssertionError(f"frontier ownership diagnostics are wrong: {diagnostics}")
        if sorted(item.get("branch_count") for item in frontier_diagnostics) != [1, 2]:
            raise AssertionError(f"frontier branch counts are wrong: {diagnostics}")
    elif diagnostics:
        raise AssertionError(f"raw arm unexpectedly constructed typed frontiers: {diagnostics}")
    summary = {
        "parent": {
            "cost": parent.get("cost"),
            "guidance": parent.get("guidance"),
        },
        "retained": {
            "cost": retained.get("cost"),
            "guidance": retained.get("guidance"),
        },
        "diagnostics": diagnostics,
    }
    async_diagnostics = (payload.get("diagnostics") or {}).get("async")
    if async_diagnostics:
        summary["async"] = async_diagnostics
    return children, summary


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.source = args.source.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("semantic-frontier source", args.source),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")

    source = args.source.read_text(encoding="utf-8")
    with tempfile.TemporaryDirectory(prefix="waymark-semantic-frontier-", dir="/tmp") as root_text:
        root = Path(root_text)
        data_root = root / "gateway-data"
        workspace_source = root / "workspace"
        socket_path = root / "gateway.sock"
        workspace_source.mkdir()
        (workspace_source / "README.md").write_text(
            "semantic frontier fixture base\n", encoding="utf-8"
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
        children: list[str] = []
        frontier_summary: dict[str, Any] = {}
        try:
            base.wait_for_socket(socket_path, server)
            spawn_source = "emit(attempt_spawn(" + ",".join(
                [
                    (
                        'task_spec={"id":"semantic-frontier-smoke",'
                        '"objective":"branch through both checkpoint owner modes"}'
                    ),
                    "task_input={}",
                    'workspace_source={"workspace":"repo"}',
                    "program="
                    + json.dumps(
                        {"kind": "stone", "source": source},
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
            try:
                children, frontier_summary = assert_result(
                    payload, args.expected_interface
                )
            except AssertionError as error:
                raise AssertionError(f"{error}\ncontroller logs:\n{logs}") from error

            for index, child in enumerate(children):
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
                expected_state = "failed" if index == 1 else "rolled_back"
                if info.get("state") != expected_state:
                    raise AssertionError(f"child was not reclaimed: {info}")
                if index == 2 and (
                    (info.get("metadata") or {}).get("workspace_source_kind")
                    != "repair-checkpoint"
                ):
                    raise AssertionError(
                        f"retained branch did not restore a repair checkpoint: {info}"
                    )
                if (
                    args.expected_interface == "typed"
                    and index == 2
                    and (info.get("metadata") or {}).get(
                        "semantic_frontier_availability"
                    )
                    != "retained"
                ):
                    raise AssertionError(
                        f"retained lowering provenance is absent: {info}"
                    )

            trace = (data_root / "traces" / "operations.jsonl").read_text(
                encoding="utf-8"
            )
            expected_trace_counts = (
                {"attempt.fork": 1, "attempt.spawn": 1, "attempt.accept": 0}
                if args.expected_interface in ("scoped", "async")
                else {"attempt.fork": 2, "attempt.spawn": 2, "attempt.accept": 1}
            )
            for operation, expected_count in expected_trace_counts.items():
                actual_count = trace.count(f'"op":"{operation}"')
                if actual_count != expected_count:
                    raise AssertionError(
                        f"trace contains {actual_count} {operation} operations; "
                        f"expected {expected_count}"
                    )
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
                    "semantic frontier smoke cleanup",
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
            raise AssertionError(
                f"semantic frontier smoke left transactions: {open_transactions}"
            )
        open_checkpoints = smoke.gateway(
            args.gateway_bin,
            data_root,
            "env",
            "list-checkpoints",
            "--workspace",
            "repo",
            env=client_env,
        ).stdout.strip()
        if open_checkpoints:
            raise AssertionError(
                f"semantic frontier smoke left checkpoints: {open_checkpoints}"
            )

    print(
        json.dumps(
            {
                "ok": True,
                "source": str(args.source),
                "expected_interface": args.expected_interface,
                "children": children,
                "frontiers": frontier_summary,
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
