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

## Stage-Scoped Repair

`workflow_patch` constructs a new workflow by replacing exactly one named
stage:

```stone
base = workflow("build", compile, bundle_runtime_broken, verify)
repaired = workflow_patch(
    base,
    "bundle_runtime_broken",
    bundle_runtime_fixed,
)
report = workflow_run(repaired)
```

The base value is unchanged. The replacement retains the target's ordered
position, a missing target fails, and a replacement name that collides with
another stage fails. The resulting report includes bounded
`patches=[{target, replacement}]` provenance. This makes a repair a typed,
stage-local program transformation rather than whole-program regeneration.

For a model-authored or otherwise data-driven selection, pass an exact patch
record followed by the explicitly allowed replacement stages:

```stone
inference = model_infer(messages, patch_schema)
repaired = workflow_patch(
    base,
    inference.value,
    bundle_runtime_fixed,
    bundle_runtime_alternate,
)
```

The record must contain exactly the string fields `target` and `replacement`.
The named replacement must resolve to one of the supplied
`workflow_stage` values. Missing fields, extra fields, repeated candidates, and
unlisted replacements fail before the workflow runs. This keeps model output
as bounded typed data; it does not dynamically evaluate authored source or
grant access to undeclared stages. See the runnable
[`workflow_model_patch_canary.stone`](../examples/scripts/workflow_model_patch_canary.stone)
for a live `model_infer` example.

The intended control pattern is:

```text
proved stage -> repairable checkpoint -> later stage fails
  -> supervisor inspects evidence
  -> patch only the failed stage
  -> second repair restores the proved frontier
  -> earlier stage is already_satisfied
```

When several candidates are plausible, treat model selection as a proposal,
not as acceptance:

```text
model proposes an admitted candidate
  -> run it from the repairable checkpoint
  -> reject it if stage evidence fails
  -> run an untried candidate from the same checkpoint
  -> accept only a candidate that satisfies final evidence
```

The runnable canary deliberately supports this bounded fallback. This explicit
control flow is also a lowering target for a future Stone candidate-exploration
construct; failed candidates remain ordinary attempt outcomes rather than
prompt-only reasoning.

When an attached controller reports its final value, Waymark also bounds each
result, error, and detail document below the Gateway authority limit.
Oversized recursive diagnostics are compacted deterministically with preserved
error identity and `_waymark_compaction` metadata; they are not allowed to turn
a useful task failure into a failed report syscall.

### Candidate Contracts And Outcomes

A candidate should eventually declare:

```text
identity
target stage
assumptions and applicability obligations
expected stage evidence
expected final evidence
```

Schema validation can prove only that a proposed identity is admitted. The
candidate must still run in an isolated attempt restored from the relevant
semantic checkpoint. Fresh evidence, not the model proposal or an action's
`ok` field, decides acceptance.

An unsatisfied candidate is an expected bounded `CandidateOutcome` so the
supervisor can eliminate it and continue. Authority denial, provider loss,
corrupt checkpoint state, and similar infrastructure faults remain structured
exceptions. A subsequent attempt is meaningful only if it changes the
candidate, assumptions, evidence, or stage-local program.

The next convenience interface should be a visible ordinary-Stone `explore`
library that lowers to existing workflow patches, checkpoint-backed child
attempts, evidence checks, accept/discard, and scope cleanup. Dedicated syntax
should wait until that interface has survived multiple tasks.

## Stage Checkpoint Declaration

Stage checkpoints should be semantically visible but physically
provider-neutral:

```stone
@stage(
    evidence=file_nonempty("/app/project/build/output"),
    checkpoint="workspace",
)
def build(step):
    return run_complete(...)
```

The declaration accepts `none`, `workspace`, `forkable`, `repairable`, or
`auto`.
The implemented first slice has these semantics:

1. `none` is the default and creates no checkpoint;
2. `workspace` requests a Gateway workspace/environment-map checkpoint plus
   the bounded attempt-memory frontier, only after fresh post-action or
   post-repair evidence is satisfied;
3. `auto` currently selects that `workspace` checkpoint bundle;
4. an already-satisfied or failed stage does not create a checkpoint;
5. the stage report exposes an opaque `reference`, selected policy, captured
   planes, workspace and memory revisions, creation cost, and stable failure
   code, but no host path;
6. `forkable` additionally seals the tool-environment plane when it is absent
   or reconstructable from a locally materialized immutable image;
7. attached containers and mutable provider roots fail closed with
   `checkpoint_plane_unavailable` unless the provider exposes an authorized
   snapshot contract;
8. `repairable` captures the same planes as `forkable`, but the Gateway retains
   only the newest such frontier across attempt failure. A repair spawned from
   that checkpoint restores workspace, memory, and tool environment; failure
   leaves it reusable, while acceptance, rollback, commit, or publication
   consumes it.

