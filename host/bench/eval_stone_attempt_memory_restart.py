#!/usr/bin/env python3
"""Compare no memory, raw transcript, and attempt memory across controller restart."""

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

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
ACTION_TOKEN = "act-a43f910be8924feaa781"
MODES = ["N", "T", "M"]
MODE_NAMES = {"N": "none", "T": "raw_transcript", "M": "attempt_memory"}


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
        default=ROOT / "examples/scripts/attempt_memory_model_restart_experiment.stone",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-attempt-memory-restart-v1",
    )
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


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


def transition_phases(payload: dict[str, Any]) -> dict[str, list[str]]:
    phases: dict[str, list[str]] = {}
    events = (payload.get("diagnostics") or {}).get("transitions") or []
    for event in events:
        if not isinstance(event, dict):
            continue
        transition_id = event.get("id")
        phase = event.get("phase")
        if isinstance(transition_id, str) and isinstance(phase, str):
            phases.setdefault(transition_id, []).append(phase)
    return phases


def input_tokens(value: dict[str, Any]) -> int:
    total = 0
    for usage in value.get("usage") or []:
        if isinstance(usage, dict) and isinstance(usage.get("input_tokens"), int):
            total += usage["input_tokens"]
    return total


def selected_decision(value: Any) -> Any:
    if isinstance(value, dict):
        return value.get("selected_action")
    return value


