#!/usr/bin/env python3
"""Run the deterministic Stone attempt-transition memory canary."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


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
        default=ROOT / "examples/scripts/transition_memory_canary.stone",
    )
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-transition-memory-canary",
    )
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def canary_gate(payload: dict[str, Any] | None) -> tuple[bool, list[str]]:
    violations: list[str] = []
    if not isinstance(payload, dict) or payload.get("ok") is not True:
        return False, ["Stone execution did not return ok=true"]
    value = payload.get("value")
    if not isinstance(value, dict):
        return False, ["Stone result is not a record"]

    control = str(value.get("control", "")).lower()
    treatment = str(value.get("treatment", "")).lower()
    if "cobalt" in control:
        violations.append("control unexpectedly contains the early requirement")
    if "cobalt" not in treatment:
        violations.append("hooked model call did not receive the projected requirement")

    control_id = value.get("control_transition_id")
    treatment_id = value.get("treatment_transition_id")
    if not control_id or not treatment_id or control_id == treatment_id:
        violations.append("model calls lack distinct transition ids")
    retained = value.get("retained")
    if not isinstance(retained, list) or not retained:
        violations.append("post hook retained no model outcome")
    elif retained[0].get("content", {}).get("transition_id") != treatment_id:
        violations.append("retained outcome is not linked to the treatment transition")

    events = (payload.get("diagnostics") or {}).get("transitions") or []
    treatment_phases = [
        event.get("phase")
        for event in events
        if isinstance(event, dict) and event.get("id") == treatment_id
    ]
    if treatment_phases != ["start", "pre", "effect", "post"]:
        violations.append(
            f"treatment transition phases are incomplete or unordered: {treatment_phases}"
        )
    return not violations, violations


def run_canary(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    data_root = run_dir / "gateway-data"
    source_root = run_dir / "source"
    work = run_dir / "work"
    source_root.mkdir()
    work.mkdir()
    (source_root / "README.md").write_text("transition memory canary\n", encoding="utf-8")
    base.gateway_command(
        args.gateway_bin,
        data_root,
        "repo",
        "snapshot",
        "--name",
        "repo",
        "--path",
        str(source_root),
    )
    tx = base.gateway_command(
        args.gateway_bin,
        data_root,
        "env",
        "snapshot",
        "--workspace",
        "repo",
    )

    socket_root = Path(tempfile.mkdtemp(prefix="waymark-transition-memory-", dir="/tmp"))
    socket_path = socket_root / "gateway.sock"
    server_env = dict(os.environ)
    server_env["WAYMARK_MODEL_PROVIDER"] = "fixture"
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
    try:
        base.wait_for_socket(socket_path, server)
        env = dict(os.environ)
        env.update(
            {
                "WAYMARK_START_DIR": str(work),
                "WAYMARK_GATEWAY_SOCKET": str(socket_path),
                "WAYMARK_GATEWAY_TX": tx,
                "WAYMARK_GATEWAY_IMAGE": "python:3.12",
                "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
                "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
            }
        )
        completed = base.run_capture(
            [str(args.waymark_bin), "eval", str(args.source.resolve())],
            cwd=work,
            env=env,
            timeout=60,
        )
        payload = base.response_payload(completed)
        gate_ok, violations = canary_gate(payload)
        return {
            "ok": completed.returncode == 0 and gate_ok,
            "exit_code": completed.returncode,
            "source": str(args.source.resolve()),
            "response": payload,
            "gate_ok": gate_ok,
            "gate_violations": violations,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
        }
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait(timeout=5)
        base.gateway_command(args.gateway_bin, data_root, "env", "rollback", tx)
        shutil.rmtree(socket_root, ignore_errors=True)


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
    result = run_canary(args)
    summary_path = args.run_dir.resolve() / "summary.json"
    summary_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
