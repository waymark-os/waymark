#!/usr/bin/env python3
"""Exercise a visible one-decision agent inside an evidence-gated workflow."""

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
            "actions": [
                {
                    "tool": "decide",
                    "input": {
                        "answer": "Build both files, run the command, then verify the frame.",
                        "findings": {
                            "source_layout": "fixture files live at the workspace root",
                            "execution_gate": "run the exact observed shell command",
                        },
                    },
                }
            ]
        },
        separators=(",", ":"),
    ),
    json.dumps(
        {
            "actions": [
                {
                    "tool": "read",
                    "input": {"path": "."},
                }
            ]
        },
        separators=(",", ":"),
    ),
    json.dumps(
        {
            "actions": [
                {
                    "tool": "run_complete",
                    "input": {
                        "argv": [
                            "sh",
                            "-c",
                            "awk 'BEGIN { for(i=0;i<20000;i++) printf \"x\" }'; printf first > first.txt",
                        ],
                        "timeout_ms": 10000,
                    },
                }
            ]
        },
        separators=(",", ":"),
    ),
    json.dumps(
        {
            "actions": [
                {
                    "tool": "run_complete",
                    "input": {
                        "argv": [
                            "sh",
                            "-c",
                            "printf second > second.txt; printf BMframe > /tmp/frame.bmp",
                        ],
                        "timeout_ms": 10000,
                    },
                }
            ]
        },
        separators=(",", ":"),
    ),
    json.dumps(
        {
            "actions": [
                {
                    "tool": "run_complete",
                    "input": {
                        "argv": ["sh", "-c", "printf observed"],
                        "timeout_ms": 10000,
                    },
                }
            ]
        },
        separators=(",", ":"),
    ),
]

HARNESS = r'''
workflow fixture:
    stage inspect(goal="record a fixture execution decision", max_actions=1):
        agent_loop()
        ensure decision_recorded(fields=["source_layout", "execution_gate"])

    stage build(goal="produce first.txt and second.txt", max_actions=3):
        agent_loop()
        ensure file_nonempty("first.txt")
        ensure file_nonempty("second.txt")

    stage execute(goal="run the observable command", max_actions=2):
        agent_loop()
        ensure command_succeeded(["sh", "-c", "printf observed"])
        ensure stdout_nonempty()

    stage verify(goal="validate the retained tool-environment output", max_actions=1):
        ensure file_valid("/tmp/frame.bmp", format="bmp", nonempty=True)

run fixture
'''


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
        default=ROOT / "examples/scripts/standard_stage_agent.stone",
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


def assert_result(payload: dict[str, Any], info: dict[str, Any]) -> None:
    if payload.get("ok") is not True:
        raise AssertionError(f"staged controller failed: {payload}")
    report = payload.get("value") or {}
    if report.get("ok") is not True or report.get("complete") is not True:
        raise AssertionError(
            "workflow did not complete: "
            + json.dumps(
                {
                    "report": report,
                    "diagnostics": payload.get("diagnostics") or {},
                },
                sort_keys=True,
            )
        )
    stages = report.get("stages") or []
    if len(stages) != 4:
        raise AssertionError(f"unexpected stage report: {stages}")
    expected = [
        {
            "name": "inspect",
            "control": "agent_loop",
            "status": "completed",
            "attempts": 1,
            "checks": 2,
            "max_actions": 1,
        },
        {
            "name": "build",
            "control": "agent_loop",
            "status": "completed",
            "attempts": 3,
            "checks": 4,
            "max_actions": 3,
        },
        {
            "name": "execute",
            "control": "agent_loop",
            "status": "completed",
            "attempts": 1,
            "checks": 2,
            "max_actions": 2,
        },
        {
            "name": "verify",
            "control": "deterministic",
            "status": "already_satisfied",
            "attempts": 0,
            "checks": 1,
            "max_actions": 1,
        },
    ]
    for stage, fields in zip(stages, expected, strict=True):
        for key, value in fields.items():
            if stage.get(key) != value:
                raise AssertionError(f"unexpected stage {key}: {stage}")
    decision_references = (stages[0].get("evidence") or {}).get("evidence") or []
    if len(decision_references) != 1 or not decision_references[0].endswith(":decision"):
        raise AssertionError(f"unexpected decision evidence: {stages[0]}")
    if "source_layout, execution_gate" not in str(
        (stages[0].get("evidence") or {}).get("summary") or ""
    ):
        raise AssertionError(f"typed decision findings were not enforced: {stages[0]}")
    build_references = set((stages[1].get("evidence") or {}).get("evidence") or [])
    if build_references != {
        "file:first.txt:size=5",
        "file:second.txt:size=6",
    }:
        raise AssertionError(f"unexpected build evidence: {stages[1]}")
    execute_references = set(
        (stages[2].get("evidence") or {}).get("evidence") or []
    )
    if len(execute_references) != 2 or not any(
        reference.startswith("command:") for reference in execute_references
    ) or not any(
        reference == "stdout:bytes=8" for reference in execute_references
    ):
        raise AssertionError(f"unexpected execute evidence: {stages[2]}")
    verify_references = (stages[3].get("evidence") or {}).get("evidence") or []
    if len(verify_references) != 1 or "plane=tool_environment" not in verify_references[0]:
        raise AssertionError(f"unexpected verify evidence: {stages[3]}")

    transitions = (payload.get("diagnostics") or {}).get("transitions") or []
    starts = [event for event in transitions if event.get("phase") == "start"]
    kinds = [event.get("kind") for event in starts]
    if kinds.count("model_call") != 5 or kinds.count("run_complete") != 3:
        raise AssertionError(f"unexpected action-state transitions: {kinds}")
    context = (payload.get("diagnostics") or {}).get("context") or {}
    written = [
        event.get("key")
        for event in context.get("events") or []
        if event.get("op") == "write"
    ]
    if written != [
        "workflow.fixture.inspect.history",
        "workflow.fixture.build.history",
        "workflow.fixture.build.history",
        "workflow.fixture.build.history",
        "workflow.fixture.execute.history",
    ]:
        raise AssertionError(f"stage outcomes were not retained: {written}")
    if (info.get("metadata") or {}).get("controller_result_status") != "succeeded":
        raise AssertionError(f"missing successful final report: {info}")


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.library = args.library.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("stage agent library", args.library),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    program = args.library.read_text(encoding="utf-8") + "\n" + HARNESS

    with tempfile.TemporaryDirectory(prefix="waymark-staged-agent-", dir="/tmp") as root_text:
        root = Path(root_text)
        data_root = root / "gateway-data"
        workspace_source = root / "workspace"
        socket_path = root / "gateway.sock"
        workspace_source.mkdir()
        (workspace_source / "README.md").write_text(
            "staged workflow fixture\n", encoding="utf-8"
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
                # Each stage decision intentionally rebuilds a bounded prompt, so
                # use the process-global fixture sequence rather than the helper
                # whose index is derived from assistant messages in one history.
                "WAYMARK_MODEL_FIXTURE_GLOBAL_SEQUENCE_JSON": json.dumps(FIXTURES),
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
                            "id": "staged-workflow-agent-smoke",
                            "objective": "Produce non-empty first.txt and second.txt.",
                        },
                        separators=(",", ":"),
                    ),
                    'workspace_source={"workspace":"repo"}',
                    "program="
                    + json.dumps(
                        {"kind": "stone", "source": program},
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
                    "staged workflow smoke cleanup",
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

    print(json.dumps({"ok": True, "library": str(args.library)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
