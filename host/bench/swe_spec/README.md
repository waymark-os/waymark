# SWE-bench Task Spec Experiments

The programs, harness, and results of the SWE-bench experiments recorded in
`docs/STONE_LANGUAGE_CONTROL_DESIGN.md`:

- 11.3: experiment 1, typed versus untyped workflow;
- 11.5: experiments 2-5, from fact statuses to task specs;
- 11.6: experiment 6, a task spec with tests as gates (set up, not yet run).

The design doc holds the results and the reasons; this directory holds what
produced them. It is an archive of the files as they ran, from one flat
working directory on the host. Paths in `run_stone.py` are machine-specific.

## What is here

| Experiment | Stone programs | Harness |
|---|---|---|
| shared | `swe_loop.stone` (the action loop), `swe_cases.stone` (runnable cases) | `run_stone.py` (one run through a throwaway Gateway), `grade.py` (official SWE-bench harness) |
| 1 | `swe_typed.stone`, `swe_untyped.stone`, `probe*.stone` (Luna first-move probe) | `pilot.py`, `analyze.py`, `probe_tools.py` |
| 2 | `swe_wf.stone`, `swe_wf_status.stone`, `swe_wf_check.stone` | `analyze_e2.py` |
| 3 | `swe_spec.stone` (spec writer), `swe_wf_spec.stone` (fixer with a spec) | `pilot_e3.py`, `analyze_e3.py`, `check_d_specs.py` |
| 4 | `swe_spec2.stone`, `swe_wf2.stone`, `swe_wf_spec2.stone` | `pilot_e4.py`, `analyze_e4.py`, `dump_e4.py` |
| 5 | `swe_design.stone`, `spec_design.stone`, `spec_blocks.stone` (type design), `swe_fill.stone` (fill), `swe_wf3.stone`, `swe_wf_spec3.stone` | `pilot_e5.py`, `analyze_e5.py`, `designs_e5.py`, `dump_e5.py` |
| 6 | `swe_plan.stone` (template, gate 1, writers, revision), `swe_wf4.stone` (fixer, gate 2, route back), `swe_tree.stone`, `swe_verify.stone` (final verifier), `swe_e6.stone` (driver), `swe_plan_grade.stone` (grades a spec on patches) | `pilot_e6.py`, `grade_plans_e6.py`, `e6cfg/` |

Tasks: `pilot_tasks.json` (experiments 1-5) and `pilot_tasks_e6.json`
(experiment 6). `hidden_e3.json` and `hidden_e6.json` hold the public
SWE-bench Verified fields the grading scripts read (reference patch, test
patch, fail-to-pass and pass-to-pass tests); the programs never see them.

`data/`:

- `results.csv`: one row per run (experiment, setup, model, task, outcome,
  harness verdict, model calls). `counted` marks the run each tally uses: a
  run that was interrupted and rerun shares its tag, and the latest graded
  one counts.
- `specs.jsonl`: every spec, spec type, or filled spec a model wrote, with
  its run.
- `e3_opus_specs/`: experiment 3's D specs and the issue text they were
  written from.
- `lab_notes.md`: working notes from experiments 1-2 and the correction
  (entries 1-20).
- `logs/`, `pilot_summary.txt`, `reps_summary.txt`: run logs and
  experiment 1's tables.

Run directories (about 2.4 GB, with task repository copies and Gateway data)
are not kept; `export_results.py RUNS_DIR OUT_DIR` rebuilds `data/` from them.

## Running

Everything runs from one directory that also holds `runs/`, `repos/`, and a
`venv/` with the `swebench` package (`.gitignore` covers them). Needs Docker
with the SWE-bench task images, a built `waymark-gateway` (the
`anthropic-provider` branch for Haiku and Sonnet), and API keys in
`~/waymark.env`; keys go only to the Gateway's environment.

```sh
python3 run_stone.py haiku swe_e6.stone --task django__django-16485 --spec e6cfg/split.json
python3 pilot_e6.py --jobs 4 --reps 2
python3 grade_plans_e6.py
```

`Q2_WAYMARK` selects the `waymark` binary.
