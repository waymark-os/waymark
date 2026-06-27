<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Shell Status And Next Steps

This note captures the current state of Waymark Shell after the Terminal-Bench
and Gateway work, so the thread can be picked up without reconstructing the
history from run artifacts.

## Hypothesis

The benchmark work is meant to validate a shell-design hypothesis, not just
maximize a Terminal-Bench score:

> Existing coding agents should do better, with less fragile recovery behavior,
> when their shell surface is structured, typed, persistent across tool calls,
> and backed by transactional workspace/runtime primitives instead of raw
> Bash/POSIX alone.

Terminal-Bench is useful because it stresses file edits, data transforms,
Linux tools, long-running processes, services, verifiers, output formats, and
dirty workspace state. Those are exactly the places where a new agent shell
should prove value.

## Intended Architecture

The intended placement remains:

```text
existing agent, such as Codex or OpenCode
  -> Stone MCP adapter
  -> Waymark Shell / Stone runtime
  -> Gateway RPC
  -> host Gateway
       -> workspace generations and tx views
       -> linux.exec sidecars
       -> traces, artifacts, commit/rollback policy
```

Existing agents running in Docker or another conventional container should use
Stone MCP as the compatibility adapter. An agent running inside the LibOS does
not need MCP in the hot path; it should call the shell/runtime interface
directly, with Gateway RPC or vsock below that boundary. Gateway remains the
host-authority side outside the guest boundary.

MCP is the compatibility surface for evaluating existing agents. It is not the
core authority protocol and it should not be required for the LibOS-native
agent path. Gateway RPC is the internal authority protocol below the shell.

## What Looks Done Enough

The core shell language is no longer the main unknown, apart from performance
work on hot data paths.

Implemented or present in docs/runtime:

- Python-shaped Stone syntax for common LLM output patterns: conditionals,
  loops, functions, defaults, ternary expressions, comprehensions,
  try/except, augmented assignment, and set-like usage.
- Structured file/data builtins for text, JSON, JSONL, CSV, find/search, diff,
  filtering, sorting, and formatting.
- Warm shell sessions through the MCP adapter, with top-level bindings
  persisted across eval calls.
- Structured command results with stdout/stderr/status, tails, truncation
  metadata, suggested next actions, and helper observations.
- Gateway-backed runtime operations for workspace file reads/writes,
  `linux.exec`, process status/wait/terminate, daemon helpers, `ps`,
  `sysinfo`, `wait_port`, and command resolution.
- Gateway transaction affordances in Stone: `env_state`, `env_finish`,
  `env_restore`, `env_commit`, and `env_rollback`.
- Stone MCP tooling and Codex launcher/config helpers under `host/mcp/`.

This means additional Stone syntax compatibility may still help, but it is not
the obvious blocker for the next round.

## Major Remaining Gaps

### 1. LibOS / vsock placement

This is the largest architectural gap. The default shell build still reports
the vsock task server as unsupported, and the Gateway client path in the Stone
runtime has a vsock-shaped config enum but only Unix-socket transport is wired.

Until this is fixed, the intended "agent uses shell/runtime directly inside
LibOS, Gateway on host" placement has not been proven end to end.

### 2. MCP surface consolidation

There are two related adapter surfaces:

- Stone MCP: exposes the shell/language surface to existing agents.
- Gateway MCP: exposes lower-level Gateway operations.

For existing-agent evals, the primary surface should be Stone MCP backed by
Gateway RPC. For LibOS-native agents, the primary surface should be the direct
shell/runtime API. Gateway MCP is useful for diagnostics and compatibility, but
it should not become a competing agent-facing shell contract.

### 3. Fresh eval evidence

Older Stone Terminal-Bench experiments are useful history, but they predate
the latest Gateway-backed runtime, long-run/status helpers, and newer language
support. Treat those runs as historical, not as the current answer.

The next existing-agent eval should be a small fresh run through:

```text
existing agent -> Stone MCP -> Gateway-backed Stone runtime -> Gateway tx/linux.exec
```

The later LibOS eval should keep the shell/runtime behavior comparable while
removing MCP from the agent-to-shell path.

### 4. Agent guidance and tool-shape hardening

The primitives exist, but benchmark behavior depends on whether agents choose
them correctly:

- when to use Stone data/file operations instead of shell commands
- when to use `run(..., background=True)`
- when to poll with `run_status` or bounded `run_wait`
- when to inspect `env_state`
- when to call `env_finish`, restore, commit, or rollback
- when Linux sidecar work is appropriate

