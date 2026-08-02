# Stone Staged Harness Syntax Experiment

## Question

What code-like Stone surface makes it easiest for an LLM to turn a public task
into an executable staged harness without authoring a DAG or regenerating the
standard agent loop?

## Compared Surfaces

Three low-reasoning `gpt-5.6-terra` trials per arm received the unchanged public
`make-doom-for-mips` instruction and generic attempt concepts only. The prompt
did not provide a Doom stage decomposition. Calls were tool-free and sources
were evaluated unchanged by a structural and public-requirement evidence gate.

The first matched comparison used the existing declaration shape:

```stone
@stage(goal="...", evidence=artifact(...), max_actions=6)
def build(step):
    return agent_loop(step)

report = workflow_run(workflow("project", build, verify))
```

and a proposed block form:

```stone
workflow project:
    stage build(goal="...", evidence=artifact(...), max_actions=6):
        agent_loop()

run project
```

A focused follow-up moved evidence into code-like postconditions:

```stone
workflow project:
    stage build(goal="...", max_actions=6):
        agent_loop()
        ensure artifact(...)

run project
```

## Results

| Surface | Syntax-valid first responses | Mean bytes | Mean lines | Complete evidence |
| --- | ---: | ---: | ---: | ---: |
| `@stage` header evidence | 3/3 | 1,131 | 35.7 | 0/3 |
| block header evidence | 3/3 | 1,272 | 37.0 | 0/3 |
| block body `ensure` | 3/3 | 809 | 25.3 | 0/3 |

All nine sources inferred three or four purposeful stages, placed the build
stage first or second, bounded stage actions, and requested a repairable
checkpoint for the build. The outer block alone was not easier than `@stage`;
both forms were authored correctly in every trial, and the decorator sources
were slightly smaller.

The header-evidence arms omitted executable non-empty-stdout evidence in every
trial. All three `ensure` sources independently wrote both
`ensure command_succeeded(...)` and `ensure stdout_nonempty()`. The contract
form was 28.5% smaller than the decorator form and 36.4% smaller than the block
header form.

No arm passed the full semantic gate because every source placed
`doomgeneric_mips` under `/app/doomgeneric/` rather than at the task workspace
root expected by `vm.js`. The prompt intentionally omitted the generic runtime
path-resolution contract. This is not evidence against the syntax: harness
authorship must receive the same provider-neutral workspace root and relative
path semantics that an executing Stone program receives. One generated
inspection stage also used already-existing input files as its completion
evidence, so it would have skipped without producing a build plan. A dedicated
verified-context evidence form is needed when inspection is meant to produce a
decision artifact.

## Selected Direction

The next prototype should use:

```stone
workflow task:
    stage build(
        goal="produce the requested executable",
        max_actions=8,
        checkpoint="repairable",
    ):
        agent_loop()
        ensure artifact("doomgeneric_mips", format="elf", arch="mips")

    stage execute(goal="run and observe the program", max_actions=4):
        agent_loop()
        ensure command_succeeded(["node", "vm.js"])
        ensure stdout_nonempty()

    stage verify(goal="verify produced outputs", max_actions=2):
        ensure file_valid("/tmp/frame.bmp", format="bmp", nonempty=True)

run task
```

`workflow`, `stage`, and `ensure` are authoring syntax that lower to the existing
typed workflow/stage/evidence IR. `ensure` is a re-checkable stage contract, not
a Python assertion: the runner checks it before execution when meaningful and
after every action or repair. A checkpoint is created only after every contract
is freshly satisfied. `max_actions` bounds model decisions inside the optional
stage-scoped `agent_loop()`; a deterministic stage may omit that loop and use
ordinary Stone effects.

Sequential source remains the default authoring model. The lowered runtime may
derive a graph for validation, scheduling, and observability, but authors do not
need to construct nodes and edges. Ordinary Stone conditionals, loops, attempt
branches, and functions remain available when control is genuinely non-linear.

## Next Gate

Implement only enough frontend lowering and stage-scoped loop support to run a
fixture. Then repeat blind authorship with the generic workspace contract and
execute the frozen Doom harness. Required evidence is:

