#!/usr/bin/env python3
"""Run a standard-controller portfolio that reuses one forked state frontier."""

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
PROBLEM = "drawer"
EXPECTED = "reward"
STRATEGIES = ("uppercase", "reverse")
WINNER_STRATEGY = "reverse"
LOSER_STRATEGY = "uppercase"
FRONTIER_MARKER = "shared-uncommitted-portfolio-prefix-84cf"
REQUIRED_KEY = "requirement.portfolio_target"
PARENT_LATE_KEY = "parent.after_portfolio_fork"
FIXTURE_RESPONSE = json.dumps(
    {"actions": [{"final": {"answer": "candidate-prepared"}}]},
    separators=(",", ":"),
)


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
    parser.add_argument(
        "--portfolio-source",
        type=Path,
        default=ROOT
        / "examples/references/standard_attempt_fork_portfolio.stone",
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
        default=ROOT / "target/runs/stone-standard-fork-portfolio-v1",
    )
    parser.add_argument("--overwrite", action="store_true")
    return parser.parse_args()


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compose_source(standard: str, portfolio: str) -> str:
    marker = standard.find(INVOCATION_MARKER)
    if marker < 0:
        raise ValueError(f"standard source is missing {INVOCATION_MARKER!r}")
    return standard[:marker].rstrip() + "\n\n" + portfolio.strip() + "\n"


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


def rollback_if_active(
    binary: Path,
    data_root: Path,
    attempt: str,
    env: dict[str, str],
) -> None:
    if not attempt:
        return
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
            "standard fork portfolio cleanup",
            env=env,
            check=False,
        )


def _by_strategy(entries: Any) -> dict[str, dict[str, Any]]:
    if not isinstance(entries, list):
        return {}
    return {
        str(entry.get("strategy") or ""): entry
        for entry in entries
        if isinstance(entry, dict)
    }


