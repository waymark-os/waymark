# Standard Visible Agent Control

`examples/scripts/standard_attempt_agent.stone` is the standard,
fully visible Stone action controller. It is ordinary admitted source rather
than a Rust workflow or Gateway mode.

## Contract

The reusable entrypoint is:

```stone
standard_agent_control(
    session,
    options,
    dispatch_action,
    verify_finish,
    record_progress,
    decision_hooks=None,
)
```

The adapter arguments are first-class named Stone functions. A task can
replace tool dispatch, finish verification, progress retention, or the
call-local decision hooks while preserving the bounded decision loop. Construct
the latter with `transition_hooks(pre=..., post=...)`; the resulting value can
be bound and passed through ordinary Stone functions.

V12 provides the accumulated controls below. The combined controller provides:

- exactly one schema-validated action per model transition;
- a compact model-facing action contract with the complete schema retained as
  runtime validation authority;
- bounded repair through `model_infer`;
- bounded conversation history, file reads, Linux output, time, rounds, and
  action count;
- recoverable structured tool errors;
- one bounded public-task requirement;
- a fixed-size ring of bounded tool evidence;
- a same-key semantic requirement audit and same-key progress retention;
- a fresh, schema-checked completion critique with bounded retries and output;
- deterministic rejection when an audit claims approval while retaining an
  unsupported requirement;
- a total model-call budget that reserves room for completion critique;
- one proactive requirement/evidence checkpoint before the finalization
  window, with remaining calls reserved for repair, explicit finish, and the
  final completion critique;
- a bounded public-task projection and hard character ceiling for the recent
  action trajectory;
- a fresh, token-bounded attempt-memory projection on every action decision;
- separate model-visible observation and retained-evidence bounds, with
  original field lengths, truncation provenance, and head/tail excerpts;
- replacement of stored message history with its compacted form;
- bounded terminate/observe/retry handling for an owned Linux run that returns
  `still_running`, including recovery when the first provider termination
  request fails;
- explicit `run_start`, `run_status`, `run_wait`, and `run_terminate` actions
  over the Gateway's existing attempt-owned operation lifecycle;
- one runtime-owned `run_complete` action for bounded foreground commands,
  lowering internal observations without spending a model decision per poll;
- one bounded `progress.active_runs` memory item, terminal-handle removal, a
  configurable active-run cap, finish rejection while runs remain active, and
  bounded reaping on controller budget exhaustion;
- authoritative startup reconstruction of that item from
  `attempt_inspect().active_runs`, before the first model decision;
- up to four fail-closed required memory keys from the attempt's typed
  `context_prompt_view`, applied only to the first action projection on each
  controller start;
- compatibility fallback to input `initial_action_memory_required_keys` for
  older admission paths, with the selected policy source recorded in results;
- up to four `action_memory_required_keys` for task harnesses whose
  before/after action-state updates must be present in every later decision;
- a first-class `decision_hooks` value passed to each main action-selection
  `model_infer`, so a task harness can prepare or record that individual
  action-state transition without installing global policy;
- an explicit finish-verification adapter;
- compact result provenance under `_control`.

The deterministic `verify_finish` adapter runs before the semantic critique.
The critic receives only the public task, visible recorded evidence, prior
audit, and candidate. It never receives hidden verifier data and cannot replace
Gateway or task-verifier authority.

Gateway still owns capabilities, credentials, transaction lifecycle, and
resource enforcement. The library's verifier only checks its local finish
contract; it is not an authoritative task verifier.

## Current Packaging

Stone does not yet have a module import mechanism. Consequently V12 is a
checked-in source bundle containing the library functions and a small default
adapter invocation. The complete source is included in the admitted program
digest. This keeps policy visible while leaving module loading as a separate
language-design decision.

The default dispatcher supports `read`, `write`, short synchronous
`run_linux`, runtime-owned `run_complete`, the four manual owned-task lifecycle
actions, and `finish`. Task-specific controls may replace it with another named
function without changing `standard_agent_control`.

## Validation