The parent attempt owns its Stone stage checkpoints. They may be borrowed by
multiple child forks and survive child finish; parent finish reclaims them.
Repairable checkpoints are the explicit exception on failure, with
newest-frontier replacement bounding retention.
Explicit diagnostic checkpoints retain their independent lifecycle and cannot
be used as attempt-fork frontiers.

Stone deliberately does not expose `linux_env.checkpoint()`. The program knows
which stage boundary is valuable, but Gateway retains authority over provider
support, secret and mount exclusion, budgets, deduplication, retention, and
garbage collection. Running Linux processes are excluded by default;
application-native resumable artifacts are preferred when work must continue
mid-stage.

The lowering is:

```text
@stage(checkpoint="workspace")
  -> typed stage checkpoint policy
  -> satisfied evidence transition
  -> Gateway workspace + attempt-memory checkpoint request
  -> opaque checkpoint ref in the workflow report
```

The first `forkable` lowering replaces the workspace-only request with a
Gateway composite checkpoint containing an opaque tool-environment generation
and disposition. Gateway resolves a declared image to its immutable provider
identity, stores that identity only in the host reconstruction record, and
gives the child an opaque selector. The child receives a fresh provider
instance with its own workspace branch; no process, socket, or provider
instance identity is cloned.

The implemented checkpoint-backed fork consumes the opaque reference directly:

```stone
report = workflow_run(workflow("prepare", dependencies))
checkpoint = report.stages[0].checkpoint.reference
child = attempt_fork(checkpoint=checkpoint, input={"strategy": "alternate"})
```

Gateway accepts only an active attempt-owned checkpoint belonging to the
selected parent. The child receives the checkpoint's workspace and bounded
memory frontiers, so later parent files and memory cannot leak into the branch.
The checkpoint is borrowed rather than consumed, allowing sibling explorations.
This initial backend is deliberately narrow. If setup has already changed a
container root, `forkable` fails because immutable-image reconstruction would
lose those changes. Likewise, accepting a child after it starts a mutable
provider container fails with `fork_join_plane_unavailable`: the workspace
cannot be accepted while silently dropping the selected provider-root state.
Mutable Docker/VM generation snapshot and join are the next provider slice.

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
- Durable restart, parallel stages, and declarative effect records are future
  extensions.

## Validation

The runtime suite covers repair-required completion, unmet evidence despite
successful actions, lambda handlers with pre-satisfied skipping, immutable
stage-scoped patches and collision rejection, malformed satisfied evidence,
checkpoint policy validation, pre-satisfied checkpoint skipping, and explicit
unavailable-plane reports. The checked-in
[`typed_evidence_workflow.stone`](../examples/scripts/typed_evidence_workflow.stone)
is the standalone model-free canary.

The Gateway repository's `smoke-stone-stage-checkpoint.sh` is the end-to-end
checkpoint canary. It proves creation after fresh evidence, opaque report
metadata, exclusion of later workspace and memory pollution, reusable sibling
forks, selected-child acceptance, restore, and parent-owned cleanup.
`smoke-stone-forkable-environment.sh` separately proves immutable-image
resolution, opaque generation reporting, child rematerialization without an
image argument, workspace isolation, conservative mutable-child join
rejection, and cleanup.

A matched three-pair live-model experiment found both the callback kernel and
`@stage` syntax correct on all first drafts. The syntax reduced mean generated
source from 558 to 307 bytes and from three to two function definitions with no
repair attempts. See
[Stone Stage Syntax Authorship Experiment](STONE_STAGE_SYNTAX_AUTHORSHIP_EXPERIMENT.md).

An official Caffe mechanism cell then exercised the complete repair pattern:
the first continuation built Caffe, sealed a repairable post-build frontier,
and failed at an intentionally defective runtime-bundling stage. A second
continuation restored that frontier, reported the build stage
`already_satisfied`, applied one `workflow_patch`, and passed the fresh-container
verifier. The run used zero model calls, left zero open checkpoints or
transactions, and recorded reward `1.0`. This validates the control primitive,
not yet LLM patch-synthesis reliability or a stable task success rate.

The later V35-V37 sequence tested model-selected candidates. The first typed
selection failed because the projected observation omitted an applicability
fact. Bounded fallback rejected it by stage evidence, restored the same
post-build checkpoint, and tried the remaining candidate without replaying the
build. A second failure showed that evidence must measure the saved artifact
rather than a nearby in-training state. After that boundary was corrected, all
six official checks passed. This is concrete support for typed,
evidence-gated exploration, still limited to one task family.
See
[Caffe Bounded Stage Exploration Experiment](../../waymark-gateway/docs/CAFFE_BOUNDED_STAGE_EXPLORATION_EXPERIMENT.md).