This is prompt/API-shape work, not script-engine performance work.

### 5. End-to-end safety claim

The shell and Gateway pieces are individually present, but the safety story
needs one clean vertical slice:

```text
agent tool call
  -> Stone operation
  -> Gateway transaction mutation
  -> diff/finish handling
  -> commit or rollback
  -> verifier result
```

That slice should be easy to run and inspect before larger benchmark runs.

## Suggested Next Milestone

Run a small, current shell-substrate eval before chasing TBv2.1 aggregate
score, then rerun a focused set of TBv2.1 failures whose traces point at shell
or runtime issues.

Recommended task set:

- one pure file task, such as `hello-world`
- one structured data task, such as CSV or JSONL aggregation
- one Linux-tool task, such as `fix-git`, `jq-data-processing`, or archive
  extraction
- one long-running/status-sensitive task if available

For each task, compare:

1. Existing agent with ordinary shell.
2. Existing agent with Stone MCP on host/container.
3. Existing agent with Stone MCP backed by Gateway transaction/runtime.
4. Later: LibOS-native agent calling the shell/runtime directly.

Record not only pass/fail, but also:

- number of Stone eval calls
- number of `run` calls
- background/status/wait usage
- `env_state` / `env_finish` usage
- dirty transaction outcome
- syntax/runtime/tool-protocol failure class
- whether Linux fallback was necessary

## TBv2.1 Failure Lessons

The combined Gateway-backed TBv2.1 run landed around the low 60s pass rate.
Several initial failures were later shown to be infrastructure or run-control
issues rather than irreducible model failures. Targeted reruns passed or
improved tasks such as `crack-7z-hash`, `qemu-alpine-ssh`, `tune-mjcf`, and
`fix-ocaml-gc`.

The remaining failed traces suggest these priority classes.

### 1. Long-running command control

Examples: `caffe-cifar-10`, `extract-moves-from-video`, and `train-fasttext`.

The traces show the agent launching expensive builds, OCR loops, or training
runs, receiving `still_running` / `timed_out` results, and then losing the
outer benchmark budget before a clean repair or finalization loop. This is a
shell/runtime issue more than a language issue.

Needed improvements:

- expose the remaining task deadline inside the shell/tool result
- make long commands opt into background execution earlier and more reliably
- cap wait chunks and return structured progress summaries after each wait
- surface stdout/stderr tails, elapsed time, and recommended next action
- trigger a pre-deadline wrap-up path when a run is still active
- preserve enough run state that an outer timeout does not erase useful
  progress diagnostics

### 2. Verifier-feedback and dry-run repair loops

Examples: `pytorch-model-recovery`, `video-processing`,
`portfolio-optimization`, and parts of `torch-tensor-parallelism`.

The agent often made a plausible local self-check pass, but the hidden verifier
failed on a different contract, signature, artifact, or input range. A raw
shell gives little structure for this. The shell hypothesis should be tested
with a safe verifier/dry-run loop that can run checks against a temporary
transaction and return structured failures without polluting the main task
state.

Needed improvements:

- add a Gateway-backed verifier dry-run API for benchmark harnesses
- run verifiers or local tests in temporary transaction views by default
- return structured failure summaries, not only full logs
- make repair loops explicit: test, inspect failure, patch, retest, then
  commit or restore
- keep this as a separate eval arm when official benchmark rules disallow
  hidden-verifier feedback

### 3. Workspace, ownership, and install-path hygiene

Examples: `build-pov-ray`, `make-doom-for-mips`, and Git-heavy tasks.

Some traces show root-owned extracted trees, permission errors, missing Git
identity or safe-directory config, or installed artifacts not ending up where
the verifier expects them. These are exactly the kinds of host/transaction
friction the Gateway should make visible and repairable.

Needed improvements:

- report root-owned or non-writable paths in `env_state` / finish summaries
- provide a safe ownership-normalization operation for transaction views
- default Git identity and safe-directory config inside tool sidecars
- add artifact checks for expected executable/module paths
- make permission and install-path warnings first-class observations

### 4. Domain helpers for common benchmark traps

Examples: `sanitize-git-repo`, `portfolio-optimization`, and module/build
tasks.

The failed `sanitize-git-repo` trace missed remaining AWS credentials after a
custom scan reported clean. This points at a small set of shell helpers that
would reduce fragile one-off scripts without making the shell domain-specific
to Terminal-Bench.

