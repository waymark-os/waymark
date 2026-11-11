"""Summarize graded pilot runs: one row per run, then each run's phase trail."""
import json, sys
from collections import Counter
from pathlib import Path
HERE = Path(__file__).resolve().parent
tag = sys.argv[1] if len(sys.argv) > 1 else "-p1"  # a glob suffix, e.g. "-r?"
rows = []
for run in sorted((HERE / "runs").glob(f"*{tag}")):
    try:
        v = json.loads((run / "result.json").read_text()).get("value")
        g = json.loads((run / "grade.json").read_text())
    except (OSError, ValueError):
        continue
    if not v:
        continue
    m = json.loads((run / "meta.json").read_text())
    trail = []
    for e in v["log"]:
        k = e["kind"]
        if k in ("check",):
            trail.append(f"[{e['phase']}: " + ("ok" if not (e.get("problems") or e.get("failures")) else f"{len(e.get('problems') or e.get('failures'))} problems") + (f", tests {e['tests_passing']}/{e['tests']}" if "tests" in e else "") + "]")
        elif k == "judgment":
            trail.append(f"[{e['phase']}: {'VERIFIED' if e['verdict'].strip().upper().startswith('VERIFIED') else 'no'}]")
        elif k in ("done", "out_of_steps", "out_of_budget", "invalid", "model_timeout"):
            trail.append(f"{e['phase']}@{e.get('step')}:{k}")
    edits = sum(1 for e in v["log"] if e["kind"] in ("edit", "write"))
    rows.append((m["profile"], m["task"].split("__")[1], m["script"][4:], v["outcome"], g["resolved"], v["usage"]["calls"], edits, len(v["patch"]), " ".join(trail)))
print(f"{'model':6} {'task':14} {'arm':8} {'claimed':14} {'resolved':8} calls edits patch")
for r in sorted(rows):
    print(f"{r[0]:6} {r[1]:14} {r[2]:8} {r[3]:14} {str(r[4]):8} {r[5]:5} {r[6]:5} {r[7]}")
print()
for r in sorted(rows):
    print(f"{r[0]} {r[1]} {r[2]}: {r[8]}")