- source admitted without repair;
- build starts while repair actions remain;
- unsatisfied `ensure` contracts stay visible in every stage decision;
- stage success creates the requested repair frontier;
- later failure resumes from that frontier;
- official requirements and lifecycle cleanup remain evidence-gated.

Artifacts:

- `target/runs/staged-harness-syntax-authorship-v1-terra/aggregate.json`
- `target/runs/staged-harness-contract-authorship-v1-terra/aggregate.json`

## Implemented Rollout Findings

The block surface now lowers to the typed workflow runtime, including visible
stage-scoped `agent_loop()`, rechecked `ensure` contracts, bounded action
budgets, compiler-hygienic private stage symbols, and automatic publication
audits. An attached fixture passes with one model decision per action.

The staged Doom rollout identified four control requirements that syntax alone
does not provide:

- bounded stage memory must retain a ledger, not only the latest outcome;
- process feedback must prefer diagnostic stdout/stderr tails so long build
  logs do not erase the final compiler error;
- exploration and exploitation are explicit action-state modes, with one
  focused diagnostic reopened after a failed gate;
- command completion mechanics (`run_linux` versus `run_complete`) must not be
  mistaken for semantic intent.

These changes moved one frozen harness from passive inspection to a real MIPS
cross-build and, in one cell, through the build frontier into VM execution. A
later cell exposed the next evidence gap: `decision_recorded()` accepted a
generic inspection summary that omitted facts needed by the build stage. The
selected refinement is a typed decision contract:

```stone
stage inspect(goal="inspect build requirements", max_actions=4):
    agent_loop()
    ensure decision_recorded(fields=[
        "source_layout",
        "toolchain",
        "frame_backend",
        "runtime_contract",
    ])
```

The runtime exposes these field names to the visible controller; its decision
schema requires one non-empty string per field, and the evidence gate checks
them independently. This remains structural semantic evidence, not proof that
the findings are true. Later stages and external verification still test the
consequences.

A fresh three-draft, low-reasoning Terra authorship cohort learned this form
from generic guidance: 3/3 sources parsed on the first response and 3/3 passed
the tightened public-requirement gate. The independently chosen field sets
covered build method/system, MIPS target requirements, frame integration, and
VM invocation. Mean source size was 1,009 bytes and 31.7 lines. This is a
learnability canary, not task-success evidence.

Artifact: `target/runs/staged-harness-typed-decisions-v1-terra/aggregate.json`

The first typed-decision Doom cell confirmed the remaining boundary. The
inspection stage supplied every required field, but the non-empty `toolchain`
finding still said that the compiler and target were unknown. The structural
contract passed even though the finding was not actionable. The next evidence
form should therefore distinguish `resolved` from `unknown` and retain a
compact observation basis that later stages can inspect. The same cell reached
a repairable linker conflict only after consuming its ordinary build actions;
stage repair reserve should be represented and accounted for separately rather
than hidden by increasing one undifferentiated action limit.

The first stateful refinement uses
`decision_recorded(resolved=[...])`. Its controller schema requires a bounded
`{state, value, basis}` record for every named finding. `unknown` is admitted as
an honest intermediate state but fails the evidence gate; `resolved` advances
only when value and basis are both present. The runtime exposes the resolved
field names independently from legacy structural `fields=[...]`, and nested
finding records survive compact workflow reports.

A fresh low-reasoning authorship cohort learned this refinement in 3/3 first
responses. Every generated inspection stage used `resolved=[...]` and selected
concrete build, target, and runtime fields. An initial sandboxed cohort never
reached the model because Codex could not initialize writable state; it is
excluded as infrastructure failure. The valid cohort artifact is
`target/runs/staged-harness-resolved-findings-v2-terra/aggregate.json`.

The first task cell then failed safely at inspection rather than handing an
unknown toolchain to the build stage. Its four actions were: shallow root read,
partial finding publication, shallow source-directory read, and a final partial
publication. `toolchain` and `frame_backend` remained explicitly `unknown`, no
Linux effects ran, and cleanup left no open checkpoint or transaction. This
validates the gate and exposes a distinct efficiency problem: publishing useful
partial state currently consumes the same action budget as observation. The
next control experiment should reduce that accounting friction—without
reclassifying unknown as resolved or merely raising the undifferentiated stage
limit.