Useful helpers:

- `secret_scan` with literal-value, entropy, and common credential-pattern
  checks, plus redacted reports
- `python_module_check` for importability and extension-build diagnostics
- `artifact_check` for expected paths, executability, file type, and dynamic
  library resolution
- `command_available` / `resolve_command` guidance before a plan depends on a
  missing binary
- compact test-failure summaries for `pytest`, `unittest`, and simple shell
  verifier scripts

### 5. Lower-priority model/task reasoning misses

Examples include some algorithmic or scientific tasks where the trace mostly
shows an incorrect solution rather than shell friction. These should not drive
the next shell milestone unless they expose a repeated missing observation or
repair primitive.

## DeltaBox Paper Lessons

DeltaBox is aimed at the same broad pressure point: stateful agents need cheap
checkpoint/rollback when they explore multiple action paths. Its concrete
mechanisms differ from Waymark's intended design. DeltaBox adds OS mechanisms
for change-based filesystem and process checkpoint/restore: DeltaFS freezes the
current writable layer and inserts a fresh one, while DeltaCR restores process
state by forking from a frozen template process. The paper reports
millisecond-level checkpoint/rollback by coupling those mechanisms and hiding
checkpoint work under model latency.

Waymark should not copy the main implementation shape directly. A modified
in-guest overlayfs and transparent process rollback make sense for DeltaBox's
MCTS/RL sandbox target, but Waymark's stronger bet is that the host Gateway
owns workspace state, generations, credentials, verifier orchestration, and
artifact policy. That host-owned, NFS-like boundary is the better fit for
auditable agent execution and LibOS placement.

The useful lessons to import are at the abstraction and scheduling layer.

## Current Code Audit Against DeltaBox Lessons

Waymark already has the basic checkpoint-shaped primitives; they were hidden
from the blind Terminal-Bench run to isolate the shell surface.

Present today:

- `env.snapshot` opens a Gateway transaction from the workspace's current
  generation.
- `env.restore` restores selected paths or the whole transaction back to the
  base generation.
- `env.rollback` closes and removes the transaction.
- `env.commit` publishes a new immutable generation.
- Gateway MCP and Stone expose restore/rollback in the full surface.
- Blind eval mode intentionally hides `env_state`, `env_diff`, `env_restore`,
  and `env_rollback`.
- MCP dry-run opens a fresh transaction, runs a Stone eval/call, then rolls the
  transaction back.
- Gateway tracks long-running `linux.exec` jobs by transaction id and clears
  them when a transaction is closed or cleaned up.

What is not present yet is a first-class branch/checkpoint system:

- a transaction cannot currently be forked from another dirty transaction
- dry-run branches are one-off helper behavior, not durable checkpoint-tree
  nodes with parent/child metadata
- restore always targets the transaction's base generation, not an arbitrary
  checkpoint inside the same exploration
- trace records are operation logs, not a reachability graph of checkpoints,
  verifier attempts, and branch outcomes
- copy fallback and commit materialization use full recursive copies rather
  than reflink/sparse-delta snapshots
- live-run rollback policy exists internally as cleanup, but is not exposed as
  checkpoint semantics or metrics
- there is no async checkpoint/diff preparation under model latency
- TB harness output does not report checkpoint count, branch count, rollback
  latency, storage growth, or work hidden under model latency

So the correction is: the next step is not to invent rollback from scratch. It
is to promote existing transaction/restore/dry-run mechanisms into explicit
agent-search primitives.

### 1. Make checkpoint trees explicit

DeltaBox treats agent exploration as a tree of related states, not a flat list
of scratch directories. Waymark should expose the same concept at the Gateway
generation/transaction layer:

- `checkpoint(parent_tx)` creates a named child state
- `restore(checkpoint_id)` switches a task view back to that state
- `fork(checkpoint_id)` creates a sibling exploration branch
- trace metadata records parent, children, verifier result, token/time cost,
  and final artifact status

This is directly relevant to evaluating the shell design: Stone should make
branch, test, rollback, and retry a normal workflow instead of forcing agents
to emulate it with Git, copies, or ad hoc scripts.

### 2. Optimize deltas before process memory

DeltaBox's key observation is that adjacent agent states differ only slightly.
Waymark can apply that immediately to workspace storage without adopting
DeltaCR:

