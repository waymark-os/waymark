# Standard Stone Executable-Verifier Experiment

## Question

Can an outer model specialize the standard Stone control with a bounded
repository test command, while hidden task authority remains outside both the
authored program and the inner agent?

This removes the known expected bytes from the earlier task-specialization
canary. It is the last constructed-task gate before a small untouched
Terminal-Bench cohort, not evidence of Terminal-Bench competence itself.

## Setup

The outer model receives only the standard adapter contract and the visible
command `python3 /app/test_solution.py`. It authors a named finish verifier
that delegates the standard checks, runs that command once with bounded output
and time, rejects failures, and annotates success.

Only after authorship, the harness creates a small repository task:
implement `normalize_words` from its README. A real inner model may inspect and
edit the transaction and run visible tests. The host then checkpoints the live
transaction and runs separate hidden tests from a read-only mount. That
verifier branch always rolls back and its checkpoint is discarded. The root
attempt also rolls back.

The visible Stone verifier supplies early feedback. The host-owned hidden
verifier is the completion authority.

## Result

`gpt-5.6-terra` authored a 751-byte specialization without tools or access to
the repository or hidden tests. A real low-reasoning Terra inner attempt
completed the task:

| Measure | Result |
| --- | ---: |
| inner model calls | 5 |
| input tokens | 2,976 |
| output tokens | 1,662 |
| workspace writes | 1 |
| inner Linux calls | 2 |
| live progress items | 1 |
| hidden verifier | passed |

The authored visible verifier returned `task_verified: true`, status `0`, its
transition id, the verified round, and standard control provenance. The
independent hidden verifier also returned status `0`.

A causal negative used the same admitted Stone source but wrote
`return list(words)`. The visible verifier rejected finish with
`task_verification_failed`; the hidden verifier independently returned status
`1`. Both checkpoint verifier branches and both root attempts rolled back.

## Interpretation

The interface is sufficient for a useful authority split:

- an outer model can author a small executable verifier adapter without knowing
  the answer;
- an inner model can use the ordinary visible agent loop to solve a repository
  task and receive task-specific finish feedback;
- the Gateway can inspect the resulting transaction with hidden, read-only
  verifier inputs without publishing verifier side effects;
- failure is causal: a plausible wrong implementation is rejected by both
  layers.

This remains one constructed Python task and one model pairing. It does not
validate broad repository work, multi-attempt search, or the attempt-first OS
hypothesis. It does justify moving to a small frozen cohort of untouched,
historically unresolved Terminal-Bench tasks with their official verifiers.

## Artifact And Reproduction

The passing artifact is:

- `target/runs/stone-standard-executable-verifier-v1-terra`

Reproduce from the Waymark repository:

```sh
python3 host/bench/eval_standard_agent_executable_verifier.py \
  --model gpt-5.6-terra \
  --inner-model gpt-5.6-terra \
  --run-root target/runs/stone-standard-executable-verifier-v1-terra \
  --overwrite \
  --warm-build 0
```
