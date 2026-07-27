#!/usr/bin/env python3
"""Run the real-model M-vs-N Stone attempt-memory causal canary."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
ACTION_TOKEN = "act-6f19bce247a54d8aa901"
CELL_ORDER = ["N", "M", "M", "N"]
RECENT_MESSAGE_COUNT = 18


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
        default=ROOT / "examples/scripts/transition_memory_behavioral_canary.stone",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--image", default="python:3.12")
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-behavioral-memory-v1",
    )
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def phase_map(payload: dict[str, Any]) -> dict[str, list[str]]:
    events = (payload.get("diagnostics") or {}).get("transitions") or []
    phases: dict[str, list[str]] = {}
    for event in events:
        if not isinstance(event, dict):
            continue
        transition_id = event.get("id")
        phase = event.get("phase")
        if isinstance(transition_id, str) and isinstance(phase, str):
            phases.setdefault(transition_id, []).append(phase)
    return phases


def token_totals(results: list[dict[str, Any]], mode: str, field: str) -> int:
    total = 0
    for result in results:
        if result.get("mode") != mode:
            continue
        usage = result.get("usage")
        if isinstance(usage, dict) and isinstance(usage.get(field), int):
            total += usage[field]
    return total


def behavioral_gate(
    payload: dict[str, Any] | None,
    *,
    action_token: str,
    model: str,
) -> tuple[bool, list[str], dict[str, Any]]:
    violations: list[str] = []
    if not isinstance(payload, dict) or payload.get("ok") is not True:
        return False, ["Stone execution did not return ok=true"], {}
    value = payload.get("value")
    if not isinstance(value, dict):
        return False, ["Stone result is not a record"], {}

    results = value.get("results")
    if not isinstance(results, list) or len(results) != len(CELL_ORDER):
        return False, ["Stone result lacks the four frozen cells"], {}
    modes = [result.get("mode") for result in results if isinstance(result, dict)]
    if modes != CELL_ORDER:
        violations.append(f"cell order differs from frozen order: {modes}")
    if value.get("recent_message_count") != RECENT_MESSAGE_COUNT:
        violations.append("recent-message window differs from the frozen count")

    retained = value.get("retained")
    early_transition_id = value.get("early_transition_id")
    if not isinstance(retained, list) or len(retained) != 1:
        violations.append("early requirement was not retained exactly once")
    else:
        item = retained[0]
        content = item.get("content") if isinstance(item, dict) else None
        if item.get("key") != "requirement.pivot_action":
            violations.append("retained item has the wrong key")
        if not isinstance(content, dict) or content.get("action_token") != action_token:
            violations.append("retained item lacks the fixture action token")
        if not isinstance(content, dict) or content.get("source_transition_id") != early_transition_id:
            violations.append("retained item is not linked to the early tool transition")

    phases = phase_map(payload)
    if phases.get(str(early_transition_id)) != ["start", "effect", "post"]:
        violations.append("early tool transition lacks start/effect/post phases")

    all_ids: list[str] = []
    m_correct = 0
    n_correct = 0
    for index, result in enumerate(results):
        if not isinstance(result, dict):
            violations.append(f"cell {index} is not a record")
            continue
        mode = result.get("mode")
        selected = result.get("selected_action")
        materialized = result.get("materialized_action")
        if not isinstance(selected, str) or not selected:
            violations.append(f"cell {index} lacks a selected action string")
        if materialized != selected:
            violations.append(f"cell {index} did not materialize the selected action")
        if result.get("provider") != "codex-chatgpt":
            violations.append(f"cell {index} used the wrong provider")
        if result.get("model") != model:
            violations.append(f"cell {index} used the wrong model")
        if mode == "M" and selected == action_token:
            m_correct += 1
        if mode == "N" and selected == action_token:
            n_correct += 1

        model_transition_id = result.get("model_transition_id")
        action_transition_id = result.get("action_transition_id")
        if isinstance(model_transition_id, str):
            all_ids.append(model_transition_id)
        if isinstance(action_transition_id, str):
            all_ids.append(action_transition_id)
        expected_model_phases = ["start", "pre", "effect"] if mode == "M" else ["start", "effect"]
        if phases.get(str(model_transition_id)) != expected_model_phases:
            violations.append(
                f"cell {index} model phases differ: {phases.get(str(model_transition_id))}"
            )
        if phases.get(str(action_transition_id)) != ["start", "effect"]:
            violations.append(f"cell {index} action transition phases differ")

    if len(all_ids) != len(set(all_ids)):
        violations.append("dynamic model/action transitions do not have unique ids")
    if m_correct != CELL_ORDER.count("M"):
        violations.append(f"memory treatment selected the correct action {m_correct}/2 times")
    if n_correct != 0:
        violations.append(f"no-memory control selected the hidden action {n_correct}/2 times")

    context_events = (payload.get("diagnostics") or {}).get("context", {}).get("events") or []
    writes = [event for event in context_events if isinstance(event, dict) and event.get("op") == "write"]
    projects = [
        event for event in context_events if isinstance(event, dict) and event.get("op") == "project"
    ]
    if [event.get("key") for event in writes] != ["requirement.pivot_action"]:
        violations.append("context writes differ from the one frozen retention event")
    if len(projects) != CELL_ORDER.count("M") or any(not event.get("selected") for event in projects):
        violations.append("memory cells did not each activate retained state")

    metrics = {
        "m_correct": m_correct,
        "m_total": CELL_ORDER.count("M"),
        "n_correct": n_correct,
        "n_total": CELL_ORDER.count("N"),
        "m_input_tokens": token_totals(results, "M", "input_tokens"),
        "n_input_tokens": token_totals(results, "N", "input_tokens"),
        "m_output_tokens": token_totals(results, "M", "output_tokens"),
        "n_output_tokens": token_totals(results, "N", "output_tokens"),
        "context_writes": len(writes),
        "context_projections": len(projects),
        "model_calls": len(results),
    }
    return not violations, violations, metrics


def run_experiment(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    source_bytes = args.source.read_bytes()
    if ACTION_TOKEN.encode() in source_bytes:
        raise RuntimeError("fixture action token must not appear in the Stone source")
    required_source_fragments = [
        b'hooks={"post": retain_requirement}',
        b'hooks={"pre": prepare_decision}',
        b'for index in range(16)',
        b'modes = ["N", "M", "M", "N"]',
    ]
    missing = [fragment.decode() for fragment in required_source_fragments if fragment not in source_bytes]
    if missing:
        raise RuntimeError(f"Stone source differs from the frozen behavioral design: {missing}")

    manifest = {
        "schema": "waymark.stone-behavioral-memory.v1",
        "frozen_at_unix": int(time.time()),
        "source": str(args.source.resolve()),
        "source_sha256": sha256_bytes(source_bytes),
        "fixture_action_token_sha256": sha256_bytes(ACTION_TOKEN.encode()),
        "cell_order": CELL_ORDER,
        "recent_message_count": RECENT_MESSAGE_COUNT,
        "model": args.model,
        "provider": "codex-chatgpt",
        "reasoning_effort": args.reasoning_effort,
        "image": args.image,
        "gateway_binary_sha256": sha256_bytes(args.gateway_bin.read_bytes()),
        "waymark_binary_sha256": sha256_bytes(args.waymark_bin.read_bytes()),
    }
    (run_dir / "frozen-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    data_root = run_dir / "gateway-data"
    source_root = run_dir / "source"
    work = run_dir / "work"
    source_root.mkdir()
    work.mkdir()
    (source_root / "early_requirement.json").write_text(
        json.dumps({"action_token": ACTION_TOKEN}, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    (source_root / "README.md").write_text(
        "Behavioral memory canary fixture. The opaque token is early evidence only.\n",
        encoding="utf-8",
    )
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
        "--workspace-mount",
        "/app",
    )

    socket_path = run_dir / "gateway.sock"
    server_env = dict(os.environ)
    server_env.update(
        {
            "WAYMARK_MODEL_PROVIDER": "codex-chatgpt",
            "WAYMARK_MODEL_CODEX_AUTH_JSON": str(args.auth_json.resolve()),
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
    try:
        base.wait_for_socket(socket_path, server)
        env = dict(os.environ)
        env.update(
            {
                "WAYMARK_START_DIR": str(work),
                "WAYMARK_GATEWAY_SOCKET": str(socket_path),
                "WAYMARK_GATEWAY_TX": tx,
                "WAYMARK_GATEWAY_IMAGE": args.image,
                "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
                "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
            }
        )
        started = time.monotonic()
        completed = base.run_capture(
            [str(args.waymark_bin), "eval", str(args.source.resolve())],
            cwd=work,
            env=env,
            timeout=args.timeout,
        )
        elapsed = time.monotonic() - started
        payload = base.response_payload(completed)
        gate_ok, violations, metrics = behavioral_gate(
            payload,
            action_token=ACTION_TOKEN,
            model=args.model,
        )
        return {
            "schema": "waymark.stone-behavioral-memory-result.v1",
            "ok": completed.returncode == 0 and gate_ok,
            "exit_code": completed.returncode,
            "duration_seconds": elapsed,
            "gate_ok": gate_ok,
            "gate_violations": violations,
            "metrics": metrics,
            "response": payload,
            "stdout": completed.stdout,
            "stderr": completed.stderr,
            "manifest": manifest,
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
        base.gateway_command(args.gateway_bin, data_root, "env", "rollback", tx)
        socket_path.unlink(missing_ok=True)


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
    summary_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
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
