# SWE-bench Task Spec Experiments

The programs, harness, and results of the SWE-bench experiments recorded in
`docs/STONE_LANGUAGE_CONTROL_DESIGN.md`:

- 11.3: experiment 1, typed versus untyped workflow;
- 11.5: experiments 2-5, from fact statuses to task specs;
- 11.6: experiment 6, a task spec with tests as gates.

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
| 8 (library) | `swe_suite.stone`: test suites the workflow holds, run on snapshots (see below) | |

## `swe_suite.stone`

A Stone module for the gates of experiments 6-7, built on `run(..., files=)`,
`run_tests()`, `snapshot()`, and `apply_patch()`. The workflow holds a suite;
registering copies every test file the tests use, and every run writes those
copies first, so editing a test file cannot change a gate (a run that finds a
repository test file edited warns, and puts the edit back afterwards).

- `new_suite(root, wrap, runner)`: snapshot the unfixed code.
- `register_tests(suite, tests, keep, revised)`: copy the test files (the new
  tests' files and the repository files keep runs name) and, while the tree
  is the unfixed code, run each new part there: fail-to-pass must fail,
  pass-to-pass must pass, keep runs must finish and pass some tests. Parts
  registered later are listed in `suite["unchecked"]`; unchanged parts are not
  run again. `revise(suite, tests=, keep=, revised=)` re-registers with the
  other parts as they are.
- `check(suite)`: run the suite on the current tree (feedback for the fixer,
  with warnings for edited test files); `rejecting(report)` names the parts
  that reject it.
- `verify(suite, patch)`: prove by snapshot id that the tree is exactly the
  unfixed code plus the patch (listing any extra or missing files), run the
  suite, and return a `Verdict` holding the results.
- `accepts(suite, verdict)`: re-check a verdict without running a test:
  genuine results and snapshots, this suite and patch, every run between the
  two snapshots (by `seq`), the registered commands and test-file copies,
  every test passing, and no keep run newly failing beyond the revised tests.
