#!/usr/bin/env python3
"""Measure Stone's divergence from planned semantics (design doc S1, L6a, L18).

Subcommands:

  probes    Run small programs in `waymark eval` and in CPython. For each
            planned change, report whether Stone matches the target today.
            For L1-L4 the target is CPython's behavior, because that is
            what Python-trained models expect when they write Stone. For
            Stone's deliberate deviations (L6a, item 4, the L18 shadowing
            error), the target is an explicit rejection. A change is done
            when all of its probes match. Probes tagged "other" measure
            limits that are not planned changes.

  snapshot  Run the executable corpus (example scripts, reference scripts,
            and help() examples) in a scratch copy and record each unit's
            result. Units whose result differs between two runs are marked
            unstable.

  diff      Re-run the corpus and list units whose result changed against a
            snapshot. Run this after each semantics change to measure its
            real impact.

See docs/STONE_LANGUAGE_CONTROL_DESIGN.md, sections 7 and 9.5.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_WAYMARK_BIN = ROOT / "target" / "debug" / "waymark"
sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_stone_help_examples as help_check  # noqa: E402


@dataclass(frozen=True)
class Probe:
    id: str
    change: str
    source: str
    # "python": the target is CPython's behavior.
    # "error": the target is a rejected program (parse, admission, or runtime error).
    # "python-if-d2": CPython's behavior if D-2 extends L4 to containers.
    target: str = "python"
    note: str = ""


def p(id: str, change: str, source: str, target: str = "python", note: str = "") -> Probe:
    return Probe(id, change, source.strip("\n") + "\n", target, note)


PROBES: list[Probe] = [
    # L1: lexical scoping and closures.
    p("l1_closure_capture", "L1", """
def make(n):
    def add(x):
        return x + n
    return add
add = make(2)
print(add(3))
"""),
    p("l1_closure_not_dynamic", "L1", """
def make(n):
    def add(x):
        return x + n
    return add
add = make(2)
n = 100
print(add(3))
""", note="the closure must use its own n, not a later global n"),
    p("l1_closure_state", "L1", """
def counter():
    c = [0]
    def inc():
        c.append(1)
        return len(c)
    return inc
inc = counter()
inc()
print(inc())
"""),
    p("l1_closure_sees_rebind", "L1", """
def outer():
    v = 1
    def get():
        return v
    v = 2
    return get()
print(outer())
"""),
    p("l1_late_binding", "L1", """
def make_all():
    fs = []
    for i in range(3):
        def f():
            return i
        fs.append(f)
    return fs
fs = make_all()
f0 = fs[0]
print(f0())
""", note="Python closures bind late: 2"),
    # Not in the plan: call-target and callable-value limits found by the probes.
    p("other_call_call_result", "other", """
def make(n):
    def add(x):
        return x + n
    return add
print(make(2)(3))
""", note="calling a call's result"),
    p("other_lambda_in_list", "other", """
fs = [lambda: 1]
f0 = fs[0]
print(f0())
""", note="callables stored in containers"),
    p("l1_no_dynamic_scope", "L1", """
def g():
    return y
def f():
    y = 1
    return g()
try:
    print(f())
except Exception as e:
    print("error")
""", note="g must not see f's local y"),
    p("l1_assign_is_local", "L1", """
x = 1
def f():
    x = 2
    return x
f()
print(x)
"""),
    p("l1_global_seen_late", "L1", """
def f():
    return z
z = 5
print(f())
"""),
    # L2: reference semantics for lists and dicts.
    p("l2_alias_list", "L2", """
a = [1]
b = a
b.append(2)
print(len(a))
"""),
    p("l2_param_mutation", "L2", """
def f(items):
    items.append(1)
x = []
f(x)
print(len(x))
"""),
    p("l2_alias_dict", "L2", """
d = {}
e = d
e["k"] = 1
print(len(d))
"""),
    p("l2_nested_alias", "L2", """
m = {"l": []}
inner = m["l"]
inner.append(1)
print(len(m["l"]))
"""),
    p("l2_explicit_copy", "L2", """
