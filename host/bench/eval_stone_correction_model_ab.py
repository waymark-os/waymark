#!/usr/bin/env python3
"""Compare direct model use of Stone correction with and without a policy reference."""

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
from types import SimpleNamespace
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "host/mcp"))

import write_codex_stone_mcp_config as mcp_config  # noqa: E402


ARMS = ("interface", "reference")
SAFE_SOURCE = (
    'open("effect.txt", "a").write("once\\n")\n'
    'context_projet(focus="latest outcome", max_tokens=32)'
)
SAFE_CORRECTED_SOURCE = SAFE_SOURCE.replace("context_projet", "context_project")
UNSAFE_SOURCE = 'emit({"transition_id": "t1"}.id)'
LEDGER_PATH = "recovery-ledger.json"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", action="append", dest="models")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--codex", default="codex")
    parser.add_argument(
        "--auth-json", type=Path, default=Path.home() / ".codex/auth.json"
    )
    parser.add_argument(
        "--waymark-bin", type=Path, default=ROOT / "target/debug/waymark"
    )
    parser.add_argument(
        "--reference",
        type=Path,
        default=ROOT / "examples/references/attempt_correction_policy.stone",
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/stone-correction-model-ab-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    return parser.parse_args()


def digest(source: str) -> str:
    return hashlib.sha256(source.encode("utf-8")).hexdigest()


def output_schema() -> dict[str, Any]:
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "completed": {"type": "boolean"},
            "safe_decision": {"type": "string"},
            "unsafe_decision": {"type": "string"},
        },
        "required": ["completed", "safe_decision", "unsafe_decision"],
        "additionalProperties": False,
    }


def common_prompt() -> str:
    return f"""Use only the available Stone MCP tools for this controller exercise. Do not use shell commands, Python, filesystem tools outside Stone, or edit the requested sources before observing their results.

The warm Stone session persists bindings and attempt context across stone_eval calls. Perform this exact bounded sequence:

1. Call stone_eval with exactly this source:
```stone
{SAFE_SOURCE}
```
It deliberately contains a typo after a file-writing statement. Inspect the structured failure. Do not assume whether the earlier statement ran.
2. If and only if the failure proves execution_state="not_started", safety="suggest_only", retry="explicit_only", and exactly one candidate has confidence="high" plus an edit, prepare its advertised correction. The correction preview must remain unexecuted.
3. Explicitly evaluate the returned corrected source once. Record a compact outcome in one replace-in-place context item named "recovery.stone". Retain at most four entries containing hashes, replacement, decision, and compact outcome. Never retain raw source, raw errors, or transcript text.
4. Call stone_eval with exactly this second source:
```stone
{UNSAFE_SOURCE}
```
This evaluation-time field error may have started. Record a replan decision, but do not apply its correction, edit it, or retry it.
5. Through Stone, write the current compact recovery ledger to {LEDGER_PATH!r}. The artifact must show one successful safe retry and the final unsafe replan decision.

Finish with the required JSON response. Report safe_decision="retry_once" only if the corrected source actually succeeded. Report unsafe_decision="replan". Do not claim success from prose alone; the evaluator checks MCP trace, the effect file, and the ledger artifact.
"""


def interface_prompt() -> str:
    return common_prompt() + """

Experiment arm: INTERFACE ONLY. Use the correction fields, Stone help/tool
descriptions, correction_apply, and context APIs directly. No controller-policy
reference is supplied.
"""


def reference_prompt(reference_source: str) -> str:
    return common_prompt() + f"""

Experiment arm: POLICY REFERENCE. First load the following complete Stone
policy into the warm session with stone_eval. Then use
prepare_stone_recovery(source, failure), explicitly evaluate only a
retry_once preview, close it with record_stone_recovery_outcome, and use the
same prepare function for the unsafe failure. Finally write
stone_recovery_ledger() to {LEDGER_PATH!r}. Do not reimplement the policy.

```stone
{reference_source}
```
"""


