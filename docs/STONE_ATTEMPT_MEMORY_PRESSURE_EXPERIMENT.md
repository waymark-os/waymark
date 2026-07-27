# Attempt Memory Pressure Experiment

## Question

Can attempt memory survive a long write trajectory without prompt or storage
growth, stale-state influence, or behavioral loss across restart and fork?

## Test

Two attempts perform the same 323 writes:

| Arm | Observation policy |
|---|---|
| `A` | append 252 observations under unique keys |
| `K` | revise a fixed window of 16 observation keys |

Both also revise one requirement from an obsolete opaque target to the current
target, revise `decision.latest` 64 times, and archive a resolved risk. After a
controller restart, each attempt forks a model worker. The worker receives a
160-token projection, selects the current target, and revises the existing
decision key in its branch. The parent then revises the same key with the
joined result.

## First Live Result

On 2026-07-23, two calls to `gpt-5.6-terra` through `codex-chatgpt` produced:

| Measure | Append-only | Keyed window |
|---|---:|---:|
| Hot items | 256 | 20 |
| Memory bytes | 192,584 | 13,636 |
| Projection tokens | 121 | 121 |
| Model input tokens | 226 | 226 |
| Selected target | current | current |

The keyed window reduced hot storage by 92.9%. Both projections included the
current verified requirement, excluded the superseded target and archived
risk, and produced the same model behavior. Restart and fork preserved revision
323; the child hook and parent promotion each revised one stable key without
increasing item count. No transaction remained open.

## Finding

The two bounds solve different problems:

- token-bounded projection already isolates model prompt cost from hot-store
  size;
- keyed supersession and archive are still necessary to bound stored state,
  fork-copy cost, and future write capacity.

Gateway should remain fail-closed at hard limits rather than guessing which
semantic fact to evict. The default Stone memory policy should use stable keys
for requirements, goals, latest decisions, and unresolved outcomes; a small
keyed window for episodic observations; and explicit archive when state is
resolved.

One limitation also became visible: projection used a single relevance sort.
After parent promotion, a 160-token projection selected the latest decision and
active goal but omitted the verified requirement. The follow-up
[required-key experiment](STONE_ATTEMPT_MEMORY_REQUIRED_PROJECTION_EXPERIMENT.md)
confirmed that a call-local required key gives critical state a deterministic
token reservation and fails closed when the guarantee cannot be met.

## Reproduce

```sh
python3 host/bench/eval_stone_attempt_memory_pressure.py
```