def experiment_gate(
    cells: dict[str, dict[str, Any]],
    *,
    action_token: str,
    model: str,
) -> tuple[bool, list[str], dict[str, Any]]:
    violations: list[str] = []
    metrics: dict[str, Any] = {"cells": {}}
    expected_final = {"N": "insufficient_evidence", "T": action_token, "M": action_token}

    for mode in MODES:
        cell = cells.get(mode)
        if not isinstance(cell, dict):
            violations.append(f"missing {mode} cell")
            continue
        seed = cell.get("seed")
        restore = cell.get("restore")
        info = cell.get("attempt_info") or {}
        if not isinstance(seed, dict) or seed.get("ok") is not True:
            violations.append(f"{mode} seed controller did not return ok=true")
            continue
        if not isinstance(restore, dict) or restore.get("ok") is not True:
            violations.append(f"{mode} restore controller did not return ok=true")
            continue
        seed_value = seed.get("value")
        value = restore.get("value")
        if not isinstance(seed_value, dict) or seed_value.get("phase") != "seed":
            violations.append(f"{mode} first controller did not seed the attempt")
            continue
        if not isinstance(value, dict) or value.get("phase") != "restore":
            violations.append(f"{mode} second controller did not restore the attempt")
            continue
        if seed_value.get("mode") != mode or value.get("mode") != mode:
            violations.append(f"{mode} cell reported a different mode")

        run_count = (info.get("metadata") or {}).get("controller_run_count")
        if run_count != "2":
            violations.append(f"{mode} controller run count is {run_count!r}, expected '2'")
        memory_revision = int(info.get("memory_revision") or 0)
        expected_revision = 5 if mode == "M" else 0
        if memory_revision != expected_revision:
            violations.append(
                f"{mode} memory revision is {memory_revision}, expected {expected_revision}"
            )

        seed_phases = transition_phases(seed)
        early_id = seed_value.get("early_transition_id")
        if seed_phases.get(str(early_id)) != ["start", "effect", "post"]:
            violations.append(f"{mode} seed action lacks start/effect/post phases")

        decisions = value.get("decisions")
        if not isinstance(decisions, list) or len(decisions) != 2:
            violations.append(f"{mode} did not make exactly two decisions")
        else:
            if selected_decision(decisions[0]) != "diagnostic_probe":
                violations.append(f"{mode} did not select the diagnostic probe on turn 0")
            if selected_decision(decisions[1]) != expected_final[mode]:
                violations.append(
                    f"{mode} final decision is {selected_decision(decisions[1])!r}, expected {expected_final[mode]!r}"
                )
        if value.get("selected_action") != expected_final[mode]:
            violations.append(f"{mode} selected_action differs from the expected final action")
        if value.get("materialized_action") != value.get("selected_action"):
            violations.append(f"{mode} did not materialize its selected final action")
        failed_probe = value.get("failed_probe")
        if not isinstance(failed_probe, dict) or failed_probe.get("exit_code") != 7:
            violations.append(f"{mode} did not observe the frozen misleading failure")
        if isinstance(failed_probe, dict) and failed_probe.get("ok") is not False:
            violations.append(f"{mode} mislabeled the failed probe as successful")
        if value.get("provider") != "codex-chatgpt":
            violations.append(f"{mode} used provider {value.get('provider')!r}")
        if value.get("model") != model:
            violations.append(f"{mode} used model {value.get('model')!r}")

        phases = transition_phases(restore)
        transition_ids = (value.get("model_transition_ids") or []) + (
            value.get("action_transition_ids") or []
        )
        if len(transition_ids) != 4 or len(set(transition_ids)) != 4:
            violations.append(f"{mode} does not expose four unique decision/action transitions")
        for transition_id in transition_ids:
            if phases.get(str(transition_id)) != ["start", "pre", "effect", "post"]:
                violations.append(
                    f"{mode} transition {transition_id!r} lacks start/pre/effect/post phases"
                )

        retained = value.get("retained")
        if mode == "M":
            keys = sorted(
                item.get("key") for item in retained or [] if isinstance(item, dict)
            )
            if keys != ["decision.last", "outcome.last_tool", "requirement.pivot_action"]:
                violations.append(f"M hot memory keys differ from the bounded three-key ledger: {keys}")
        elif retained != []:
            violations.append(f"{mode} unexpectedly returned managed memory")

        metrics["cells"][mode] = {
            "name": MODE_NAMES[mode],
            "expected_behavior": value.get("selected_action") == expected_final[mode],
            "task_success": value.get("selected_action") == action_token,
            "input_tokens": input_tokens(value),
            "memory_revision": memory_revision,
            "controller_runs": int(run_count or 0),
            "model_calls": len(value.get("usage") or []),
            "tool_calls": len(value.get("action_transition_ids") or []),
            "redundant_probe_retries": max(
                0,
                sum(
                    selected_decision(decision) == "diagnostic_probe"
                    for decision in value.get("decisions") or []
                )
                - 1,
            ),
        }

    managed_tokens = metrics.get("cells", {}).get("M", {}).get("input_tokens", 0)
    transcript_tokens = metrics.get("cells", {}).get("T", {}).get("input_tokens", 0)
    if managed_tokens <= 0 or transcript_tokens <= 0:
        violations.append("model usage did not expose positive input-token counts")
    elif transcript_tokens <= managed_tokens:
        violations.append(
            f"raw transcript used {transcript_tokens} input tokens, not more than memory's {managed_tokens}"
        )
    metrics["transcript_minus_memory_input_tokens"] = transcript_tokens - managed_tokens
    metrics["memory_input_token_reduction_fraction"] = (
        (transcript_tokens - managed_tokens) / transcript_tokens if transcript_tokens else 0.0
    )
    return not violations, violations, metrics