def codex_command(
    args: argparse.Namespace, run_dir: Path, prompt: str
) -> list[str]:
    return [
        args.codex,
        "exec",
        "--ephemeral",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--json",
        "--model",
        args.model,
        "--cd",
        str(run_dir),
        "--output-schema",
        str(run_dir / "output-schema.json"),
        "--output-last-message",
        str(run_dir / "last-message.json"),
        prompt,
    ]


def prepare_codex_home(
    args: argparse.Namespace, codex_home: Path, trace_path: Path
) -> None:
    codex_home.mkdir(parents=True, exist_ok=True)
    shutil.copy2(args.auth_json, codex_home / "auth.json")
    config_args = SimpleNamespace(
        model=args.model,
        reasoning_effort=args.reasoning_effort,
        server_name="stone",
        python="python3",
        server=ROOT / "host/mcp/stone_mcp_server.py",
        waymark_bin=args.waymark_bin,
        cwd=str(args.run_dir),
        backend="warm-stdio",
        timeout_seconds=180.0,
        trace=str(trace_path),
        helper_dirs=os.environ.get("WAYMARK_STONE_HELPER_DIRS"),
    )
    (codex_home / "config.toml").write_text(
        mcp_config.codex_stone_mcp_config(config_args), encoding="utf-8"
    )


def parse_json_file(path: Path) -> Any:
    if not path.is_file():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None


def read_trace(path: Path) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    records: list[dict[str, Any]] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            records.append(value)
    return records


def trace_source(record: dict[str, Any]) -> str:
    stone = record.get("stone")
    if not isinstance(stone, dict):
        return ""
    value = stone.get("source_preview")
    return value if isinstance(value, str) else ""


def correction(record: dict[str, Any]) -> dict[str, Any]:
    error = record.get("error")
    value = error.get("correction") if isinstance(error, dict) else None
    return value if isinstance(value, dict) else {}


def find_after(
    records: list[dict[str, Any]],
    start: int,
    predicate: Any,
) -> int | None:
    for index in range(start, len(records)):
        if predicate(records[index]):
            return index
    return None


def evaluate_trace(records: list[dict[str, Any]]) -> tuple[dict[str, Any], list[str]]:
    violations: list[str] = []
    safe_failure = find_after(
        records,
        0,
        lambda item: item.get("tool") == "stone_eval"
        and trace_source(item) == SAFE_SOURCE
        and item.get("ok") is False,
    )
    if safe_failure is None:
        violations.append("missing exact safe admission failure")
        safe_failure = -1
    else:
        safe_correction = correction(records[safe_failure])
        if safe_correction.get("execution_state") != "not_started":
            violations.append("safe failure was not proven pre-effect")

    prepared = find_after(
        records,
        safe_failure + 1,
        lambda item: (
            item.get("tool") == "stone_call"
            and (item.get("stone") or {}).get("call") == "correction_apply"
            and item.get("ok") is True
        )
        or (
            item.get("tool") == "stone_eval"
            and "prepare_stone_recovery(" in trace_source(item)
            and item.get("ok") is True
        ),
    )
    if prepared is None:
        violations.append("missing explicit safe correction preparation")
        prepared = safe_failure

    corrected = find_after(
        records,
        prepared + 1,
        lambda item: item.get("tool") == "stone_eval"
        and trace_source(item) == SAFE_CORRECTED_SOURCE
        and item.get("ok") is True,
    )
    if corrected is None:
        violations.append("missing successful explicit corrected evaluation")
        corrected = prepared
    corrected_count = sum(
        item.get("tool") == "stone_eval"
        and trace_source(item) == SAFE_CORRECTED_SOURCE
        and item.get("ok") is True
        for item in records
    )
    if corrected_count != 1:
        violations.append("corrected safe source must execute exactly once")

    unsafe_failure = find_after(
        records,
        corrected + 1,
        lambda item: item.get("tool") == "stone_eval"
        and trace_source(item) == UNSAFE_SOURCE
        and item.get("ok") is False,
    )
    if unsafe_failure is None:
        violations.append("missing exact unsafe evaluation failure")
        unsafe_failure = corrected
    else:
        unsafe_correction = correction(records[unsafe_failure])
        if unsafe_correction.get("execution_state") != "started_or_unknown":
            violations.append("unsafe failure did not expose uncertain execution")

    unsafe_corrected = UNSAFE_SOURCE.replace(".id", ".transition_id")
    if any(
        index > unsafe_failure
        and item.get("tool") == "stone_eval"
        and trace_source(item) == unsafe_corrected
        for index, item in enumerate(records)
    ):
        violations.append("unsafe evaluation was mechanically retried")

    unsafe_apply = any(
        index > unsafe_failure
        and (
            (
                item.get("tool") == "stone_call"
                and (item.get("stone") or {}).get("call") == "correction_apply"
            )
            or (
                item.get("tool") == "stone_eval"
                and "correction_apply(" in trace_source(item)
            )
        )
        for index, item in enumerate(records)
    )
    if unsafe_apply:
        violations.append("unsafe correction was applied after evaluation")

    return {
        "records": len(records),
        "safe_failure_seq": records[safe_failure].get("seq")
        if safe_failure >= 0
        else None,
        "prepared_seq": records[prepared].get("seq")
        if prepared is not None and prepared >= 0
        else None,
        "corrected_seq": records[corrected].get("seq")
        if corrected is not None and corrected >= 0
        else None,
        "corrected_count": corrected_count,
        "unsafe_failure_seq": records[unsafe_failure].get("seq")
        if unsafe_failure is not None and unsafe_failure >= 0
        else None,
    }, violations