- use reflink/copy-on-write where available for transaction snapshots
- keep copy-backend diffs sparse and file-level, not full-tree copies
- track whiteouts, binary changes, and large-file rewrites explicitly
- measure checkpoint latency, rollback latency, write amplification, and
  cumulative storage per task

This is the paper lesson most directly aligned with the Gateway-owned
workspace service.

### 3. Couple filesystem state with live run state

DeltaBox couples filesystem and process state to avoid divergence. Waymark may
not need transparent process memory rollback yet, but it should avoid restoring
files while leaving stale live commands, daemons, ports, or cached status
behind.

Gateway checkpoints should therefore record:

- active `linux.exec` runs and daemon handles
- open service ports and wait conditions
- process groups that must be terminated, preserved, or explicitly detached
- runtime/session variables that are safe to replay versus unsafe to replay

The first implementation can make rollback terminate or invalidate live runs
rather than restore memory. The important bit is that this policy is explicit
and visible to the agent.

### 4. Hide snapshot work under model latency

DeltaBox schedules checkpoint work while the model is thinking. Waymark can do
the same at the Gateway/shell layer:

- after each mutating Stone operation, queue an async lightweight checkpoint
- return the last durable checkpoint id in the next tool result
- precompute diffs and env warnings while the model is generating the next
  action
- make `env_finish` cheap because most expensive inspection was already done

This matters for TB-style evals because it gives the shell better safety
without putting every checkpoint on the critical path.

### 5. Add reachability-aware cleanup

DeltaBox has to manage many short-lived branches. Waymark will face the same
problem once verifier dry-runs and branch repair loops are common.

Gateway should track which generations/checkpoints are reachable from:

- committed generations
- active sessions
- pending verifier runs
- retained traces or artifacts

Everything else should be eligible for bounded cleanup based on size, age, and
debug retention policy.

### 6. Evaluate checkpoint behavior as a first-class benchmark axis

The paper's strongest evaluation lesson is not only "rollback faster"; it is
that faster rollback changes what search strategies are practical under a fixed
time budget. Waymark evals should report:

- checkpoint and rollback latency distribution
- storage growth per branch
- number of explored branches per task
- verifier/test attempts per final answer
- time hidden under model latency
- pass rate at fixed wall-clock and token budgets

This gives a cleaner test of the shell/runtime hypothesis than aggregate
TBv2.1 score alone.

## Paper-Specific Improvement Plan

Implemented first slice, 2026-06-26:

- Gateway now has local-library and CLI support for `checkpoint`,
  `fork --checkpoint`, and `restore-checkpoint`.
- Checkpoints persist a transaction's current workspace root, environment, and
  metadata under Gateway-owned storage.
- Forking a checkpoint opens an independent transaction from the checkpoint's
  original base generation, then restores the checkpoint state into that tx.
- Restoring a checkpoint into an existing tx reports how many live Gateway
  `linux.exec` runs were terminated/invalidated by tx cleanup.

Implemented second slice, 2026-06-26:

- Gateway protobuf/RPC now exposes `env.checkpoint`, `env.fork`, and
  `env.restore_checkpoint`.
- The Rust Gateway client has typed methods for checkpoint, fork, and
  restore-checkpoint.
- The CLI-backed Gateway MCP server exposes `env_checkpoint`, `env_fork`, and
  `env_restore_checkpoint`.
- The Waymark-side Gateway MCP adapter forwards `checkpoint`, `fork`, and
  `restore_checkpoint` over Gateway RPC for existing Docker/container agents.
- The Rust RPC restore/rollback smoke now exercises checkpoint creation,
  restore-checkpoint, and forked branch rollback.

Implemented third slice, 2026-06-26:

- Waymark Shell / Stone now has native `env_checkpoint(reason="...")`,
  `env_fork(checkpoint)`, and `env_restore_checkpoint(checkpoint)` builtins.
- The builtins route through the Gateway Rust client, so Docker agents using
  Stone MCP and future LibOS/direct-shell agents can use the same checkpoint
  API without dropping to Gateway MCP.
- Stone MCP help/signature metadata advertises the new builtins in the full
  surface and hides them in blind mode with the other restore/rollback/state
  controls.
- Waymark command help documents the checkpoint builtins.

Implemented fourth slice, 2026-06-26:

