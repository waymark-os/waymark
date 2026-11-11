"""Experiment 2 (H-16) summary.

Per arm: resolved, claimed "verified", wrong claims, calls. For wf_check,
verified runs are split by whether the judgment "the reproduction captures
the issue" was cross-checked by an independent reproduction. Earlier typed and
untyped Haiku runs (tag -r1..3) are shown for reference.
"""
import json
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent


def runs(pattern):
    for run in sorted((HERE / "runs").glob(pattern)):
        try:
            v = json.loads((run / "result.json").read_text()).get("value")
            g = json.loads((run / "grade.json").read_text())
        except (OSError, ValueError):
            continue
        if v:
            yield run, json.loads((run / "meta.json").read_text()), v, g


def report(model, patterns):
    print(f"===== {model} =====")
    rows = defaultdict(list)
    for pattern in patterns:
        for run, meta, v, g in runs(pattern):
            rows[meta["script"][4:]].append((meta["task"].split("__")[1], v, g["resolved"], run.name))
    print(f"{'arm':10} runs resolved verified wrong_verified correct_rejected calls")
    for arm in ("wf_status", "wf_check", "typed", "untyped"):
        r = rows.get(arm, [])
        if not r:
            continue
        ver = [x for x in r if x[1]["outcome"] == "verified"]
        print(f"{arm:10} {len(r):4} {sum(x[2] for x in r):8} {len(ver):8} {sum(1 for x in ver if not x[2]):14}"
              f" {sum(1 for x in r if x[2] and x[1]['outcome'] != 'verified'):16}"
              f" {sum(x[1]['usage']['calls'] for x in r) / len(r):5.1f}")
    print()
    print("wf_check: does a cross-checked judgment predict a correct 'verified'?")
    split = defaultdict(lambda: [0, 0])
    for task, v, res, name in rows.get("wf_check", []):
        if v["outcome"] == "verified":
            key = v["repro2"]
            split[key][0] += 1
            split[key][1] += res
    for key, (n, ok) in split.items():
        print(f"  independent reproduction {key}: {n} verified, {ok} resolved")
    print()
    print("Per run (wf arms):")
    for arm in ("wf_status", "wf_check"):
        for task, v, res, name in sorted(rows.get(arm, []), key=lambda x: (x[0], x[3])):
            stages = " ".join(f"{s['name']}:{s['status']}/{s['attempts']}" for s in v["stages"])
            print(f"  {arm:9} {task:16} {v['outcome']:13} resolved={str(res):5} calls={v['usage']['calls']:2}"
                  f" repro2={v.get('repro2', '-'):11} {stages}")
    print()


report("haiku", ("*-haiku-swe_wf_*-e2r?", "*-haiku-swe_typed-*-r?", "*-haiku-swe_untyped-*-r?"))
report("qwen", ("*-qwen-swe_*-e2q?",))
