#!/usr/bin/env python3
"""Verify V7 startup recovery of a run lost before ledger recording."""

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
INVOCATION_MARKER = "\nsession = agent_session()"

SUFFIX = r'''
def fixture_recovery_state():
    return {
        "actions": 0,
        "run_start_calls": 0,
        "run_observations": 0,
        "run_termination_calls": 0,
        "run_completions": 0,
        "run_reconciliations": 0,
        "recovered_active_runs": 0,
        "run_reconciliation_over_capacity": 0,
        "peak_active_runs": 0,
        "active_runs": [],
    }


lifecycle = attempt_info()
options = standard_agent_options(task_input())
if lifecycle.controller_run_count == 1:
    # Deliberately end the controller after Gateway starts the operation but
    # before any progress.active_runs write. This models the narrowest lost
    # handle window without replaying the operation on the next start.
    started = run(
        ["/bin/sh", "-lc", "sleep 300"],
        background=True,
        max_stdout_bytes=1024,
        max_stderr_bytes=1024,
    )
    emit({
        "phase": "operation_started_handle_unrecorded",
        "still_running": started.still_running,
        "memory": context_read(limit=8),
    })
else:
    state = fixture_recovery_state()
    state = standard_reconcile_active_runs(state, options)
    recovered = map(lambda active: active.run_id, state.active_runs)
    state = standard_reap_active_runs(state, options)
    authoritative = attempt_inspect(
        include_details=False,
        trace_limit=1,
        max_bytes=1024,
    )
    emit({
        "phase": "reconciled_and_reaped",
        "recovered": recovered,
        "remaining": authoritative.active_runs,
        "metrics": {
            "run_reconciliations": state.run_reconciliations,
            "recovered_active_runs": state.recovered_active_runs,
            "run_termination_calls": state.run_termination_calls,
            "run_completions": state.run_completions,
            "active_runs": len(state.active_runs),
        },
        "memory": context_read(keys=["progress.active_runs"], limit=1),
    })
'''.lstrip()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument(
        "--gateway-bin",
        type=Path,
        default=SANDBOX / "waymark-gateway/target/debug/waymark-gateway",
    )
    parser.add_argument(
        "--standard-source",
        type=Path,
        default=ROOT / "examples/scripts/standard_attempt_agent.stone",
    )
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-standard-restart-reconciliation-v8",
    )
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def compose_source(source: str) -> str:
    marker = source.find(INVOCATION_MARKER)
    if marker < 0:
        raise ValueError(f"standard source is missing {INVOCATION_MARKER!r}")
    return source[:marker].rstrip() + "\n\n" + SUFFIX


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


def parse_multimap(text: str) -> dict[str, list[str]]:
    fields: dict[str, list[str]] = {}
    for line in text.splitlines():
        key, separator, value = line.partition("\t")
        if separator:
            fields.setdefault(key, []).append(value)
    return fields


def response_from_logs(text: str) -> dict[str, Any] | None:
    return base.response_payload(subprocess.CompletedProcess([], 0, text, ""))


def gate_result(result: dict[str, Any]) -> list[str]:
    violations: list[str] = []
    runs = result.get("controller_runs") or []
    if len(runs) != 2:
        return ["restart canary did not produce exactly two controller runs"]
    first = ((runs[0].get("payload") or {}).get("value") or {})
    second = ((runs[1].get("payload") or {}).get("value") or {})
    after_first = runs[0].get("inspection") or {}
    after_second = runs[1].get("inspection") or {}

    if first.get("phase") != "operation_started_handle_unrecorded":
        violations.append("first controller did not stop in the lost-handle window")
    if first.get("still_running") is not True:
        violations.append("first controller did not leave a live operation")
    if any(
        item.get("key") == "progress.active_runs"
        for item in first.get("memory") or []
        if isinstance(item, dict)
    ):
        violations.append("first controller recorded the handle before stopping")
    first_active = after_first.get("active_run") or []
    if len(first_active) != 1:
        violations.append(
            f"Gateway did not retain exactly one authoritative run: {first_active}"
        )

    if second.get("phase") != "reconciled_and_reaped":
        violations.append("second controller did not run recovery")
    if second.get("recovered") != first_active:
        violations.append("startup recovery did not rebuild the authoritative handle")
    if second.get("remaining") != []:
        violations.append("recovered operation remained active after reaping")
    if after_second.get("active_run"):
        violations.append("Gateway still reports an active run after recovery")
    metrics = second.get("metrics") or {}
    expected_metrics = {
        "run_reconciliations": 1,
        "recovered_active_runs": 1,
        "run_termination_calls": 1,
        "run_completions": 1,
        "active_runs": 0,
    }
    for key, expected in expected_metrics.items():
        if metrics.get(key) != expected:
            violations.append(
                f"recovery metric {key}={metrics.get(key)!r}, expected {expected}"
            )
    memory = second.get("memory") or []
    if len(memory) != 1:
        violations.append("recovery did not retain one bounded active-run ledger item")
    else:
        item = memory[0]
        if item.get("status") != "verified":
            violations.append("settled active-run ledger is not verified")
        content = item.get("content") or {}
        if content.get("count") != 0 or content.get("runs") != []:
            violations.append("settled active-run ledger retained a stale handle")
    return violations