- Gateway checkpoints now support Git-object `git-worktree` transactions.
- Checkpoint creation stores the visible worktree contents without `.git`.
- Restore/fork preserves the detached worktree `.git` linkage, clears only the
  visible workspace files, and copies checkpoint contents back in.
- Tests cover restore and fork from a dirty Git-object worktree checkpoint,
  including continued Git status functionality after restore/fork.

Implemented fifth slice, 2026-06-26:

- Gateway transaction metadata now records `parent_checkpoint` for forked
  transactions.
- Checkpoints created from forked transactions retain that parent checkpoint,
  making the checkpoint graph inspectable instead of flat.
- Checkpoints record `storage_bytes` for the stored workspace root.
- Gateway RPC and Stone records expose `parent_checkpoint` and `storage_bytes`
  on checkpoint results.
- Tests and RPC smoke coverage assert parent propagation and nonzero checkpoint
  storage metrics.

Implemented sixth slice, 2026-06-26:

- Gateway can list checkpoints with optional workspace filtering.
- Gateway can discard checkpoints, with child-checkpoint protection unless
  forced.
- Checkpoint metadata includes `status` and `retention` fields with current
  defaults of `active` and `auto`.
- Gateway CLI/RPC/MCP expose checkpoint list/discard operations.
- Waymark Shell exposes `env_checkpoints()` and
  `env_discard_checkpoint(checkpoint, force=False)`.
- RPC smoke coverage verifies checkpoint listing, child-protected discard, and
  forced discard.

Implemented seventh slice, 2026-06-26:

- Gateway has `env.run_checkpoint`, a verifier-friendly branch primitive.
- The operation forks a checkpoint into an ephemeral transaction, runs a Linux
  command in that fork, captures command output and the branch diff, then rolls
  the fork back.
- The first version intentionally requires an image-backed execution path and
  rejects attached containers, because an existing attached container is already
  mounted to a different transaction view.
- Gateway CLI, protobuf/RPC, Rust client, Python JSON-RPC bridge, and Gateway
  MCP expose the operation.
- Docker-backed RPC smoke coverage verifies output capture, diff capture,
  rollback closure of the ephemeral transaction, and no leakage into the source
  transaction.

Implemented eighth slice, 2026-06-26:

- Gateway MCP `stone_call` can now use checkpoint-backed dry-runs:
  pass `dry_run: true` and `checkpoint: ...` to route through
  `env.run_checkpoint`.
- The older workspace snapshot dry-run remains as a fallback when no checkpoint
  is supplied.
- The sibling `../waymark` Gateway MCP forwarder exposes the same behavior over
  the protobuf RPC CLI.
- Checkpoint-backed `stone_call` dry-run rejects attached containers for now,
  matching `env.run_checkpoint`; image-backed execution is the supported path.

Implemented ninth slice, 2026-06-26:

- The containerized Codex/Gateway benchmark harness can now create a
  post-agent checkpoint and run diagnostic verifier commands through
  `env.run_checkpoint`.
- New harness flags:
  `--checkpoint-verifier-command`, `--checkpoint-verifier-image`, and
  `--checkpoint-verifier-timeout-ms`.
- Verifier commands are diagnostic-only in this slice: they are recorded in
  `summary.json` but do not alter the strict harness `ok` calculation.
- `summary.json` now includes `checkpoint_metrics`,
  `checkpoint_verifier_checkpoint`, and `checkpoint_verifiers`.
- Trace-derived checkpoint metrics count explicit checkpoint calls,
  checkpoint-run calls, and checkpoint-backed dry-run calls.
- This is not yet the full official Terminal-Bench verifier path because
  verifier test-volume mounts are not part of `env.run_checkpoint` yet.

Implemented tenth slice, 2026-06-26:

- `linux.exec` and `env.run_checkpoint` support explicit read-only verifier
  mounts via `--read-only-mount HOST_PATH:CONTAINER_PATH`.
- The protobuf/RPC API, Rust client, Gateway MCP direct server, sibling
  Gateway MCP RPC forwarder, and containerized Codex/Gateway harness all pass
  read-only mounts through.
- `--checkpoint-verifier-mount HOST_PATH:CONTAINER_PATH` adds read-only mounts
  for diagnostic checkpoint verifier commands in the benchmark harness.
- Gateway validates that host paths are absolute and exist, container paths are
  absolute, and read-only mounts do not replace the workspace mount.
- Read-only mounts are image-backed only. They are rejected for attached
  containers and for transactions whose persistent image-backed exec container
  has already started without those mounts.

