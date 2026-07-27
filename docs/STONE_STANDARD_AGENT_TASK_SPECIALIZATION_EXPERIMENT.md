# Standard Stone Task Specialization Experiment

## Question

Can an outer model specialize the standard control for a real, simple
repository task, while an independent harness—not the authored verifier—owns
the completion decision?

This follows the adapter-interface canary with one task-policy canary. It is
still much smaller than Terminal-Bench.

## Frozen Task

The workspace starts with duplicate lowercase fruit names in
`/app/input.txt`. The inner agent must use the standard read/write tools to
create `/app/output.txt` containing the unique names uppercased, sorted, and
newline-terminated:

```text
APPLE
BANANA
PEAR
```

The outer model receives the standard adapter contract and writes only a
finish-verifier suffix. Its verifier delegates the standard finish checks,
performs one bounded read of `output.txt`, rejects nonmatching bytes, and
annotates a verified result.

`host/bench/eval_standard_agent_task_specialization.py` composes that suffix
with the complete visible library. The harness independently requires:

- the exact output bytes;
- four model decisions;
- three workspace reads (input, read-back, verifier) and one write;
- no Linux execution;
- the expected progress revisions and standard result provenance;
- rollback.

## Result

Terra authored a 695-byte suffix with no author-side tool calls. A real
`gpt-5.6-terra` low-reasoning inner run passed:

| Measure | Result |
| --- | ---: |
| model calls | 4 |
| input tokens | 1,912 |
| output tokens | 149 |
| workspace reads | 3 |
| workspace writes | 1 |
| Linux calls | 0 |
| progress revisions | 9 |
| live progress items | 1 |

The result reported `verified-transform`, `task_verified: true`,
`verified_bytes: 18`, `verified_round: 4`, and the standard `_control`
provenance. The external byte check passed and the attempt rolled back.

A causal negative run used the same admitted source but fixture decisions that
wrote plausible, incomplete bytes. The authored verifier returned the declared
`task_verification_failed` error. The trace contained four decisions, the same
three reads and one write, eight progress revisions, no successful finish
state, and clean rollback.

## Interpretation

This is the first evidence here that model-authored specialization contributes
task behavior rather than result decoration: removing the correct bytes causes
the verifier to reject completion. It also shows the intended authority split:
the Stone verifier gives the inner loop early task feedback, while the harness
still checks workspace bytes and effects independently.

The task and verifier contract were deliberately prescriptive, the expected
bytes were known to the outer author, and only one author/inner model pairing
was sampled. This does not establish autonomous verifier synthesis, broad
repository competence, multi-attempt search, or Terminal-Bench success.

## Artifacts

- `target/runs/stone-standard-task-specialization-v1-terra` — positive-only
  pilot
- `target/runs/stone-standard-task-specialization-v2-terra` — positive plus
  causal negative

Reproduce from the Waymark repository:

```sh
python3 host/bench/eval_standard_agent_task_specialization.py \
  --model gpt-5.6-terra \
  --inner-model gpt-5.6-terra \
  --run-root target/runs/stone-standard-task-specialization-v2-terra \
  --overwrite
```
