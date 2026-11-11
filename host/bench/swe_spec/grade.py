"""Grade finished pilot runs with the official SWE-bench harness.

Usage: venv/bin/python grade.py RUN_DIR [RUN_DIR ...]
Each run dir needs meta.json and patch.diff (written by run_stone.py --task).
Writes grade.json into each run dir: {"resolved": bool, ...}.
"""
import json, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main():
    runs = [Path(p).resolve() for p in sys.argv[1:]]
    run_id = "q2-" + time.strftime("%Y%m%d-%H%M%S")
    work = HERE / "grading" / run_id
    work.mkdir(parents=True)
    preds = {}
    for run in runs:
        meta = json.loads((run / "meta.json").read_text())
        patch = (run / "patch.diff").read_text() if (run / "patch.diff").exists() else ""
        # One prediction file per run, since a file may hold one entry per task.
        name = run.name
        preds[name] = (run, meta["task"], patch)
        (work / f"{name}.jsonl").write_text(json.dumps({"instance_id": meta["task"], "model_name_or_path": name,
                                                         "model_patch": patch}) + "\n")
    for name, (run, task, patch) in preds.items():
        if not patch.strip():
            (run / "grade.json").write_text(json.dumps({"resolved": False, "reason": "empty patch"}))
            continue
        subprocess.run([sys.executable, "-m", "swebench.harness.run_evaluation",
                        "--dataset_name", "SWE-bench/SWE-bench_Verified", "--predictions_path", str(work / f"{name}.jsonl"),
                        "--instance_ids", task, "--max_workers", "1", "--run_id", f"{run_id}-{name}",
                        "--cache_level", "instance", "--timeout", "1800"],
                       cwd=work, capture_output=True, text=True)
        report = work / f"{name}.{run_id}-{name}.json"
        resolved = False
        if report.exists():
            resolved = task in json.loads(report.read_text()).get("resolved_ids", [])
        (run / "grade.json").write_text(json.dumps({"resolved": resolved, "report": str(report) if report.exists() else None}))
        print(name, "resolved" if resolved else "not resolved", flush=True)


main()
