#!/usr/bin/env python3
"""Compare relevance-only projection with a call-local required memory key."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
MODES = ("U", "R")
MODE_NAMES = {"U": "unpinned", "R": "required_key"}
CURRENT_TARGET = "required-target-47c1d06a"
DECOY_TARGET = "pending-candidate-a52e908f"


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
        default=ROOT / "examples/scripts/attempt_memory_required_projection_experiment.stone",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-attempt-memory-required-projection-v1",
    )
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def gateway_command(
    gateway_bin: Path,
    data_root: Path,
    *args: str,
    env: dict[str, str] | None = None,
    timeout: float = 60.0,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    completed = base.run_capture(
        [str(gateway_bin), "--data-root", str(data_root), *args],
        env=env,
        timeout=timeout,
    )
    if check and completed.returncode != 0:
        raise RuntimeError(
            f"Gateway command failed: {args}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed


def parse_attempt_info(text: str) -> dict[str, Any]:
    result: dict[str, Any] = {"metadata": {}}
    for line in text.splitlines():
        key, separator, value = line.partition("\t")
        if not separator:
            continue
        if key == "metadata":
            meta_key, equals, meta_value = value.partition("=")
            if equals:
                result["metadata"][meta_key] = meta_value
        else:
            result[key] = value
    return result


def response_from_logs(text: str) -> dict[str, Any] | None:
    return base.response_payload(subprocess.CompletedProcess([], 0, text, ""))


def transition_phases(payload: dict[str, Any], transition_id: str) -> list[str]:
    return [
        event["phase"]
        for event in (payload.get("diagnostics") or {}).get("transitions") or []
        if event.get("id") == transition_id and isinstance(event.get("phase"), str)
    ]


def input_tokens(value: dict[str, Any]) -> int:
    usage = value.get("usage")
    if not isinstance(usage, dict):
        return 0
    tokens = usage.get("input_tokens")
    return tokens if isinstance(tokens, int) else 0


def experiment_gate(
    cells: dict[str, dict[str, Any]],
    *,
    model: str,
    current_target: str,
    decoy_target: str,
) -> tuple[bool, list[str], dict[str, Any]]:
    violations: list[str] = []
    metrics: dict[str, Any] = {"cells": {}, "model_calls": 0, "input_tokens": 0}

    for mode in MODES:
        cell = cells.get(mode)
        payload = (cell or {}).get("payload") if isinstance(cell, dict) else None
        if not isinstance(payload, dict) or payload.get("ok") is not True:
            violations.append(f"{mode} controller did not return ok=true")
            continue
        value = payload.get("value")
        if not isinstance(value, dict) or value.get("mode") != mode:
            violations.append(f"{mode} returned an invalid result")
            continue
        child = value.get("child_result")
        if not isinstance(child, dict):
            violations.append(f"{mode} did not expose a structured child result")
            continue
        keys = child.get("projection_keys")
        required = child.get("required_keys")
        if not isinstance(keys, list):
            violations.append(f"{mode} did not expose projection keys")
            continue
        if child.get("projection_tokens", 10000) > 96:
            violations.append(f"{mode} projection exceeded its token budget")
        if child.get("projection_truncated") is not True:
            violations.append(f"{mode} projection did not report truncation")
        if child.get("provider") != "codex-chatgpt" or child.get("model") != model:
            violations.append(f"{mode} used an unexpected model provider")
        if value.get("fork_origin_revision") != 3:
            violations.append(f"{mode} fork did not capture revision 3")
        if child.get("memory_revision") != 4:
            violations.append(f"{mode} child post-hook did not revise decision.latest")
        if value.get("parent_memory_revision") != 3:
            violations.append(f"{mode} child memory leaked into the parent")
        if value.get("clean") is not True:
            violations.append(f"{mode} did not close its child scope")
        phases = transition_phases(
            cell.get("child_payload") or {},
            str(child.get("transition_id") or ""),
        )
        if phases != ["start", "pre", "effect", "post"]:
            violations.append(f"{mode} model transition phases are {phases}")

        if mode == "U":
            if required != []:
                violations.append(f"U unexpectedly reported required keys: {required}")
            if "requirement.target" in keys:
                violations.append(f"U relevance projection unexpectedly included requirement: {keys}")
            if child.get("contains_current") is not False:
                violations.append("U projection leaked the current opaque target")
            if child.get("selected_target") != "insufficient_evidence":
                violations.append(
                    f"U selected {child.get('selected_target')!r}, expected insufficient_evidence"
                )
        else:
            if required != ["requirement.target"]:
                violations.append(f"R did not report its required key: {required}")
            if not keys or keys[0] != "requirement.target":
                violations.append(f"R did not emit the required key first: {keys}")
            if child.get("contains_current") is not True:
                violations.append("R projection omitted the current opaque target")
            if child.get("contains_decoy") is not False:
                violations.append("R projection included the pending decoy")
            if child.get("selected_target") != current_target:
                violations.append(
                    f"R selected {child.get('selected_target')!r}, expected {current_target!r}"
                )
            if child.get("selected_target") == decoy_target:
                violations.append("R was steered by the pending candidate")

        tokens = input_tokens(child)
        metrics["model_calls"] += 1
        metrics["input_tokens"] += tokens
        metrics["cells"][mode] = {
            "name": MODE_NAMES[mode],
            "projection_keys": keys,
            "required_keys": required,
            "projection_tokens": child.get("projection_tokens"),
            "model_input_tokens": tokens,
            "selected_target": child.get("selected_target"),
        }

    if metrics["model_calls"] != 2:
        violations.append(f"experiment made {metrics['model_calls']} model calls, expected 2")
    if metrics["input_tokens"] <= 0:
        violations.append("model calls exposed no positive input-token usage")
    return not violations, violations, metrics


def rollback_if_active(
    gateway_bin: Path,
    data_root: Path,
    attempt: str,
    client_env: dict[str, str],
) -> None:
    info = gateway_command(
        gateway_bin,
        data_root,
        "attempt",
        "info",
        attempt,
        env=client_env,
        check=False,
    )
    if info.returncode == 0 and parse_attempt_info(info.stdout).get("state") == "active":
        gateway_command(
            gateway_bin,
            data_root,
            "attempt",
            "finish",
            attempt,
            "--rollback",
            "--reason",
            "required projection experiment cleanup",
            env=client_env,
        )


def related_attempt_ids(
    gateway_bin: Path,
    data_root: Path,
    workspace: str,
    root_attempt: str,
    logs: str,
    client_env: dict[str, str],
) -> list[str]:
    attempts = set(re.findall(r"attempt-\d+-\d+", logs))
    listed = gateway_command(
        gateway_bin,
        data_root,
        "attempt",
        "list",
        "--workspace",
        workspace,
        env=client_env,
        check=False,
    )
    attempts.update(re.findall(r"attempt-\d+-\d+", listed.stdout))
    attempts.discard(root_attempt)
    return sorted(attempts)


def run_cell(
    args: argparse.Namespace,
    *,
    mode: str,
    data_root: Path,
    fixtures_root: Path,
    client_env: dict[str, str],
) -> dict[str, Any]:
    workspace = f"required-projection-{mode.lower()}"
    source_root = fixtures_root / mode.lower()
    source_root.mkdir(parents=True)
    (source_root / "mode.txt").write_text(mode + "\n", encoding="utf-8")
    (source_root / "targets.json").write_text(
        json.dumps(
            {"current": CURRENT_TARGET, "decoy": DECOY_TARGET},
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    gateway_command(
        args.gateway_bin,
        data_root,
        "repo",
        "snapshot",
        "--name",
        workspace,
        "--path",
        str(source_root),
    )
    root_attempt = gateway_command(
        args.gateway_bin,
        data_root,
        "attempt",
        "spawn",
        "--task",
        f"required-projection-{mode.lower()}",
        "--workspace",
        workspace,
        "--controller",
        "stone",
        "--workspace-mount",
        "/app",
        "--program-stone-file",
        str(args.source),
        "--program-entrypoint",
        "main",
        env=client_env,
    ).stdout.strip()
    logs = ""
    try:
        gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "start",
            root_attempt,
            "--wait",
            "--timeout-ms",
            str(int(args.timeout * 1000)),
            env=client_env,
            timeout=args.timeout + 30,
        )
        logs = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            root_attempt,
            "--max-bytes",
            "524288",
            env=client_env,
        ).stdout
        payload = response_from_logs(logs)
        if not isinstance(payload, dict):
            raise AssertionError(f"{mode} root logs contain no Stone response: {logs}")
        child = str((payload.get("value") or {}).get("child") or "")
        if not child:
            raise AssertionError(f"{mode} returned no child attempt: {payload}")
        child_logs = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            child,
            "--max-bytes",
            "262144",
            env=client_env,
        ).stdout
        logs += child_logs
        return {
            "mode": mode,
            "root_attempt": root_attempt,
            "child_attempt": child,
            "payload": payload,
            "child_payload": response_from_logs(child_logs),
            "root_logs": logs,
            "child_logs": child_logs,
        }
    finally:
        for attempt in related_attempt_ids(
            args.gateway_bin,
            data_root,
            workspace,
            root_attempt,
            logs,
            client_env,
        ):
            rollback_if_active(args.gateway_bin, data_root, attempt, client_env)
        rollback_if_active(args.gateway_bin, data_root, root_attempt, client_env)


def run_experiment(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    source_bytes = args.source.read_bytes()
    leaked = [
        value
        for value in (CURRENT_TARGET, DECOY_TARGET)
        if value.encode() in source_bytes
    ]
    if leaked:
        raise RuntimeError(f"opaque target leaked into Stone source: {leaked}")
    required = (
        b'required_keys=required',
        b'required = ["requirement.target"]',
        b"child_result = outcome.result.value",
    )
    missing = [fragment.decode() for fragment in required if fragment not in source_bytes]
    if missing:
        raise RuntimeError(f"Stone source differs from required-key contract: {missing}")

    manifest = {
        "schema": "waymark.stone-attempt-memory-required-projection.v1",
        "source": str(args.source),
        "source_sha256": sha256_bytes(source_bytes),
        "current_target_sha256": sha256_bytes(CURRENT_TARGET.encode()),
        "decoy_target_sha256": sha256_bytes(DECOY_TARGET.encode()),
        "modes": list(MODES),
        "mode_names": MODE_NAMES,
        "model": args.model,
        "provider": "codex-chatgpt",
        "reasoning_effort": args.reasoning_effort,
        "gateway_binary_sha256": sha256_bytes(args.gateway_bin.read_bytes()),
        "waymark_binary_sha256": sha256_bytes(args.waymark_bin.read_bytes()),
        "started_at_unix": int(time.time()),
    }
    (run_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    data_root = run_dir / "gateway-data"
    fixtures_root = run_dir / "fixtures"
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-required-projection-", dir="/tmp"))
    socket_path = socket_root / "gateway.sock"
    shared_env = {
        "WAYMARK_STONE_BIN": str(args.waymark_bin),
        "WAYMARK_GATEWAY_SOCKET": str(socket_path),
        "WAYMARK_GATEWAY_IMAGE": args.image,
        "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
        "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
    }
    server_env = dict(os.environ)
    server_env.update(shared_env)
    server_env.update(
        {
            "WAYMARK_MODEL_PROVIDER": "codex-chatgpt",
            "WAYMARK_MODEL_CODEX_AUTH_JSON": str(args.auth_json),
            "WAYMARK_MODEL": args.model,
            "WAYMARK_MODEL_ALLOWLIST": args.model,
            "WAYMARK_MODEL_REASONING_EFFORT": args.reasoning_effort,
        }
    )
    client_env = dict(os.environ)
    client_env.update(shared_env)
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
        env=server_env,
        text=True,
        stdout=gateway_stdout,
        stderr=gateway_stderr,
    )
    started = time.monotonic()
    cells: dict[str, dict[str, Any]] = {}
    try:
        base.wait_for_socket(socket_path, server)
        for mode in MODES:
            cells[mode] = run_cell(
                args,
                mode=mode,
                data_root=data_root,
                fixtures_root=fixtures_root,
                client_env=client_env,
            )
        ok, violations, metrics = experiment_gate(
            cells,
            model=args.model,
            current_target=CURRENT_TARGET,
            decoy_target=DECOY_TARGET,
        )
        open_transactions = gateway_command(
            args.gateway_bin,
            data_root,
            "env",
            "list-tx",
            env=client_env,
        ).stdout.strip()
        if open_transactions:
            ok = False
            violations.append("experiment left open transactions")
        return {
            "schema": "waymark.stone-attempt-memory-required-projection-result.v1",
            "ok": ok,
            "duration_seconds": time.monotonic() - started,
            "gate_violations": violations,
            "metrics": metrics,
            "cells": cells,
            "manifest": manifest,
            "open_transactions": open_transactions,
        }
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
    args.auth_json = args.auth_json.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("Stone source", args.source),
        ("Codex auth", args.auth_json),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    result = run_experiment(args)
    summary_path = args.run_dir.resolve() / "summary.json"
    summary_path.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(
        json.dumps(
            {
                "ok": result["ok"],
                "duration_seconds": result["duration_seconds"],
                "metrics": result["metrics"],
                "gate_violations": result["gate_violations"],
                "summary": str(summary_path),
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
