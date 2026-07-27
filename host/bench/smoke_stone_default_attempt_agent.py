#!/usr/bin/env python3
"""Exercise the reference attempt agent with a failed run and final answer."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
FIXTURES = [
    json.dumps(
        {
            "kind": "run",
            "argv": ["sh", "-c", "printf 'expected failure\\n' >&2; exit 7"],
        },
        separators=(",", ":"),
    ),
    json.dumps({"kind": "finish", "answer": "ready"}, separators=(",", ":")),
]


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
        default=ROOT / "examples/scripts/default_attempt_agent.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=120.0)
    return parser.parse_args()


def gateway(
    binary: Path,
    data_root: Path,
    *args: str,
    env: dict[str, str] | None = None,
    timeout: float = 30.0,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    completed = base.run_capture(
        [str(binary), "--data-root", str(data_root), *args],
        env=env,
        timeout=timeout,
    )
    if check and completed.returncode != 0:
        raise RuntimeError(
            f"Gateway command failed: {args}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed


def parse_info(text: str) -> dict[str, Any]:
    info: dict[str, Any] = {"metadata": {}}
    for line in text.splitlines():
        key, separator, value = line.partition("\t")
        if not separator:
            continue
        if key == "metadata":
            metadata_key, equals, metadata_value = value.partition("=")
            if equals:
                info["metadata"][metadata_key] = metadata_value
        else:
            info[key] = value
    return info


def transition_phases(payload: dict[str, Any]) -> dict[str, tuple[str, list[str]]]:
    transitions: dict[str, tuple[str, list[str]]] = {}
    for event in (payload.get("diagnostics") or {}).get("transitions") or []:
        transition_id = event.get("id")
        kind = event.get("kind")
        phase = event.get("phase")
        if not all(isinstance(value, str) for value in (transition_id, kind, phase)):
            continue
        if transition_id not in transitions:
            transitions[transition_id] = (kind, [])
        transitions[transition_id][1].append(phase)
    return transitions


def assert_result(payload: dict[str, Any], info: dict[str, Any]) -> None:
    if payload.get("ok") is not True:
        raise AssertionError(f"Stone controller failed: {payload}")
    value = payload.get("value") or {}
    if value.get("answer") != "ready" or value.get("turns") != 2:
        raise AssertionError(f"unexpected result: {value}")
    if value.get("controller_run_count") != 1:
        raise AssertionError(f"unexpected controller lifecycle: {value}")
    expected_keys = {
        "requirement.objective",
        "decision.last",
        "outcome.last_tool",
        "goal.active",
    }
    if set(value.get("memory_keys") or []) != expected_keys:
        raise AssertionError(f"unexpected memory keys: {value.get('memory_keys')}")

    final_text = (value.get("final_context") or {}).get("text", "")
    if "outcome.last_tool" not in final_text or "expected failure" not in final_text:
        raise AssertionError(f"failed outcome missing from final projection: {final_text!r}")

    transitions = transition_phases(payload)
    kinds = [kind for kind, _ in transitions.values()]
    if kinds != ["model_call", "run", "model_call"]:
        raise AssertionError(f"unexpected transition kinds: {kinds}")
    for transition_id, (_, phases) in transitions.items():
        if phases != ["start", "pre", "effect", "post"]:
            raise AssertionError(f"incomplete transition {transition_id}: {phases}")

    context = (payload.get("diagnostics") or {}).get("context") or {}
    writes = [
        event.get("key")
        for event in context.get("events") or []
        if event.get("op") == "write"
    ]
    if writes != [
        "requirement.objective",
        "decision.last",
        "outcome.last_tool",
        "decision.last",
        "goal.active",
    ]:
        raise AssertionError(f"unexpected memory write sequence: {writes}")

    if info.get("memory_revision") != "5":
        raise AssertionError(f"unexpected memory revision: {info}")
    if (info.get("metadata") or {}).get("controller_result_status") != "succeeded":
        raise AssertionError(f"missing final attempt report: {info}")


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

    with tempfile.TemporaryDirectory(prefix="waymark-default-agent-", dir="/tmp") as root_text:
        root = Path(root_text)
        data_root = root / "gateway-data"
        workspace_source = root / "workspace"
        socket_path = root / "gateway.sock"
        workspace_source.mkdir()
        (workspace_source / "README.md").write_text(
            "default attempt agent smoke fixture\n", encoding="utf-8"
        )
        gateway(
            args.gateway_bin,
            data_root,
            "repo",
            "snapshot",
            "--name",
            "repo",
            "--path",
            str(workspace_source),
        )
        bootstrap_tx = gateway(
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
                "WAYMARK_MODEL_PROVIDER": "fixture",
                "WAYMARK_MODEL_FIXTURE_SEQUENCE_JSON": json.dumps(FIXTURES),
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
                "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
            }
        )
        try:
            base.wait_for_socket(socket_path, server)
            spawn_source = "emit(attempt_spawn(" + ",".join(
                [
                    "task_spec="
                    + json.dumps(
                        {
                            "id": "default-attempt-agent-smoke",
                            "objective": "Return ready after observing one failed diagnostic.",
                        },
                        separators=(",", ":"),
                    ),
                    'workspace_source={"workspace":"repo"}',
                    "program="
                    + json.dumps(
                        {"kind": "stone", "source": args.source.read_text(encoding="utf-8")},
                        separators=(",", ":"),
                    ),
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
            info_text = gateway(
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
            ).stdout
            logs = gateway(
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
            assert_result(payload, parse_info(info_text))

            restart = gateway(
                args.gateway_bin,
                data_root,
                "attempt",
                "start",
                attempt,
                env=client_env,
                check=False,
            )
            if restart.returncode == 0:
                raise AssertionError("Gateway allowed restart after final attempt_report")
        finally:
            if attempt:
                gateway(
                    args.gateway_bin,
                    data_root,
                    "attempt",
                    "finish",
                    attempt,
                    "--rollback",
                    "--reason",
                    "default agent smoke cleanup",
                    env=client_env,
                )
            gateway(
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

        open_transactions = gateway(
            args.gateway_bin, data_root, "env", "list-tx", env=client_env
        ).stdout.strip()
        if open_transactions:
            raise AssertionError(f"smoke left open transactions: {open_transactions}")

    print(json.dumps({"ok": True, "source": str(args.source)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
