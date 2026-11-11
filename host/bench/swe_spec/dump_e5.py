import json, re, sys
from pathlib import Path
task = sys.argv[1]; width = int(sys.argv[2]) if len(sys.argv) > 2 else 160
for d in sorted(Path("runs").glob(f"*-haiku-swe_wf_spec3-*{task}*-e5[EFG][0-9]")):
    g = json.loads((d/"grade.json").read_text())
    v = json.loads((d/"result.json").read_text()).get("value") or {}
    tag = d.name[-2:]
    dd = sorted(Path("runs").glob(f"*-swe_design-*{task}*-e5{tag}"))
    design = (json.loads((dd[-1]/"result.json").read_text() or "{}").get("value") or {}).get("design") if dd else None
    s = json.loads((d/"spec_in.json").read_text() or "null")
    print("=====", d.name[-3:], "resolved", g.get("resolved"), "outcome", v.get("outcome"))
    if design:
        print(" PURPOSE:", design["purpose"][:width])
    if not s: print(" NO SPEC"); continue
    for f in (design or {}).get("fields", []):
        val = s.get(f["name"])
        if f["type"] == "decisions":
            print(f" [{f['name']}:decisions]")
            for dc in val or []:
                print("   D[%s] %s -> %s" % (dc["basis"], dc["question"][:width], dc["choice"][:width]))
        elif f["type"] in ("quotes",):
            print(f" [{f['name']}:quotes] {len(val or [])} quotes")
        else:
            opt = (" options=" + json.dumps(f["options"])[:width]) if f["options"] else ""
            print(f" [{f['name']}:{f['type']}]{opt} = {json.dumps(val)[:width]}")
    for c in s["cases"]:
        print("   C[%s] %s :: expect %s" % (c["kind"], c["name"][:60], c["expect"][:width]))
    print("   changes_existing:", s["changes_existing"], " scope:", s["scope"])
