"""Check the D specs the way swe_spec.stone checks B/C specs: keys, sizes,
and every quote verbatim (whitespace-normalized) in the issue or its file."""
import json, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPECS = HERE.parent / "q2_d_input" / "specs"
LIMITS = {"rule_evidence": (1, 8), "examples": (1, 6), "named_hints": (0, 8), "related_sites": (0, 10), "open_questions": (0, 6)}
squash = lambda s: " ".join(s.split())
bad = 0
for t in json.loads((HERE / "pilot_tasks.json").read_text()):
    repo = HERE / "repos" / t["image"].replace("/", "_").replace(":", "_")
    path = SPECS / f"{t['id']}.json"
    if not path.exists():
        print(t["id"], "MISSING"); bad += 1; continue
    spec = json.loads(path.read_text())
    problems = [f"missing key {k}" for k in ["symptom", "expected_rule", *LIMITS] if k not in spec]
    for k, (lo, hi) in LIMITS.items():
        if k in spec and not lo <= len(spec[k]) <= hi:
            problems.append(f"{k} has {len(spec[k])} items")
    for e in spec.get("rule_evidence", []):
        q = squash(e["quote"])
        if e["source"] == "issue":
            ok = q in squash(t["problem"])
        elif e["source"].startswith("/testbed/"):
            f = repo / e["source"][len("/testbed/"):]
            ok = f.exists() and q in squash(f.read_text(errors="replace"))
        else:
            ok = False
        if not ok:
            problems.append(f"quote not found in {e['source']}: {e['quote'][:80]}")
    words = sum(len(str(v).split()) for v in spec.values())
    print(t["id"], "ok" if not problems else problems, f"({words} words)")
    bad += bool(problems)
sys.exit(1 if bad else 0)
