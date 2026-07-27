# Required-Key Projection Experiment

## Question

Can a Stone controller guarantee that a critical memory item survives a tight
projection budget without weakening the budget or changing global ranking?

## Interface

```stone
context_project(
    focus="...",
    max_tokens=96,
    required_keys=["requirement.target"],
)
```

Required keys are call-local policy. They are emitted first in request order.
Projection then fills remaining space using the existing deterministic
relevance ranking. The call fails when a required key is missing, duplicated,
or cannot fit within the declared budget.

## Live Result

The canary placed a verified opaque target in `requirement.target`, plus a
higher-ranked active goal and pending decision:

| Arm | Projection | Model result | Projection tokens |
|---|---|---|---:|
| unpinned | `goal.active` | `insufficient_evidence` | 74 |
| required | `requirement.target` | current opaque target | 65 |

On 2026-07-23, both calls used `gpt-5.6-terra` through `codex-chatgpt`. The
unpinned model correctly refused to infer the absent target. The required arm
recovered it without including the pending decoy and without increasing prompt
cost. Both child branches closed cleanly and no transaction remained open.

## Finding

Keep relevance ranking as the default soft selector, and use exact
`required_keys` for a small critical frontier. Missing critical state is a
control error, not a reason to silently send a weaker prompt.

This is preferable to global pin metadata in V1: the guarantee is explicit at
the consequential model call, does not make every projection carry the same
items, and leaves storage policy separate from prompt policy.

## Reproduce

```sh
python3 host/bench/eval_stone_attempt_memory_required_projection.py
```
