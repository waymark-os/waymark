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

GPT-5.4 was then run as the second independently addressable author model with
the same frozen prompt and validators. It produced a distinct 1,631-byte
program with zero tool calls. The source passed preflight and Gateway fixture
execution on its first response, without a runtime change.

Local artifacts:

- `target/runs/stone-agent-authorship-v1-gpt55/aggregate.json`
- `target/runs/stone-agent-authorship-v1-gpt54/aggregate.json`
- `target/runs/stone-agent-authorship-v1-56/aggregate.json`

## Interpretation

This pilot supports a narrow language-usability claim: GPT-5.5 and GPT-5.4 can
each author a distinct visible bounded Stone model/tool loop from compact
interface help. The first observed GPT-5.5 failure exposed a fixable
LLM-language compatibility defect rather than a need for hidden harness
control.

The exact saved GPT-5.5 source was subsequently admitted byte-for-byte through
Gateway and executed in Waymark LibOS. It made one attached `model.call` and
the LibOS controller reported `{"answer":"ready","turns":1}`. The checked-in
`examples/scripts/react_agent.stone` baseline also passed in LibOS while
reading its Gateway-backed `task_spec()` and dynamic `task_input()`.

Both exact admitted sources were later exercised through their `run` branches
in Waymark LibOS. Each made two attached model calls, caused Linux RPC to write
an author-specific transaction artifact, finished on the second response, had
the artifact verified before rollback, and left the canonical generation
unchanged. Their checked-in source bytes and author identities are under
`examples/generated/`.

Additional local artifacts:

- `../waymark-gateway/target/runs/stone-agent-authorship-m2-gpt55-libos-current/summary.json`
- `../waymark-gateway/target/runs/stone-agent-authorship-m2-gpt55-libos-tool/summary.json`
- `../waymark-gateway/target/runs/stone-agent-authorship-m2-gpt54-libos-tool/summary.json`
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

A second shared sequence returned an unknown tool on round one, followed by a
write and final action in the same round-two response. Both frontends recovered,
made two model calls, executed all three action turns, returned the same value,
and produced the same verified transaction artifact. This covers tool-error
feedback and multiple actions in one round:

- Stone: `../waymark-gateway/target/runs/stone-react-parity-recover-multiaction/summary.json`
- Rust: `../waymark-gateway/target/runs/rust-react-parity-recover-multiaction/summary.json`

## Shared Failure And Limit Parity

The remaining four shared cases were frozen in
`host/bench/run_stone_react_parity.py`. Rust and Stone received identical
fixture responses and limits. Both produced the same logical outcome and model
call count in every case:

| Case | Logical error | Model calls per surface |
| --- | --- | ---: |
| malformed JSON | `invalid_model_response` | 1 |
| empty actions | `invalid_model_response` | 1 |
| turn exhaustion | `turn_limit_exceeded` | 1 |
| round exhaustion | `round_limit_exceeded` | 2 |

The stable runtime error category for an intentional `fail(...)` remains
`task_failure`; its program-declared reason is preserved separately as
`error.declared_code`. The runner compares the declared reason. This avoids
turning application-specific stop names into unstable OS error categories.

- `../waymark-gateway/target/runs/stone-react-parity-failures-v4/aggregate.json`

## One-Repair Cohort

`host/bench/eval_stone_agent_repair.py` injected one fault at a time into the
admitted GPT-5.5 source: a missing function colon, an incompatible explicit
return annotation, and an invalid `model_call` option. GPT-5.5 received only
the broken source, the ordinary bounded structured diagnostic, and compact
live help. It was allowed one response and no tools.

All three repairs passed. Each changed the source, retained the visible agent
control gates, reached `model_call`, and completed under the Gateway fixture.
The parse, type-check, and model-effect diagnostics were therefore sufficient
for these bounded repair cases.

- `target/runs/stone-agent-repair-v1-gpt55/aggregate.json`

## Transactional Workspace Task

The reusable checked-in Stone loop then ran a nontrivial workspace
transformation in Waymark LibOS. Starting from duplicate, unsorted lines in
canonical `input.txt`, a `run_linux` action produced the sorted unique value
`apple,banana,pear` in `/app/report.txt`. A second model response returned its
answer path. The runner verified the transaction artifact, verified canonical
input bytes were unchanged, and rolled the attempt back.

This run exposed and caused two runtime fixes before it passed:

1. explicit `None` for optional `run` arguments now means the option was
   omitted, matching the documented Stone signature;
2. omitted Gateway-backed `run` cwd now defaults to the Gateway workspace mount
   (`/app`), not the LibOS guest's unrelated local `/work` directory.

- `../waymark-gateway/target/runs/stone-agent-real-workspace-transform-v3/summary.json`

## M2 Result And Boundary

The M2 exit criterion is satisfied for the mechanism tested here:

- two outer models synthesized distinct admitted Stone agents;
- both exact programs executed model, Linux, and transaction effects in LibOS;
- the checked-in Stone loop matches the fixed Rust loop on the frozen shared
  success, recovery, malformed-output, empty-action, turn-limit, and
  round-limit cases;
- no runtime code change was needed for the GPT-5.4 program, while the original
  GPT-5.5 observation and real workspace probe found general language/runtime
  defects that were fixed and regression-tested.

This does **not** validate the larger attempt-first OS hypothesis. The model
responses here are deterministic fixtures, and the workspace task is
constructed mechanism conformance. The next decisive question is
programmability versus configuration: can an outer agent express behavior in
Stone that the frozen Rust loop cannot express, followed by untouched,
historically unresolved Terminal-Bench tasks.