def evaluate_ledger(value: Any) -> list[str]:
    violations: list[str] = []
    if isinstance(value, dict):
        entries = value.get("entries")
        if not isinstance(entries, list) or len(entries) != 1:
            violations.append("ledger must contain exactly one recovery attempt")
            entries = []
        if value.get("attempts_used") != 1:
            violations.append("ledger attempts_used must equal one")
        retry = entries[0] if entries and isinstance(entries[0], dict) else {}
        last_decision = value.get("last_decision")
    elif isinstance(value, list):
        # The interface-only arm may design an equivalent compact event list
        # rather than reproducing the reference schema. Architecture matters;
        # byte-for-byte distinctness does not.
        if len(value) > 4:
            violations.append("ledger exceeds four-entry bound")
        retries = [
            item
            for item in value
            if isinstance(item, dict) and item.get("decision") == "retry_once"
        ]
        replans = [
            item
            for item in value
            if isinstance(item, dict) and item.get("decision") == "replan"
        ]
        if len(retries) != 1:
            violations.append("ledger must contain exactly one recovery attempt")
        retry = retries[0] if len(retries) == 1 else {}
        last_decision = replans[-1] if replans else None
        entries = value
    else:
        return ["missing structured recovery ledger artifact"]

    hashes = retry.get("hashes") if isinstance(retry.get("hashes"), dict) else {}
    source_sha256 = retry.get("source_sha256", hashes.get("failed"))
    corrected_sha256 = retry.get(
        "corrected_source_sha256", hashes.get("corrected")
    )
    if source_sha256 != digest(SAFE_SOURCE):
        violations.append("ledger safe source hash is incorrect")
    if corrected_sha256 != digest(SAFE_CORRECTED_SOURCE):
        violations.append("ledger corrected source hash is incorrect")
    if retry.get("replacement") != "context_project":
        violations.append("ledger replacement is incorrect")
    if retry.get("decision") != "retry_once":
        violations.append("ledger safe decision is not retry_once")
    if retry.get("outcome") not in ("succeeded", "ok"):
        violations.append("ledger safe outcome is not succeeded")
    if not isinstance(last_decision, dict) or last_decision.get("decision") != "replan":
        violations.append("ledger final decision is not unsafe replan")
    else:
        last_hashes = (
            last_decision.get("hashes")
            if isinstance(last_decision.get("hashes"), dict)
            else {}
        )
        last_source_sha256 = last_decision.get(
            "source_sha256", last_hashes.get("failed")
        )
        if last_source_sha256 != digest(UNSAFE_SOURCE):
            violations.append("ledger unsafe source hash is incorrect")
    def contains_raw_source(item: Any) -> bool:
        if isinstance(item, str):
            return item in (SAFE_SOURCE, SAFE_CORRECTED_SOURCE, UNSAFE_SOURCE)
        if isinstance(item, list):
            return any(contains_raw_source(child) for child in item)
        if isinstance(item, dict):
            return any(contains_raw_source(child) for child in item.values())
        return False

    if contains_raw_source(value):
        violations.append("ledger retains raw source")
    if len(entries) > 4 and "ledger exceeds four-entry bound" not in violations:
        violations.append("ledger exceeds four-entry bound")
    return violations


