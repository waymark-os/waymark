# Stone Bounded Exploration Authorship Experiment

Status: narrow capability and usability hypotheses supported on 2026-07-30.

## Question

Can a visible ordinary-Stone control make checkpoint-backed candidate
exploration both available to task programs and easier for an LLM to author
than the equivalent explicit attempt lifecycle?

The tested control shape was:

```text
prepare and checkpoint one frontier
  -> propose the wrong admitted candidate
  -> fork, join, and reject it by evidence
  -> fork the remaining candidate from the same checkpoint
  -> accept it by fresh evidence
  -> close and reclaim the attempt scope
```

## Mechanism

[`bounded_attempt_explore.stone`](../examples/scripts/bounded_attempt_explore.stone)
is visible library source, not a builtin. It provides:

- `candidate(...)`, which constructs a bounded candidate contract; and
- `explore(...)`, which validates candidate identity, optionally calls a
  proposal callback, tries candidates sequentially from one checkpoint,
  distinguishes expected rejection from controller failure, accepts one
  evidence-bearing result, and closes its supervision scope.

Stone user functions gained keyword arguments so this multi-policy interface
does not depend on memorizing positional order. Unknown keywords, duplicate
bindings, and missing arguments name the offending parameter. Optional
task-owned callbacks also support `callback is None` without attempting to
serialize the callback.

The deterministic canary deliberately proposed `deserts` before `desserts` for
the checkpointed problem `stressed`. It rejected the first child, accepted the
second, imported only the winning workspace, kept child memory isolated, and
left both children rolled back with no open transaction.

## Frozen Authorship Comparison

Three matched pairs used `gpt-5.6-terra` at low reasoning effort. Every call
received the same fixed task functions and authored only `main(input)`.

- The explicit arm had to write scope, fork, join, outcome classification,
  discard, accept, result construction, and cleanup.
- The library arm called `explore(...)` with named arguments.

Every authored source was executed unchanged in a fresh Gateway attempt tree.
One diagnostic-guided repair was allowed.

| Metric | Explicit lifecycle | Visible library |
|---|---:|---:|
| First-response passes | 1/3 | 3/3 |
| Eventual passes | 2/3 | 3/3 |
| Repair attempts | 2 | 0 |
| Mean passing source lines | 72.5 | 13 |
| Mean passing source bytes | 2,523 | 390 |

The library reduced authored bytes by 84.5% and passed all three first
responses. One explicit source omitted scope closure and passed after repair.
Another executed the correct two attempts but labeled both outcome records
with controller status `succeeded` instead of evidence status
`rejected`/`accepted`; its repair did not correct that semantic conflation.

The aggregate is
`target/runs/bounded-attempt-explore-authorship-ab-v1-terra-20260730/aggregate.json`.

## Harness And Diagnostic Findings

An excluded pilot found two infrastructure issues before the frozen run:

1. The external trace gate required the library's exact rejection-reason text,
   even though child state, returned evidence, and cleanup already proved the
   explicit source correct. The gate was relaxed and the frozen explicit source
   passed unchanged.
2. `attempt_join(..., scope=scope)` initially tried to materialize the
   task-owned scope and emitted “cannot cross this boundary.” Stone now rejects
   the unsupported keyword before evaluation and explains that
   `attempt_fork(..., scope=scope)` already registered the child.

Attribute assignment diagnostics now also translate
`record.status = "accepted"` to Stone's supported
`record["status"] = "accepted"` form.

These are part of the usability result: an experiment must not trigger program
repair until it distinguishes a candidate/program failure from a faulty
evidence gate, and language errors should state the local repair.

## Finding

The visible library enabled a reusable agent-level capability without moving
search policy into Gateway. It also made the computer materially easier for the
author model to use:

```text
LLM authors task-specific policy and callbacks
Stone library owns repeated control mechanics
Waymark/Gateway own authority, isolation, evidence, and cleanup
```

This supports keeping `explore` as ordinary source while its interface is still
changing. It does not yet justify dedicated syntax: only one cheap task, one
model/configuration, and one fixed two-candidate authorship policy were tested.

## Caffe Transfer

The exact library was subsequently composed into a Caffe specialization
without changing its source. On the official task, one model call again ordered
the wrong lexical-path candidate first. `explore(...)` rejected it in 495 ms,
forked the inode-safe candidate from the same 2.59-GB post-build checkpoint,
accepted independent saved-model evidence, and passed all six official checks
with reward `1.0`. The build stage was `already_satisfied` in both children and
the final tree had no live attempt, checkpoint, or transaction.

This supports capability transfer, not yet Caffe authorship quality. It also
found the next interface boundary: parent-owned checkpoints lower to
`attempt_fork`, while a retained frontier owned by a failed attempt currently
requires repair-checkpoint spawn. A future typed frontier value should hide
that kernel distinction from task policy. See
[Caffe Bounded-Explore Library Experiment](../../waymark-gateway/docs/CAFFE_BOUNDED_EXPLORE_LIBRARY_EXPERIMENT.md).

## Reproduction

```sh
python3 host/bench/smoke_bounded_attempt_explore.py

python3 host/bench/eval_bounded_attempt_explore_authorship_ab.py \
  --model gpt-5.6-terra \
  --reasoning-effort low \
  --trials 3 \
  --max-repairs 1 \
  --run-root target/runs/bounded-attempt-explore-authorship-ab-v1-terra-20260730 \
  --overwrite
```