def gateway_command(
    gateway_bin: Path,
    data_root: Path,
    *args: str,
    env: dict[str, str] | None = None,
    timeout: float = 60.0,
) -> str:
    completed = base.run_capture(
        [str(gateway_bin), "--data-root", str(data_root), *args],
        env=env,
        timeout=timeout,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"Gateway command failed: {args}\nstdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    return completed.stdout


def response_from_logs(text: str) -> dict[str, Any] | None:
    completed = subprocess.CompletedProcess([], 0, text, "")
    return base.response_payload(completed)


def run_cell(
    args: argparse.Namespace,
    *,
    mode: str,
    data_root: Path,
    fixtures_root: Path,
    client_env: dict[str, str],
) -> dict[str, Any]:
    workspace = f"restart-memory-{mode.lower()}"
    source_root = fixtures_root / mode.lower()
    source_root.mkdir(parents=True)
    (source_root / "mode.txt").write_text(mode + "\n", encoding="utf-8")
    (source_root / "early_requirement.json").write_text(
        json.dumps({"action_token": ACTION_TOKEN}, separators=(",", ":")) + "\n",
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
    attempt = gateway_command(
        args.gateway_bin,
        data_root,
        "attempt",
        "spawn",
        "--task",
        f"restart-memory-{mode.lower()}",
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
    rolled_back = False
    try:
        first_info_text = gateway_command(
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
        seed_logs = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            attempt,
            "--max-bytes",
            "262144",
            env=client_env,
        )
        second_info_text = gateway_command(
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
        restore_logs = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            attempt,
            "--max-bytes",
            "524288",
            env=client_env,
        )
        cell = {
            "mode": mode,
            "attempt": attempt,
            "first_attempt_info": parse_attempt_info(first_info_text),
            "attempt_info": parse_attempt_info(second_info_text),
            "seed": response_from_logs(seed_logs),
            "restore": response_from_logs(restore_logs),
            "seed_logs": seed_logs,
            "restore_logs": restore_logs,
        }
        return cell
    finally:
        finish = gateway_command(
            args.gateway_bin,
            data_root,
            "attempt",
            "finish",
            attempt,
            "--rollback",
            "--reason",
            "memory experiment cleanup",
            env=client_env,
        )
        rolled_back = "state\trolled_back" in finish
        if not rolled_back:
            raise RuntimeError(f"attempt {attempt} did not roll back cleanly: {finish}")


def run_experiment(
    args: argparse.Namespace,
    *,
    enforce_frozen_source: bool = True,
) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    source_bytes = args.source.read_bytes()
    if ACTION_TOKEN.encode() in source_bytes:
        raise RuntimeError("opaque action token must not appear in Stone source")
    if enforce_frozen_source:
        required_fragments = [
            b'hooks={"post": retain_early_requirement}',
            b'hooks={"pre": prepare_decision, "post": record_decision}',
            b'hooks={"pre": check_action, "post": record_outcome}',
            b'for turn in range(2)',
        ]
        missing = [
            fragment.decode() for fragment in required_fragments if fragment not in source_bytes
        ]
        if missing:
            raise RuntimeError(f"Stone source differs from the frozen experiment: {missing}")

    manifest = {
        "schema": "waymark.stone-attempt-memory-restart.v1",
        "frozen_at_unix": int(time.time()),
        "source": str(args.source),
        "source_sha256": sha256_bytes(source_bytes),
        "source_contract": "frozen" if enforce_frozen_source else "authored",
        "action_token_sha256": sha256_bytes(ACTION_TOKEN.encode()),
        "modes": MODES,
        "mode_names": MODE_NAMES,
        "model": args.model,
        "provider": "codex-chatgpt",
        "reasoning_effort": args.reasoning_effort,
        "image": args.image,
        "gateway_binary_sha256": sha256_bytes(args.gateway_bin.read_bytes()),
        "waymark_binary_sha256": sha256_bytes(args.waymark_bin.read_bytes()),
    }
    (run_dir / "frozen-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    data_root = run_dir / "gateway-data"
    fixtures_root = run_dir / "fixtures"
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-memory-restart-", dir="/tmp"))
    socket_path = socket_root / "gateway.sock"
    server_env = dict(os.environ)
    server_env.update(
        {
            "WAYMARK_MODEL_PROVIDER": "codex-chatgpt",
            "WAYMARK_MODEL_CODEX_AUTH_JSON": str(args.auth_json),
            "WAYMARK_MODEL": args.model,
            "WAYMARK_MODEL_ALLOWLIST": args.model,
            "WAYMARK_MODEL_REASONING_EFFORT": args.reasoning_effort,
        }
    )
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
    client_env = dict(os.environ)
    client_env.update(
        {
            "WAYMARK_STONE_BIN": str(args.waymark_bin),
            "WAYMARK_GATEWAY_SOCKET": str(socket_path),
            "WAYMARK_GATEWAY_IMAGE": args.image,
            "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
            "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
        }
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
        gate_ok, violations, metrics = experiment_gate(
            cells,
            action_token=ACTION_TOKEN,
            model=args.model,
        )
        open_transactions = gateway_command(
            args.gateway_bin,
            data_root,
            "env",
            "list-tx",
            env=client_env,
        ).strip()
        if open_transactions:
            gate_ok = False
            violations.append("experiment left open transactions")
        return {
            "schema": "waymark.stone-attempt-memory-restart-result.v1",
            "ok": gate_ok,
            "duration_seconds": time.monotonic() - started,
            "gate_ok": gate_ok,
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