def run_experiment(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    composed = compose_source(args.standard_source.read_text(encoding="utf-8"))
    source_path = run_dir / "restart-reconciliation.stone"
    source_path.write_text(composed, encoding="utf-8")
    manifest = {
        "schema": "waymark.standard-agent-restart-reconciliation-manifest.v8",
        "standard_source": str(args.standard_source),
        "standard_source_sha256": digest(args.standard_source),
        "composed_source_sha256": digest(source_path),
        "waymark_sha256": digest(args.waymark_bin),
        "gateway_sha256": digest(args.gateway_bin),
    }
    (run_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    data_root = run_dir / "gateway-data"
    fixture = run_dir / "fixture"
    fixture.mkdir()
    (fixture / "README.md").write_text(
        "restart reconciliation fixture\n", encoding="utf-8"
    )
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-reconcile-", dir="/tmp"))
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
    attempt = ""
    started = time.monotonic()
    finish_text = ""
    try:
        base.wait_for_socket(socket_path, server)
        gateway(
            args.gateway_bin,
            data_root,
            "repo",
            "snapshot",
            "--name",
            "restart-reconciliation",
            "--path",
            str(fixture),
        )
        attempt = gateway(
            args.gateway_bin,
            data_root,
            "attempt",
            "spawn",
            "--task",
            "standard-restart-reconciliation",
            "--workspace",
            "restart-reconciliation",
            "--controller",
            "stone",
            "--workspace-mount",
            "/app",
            "--program-stone-file",
            str(source_path),
            env=client_env,
        ).strip()
        controller_runs = []
        for _ in range(2):
            gateway(
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
            inspection = parse_multimap(
                gateway(
                    args.gateway_bin,
                    data_root,
                    "rpc",
                    "call",
                    "--socket",
                    str(socket_path),
                    "attempt.inspect",
                    "--attempt",
                    attempt,
                    "--trace-limit",
                    "4",
                    env=client_env,
                )
            )
            controller_runs.append(
                {
                    "payload": response_from_logs(logs),
                    "inspection": inspection,
                }
            )
        result = {
            "schema": "waymark.standard-agent-restart-reconciliation-result.v8",
            "ok": False,
            "duration_seconds": time.monotonic() - started,
            "violations": [],
            "attempt": attempt,
            "controller_runs": controller_runs,
            "manifest": manifest,
        }
        result["violations"] = gate_result(result)
        result["ok"] = not result["violations"]
        return result
    finally:
        if attempt:
            finish_text = gateway(
                args.gateway_bin,
                data_root,
                "attempt",
                "finish",
                attempt,
                "--rollback",
                "--reason",
                "restart reconciliation canary cleanup",
                env=client_env,
            )
        if "result" in locals():
            if "state\trolled_back" not in finish_text:
                result["ok"] = False
                result["violations"].append("attempt did not roll back cleanly")
            open_transactions = gateway(
                args.gateway_bin,
                data_root,
                "env",
                "list-tx",
                env=client_env,
            ).strip()
            result["open_transactions"] = open_transactions
            if open_transactions:
                result["ok"] = False
                result["violations"].append("experiment left open transactions")
            (run_dir / "summary.json").write_text(
                json.dumps(result, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
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
    args.standard_source = args.standard_source.resolve()
    args.run_dir = args.run_dir.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("standard source", args.standard_source),
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
    raise SystemExit(main())
