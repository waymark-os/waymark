"""Export a compact record of every SWE-bench task spec run (design doc 11.3,
11.5, 11.6) from the run directories.

Writes data/results.csv (one row per run: experiment, setup, model, task,
outcome, harness verdict, calls, and for experiment 6 the spec's grade
against the hidden tests) and data/specs.jsonl (every spec or spec
type a model wrote, with the run it came from). Smoke and probe runs are
left out. Usage: python3 export_results.py [RUNS_DIR] [OUT_DIR]
"""
import csv, json, re, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUNS = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "runs"
OUT = Path(sys.argv[2]) if len(sys.argv) > 2 else HERE / "data"

# (tag prefix without the repetition number, script) -> (experiment, setup)
SETUPS = {
    ("", "swe_typed"): ("1", "typed, first round"), ("", "swe_untyped"): ("1", "untyped, first round"),
    ("-p", "swe_typed"): ("1", "typed, one run"), ("-p", "swe_untyped"): ("1", "untyped, one run"),
    ("-r", "swe_typed"): ("1", "typed"), ("-r", "swe_untyped"): ("1", "untyped"),
    ("-e2r", "swe_wf_status"): ("2", "statuses only"), ("-e2r", "swe_wf_check"): ("2", "statuses + cross-check"),
    ("-e2q", "swe_wf_status"): ("2", "statuses only"), ("-e2q", "swe_wf_check"): ("2", "statuses + cross-check"),
    ("-e2q", "swe_untyped"): ("2", "untyped"),
    ("-e3A", "swe_wf_status"): ("3", "A no spec"), ("-e3B", "swe_wf_spec"): ("3", "B Haiku spec"),
    ("-e3C", "swe_wf_spec"): ("3", "C Sonnet spec"), ("-e3D", "swe_wf_spec"): ("3", "D Opus spec"),
    ("-e4B", "swe_wf_spec2"): ("4", "B Haiku spec"), ("-e4C", "swe_wf_spec2"): ("4", "C Sonnet spec"),
    ("-e5E", "swe_wf_spec3"): ("5", "E Haiku designs+fills"), ("-e5F", "swe_wf_spec3"): ("5", "F Sonnet designs, Haiku fills"),
    ("-e5G", "swe_wf_spec3"): ("5", "G Sonnet designs+fills"),
}
E6 = {"N": "none", "S": "single_strong", "D": "single_default", "P": "split"}
SPEC_WRITERS = {"swe_spec", "swe_spec2", "swe_design", "swe_fill"}


def load(path):
    try:
        return json.loads(path.read_text() or "null")
    except (OSError, ValueError):
        return None


def calls_of(usage):
    if not isinstance(usage, dict):
        return ""
    if "calls" in usage:
        return usage["calls"]
    return sum((u or {}).get("calls") or 0 for u in usage.values())


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    rows, specs = [], []
    for run_dir in sorted(RUNS.iterdir()):
        meta = load(run_dir / "meta.json")
        if not meta or not meta.get("task"):
            continue
        tag, script, model = meta.get("tag", ""), meta.get("script", ""), meta.get("profile", "")
        if "smoke" in tag or "probe" in tag or "grade" in tag:
            continue
        prefix = re.sub(r"\d+$", "", tag)
        result = load(run_dir / "result.json") or {}
        value = result.get("value") if isinstance(result, dict) else None
        value = value if isinstance(value, dict) else {}
        if script in SPEC_WRITERS:
            spec = value.get("spec") or value.get("design") or value.get("filled")
            specs.append({"run": run_dir.name, "task": meta["task"], "tag": tag, "model": model, "script": script,
                          "spec": spec, "problems": value.get("spec_problems") or value.get("problems"),
                          "calls": calls_of(value.get("usage"))})
            continue
        if prefix.startswith("-e6") and script == "swe_e6":
            experiment, setup = "6", E6.get(prefix[3:], prefix)
            if value.get("spec"):
                specs.append({"run": run_dir.name, "task": meta["task"], "tag": tag, "model": model, "script": script,
                              "setup": setup, "spec": value["spec"], "first_spec_hash": value.get("first_spec_hash"),
                              "grade": {k: v for k, v in (load(run_dir / "spec_grade.json") or {}).items() if k != "rows"}})
        elif (prefix, script) in SETUPS:
            experiment, setup = SETUPS[(prefix, script)]
        else:
            continue
        grade = load(run_dir / "grade.json") or {}
        spec_grade = load(run_dir / "spec_grade.json") or {}
        spec_in = load(run_dir / "spec_in.json")
        rows.append({"experiment": experiment, "setup": setup, "model": model, "task": meta["task"], "tag": tag,
                     "outcome": value.get("outcome", "error" if result.get("ok") is False else ""),
                     "resolved": grade.get("resolved", ""), "calls": calls_of(value.get("usage")),
                     "spec_given": bool(spec_in) if experiment in ("3", "4", "5") else "",
                     "spec_class": spec_grade.get("class", ""),
                     "spec_false_reject": ";".join(spec_grade.get("false_reject") or []),
                     "spec_false_accept": ";".join(spec_grade.get("false_accept") or []),
                     "run": run_dir.name})
    # A run that was interrupted and rerun has the same tag; the tallies in
    # the design doc count the latest graded run per (task, setup, tag).
    groups = {}
    for row in rows:
        groups.setdefault((row["task"], row["tag"], row["model"], row["setup"]), []).append(row)
    for group in groups.values():
        graded = [r for r in group if r["resolved"] != ""]
        counted = (graded or group)[-1]
        for row in group:
            row["counted"] = row is counted
    with open(OUT / "results.csv", "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    with open(OUT / "specs.jsonl", "w") as f:
        for spec in specs:
            f.write(json.dumps(spec) + "\n")
    print(len(rows), "runs,", len(specs), "specs ->", OUT)


if __name__ == "__main__":
    main()