Artifact:
`../../waymark-gateway/target/runs/staged-stone-doom-v13-resolved-findings-terra/cell/cell.json`

The next slice removed that publication action. The workflow kernel now owns a
bounded accumulated per-stage finding map and exposes it as `step.findings`. A
model may
attach a small `learned={field: value}` delta to the action following a real
tool observation; the runtime validates the field, marks it resolved, and
derives provenance from the prior action. Final decisions consume accumulated
state instead of regenerating every `{state, value, basis}` record. An optional
`unknown={field: reason}` keeps bounded failure reports explicit.

The rollout distinguished representation from control. Nested finding updates
were ignored, while the smaller delta was used spontaneously. But the model
still spent decisions before resolving the declared interface. The standard
controller now exposes only tool actions while resolved fields remain open,
then exposes decision actions after resolution or on the last bounded action.
Its schema hint is generated from the same current state. JSON `oneOf`
diagnostics name allowed discriminators and the nearest branch error rather
than reporting only “matched zero branches.”

In the final Doom cell, all four responses were schema-valid, `source_layout`
was retained with runtime provenance, and the report named `toolchain`,
`frame_backend`, and `runtime_contract` as still missing. The stage still
failed because the remaining probes were weak. This is positive evidence for
runtime-owned incremental state and evidence-gated action availability, not
for task success. The next construct should help select field-directed probes
rather than further changing the finding representation.

Artifact:
`../../waymark-gateway/target/runs/staged-stone-doom-v22-dynamic-schema-hint-terra/cell/cell.json`

The field-directed follow-up lets the harness author name not just finding
fields but their observation kinds and questions:

```stone
ensure decision_recorded(resolved={
    "frame_backend": {
        "kind": "file",
        "question": "Which source implements the required frame writer?",
    },
    "runtime_contract": {
        "kind": "command",
        "question": "How does vm.js invoke the ELF and what output is required?",
    },
})
```

The standard controller requires `for_field` on every inspection tool action
and permits `resolves` only for fields with retained observations. The runtime
owns the bounded probe ledger and provenance. Four declared fields plus one
decision used `max_actions=5`.

The Doom rollout selected all four fields, retained all four observations, and
resolved one. The last decision explicitly reported the other three as
unknown, and the evidence summary named them as missing. This is substantially
better control coverage than the prior rollout, but not success: a failed
compiler lookup, a shallow directory listing, and a prefix-only file read did
not answer their questions. A useful next syntax/runtime experiment is a
per-field acquisition loop with evidence-quality feedback and focused
search/tail probes, rather than another decision-record shape.

Artifact:
`../../waymark-gateway/target/runs/staged-stone-doom-v24-field-directed-probes-terra/cell/cell.json`

The next slice added native recursive `find`, bounded literal/regex `search`,
and offset/tail/line-range reads to the standard controller. Line ranges are
streamed from the requested line instead of slicing an 8 KiB prefix. Repeated
probes retain bounded revisions and provenance, and the schema schedules every
unobserved field before refinement. Empty results are explicitly
`insufficient` rather than resolvable observations.

The first native-acquisition cell exposed the line-range and field-starvation
bugs and still stopped at inspection. With both fixed, the next cell completed
inspection in five actions, completed the build with a valid MIPS ELF, and
failed at the run gate. `node vm.js` exited successfully with stdout but
executed only nine instructions and produced no BMP; a final combined rebuild
and run action then violated the exact-command contract and failed because it
lost the configured compiler. This is evidence that the acquisition syntax
improves control, but filename/tool discovery alone is too weak for a task
whose real requirement is a VM-compatible freestanding ABI.

Artifacts:
`../../waymark-gateway/target/runs/staged-stone-doom-v25-native-acquisition-terra/cell/cell.json`
and
`../../waymark-gateway/target/runs/staged-stone-doom-v26-streamed-lines-coverage-terra/cell/cell.json`

Artifact:
`../../waymark-gateway/target/runs/staged-stone-doom-v12-typed-decisions-terra/cell/cell.json`
