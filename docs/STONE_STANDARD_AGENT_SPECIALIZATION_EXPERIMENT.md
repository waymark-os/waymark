# Standard Stone Agent Specialization Experiment

## Question

Can an outer model use the standard control as a library by writing a small
named adapter, rather than copying or rewriting the agent loop?

This is an interface-learnability canary, not a task-performance experiment.

## Method

`host/bench/eval_standard_agent_specialization.py` gives the author model only
the compact adapter contract. It asks for a verifier that:

- delegates to `standard_verify_finish`;
- annotates the result from task input and loop state;
- is passed directly as a named function to `standard_agent_control`;
- leaves dispatch and progress on their standard adapters.

The harness prepends the complete visible library to the authored suffix. It
then requires parse/admission preflight, executes the composed source in an
attached fixture attempt, checks the annotations and `_control` provenance,
and requires rollback. The author is not allowed tool calls, model effects,
library redefinition, imports, or lambdas.

## Findings

The first Terra sample understood the adapter structure but used Python-style
attribute assignment for record mutation. Stone rejected it before any attempt
or model effect. The compact author guide now states the relevant language
rule: record fields may be read with dot access, but mutation uses item
assignment.

With that rule present, independent Terra and Luna samples both passed:

| Author model | Suffix bytes | Author tool calls | Fixture result |
| --- | ---: | ---: | --- |
| `gpt-5.6-terra` | 536 | 0 | pass |
| `gpt-5.6-luna` | 513 | 0 | pass |

Each composed program made one inner model transition, returned
`specialization-ready`, copied `adapter-canary` from task input, recorded
`verified_round: 1`, preserved standard control provenance, retained one live
progress item through three revisions, and rolled back.

This supports a narrow claim: the standard adapter interface is usable by more
than one outer model when the non-Python mutation rule is visible. It does not
show that a model can invent a good dispatcher, verifier, search policy, or
task skill from a natural-language task.

## Artifacts

- `target/runs/stone-standard-specialization-v1-terra` — rejected attribute
  assignment probe
- `target/runs/stone-standard-specialization-v2-terra`
- `target/runs/stone-standard-specialization-v2-luna`

Reproduce from the Waymark repository:

```sh
python3 host/bench/eval_standard_agent_specialization.py \
  --model gpt-5.6-terra \
  --run-root target/runs/stone-standard-specialization-v2-terra \
  --overwrite
```
