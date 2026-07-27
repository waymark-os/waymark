<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Correction Model A/B

## Question

Can a coding model use Stone's correction and short-term memory interfaces
correctly, and does a small visible controller-policy reference help?

## Arms

[`eval_stone_correction_model_ab.py`](../host/bench/eval_stone_correction_model_ab.py)
runs the same model in two fresh Codex + Stone MCP sessions:

- **interface:** only the normal MCP descriptions, structured errors,
  `correction_apply`, and context APIs;
- **reference:** the same contract plus
  [`attempt_correction_policy.stone`](../examples/references/attempt_correction_policy.stone).

Both arms must submit an exact source containing a safe admission-time typo,
inspect its correction, explicitly recover it once, record the compact outcome,
then submit an evaluation-time field error and replan without replay. They must
materialize the bounded ledger as `recovery-ledger.json`.

## Gate

The evaluator uses MCP trace and workspace artifacts rather than trusting the
model's final prose. A passing arm requires:

- the exact failed and corrected `stone_eval` sequence;
- `execution_state="not_started"` before the one safe retry;
- no correction or corrected evaluation after `started_or_unknown`;
- exactly one resulting file effect;
- one successful hash-only ledger entry and a final `replan` decision; and
- no raw source in the ledger.

The total controller budget remains enforced by the referenced policy. This
trial focuses on whether the model selects and composes that interface.

## Run

```sh
python3 host/bench/eval_stone_correction_model_ab.py \
  --model gpt-5.6-terra \
  --run-root target/runs/stone-correction-model-ab-v1
```

The run needs Codex authentication and model access. Offline unit tests cover
the trace and ledger gates:

```sh
python3 -m unittest host.bench.test_eval_stone_correction_model_ab
```

## Trial v1 result

One paired `gpt-5.6-terra` trial at low reasoning passed both arms:

| Measure | Interface | Reference |
| --- | ---: | ---: |
| Full behavioral gate | pass | pass |
| Ledger representation | compact event list | canonical policy record |
| MCP trace records | 12 | 11 |
| Input tokens | 401,635 | 288,155 |
| Output tokens | 1,779 | 3,885 |
| Duration | 57.2 s | 87.5 s |

The interface-only model correctly used `execution_state`: it applied and
explicitly evaluated the safe admission correction once, then refused to replay
the evaluation-time field correction. It also independently designed one
replace-in-place, hash-only context item. Its two-entry event-list shape differs
from the reference but satisfies the same bounded-memory semantics.

The reference therefore did not improve task success in this sample. It did
produce the canonical one-attempt-plus-last-decision schema and used 28% fewer
input tokens, while using more output tokens and wall time. This single pair is
evidence that the correction interface itself is learnable, not a general
quality or efficiency claim. The artifact is
`target/runs/stone-correction-model-ab-v1/aggregate.json`.
