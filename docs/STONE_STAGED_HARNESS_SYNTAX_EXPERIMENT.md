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
