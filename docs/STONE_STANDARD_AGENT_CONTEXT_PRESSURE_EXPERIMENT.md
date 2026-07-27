# Standard Agent Context-Pressure Experiment

## Question

Can the attempt-oriented controller expose useful recent state to every action
decision without replaying large tool outputs or retaining an ever-growing
message list?

## V3 Boundary

Before each action-model call, V3:

1. projects current attempt memory with a token-bounded `context_project`;
2. bounds the public task and each model-visible observation independently;
3. preserves original output lengths, truncation flags, and head/tail excerpts;
4. drops the oldest action/observation pairs until recent history fits a hard
   character budget;
5. injects the projection ephemerally before the newest instruction;
6. replaces the controller's stored history with the compacted history.

Richer completion evidence remains in the fixed eight-slot memory ring under a
separate bound. This separates model-facing L0 from attempt-memory retention.

## Pressure Cell

The Gateway/LibOS fixture ran fourteen Linux actions. Each returned 32,768
characters containing token-realistic repeated output between distinct head
and tail markers. A fifteenth model call finished.

| Measure | Result |
| --- | ---: |
| Raw tool output | 14 × 32,768 characters |
| Action-context ceiling | 16,384 characters |
| Observed peak | 16,266 characters |
| Old messages dropped | 4 |
| Memory projections | 15 |
| Field / whole-observation truncations | 14 / 14 |
| Retained memory items | 10 |
| Fixture input-token peak | 4,147 |
| Uncompacted peak lower bound | 229,222 |
| Peak reduction factor | 55.3x |
| Fixture input-token total | 35,949 |
| Uncompacted total lower bound | 1,719,165 |
| Total reduction factor | 47.8x |

All eight retained evidence slots recorded the true 32,768-character length,
were marked truncated, remained at 2,048 characters, and preserved both
markers. The attempt rolled back with no open transaction or checkpoint.

The token comparison is a lower bound derived from the synthetic
whitespace-separated payload and the fixture estimator. It is not a production
tokenizer benchmark.

## Real-Model Regression

Terra then passed the existing failed-action recovery trajectory through V3:
five action-model calls, five memory projections, one failed Linux action, one
write, two reads, and clean rollback. Total usage was 5,102 input and 188
output tokens.

The V3 completion-control fixture and three-case real completion-critic gate
also pass unchanged.

## Artifacts

- Pressure aggregate:
  `target/runs/stone-standard-context-pressure-v3/aggregate.json`
- Pressure trace:
  `target/runs/stone-standard-context-pressure-v3/pressure/gateway-data/traces/operations.jsonl`
- V3 completion aggregate:
  `target/runs/stone-standard-completion-critique-v3/aggregate.json`
- V3 real critic aggregate:
  `target/runs/stone-standard-critic-quality-terra-v3/aggregate.json`
- Real recovery aggregate:
  `../waymark-gateway/target/runs/stone-standard-v3-real-recovery/aggregate.json`

Run the pressure cell with:

```sh
python3 host/bench/eval_standard_agent_context_pressure.py --overwrite
```
