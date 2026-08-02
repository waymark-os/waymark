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

A later task-shaped authorship experiment found that both the existing
`@stage` form and a proposed `workflow`/`stage` block were syntactically easy:
each passed 3/3 first responses. Moving evidence from nested declaration fields
to body-level `ensure` contracts produced 3/3 syntax-valid sources that were
28.5% smaller than the decorator form, and all three explicitly represented a
public stdout obligation that both header-evidence arms omitted. This supports
prototype lowering for code-like workflow blocks and re-checkable `ensure`
postconditions, while retaining this typed kernel. See
[Stone Staged Harness Syntax Experiment](STONE_STAGED_HARNESS_SYNTAX_EXPERIMENT.md).

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

A candidate at this boundary declares:

```text
identity
task-specific input
assumptions and applicability obligations
expected acceptance evidence
```

A stage-patch candidate additionally names its target stage and expected stage
evidence in task-specific input.

Schema validation can prove only that a proposed identity is admitted. The
candidate must still run in an isolated attempt restored from the relevant
semantic checkpoint. Fresh evidence, not the model proposal or an action's
`ok` field, decides acceptance.

An unsatisfied candidate is an expected bounded `CandidateOutcome` so the
supervisor can eliminate it and continue. Authority denial, provider loss,
corrupt checkpoint state, and similar infrastructure faults remain structured
exceptions. A subsequent attempt is meaningful only if it changes the
candidate, assumptions, evidence, or stage-local program.

The first convenience interface is now the visible ordinary-Stone
[`bounded_attempt_explore.stone`](../examples/scripts/bounded_attempt_explore.stone)
library. Its `candidate(...)` records expose identity, assumptions, and ensures.
Its compatibility `explore(checkpoint=...)` control accepts a raw parent-owned
checkpoint. New programs use
`explore_frontier(frontier=semantic_frontier(...))`, which applies the same
policy loop to parent-owned and retained repair frontiers. Both lowerings carry
the same optional bounded context projection, evidence checks, accept/discard,
and scope cleanup. Expected rejection remains a bounded outcome while
controller/infrastructure failure remains exceptional.

In a three-pair authorship comparison, the library arm passed 3/3 first
responses with no repair and averaged 390 authored bytes. Explicit lifecycle
control passed 1/3 first responses and 2/3 after two repairs, averaging 2,523
bytes among passing sources. See
[Stone Bounded Exploration Authorship Experiment](STONE_BOUNDED_EXPLORATION_AUTHORSHIP_EXPERIMENT.md).
Dedicated syntax should still wait until the same source survives multiple
task shapes.

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

The preferred Stone interface keeps the opaque reference and its ownership
mode inside a nominal task-owned capability:

```stone
report = workflow_run(workflow("prepare", dependencies))
frontier = semantic_frontier(report.stages[0].checkpoint)
child = attempt_branch(frontier, input={"strategy": "alternate"})
```

For a retained checkpoint owned by a failed child, the author supplies the
owner once:

```stone
frontier = semantic_frontier(checkpoint, owner=failed_child)
child = attempt_branch(frontier, input={"strategy": "repaired"})
```

`attempt_branch` selects the authorized parent fork or repair restoration
without exposing raw workspace-source records. Gateway still validates the
checkpoint, owner, task tree, captured planes, and budget. The child receives
the checkpoint's workspace and bounded memory frontiers, so later parent files
and memory cannot leak into the branch.

Every created stage checkpoint reports deterministic `guidance`; a frontier
also exposes its measured `cost` and budget-aware guidance, not raw authority
identifiers. Seal duration at or above ten seconds, five percent of remaining
attempt time, or large retained storage recommends reuse. Evaluation
diagnostics report branch counts and unused constructed frontiers. This is
guidance rather than automatic placement: declare a frontier at a likely
exploration or repair boundary, reuse it across bounded candidates, and avoid
sealing equivalent state.

In a frozen three-pair authorship comparison, raw and typed controllers both
passed 3/3 after at most one repair and both emitted zero redundant seals. The
typed programs averaged 42.7 lines versus 53.7 for raw control, but one typed
draft required repair after replacing its child handle with the record
returned by `attempt_accept`; all raw drafts passed initially. This does not
establish an authorship-rate gain. It does identify the next typing seam:
lifecycle reports should not be easy to confuse with the capabilities they
transform.

The follow-up makes that seam nominal. `attempt_accept` now returns an
`attempt_acceptance` transition:

```stone
accepted = attempt_accept(root, child)
selected = accepted.selected
```

`accepted.selected` retains type `attempt_handle`; the acceptance itself
carries `status`, `attempt`, `parent`, `child`, and import-diff fields. Stone
function annotations recognize the attempt-control types, so a parameter
declared `repaired: attempt_handle` rejects an `attempt_acceptance` before
entering the helper body and emits targeted repair guidance. These stable
runtime tags are also a useful boundary for future JIT work, without requiring
static inference in the current interpreter.