def gate_result(result: dict[str, Any]) -> list[str]:
    violations: list[str] = []
    root_payload = result.get("root_payload")
    if not isinstance(root_payload, dict) or root_payload.get("ok") is not True:
        return ["root portfolio controller did not return ok=true"]
    value = root_payload.get("value") or {}

    if value.get("answer") != EXPECTED:
        violations.append("accepted workspace does not contain the expected answer")
    if value.get("accepted") != value.get("winner"):
        violations.append("accepted child is not the selected winner")
    if value.get("winner_strategy") != WINNER_STRATEGY:
        violations.append("portfolio selected the wrong strategy")
    if value.get("loser_strategy") != LOSER_STRATEGY:
        violations.append("portfolio did not retain the expected losing strategy")
    if value.get("clean") is not True or value.get("baseline_clean") is not True:
        violations.append("portfolio or baseline scope did not close cleanly")
    if value.get("parent_keys") != [PARENT_LATE_KEY, REQUIRED_KEY]:
        violations.append("child memory leaked into the parent ledger")
    if value.get("parent_memory_revision") != 2:
        violations.append("parent memory frontier has an unexpected revision")

    baseline = value.get("baseline_result") or {}
    for field in ("problem_seen", "verifier_seen", "marker_seen", "memory_seen"):
        if baseline.get(field) is not False:
            violations.append(f"spawn baseline unexpectedly inherited {field}")
    if baseline.get("parent_attempt") != result.get("root_attempt"):
        violations.append("spawn baseline lost lifecycle parentage")
    if baseline.get("canonical_readme") != "fork portfolio canonical base":
        violations.append("spawn baseline did not start from the canonical generation")

    children = _by_strategy(value.get("child_results"))
    inspections = _by_strategy(value.get("pre_inspections"))
    expected_outputs = {
        WINNER_STRATEGY: EXPECTED,
        LOSER_STRATEGY: PROBLEM.upper(),
    }
    expected_pass = {
        WINNER_STRATEGY: True,
        LOSER_STRATEGY: False,
    }
    if set(children) != set(STRATEGIES):
        violations.append("portfolio did not return both strategy results")
    for strategy in STRATEGIES:
        entry = children.get(strategy) or {}
        child = entry.get("result") or {}
        agent = child.get("agent_result") or {}
        control = agent.get("_control") or {}
        if child.get("prepared_output") != expected_outputs[strategy]:
            violations.append(f"{strategy} prepared an unexpected candidate")
        if agent.get("candidate_output") != expected_outputs[strategy]:
            violations.append(f"{strategy} verifier observed an unexpected candidate")
        if agent.get("passed") is not expected_pass[strategy]:
            violations.append(f"{strategy} has the wrong verification decision")
        if child.get("shared_prefix") != FRONTIER_MARKER:
            violations.append(f"{strategy} missed the uncommitted shared prefix")
        if child.get("legacy_policy_in_input") is not False:
            violations.append(f"{strategy} still carries boot policy in task input")
        if child.get("session_context_prompt_view") != {
            "required_keys": [REQUIRED_KEY]
        }:
            violations.append(f"{strategy} lost its typed context prompt view")
        if child.get("post_fork_parent_key_seen") is not False:
            violations.append(f"{strategy} observed post-fork parent memory")
        if entry.get("fork_memory_revision") != 1:
            violations.append(f"{strategy} forked from the wrong memory revision")
        if control.get("name") != "stone.standard_action_v12":
            violations.append(f"{strategy} did not use standard controller V8")
        if control.get("initial_action_memory_policy_source") != "attempt_admission":
            violations.append(f"{strategy} did not use admission-owned boot policy")
        if control.get("initial_action_memory_required_keys") != [REQUIRED_KEY]:
            violations.append(f"{strategy} lost its required first-context key")
        if REQUIRED_KEY not in (control.get("initial_action_memory_projection_keys") or []):
            violations.append(f"{strategy} first projection omitted the required key")
        model_calls = control.get("model_calls")
        if not isinstance(model_calls, int) or not 1 <= model_calls <= 4:
            violations.append(f"{strategy} used an invalid model-call count")

        inspection = inspections.get(strategy) or {}
        trace_ops = inspection.get("trace_ops") or []
        if inspection.get("resource_state") != "retained":
            violations.append(f"{strategy} was not retained for parent inspection")
        if inspection.get("resources_reclaimed") is not False:
            violations.append(f"{strategy} was reclaimed before parent inspection")
        if inspection.get("summary_passed") is not expected_pass[strategy]:
            violations.append(f"{strategy} inspection summary disagrees with its result")
        for required_op in ("attempt.memory.project", "attempt.rpc.model.call"):
            if required_op not in trace_ops:
                violations.append(f"{strategy} inspection trace omitted {required_op}")

    post = value.get("post_cleanup") or {}
    for branch in ("winner", "loser"):
        if post.get(f"{branch}_resource_state") != "reclaimed":
            violations.append(f"{branch} resources were not reclaimed")
        if post.get(f"{branch}_resources_reclaimed") is not True:
            violations.append(f"{branch} cleanup flag is false")

    child_attempts = {
        str(entry.get("attempt") or "")
        for entry in children.values()
        if entry.get("attempt")
    }
    fork_trace = result.get("fork_trace") or []
    if len(fork_trace) != 2:
        violations.append("Gateway did not record exactly two portfolio forks")
    for event in fork_trace:
        if event.get("context_prompt_required_keys") != [REQUIRED_KEY]:
            violations.append("fork trace lost the typed context prompt view")
    projection_trace = result.get("projection_trace") or []
    for child_attempt in child_attempts:
        events = [
            event
            for event in projection_trace
            if event.get("attempt") == child_attempt
        ]
        if not events or events[0].get("required_keys") != [REQUIRED_KEY]:
            violations.append(f"{child_attempt} has no required initial projection trace")
        if any(event.get("required_keys") for event in events[1:]):
            violations.append(f"{child_attempt} reused boot keys after its first projection")
    model_trace = result.get("model_trace") or []
    expected_provider = (result.get("manifest") or {}).get("provider")
    for child_attempt in child_attempts:
        events = [
            event for event in model_trace if event.get("attempt") == child_attempt
        ]
        if not events:
            violations.append(f"{child_attempt} has no model trace")
        for event in events:
            if event.get("status") != "ok":
                violations.append(f"{child_attempt} has a failed model trace")
            if event.get("provider") != expected_provider:
                violations.append(f"{child_attempt} used an unexpected model provider")

    if (result.get("trace_counts") or {}).get("attempt.accept") != 1:
        violations.append("trace does not contain exactly one accept")
    return violations


