import json, sys
from pathlib import Path
task = sys.argv[1]; w = int(sys.argv[2]) if len(sys.argv) > 2 else 200
latest = {}
for d in sorted(Path("runs").glob(f"*-swe_design-*{task}*-e5[EFG][0-9]")):
    latest[d.name[-3:]] = d
for tag, d in sorted(latest.items()):
    v = json.loads((d/"result.json").read_text() or "{}").get("value") or {}
    des = v.get("design")
    print("=====", tag, d.name.split("-")[2])
    if not des: print(" no design"); continue
    for f in des["fields"]:
        opt = " OPTIONS " + json.dumps(f["options"]) if f["options"] else ""
        print(f" - {f['name']} ({f['type']}){opt[:w]}: {f['description'][:w]}")
