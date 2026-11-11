"""Run the Q2 pilot: every task x arm x model, a few at a time, grading each
run as soon as it finishes. Progress goes to pilot.log; a summary table to
pilot_summary.txt at the end.

Usage: python3 pilot.py [--jobs 4] [--models haiku,luna] [--tag -p1]
"""
import argparse, json, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Lock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from run_stone import repo_from_image  # noqa: E402

lock = Lock()


def say(msg):
    with lock:
        with open(HERE / "pilot.log", "a") as f:
            f.write(time.strftime("%H:%M:%S ") + msg + "\n")


def finished(model, task, arm, tag):
    """A run counts once its program produced a value and it was graded."""
    label = task.split("__")[-1]
    for run in (HERE / "runs").glob(f"*-{model}-swe_{arm}-{label}{tag}"):
        try:
            if json.loads((run / "result.json").read_text()).get("value") and (run / "grade.json").exists():
                return True
        except (OSError, ValueError):
            pass
    return False


def one(job, tag):
    model, task, arm = job
    t = time.time()
    out = subprocess.run([sys.executable, str(HERE / "run_stone.py"), model, str(HERE / f"swe_{arm}.stone"),
                          "--task", task, f"--tag={tag}"], capture_output=True, text=True)
    line = (out.stderr.strip().splitlines() or ["?"])[-1]
    run_dir = line.split("data ")[-1] if "data " in line else None
    if not run_dir or not Path(run_dir, "result.json").exists():
        say(f"FAILED {model} {arm} {task}: {out.stderr[-500:]}")
        return
    value = json.loads(Path(run_dir, "result.json").read_text() or "{}").get("value") or {}
    subprocess.run([str(HERE / "venv/bin/python"), str(HERE / "grade.py"), run_dir], capture_output=True, text=True)
    grade = json.loads(Path(run_dir, "grade.json").read_text()) if Path(run_dir, "grade.json").exists() else {}
    say(f"done {model} {arm} {task}: claimed={value.get('outcome')} resolved={grade.get('resolved')} "
        f"calls={(value.get('usage') or {}).get('calls')} {time.time() - t:.0f}s {Path(run_dir).name}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--models", default="haiku,luna")
    ap.add_argument("--tag", default="-p1")
    ap.add_argument("--arms", default="typed,untyped", help="programs swe_<arm>.stone")
    ap.add_argument("--reps", type=int, default=0, help="run each job N times, tagged TAG1..TAGN")
    a = ap.parse_args()
    tasks = json.loads((HERE / "pilot_tasks.json").read_text())
    for t in tasks:  # copy repos up front, one at a time, so runs never race on a copy
        repo_from_image(t["image"], "/testbed")
    say(f"repos ready; starting {a.models} arms {a.arms} tag {a.tag} reps {a.reps}")
    # Interleave so both arms of a task/model run side by side under the same API conditions.
    tags = [a.tag + str(i + 1) for i in range(a.reps)] if a.reps else [a.tag]
    jobs = [(m, t["id"], arm, tag) for tag in tags for t in tasks for m in a.models.split(",") for arm in a.arms.split(",")
            if not finished(m, t["id"], arm, tag)]
    say(f"{len(jobs)} jobs to run")
    with ThreadPoolExecutor(a.jobs) as pool:
        list(pool.map(lambda j: one(j[:3], j[3]), jobs))
    say("all done")


if __name__ == "__main__":
    main()
