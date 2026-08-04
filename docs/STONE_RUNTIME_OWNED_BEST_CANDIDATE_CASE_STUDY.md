# LLM-Authored Best-Candidate Attempts: Case Study

Status: positive narrow demonstration, 2026-08-03.

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
scoring policy were fixed, the worker itself was simple, and only one model
configuration produced the final positive cell. The next evidence should come
from transferring the same interfaces to a different task shape without
task-specific language additions.

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

