# LLM-Authored Best-Candidate Attempts: Case Study

Status: two positive narrow demonstrations, 2026-08-03.

## Question

Can an existing coding model author a complete Stone attempt program that
explores isolated candidates, retains the best one, imports its workspace, and
reclaims the rest without seeing a reference implementation?

This is a useful Attempt test because the candidates write conflicting
`answer.txt` artifacts. Running them sequentially in one workspace would lose
isolation, while merely comparing returned strings would not demonstrate
workspace selection or cleanup.

## Demonstration

The task presented three candidates in adversarial order:

| Candidate | Artifact | Score |
|---|---|---:|
| baseline | `alpha` | 0.50 |
| winner | `beta` | 0.90 |
| late-worse | `gamma` | 0.70 |

The authored program had to:

1. define a worker in the same Stone source;
2. fork and join one isolated child per candidate;
3. pass each typed outcome to a runtime-owned maximizing selector;
4. accept the retained child into the unchanged parent;
5. close the supervision scope and prove cleanup;
6. verify that the imported artifact contains `beta`; and
7. publish an exact structured result.

The outer `gpt-5.6-terra` call received compact builtin help and the task
contract. It was forbidden from inspecting the repository, calling tools, or
copying the checked-in canary. The generated source was then executed unchanged
in a fresh Gateway attempt tree.

The central authored control flow was ordinary Stone:

```python
scope = attempt_scope(join_timeout_ms=60000)
best = attempt_best(scope, objective="max")

for candidate in candidates:
    child = attempt_fork(
        input=candidate,
        program=current_program(entrypoint="worker"),
        entrypoint="worker",
        start=True,
        scope=scope,
    )
    outcome = attempt_join(child, timeout_ms=60000)
    decision = attempt_best_consider(
        best,
        outcome,
        score=outcome.result.value.score,
        summary="candidate " + outcome.result.value.name,
        evidence=outcome.result.value.evidence,
        artifacts=outcome.result.value.artifacts,
    )

accepted = attempt_best_accept(best)
cleanup = attempt_scope_close(scope)
```

The full deterministic reference is
[`attempt_best_canary.stone`](../examples/references/attempt_best_canary.stone).
The blind-authorship harness does not expose that file to the author model.

## Result

The first fresh response under the final interface passed unchanged:

| Observation | Result |
|---|---:|
| Outer model | `gpt-5.6-terra` |
| Codex tool calls while authoring | 0 |
| Repair rounds | 0 |
| Authored source | 2,738 bytes |
| Candidate forks | 3 |
| Accepted children | 1 |
| Selected answer and score | `beta`, 0.90 |
| Runtime considered / replacements | 3 / 1 |
| Candidate resources reclaimed | 3 / 3 |
| Open transactions after cleanup | 0 |

The selected child was the middle candidate, not the last candidate. The
baseline was replaced, the worse late candidate was rejected, the winning
workspace was imported, the selector released its full retained outcome, and
every child ended rolled back after its resources were reclaimed.

## Transfer: Derived-Cost Minimization

A second blind-authorship task reused the exact Attempt/Stone interfaces but
changed the selection semantics:

- the selector minimized rather than maximized;
- candidate records contained separate setup and run costs, not a score;
- each isolated worker derived its total cost and returned it with the
  candidate artifact; and
- the parent had to select from child-produced measurements while continuing
  to rely on runtime-owned lifecycle counters.

The candidates cost 1.50, 0.90, and 1.20 in declared order, so the winner was
again in the middle and the late candidate tested that the selector did not
degrade after finding the minimum. A fresh `gpt-5.6-terra` response authored
the complete 2,661-byte program. It used `objective="min"`, computed
`input.setup_cost + input.run_cost` inside the worker, and passed unchanged
with zero Codex tool calls and zero repair rounds. The end-to-end cell took
33.18 seconds, reclaimed all three children, and left no open transaction.

No Stone syntax, runtime behavior, or builtin help changed for this transfer.
Only the task contract and benchmark case changed. This is useful evidence
that the positive result is not limited to maximizing a parent-supplied score,
though both tasks still share the same small three-candidate lifecycle shape.

## What The Failed Iterations Taught

The demonstration became positive through general interface corrections, not
by embedding the final program:

| Observation | Interface lesson |
|---|---|
| A generated `except error:` was rejected before execution. | Preserve Python-shaped meaning and return a targeted `exception_binding` correction instead of inventing divergent syntax. |
| A correct execution was rejected because scalar result meanings existed only in the verifier. | Observable requirements must be explicit in the task contract. |
| A model manually counted replacements and treated the initial `None` incumbent as a replacement. | Runtime-owned invariants are useful only when compact help exposes them. |
| Help named `best.considered` and `best.replacements`; the next fresh program used them and passed. | Prefer discoverable OS-owned state over model-written lifecycle bookkeeping. |

This progression is part of the result. Stone supplied familiar local control
flow, Attempt supplied process-like isolation, and Waymark/Gateway owned the
selection and cleanup invariants that should not depend on generated code.

## Claim Boundary

This case supports a narrow capability and usability claim: an existing model
can author this nontrivial Attempt harness in Stone, and runtime-owned selection
state makes the program more robust and easier to write.

It does not establish general task success, transfer to unrelated workflows,
or superiority over other agent-control representations. The candidates and
scoring policies were fixed, both workers were simple, and only one model
configuration produced the positive cells. The next evidence should come from
a real workload where candidate evaluation can fail, take substantial time,
or produce imperfect evidence.

## Reproduction

Run the deterministic mechanism canary:

```sh
python3 host/bench/smoke_attempt_best.py
```

Run a fresh blind-authorship cell (requires Codex authentication and model
availability):

```sh
python3 host/bench/eval_stone_attempt_best_authorship.py \
  --model gpt-5.6-terra \
  --run-root target/runs/stone-attempt-best-authorship-case-study
```

Run the minimizing derived-cost transfer:

```sh
python3 host/bench/eval_stone_attempt_best_authorship.py \
  --case min-derived-cost \
  --model gpt-5.6-terra \
  --run-root target/runs/stone-attempt-best-min-cost-authorship
```
