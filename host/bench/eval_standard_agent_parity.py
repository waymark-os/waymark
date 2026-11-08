#!/usr/bin/env python3
"""Compare two standard Stone controllers (by default V14 and V15) on the
same tasks, each run as a Gateway attempt.

Scripted mode (default) replays fixed model answers from a small local
OpenAI-compatible server that records every request, so both controllers
see the same answers and their prompts can be measured. It compares the
outcome, model-call count, memory-write sequence, workspace changes, and
control counters. --live runs a few small tasks against a real model
instead (by default the local llama.cpp server used for development).

Needs a built waymark and waymark-gateway, and Docker for commands.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import eval_standard_agent_completion_critique as completion  # noqa: E402
import eval_standard_agent_run_cleanup as cleanup  # noqa: E402
import eval_standard_agent_runtime_owned_run as owned  # noqa: E402

DEFAULT_GATEWAY = ROOT.parent / "waymark-gateway/target/debug/waymark-gateway"
DEFAULT_OLD = ROOT / "examples/scripts/standard_attempt_agent.stone"
DEFAULT_NEW = ROOT / "examples/scripts/standard_attempt_agent_v15.stone"


# ---------------------------------------------------------------------------
# Tasks

OBJECTIVE = (
    "Create /app/result.txt containing READY followed by one newline, "
    "read it back to verify the exact bytes, then finish with answer "
    f"{completion.ANSWER}."
)
action = completion.action
WRITE = action({"tool": "write", "input": {"path": "/app/result.txt", "content": "READY\n"}})
READ = action({"tool": "read", "input": {"path": "/app/result.txt"}})
FINAL = action({"final": {"answer": completion.ANSWER}})
FINISH = action({"tool": "finish", "input": {"answer": completion.ANSWER}})
MISSING = action({"tool": "read", "input": {"path": "/app/missing.txt"}})


def task_input(sequence, critique, *, budget=None, proactive=False, partial=False, stagnant=3):
    budget = budget if budget is not None else len(sequence)
    return {
        "max_turns": budget,
        "max_rounds": budget,
        "completion_critique": critique,
        "proactive_completion_checkpoint": proactive,
        "finalization_window": 4,
        "max_completion_critiques": 2,
        "max_stagnant_actions": stagnant,
        "report_partial_on_limit": partial,
    }


def scripted_tasks() -> dict[str, dict[str, Any]]:
    tasks = {}

    def add(name, sequence, inputs, files=()):
        tasks[name] = {"objective": OBJECTIVE, "answers": sequence, "input": inputs, "files": list(files)}

    seq = completion.treatment_sequence()
    add("critique-reject-then-approve", seq, task_input(seq, True))
    seq = completion.ablation_sequence()
    add("no-critique", seq, task_input(seq, False))
    seq = completion.inconsistent_approval_sequence()
    add("inconsistent-approval", seq, task_input(seq, True))
    seq = completion.proactive_finalization_sequence()
    add("budget-checkpoint", seq, task_input(seq, True, budget=8, proactive=True))
    seq = completion.no_checkpoint_limit_sequence()
    add("model-call-limit-partial", seq, task_input(seq, True, budget=8, partial=True, stagnant=16))
    seq = owned.sequence()
    add("run-complete", seq, task_input(seq, True, budget=4))
    seq = cleanup.cleanup_sequence()
    add("timed-out-run-reaped", seq, task_input(seq, True, budget=5))
    seq = [action({"tool": "run_start", "input": {"argv": ["/bin/true"], "timeout_ms": 5}}), WRITE, READ, FINISH]
    add("repair-bad-field", seq, task_input(seq, False, budget=6))
    seq = ["I will write the file now.", WRITE, READ, FINAL]
    add("repair-not-json", seq, task_input(seq, False, budget=6))
    bad = action({"tool": "write", "input": {"path": "", "content": "x"}})
    add("repair-exhausted", [bad, bad], task_input([bad, bad], False, budget=6))
    seq = [MISSING, MISSING, MISSING, FINAL]
    add("repeated-failing-read", seq, task_input(seq, False, budget=8))
    seq = [
        action({"tool": "edit", "input": {"path": "/app/seed.txt", "old": "one", "new": "two"}}),
        action({"tool": "read", "input": {"path": "/app/seed.txt"}}),
        FINISH,
    ]
    add("edit-file", seq, task_input(seq, False, budget=4), files=[("seed.txt", "one\n")])
    return tasks


LIVE_TASKS = {
    "write-and-verify": {
        "objective": OBJECTIVE.replace(completion.ANSWER, "verified-ready"),
        "files": [],
    },
    "fix-bug": {
        "objective": "The test /app/test_calc.py fails. Fix the bug in /app/calc.py (do not change the test) so that `python3 /app/test_calc.py` prints OK, run it to confirm, then finish with a one-line summary.",
        "files": [
            ("calc.py", "def add(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n"),
            ("test_calc.py", "import sys\nsys.path.insert(0, '/app')\nfrom calc import add, mul\nassert add(2, 3) == 5, add(2, 3)\nassert mul(2, 3) == 6\nprint('OK')\n"),
        ],
    },
    "csv-total": {
        "objective": "Compute the sum of the `amount` column in /app/sales.csv and write just that number followed by a newline to /app/total.txt. Verify the file, then finish with the number as the answer.",
        "files": [("sales.csv", "region,amount\nnorth,120\nsouth,75\neast,305\nwest,42\n")],
    },
}


# ---------------------------------------------------------------------------
# A scripted OpenAI-compatible model that records requests

def start_scripted_model(answers: list[str]) -> tuple[ThreadingHTTPServer, list[dict[str, Any]]]:
    requests: list[dict[str, Any]] = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, payload):
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            self.reply({"data": [{"id": "scripted"}]})

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
            index = len(requests)
            requests.append(request)
            text = answers[index] if index < len(answers) else '{"error": "scripted answers exhausted"}'
            prompt = sum(len(str(m.get("content", ""))) for m in request.get("messages", []))
            self.reply({
                "id": f"scripted-{index}",
                "object": "chat.completion",
                "model": "scripted",
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": text}}],
                "usage": {"prompt_tokens": prompt // 4, "completion_tokens": len(text) // 4,
                          "total_tokens": (prompt + len(text)) // 4},
            })

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, requests


# ---------------------------------------------------------------------------
# One attempt

def run(argv, env=None, timeout=60, check=True):
    done = subprocess.run(argv, env=env, text=True, capture_output=True, timeout=timeout)
    if check and done.returncode != 0:
        raise RuntimeError(f"{argv[:4]} failed:\n{done.stdout}\n{done.stderr}")
    return done


def wait_for_socket(path: Path, server: subprocess.Popen, timeout=20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if server.poll() is not None:
            raise RuntimeError(f"gateway exited: {server.stderr.read()}")
        if path.exists():
            try:
                with socket.socket(socket.AF_UNIX) as probe:
                    probe.connect(str(path))
                    return
            except OSError:
                pass
        time.sleep(0.1)
    raise RuntimeError("gateway socket did not appear")


def stone_literal(value: Any) -> str:
    return json.dumps(value).replace("true", "True").replace("false", "False").replace("null", "None")


def run_attempt(args, source: Path, task: dict[str, Any], live: bool) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="wm-parity-", dir="/tmp") as tmp:
        root = Path(tmp)
        data, work, sock = root / "data", root / "workspace", root / "gw.sock"
        work.mkdir()
        (work / ".keep").write_text("")
        for name, content in task.get("files", []):
            (work / name).write_text(content)
        gw = [str(args.gateway_bin), "--data-root", str(data)]
        run(gw + ["repo", "snapshot", "--name", "repo", "--path", str(work)])
        tx = run(gw + ["env", "snapshot", "--workspace", "repo"]).stdout.strip()
        server_env = dict(os.environ)
        scripted, requests = (None, [])
        if live:
            server_env.update({
                "WAYMARK_MODEL_PROVIDER": "vllm",
                "WAYMARK_MODEL_BASE_URL": args.live_base_url,
                "WAYMARK_MODEL_DEFAULT": args.live_model,
                "WAYMARK_MODEL_DISABLE_THINKING": "true",
                "WAYMARK_MODEL_HTTP_TIMEOUT_MS": "300000",
            })
        else:
            scripted, requests = start_scripted_model(task["answers"])
            server_env.update({
                "WAYMARK_MODEL_PROVIDER": "vllm",
                "WAYMARK_MODEL_BASE_URL": f"http://127.0.0.1:{scripted.server_address[1]}/v1",
                "WAYMARK_MODEL_DEFAULT": "scripted",
            })
        # The controller process inherits the server's environment, so the
        # command image must be set here; without it every command is
        # refused by the Gateway.
        server_env.update({
            "WAYMARK_STONE_BIN": str(args.waymark_bin),
            "WAYMARK_GATEWAY_IMAGE": args.image,
            "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
        })
        gateway = subprocess.Popen(gw + ["rpc", "serve", "--socket", str(sock)], env=server_env,
                                   text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        client_env = dict(os.environ)
        client_env.update({
            "WAYMARK_STONE_BIN": str(args.waymark_bin),
            "WAYMARK_GATEWAY_SOCKET": str(sock),
            "WAYMARK_GATEWAY_TX": tx,
            "WAYMARK_GATEWAY_IMAGE": args.image,
            "WAYMARK_GATEWAY_WORKSPACE_MOUNT": "/app",
            "WAYMARK_GATEWAY_MODEL_CLASS": "agent",
        })
        timeout_ms = int(args.timeout * 1000)
        driver = root / "driver.stone"
        driver.write_text(
            f"a = attempt_spawn(task_spec={stone_literal({'id': 'parity', 'objective': task['objective']})}, "
            f"task_input={stone_literal(task.get('input', {}))}, "
            'workspace_source={"workspace": "repo"}, '
            f'program={{"kind": "stone", "source": {json.dumps(source.read_text())}}}, '
            'controller="stone", workspace_mount="/app")\n'
            "attempt_start(a.attempt)\n"
            f"w = attempt_wait(a.attempt, timeout_ms={timeout_ms})\n"
            'emit({"wait": w, "inspect": attempt_inspect(a.attempt, trace_limit=1, max_bytes=65536)})\n'
        )
        started = time.time()
        try:
            wait_for_socket(sock, gateway)
            done = run([str(args.waymark_bin), "eval", str(driver)], env=client_env,
                       timeout=args.timeout + 120, check=False)
            payload = json.loads(done.stdout) if done.stdout.strip().startswith("{") else {
                "error": done.stdout[-1000:] + done.stderr[-1000:]}
        finally:
            gateway.terminate()
            try:
                gateway.wait(timeout=10)
            except subprocess.TimeoutExpired:
                gateway.kill()
            if scripted:
                scripted.shutdown()
        summary = summarize(payload, gw, data)
        summary["elapsed_s"] = round(time.time() - started, 1)
        summary["request_characters"] = [
            sum(len(str(m.get("content", ""))) for m in r.get("messages", [])) for r in requests]
        summary.update(prefix_reuse(requests))
        return summary


def prefix_reuse(requests: list[dict[str, Any]]) -> dict[str, int]:
    """How much of each prompt a prefix cache could skip: the leading
    characters shared with the previous prompt (one cache slot) and with any
    earlier prompt (a cache that keeps every prompt)."""
    texts = ["".join(f"\x00{m.get('role')}\x00{m.get('content', '')}" for m in r.get("messages", []))
             for r in requests]

    def shared(a: str, b: str) -> int:
        n = 0
        for x, y in zip(a, b):
            if x != y:
                break
            n += 1
        return n

    previous = sum(shared(texts[i - 1], texts[i]) for i in range(1, len(texts)))
    any_earlier = sum(max((shared(t, texts[i]) for t in texts[:i]), default=0)
                      for i in range(1, len(texts)))
    return {"prompt_characters": sum(map(len, texts)), "prefix_reused_previous": previous,
            "prefix_reused_any": any_earlier}


def summarize(payload: dict[str, Any], gw: list[str], data: Path) -> dict[str, Any]:
    value = payload.get("value")
    if not isinstance(value, dict):
        return {"ok": False, "harness_error": payload.get("error") or payload}
    inspect = value["inspect"]
    # attempt_inspect returns a bounded trace; read the whole operation log.
    attempt_id = (inspect.get("attempt") or {}).get("attempt")
    trace = []
    log = data / "traces/operations.jsonl"
    if log.is_file():
        for line in log.read_text().splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if event.get("attempt") == attempt_id:
                trace.append(event)
    meta = value["wait"].get("metadata") or {}
    controller: dict[str, Any] = {}
    stdout_path = Path(meta.get("controller_stdout_path") or "/nonexistent")
    if stdout_path.is_file():
        for line in reversed(stdout_path.read_text().splitlines()):
            try:
                controller = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
    stderr_path = Path(meta.get("controller_stderr_path") or "/nonexistent")
    if not controller and stderr_path.is_file():
        try:
            controller = json.loads(stderr_path.read_text().strip().splitlines()[-1])
        except (json.JSONDecodeError, IndexError):
            pass
    diff = {}
    tx = (inspect.get("attempt") or {}).get("tx")
    if tx:
        try:
            diff = json.loads(run(gw + ["env", "diff", tx, "--json"], check=False).stdout)
        except json.JSONDecodeError:
            pass
    calls = [e for e in trace if e.get("op") == "attempt.rpc.model.call"]
    result = controller.get("value") if isinstance(controller.get("value"), dict) else {}
    error = controller.get("error") if isinstance(controller.get("error"), dict) else {}
    return {
        "ok": controller.get("ok") is True,
        "answer": result.get("answer"),
        "completion": result.get("completion"),
        "error": error.get("code"),
        "error_type": error.get("type"),
        "error_message": error.get("detail") or error.get("message"),
        "model_calls": len(calls),
        "commands": sum(1 for e in trace if e.get("op") == "attempt.rpc.linux.exec"),
        "gateway_errors": [[e.get("operation"), e.get("kind")] for e in trace
                           if e.get("op") == "attempt.rpc.error"],
        "input_tokens": sum(int(e.get("input_tokens") or 0) for e in calls),
        "output_tokens": sum(int(e.get("output_tokens") or 0) for e in calls),
        "memory_writes": [[e.get("key"), e.get("kind"), e.get("status")] for e in trace
                          if e.get("op") == "attempt.memory.write"],
        "files": [[s["kind"], s["path"]] for s in diff.get("samples", [])],
        "ops": {op: sum(1 for e in trace if e.get("op") == op)
                for op in sorted({str(e.get("op")) for e in trace})
                if not op.startswith("attempt.channel") and "inspect" not in op},
        "control": {k: v for k, v in (result.get("_control") or {}).items()
                    if k not in ("name", "last_model_call", "last_transition_id",
                                 "peak_action_context_characters")},
    }


# ---------------------------------------------------------------------------

# The error code is reported but not compared: an answer that cannot be
# repaired ends V14 with model_infer's model_schema_validation_failed and V15
# with model.ask's ModelOutputError.
COMPARED = ["ok", "answer", "completion", "model_calls", "commands", "gateway_errors",
            "memory_writes", "files", "ops", "control"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("tasks", nargs="*", help="task names (default: all)")
    parser.add_argument("--old", type=Path, default=DEFAULT_OLD)
    parser.add_argument("--new", type=Path, default=DEFAULT_NEW)
    parser.add_argument("--live", action="store_true", help="run the live tasks against a real model")
    parser.add_argument("--live-base-url", default="http://127.0.0.1:8080/v1")
    parser.add_argument("--live-model", default="/models/Qwen3.8-27B-UD-Q4_K_XL.gguf")
    parser.add_argument("--waymark-bin", type=Path, default=ROOT / "target/debug/waymark")
    parser.add_argument("--gateway-bin", type=Path, default=DEFAULT_GATEWAY)
    parser.add_argument("--image", default="python:3.12-slim")
    parser.add_argument("--timeout", type=float, default=600.0, help="seconds per attempt")
    parser.add_argument("--out", type=Path, help="write the full report here")
    args = parser.parse_args()
    args.waymark_bin = args.waymark_bin.resolve()
    args.gateway_bin = args.gateway_bin.resolve()

    catalog = LIVE_TASKS if args.live else scripted_tasks()
    if args.live:
        for task in catalog.values():
            task.setdefault("input", {"max_turns": 12, "max_rounds": 12})
    names = args.tasks or list(catalog)
    report: dict[str, Any] = {}
    mismatches = 0
    for name in names:
        old = run_attempt(args, args.old.resolve(), catalog[name], args.live)
        new = run_attempt(args, args.new.resolve(), catalog[name], args.live)
        differences = {key: [old.get(key), new.get(key)] for key in COMPARED if old.get(key) != new.get(key)}
        if not args.live and differences:
            mismatches += 1
        report[name] = {"old": old, "new": new, "differences": differences}
        brief = {
            side: {k: run_result.get(k) for k in ("ok", "answer", "error", "error_type", "model_calls",
                                                   "commands", "gateway_errors",
                                                   "input_tokens", "output_tokens", "prompt_characters",
                                                   "prefix_reused_previous", "prefix_reused_any",
                                                   "elapsed_s")}
            for side, run_result in (("old", old), ("new", new))
        }
        print(json.dumps({"task": name, "same": not differences, **brief,
                          "differences": sorted(differences)}), flush=True)
    if args.out:
        args.out.write_text(json.dumps(report, indent=1) + "\n")
    return 1 if mismatches else 0


if __name__ == "__main__":
    sys.exit(main())
