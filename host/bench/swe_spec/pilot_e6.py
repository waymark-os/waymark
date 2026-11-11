"""Experiment 6: a test step that defines correct and wrong before any fix,
on 8 fresh SWE-bench Verified tasks (one per repo, pilot_tasks_e6.json).

Haiku fixes in every setup; setups differ in who writes the task spec
(swe_e6.stone):
  N none            no test step
  S single_strong   Sonnet writes the whole plan
  D single_default  Haiku writes the whole plan
  P split           Haiku lists the questions, Sonnet answers, Haiku writes the cases

Each run is graded with the SWE-bench harness. grade_plans_e6.py then grades
each spec on its own against the hidden tests.
Progress goes to pilot_e6.log. Finished (graded) runs are skipped.
Usage: python3 pilot_e6.py [--jobs 4] [--reps 2] [--setups N,S,D,P] [--tasks id,id]
"""
import argparse, json, os, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Lock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from run_stone import repo_from_image  # noqa: E402

SETUPS = {"N": "none", "S": "single_strong", "D": "single_default", "P": "split"}
LOG = "pilot_e6.log"
lock = Lock()


def say(msg):
    with lock:
        with open(HERE / LOG, "a") as f:
            f.write(time.strftime("%H:%M:%S ") + msg + "\n")


def value_of(run_dir):
    try:
        return json.loads((run_dir / "result.json").read_text() or "{}").get("value") or {}
    except (ValueError, OSError):
        return {}


def run_dirs(task, tag):
    label = task.split("__")[-1]
    return sorted((HERE / "runs").glob(f"*-haiku-swe_e6-{label}{tag}"))


def finished(task, tag):
    return any(value_of(d) and (d / "grade.json").exists() for d in run_dirs(task, tag))


def job(task, code, tag):
    t = time.time()
    cmd = [sys.executable, str(HERE / "run_stone.py"), "haiku", str(HERE / "swe_e6.stone"), "--task", task,
           f"--tag={tag}", f"--spec={HERE / 'e6cfg' / (SETUPS[code] + '.json')}"]
    out = subprocess.run(cmd, capture_output=True, text=True)
    line = (out.stderr.strip().splitlines() or ["?"])[-1]
    run_dir = Path(line.split("data ")[-1]) if "data " in line else None
    if not run_dir or not (run_dir / "result.json").exists():
        say(f"FAILED {code} {task} {tag}: {out.stderr[-500:]}")
        return
    value = value_of(run_dir)
    subprocess.run([str(HERE / "venv/bin/python"), str(HERE / "grade.py"), str(run_dir)], capture_output=True, text=True)
    grade = json.loads((run_dir / "grade.json").read_text()) if (run_dir / "grade.json").exists() else {}
    usage = value.get("usage") or {}
    calls = {k: (v or {}).get("calls") for k, v in usage.items()}
    info = value.get("spec_info") or {}
    fix = value.get("fix") or {}
    verdict = value.get("verdict") or {}
    revision = (fix.get("revision") or {}).get("decision")
    say(f"done {code} {task}: verdict={value.get('outcome')} resolved={grade.get('resolved')} "
        f"spec={'yes' if value.get('spec') else 'no'} fell_back={value.get('fell_back')} "
        f"spec_problems={len(info.get('problems') or [])} objections={len(fix.get('objections') or [])} "
        f"revision={revision} regressions={len(verdict.get('regressions') or [])} "
        f"calls={calls} {time.time() - t:.0f}s {run_dir.name}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--reps", type=int, default=2)
    ap.add_argument("--setups", default="N,S,D,P")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--tag", default="-e6")
    a = ap.parse_args()
    tasks = json.loads((HERE / "pilot_tasks_e6.json").read_text())
    if a.tasks:
        tasks = [t for t in tasks if t["id"] in a.tasks.split(",")]
    for t in tasks:
        repo_from_image(t["image"], "/testbed")
    codes = a.setups.split(",")
    # Interleave setups so they run side by side under the same API conditions.
    jobs = [(t["id"], code, f"{a.tag}{code}{rep + 1}") for rep in range(a.reps) for t in tasks for code in codes
            if not finished(t["id"], f"{a.tag}{code}{rep + 1}")]
    say(f"starting setups {codes} reps {a.reps} tasks {len(tasks)}: {len(jobs)} jobs")
    with ThreadPoolExecutor(a.jobs) as pool:
        list(pool.map(lambda j: job(*j), jobs))
    say("all done")


if __name__ == "__main__":
    main()
