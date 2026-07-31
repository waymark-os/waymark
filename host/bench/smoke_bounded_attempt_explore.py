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
    parser.add_argument(
        "--expected-origin",
        choices=("parent", "retained"),
        default="parent",
    )
    parser.add_argument(
        "--expected-status",
        choices=("accepted", "exhausted", "error"),
        default="accepted",
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


def assert_result(
    payload: dict[str, Any],
    expected_origin: str = "parent",
    expected_status: str = "accepted",
) -> tuple[str, list[str]]:
    if expected_status == "error":
        if payload.get("ok") is not False:
            raise AssertionError(f"frontier unwind fixture unexpectedly succeeded: {payload}")
        error = payload.get("error") or {}
        if error.get("declared_code") != "semantic_frontier_unwind_canary":
            raise AssertionError(f"frontier cleanup masked the primary error: {payload}")
        related = error.get("related") or []
        if any("cleanup" in str(item.get("detail") or "") for item in related):
            raise AssertionError(f"frontier cleanup failed during unwind: {payload}")
        return "", []
    if payload.get("ok") is not True:
        raise AssertionError(f"bounded exploration controller failed: {payload}")
    value = payload.get("value") or {}
    expected_answer = EXPECTED if expected_status == "accepted" else None
    if value.get("answer") != expected_answer or value.get("tried") != 2:
        raise AssertionError(f"bounded exploration returned the wrong result: {value}")
    if value.get("status") != expected_status:
        raise AssertionError(f"bounded exploration returned the wrong status: {value}")
    release = value.get("release") or {}
    if (
        release.get("status") != "released"
        or release.get("checkpoint_reclaimed") is not True
        or release.get("branches") != 2
    ):
        raise AssertionError(f"semantic frontier was not released: {value}")
    if value.get("clean") is not True:
        raise AssertionError(f"exploration scope did not close cleanly: {value}")
    if value.get("parent_keys") != ["requirement.explore_target"]:
        raise AssertionError(f"candidate memory leaked into the parent: {value}")
    if value.get("origin") != expected_origin:
        raise AssertionError(f"exploration used the wrong frontier origin: {value}")
    if value.get("source_type") != "semantic_frontier":
        raise AssertionError(f"exploration did not use the typed frontier API: {value}")
    if value.get("frontier_status") != "released":
        raise AssertionError(f"nominal frontier capability remained usable: {value}")
    if value.get("release_guard") is not True:
        raise AssertionError(f"released frontier accepted another branch: {value}")
    if (value.get("proposal") or {}).get("candidate") != REJECTED:
        raise AssertionError(f"fixture did not propose the losing candidate: {value}")
    if not str(value.get("checkpoint") or "").startswith("cp-"):
        raise AssertionError(f"semantic checkpoint was not reported: {value}")

    outcomes = value.get("outcomes") or []
    if len(outcomes) != 2:
        raise AssertionError(f"expected two sequential candidate outcomes: {value}")
    first, second = outcomes
    if first.get("candidate") != REJECTED or first.get("status") != "rejected":
        raise AssertionError(f"first candidate was not rejected: {first}")
    if first.get("evidence") != []:
        raise AssertionError(f"rejected candidate claimed evidence: {first}")
    if second.get("candidate") != EXPECTED:
        raise AssertionError(f"fallback candidate identity changed: {second}")
    if expected_status == "accepted":
        if second.get("status") != "accepted":
            raise AssertionError(f"fallback candidate was not accepted: {second}")
        if second.get("evidence") != ["canary:reversal"]:
            raise AssertionError(f"accepted candidate lacks fresh evidence: {second}")
    elif second.get("status") != "rejected" or second.get("evidence") != []:
        raise AssertionError(f"exhausted fallback was not rejected cleanly: {second}")
    for outcome in outcomes:
        result = outcome.get("result") or {}
        if result.get("problem") != PROBLEM:
            raise AssertionError(f"candidate missed checkpoint workspace: {outcome}")
        if result.get("memory_revision_after_result") != 2:
            raise AssertionError(f"candidate missed checkpoint memory: {outcome}")

    accepted_attempt = str(value.get("accepted_attempt") or "")
    rejected_attempts = [
        str(outcome.get("attempt") or "")
        for outcome in outcomes
        if outcome.get("status") == "rejected"
    ]
    if expected_status == "accepted":
        if accepted_attempt != str(second.get("attempt") or ""):
            raise AssertionError(f"accepted attempt identity is inconsistent: {value}")
        if not accepted_attempt:
            raise AssertionError(f"accepted attempt identity is missing: {value}")
    elif accepted_attempt:
        raise AssertionError(f"exhausted search reported an accepted attempt: {value}")
    if not rejected_attempts or any(not attempt for attempt in rejected_attempts):
        raise AssertionError(f"candidate attempt identities are missing: {value}")
    return accepted_attempt, rejected_attempts


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
        rejected_attempts: list[str] = []
        try:
            base.wait_for_socket(socket_path, server)
            spawn_source = "emit(attempt_spawn(" + ",".join(
                [
                    (
                        'task_spec={"id":"bounded-explore-smoke",'
                        '"objective":"reject one candidate and accept a fallback"}'
                    ),
                    "task_input="
                    + json.dumps(
                        {
                            "exhaust": args.expected_status == "exhausted",
                            "unwind": args.expected_status == "error",
                        },
                        separators=(",", ":"),
                    ),
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
                accepted_attempt, rejected_attempts = assert_result(
                    payload,
                    args.expected_origin,
                    args.expected_status,
                )
            except AssertionError as error:
                raise AssertionError(f"{error}\ncontroller logs:\n{logs}") from error

            candidates = [
                *(([("accepted", accepted_attempt)]) if accepted_attempt else []),
                *[
                    (f"rejected-{index}", child)
                    for index, child in enumerate(rejected_attempts, start=1)
                ],
            ]
            for label, child in candidates:
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
            if args.expected_status == "error":
                expected_forks = 0 if args.expected_origin == "parent" else 1
                if trace.count('"op":"attempt.fork"') != expected_forks:
                    raise AssertionError(
                        f"unwind trace does not contain exactly {expected_forks} setup forks"
                    )
                if '"semantic_frontier_availability":"retained"' in trace:
                    raise AssertionError("unwind trace unexpectedly started a repair candidate")
            elif args.expected_origin == "parent":
                if trace.count('"op":"attempt.fork"') != 2:
                    raise AssertionError(
                        "trace does not contain exactly two candidate forks"
                    )
            elif trace.count(
                '"semantic_frontier_availability":"retained"'
            ) < 2:
                raise AssertionError(
                    "trace does not contain two retained-frontier candidate restores"
                )
            expected_accepts = 1 if args.expected_status == "accepted" else 0
            if trace.count('"op":"attempt.accept"') != expected_accepts:
                raise AssertionError(
                    f"trace does not contain exactly {expected_accepts} acceptance operations"
                )
            if args.expected_status == "error" and trace.count(
                '"op":"env.discard_checkpoint"'
            ) < 1:
                raise AssertionError("unwind cleanup did not release the semantic frontier")
            active_checkpoints = smoke.gateway(
                args.gateway_bin,
                data_root,
                "env",
                "list-checkpoints",
                "--workspace",
                "repo",
                env=client_env,
            ).stdout.strip()
            if active_checkpoints:
                raise AssertionError(
                    "semantic frontier release left active checkpoints: "
                    + active_checkpoints
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
                "status": args.expected_status,
                "accepted_attempt": accepted_attempt,
                "rejected_attempts": rejected_attempts,
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