def run_experiment(args: argparse.Namespace) -> dict[str, Any]:
    run_dir = args.run_dir.resolve()
    if run_dir.exists():
        if not args.overwrite:
            raise SystemExit(f"refusing to overwrite existing run directory: {run_dir}")
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)

    composed = compose_source(
        args.standard_source.read_text(encoding="utf-8"),
        args.portfolio_source.read_text(encoding="utf-8"),
    )
    if EXPECTED in composed:
        raise RuntimeError("opaque expected answer leaked into admitted Stone source")
    source_path = run_dir / "standard-fork-portfolio.stone"
    source_path.write_text(composed, encoding="utf-8")
    manifest = {
        "schema": "waymark.standard-agent-fork-portfolio-manifest.v1",
        "standard_source": str(args.standard_source),
        "standard_source_sha256": digest(args.standard_source),
        "portfolio_source": str(args.portfolio_source),
        "portfolio_source_sha256": digest(args.portfolio_source),
        "composed_source_sha256": digest(source_path),
        "expected_sha256": hashlib.sha256(EXPECTED.encode()).hexdigest(),
        "provider": args.provider,
        "model": "fixture" if args.provider == "fixture" else args.model,
        "waymark_sha256": digest(args.waymark_bin),
        "gateway_sha256": digest(args.gateway_bin),
    }
    (run_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )

    fixture = run_dir / "fixture"
    fixture.mkdir()
    (fixture / "README.md").write_text(
        "fork portfolio canonical base\n",
        encoding="utf-8",
    )
    data_root = run_dir / "gateway-data"
    socket_root = Path(tempfile.mkdtemp(prefix="waymark-fork-portfolio-", dir="/tmp"))
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
    child_attempts: list[str] = []
    baseline_attempt = ""
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
            "fork-portfolio",
            "--path",
            str(fixture),
        )
        child_max_turns = 1 if args.provider == "fixture" else 4
        task_input = {
            "problem": PROBLEM,
            "expected": EXPECTED,
            "frontier_marker": FRONTIER_MARKER,
            "strategies": list(STRATEGIES),
            "max_turns": child_max_turns,
        }
        root_attempt = gateway(
            args.gateway_bin,
            data_root,
            "attempt",
            "spawn",
            "--task",
            "fork-portfolio",
            "--task-spec-id",
            "fork-portfolio",
            "--task-objective",
            (
                "Evaluate two isolated strategies from one in-progress state. "
                "Each child starts with answer.txt already prepared; inspect it "
                "if useful, do not alter files, and finish so parent policy can "
                "verify and select the candidate."
            ),
            "--workspace",
            "fork-portfolio",
            "--controller",
            "stone",
            "--workspace-mount",
            "/app",
            "--task-input-json",
            json.dumps(task_input, separators=(",", ":")),
            "--program-stone-file",
            str(source_path),
            "--program-entrypoint",
            "portfolio_parent",
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
            "2097152",
            env=client_env,
        )
        root_payload = response_from_logs(root_logs)
        value = (
            root_payload.get("value") or {}
            if isinstance(root_payload, dict)
            else {}
        )
        child_attempts = [
            str(entry.get("attempt") or "")
            for entry in value.get("child_results") or []
            if isinstance(entry, dict) and entry.get("attempt")
        ]
        baseline_attempt = str(value.get("baseline_attempt") or "")
        child_payloads = {}
        for attempt in child_attempts:
            logs = gateway(
                args.gateway_bin,
                data_root,
                "attempt",
                "logs",
                attempt,
                "--max-bytes",
                "1048576",
                env=client_env,
            )
            child_payloads[attempt] = response_from_logs(logs)

        trace_path = data_root / "traces" / "operations.jsonl"
        trace_events = (
            [
                json.loads(line)
                for line in trace_path.read_text(encoding="utf-8").splitlines()
            ]
            if trace_path.is_file()
            else []
        )
        child_set = set(child_attempts)
        fork_trace = [
            event
            for event in trace_events
            if event.get("op") == "attempt.fork"
            and event.get("attempt") in child_set
        ]
        projection_trace = [
            event
            for event in trace_events
            if event.get("op") == "attempt.memory.project"
            and event.get("attempt") in child_set
        ]
        model_trace = [
            event
            for event in trace_events
            if event.get("op") == "attempt.rpc.model.call"
            and event.get("attempt") in child_set
        ]
        trace_counts = {
            op: sum(event.get("op") == op for event in trace_events)
            for op in (
                "attempt.spawn",
                "attempt.fork",
                "attempt.accept",
                "attempt.finish",
                "attempt.rpc.attempt_inspect",
                "attempt.memory.project",
                "attempt.rpc.model.call",
            )
        }
        result = {
            "schema": "waymark.standard-agent-fork-portfolio-result.v1",
            "ok": False,
            "duration_seconds": time.monotonic() - started,
            "violations": [],
            "root_attempt": root_attempt,
            "baseline_attempt": baseline_attempt,
            "child_attempts": child_attempts,
            "root_payload": root_payload,
            "child_payloads": child_payloads,
            "fork_trace": fork_trace,
            "projection_trace": projection_trace,
            "model_trace": model_trace,
            "trace_counts": trace_counts,
            "manifest": manifest,
        }
        result["violations"] = gate_result(result)
        result["ok"] = not result["violations"]
        return result
    finally:
        for attempt in child_attempts:
            rollback_if_active(args.gateway_bin, data_root, attempt, client_env)
        rollback_if_active(args.gateway_bin, data_root, baseline_attempt, client_env)
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
    args.portfolio_source = args.portfolio_source.resolve()
    args.run_dir = args.run_dir.resolve()
    for label, path in (
        ("Waymark", args.waymark_bin),
        ("Gateway", args.gateway_bin),
        ("standard source", args.standard_source),
        ("portfolio source", args.portfolio_source),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    if args.provider == "codex-chatgpt" and not args.auth_json.is_file():
        raise SystemExit(f"Codex auth JSON not found: {args.auth_json}")

    result = run_experiment(args)
    print(
        json.dumps(
            {
                "ok": result["ok"],
                "violations": result["violations"],
                "duration_seconds": result["duration_seconds"],
                "summary": str(args.run_dir / "summary.json"),
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
