"""Experiment 4: spec v2 (rule, decided questions, runnable cases checked
against the code), written by Haiku (B) or Sonnet 5.5 (C); the Haiku fixer
runs swe_wf2, where the spec's cases are acceptance checks and the spec's
changes_existing tests are not counted as regressions. Sphinx is left out
(every arm resolved it in experiment 3).

Experiment 3: does a typed task spec help, and does it matter who writes it?

Every arm runs the same Haiku fixer (swe_wf_status's workflow, 60 calls);
only the spec it is given differs:
  A  no spec
  B  spec written by Haiku (swe_spec.stone, its own run, one per repetition)
  C  spec written by Sonnet 5.5 (same program)
  D  spec written once per task by a separate Opus 5.5 agent (q2_d_input/specs)

Progress goes to pilot_e3.log. Finished (graded) fixer runs are skipped.
Usage: python3 pilot_e3.py [--jobs 4] [--reps 3] [--arms A,B,C,D] [--tag -e3]
"""
import argparse, json, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Lock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from run_stone import repo_from_image  # noqa: E402

D_SPECS = HERE.parent / "q2_d_input" / "specs"
SPEC_WRITER = {"B": "haiku", "C": "sonnet"}
lock = Lock()
LOG = "pilot_e4.log"
SKIP = {"sphinx-doc__sphinx-10673"}


def say(msg):
    with lock:
        with open(HERE / LOG, "a") as f:
            f.write(time.strftime("%H:%M:%S ") + msg + "\n")


def run(profile, script, task, tag, spec=None):
    """One run_stone.py run; returns its run dir or None."""
    cmd = [sys.executable, str(HERE / "run_stone.py"), profile, str(HERE / script), "--task", task, f"--tag={tag}"]
    if spec:
        cmd.append(f"--spec={spec}")
    out = subprocess.run(cmd, capture_output=True, text=True)
    line = (out.stderr.strip().splitlines() or ["?"])[-1]
    run_dir = Path(line.split("data ")[-1]) if "data " in line else None
    if not run_dir or not (run_dir / "result.json").exists():
        say(f"FAILED {profile} {script} {task} {tag}: {out.stderr[-500:]}")
        return None
    return run_dir


def value_of(run_dir):
    try:
        return json.loads((run_dir / "result.json").read_text() or "{}").get("value") or {}
    except (ValueError, OSError):
        return {}


def finished(task, tag):
    label = task.split("__")[-1]
    for run_dir in (HERE / "runs").glob(f"*-haiku-swe_wf_*-{label}{tag}"):
        if value_of(run_dir) and (run_dir / "grade.json").exists():
            return True
    return False


def job(task, arm, tag):
    t = time.time()
    spec_path, spec_note = None, ""
    if arm in SPEC_WRITER:
        spec_run = run(SPEC_WRITER[arm], "swe_spec2.stone", task, tag)
        value = value_of(spec_run) if spec_run else {}
        spec = value.get("spec")
        spec_note = (f" spec_calls={(value.get('usage') or {}).get('calls')}"
                     f" spec_problems={len(value.get('spec_problems') or [])} check_rounds={value.get('check_rounds')}")
        if spec is None:
            say(f"NOSPEC {arm} {task} {tag}: the spec writer produced no spec; fixer runs without one")
            spec_note += " spec=missing"
        else:
            spec_path = spec_run / "spec.json"
            spec_path.write_text(json.dumps(spec, indent=1))
    elif arm == "D":
        spec_path = D_SPECS / f"{task}.json"
    script = "swe_wf_status.stone" if arm == "A" else "swe_wf_spec2.stone"
    run_dir = run("haiku", script, task, tag, spec_path)
    if run_dir is None:
        return
    value = value_of(run_dir)
    subprocess.run([str(HERE / "venv/bin/python"), str(HERE / "grade.py"), str(run_dir)], capture_output=True, text=True)
    grade = json.loads((run_dir / "grade.json").read_text()) if (run_dir / "grade.json").exists() else {}
    say(f"done {arm} {task}: claimed={value.get('outcome')} resolved={grade.get('resolved')} "
        f"calls={(value.get('usage') or {}).get('calls')} guesses={value.get('spec_guesses')} scope_extra={len(value.get('scope_extra') or [])}{spec_note} {time.time() - t:.0f}s {run_dir.name}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--arms", default="B,C")
    ap.add_argument("--tag", default="-e4")
    a = ap.parse_args()
    tasks = [t for t in json.loads((HERE / "pilot_tasks.json").read_text()) if t["id"] not in SKIP]
    for t in tasks:
        repo_from_image(t["image"], "/testbed")
    arms = a.arms.split(",")
    if "D" in arms:
        missing = [t["id"] for t in tasks if not (D_SPECS / f"{t['id']}.json").exists()]
        if missing:
            sys.exit(f"D specs missing for {missing}")
    # Interleave arms so they run side by side under the same API conditions.
    jobs = [(t["id"], arm, f"{a.tag}{arm}{rep + 1}") for rep in range(a.reps) for t in tasks for arm in arms
            if not finished(t["id"], f"{a.tag}{arm}{rep + 1}")]
    say(f"starting arms {arms} reps {a.reps} tag {a.tag}: {len(jobs)} jobs")
    with ThreadPoolExecutor(a.jobs) as pool:
        list(pool.map(lambda j: job(*j), jobs))
    say("all done")


if __name__ == "__main__":
    main()