The deterministic fixture and `gpt-5.6-terra` low-reasoning baselines both
passed all three short trajectories: direct finish, transactional file
mutation/read-back, and recovery after a failed Linux action. Each run made the
expected ten model calls and exact expected tool calls; every attempt rolled
back cleanly.

An attached malformed-response canary also proves the repair boundary: the
first invalid action list caused one separately traced repair call, the valid
response returned `validation_retries: 1`, and the invalid response was not
counted as an action.

The compact V4 loop retained the exact ten-call trajectory and reduced the
three-cell real-model input total from 8,202 to 4,962 tokens. This was 12.7%
below the typed task-shaped loop, though still 11.6% above raw control. These
are single live samples, so output-token differences are not interpreted.

Two outer models also authored small named verifier adapters from the compact
contract and passed an attached fixture. The admitted results prove delegation,
task-input/state annotation, standard provenance, bounded progress, and clean
rollback. See
[Standard Agent Specialization Experiment](STONE_STANDARD_AGENT_SPECIALIZATION_EXPERIMENT.md).

A follow-up repository canary used a model-authored verifier with a real inner
model. The external harness checked exact workspace bytes and effects; a
wrong-output run was rejected by the same verifier. See
[Standard Agent Task Specialization Experiment](STONE_STANDARD_AGENT_TASK_SPECIALIZATION_EXPERIMENT.md).

The next canary removed the known answer from the authored suffix. Terra
authored a bounded executable verifier, a real inner attempt passed visible
tests, and a host-owned hidden verifier independently passed the checkpointed
transaction. A wrong implementation was rejected by both layers; verifier
branches and root attempts rolled back. See
[Standard Agent Executable-Verifier Experiment](STONE_STANDARD_AGENT_EXECUTABLE_VERIFIER_EXPERIMENT.md).

The generic controller then completed the untouched TBv2.1
`portfolio-optimization` task and earned official Harbor reward `1.0` in eight
model decisions. The run used no task-specific Stone source or attempt forks;
source/admission hashes matched, guarded binary publication stayed within its
human-approved scope, and lifecycle state closed cleanly. This is an
engineering mechanism result rather than a matched benchmark comparison. See
`../waymark-gateway/docs/STANDARD_STONE_TBV21_PORTFOLIO_EXPERIMENT.md`.

The exact repaired treatment was then frozen for untouched
`mcmc-sampling-stan`. It published cleanly and reached the official verifier,
but earned reward `0.0`: numerical estimates passed while RStan installation
and end-to-end sampling failed. The controller's only memory was loop progress,
and its non-empty-answer finish check accepted completion without per-
requirement evidence. See
`../waymark-gateway/docs/STANDARD_STONE_TBV21_MCMC_EXPERIMENT.md`.

V1 directly addresses that observed gap. In a constructed causal comparison,
the critic rejected a premature finish, returned a read-back repair objective,
and approved only after the missing evidence was recorded. The same first two
actions finished without the read when critique was disabled. A contradictory
`approved=true` audit with an unsupported requirement was also rejected.
All cells rolled back and retained at most five memory items. This validates
the mechanism, not real-model critic quality. See
[Standard Agent Completion-Critique Experiment](STONE_STANDARD_AGENT_COMPLETION_CRITIQUE_EXPERIMENT.md).

The follow-up real-model quality gate passed all three constructed judgments
with one Terra call each: reject missing read-back, approve complete
write/read evidence, and reject a failed RStan execution. The failed-execution
probe also exposed and fixed the V1 failed-evidence status mismatch
(`contradicted`, not `rejected`). See
[Standard Completion-Critic Quality Experiment](STONE_STANDARD_COMPLETION_CRITIC_QUALITY_EXPERIMENT.md).

The next frozen V1 target, untouched `make-doom-for-mips`, exposed a second
control gap before official verification: the action model never proposed
finish, so the finish-triggered critic never ran. It used 31 model calls and
returned incomplete; safe publication then refused 83 binary artifacts and
rolled the failed attempt back with no live transaction or checkpoint. V2
addresses that generic failure mode with the proactive finalization
checkpoint. A matched fixture pair shows the checkpoint inducing the missing
repair and finish while the no-checkpoint cell exhausts its usable budget.