a = [1]
b = list(a)
b.append(2)
print(len(a))
"""),
    # L3: `or` / `and` return an operand.
    p("l3_or_default", "L3", 'print([] or "d")\n'),
    p("l3_or_first", "L3", 'print("x" or "y")\n'),
    p("l3_and_last", "L3", 'print(1 and "x")\n'),
    p("l3_and_short", "L3", 'print(0 and "x")\n'),
    p("l3_none_or", "L3", "v = None or 5\nprint(v)\n"),
    p("l3_test_context", "L3", """
if [] or 0:
    print("t")
else:
    print("f")
"""),
    # L4: Python str() and round() for scalars.
    p("l4_str_none", "L4", 'print(str(None) + "|")\n'),
    p("l4_str_bool", "L4", "print(str(True))\n"),
    p("l4_str_float", "L4", "print(str(1.0))\n"),
    p("l4_print_none", "L4", "print(None)\n"),
    p("l4_print_bool", "L4", "print(False)\n"),
    p("l4_fstring_none", "L4", 'print(f"{None}")\n'),
    p("l4_float_repr", "L4", "print(0.1 + 0.2)\n"),
    p("l4_round_half", "L4", "print(round(2.675, 2))\n", note="binary float: 2.67"),
    p("l4_round_bankers", "L4", "print(round(0.5) + round(1.5) + round(2.5))\n",
      note="round-half-even: 0+2+2"),
    p("l4_round_int", "L4", 'print(str(round(2.4)) + "|")\n'),
    p("l4_str_compare", "L4", 'print("yes" if str(True) == "True" else "no")\n'),
    # D-2: str() of containers (open decision).
    p("d2_str_list", "D-2", "print(str([1.0, None, True]))\n", target="python-if-d2"),
    p("d2_str_dict", "D-2", 'print(str({"a": None}))\n', target="python-if-d2"),
    p("d2_str_nested_str", "D-2", 'print(str(["x"]))\n', target="python-if-d2"),
    p("d2_print_list", "D-2", 'print([1, "x"])\n', target="python-if-d2",
      note="D-2 proposal keeps print() of containers as JSON"),
    # L6a and item 4: exception handlers.
    p("l6a_bare_except", "L6a", """
try:
    x = 1 / 0
except:
    print("caught")
""", target="error", note="parse error; suggest `except Exception as e:`"),
    p("l6a_pass_handler", "L6a", """
try:
    x = 1 / 0
except Exception:
    pass
print("after")
""", target="error", note="admission error for pass-only handler"),
    p("l6a_named_handler_ok", "L6a", """
try:
    x = 1 / 0
except Exception as e:
    print("caught")
"""),
    p("item4_base_exception", "item 4", """
try:
    x = 1 / 0
except BaseException:
    print("caught")
""", target="error", note="BaseException may not be caught"),
    # L18: one namespace, frozen builtins, Python shadowing.
    p("l18_noncallable_shadow", "L18", "len = 5\nprint(len([1]))\n", target="error",
      note="prescriptive error naming the binding site"),
    p("l18_def_shadows", "L18", """
def len(x):
    return 42
print(len([1, 2]))
"""),
    p("l18_local_shadow", "L18", """
def f():
    sum = 0
    for i in [1, 2]:
        sum += i
    return sum
