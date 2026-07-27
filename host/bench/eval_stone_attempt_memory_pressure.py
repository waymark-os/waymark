#!/usr/bin/env python3
"""Compare append-only and keyed attempt memory under hot-store pressure."""

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
MODES = ("A", "K")
MODE_NAMES = {"A": "append_only", "K": "keyed_window"}
CURRENT_TARGET = "current-target-c7a40d9e"
OLD_TARGET = "obsolete-target-81b2f630"
SEED_REVISION = 323
EXPECTED_ITEM_COUNTS = {"A": 256, "K": 20}
EXPECTED_PRESSURE_COUNTS = {"A": 252, "K": 16}


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
        default=ROOT / "examples/scripts/attempt_memory_pressure_experiment.stone",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-attempt-memory-pressure-v1",
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
    phases = []
    for event in (payload.get("diagnostics") or {}).get("transitions") or []:
        if event.get("id") == transition_id and isinstance(event.get("phase"), str):
            phases.append(event["phase"])
    return phases


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
    old_target: str,
) -> tuple[bool, list[str], dict[str, Any]]:
    violations: list[str] = []
    metrics: dict[str, Any] = {"cells": {}, "model_calls": 0, "input_tokens": 0}

    for mode in MODES:
        cell = cells.get(mode)
        seed = (cell or {}).get("seed") if isinstance(cell, dict) else None
        restore = (cell or {}).get("restore") if isinstance(cell, dict) else None
        if not isinstance(seed, dict) or seed.get("ok") is not True:
            violations.append(f"{mode} seed controller did not return ok=true")
            continue
        if not isinstance(restore, dict) or restore.get("ok") is not True:
            violations.append(f"{mode} restore controller did not return ok=true")
            continue
        seed_value = seed.get("value")
        value = restore.get("value")
        if not isinstance(seed_value, dict) or seed_value.get("phase") != "seed":
            violations.append(f"{mode} did not return a seed result")
            continue
        if not isinstance(value, dict) or value.get("phase") != "restore":
            violations.append(f"{mode} did not return a restore result")
            continue
        info = cell.get("root_info") or {}
        child_info = cell.get("child_info") or {}
        child_payload = cell.get("child_payload") or {}
        child = value.get("child_result")
        if not isinstance(child, dict):
            violations.append(f"{mode} parent did not join a structured child result")
            continue

        expected_count = EXPECTED_ITEM_COUNTS[mode]
        expected_pressure = EXPECTED_PRESSURE_COUNTS[mode]
        for label, actual in (
            ("seed item count", seed_value.get("item_count")),
            ("parent pre-fork item count", value.get("parent_item_count_before")),
            ("parent final item count", value.get("parent_item_count_after")),
            ("child pre-model item count", child.get("item_count_before")),
            ("child final item count", child.get("item_count_after")),
        ):
            if actual != expected_count:
                violations.append(f"{mode} {label} is {actual}, expected {expected_count}")
        if seed_value.get("pressure_item_count") != expected_pressure:
            violations.append(
                f"{mode} pressure item count is {seed_value.get('pressure_item_count')}, expected {expected_pressure}"
            )
        if seed_value.get("latest_decision_sequence") != 63:
            violations.append(f"{mode} did not retain the latest decision revision")
        if seed_value.get("latest_pressure_sequence") != 251:
            violations.append(f"{mode} did not retain the latest pressure observation")

        for label, actual in (
            ("seed revision", seed_value.get("memory_revision")),
            ("parent pre-fork revision", value.get("parent_revision_before")),
            ("fork origin revision", value.get("fork_origin_revision")),
            ("child projection revision", child.get("projection_revision")),
        ):
            if actual != SEED_REVISION:
                violations.append(f"{mode} {label} is {actual}, expected {SEED_REVISION}")
        if value.get("parent_revision_after") != SEED_REVISION + 1:
            violations.append(f"{mode} parent promotion did not revise one stable key")
        if child.get("memory_revision") != SEED_REVISION + 1:
            violations.append(f"{mode} child post-hook did not revise one stable key")
        if info.get("memory_revision") != str(SEED_REVISION + 1):
            violations.append(f"{mode} root info has the wrong final revision")
        if child_info.get("memory_revision") != str(SEED_REVISION + 1):
            violations.append(f"{mode} child info has the wrong final revision")
        if (info.get("metadata") or {}).get("controller_run_count") != "2":
            violations.append(f"{mode} did not cross a controller restart")

        if seed_value.get("requirement_target") != current_target:
            violations.append(f"{mode} seed did not retain the current requirement")
        if value.get("requirement_target") != current_target:
            violations.append(f"{mode} restart lost the current requirement")
        if seed_value.get("requirement_supersedes") != "memory-item-1":
            violations.append(f"{mode} requirement did not supersede its obsolete revision")
        if value.get("requirement_supersedes") != "memory-item-1":
            violations.append(f"{mode} fork frontier lost supersession provenance")
        if seed_value.get("resolved_risk_count") != 0 or value.get("resolved_risk_count") != 0:
            violations.append(f"{mode} archived risk remained in the hot frontier")

        keys = child.get("projection_keys")
        if not isinstance(keys, list) or "requirement.target" not in keys:
            violations.append(f"{mode} projection omitted the current requirement: {keys}")
        if isinstance(keys, list) and "risk.resolved" in keys:
            violations.append(f"{mode} projection included archived state")
        if child.get("projection_contains_current") is not True:
            violations.append(f"{mode} projection did not contain the current opaque target")
        if child.get("projection_contains_old") is not False:
            violations.append(f"{mode} projection leaked the obsolete opaque target")
        if child.get("selected_target") != current_target:
            violations.append(
                f"{mode} selected {child.get('selected_target')!r}, expected {current_target!r}"
            )
        if child.get("selected_target") == old_target:
            violations.append(f"{mode} was steered by the superseded target")
        if child.get("projection_estimated_tokens", 10000) > 160:
            violations.append(f"{mode} projection exceeded its token budget")
        if child.get("projection_truncated") is not True:
            violations.append(f"{mode} pressure projection was not marked truncated")
        if child.get("provider") != "codex-chatgpt" or child.get("model") != model:
            violations.append(f"{mode} used an unexpected model provider")
        if value.get("clean") is not True:
            violations.append(f"{mode} did not close its child scope")
        phases = transition_phases(child_payload, str(child.get("transition_id") or ""))
        if phases != ["start", "pre", "effect", "post"]:
            violations.append(f"{mode} model transition phases are {phases}")

        tokens = input_tokens(child)
        metrics["model_calls"] += 1
        metrics["input_tokens"] += tokens
        metrics["cells"][mode] = {
            "name": MODE_NAMES[mode],
            "hot_items": expected_count,
            "pressure_items": expected_pressure,
            "memory_file_bytes": cell.get("root_memory_bytes"),
            "child_memory_file_bytes": cell.get("child_memory_bytes"),
            "projection_tokens": child.get("projection_estimated_tokens"),
            "model_input_tokens": tokens,
            "selected_target": child.get("selected_target"),
        }

    append_bytes = metrics.get("cells", {}).get("A", {}).get("memory_file_bytes", 0)
    keyed_bytes = metrics.get("cells", {}).get("K", {}).get("memory_file_bytes", 0)
    if not isinstance(append_bytes, int) or not isinstance(keyed_bytes, int):
        violations.append("memory file sizes were not recorded")
    elif keyed_bytes <= 0 or append_bytes <= keyed_bytes * 4:
        violations.append(
            f"append-only memory used {append_bytes} bytes versus keyed memory's {keyed_bytes}; expected >4x"
        )
    metrics["append_minus_keyed_bytes"] = append_bytes - keyed_bytes
    metrics["keyed_storage_reduction_fraction"] = (
        (append_bytes - keyed_bytes) / append_bytes if append_bytes else 0.0
    )
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
            "attempt memory pressure cleanup",
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
    workspace = f"memory-pressure-{mode.lower()}"
    source_root = fixtures_root / mode.lower()
    source_root.mkdir(parents=True)
    (source_root / "mode.txt").write_text(mode + "\n", encoding="utf-8")
    (source_root / "targets.json").write_text(
        json.dumps({"current": CURRENT_TARGET, "old": OLD_TARGET}, separators=(",", ":"))
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
        f"memory-pressure-{mode.lower()}",
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
    all_logs = ""
    child = ""
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
        seed_logs = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            root_attempt,
            "--max-bytes",
            "1048576",
            env=client_env,
        ).stdout
        all_logs += seed_logs
        seed = response_from_logs(seed_logs)

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
        restore_logs = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            root_attempt,
            "--max-bytes",
            "1048576",
            env=client_env,
        ).stdout
        all_logs += restore_logs
        restore = response_from_logs(restore_logs)
        if not isinstance(restore, dict):
            raise AssertionError(f"{mode} restore logs contain no Stone response: {restore_logs}")
        value = restore.get("value") or {}
        child = str(value.get("child") or "")
        if not child:
            raise AssertionError(f"{mode} restore returned no child: {restore}")
        child_logs = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            child,
            "--max-bytes",
            "524288",
            env=client_env,
        ).stdout
        all_logs += child_logs
        root_info = parse_attempt_info(
            gateway_command(
                args.gateway_bin,
                data_root,
                "attempt",
                "info",
                root_attempt,
                env=client_env,
            ).stdout
        )
        child_info = parse_attempt_info(
            gateway_command(
                args.gateway_bin,
                data_root,
                "attempt",
                "info",
                child,
                env=client_env,
            ).stdout
        )
        root_memory = data_root / "attempts" / root_attempt / "memory-v0.json"
        child_memory = data_root / "attempts" / child / "memory-v0.json"
        return {
            "mode": mode,
            "root_attempt": root_attempt,
            "child_attempt": child,
            "seed": seed,
            "restore": restore,
            "child_payload": response_from_logs(child_logs),
            "root_info": root_info,
            "child_info": child_info,
            "root_memory_bytes": root_memory.stat().st_size,
            "child_memory_bytes": child_memory.stat().st_size,
            "seed_logs": seed_logs,
            "restore_logs": restore_logs,
            "child_logs": child_logs,
        }
    finally:
        for attempt in related_attempt_ids(
            args.gateway_bin,
            data_root,
            workspace,
            root_attempt,
            all_logs,
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
        for value in (CURRENT_TARGET, OLD_TARGET)
        if value.encode() in source_bytes
    ]
    if leaked:
        raise RuntimeError(f"opaque target leaked into Stone source: {leaked}")
    required = (
        b'for sequence in range(64):',
        b'for index in range(252):',
        b'key = "observation.slot." + str(index % 16)',
        b'status="archived"',
        b"child_result = outcome.result.value",
    )
    missing = [fragment.decode() for fragment in required if fragment not in source_bytes]
    if missing:
        raise RuntimeError(f"Stone source differs from pressure contract: {missing}")

    manifest = {
        "schema": "waymark.stone-attempt-memory-pressure.v1",
        "source": str(args.source),
        "source_sha256": sha256_bytes(source_bytes),
        "current_target_sha256": sha256_bytes(CURRENT_TARGET.encode()),
        "old_target_sha256": sha256_bytes(OLD_TARGET.encode()),
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
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-memory-pressure-", dir="/tmp"))
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
            old_target=OLD_TARGET,
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
            "schema": "waymark.stone-attempt-memory-pressure-result.v1",
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
