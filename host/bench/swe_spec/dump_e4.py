import json, re, sys
from pathlib import Path
task = sys.argv[1]
for d in sorted(Path("runs").glob(f"*-haiku-swe_wf_spec2-*{task}*-e4[BC][0-9]")):
    s = json.loads((d/"spec_in.json").read_text() or "null")
    g = json.loads((d/"grade.json").read_text())
    v = json.loads((d/"result.json").read_text()).get("value") or {}
    print("=====", d.name[-4:], "resolved", g.get("resolved"), "outcome", v.get("outcome"))
    if not s: print("NO SPEC"); continue
    print("RULE:", s["rule"])
    for dc in s["decisions"]:
        print(" D[%s] %s -> %s | rejected %s" % (dc["basis"], dc["question"][:150], dc["choice"][:250], [r[:80] for r in dc["rejected"]]))
    for c in s["cases"]:
        print(" C[%s/%s] %s :: %s :: expect %s" % (c["kind"], c["basis"], c["name"], " ".join(c["command"])[:100], c["expect"][:200]))
    print(" changes_existing:", s["changes_existing"], " scope:", s["scope"])