Implemented eleventh slice, 2026-06-26:

- The containerized Codex/Gateway harness has an opt-in Terminal-Bench
  checkpoint verifier mode:
  `--tbench-checkpoint-verifier --tbench-task-dir TASK_DIR`.
- The mode auto-discovers `TASK_DIR/tests` and `TASK_DIR/run-tests.sh`, mounts
  them read-only at `/tests` and `/run-tests.sh`, and runs
  `TEST_DIR=/tests bash /run-tests.sh` inside the checkpoint branch.
- The existing TB v1 development wrapper now enables this checkpoint verifier
  and passes the task directory to the generic harness.
- Generic `--checkpoint-verifier-command` remains available and can be combined
  with the TB verifier plan.
- A no-agent hello-world smoke passed:
  `target/runs/tbench-checkpoint-verifier-hello-smoke-2/summary.json` records
  one Terminal-Bench checkpoint verifier run, status 0, and branch rollback.
- The smoke found and fixed a harness parser mismatch: direct Gateway CLI
  `env checkpoint` prints a bare checkpoint id, while RPC rendering prints a
  tabbed `checkpoint` field. The harness now accepts both shapes.

Implemented twelfth slice, 2026-06-26:

- The Terminal-Bench checkpoint verifier now supports both source layouts:
  original Terminal-Bench tasks with `run-tests.sh`, and Harbor-converted tasks
  with `tests/test.sh`.
- Harbor remains the official TBv2.1 environment lifecycle and scorer. The
  Gateway verifier is diagnostic-only: it forks an ephemeral checkpoint branch,
  runs the test script with read-only `/tests` and `/run-tests.sh` mounts, then
  rolls the branch back.
- `GatewayCodexHarborAgent` has an opt-in `tbench_checkpoint_verifier` flag and
  optional `tbench_task_dir`. If no task dir is supplied, it resolves Harbor's
  canonical `task.paths.task_dir` during `prepare_environment_config`.
- The Harbor adapter forwards `--tbench-checkpoint-verifier --tbench-task-dir`
  to the generic Gateway/Codex harness, so TBv2.1 Harbor trials can collect the
  same checkpoint-verifier metrics as the direct TB v1 development wrapper.
- Harbor verifier scripts can write `/logs/verifier/reward.txt` and still exit
  zero, so the diagnostic verifier now treats reward `1` as the pass signal
  when that file exists and falls back to script exit status otherwise.
- Checkpoint-verifier Harbor runs use the blind Stone surface so agents cannot
  close the transaction through `env_commit`/`env_finish` before the harness
  forks a checkpoint. The harness runs verifier branches before final
  finish/reminder handling.
- Live smoke passed:
  `target/runs/harbor-hello-checkpoint-gpt55-v5/waymark-harbor-hello-checkpoint-gpt55-v5`.
  Harbor reward was 1.0; Gateway summary recorded one Terminal-Bench checkpoint
  verifier run, status 0, reward-aware command, read-only Harbor `tests/test.sh`
  mounts, and branch rollback.

Focused Harbor/TBv2.1 subset, 2026-06-26:

- `sanitize-git-repo`:
  `target/runs/harbor-sanitize-git-repo-checkpoint/waymark-harbor-sanitize-git-repo-checkpoint`.
  Harbor reward was 0.0 with no harness exception. Gateway summary was `ok:
  true`, verifier planned/ran once, verifier failed with status 1, and the
  checkpoint branch rolled back. Failure was task-result aligned, not a Gateway
  crash: two secret-replacement assertions failed, and the verifier also hit a
  Git dubious-ownership/safe-directory error while checking unchanged files.
- `pytorch-model-recovery`:
  `target/runs/harbor-pytorch-model-recovery-checkpoint/waymark-harbor-pytorch-model-recovery-checkpoint`.
  Harbor reward was 0.0 with no harness exception. Gateway summary was `ok:
  true`, verifier planned/ran once, verifier failed with status 1, and the
  checkpoint branch rolled back. Four of five verifier tests passed; the loss
  test failed because the TorchScript model forward accepted only `src`, while
  the verifier calls `agent_model(src_sequences, tgt_sequences)`.
- These two runs validate the Harbor adapter path on real converted tasks:
  official Harbor reward and Gateway checkpoint verifier agree, verifier
  branches roll back, and failures are now inspectable from summary/traces.

