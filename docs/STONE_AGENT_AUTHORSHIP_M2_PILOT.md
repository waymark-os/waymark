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

The exact saved GPT-5.5 source was subsequently admitted byte-for-byte through
Gateway and executed in Waymark LibOS. It made one attached `model.call` and
the LibOS controller reported `{"answer":"ready","turns":1}`. The checked-in
`examples/scripts/react_agent.stone` baseline also passed in LibOS while
reading its Gateway-backed `task_spec()` and dynamic `task_input()`.

This still does not satisfy the M2 exit criterion. Only one author model was
available and the fixture finished before tool dispatch. Deterministic
Rust/Stone loop parity across tool, failure, and limit cases; repair trials;
a second author model; and real task execution remain required.

Additional local artifacts:

- `../waymark-gateway/target/runs/stone-agent-authorship-m2-gpt55-libos-current/summary.json`
- `../waymark-gateway/target/runs/stone-agent-react-baseline-m2-libos/summary.json`

## First Shared-Fixture Parity Cell

The checked-in Stone baseline was then aligned with the fixed Rust frontend's
`{"actions":[...]}` response envelope, final forms, visible tool-observation
feedback, and 16-round/16-turn accounting. Gateway's fixture backend selects a
response by the number of assistant messages already visible in the request,
so it needs no hidden mutable call counter.

Both frontends received the identical two-response sequence:

1. write the expected text to `/app/hello.txt`;
2. return a final value with that answer and answer path.

Both made two attached model calls and returned the same controller value.
The Stone cell additionally verified the transaction file content before
rollback and recorded the admitted source digest. This establishes one causal
tool-feedback parity case, not full loop parity.

- Stone: `../waymark-gateway/target/runs/stone-react-parity-write-final-verified/summary.json`
- Rust: `../waymark-gateway/target/runs/rust-react-parity-write-final/summary.json`

Still untested under shared fixtures: malformed output, empty actions, tool
failure and recovery, multiple actions in one round, turn exhaustion, and
round exhaustion. The Stone baseline intentionally exposes only the common
`read`, `write`, and `run_linux` subset at this stage; the Rust compatibility
frontend has additional legacy tools.