A fresh one-pair, no-repair authorship smoke passed both interfaces on the
first response. The typed draft preserved its repaired handle across
`attempt_accept` and used 27 lines versus 55 in the raw draft. Treat this as a
learnability canary only; the earlier frozen three-pair result remains the
comparative evidence.

This initial backend is deliberately narrow. If setup has already changed a
container root, `forkable` fails because immutable-image reconstruction would
lose those changes. Likewise, accepting a child after it starts a mutable
provider container fails with `fork_join_plane_unavailable`: the workspace
cannot be accepted while silently dropping the selected provider-root state.
Mutable Docker/VM generation snapshot and join are the next provider slice.

## Example

An inspection stage can declare the semantic inputs required downstream:

```stone
workflow project:
    stage inspect(goal="resolve build requirements", max_actions=4):
        agent_loop()
        ensure decision_recorded(fields=[
            "source_layout",
            "toolchain",
            "frame_backend",
            "runtime_contract",
        ])
```

The standard stage agent's `decide` action must provide an answer plus a
non-empty finding for each field. The runtime rechecks those fields before the
stage advances and reports the missing names. This proves that the typed
decision record exists; artifact, command, and external verifier evidence must
still validate its consequences.

A typed shape gate also does not imply that a finding is resolved. A task cell
can satisfy a required non-empty field with an honest statement that the fact
is still unknown. Resolution therefore needs explicit state and a compact
observation basis, while final correctness remains under fresh evidence gates.

The experimental stateful form is:

```stone
ensure decision_recorded(resolved=["source_layout", "toolchain"])
```

Each named finding is a bounded `{state, value, basis}` record. `state` is
`resolved` or `unknown`; both are representable, but only `resolved` satisfies
the contract. An honest unknown therefore keeps the stage open and its value
and basis explain what remains missing. `basis` is compact semantic provenance,
not authoritative proof; artifact, command, and verifier gates retain that
role. The older `fields=[...]` form continues to accept structural strings.

In the first task-shaped run, the model used `unknown` honestly and the runtime
kept the inspection stage open. That prevents a bad handoff, but it also showed
that a partial finding publication consumes a stage action just like a file
observation. Finding state and budget accounting are therefore separate design
axes: retain the strict state gate, then make incremental publication cheaper
or give it an explicit budget class.

### Runtime-owned incremental findings

The workflow kernel now retains one bounded finding map per stage and exposes
it as `step.findings`. An action may return `finding_values={field: value}` for
facts established by its prior tool outcome. The kernel accepts only declared
resolved fields, creates the `{state: "resolved", value, basis}` record, and
derives `basis` from that prior action. Explicit nested `finding_updates` and
decision findings remain supported as the lower-level compatibility surface.

The standard controller presents the smaller form as a top-level
`learned={field: value}` action annotation and accepts
`unknown={field: reason}` on the bounded final report. Only unresolved fields
appear in those schemas. `learned` is unavailable until a real `read`, `write`,
`run_linux`, or `run_complete` outcome exists, so a schema error or prose
decision cannot become provenance. The final decision requires only its
answer; evidence evaluation reads the accumulated kernel state.

For resolved-decision stages, unresolved fields admit only tool/probe actions.
Decision actions become available once all fields resolve, or on the last
action so failure can be reported honestly. The schema prompt is generated
from the same state. Validation failures for `oneOf` action schemas enumerate
allowed discriminators and describe the nearest branch, making the recovery
visible to the model.

A Doom rollout used this form without a separate publication action and
retained `source_layout` with basis `runtime:action:1:read:.`. It then failed
safely with the other three fields named as missing. This validates incremental
state retention and control gating; it does not yet validate evidence
acquisition policy. Field-directed probes are the next open construct.

### Field-directed probes

`decision_recorded(resolved=...)` also accepts typed finding descriptors:

```stone
ensure decision_recorded(resolved={
    "source_layout": {
        "kind": "path",
        "question": "Which directory contains the Doom C sources?",
    },
    "toolchain": {
        "kind": "command",
        "question": "Which compiler and flags produce the target ELF?",
    },
})
```

The kernel exposes these as `step.finding_specs` and retains bounded
`step.probes`. On a resolved-finding stage, each ordinary tool action carries
`for_field`; a later action may carry `resolves={field: interpretation}`. The
kernel accepts a resolution only when that field has a retained observation,
copies the observation's runtime-owned basis into the finding, and marks the
probe resolved. A response may resolve one observed field while probing the
next. The compatibility forms `finding_values` and `finding_updates` remain
available but are no longer the standard controller surface.

