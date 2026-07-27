#!/usr/bin/env python3
"""Compare model behavior in memory-inheriting fork and empty-memory spawn children."""

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
ARMS = ("F", "S")
TARGET = "candidate-cobalt-731"
CANDIDATES = (TARGET, "candidate-amber-284")


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
        default=ROOT / "examples/scripts/attempt_memory_fork_model_experiment.stone",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-attempt-memory-fork-model-v1",
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


def usage_input_tokens(result: dict[str, Any]) -> int:
    usage = result.get("usage")
    if not isinstance(usage, dict):
        return 0
    value = usage.get("input_tokens")
    return value if isinstance(value, int) else 0


def experiment_gate(
    cells: dict[str, dict[str, Any]],
    *,
    target: str,
    candidates: tuple[str, str],
    model: str,
) -> tuple[bool, list[str], dict[str, Any]]:
    violations: list[str] = []
    metrics: dict[str, Any] = {"cells": {}, "model_calls": 0, "input_tokens": 0}

    for arm in ARMS:
        cell = cells.get(arm)
        payload = (cell or {}).get("payload") if isinstance(cell, dict) else None
        if not isinstance(payload, dict) or payload.get("ok") is not True:
            violations.append(f"{arm} controller did not return ok=true")
            continue
        value = payload.get("value")
        if not isinstance(value, dict) or value.get("arm") != arm:
            violations.append(f"{arm} returned an invalid arm result")
            continue
        children = value.get("children")
        if not isinstance(children, list) or len(children) != 2:
            violations.append(f"{arm} did not return exactly two child results")
            continue
        by_candidate: dict[str, dict[str, Any]] = {}
        for child in children:
            result = child.get("result") if isinstance(child, dict) else None
            if not isinstance(result, dict):
                violations.append(f"{arm} child lacks a structured joined result")
                continue
            candidate = result.get("candidate")
            if candidate not in candidates:
                violations.append(f"{arm} child returned unknown candidate {candidate!r}")
                continue
            by_candidate[str(candidate)] = result
            if result.get("provider") != "codex-chatgpt":
                violations.append(f"{arm}/{candidate} used provider {result.get('provider')!r}")
            if result.get("model") != model:
                violations.append(f"{arm}/{candidate} used model {result.get('model')!r}")
            metrics["model_calls"] += 1
            metrics["input_tokens"] += usage_input_tokens(result)

        if set(by_candidate) != set(candidates):
            violations.append(f"{arm} did not cover both candidates")
            continue

        decisions = {candidate: result.get("decision") for candidate, result in by_candidate.items()}
        projected = {
            candidate: result.get("projected_keys") for candidate, result in by_candidate.items()
        }
        revisions = {
            candidate: result.get("memory_revision") for candidate, result in by_candidate.items()
        }
        if arm == "F":
            expected = {target: "select", candidates[1]: "reject"}
            if decisions != expected:
                violations.append(f"F decisions are {decisions}, expected {expected}")
            if any(keys != ["requirement.target"] for keys in projected.values()):
                violations.append(f"F children did not inherit the target projection: {projected}")
            if any(revision != 2 for revision in revisions.values()):
                violations.append(f"F child memory revisions are not inherited+local: {revisions}")
            accepted_candidates = [
                child["result"]["candidate"]
                for child in children
                if child.get("attempt") == value.get("accepted")
            ]
            if accepted_candidates != [target]:
                violations.append(f"F accepted the wrong child: {accepted_candidates}")
            if value.get("promoted") is not True:
                violations.append("F did not explicitly promote the selected result")
            if value.get("parent_memory_revision") != 2:
                violations.append("F parent memory revision is not seed+promotion")
            if set(value.get("parent_keys") or []) != {
                "requirement.target",
                "candidate.accepted",
            }:
                violations.append(f"F parent memory contains leaked child state: {value.get('parent_keys')}")
        else:
            expected = {candidate: "insufficient" for candidate in candidates}
            if decisions != expected:
                violations.append(f"S decisions are {decisions}, expected {expected}")
            if any(keys != [] for keys in projected.values()):
                violations.append(f"S children unexpectedly inherited parent memory: {projected}")
            if any(revision != 1 for revision in revisions.values()):
                violations.append(f"S child memory revisions are not local-only: {revisions}")
            if value.get("accepted") != "" or value.get("promoted") is not False:
                violations.append("S selected or promoted without inherited evidence")
            if value.get("parent_memory_revision") != 1:
                violations.append("S parent memory changed after its seed")
            if set(value.get("parent_keys") or []) != {"requirement.target"}:
                violations.append(f"S parent memory contains leaked child state: {value.get('parent_keys')}")

        if value.get("clean") is not True:
            violations.append(f"{arm} did not close its child scope cleanly")
        metrics["cells"][arm] = {
            "decisions": decisions,
            "projected_keys": projected,
            "child_memory_revisions": revisions,
            "parent_memory_revision": value.get("parent_memory_revision"),
            "accepted": value.get("accepted"),
            "promoted": value.get("promoted"),
            "input_tokens": sum(usage_input_tokens(result) for result in by_candidate.values()),
        }

    if metrics["model_calls"] != 4:
        violations.append(f"experiment made {metrics['model_calls']} model calls, expected 4")
    if metrics["input_tokens"] <= 0:
        violations.append("model calls exposed no positive input-token usage")
    return not violations, violations, metrics


