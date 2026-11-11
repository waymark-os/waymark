"""Tally experiment 3: latest graded fixer run per (task, arm, rep)."""
import json, re, sys
from collections import defaultdict
from pathlib import Path
HERE = Path(__file__).resolve().parent
rows = {}
for d in sorted((HERE / "runs").glob("*-haiku-swe_wf_*-e3[A-D][0-9]")):
    m = re.search(r"-haiku-(swe_wf_\w+)-(.+)-e3([A-D])([0-9])$", d.name)
    if not m or not (d / "grade.json").exists():
        continue
    try:
        v = json.loads((d / "result.json").read_text()).get("value") or {}
    except (OSError, ValueError):
        continue
    g = json.loads((d / "grade.json").read_text())
    spec_in = (d / "spec_in.json")
    has_spec = spec_in.exists() and spec_in.read_text().strip() not in ("", "null")
    rows[(m.group(2), m.group(3), m.group(4))] = dict(
        dir=d.name, outcome=str(v.get("outcome")), resolved=bool(g.get("resolved")),
        calls=(v.get("usage") or {}).get("calls"), has_spec=has_spec)
tasks = sorted({k[0] for k in rows})
for arm in "ABCD":
    line = []
    for t in tasks:
        cells = ""
        for rep in "123":
            r = rows.get((t, arm, rep))
            if r is None: cells += "?"; continue
            o = r["outcome"]
            c = ("R" if o == "verified" else "u") if r["resolved"] else ("x" if o == "verified" else ("n" if o.startswith("not_verified") else "!"))
            if arm != "A" and not r["has_spec"]: c += "*"
            cells += c
        line.append(f"{t[:10]:>10} {cells:<4}")
    sub = [r for k, r in rows.items() if k[1] == arm]
    res = sum(r["resolved"] for r in sub); claim = sum(r["outcome"] == "verified" for r in sub)
    wrong = sum(r["outcome"] == "verified" and not r["resolved"] for r in sub)
    nospec = sum(arm != "A" and not r["has_spec"] for r in sub)
    calls = sorted(r["calls"] or 0 for r in sub)
    print(f"{arm}: n={len(sub)} resolved={res} claimed={claim} wrong_claims={wrong} no_spec={nospec} median_calls={calls[len(calls)//2] if calls else '-'} | " + " ".join(line))
print("R resolved+claimed, u resolved but not claimed, x verified-but-wrong, n honest not_verified, ! other, * no spec given")
for k, r in sorted(rows.items()):
    if not (r["outcome"] in ("verified",) or r["outcome"].startswith("not_verified")):
        print("odd:", k, r["outcome"], r["dir"])
