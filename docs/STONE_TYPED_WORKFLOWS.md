# Stone Typed Workflows

Status: initial vertical slice implemented and validated.

## Purpose

An attempt program should not ask a model to rediscover a stable procedure on
every action. Stone workflows encode that procedure as deterministic stages and
use the model only outside the proved path or inside an explicit repair
callback.

The core rule is:

> A stage advances only when its evidence function returns a satisfied,
> bounded evidence value.

An action's exit status is useful input, but it is not completion evidence by
itself.

## Values

The preferred LLM-facing declaration form is:

```stone
def repair_output(step):
    return run(["sh", "-c", "printf ready > artifact.txt"])

@stage(
    evidence=file_nonempty("artifact.txt"),
    repair=repair_output,
    max_attempts=1,
)
def artifact(step):
    return run(["sh", "-c", "exit 7"])

report = workflow_run(workflow("build-artifact", artifact))
```

`@stage` is syntax, not a general Python decorator. The frontend lowers the
decorated action definition to the same typed stage representation used by the
interpreter and future compilers. `file_nonempty` is a lazy typed evidence
specification: the runtime performs the workspace probe and constructs the
path/size reference, so the author does not need a checker callback.

The lowering boundary is intentional:

```text
@stage declaration
  -> typed stage + evidence-spec IR
  -> interpreter today / JIT compiler later
  -> identical evidence-gated runner semantics
```

New authoring constructs should lower into this kernel rather than duplicate
retry, evidence, or report policy in a second runtime.

The explicit semantic-kernel form remains available:

```stone
evidence = workflow_evidence(
    satisfied,
    summary,
    ["bounded:evidence-reference"],
)

stage = workflow_stage(
    "stage-name",
    evidence=check_stage,
    action=run_stage,
    repair=repair_stage,
    max_attempts=2,
)

plan = workflow("workflow-name", stage, ...)
report = workflow_run(plan)
```

`workflow_stage` and `workflow` return first-class typed control values.
`@stage` binds the decorated function name to the equivalent stage value. These
values may be passed through ordinary Stone functions, but cannot be serialized
into ordinary records or JSON.

Every handler takes one structured context record. An evidence handler must
return `workflow_evidence(...)`. Action and repair handlers must return a
record with a boolean `ok` field.

## Execution

For each stage, `workflow_run`:

1. checks evidence before taking an action;
2. skips an already-satisfied stage;
3. invokes the action at most `max_attempts` times;
4. checks evidence after every action;
5. optionally invokes the repair handler, then checks evidence again;
6. advances only after a satisfied evidence check;
7. otherwise returns a compact failed workflow report.

Evidence is deliberately bounded: a non-empty summary of at most 1,024
characters and at most 16 non-empty string references of at most 256
characters each. A satisfied result requires at least one reference.

The workflow runner does not hide callback errors, silently retry exceptions,
or infer success from `ok=true`. Recursive `workflow_run` calls are rejected in
this first slice.

## Target Checkpoint Declaration

Stage checkpoints should be semantically visible but physically
provider-neutral. The proposed declaration is:

```stone
@stage(
    evidence=file_nonempty("/app/project/build/output"),
    checkpoint="forkable",
)
def build(step):
    return run_complete(...)
```

The declaration is not implemented in the initial workflow slice. Its target
semantics are:

1. `checkpoint` accepts `none`, `workspace`, `forkable`, or `auto`;
2. the runtime requests it only after fresh stage evidence is satisfied;
3. Gateway atomically snapshots the selected Attempt state planes;
4. the workflow report exposes only an opaque checkpoint reference and
   forkability/cost metadata;
5. a later `attempt_fork` may name that reference without seeing Docker, host
   paths, overlay directories, or VM snapshot identifiers.

Stone deliberately does not expose `linux_env.checkpoint()`. The program knows
which stage boundary is valuable, but Gateway retains authority over provider
support, secret and mount exclusion, budgets, deduplication, retention, and
garbage collection. Running Linux processes are excluded by default;
application-native resumable artifacts are preferred when work must continue
mid-stage.

The expected lowering is:

```text
@stage(checkpoint="forkable")
  -> typed stage checkpoint policy
  -> satisfied evidence transition
  -> Gateway composite checkpoint request
  -> opaque checkpoint ref in the workflow report
```

## Example

```stone
def repair_output(step):
    return run(["sh", "-c", "printf ready > artifact.txt"])

@stage(
    evidence=file_nonempty("artifact.txt"),
    repair=repair_output,
    max_attempts=1,
)
def artifact(step):
    return run(["sh", "-c", "exit 7"])

report = workflow_run(workflow("build-artifact", artifact))
emit(report)
```

The failed action does not complete the stage. The repair creates the artifact,
the evidence check proves it, and only then does the workflow complete.

## Boundary

This is a control-flow primitive, not a general workflow service:

- Waymark retains authority over effects, resources, transactions, and traces.
- Handlers may call normal Stone and Gateway effects.
- Evidence references are compact claims, not archived logs or artifact bytes.
- Cross-attempt distribution, durable restart, parallel stages, and declarative
  effect records are future extensions.

## Validation

The runtime suite covers repair-required completion, unmet evidence despite
successful actions, lambda handlers with pre-satisfied skipping, and malformed
satisfied evidence. The checked-in
[`typed_evidence_workflow.stone`](../examples/scripts/typed_evidence_workflow.stone)
is the standalone model-free canary.

A matched three-pair live-model experiment found both the callback kernel and
`@stage` syntax correct on all first drafts. The syntax reduced mean generated
source from 558 to 307 bytes and from three to two function definitions with no
repair attempts. See
[Stone Stage Syntax Authorship Experiment](STONE_STAGE_SYNTAX_AUTHORSHIP_EXPERIMENT.md).
