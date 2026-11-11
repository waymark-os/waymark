"""Experiment 5: a model designs the spec's type, a model fills it in, and the
Haiku fixer (swe_wf3, same workflow as experiment 4) carries it out.
  E  Haiku designs, Haiku fills
  F  Sonnet 5.5 designs, Haiku fills
  G  Sonnet 5.5 designs, Sonnet 5.5 fills
Each stage is its own program with a fresh conversation: the filler sees the
issue and the designed type, not the designer's exploration. The design is
turned into Stone source (designed_spec.stone) in the design run's lib/
directory, which the fill run imports. A stage that dies on a model transport
error (e.g. provider HTTP 500) is retried once.
Usage: python3 pilot_e5.py [--jobs 4] [--reps 3] [--arms E,F,G] [--tag -e5] [--tasks id,...]
"""
import argparse, json, os, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Lock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from run_stone import repo_from_image, WAYMARK  # noqa: E402

ARMS = {"E": ("haiku", "haiku"), "F": ("sonnet", "haiku"), "G": ("sonnet", "sonnet")}
lock = Lock()
LOG = "pilot_e5.log"
SKIP = {"sphinx-doc__sphinx-10673"}


def say(msg):
    with lock:
        with open(HERE / LOG, "a") as f:
            f.write(time.strftime("%H:%M:%S ") + msg + "\n")


def value_of(run_dir):
    try:
        return json.loads((run_dir / "result.json").read_text() or "{}").get("value") or {}
    except (ValueError, OSError):
        return {}


def transport_failed(run_dir):
    try:
        text = (run_dir / "result.json").read_text() + (run_dir / "stderr.txt").read_text()
    except OSError:
        return True
    return "model_transport_failed" in text and not value_of(run_dir)


def run(profile, script, task, tag, *extra):
    """One run_stone.py run, retried once on a model transport failure; returns its run dir or None."""
    for attempt in (1, 2):
        cmd = [sys.executable, str(HERE / "run_stone.py"), profile, str(HERE / script), "--task", task, f"--tag={tag}", *extra]
        out = subprocess.run(cmd, capture_output=True, text=True)
        line = (out.stderr.strip().splitlines() or ["?"])[-1]
        run_dir = Path(line.split("data ")[-1]) if "data " in line else None
        if not run_dir or not (run_dir / "result.json").exists():
            say(f"FAILED {profile} {script} {task} {tag}: {out.stderr[-500:]}")
            return None
        if attempt == 1 and transport_failed(run_dir):
            say(f"RETRY {profile} {script} {task} {tag}: model transport failure in {run_dir.name}")
            os.makedirs(HERE / "runs_invalid", exist_ok=True)
            run_dir.rename(HERE / "runs_invalid" / run_dir.name)
            continue
        return run_dir


def finished(task, tag):
    label = task.split("__")[-1]
    return any(value_of(d) and (d / "grade.json").exists() for d in (HERE / "runs").glob(f"*-haiku-swe_wf_*-{label}{tag}"))


def type_loads(lib):
    """Load the generated type outside any sandbox, to catch generator bugs."""
    probe = lib / "probe.stone"
    probe.write_text("from designed_spec import TaskSpec\nemit(\"ok\")\n")
    env = {"PATH": os.environ["PATH"], "HOME": os.environ["HOME"], "WAYMARK_STONE_LIB_DIRS": os.pathsep.join([str(lib), str(HERE)])}
    out = subprocess.run([str(WAYMARK), "eval", str(probe)], capture_output=True, text=True, env=env, cwd=lib)
    probe.unlink()
    return '"ok":true' in out.stdout.replace(" ", ""), (out.stdout + out.stderr)[-800:]


def job(task, arm, tag):
    t = time.time()
    designer, filler = ARMS[arm]
    note, spec_path = "", None
    design_run = run(designer, "swe_design.stone", task, tag)
    dv = value_of(design_run) if design_run else {}
    note += f" design_calls={(dv.get('usage') or {}).get('calls')} design_rounds={dv.get('check_rounds')} fields={len((dv.get('design') or {}).get('fields') or [])}"
    if dv.get("design"):
        lib = design_run / "lib"
        lib.mkdir(exist_ok=True)
        (lib / "designed_spec.stone").write_text(dv["type_source"])
        (design_run / "design.json").write_text(json.dumps(dv["design"], indent=1))
        ok, why = type_loads(lib)
        if not ok:
            say(f"TYPEFAIL {arm} {task} {tag}: {why}")
        else:
            fill_run = run(filler, "swe_fill.stone", task, tag, f"--design={design_run / 'design.json'}", f"--lib={lib}")
            fv = value_of(fill_run) if fill_run else {}
            note += (f" fill_calls={(fv.get('usage') or {}).get('calls')} spec_problems={len(fv.get('spec_problems') or [])}"
                     f" check_rounds={fv.get('check_rounds')}")
            if fv.get("spec") is not None:
                spec_path = fill_run / "spec.json"
                spec_path.write_text(json.dumps(fv["spec"], indent=1))
    else:
        say(f"NODESIGN {arm} {task} {tag}: problems={dv.get('design_problems')}")
    if spec_path is None:
        say(f"NOSPEC {arm} {task} {tag}: fixer runs without a spec")
        note += " spec=missing"
    run_dir = run("haiku", "swe_wf_spec3.stone", task, tag, *([f"--spec={spec_path}"] if spec_path else []))
    if run_dir is None:
        return
    value = value_of(run_dir)
    subprocess.run([str(HERE / "venv/bin/python"), str(HERE / "grade.py"), str(run_dir)], capture_output=True, text=True)
    grade = json.loads((run_dir / "grade.json").read_text()) if (run_dir / "grade.json").exists() else {}
    say(f"done {arm} {task}: claimed={value.get('outcome')} resolved={grade.get('resolved')} "
        f"calls={(value.get('usage') or {}).get('calls')} guesses={value.get('spec_guesses')} scope_extra={len(value.get('scope_extra') or [])}{note} {time.time() - t:.0f}s {run_dir.name}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--arms", default="E,F,G")
    ap.add_argument("--tag", default="-e5")
    ap.add_argument("--tasks", default="")
    a = ap.parse_args()
    tasks = [t for t in json.loads((HERE / "pilot_tasks.json").read_text()) if t["id"] not in SKIP]
    if a.tasks:
        tasks = [t for t in tasks if t["id"] in a.tasks.split(",")]
    for t in tasks:
        repo_from_image(t["image"], "/testbed")
    arms = a.arms.split(",")
    jobs = [(t["id"], arm, f"{a.tag}{arm}{rep + 1}") for rep in range(a.reps) for t in tasks for arm in arms
            if not finished(t["id"], f"{a.tag}{arm}{rep + 1}")]
    say(f"starting arms {arms} reps {a.reps} tag {a.tag}: {len(jobs)} jobs")
    with ThreadPoolExecutor(a.jobs) as pool:
        list(pool.map(lambda j: job(*j), jobs))
    say("all done")


if __name__ == "__main__":
    main()
