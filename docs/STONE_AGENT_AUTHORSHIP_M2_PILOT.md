# Stone Agent Authorship M2 Pilot

Date: 2026-07-18

## Question

Can an outer language model write a small, bounded inner agent as ordinary
Stone source from compact help, without using tools or relying on a hidden
agent loop?

## Frozen Treatment

The harness in `host/bench/eval_stone_agent_authorship.py` gives each author
model the same compact Stone description and live `help("model_call")` entry.
It requires source with visible conversation state, a bounded loop, one
`model_call` per iteration, JSON action parsing, explicit `run` dispatch, a
finish branch, and a turn-limit failure. Codex runs read-only with instructions
not to inspect the filesystem or call tools.

Validation has two stages:

1. without Gateway, execution must reach the stable `model_unavailable`
   boundary;
2. with a fixture Gateway returning
   `{"kind":"finish","answer":"ready"}`, the program must return the answer
   `ready` successfully.

## Observation

GPT-5.5 produced a 1,384-byte Stone program with zero Codex tool calls. Its
control structure satisfied every source gate and reached `model_call`, but the
first fixture run failed because Stone inferred an unannotated zero-argument
function as returning `None`. The generated program used the ordinary
Python-shaped expectation that `def bounded_inner_agent():` may return a
record.

Stone was corrected so every omitted return annotation means `Any`; an
explicit `-> None` now denotes a checked procedure. The exact saved GPT-5.5
source, with no repair or regeneration, then passed both validation stages and
returned `{"answer":"ready","turns":1}`.

The requested `gpt-5.6-terran` and `gpt-5.6-lunar` cells were unavailable to
the ChatGPT-backed Codex account. Both failed before inference with HTTP 400
"model is not supported" responses. They are availability failures, not Stone
authorship failures.

Local artifacts:

- `target/runs/stone-agent-authorship-v1-gpt55/aggregate.json`
- `target/runs/stone-agent-authorship-v1-56/aggregate.json`

## Interpretation

This pilot supports one narrow claim: GPT-5.5 can author a visible bounded
Stone model/tool loop from compact interface help, and the first observed
failure exposed a fixable LLM-language compatibility defect rather than a need
for hidden harness control.

It does not yet satisfy the M2 exit criterion. Only one author model was
available, the fixture finished before tool dispatch, and the generated source
has not yet been admitted unchanged through Waymark LibOS. Reusable structured
task views, deterministic Rust/Stone loop parity, repair trials, and real task
execution remain required.