The Doom comparison improved obligation coverage from one of four fields to
four of four. It still failed safely: only `source_layout` resolved. Compiler
discovery failed, a shallow source listing did not identify the supplied frame
backend, and the bounded prefix of `vm.js` did not reach its invocation logic.
This separates two concerns that the earlier interface conflated:
field-directed scheduling works, but an observation is not necessarily
sufficient evidence. The next acquisition construct should support focused
search/tail or other typed probes, an explicit insufficient/refine state, and
retry budget beyond one probe per field.

Artifact:
`../../waymark-gateway/target/runs/staged-stone-doom-v24-field-directed-probes-terra/cell/cell.json`

The acquisition follow-up exposes native recursive `find` and bounded
literal/regex `search` actions, plus byte-offset, tail, and streamed line-range
reads. Probe refinements retain a bounded revision and up to four runtime-owned
bases. Scheduling covers fields with no observation before revisiting an
observed field. An empty tool result is now retained as `insufficient`, so it
cannot be resolved until a non-empty refinement supplies evidence.

This removed two concrete controller failures. The first rollout exposed the
new tools but starved three fields while repeatedly reading `vm.js`; it also
found that a line range was incorrectly sliced from only the first 8 KiB. After
streaming line ranges and coverage-first scheduling, Doom completed inspection
in five actions with four resolved fields, built a valid MIPS ELF, and reached
the run stage. It then failed because the produced conventional Linux ELF ran
only nine VM instructions and emitted no frame; the final repair also replaced
the required exact `node vm.js` command with a build-and-run shell command.

The Rust acquisition path is not the bottleneck: a local recursive find over
95 C files, symbol search, and 4 KiB tail read took about 40 ms. The successful
inspection/build rollout spent about 150 seconds overall and made 23 model
calls. The next typed-acquisition problem is semantic sufficiency: non-empty
evidence must answer the field question (for example, freestanding VM ABI and
entry requirements), and a failed multi-contract run gate needs a more useful
contract-directed repair interface.

Artifacts:
`../../waymark-gateway/target/runs/staged-stone-doom-v25-native-acquisition-terra/cell/cell.json`
and
`../../waymark-gateway/target/runs/staged-stone-doom-v26-streamed-lines-coverage-terra/cell/cell.json`

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

The next official cell lowered that search to the unchanged visible
`bounded_attempt_explore.stone` library. It rejected the same lexical candidate,
reused the post-build frontier for the inode candidate, and passed all six
checks. This validated transfer of the library control and exposed the
frontier typing gap. The subsequent semantic-frontier canary closed that
authoring gap: the same `attempt_branch` call passed for a parent-owned
checkpoint and a retained failed-owner checkpoint, with authoritative cleanup
and zero leaked transactions or checkpoints. See
[Caffe Bounded-Explore Library Experiment](../../waymark-gateway/docs/CAFFE_BOUNDED_EXPLORE_LIBRARY_EXPERIMENT.md).

A later task-shaped canary passed the full visible library through both
frontier origins. Each path rejected one candidate, reused the same checkpoint,
accepted the second, and admitted the same required-memory projection to both
workers. This closes the library-level frontier gap: task policy no longer
needs to know whether the runtime will fork a current parent or restore a
retained repair checkpoint.

`explore_frontier(...)` now owns the supplied frontier for the duration of that
bounded search and calls `semantic_frontier_release(...)` after either
acceptance or exhaustion. Release is idempotent, changes the nominal
capability's status to `released`, and prevents later branches. Matched live
cells covered parent and retained origins with both terminal outcomes; all four
had no active checkpoint before root cleanup. Gateway verifies that the
attached releaser is the owner or the retained owner's direct parent.

Semantic frontiers also have evaluation-scope cleanup. After automatically
closing open child scopes, Stone releases every frontier that was not already
released explicitly. This covers parseable programs that exit through `fail`
or another runtime exception after sealing a frontier. The original program
error remains primary; a failed release is attached as a related cleanup error.
Diagnostics distinguish `explicit` from `evaluation_cleanup` release and count
automatically released values.

The scoped-resource follow-up makes ownership visible in ordinary Stone.
`with semantic_frontier(...) as frontier:` releases the checkpoint at that
boundary, while `with scope:` performs checked cancel-then-join cleanup.
Nominal methods lower to the existing kernel operations. The first authorship
run failed because Stone unexpectedly hid bindings after `with`; after adopting
Python block visibility, scoped syntax passed 3/3 first responses with no
repairs and was slightly smaller than typed-functional control. Resources are
closed at exit, but their handles and evidence remain visible for finalization.

Restricted `async def`/`async with`/`await` syntax also passed 3/3 without
repairs. It currently marks blocking attempt effects and did not improve on the
synchronous scoped form, so it remains experimental rather than a general
async runtime. See
[Scoped Attempt Resources Experiment](STONE_SCOPED_ATTEMPT_RESOURCES_EXPERIMENT.md)
and [Async Attempt Control Experiment](STONE_ASYNC_ATTEMPT_CONTROL_EXPERIMENT.md).
