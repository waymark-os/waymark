<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Attempt-local Stone Correction

## Question

Can a controller recover a harmless model-authored Stone typo without replaying
uncertain effects or growing short-term memory without bound?

## Policy

The reference
[`attempt_correction_policy.stone`](../examples/references/attempt_correction_policy.stone)
wraps one `stone_eval` action-state pair. It returns `retry_once` only when:

- admission reports `execution_state="not_started"`;
- the correction is `suggest_only` and `explicit_only`;
- exactly one advertised edit has high confidence;
- the source/candidate pair has not been tried;
- the corrected source does not form a known cycle; and
- the four-attempt budget remains.

Every other case returns `replan`. `correction_apply` only produces an
unexecuted preview; the controller remains responsible for the retry.

One replace-in-place `recovery.stone` context item retains at most four entries.
Each entry contains hashes, replacement, decision, and compact outcome. Full
source and errors remain in trace rather than hot attempt memory.

## Deterministic A/B

Run:

```sh
python3 host/bench/eval_stone_correction_recovery.py
```

The baseline stops on a misspelled `context_project`. The policy arm explicitly
applies and evaluates the unique pre-effect edit, records its outcome, and
blocks a second replay. The gate also checks evaluation-time refusal, semantic
repair delegation, cycle detection, total budget enforcement, exactly one hot
memory item, four retained entries, and absence of raw source in the ledger.

This tests the controller contract. The follow-up
[Stone Correction Model A/B](STONE_CORRECTION_MODEL_AB.md) tests whether a
coding model selects the contract through MCP; its first paired trial passed
both the interface-only and policy-reference arms.
