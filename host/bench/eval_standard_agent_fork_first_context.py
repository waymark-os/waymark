#!/usr/bin/env python3
"""Verify the standard controller's first projection in a forked child."""

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

sys.path.insert(0, str(Path(__file__).resolve().parent))

import eval_stone_agent_authorship as base


ROOT = Path(__file__).resolve().parents[2]
SANDBOX = ROOT.parent
INVOCATION_MARKER = "\nsession = agent_session()"
TARGET = "fork-first-target-6d901c"
PROBLEM = "uncommitted fork prefix"
REQUIRED_KEY = "requirement.fork_target"
PARENT_LATE_KEY = "parent.after_fork"
FIXTURE_RESPONSE = json.dumps(
    {"actions": [{"final": {"answer": TARGET}}]},
    separators=(",", ":"),
)

SUFFIX = r'''
def first_context_worker(input):
    problem = read_text("problem.txt").strip()
    session = agent_session()
    options = standard_agent_options(session.input)
    result = standard_agent_control(
        session,
        options,
        standard_shell_dispatch,
        standard_verify_finish,
        standard_record_progress,
    )
    return {
        "problem": problem,
        "legacy_policy_in_input": (
            "initial_action_memory_required_keys" in keys(input)
        ),
        "session_context_prompt_view": session.context_prompt_view,
        "agent_result": result,
    }


def first_context_parent(input):
    root = attempt_info()
    write_file("problem.txt", input.problem + "\n")
    context_write(
        "requirement.fork_target",
        "requirement",
        {"target": input.target},
        status="verified",
        evidence=["parent:fork-frontier"],
    )
    scope = attempt_scope(join_timeout_ms=60000)
    child = attempt_fork(
        program=current_program(),
        entrypoint="first_context_worker",
        input={
            "candidate": "opaque-candidate-a",
            "action_memory_projection_tokens": 128,
            "max_turns": 1,
            "completion_critique": False,
            "proactive_completion_checkpoint": False,
        },
        context_prompt_view={
            "required_keys": ["requirement.fork_target"],
        },
        start=True,
        scope=scope,
    )

    # This revision is newer than the fork frontier and must never appear in
    # the child's initial projection.
    context_write(
        "parent.after_fork",
        "fact",
        {"value": "parent-only"},
        status="verified",
        evidence=["parent:post-fork"],
    )
    outcome = attempt_join(child, timeout_ms=60000)
    if not outcome.joined or outcome.result.status != "succeeded":
        fail(
            "fork first-context worker failed",
            code="fork_first_context_child_failed",
            detail=outcome,
        )
    child_result = outcome.result.value
    attempt_discard(child, reason="fork first-context canary complete")
    cleanup = attempt_scope_close(scope)
    if not cleanup.clean:
        fail(
            "fork first-context scope did not close",
            code="fork_first_context_cleanup_failed",
            detail=cleanup,
        )
    return {
        "child": child.attempt,
        "fork_memory_ref": child.fork_origin.memory_ref,
        "fork_memory_revision": child.fork_origin.memory_revision,
        "parent_memory_ref": root.memory_ref,
        "parent_memory_revision": attempt_info().memory_revision,
        "parent_keys": map(lambda item: item.key, context_read(limit=8)),
        "child_result": child_result,
        "clean": cleanup.clean,
    }


control = attempt_info()
entrypoint = get(control.metadata, "program_entrypoint", "first_context_parent")
if entrypoint == "first_context_worker":
    result = first_context_worker(task_input())
else:
    result = first_context_parent(task_input())
attempt_report(
    status="succeeded",
    result=result,
    metadata={"control": "standard_fork_first_context_v1"},
)
emit(result)
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
    parser.add_argument(
        "--provider",
        choices=("fixture", "codex-chatgpt"),
        default="fixture",
    )
    parser.add_argument("--auth-json", type=Path, default=Path.home() / ".codex/auth.json")
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=ROOT / "target/runs/stone-standard-fork-first-context-v8",
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
    check: bool = True,
) -> str:
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
    return completed.stdout


def response_from_logs(text: str) -> dict[str, Any] | None:
    return base.response_payload(subprocess.CompletedProcess([], 0, text, ""))


def context_projection_events(payload: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        event
        for event in (payload.get("diagnostics") or {}).get("context", {}).get("events", [])
        if isinstance(event, dict) and event.get("op") == "project"
    ]


def gate_result(result: dict[str, Any]) -> list[str]:
    violations: list[str] = []
    root_payload = result.get("root_payload")
    child_payload = result.get("child_payload")
    if not isinstance(root_payload, dict) or root_payload.get("ok") is not True:
        return ["root controller did not return ok=true"]
    if not isinstance(child_payload, dict) or child_payload.get("ok") is not True:
        return ["child controller did not return ok=true"]
    value = root_payload.get("value") or {}
    child = value.get("child_result") or {}
    agent = child.get("agent_result") or {}
    control = agent.get("_control") or {}

    if child.get("problem") != PROBLEM:
        violations.append("child did not inherit the uncommitted workspace prefix")
    if agent.get("answer") != TARGET:
        violations.append("child did not return the projected opaque target")
    if control.get("name") != "stone.standard_action_v14":
        violations.append("child did not use standard controller V8")
    if control.get("model_calls") != 1 or control.get("actions") != 1:
        violations.append("child did not decide in exactly one first-context call")
    if control.get("initial_action_memory_required_keys") != [REQUIRED_KEY]:
        violations.append("child control lost its required first-context key")
    if control.get("initial_action_memory_policy_source") != "attempt_admission":
        violations.append("child did not source first-context policy from admission")
    if child.get("legacy_policy_in_input") is not False:
        violations.append("child task input still carries the legacy memory policy")
    if child.get("session_context_prompt_view") != {
        "required_keys": [REQUIRED_KEY]
    }:
        violations.append("agent_session lost the typed context prompt view")
    projected = control.get("initial_action_memory_projection_keys")
    if not isinstance(projected, list) or REQUIRED_KEY not in projected:
        violations.append("child first projection omitted the required fork key")
    if isinstance(projected, list) and PARENT_LATE_KEY in projected:
        violations.append("child first projection included a post-fork parent key")
    if value.get("fork_memory_revision") != 1:
        violations.append("child fork did not capture parent memory revision 1")
    if value.get("parent_memory_revision") != 2:
        violations.append("child memory changed the parent revision")
    if value.get("parent_keys") != [PARENT_LATE_KEY, REQUIRED_KEY]:
        violations.append("parent memory frontier is not isolated from the child")
    if value.get("clean") is not True:
        violations.append("fork scope did not close cleanly")

    projections = context_projection_events(child_payload)
    if len(projections) != 1:
        violations.append(
            f"child emitted {len(projections)} context projections, expected one"
        )
    else:
        event = projections[0]
        selected = event.get("selected") or []
        if len(selected) != len(projected or []):
            violations.append("context diagnostic and control selected different item counts")
    projection_trace = result.get("projection_trace") or []
    if len(projection_trace) != 1:
        violations.append(
            f"Gateway recorded {len(projection_trace)} projection traces, expected one"
        )
    elif projection_trace[0].get("required_keys") != [REQUIRED_KEY]:
        violations.append("Gateway projection trace lost the exact required key")
    model_trace = result.get("model_trace") or []
    if len(model_trace) != 1:
        violations.append(f"Gateway recorded {len(model_trace)} model calls, expected one")
    else:
        expected_provider = (result.get("manifest") or {}).get("provider")
        if model_trace[0].get("status") != "ok":
            violations.append("Gateway model call did not complete successfully")
        if model_trace[0].get("provider") != expected_provider:
            violations.append("Gateway model call used an unexpected provider")
    return violations


def rollback_if_active(
    binary: Path,
    data_root: Path,
    attempt: str,
    env: dict[str, str],
) -> None:
    info = gateway(
        binary,
        data_root,
        "attempt",
        "info",
        attempt,
        env=env,
        check=False,
    )
    if "\nstate\tactive\n" in "\n" + info:
        gateway(
            binary,
            data_root,
            "attempt",
            "finish",
            attempt,
            "--rollback",
            "--reason",
            "fork first-context canary cleanup",
            env=env,
        )


def run_experiment(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    composed = compose_source(args.standard_source.read_text(encoding="utf-8"))
    if TARGET in composed:
        raise RuntimeError("opaque target leaked into admitted Stone source")
    source_path = run_dir / "fork-first-context.stone"
    source_path.write_text(composed, encoding="utf-8")
    manifest = {
        "schema": "waymark.standard-agent-fork-first-context-manifest.v8",
        "standard_source": str(args.standard_source),
        "standard_source_sha256": digest(args.standard_source),
        "composed_source_sha256": digest(source_path),
        "target_sha256": hashlib.sha256(TARGET.encode()).hexdigest(),
        "provider": args.provider,
        "model": "fixture" if args.provider == "fixture" else args.model,
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
    (fixture / "README.md").write_text("fork first-context base\n", encoding="utf-8")
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-fork-context-", dir="/tmp"))
    socket_path = socket_root / "gateway.sock"
    gateway_stdout = (run_dir / "gateway.stdout").open("w", encoding="utf-8")
    gateway_stderr = (run_dir / "gateway.stderr").open("w", encoding="utf-8")
    shared_env = {
        "WAYMARK_STONE_BIN": str(args.waymark_bin),
        "WAYMARK_GATEWAY_SOCKET": str(socket_path),
        "WAYMARK_GATEWAY_IMAGE": args.image,
        "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
        "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
    }
    model_env = {
        "WAYMARK_MODEL_PROVIDER": "fixture",
        "WAYMARK_MODEL_FIXTURE_TEXT": FIXTURE_RESPONSE,
    }
    if args.provider == "codex-chatgpt":
        model_env = {
            "WAYMARK_MODEL_PROVIDER": "codex-chatgpt",
            "WAYMARK_MODEL_CODEX_AUTH_JSON": str(args.auth_json),
            "WAYMARK_MODEL": args.model,
            "WAYMARK_MODEL_ALLOWLIST": args.model,
            "WAYMARK_MODEL_REASONING_EFFORT": args.reasoning_effort,
        }
    server_env = {**os.environ, **shared_env, **model_env}
    client_env = {**os.environ, **shared_env}
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
    root_attempt = ""
    child_attempt = ""
    result: dict[str, Any] | None = None
    started = time.monotonic()
    try:
        base.wait_for_socket(socket_path, server)
        gateway(
            args.gateway_bin,
            data_root,
            "repo",
            "snapshot",
            "--name",
            "fork-first-context",
            "--path",
            str(fixture),
        )
        root_attempt = gateway(
            args.gateway_bin,
            data_root,
            "attempt",
            "spawn",
            "--task",
            "fork-first-context",
            "--task-spec-id",
            "fork-first-context",
            "--task-objective",
            (
                "On the first decision, read requirement.fork_target from "
                "Active bounded attempt memory and finish with its content.target "
                "exactly. Do not infer a target from task input."
            ),
            "--workspace",
            "fork-first-context",
            "--controller",
            "stone",
            "--workspace-mount",
            "/app",
            "--task-input-json",
            json.dumps({"target": TARGET, "problem": PROBLEM}, separators=(",", ":")),
            "--program-stone-file",
            str(source_path),
            "--program-entrypoint",
            "first_context_parent",
            env=client_env,
        ).strip()
        gateway(
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
        root_logs = gateway(
            args.gateway_bin,
            data_root,
            "attempt",
            "logs",
            root_attempt,
            "--max-bytes",
            "1048576",
            env=client_env,
        )
        root_payload = response_from_logs(root_logs)
        if isinstance(root_payload, dict):
            child_attempt = str((root_payload.get("value") or {}).get("child") or "")
        child_logs = (
            gateway(
                args.gateway_bin,
                data_root,
                "attempt",
                "logs",
                child_attempt,
                "--max-bytes",
                "1048576",
                env=client_env,
            )
            if child_attempt
            else ""
        )
        child_payload = response_from_logs(child_logs)
        trace_path = data_root / "traces" / "operations.jsonl"
        trace_events = (
            [
                json.loads(line)
                for line in trace_path.read_text(encoding="utf-8").splitlines()
            ]
            if trace_path.is_file()
            else []
        )
        projection_trace = [
            event
            for event in trace_events
            if event.get("op") == "attempt.memory.project"
            and event.get("attempt") == child_attempt
        ]
        model_trace = [
            event
            for event in trace_events
            if event.get("op") == "attempt.rpc.model.call"
            and event.get("attempt") == child_attempt
        ]
        result = {
            "schema": "waymark.standard-agent-fork-first-context-result.v8",
            "ok": False,
            "duration_seconds": time.monotonic() - started,
            "violations": [],
            "root_attempt": root_attempt,
            "child_attempt": child_attempt,
            "root_payload": root_payload,
            "child_payload": child_payload,
            "projection_trace": projection_trace,
            "model_trace": model_trace,
            "manifest": manifest,
        }
        result["violations"] = gate_result(result)
        result["ok"] = not result["violations"]
        return result
    finally:
        if child_attempt:
            rollback_if_active(args.gateway_bin, data_root, child_attempt, client_env)
        if root_attempt:
            rollback_if_active(args.gateway_bin, data_root, root_attempt, client_env)
        if result is not None:
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
            trace_path = data_root / "traces" / "operations.jsonl"
            trace = trace_path.read_text(encoding="utf-8") if trace_path.is_file() else ""
            result["trace_counts"] = {
                name: trace.count(f'"op":"{name}"')
                for name in (
                    "attempt.fork",
                    "attempt.memory.project",
                    "attempt.rpc.model.call",
                    "attempt.finish",
                )
            }
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