print(f())
"""),
    p("l18_unrelated_binding", "L18", "state = 1\nprint(len([1]) + state)\n"),
]


def run_stone(waymark_bin: Path, source: str, cwd: Path) -> dict[str, Any]:
    proc = subprocess.run(
        [str(waymark_bin), "eval", "-c", source],
        cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
    )
    payload = help_check.parse_waymark_payload(proc.stdout) or help_check.parse_waymark_payload(proc.stderr)
    if payload is None:
        return {"ok": False, "stdout": "", "error": f"no payload (exit {proc.returncode})"}
    stdout = (payload.get("output") or {}).get("stdout", "")
    error = payload.get("error") or {}
    detail = error.get("detail") or error.get("message") or ""
    return {"ok": bool(payload.get("ok")), "stdout": stdout, "error": detail}


def run_python(source: str, cwd: Path) -> dict[str, Any]:
    proc = subprocess.run(
        [sys.executable, "-I", "-c", source],
        cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
    )
    last = proc.stderr.strip().splitlines()[-1] if proc.stderr.strip() else ""
    return {"ok": proc.returncode == 0, "stdout": proc.stdout, "error": last}


def judge(probe: Probe, stone: dict[str, Any], python: dict[str, Any]) -> str:
    if probe.target == "error":
        return "match" if not stone["ok"] else "diverges"
    same = stone["ok"] == python["ok"] and stone["stdout"] == python["stdout"]
    if probe.target == "python-if-d2":
        return "match" if same else "diverges (D-2)"
    return "match" if same else "diverges"


def show(result: dict[str, Any]) -> str:
    if result["ok"]:
        return result["stdout"].rstrip("\n").replace("\n", "\\n") or "(no output)"
    out = result["stdout"].rstrip("\n").replace("\n", "\\n")
    err = (result["error"] or "error")[:60]
    return (out + " " if out else "") + f"<error: {err}>"


def cmd_probes(args: argparse.Namespace) -> int:
    rows = []
    with tempfile.TemporaryDirectory(prefix="stone-compat-probes-") as temp:
        cwd = Path(temp)
        for probe in PROBES:
            stone = run_stone(args.waymark_bin, probe.source, cwd)
            python = run_python(probe.source, cwd)
            target = "error" if probe.target == "error" else show(python)
            rows.append({
                "id": probe.id, "change": probe.change, "verdict": judge(probe, stone, python),
                "stone": show(stone), "target": target, "note": probe.note,
            })
    if args.json:
        print(json.dumps(rows, indent=2))
    else:
        width = max(len(r["id"]) for r in rows)
        for r in rows:
            print(f"{r['change']:7} {r['id']:{width}}  {r['verdict']:15} stone={r['stone']!r}  target={r['target']!r}")
        print()
        by_change: dict[str, list[int]] = {}
        for r in rows:
            counts = by_change.setdefault(r["change"], [0, 0])
            counts[0] += r["verdict"] == "match"
            counts[1] += 1
        print("matching target, by change: " + ", ".join(f"{c} {m}/{n}" for c, (m, n) in by_change.items()))
    return 0


# Corpus: example scripts, reference scripts, and help() examples.

VOLATILE_TEXT = [
    (re.compile(r"/tmp/[A-Za-z0-9._-]+"), "<tmp>"),
    (re.compile(r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?"), "<time>"),
]
VOLATILE_KEY = re.compile(r"(^pid$|_ms$|^ms$|seconds$|^elapsed|^duration|_at$|timestamp$)")


def normalize(value: Any, roots: list[Path], key: str = "") -> Any:
    if isinstance(value, dict):
        return {k: normalize(v, roots, k) for k, v in value.items()}
    if isinstance(value, list):
        return [normalize(v, roots, key) for v in value]
    if isinstance(value, (int, float)) and not isinstance(value, bool) and VOLATILE_KEY.search(key):
        return "<n>"
    if isinstance(value, str):
        for root in roots:
            value = value.replace(str(root), "<root>")
        for pattern, repl in VOLATILE_TEXT:
            value = pattern.sub(repl, value)
    return value


def unit_result(proc: subprocess.CompletedProcess[str], roots: list[Path]) -> dict[str, Any]:
    payload = help_check.parse_waymark_payload(proc.stdout) or help_check.parse_waymark_payload(proc.stderr)
    if payload is None:
        return {"ok": False, "value": None, "stdout": "", "error": f"no payload (exit {proc.returncode})"}
    error = payload.get("error") or {}
    result = {
        "ok": bool(payload.get("ok")),
        "value": payload.get("value"),
        "stdout": (payload.get("output") or {}).get("stdout", ""),
        "error": error.get("detail") or error.get("message") or "",
    }
    return normalize(result, roots)


def run_scripts(waymark_bin: Path) -> dict[str, Any]:
    results: dict[str, Any] = {}
    with tempfile.TemporaryDirectory(prefix="stone-compat-corpus-") as temp:
        work = Path(temp)
        shutil.copytree(ROOT / "examples", work / "examples")
        for path in sorted((work / "examples").glob("*/*.stone")):
            rel = path.relative_to(work).as_posix()
            try:
                proc = subprocess.run(
                    [str(waymark_bin), "eval", str(path)],
                    cwd=work, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60,
                )
                results[rel] = unit_result(proc, [work])
            except subprocess.TimeoutExpired:
                results[rel] = {"ok": False, "value": None, "stdout": "", "error": "timeout"}
    return results


def run_help_examples(waymark_bin: Path) -> dict[str, Any]:
    results: dict[str, Any] = {}
    with tempfile.TemporaryDirectory(prefix="stone-compat-help-") as temp:
        root = Path(temp)
        help_check.prepare_fixture(root)
        _, entries = help_check.check_help_coverage(waymark_bin, root)
    for entry in entries:
        use_when = entry.get("use_when")
        if isinstance(use_when, str) and "Gateway mode" in use_when:
            continue
        for index, example in enumerate(entry.get("examples", [])):
            if not isinstance(example, str) or not example.strip():
                continue
            with tempfile.TemporaryDirectory(prefix="stone-compat-help-") as temp:
                root = Path(temp)
                app = help_check.prepare_fixture(root)
                source = help_check.example_prelude(app, example)
                source += help_check.rewrite_logical_paths(example, app) + "\n"
                proc = subprocess.run(
                    [str(waymark_bin), "eval", "-c", source],
                    cwd=root, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
                )
                results[f"help({entry['name']})[{index}]"] = unit_result(proc, [app, root])
    return results


def run_corpus(waymark_bin: Path) -> dict[str, Any]:
    return {**run_scripts(waymark_bin), **run_help_examples(waymark_bin)}


def cmd_snapshot(args: argparse.Namespace) -> int:
    first = run_corpus(args.waymark_bin)
    second = run_corpus(args.waymark_bin)
    unstable = sorted(k for k in first if first[k] != second.get(k))
    snapshot = {"units": first, "unstable": unstable}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(snapshot, indent=1, sort_keys=True) + "\n")
    ok = sum(1 for r in first.values() if r["ok"])
    print(f"snapshot: {len(first)} units ({ok} ok, {len(first) - ok} error), {len(unstable)} unstable -> {args.out}")
    for name in unstable:
        print(f"  unstable: {name}")
    return 0


def cmd_diff(args: argparse.Namespace) -> int:
    baseline = json.loads(args.baseline.read_text())
    unstable = set(baseline["unstable"])
    current = run_corpus(args.waymark_bin)
    changed = []
    for name in sorted(set(baseline["units"]) | set(current)):
        if name in unstable:
            continue
        before, after = baseline["units"].get(name), current.get(name)
        if before != after:
            changed.append((name, before, after))
    for name, before, after in changed:
        print(f"changed: {name}")
        for key in ("ok", "stdout", "value", "error"):
            b = None if before is None else before.get(key)
            a = None if after is None else after.get(key)
            if b != a:
                print(f"  {key}: {json.dumps(b)[:200]} -> {json.dumps(a)[:200]}")
    print(f"{len(changed)} of {len(current)} units changed ({len(unstable)} unstable units skipped)")
    return 1 if changed and args.fail_on_change else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--waymark-bin", type=Path, default=DEFAULT_WAYMARK_BIN)
    sub = parser.add_subparsers(dest="command", required=True)
    probes = sub.add_parser("probes", help="compare Stone with target semantics on small programs")
    probes.add_argument("--json", action="store_true")
    snap = sub.add_parser("snapshot", help="record corpus results")
    snap.add_argument("--out", type=Path, required=True)
    diff = sub.add_parser("diff", help="compare corpus results with a snapshot")
    diff.add_argument("--baseline", type=Path, required=True)
    diff.add_argument("--fail-on-change", action="store_true")
    args = parser.parse_args()
    if not args.waymark_bin.exists():
        print(f"waymark binary not found: {args.waymark_bin}; build it with: cargo build -p waymark", file=sys.stderr)
        return 2
    return {"probes": cmd_probes, "snapshot": cmd_snapshot, "diff": cmd_diff}[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