V3 addresses the remaining action-context pressure. Fourteen consecutive
32-KiB tool outputs stayed under a 16,384-character action-context ceiling,
dropped four old messages, retained ten memory items, and preserved explicit
raw lengths plus both output ends. Token-realistic fixture requests peaked at
4,147 input tokens, 55.3x below the uncompacted peak lower bound. A real Terra
failed-action recovery also passed in five calls with five memory projections.
See
[Standard Agent Context-Pressure Experiment](STONE_STANDARD_AGENT_CONTEXT_PRESSURE_EXPERIMENT.md).

V4 addresses the owned-run leak exposed by the first
`extract-moves-from-video` cell. A Firecracker regression records
`timed_out + still_running`, `timed_out_and_reaped`, accepted result reporting,
and rollback in order. In the frozen TBv2.1 replication, V4 recovered from
five timeouts and four provider cancellation escalations, then successfully
reported an explicit incomplete result. This validates cleanup and reporting,
not task completion; no official verifier ran. See
`../waymark-gateway/docs/STANDARD_STONE_TBV21_EXTRACT_MOVES_V3_V4_EXPERIMENT.md`.

V5 exposes the underlying long-run lifecycle to the action model. A real Terra
canary learned the exact JSON interface, copied a dynamic `run_id` from
`run_start` into `run_wait`, observed terminal success, read the output, and
finished. A deterministic fixture separately proves early-finish rejection,
the active-run cap, bounded same-key handle memory, terminal-handle removal,
and zero live handles in the final report. See
[Standard Agent Owned-Task Management](STONE_STANDARD_AGENT_TASK_MANAGEMENT_EXPERIMENT.md).

V6 closes the controller-restart gap. The ledger is treated as a model-facing
index, not resource authority: every controller start rebuilds it from
Gateway's attempt-scoped active-run snapshot. A two-run canary stops after
starting a background operation but before recording its handle; the restarted
controller recovers that handle, terminates and reaps it, settles the same-key
ledger to an empty verified item, and rolls the attempt back cleanly. See
[Standard Agent Restart Reconciliation](STONE_STANDARD_AGENT_RESTART_RECONCILIATION_EXPERIMENT.md).

V7 makes a forked child's first-context contract explicit. Fork copies the
parent ledger revision but does not implicitly inject memory into a prompt. A
standard child can declare up to four
`initial_action_memory_required_keys`; the first action projection includes
them or fails closed. Fixture and real Terra canaries inherited an uncommitted
workspace prefix and `requirement.fork_target`, excluded a post-fork parent
write, and finished in one model call. See
[Standard Agent Fork First Context](STONE_STANDARD_AGENT_FORK_FIRST_CONTEXT_EXPERIMENT.md).

V8 promotes that boot policy out of task input. `attempt_fork` persists a typed
`context_prompt_view` on the child admission record, `agent_session()` exposes
it, and the standard controller gives it precedence over the V7 compatibility
option. The fixture canary proves the child input no longer carries the policy
while the exact required key still reaches the first projection.

V9 adds a distinct ongoing frontier. The boot policy remains first-projection
only, while `action_memory_required_keys` lets a specialized harness require up
to four dynamically revised keys in every later projection. This is intended
for targeted before/after action-state memory, not for retaining an unbounded
trajectory.

V10 exposes the existing Stone transition-hook mechanism at the standard
controller's action-selection call. A specialization may pass named
`prepare_decision` and `record_decision` functions after the existing adapters.
They default to a no-op. Direct callable arguments are intentional: Stone
callables cannot be retained inside an ordinary data record such as `options`.
The pre hook can replace only that decision's visible messages; the post hook
can retain its outcome. Runtime and Gateway policy remain authoritative, and
the hooks cannot recursively invoke model or run effects.

V11 removes that call-site-only workaround. `transition_hooks(pre=..., post=...)`
now creates a first-class task-owned value that may be bound, reused, and passed
through ordinary Stone functions. The standard controller accepts one such
value as `decision_hooks`. Captured resources must remain session-persistable,
and hook values still cannot be serialized into JSON/data records or cross the
task authority boundary.