def child_attempt_ids(
    gateway_bin: Path,
    data_root: Path,
    workspace: str,
    root_attempt: str,
    logs: str,
    client_env: dict[str, str],
) -> list[str]:
    found = set(re.findall(r"attempt-\d+-\d+", logs))
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
    found.update(re.findall(r"attempt-\d+-\d+", listed.stdout))
    found.discard(root_attempt)
    return sorted(found)


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
    if info.returncode != 0:
        return
    if parse_attempt_info(info.stdout).get("state") == "active":
        gateway_command(
            gateway_bin,
            data_root,
            "attempt",
            "finish",
            attempt,
            "--rollback",
            "--reason",
            "fork model experiment cleanup",
            env=client_env,
        )


def run_cell(
    args: argparse.Namespace,
    *,
    arm: str,
    data_root: Path,
    fixtures_root: Path,
    client_env: dict[str, str],
) -> dict[str, Any]:
    workspace = f"fork-model-{arm.lower()}"
    source_root = fixtures_root / arm.lower()
    source_root.mkdir(parents=True)
    (source_root / "README.md").write_text("fork model experiment fixture\n", encoding="utf-8")
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
    task_input = json.dumps(
        {"arm": arm, "target": TARGET, "candidates": list(CANDIDATES)},
        separators=(",", ":"),
    )
    root_attempt = gateway_command(
        args.gateway_bin,
        data_root,
        "attempt",
        "spawn",
        "--task",
        f"fork-model-{arm.lower()}",
        "--workspace",
        workspace,
        "--controller",
        "stone",
        "--workspace-mount",
        "/app",
        "--task-input-json",
        task_input,
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
            "1048576",
            env=client_env,
        ).stdout
        payload = response_from_logs(logs)
        if not isinstance(payload, dict):
            raise AssertionError(f"{arm} logs contain no Stone response: {logs}")
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
        return {
            "arm": arm,
            "root_attempt": root_attempt,
            "root_info": root_info,
            "payload": payload,
            "logs": logs,
        }
    finally:
        for child in child_attempt_ids(
            args.gateway_bin,
            data_root,
            workspace,
            root_attempt,
            logs,
            client_env,
        ):
            rollback_if_active(args.gateway_bin, data_root, child, client_env)
        rollback_if_active(args.gateway_bin, data_root, root_attempt, client_env)


def run_experiment(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    source_bytes = args.source.read_bytes()
    leaked = [candidate for candidate in CANDIDATES if candidate.encode() in source_bytes]
    if leaked:
        raise RuntimeError(f"opaque candidate leaked into Stone source: {leaked}")
    required = (
        b"projection = context_project(",
        b'"pre": lambda step: prepare_candidate_decision(step, projection)',
        b"first_outcome.result.value",
        b'context_write(\n            "candidate.accepted"',
    )
    missing = [fragment.decode() for fragment in required if fragment not in source_bytes]
    if missing:
        raise RuntimeError(f"Stone source differs from experiment contract: {missing}")

    manifest = {
        "schema": "waymark.stone-attempt-memory-fork-model.v1",
        "source": str(args.source),
        "source_sha256": sha256_bytes(source_bytes),
        "target_sha256": sha256_bytes(TARGET.encode()),
        "candidate_sha256": [sha256_bytes(candidate.encode()) for candidate in CANDIDATES],
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
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-fork-model-", dir="/tmp"))
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
        for arm in ARMS:
            cells[arm] = run_cell(
                args,
                arm=arm,
                data_root=data_root,
                fixtures_root=fixtures_root,
                client_env=client_env,
            )
        ok, violations, metrics = experiment_gate(
            cells,
            target=TARGET,
            candidates=CANDIDATES,
            model=args.model,
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
            "schema": "waymark.stone-attempt-memory-fork-model-result.v1",
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
