#!/usr/bin/env python3
"""Exercise the visible Stone bounded-exploration control from one checkpoint."""

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
REJECTED = "deserts"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--library",
        type=Path,
        default=ROOT / "examples/scripts/bounded_attempt_explore.stone",
    )
    parser.add_argument(
        "--specialization",
        type=Path,
        default=ROOT / "examples/references/bounded_attempt_explore_canary.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=180.0)
    return parser.parse_args()


def composed_source(library: Path, specialization: Path) -> str:
    return (
        library.read_text(encoding="utf-8").rstrip()
        + "\n\n"
        + specialization.read_text(encoding="utf-8").lstrip()
    )


def assert_result(payload: dict[str, Any]) -> tuple[str, str]:
    if payload.get("ok") is not True:
        raise AssertionError(f"bounded exploration controller failed: {payload}")
    value = payload.get("value") or {}
    if value.get("answer") != EXPECTED or value.get("tried") != 2:
        raise AssertionError(f"bounded exploration did not recover: {value}")
    if value.get("clean") is not True:
        raise AssertionError(f"exploration scope did not close cleanly: {value}")
    if value.get("parent_keys") != ["requirement.explore_target"]:
        raise AssertionError(f"candidate memory leaked into the parent: {value}")
    if (value.get("proposal") or {}).get("candidate") != REJECTED:
        raise AssertionError(f"fixture did not propose the losing candidate: {value}")
    if not str(value.get("checkpoint") or "").startswith("cp-"):
        raise AssertionError(f"semantic checkpoint was not reported: {value}")

    outcomes = value.get("outcomes") or []
    if len(outcomes) != 2:
        raise AssertionError(f"expected two sequential candidate outcomes: {value}")
    rejected, accepted = outcomes
    if rejected.get("candidate") != REJECTED or rejected.get("status") != "rejected":
        raise AssertionError(f"first candidate was not rejected: {rejected}")
    if rejected.get("evidence") != []:
        raise AssertionError(f"rejected candidate claimed evidence: {rejected}")
    if accepted.get("candidate") != EXPECTED or accepted.get("status") != "accepted":
        raise AssertionError(f"fallback candidate was not accepted: {accepted}")
    if accepted.get("evidence") != ["canary:reversal"]:
        raise AssertionError(f"accepted candidate lacks fresh evidence: {accepted}")
    for outcome in outcomes:
        result = outcome.get("result") or {}
        if result.get("problem") != PROBLEM:
            raise AssertionError(f"candidate missed checkpoint workspace: {outcome}")
        if result.get("memory_revision_after_result") != 2:
            raise AssertionError(f"candidate missed checkpoint memory: {outcome}")

    accepted_attempt = str(value.get("accepted_attempt") or "")
    rejected_attempt = str(rejected.get("attempt") or "")
    if accepted_attempt != str(accepted.get("attempt") or ""):
        raise AssertionError(f"accepted attempt identity is inconsistent: {value}")
    if not accepted_attempt or not rejected_attempt:
        raise AssertionError(f"candidate attempt identities are missing: {value}")
    return accepted_attempt, rejected_attempt


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.library = args.library.resolve()
    args.specialization = args.specialization.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("exploration library", args.library),
        ("canary specialization", args.specialization),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")

    source = composed_source(args.library, args.specialization)
    with tempfile.TemporaryDirectory(
        prefix="waymark-bounded-explore-", dir="/tmp"
    ) as root_text:
        root = Path(root_text)
        data_root = root / "gateway-data"
        workspace_source = root / "workspace"
        socket_path = root / "gateway.sock"
        workspace_source.mkdir()
        (workspace_source / "README.md").write_text(
            "bounded exploration fixture base\n", encoding="utf-8"
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
        accepted_attempt = ""
        rejected_attempt = ""
        try:
            base.wait_for_socket(socket_path, server)
            spawn_source = "emit(attempt_spawn(" + ",".join(
                [
                    (
                        'task_spec={"id":"bounded-explore-smoke",'
                        '"objective":"reject one candidate and accept a fallback"}'
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
                accepted_attempt, rejected_attempt = assert_result(payload)
            except AssertionError as error:
                raise AssertionError(f"{error}\ncontroller logs:\n{logs}") from error

            for label, child in (
                ("accepted", accepted_attempt),
                ("rejected", rejected_attempt),
            ):
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
                    raise AssertionError(f"{label} child was not reclaimed: {info}")

            trace = (
                data_root / "traces" / "operations.jsonl"
            ).read_text(encoding="utf-8")
            if trace.count('"op":"attempt.fork"') != 2:
                raise AssertionError("trace does not contain exactly two candidate forks")
            if trace.count('"op":"attempt.accept"') != 1:
                raise AssertionError("trace does not contain exactly one acceptance")
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
                    "bounded exploration smoke cleanup",
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
                f"bounded exploration smoke left transactions: {open_transactions}"
            )

    print(
        json.dumps(
            {
                "ok": True,
                "library": str(args.library),
                "specialization": str(args.specialization),
                "accepted_attempt": accepted_attempt,
                "rejected_attempt": rejected_attempt,
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
