#!/usr/bin/env python3
"""Verify crash-safe in-flight action memory across Gateway controller restarts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
BOUNDARIES = ("prepared", "started", "completed")
EXPECTED_REVISIONS = {"prepared": 5, "started": 3, "completed": 5}


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
        default=ROOT / "examples/scripts/attempt_inflight_restart_experiment.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-inflight-restart-v1",
    )
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def gateway(
    binary: Path,
    data_root: Path,
    *args: str,
    env: dict[str, str] | None = None,
    timeout: float = 60.0,
) -> str:
    completed = base.run_capture(
        [str(binary), "--data-root", str(data_root), *args],
        env=env,
        timeout=timeout,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"Gateway command failed: {args}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed.stdout


def parse_info(text: str) -> dict[str, Any]:
    info: dict[str, Any] = {"metadata": {}}
    for line in text.splitlines():
        key, separator, value = line.partition("\t")
        if not separator:
            continue
        if key == "metadata":
            name, equals, metadata_value = value.partition("=")
            if equals:
                info["metadata"][name] = metadata_value
        else:
            info[key] = value
    return info


def response_from_logs(text: str) -> dict[str, Any] | None:
    return base.response_payload(subprocess.CompletedProcess([], 0, text, ""))


def transition_phases(payload: dict[str, Any]) -> dict[str, list[str]]:
    phases: dict[str, list[str]] = {}
    for event in (payload.get("diagnostics") or {}).get("transitions") or []:
        if not isinstance(event, dict):
            continue
        transition_id = event.get("id")
        phase = event.get("phase")
        if isinstance(transition_id, str) and isinstance(phase, str):
            phases.setdefault(transition_id, []).append(phase)
    return phases


def expected_transition(run: int) -> str:
    return f"run-{run}-transition-1"


def memory_keys(value: dict[str, Any]) -> list[str]:
    return sorted(
        item.get("key")
        for item in value.get("memory") or []
        if isinstance(item, dict) and isinstance(item.get("key"), str)
    )


def gate_cell(boundary: str, cell: dict[str, Any]) -> list[str]:
    violations: list[str] = []
    attempt = str(cell.get("attempt") or "")
    runs = cell.get("runs") or []
    if len(runs) != 3:
        return [f"{boundary} did not produce three controller runs"]
    payloads = [run.get("payload") for run in runs]
    if any(not isinstance(payload, dict) or payload.get("ok") is not True for payload in payloads):
        return [f"{boundary} controller response failed"]
    values = [payload.get("value") for payload in payloads]
    if any(not isinstance(value, dict) for value in values):
        return [f"{boundary} controller returned no structured value"]
    first, second, third = values

    for index, value in enumerate(values, start=1):
        if value.get("boundary") != boundary:
            violations.append(f"{boundary} run {index} reported another boundary")
        if value.get("controller_run_count") != index:
            violations.append(
                f"{boundary} run {index} reported controller count "
                f"{value.get('controller_run_count')!r}"
            )

    expected_first = {
        "prepared": "stop_before_execution",
        "started": "lose_outcome",
        "completed": "stop_before_consolidation",
    }[boundary]
    expected_second = {
        "prepared": "resume_once",
        "started": "replan",
        "completed": "record_outcome",
    }[boundary]
    if first.get("decision") != expected_first:
        violations.append(f"{boundary} initial decision is not {expected_first}")
    if second.get("decision") != expected_second:
        violations.append(f"{boundary} restart decision is not {expected_second}")
    if third.get("decision") != "terminal_noop":
        violations.append(f"{boundary} terminal replay was not a no-op")

    expected_first_effects = 0 if boundary == "prepared" else 1
    if len(first.get("effect_lines") or []) != expected_first_effects:
        violations.append(f"{boundary} initial effect count is incorrect")
    if second.get("effect_lines") != ["effect"] or third.get("effect_lines") != ["effect"]:
        violations.append(f"{boundary} did not preserve exactly one external effect")

    state = third.get("state") or {}
    if state.get("phase") != "terminal":
        violations.append(f"{boundary} did not reach terminal state")
    if boundary == "started":
        if (
            state.get("decision") != "replan"
            or state.get("execution_state") != "started_or_unknown"
            or state.get("outcome") is not None
        ):
            violations.append("started boundary did not conservatively replan")
    elif (
        state.get("decision") != "record_outcome"
        or state.get("execution_state") != "completed"
        or not isinstance(state.get("outcome"), dict)
        or state["outcome"].get("ok") is not True
        or state["outcome"].get("exit_code") != 0
    ):
        violations.append(f"{boundary} did not consolidate the completed outcome")

    if memory_keys(second) != ["action.inflight"]:
        violations.append(f"{boundary} restart did not compact to one hot item")
    if memory_keys(third) != ["action.inflight"]:
        violations.append(f"{boundary} terminal run changed the hot memory shape")

    second_revision = int((runs[1].get("info") or {}).get("memory_revision") or 0)
    third_revision = int((runs[2].get("info") or {}).get("memory_revision") or 0)
    expected_revision = EXPECTED_REVISIONS[boundary]
    if second_revision != expected_revision or third_revision != expected_revision:
        violations.append(
            f"{boundary} revisions were {second_revision}/{third_revision}, "
            f"expected stable {expected_revision}"
        )

    action_run = 2 if boundary == "prepared" else 1
    transition_id = expected_transition(action_run)
    observed = second.get("transition_id") if boundary == "prepared" else first.get("transition_id")
    if observed != transition_id or state.get("transition_id") != transition_id:
        violations.append(f"{boundary} transition ID is not controller-run scoped")
    action_payload = payloads[action_run - 1]
    expected_phases = (
        ["start", "pre", "effect"]
        if boundary == "started"
        else ["start", "pre", "effect", "post"]
    )
    if transition_phases(action_payload).get(transition_id) != expected_phases:
        violations.append(f"{boundary} action trace phases are incomplete")
    if transition_phases(payloads[2]):
        violations.append(f"{boundary} terminal no-op emitted another transition")

    all_ids = [
        transition_id
        for payload in payloads
        for transition_id in transition_phases(payload)
    ]
    if len(all_ids) != len(set(all_ids)):
        violations.append(f"{boundary} reused a transition ID across controller runs")
    return violations


def experiment_gate(cells: dict[str, dict[str, Any]]) -> tuple[bool, list[str]]:
    violations = [
        violation
        for boundary in BOUNDARIES
        for violation in gate_cell(boundary, cells.get(boundary) or {})
    ]
    return not violations, violations


def run_cell(
    args: argparse.Namespace,
    *,
    boundary: str,
    data_root: Path,
    fixtures_root: Path,
    client_env: dict[str, str],
) -> dict[str, Any]:
    workspace = f"inflight-{boundary}"
    source_root = fixtures_root / boundary
    source_root.mkdir(parents=True)
    (source_root / "mode.txt").write_text(boundary + "\n", encoding="utf-8")
    gateway(
        args.gateway_bin,
        data_root,
        "repo",
        "snapshot",
        "--name",
        workspace,
        "--path",
        str(source_root),
    )
    attempt = gateway(
        args.gateway_bin,
        data_root,
        "attempt",
        "spawn",
        "--task",
        f"inflight-{boundary}",
        "--workspace",
        workspace,
        "--controller",
        "stone",
        "--workspace-mount",
        "/app",
        "--program-stone-file",
        str(args.source),
        env=client_env,
    ).strip()
    try:
        runs = []
        for _ in range(3):
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
            )
            logs = gateway(
                args.gateway_bin,
                data_root,
                "attempt",
                "logs",
                attempt,
                "--max-bytes",
                "524288",
                env=client_env,
            )
            runs.append(
                {
                    "info": parse_info(info_text),
                    "payload": response_from_logs(logs),
                    "logs": logs,
                }
            )
        return {"boundary": boundary, "attempt": attempt, "runs": runs}
    finally:
        finish = gateway(
            args.gateway_bin,
            data_root,
            "attempt",
            "finish",
            attempt,
            "--rollback",
            "--reason",
            "in-flight restart experiment cleanup",
            env=client_env,
        )
        if "state\trolled_back" not in finish:
            raise RuntimeError(f"attempt {attempt} did not roll back cleanly: {finish}")


def run_experiment(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    manifest = {
        "schema": "waymark.stone-inflight-restart-manifest.v1",
        "source": str(args.source),
        "source_sha256": digest(args.source),
        "boundaries": list(BOUNDARIES),
        "expected_revisions": EXPECTED_REVISIONS,
        "waymark_sha256": digest(args.waymark_bin),
        "gateway_sha256": digest(args.gateway_bin),
    }
    (run_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    data_root = run_dir / "gateway-data"
    fixtures_root = run_dir / "fixtures"
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-inflight-restart-", dir="/tmp"))
    socket_path = socket_root / "gateway.sock"
    gateway_stdout = (run_dir / "gateway.stdout").open("w", encoding="utf-8")
    gateway_stderr = (run_dir / "gateway.stderr").open("w", encoding="utf-8")
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
        text=True,
        stdout=gateway_stdout,
        stderr=gateway_stderr,
    )
    client_env = {
        **os.environ,
        "WAYMARK_STONE_BIN": str(args.waymark_bin),
        "WAYMARK_GATEWAY_SOCKET": str(socket_path),
        "WAYMARK_GATEWAY_IMAGE": args.image,
        "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
    }
    started = time.monotonic()
    try:
        base.wait_for_socket(socket_path, server)
        cells = {
            boundary: run_cell(
                args,
                boundary=boundary,
                data_root=data_root,
                fixtures_root=fixtures_root,
                client_env=client_env,
            )
            for boundary in BOUNDARIES
        }
        ok, violations = experiment_gate(cells)
        open_transactions = gateway(
            args.gateway_bin, data_root, "env", "list-tx", env=client_env
        ).strip()
        if open_transactions:
            ok = False
            violations.append("experiment left open transactions")
        result = {
            "schema": "waymark.stone-inflight-restart-result.v1",
            "ok": ok,
            "duration_seconds": time.monotonic() - started,
            "violations": violations,
            "cells": cells,
            "open_transactions": open_transactions,
            "manifest": manifest,
        }
        (run_dir / "summary.json").write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        return result
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=5)
        gateway_stdout.close()
        gateway_stderr.close()
        shutil.rmtree(socket_root, ignore_errors=True)


def main() -> int:
    args = parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()
    args.source = args.source.resolve()
    args.run_dir = args.run_dir.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("Stone source", args.source),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    result = run_experiment(args)
    print(
        json.dumps(
            {
                "ok": result["ok"],
                "duration_seconds": result["duration_seconds"],
                "violations": result["violations"],
                "summary": str(args.run_dir / "summary.json"),
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