V12 adds runtime-owned foreground completion. The model can choose one
`run_complete` action for a bounded build, install, test, download, benchmark,
or data-processing command. Stone performs the internal Gateway waits without
spending another model decision per observation and reports
`completion_waits`, the requested total timeout, and terminal output as one
action outcome. Manual `run_start`/status/wait/terminate remains available for
real overlap and services. At the language level, `await run(...)` lowers to
the same owned lifecycle.

The staged controller additionally exposes `ensure_command` for a finding that
declares `evidence.tool="ensure_command"`. It owns a check, an optional explicit
provisioning command, and a verification check as one action, returning typed
environment-transition and verification records. The action is omitted from
schemas for unrelated findings. This is an effect-structuring convenience, not
an authority bypass; the provisioning argv is still subject to Gateway policy.

In the first task-shaped rollout, a medium-reasoning model selected this action
without a repair, installed `gcc-mips-linux-gnu`, and verified the resulting
compiler path in the same action. Inspection nevertheless exhausted its budget
before two unrelated findings, so this is evidence for the action interface,
not for end-task success or the controller's acquisition schedule.

The first Gateway fixture used one `run_complete` action to create an output,
then one read and one finish action. It made no model-driven run observations,
retained verified evidence and audit state, reported zero active handles, and
rolled back cleanly. A separate Gateway canary forced a command beyond its
first 100 ms observation and confirmed that `await run` performed and traced
the required follow-up wait inside the runtime. See
[Stone Runtime-owned Run Await](STONE_RUNTIME_OWNED_RUN_AWAIT.md).

A low-reasoning Terra probe also selected `run_complete` on its first response
from the generic V12 prompt, then read the exact output and finished in three
model calls with no poll actions or schema repairs. This establishes basic
learnability.

A frozen one-cell comparison on untouched `mcmc-sampling-stan` then provided
task-shaped evidence. V11 exhausted 31 model calls after 14 manual run
observations and did not reach the verifier. V12 used 12 terminal
`run_complete` actions, zero manual observations, and three internal Gateway
waits; it finished in 14 model calls, cut total tokens from 132,395 to 68,497,
and earned official reward `1.0`. This is one stochastic matched pair, not a
broad task-level estimate. See
[Stone Runtime-owned Run Await](STONE_RUNTIME_OWNED_RUN_AWAIT.md).

The next PL slice is now available independently of this generic ReAct loop:
typed Stone workflows encode deterministic stages with explicit evidence,
bounded action attempts, and optional repair. Stage advancement is enforced by
the runtime rather than advised through a prompt or transition hook. This is
the intended substrate for the next Caffe-specific harness specialization; it
does not silently change the frozen V11 controller. See
[Stone Typed Workflows](STONE_TYPED_WORKFLOWS.md).

The next composition canary runs two V8 children from one useful frontier,
inspects both reports and traces while retained, accepts the verified branch,
discards the loser, and observes both as reclaimed. A lifecycle-parented spawn
baseline sees neither the dirty files nor hot memory. Fixture and Terra runs
pass; Terra also recovers from the specialized dispatcher's read-only
rejection. See
[Standard Agent Fork Portfolio](STONE_STANDARD_AGENT_FORK_PORTFOLIO_EXPERIMENT.md).

See `../waymark-gateway/docs/STONE_ACTION_BASELINE.md` for the comparison and
artifact paths.

Run the source checks with:

```sh
python3 host/bench/test_standard_attempt_agent.py
python3 host/bench/test_eval_standard_agent_completion_critique.py
python3 host/bench/test_eval_standard_agent_context_pressure.py
python3 host/bench/test_eval_standard_agent_run_cleanup.py
python3 host/bench/test_eval_standard_agent_task_management.py
python3 host/bench/test_eval_standard_agent_restart_reconciliation.py
python3 host/bench/test_eval_standard_agent_fork_first_context.py
python3 host/bench/test_eval_standard_completion_critic_quality.py
python3 host/bench/test_eval_standard_agent_specialization.py
python3 host/bench/test_eval_standard_agent_task_specialization.py
python3 host/bench/test_eval_standard_agent_executable_verifier.py
```
