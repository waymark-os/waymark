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

Artifact:
`../../waymark-gateway/target/runs/staged-stone-doom-v12-typed-decisions-terra/cell/cell.json`
