"""Experiment 6: grade every task spec on its own, against the hidden tests.

The hidden tests are the spec of record: a patch is right when the SWE-bench
harness resolves it. For each task, the patches are the reference fix and
every distinct non-empty patch an e6 run produced, each with its hidden-test
verdict. swe_plan_grade.stone applies each patch in the task's container and
reports whether the spec accepts it. Each spec is then:
  agrees      accepts exactly the patches the hidden tests accept
  compatible  disagrees somewhere, but some patch passes both
  contrary    no patch passes both (read these by hand)
plus false rejects (hidden pass, spec rejects) and false accepts (hidden
fail, spec accepts). It also compares the spec's revised tests with the
existing tests whose old lines the real test patch removes or changes
(tests it only adds lines to are reported as extended).

Writes spec_grade.json into each run dir.
Usage: python3 grade_plans_e6.py [--jobs 4] [--include-smoke]
"""
import argparse, json, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
HIDDEN = json.loads((HERE / "hidden_e6.json").read_text())
TASKS = {t["id"]: t for t in json.loads((HERE / "pilot_tasks_e6.json").read_text())}


def value_of(run_dir):
    try:
        return json.loads((run_dir / "result.json").read_text() or "{}").get("value") or {}
    except (ValueError, OSError):
        return {}


def e6_runs(include_smoke):
    for run_dir in sorted((HERE / "runs").glob("*-haiku-swe_e6-*")):
        meta = json.loads((run_dir / "meta.json").read_text())
        if "smoke" in meta.get("tag", "") and not include_smoke:
            continue
        value = value_of(run_dir)
        grade = json.loads((run_dir / "grade.json").read_text()) if (run_dir / "grade.json").exists() else None
        if value and grade is not None:
            yield run_dir, meta["task"], value, grade


def test_name(hidden_id):
    """The bare function name of a hidden test id (pytest, django, sympy)."""
    if " (" in hidden_id:
        return hidden_id.split(" (")[0]
    return hidden_id.split("::")[-1].split("[")[0]


def existing_test_changes(task):
    """Existing tests the real test patch touches, split into revised (an old
    line removed or changed: the old expectation no longer holds) and
    extended (only lines added). New tests are neither."""
    from run_stone import repo_from_image
    repo = repo_from_image(TASKS[task]["image"], "/testbed")
    touched = {}
    current = None
    for line in HIDDEN[task]["test_patch"].splitlines():
        if line.startswith("diff --git"):
            current = None
            continue
        if line.startswith("@@"):
            m = re.search(r"def (\w+)", line.split("@@")[-1])
            current = m.group(1) if m else None
            continue
        if line.startswith(("---", "+++")):
            continue
        m = re.match(r"^[ +-]\s*def (\w+)", line)
        if m:
            current = m.group(1)
            continue
        if current and current.startswith("test") and line[:1] in "+-" and line[1:].strip():
            touched.setdefault(current, set()).add(line[0])
    revised, extended = [], []
    for name, signs in sorted(touched.items()):
        exists = subprocess.run(["git", "-c", "safe.directory=*", "-C", str(repo), "grep", "-q", "-w", "def " + name],
                                capture_output=True).returncode == 0
        if not exists:
            continue
        (revised if "-" in signs else extended).append(name)
    return revised, extended


def stone_safe(obj):
    """Drop None so the JSON can stand in for a Stone literal."""
    if isinstance(obj, dict):
        return {k: stone_safe(v) for k, v in obj.items() if v is not None}
    if isinstance(obj, list):
        return [stone_safe(v) for v in obj if v is not None]
    return obj


def grade_spec(run_dir, task, value, patches):
    spec = value["spec"]
    baseline = (value.get("spec_info") or {}).get("baseline") or []
    if value.get("first_spec_hash") != spec.get("hash"):
        baseline = []  # revised mid-run: its keep baseline is not in spec_info
    guards = [k["command"] for k in baseline if k.get("passed")] or spec["keep"]
    spec_in = {"plan": stone_safe({k: spec[k] for k in ["new_tests", "revised"]}), "guards": guards,
               "patches": [{"name": p["name"], "patch": p["patch"]} for p in patches]}
    spec_path = run_dir / "spec_grade_in.json"
    spec_path.write_text(json.dumps(spec_in))
    cmd = [sys.executable, str(HERE / "run_stone.py"), "haiku", str(HERE / "swe_plan_grade.stone"), "--task", task,
           f"--tag=-e6grade-{run_dir.name[-8:]}", f"--spec={spec_path}"]
    out = subprocess.run(cmd, capture_output=True, text=True)
    line = (out.stderr.strip().splitlines() or ["?"])[-1]
    grade_dir = Path(line.split("data ")[-1]) if "data " in line else None
    results = value_of(grade_dir) if grade_dir else None
    report = {"grade_run": grade_dir.name if grade_dir else None, "results": results}
    if isinstance(results, list):
        hidden = {p["name"]: p["hidden_pass"] for p in patches}
        rows = [{"patch": r["name"], "hidden_pass": hidden.get(r["name"]), "spec_accepts": r.get("accepts"),
                 "failing_spec_tests": [c["name"] for c in r.get("cases", []) if c.get("passed") is not True],
                 "error": r.get("error")} for r in results]
        judged = [r for r in rows if r["spec_accepts"] is not None]
        both = [r for r in judged if r["hidden_pass"] and r["spec_accepts"]]
        false_reject = [r["patch"] for r in judged if r["hidden_pass"] and not r["spec_accepts"]]
        false_accept = [r["patch"] for r in judged if not r["hidden_pass"] and r["spec_accepts"]]
        report.update({"rows": rows, "false_reject": false_reject, "false_accept": false_accept,
                       "class": "agrees" if not false_reject and not false_accept else ("compatible" if both else "contrary")})
    listed = sorted({r["test"].split("::")[-1].split("[")[0] for r in spec["revised"]})
    truth, extended = existing_test_changes(task)
    report.update({"revised_listed": listed, "revised_truth": truth, "extended_truth": extended,
                   "revised_hits": sorted(set(listed) & set(truth))})
    (run_dir / "spec_grade.json").write_text(json.dumps(report, indent=1))
    print(run_dir.name, report.get("class"), "false_reject", len(report.get("false_reject") or []),
          "false_accept", len(report.get("false_accept") or []), "revised", listed, "truth", truth, flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--include-smoke", action="store_true")
    ap.add_argument("--redo", action="store_true")
    a = ap.parse_args()
    runs = list(e6_runs(a.include_smoke))
    patches = {}
    for task in {t for _, t, _, _ in runs}:
        patches[task] = [{"name": "reference_fix", "patch": HIDDEN[task]["patch"], "hidden_pass": True}]
    for run_dir, task, value, grade in runs:
        patch = value.get("patch") or ""
        if patch.strip() and all(p["patch"] != patch for p in patches[task]):
            patches[task].append({"name": run_dir.name, "patch": patch, "hidden_pass": bool(grade.get("resolved"))})
    todo = [(d, t, v, patches[t]) for d, t, v, g in runs
            if v.get("spec") and (a.redo or not (d / "spec_grade.json").exists())]
    with ThreadPoolExecutor(a.jobs) as pool:
        list(pool.map(lambda j: grade_spec(*j), todo))


if __name__ == "__main__":
    main()