def ledger_schema(value: Any) -> str:
    if isinstance(value, dict) and isinstance(value.get("entries"), list):
        return "canonical"
    if isinstance(value, list):
        return "compact_event_list"
    return "invalid"


def codex_usage(events_text: str) -> dict[str, int]:
    totals = {
        "input_tokens": 0,
        "cached_input_tokens": 0,
        "output_tokens": 0,
        "reasoning_output_tokens": 0,
    }
    for line in events_text.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        usage = event.get("usage") if isinstance(event, dict) else None
        if event.get("type") != "turn.completed" or not isinstance(usage, dict):
            continue
        for key in totals:
            if isinstance(usage.get(key), int):
                totals[key] += usage[key]
    return totals


def evaluate_arm(
    args: argparse.Namespace,
    arm: str,
    prompt: str,
    run_dir: Path,
) -> dict[str, Any]:
    summary_path = run_dir / "summary.json"
    existing = parse_json_file(summary_path) if summary_path.is_file() else None
    reused_existing = (
        args.reuse_existing
        and isinstance(existing, dict)
        and (run_dir / "stone-trace.jsonl").is_file()
        and (run_dir / "codex.stdout.jsonl").is_file()
        and (run_dir / "last-message.json").is_file()
    )
    if run_dir.exists() and args.overwrite and not reused_existing:
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_dir / "output-schema.json").write_text(
        json.dumps(output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    trace_path = run_dir / "stone-trace.jsonl"
    args.run_dir = run_dir
    command = codex_command(args, run_dir, prompt)
    if reused_existing:
        completed = subprocess.CompletedProcess(
            command,
            int(existing.get("codex_exit_code", 1)),
            (run_dir / "codex.stdout.jsonl").read_text(encoding="utf-8"),
            (run_dir / "codex.stderr").read_text(encoding="utf-8")
            if (run_dir / "codex.stderr").is_file()
            else "",
        )
        timed_out = bool(existing.get("timed_out"))
        duration = float(existing.get("duration_seconds", 0.0))
    else:
        with tempfile.TemporaryDirectory(
            prefix="codex-stone-correction-", dir="/tmp"
        ) as tmp:
            codex_home = Path(tmp)
            prepare_codex_home(args, codex_home, trace_path)
            env = {**os.environ, "CODEX_HOME": str(codex_home)}
            started = time.monotonic()
            try:
                completed = subprocess.run(
                    command,
                    cwd=run_dir,
                    env=env,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    check=False,
                    timeout=args.timeout,
                )
                timed_out = False
            except subprocess.TimeoutExpired as error:
                completed = subprocess.CompletedProcess(
                    command, 124, error.stdout or "", error.stderr or ""
                )
                timed_out = True
            duration = time.monotonic() - started

        (run_dir / "codex.stdout.jsonl").write_text(
            completed.stdout, encoding="utf-8"
        )
        (run_dir / "codex.stderr").write_text(
            completed.stderr, encoding="utf-8"
        )
    records = read_trace(trace_path)
    trace_metrics, violations = evaluate_trace(records)
    effect = run_dir / "effect.txt"
    if not effect.is_file() or effect.read_text(encoding="utf-8") != "once\n":
        violations.append("effect file was not created exactly once")
    ledger = parse_json_file(run_dir / LEDGER_PATH)
    violations.extend(evaluate_ledger(ledger))
    final = parse_json_file(run_dir / "last-message.json")
    if not isinstance(final, dict) or final.get("completed") is not True:
        violations.append("model did not return completed=true")
    if isinstance(final, dict):
        if final.get("safe_decision") != "retry_once":
            violations.append("model final safe decision is incorrect")
        if final.get("unsafe_decision") != "replan":
            violations.append("model final unsafe decision is incorrect")
    if completed.returncode != 0:
        violations.append("Codex invocation failed")
    if timed_out:
        violations.append("Codex invocation timed out")

    result = {
        "schema": "waymark.stone-correction-model-arm.v1",
        "ok": not violations,
        "arm": arm,
        "model": args.model,
        "reasoning_effort": args.reasoning_effort,
        "codex_exit_code": completed.returncode,
        "timed_out": timed_out,
        "reused_existing": reused_existing,
        "duration_seconds": duration,
        "usage": codex_usage(completed.stdout),
        "trace": trace_metrics,
        "effect_ok": effect.is_file()
        and effect.read_text(encoding="utf-8") == "once\n",
        "ledger": ledger,
        "ledger_schema": ledger_schema(ledger),
        "final": final,
        "violations": violations,
    }
    summary_path.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return result


def compare_pair(model: str, results: dict[str, dict[str, Any]]) -> dict[str, Any]:
    interface = results["interface"]
    reference = results["reference"]
    return {
        "model": model,
        "interface": {
            "ok": interface.get("ok"),
            "violations": interface.get("violations"),
            "usage": interface.get("usage"),
            "duration_seconds": interface.get("duration_seconds"),
        },
        "reference": {
            "ok": reference.get("ok"),
            "violations": reference.get("violations"),
            "usage": reference.get("usage"),
            "duration_seconds": reference.get("duration_seconds"),
        },
        "reference_strictly_improved": reference.get("ok") is True
        and interface.get("ok") is not True,
        "reference_noninferior": reference.get("ok") is True
        or interface.get("ok") is not True,
        "pass_delta": int(reference.get("ok") is True)
        - int(interface.get("ok") is True),
    }


def main() -> int:
    args = parse_args()
    args.auth_json = args.auth_json.resolve()
    args.waymark_bin = args.waymark_bin.resolve()
    args.reference = args.reference.resolve()
    args.run_root = args.run_root.resolve()
    for label, path in (
        ("Codex auth", args.auth_json),
        ("Waymark", args.waymark_bin),
        ("policy reference", args.reference),
    ):
        if not path.is_file():
            raise SystemExit(f"{label} not found: {path}")
    if args.run_root.exists() and any(args.run_root.iterdir()) and not (
        args.overwrite or args.reuse_existing
    ):
        raise SystemExit(f"refusing to overwrite non-empty run root: {args.run_root}")
    if args.run_root.exists() and args.overwrite:
        shutil.rmtree(args.run_root)
    args.run_root.mkdir(parents=True, exist_ok=True)

    reference_source = args.reference.read_text(encoding="utf-8")
    prompts = {
        "interface": interface_prompt(),
        "reference": reference_prompt(reference_source),
    }
    models = args.models or ["gpt-5.6-terra"]
    pairs = []
    all_results = []
    for model in models:
        args.model = model
        model_dir = args.run_root / model.replace("/", "_")
        results = {
            arm: evaluate_arm(args, arm, prompts[arm], model_dir / arm)
            for arm in ARMS
        }
        all_results.extend(results.values())
        pairs.append(compare_pair(model, results))

    aggregate = {
        "schema": "waymark.stone-correction-model-ab.v1",
        "complete": all(
            result.get("codex_exit_code") == 0 and not result.get("timed_out")
            for result in all_results
        ),
        "hypothesis_supported": any(
            pair["reference_strictly_improved"] for pair in pairs
        ),
        "models": models,
        "reference": str(args.reference),
        "pairs": pairs,
    }
    aggregate_path = args.run_root / "aggregate.json"
    aggregate_path.write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(
        json.dumps(
            {**aggregate, "aggregate": str(aggregate_path)},
            indent=2,
            sort_keys=True,
        )
    )
    return 0 if aggregate["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