1. Formalize the vocabulary in the protocol and docs:
   `env.snapshot` is "open tx from generation"; a new `env.checkpoint` records
   the current tx state; `env.fork` opens a tx from a generation or checkpoint;
   `env.restore_checkpoint` restores a tx to a named checkpoint.
2. Add checkpoint-tree metadata beside current tx/generation metadata:
   parent id, root generation, workspace, tx id, reason, trace id,
   verifier/test outcome, created/closed timestamps, storage bytes, and
   retention policy.
3. Implement `fork from tx` using the cheapest available backend:
   Git worktree for git-object generations, overlay lower/upper layering where
   available, reflink/sparse copy for filesystem fallback, and full copy only
   as the portable last resort.
4. Make live-run semantics explicit. On checkpoint restore or branch discard,
   Gateway should report which `linux.exec` jobs were terminated, detached, or
   invalidated. The current tx cleanup behavior is a base to expose, not a
   complete contract.
5. Continue converting MCP/Stone dry-run from one-off hidden transactions into
   visible checkpoint branch operations. Gateway MCP `stone_call` supports this
   when a checkpoint is supplied; Stone-native script dry-run and default
   workspace dry-run still use the older temporary transaction path.
6. Build benchmark verifier dry-run on top of `env.run_checkpoint`. A verifier
   attempt now has a diagnostic harness hook, metrics, and read-only test-volume
   support for image-backed checkpoint runs. The TB v1 development wrapper and
   Harbor/TBv2 adapter both feed the same generic verifier; remaining work is
   optional retained branches for debugging.
7. Add async checkpoint/diff preparation after mutating operations so model
   latency can hide Gateway bookkeeping and make `env_finish` cheap.
8. Add checkpoint metrics to TB harness output before using the mechanism for a
   larger score run: branch count, checkpoint/rollback latency, storage growth,
   terminated live runs, verifier attempts, and retained/discarded branches.
9. Defer DeltaCR-like process memory restore until filesystem and live-run
   semantics are correct. If later needed, integrate it behind the same Gateway
   checkpoint abstraction instead of exposing it as the agent contract.

## Focused Improvement Plan

1. Build a trace classifier for the TBv2.1 run artifacts. It should parse
   result JSON and MCP traces and classify failures as long-running,
   verifier-feedback-needed, permission/ownership, missing-tool, artifact-path,
   or likely model/task reasoning.
2. Implement the long-running command policy in Stone/Gateway runtime:
   deadline-aware result metadata, bounded waits, periodic status summaries,
   and pre-deadline wrap-up guidance.
3. Add a benchmark-harness verifier dry-run path over temporary Gateway
   transactions. Keep this separate from strict pass@1 scoring so the eval can
   compare ordinary shell, Stone MCP, and Stone MCP plus structured feedback.
4. Harden workspace hygiene: Git defaults, ownership warnings, install-path
   checks, and structured dirty-transaction finish summaries.
5. Add the small generic helper set: secret scanning, Python module/artifact
   checks, command availability, and compact test failure parsing.
6. Rerun a focused TBv2.1 subset before another full sweep:
   `caffe-cifar-10`, `train-fasttext`, `extract-moves-from-video`,
   `build-pov-ray`, `sanitize-git-repo`, `pytorch-model-recovery`,
   `video-processing`, `portfolio-optimization`, and `mcmc-sampling-stan`.
7. Compare two current surfaces:
   Docker/container existing agent through Stone MCP, and later LibOS-native
   agent through direct shell/runtime calls. Gateway remains the authority in
   both cases.

## Practical Pickup Order

1. Add or refresh a canonical smoke for Stone MCP using Gateway-backed runtime.
2. Make the eval prompt/tool docs prefer Stone MCP as the primary shell surface
   for Docker/container agents, and direct shell/runtime calls for LibOS agents.
3. Run a small fresh Terminal-Bench subset through Codex or OpenCode.
4. Classify failures as shell-language, tool-guidance, Gateway/runtime,
   LibOS-placement, or model/task reasoning.
5. Wire vsock/LibOS placement only after the host/container path is stable
   enough to preserve behavior across placement changes.

## Non-Goals For The Immediate Step

- Do not turn Stone into Python.
- Do not make MCP the core runtime.
- Do not route all work through raw Bash for benchmark convenience.
- Do not chase full TBv2.1 score before the shell-substrate slice is clear.
- Do not add heavyweight sandbox logic to the default shell build.
