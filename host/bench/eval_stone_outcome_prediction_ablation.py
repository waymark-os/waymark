#!/usr/bin/env python3
"""Outcome-prediction ablation (design doc 11.1): effect tiers, with and without `expect`.

Both arms run under D-1's effect tiers, emulated at harness level:

  tier 1  every effectful action's observed file changes are reported back
  tier 2  protected paths (for example /app/tests) are mounted read-only
  tier 3  no network

Arm A: tiers only. Arm B: tiers, plus a required `expect` record in each
stone_eval / escape_linux action. The harness checks the expectation against
the observation. A mismatch adds the failed checks to the observation (so it
stays in history); a met expectation adds nothing.

The task verifier is shown only when the model finishes (--oracle finish,
the default). The base Terminal-Bench harness shows it after every step,
which would make outcome prediction redundant.

Reuses the sandbox, model clients, tasks, and parsing of
pilot_declared_writes.py. Run --dry-run first; it needs no model access.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import time
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pilot_declared_writes as pw
import run_stone_tbench4 as tb

ROOT = tb.ROOT
ARMS = ("A", "B")

# Tier 2: paths the grant protects, per task. Tasks not listed protect /app/tests if it exists.
PROTECTED = {
    "synthetic-py-fix": ["/app/tests"],
    "synthetic-c-build": ["/app/input.txt"],
}

READONLY_ERROR = re.compile(r"Read-only file system|EROFS", re.I)
NETWORK_ERROR = re.compile(
    r"Network is unreachable|Temporary failure in name resolution|Could not resolve host|Name or service not known|ENETUNREACH",
    re.I,
)

# Closed outcome vocabulary for harness actions (design doc 5.8, outcome vocabulary).
EXPECT_KEYS = {
    "ok": bool,               # the action succeeds (Stone ok, or exit status 0)
    "exit": int,              # escape_linux exit status
    "stdout_contains": str,   # substring of stdout (Stone: printed output or emitted value)
    "files_exist": list,      # absolute paths that exist afterward
    "file_contains": dict,    # {absolute path: substring}
    "unchanged": list,        # absolute paths the action must not change
}


def protected_paths(ctx: tb.RunContext) -> list[str]:
    if ctx.task_id in PROTECTED:
        return PROTECTED[ctx.task_id]
    return ["/app/tests"] if (ctx.app_dir / "tests").exists() else []


@dataclass
class TieredSandbox(pw.Sandbox):
    protected: list[str] = field(default_factory=list)

    @classmethod
    def for_task(cls, ctx: tb.RunContext) -> "TieredSandbox":
        base = pw.Sandbox.create(ctx)
        return cls(base.run_dir, base.dirs, base.waymark_bin, protected_paths(ctx))

    def host_path(self, logical: str) -> Path | None:
        for root, host in self.dirs.items():
            if logical == root or logical.startswith(root + "/"):
                return host / os.path.relpath(logical, root)
        return None

    def bwrap_argv(self, cwd: str, command: list[str]) -> list[str]:
        argv = super().bwrap_argv(cwd, command)
        # Read-only binds go after the writable binds so they mount on top.
        split = argv.index("--remount-ro")
        extra: list[str] = []
        for logical in self.protected:
            host = self.host_path(logical)
            if host is not None and host.exists():
                extra += ["--ro-bind", str(host), logical]
        return argv[:split] + extra + argv[split:]


# ------------------------------------------------------------ expectations


def validate_expect(raw: Any) -> tuple[dict[str, Any] | None, str | None]:
    if not isinstance(raw, dict) or not raw:
        return None, 'input.expect must be a non-empty object, for example {"ok": true, "files_exist": ["/work/out.txt"]}'
    unknown = sorted(set(raw) - set(EXPECT_KEYS))
    if unknown:
        return None, f"unknown expect keys {unknown}; allowed: {sorted(EXPECT_KEYS)}"
    for key, kind in EXPECT_KEYS.items():
        if key in raw and not isinstance(raw[key], kind):
            return None, f"expect.{key} must be a {kind.__name__}"
    return raw, None


def action_stdout(observation: dict[str, Any]) -> str:
    if observation.get("kind") == "stone_eval":
        response = observation.get("response") or {}
        output = (response.get("output") or {}).get("stdout", "")
        value = response.get("value")
        return output + ("" if value is None else json.dumps(value))
    return observation.get("stdout_tail") or ""


def check_expect(
    expect: dict[str, Any],
    observation: dict[str, Any],
    sandbox: TieredSandbox,
    changed: list[str],
) -> list[dict[str, Any]]:
    checks = []

    def add(check: str, expected: Any, actual: Any, met: bool) -> None:
        checks.append({"check": check, "expected": expected, "actual": actual, "met": met})

    if "ok" in expect:
        add("ok", expect["ok"], bool(observation.get("ok")), bool(observation.get("ok")) == expect["ok"])
    if "exit" in expect:
        actual = observation.get("returncode")
        add("exit", expect["exit"], actual, actual == expect["exit"])
    if "stdout_contains" in expect:
        out = action_stdout(observation)
        found = expect["stdout_contains"] in out
        add("stdout_contains", expect["stdout_contains"], "found" if found else f"not found (tail: {out[-200:]!r})", found)
    for path in expect.get("files_exist", []):
        host = sandbox.host_path(str(path))
        exists = bool(host and host.exists())
        add("files_exist", path, "exists" if exists else "missing", exists)
    for path, text in expect.get("file_contains", {}).items():
        host = sandbox.host_path(str(path))
        content = host.read_text(errors="replace") if host and host.is_file() else None
        found = content is not None and str(text) in content
        actual = "missing" if content is None else ("found" if found else f"not found (head: {content[:200]!r})")
        add("file_contains", {path: text}, actual, found)
    for path in expect.get("unchanged", []):
        touched = any(c == path or c.startswith(str(path) + "/") or str(path).startswith(c + "/") for c in changed)
        add("unchanged", path, "changed" if touched else "unchanged", not touched)
    return checks


# ------------------------------------------------------------ prompts


def action_schema(arm: str, protected: list[str]) -> str:
    expect = ',"expect":{"ok":true,"files_exist":["/work/out.txt"]}' if arm == "B" else ""
    text = (
        "\n\nAction schema:\n"
        f'{{"tool":"stone_eval","input":{{"source":"..."{expect}}}}}\n'
        '{"tool":"stone_help","input":{"name":"optional_builtin_or_topic"}}\n'
        f'{{"tool":"escape_linux","input":{{"command":"...","cwd":"/app","timeout_ms":30000{expect}}}}}\n'
        '{"tool":"finish","input":{}}\n'
        "Use one action per turn. Do not finish until the required /work output exists."
        "\nThe sandbox has no network. /app, /work, /tmp and $HOME are writable; everything else is read-only."
        "\nEach result lists the files the action changed."
    )
    if protected:
        text += "\nThese paths are read-only for this task: " + ", ".join(protected) + "."
    if arm == "B":
        text += (
            '\nEvery stone_eval and escape_linux action must include "expect" in its input: what you predict the action '
            "achieves. Allowed keys: ok (bool), exit (int), stdout_contains (str), files_exist (list of absolute paths), "
            "file_contains (object mapping an absolute path to a substring), unchanged (list of absolute paths). "
            "If the result does not match, you get the failed checks back."
        )
    return text


# ------------------------------------------------------------ episodes


@dataclass
class StepRecord:
    turn: int
    family: str
    signature: str
    ok: bool
    changed: int
    readonly_errors: int
    network_errors: int
    expect_given: bool
    expect_unmet: int
    format_error: str | None = None


@dataclass
class Episode:
    task_id: str
    model: str
    arm: str
    steps: list[StepRecord] = field(default_factory=list)
    resolved: bool = False
    finished: bool = False
    turns: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    parse_errors: int = 0
    lenient_parses: int = 0
    error: str | None = None
    verification: dict[str, Any] = field(default_factory=dict)


def signature(action: dict[str, Any]) -> str:
    inp = action.get("input") if isinstance(action.get("input"), dict) else {}
    body = inp.get("source") if action.get("tool") == "stone_eval" else inp.get("command")
    return f"{action.get('tool')}:{' '.join(str(body).split())}"


def step(ctx: tb.RunContext, sandbox: TieredSandbox, action: dict[str, Any], arm: str, turn: int,
         episode: Episode, args: argparse.Namespace) -> dict[str, Any]:
    family = pw.command_family(action)
    inp = action.get("input") if isinstance(action.get("input"), dict) else {}
    expect = None
    if arm == "B":
        if "expect" not in inp:
            episode.steps.append(StepRecord(turn, family, signature(action), False, 0, 0, 0, False, 0, "missing"))
            return {"ok": False, "error": 'missing "expect" in input: predict what this action achieves (see the action schema)'}
        expect, error = validate_expect(inp["expect"])
        if error:
            episode.steps.append(StepRecord(turn, family, signature(action), False, 0, 0, 0, False, 0, "malformed"))
            return {"ok": False, "error": error}
    before = sandbox.snapshot()
    observation = pw.execute(ctx, sandbox, action, args.timeout)
    after = sandbox.snapshot()
    changed = pw.observed_items(pw.diff_snapshots(before, after), before, after)
    observation["effects"] = {"changed": changed}
    text = json.dumps(observation)
    unmet: list[dict[str, Any]] = []
    if expect is not None:
        unmet = [c for c in check_expect(expect, observation, sandbox, changed) if not c["met"]]
        if unmet:
            observation["expectation"] = {"met": False, "failed_checks": unmet}
    if args.oracle == "step":
        observation["verification"] = pw.verify(ctx)
    episode.steps.append(StepRecord(
        turn, family, signature(action), bool(observation.get("ok")), len(changed),
        len(READONLY_ERROR.findall(text)), len(NETWORK_ERROR.findall(text)), expect is not None, len(unmet),
    ))
    return observation


def run_episode(task_id: str, model_key: str, arm: str, args: argparse.Namespace, out_dir: Path) -> Episode:
    ctx = pw.prepare(task_id, args, out_dir)
    sandbox = TieredSandbox.for_task(ctx)
    episode = Episode(task_id, model_key, arm)
    guide = tb.build_prompt_guide(args.waymark_bin, "full")
    system = (
        "You are a Stone Terminal-Bench agent. Return only JSON action objects. "
        "Use Stone for file, CSV, JSON, JSONL, text, find/glob, and simple transform work."
    )
    messages = [{
        "role": "user",
        "content": guide
        + "\n\nTask instruction:\n" + ctx.instruction
        + "\n\nHarness execution constraints:\n"
        "- Each stone_eval call runs in a fresh Waymark CLI process; variables do not persist between tool calls.\n"
        "- Repeat needed reads and setup in the same stone_eval source that uses them.\n"
        + action_schema(arm, sandbox.protected),
    }]
    for turn in range(1, args.max_turns + 1):
        episode.turns = turn
        try:
            text, usage = pw.chat(model_key, args, system, messages)
        except Exception as err:
            episode.error = f"{type(err).__name__}: {err}"
            break
        episode.input_tokens += usage["prompt_tokens"]
        episode.output_tokens += usage["completion_tokens"]
        action, parse_error, lenient = pw.parse_action(text)
        episode.lenient_parses += lenient
        episode.parse_errors += parse_error is not None
        tb.write_json_file(ctx.log_dir / f"action-{turn:03d}.json", {"raw": text, "action": action, "error": parse_error, "lenient": lenient})
        tool = action.get("tool") if not parse_error else None
        if parse_error:
            observation: dict[str, Any] = {"ok": False, "error": parse_error, "guidance": "Return one JSON action object only."}
        elif tool == "finish":
            verification = pw.verify(ctx)
            observation = {"ok": verification["ok"], "finished": True, "verification": verification}
            episode.finished = True
        elif tool == "stone_help":
            observation = tb.stone_help(ctx, (action.get("input") or {}).get("name"), args.timeout)
        elif tool in ("stone_eval", "escape_linux"):
            observation = step(ctx, sandbox, action, arm, turn, episode, args)
        else:
            observation = {"ok": False, "error": f"unsupported tool {tool!r}"}
        tb.write_json_file(ctx.log_dir / f"observation-{turn:03d}.json", observation)
        messages.append({"role": "assistant", "content": text if text.strip() else "{}"})
        messages.append({
            "role": "user",
            "content": "Observation:\n" + tb.truncate(json.dumps(observation, indent=2, sort_keys=True), 12000)
            + "\nIf the task-required /work output exists and is correct, return finish. Otherwise continue.",
        })
        if episode.finished:
            # A failed finish shows the verifier once; the model may keep working.
            if observation["ok"]:
                break
            episode.finished = False
    episode.verification = pw.verify(ctx)
    episode.resolved = bool(episode.verification.get("ok"))
    return episode


# ------------------------------------------------------------ reporting


def repeats_after_failure(steps: list[StepRecord]) -> int:
    return sum(1 for prev, cur in zip(steps, steps[1:]) if not prev.ok and cur.signature == prev.signature)


def steps_to_recovery(steps: list[StepRecord]) -> int | None:
    """Actions from the first failed action to the next successful one (design doc 11.1 proxy)."""
    for i, s in enumerate(steps):
        if not s.ok:
            for j in range(i + 1, len(steps)):
                if steps[j].ok:
                    return j - i
            return None
    return None


def summarize(episodes: list[Episode]) -> dict[str, Any]:
    groups: dict[tuple[str, str], list[Episode]] = {}
    for ep in episodes:
        groups.setdefault((ep.model, ep.arm), []).append(ep)
    rows = []
    for (model, arm), eps in sorted(groups.items()):
        steps = [s for ep in eps for s in ep.steps]
        recoveries = [r for r in (steps_to_recovery(ep.steps) for ep in eps) if r is not None]
        rows.append({
            "model": model,
            "arm": arm,
            "episodes": len(eps),
            "resolved": sum(ep.resolved for ep in eps),
            "harness_errors": sum(ep.error is not None for ep in eps),
            "mean_turns": round(sum(ep.turns for ep in eps) / len(eps), 2),
            "tokens_in": sum(ep.input_tokens for ep in eps),
            "tokens_out": sum(ep.output_tokens for ep in eps),
            "parse_errors": sum(ep.parse_errors for ep in eps),
            "lenient_parses": sum(ep.lenient_parses for ep in eps),
            "effect_actions": len(steps),
            "expect_format_errors": sum(s.format_error is not None for s in steps),
            "expect_given": sum(s.expect_given for s in steps),
            "expect_mismatches": sum(s.expect_unmet > 0 for s in steps),
            "repeats_after_failure": sum(repeats_after_failure(ep.steps) for ep in eps),
            "mean_steps_to_recovery": round(sum(recoveries) / len(recoveries), 2) if recoveries else None,
            "readonly_errors": sum(s.readonly_errors for s in steps),
            "network_errors": sum(s.network_errors for s in steps),
            "by_family": dict(Counter(s.family for s in steps)),
        })
    return {"groups": rows}


def episode_json(ep: Episode) -> dict[str, Any]:
    return {
        "task_id": ep.task_id, "model": ep.model, "arm": ep.arm, "resolved": ep.resolved, "turns": ep.turns,
        "finished": ep.finished, "error": ep.error, "input_tokens": ep.input_tokens, "output_tokens": ep.output_tokens,
        "parse_errors": ep.parse_errors, "lenient_parses": ep.lenient_parses,
        "verification": ep.verification, "steps": [s.__dict__ for s in ep.steps],
    }


# ------------------------------------------------------------ dry run


def self_test_expectations(sandbox: TieredSandbox) -> list[str]:
    """Check the expectation checker on synthetic observations (no model, no sandbox run)."""
    failures = []
    work = sandbox.dirs["/work"]
    (work / "out.txt").write_text("OK done\n")
    obs = {"ok": True, "kind": "escape_linux", "returncode": 0, "stdout_tail": "3 passed\n"}
    met = check_expect(
        {"ok": True, "exit": 0, "stdout_contains": "passed", "files_exist": ["/work/out.txt"],
         "file_contains": {"/work/out.txt": "OK"}, "unchanged": ["/app/tests"]},
        obs, sandbox, ["/work/out.txt"],
    )
    if not all(c["met"] for c in met):
        failures.append(f"expected all checks met: {met}")
    unmet = check_expect(
        {"exit": 0, "stdout_contains": "FAILED", "files_exist": ["/work/missing.txt"], "unchanged": ["/work"]},
        obs, sandbox, ["/work/out.txt"],
    )
    if [c["check"] for c in unmet if not c["met"]] != ["stdout_contains", "files_exist", "unchanged"]:
        failures.append(f"expected three unmet checks: {unmet}")
    for bad in ({}, {"exit_code": 0}, {"ok": "yes"}):
        if validate_expect(bad)[1] is None:
            failures.append(f"expect {bad!r} should be rejected")
    stone_obs = {"ok": True, "kind": "stone_eval", "response": {"ok": True, "value": {"rows": 3}, "output": {"stdout": ""}}}
    if not check_expect({"stdout_contains": '"rows": 3'}, stone_obs, sandbox, [])[0]["met"]:
        failures.append("stone_eval value should count as output for stdout_contains")
    (work / "out.txt").unlink()
    return failures


def dry_run(args: argparse.Namespace, out_dir: Path) -> int:
    failures = 0
    for task_id in args.tasks:
        try:
            ctx = pw.prepare(task_id, args, out_dir)
            sandbox = TieredSandbox.for_task(ctx)
            problems = self_test_expectations(sandbox)
            for logical in sandbox.protected:
                target = logical if (sandbox.host_path(logical) or Path()).is_dir() else os.path.dirname(logical)
                probe = f"{target}/.probe" if target == logical else logical
                result = sandbox.run("/app", ["bash", "-c", f"echo x >> {probe} 2>&1; echo rc=$?"], 30)
                # Bash reports a failed redirect on its own stderr, outside the command's 2>&1.
                output = result.stdout + result.stderr
                if "rc=0" in output or not READONLY_ERROR.search(output):
                    problems.append(f"protected path {logical} is writable: {output.strip()}")
            before = sandbox.snapshot()
            writable = sandbox.run("/app", ["bash", "-c", "touch /app/.probe /work/.probe"], 30)
            changes = pw.diff_snapshots(before, sandbox.snapshot())
            if writable.returncode != 0 or sorted(changes["created"]) != ["/app/.probe", "/work/.probe"]:
                problems.append(f"writable dirs not observed: {changes}")
            net = sandbox.run("/app", ["python3", "-c", "import socket; socket.create_connection(('1.1.1.1', 53), 3)"], 30)
            if net.returncode == 0:
                problems.append("network is reachable")
            stone = sandbox.run("/app", ["waymark", "eval", "-c", "emit(1 + 1)"], 30)
            if '"value":2' not in stone.stdout:
                problems.append("waymark eval failed in sandbox")
            print(json.dumps({"task": task_id, "ok": not problems, "protected": sandbox.protected, "problems": problems}))
            failures += bool(problems)
        except Exception as err:
            print(json.dumps({"task": task_id, "ok": False, "error": f"{type(err).__name__}: {err}"}))
            failures += 1
    for arm in args.arms:
        print(f"--- action schema for arm {arm} ---{action_schema(arm, ['/app/tests'])}\n")
    return 1 if failures else 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--models", nargs="+", default=["qwen-local"], choices=sorted(pw.MODELS))
    parser.add_argument("--arms", nargs="+", default=list(ARMS), choices=ARMS)
    parser.add_argument("--tasks", nargs="+", default=list(pw.DEFAULT_TASKS))
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--oracle", choices=("finish", "step"), default="finish",
                        help="when the task verifier is shown to the model (default: only on finish)")
    parser.add_argument("--tasks-root", default=str(tb.DEFAULT_TASKS_ROOT))
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument("--fixture-cache-dir", type=Path, default=ROOT / "target/runs/stone-tbench4-fixtures")
    parser.add_argument("--out-dir", type=Path)
    parser.add_argument("--max-turns", type=int, default=12)
    parser.add_argument("--max-tokens", type=int, default=8192)
    parser.add_argument("--timeout", type=float, default=120.0, help="per-action timeout in seconds")
    parser.add_argument("--model-timeout", type=float, default=300.0)
    parser.add_argument("--local-model", default="local")
    parser.add_argument("--local-temperature", type=float, default=0.7)
    parser.add_argument("--dry-run", action="store_true", help="check sandbox tiers, schemas, and the expectation checker; no model calls")
    args = parser.parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    return args


def main() -> int:
    args = parse_args()
    out_dir = (args.out_dir or ROOT / "target/runs" / f"outcome-prediction-ablation-{time.strftime('%Y%m%dT%H%M%S')}").resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    if args.dry_run:
        return dry_run(args, out_dir)
    episodes = []
    for repeat in range(args.repeats):
        for model_key in args.models:
            for arm in args.arms:
                for task_id in args.tasks:
                    run_out = out_dir / model_key / arm / f"r{repeat}"
                    run_out.mkdir(parents=True, exist_ok=True)
                    ep = run_episode(task_id, model_key, arm, args, run_out)
                    episodes.append(ep)
                    tb.write_json_file(run_out / task_id / "episode.json", episode_json(ep))
                    print(json.dumps({k: v for k, v in episode_json(ep).items() if k not in ("steps", "verification")}), flush=True)
                    tb.write_json_file(out_dir / "summary.json", summarize(episodes))
    print(f"wrote {out_dir / 'summary.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
