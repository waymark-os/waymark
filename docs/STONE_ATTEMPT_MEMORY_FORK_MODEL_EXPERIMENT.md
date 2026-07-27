# Attempt Memory Fork Model Experiment

## Question

Does attempt-local memory give forked model workers useful inherited state while
keeping ordinary lifecycle-parented workers and the parent ledger isolated?

## Test

One parent stores an opaque target as `requirement.target`. Two workers each
receive only one candidate:

- `F`: workers are created with `attempt_fork`;
- `S`: workers use `attempt_spawn` with the same lifecycle parent.

Each worker projects its own attempt memory in a model-call pre-hook and reports
a structured `select`, `reject`, or `insufficient` decision. The parent joins
both results. In `F` it accepts the selected workspace branch and explicitly
promotes one `candidate.accepted` item; accept itself never merges memory.

## First Live Result

On 2026-07-23, four calls to `gpt-5.6-terra` through `codex-chatgpt` produced:

| Arm | Target worker | Other worker | Child revisions | Parent result |
|---|---|---|---|---|
| fork | `select` | `reject` | 2, 2 | selected child accepted; one fact promoted |
| spawn | `insufficient` | `insufficient` | 1, 1 | neither accepted nor promoted |

The fork workers projected `requirement.target`; the spawn workers projected no
items. The fork parent's final keys were only `requirement.target` and
`candidate.accepted`, proving child `decision.child` entries did not leak back.
The run used 616 input tokens across four calls and left no open transactions.

## Finding

Keep the current semantics:

- fork copies a bounded point-in-time memory frontier;
- spawn does not inherit memory from lifecycle parentage;
- child decisions cross the boundary as bounded structured reports;
- workspace accept is separate from explicit semantic promotion.

This is enough for a parent Stone policy to compare model workers without raw
log scraping or automatic memory merge.

Run:

```sh
python3 host/bench/eval_stone_attempt_memory_fork_model.py
```
