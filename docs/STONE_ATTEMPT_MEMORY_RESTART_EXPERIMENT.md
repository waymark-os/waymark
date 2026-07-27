<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Attempt-Memory Restart Experiment

## Question

Does bounded semantic attempt memory help a Stone agent recover consequential
state after its controller process is replaced, without paying the prompt cost
of replaying a raw transcript?

## Frozen Design

One Stone program runs twice in each of three independent attempts:

| Cell | State preserved across restart |
| --- | --- |
| N | no early requirement |
| T | raw 49-message transcript in the transactional workspace |
| M | one semantic requirement in Gateway attempt memory |

The first controller observes an opaque required action through `run.post`,
persists state according to its cell, writes a restart marker, and exits. The
Gateway then starts a fresh controller in the same attempt.

The second controller makes two model decisions. Between them, a tool emits a
frozen, misleading exit-7 failure. Call-local hooks project memory before each
decision and record decisions/tool outcomes afterward. The final decision must
use the opaque action when evidence survived, or return `insufficient_evidence`
when it did not. The opaque value is absent from the Stone source.

## Result

The real-model canary passed on 2026-07-22 with `gpt-5.6-terra`:

| Cell | Task success | Controller runs | Input tokens | Hot memory revision |
| --- | ---: | ---: | ---: | ---: |
| N | no | 2 | 1,014 | 0 |
| T | yes | 2 | 2,990 | 0 |
| M | yes | 2 | 1,276 | 5 |

All cells made two model calls and two tool calls, observed the exit-7 outcome,
and made no redundant probe retry. M retained exactly three current keys:
requirement, latest decision, and latest tool outcome. T and M both recovered
the required action, while N refused to invent it.

Compared with raw transcript replay, bounded memory saved 1,714 input tokens,
or 57.3%, in this canary. This supports the V0 architecture: semantic current
state belongs in attempt memory, raw history belongs in traces or workspace
artifacts, and projection should remain explicit at an individual model call.

## Limits

This is a causal canary, not a general benchmark: one model, one trial per
cell, a synthetic opaque requirement, and a program-authored retention policy.
It establishes restart continuity and prompt-density benefit. It does not yet
show that an LLM can author the policy reliably, choose what to retain on open
ended tasks, or resolve stale/conflicting memories.

The follow-up [authorship experiment](STONE_ATTEMPT_MEMORY_AUTHORSHIP_EXPERIMENT.md)
confirmed that the model could use the individual memory/hook operations, but
did not pass the complete two-process lifecycle contract in one unedited
response.

## Reproduce

```sh
python3 host/bench/eval_stone_attempt_memory_restart.py
```

The harness freezes source and binary hashes, records the full structured
responses under `target/runs/stone-attempt-memory-restart-v1`, rolls every
attempt back, and fails if any transaction remains open.
