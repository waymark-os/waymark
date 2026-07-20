<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Process Fork And Cleanup Design

## Status

Design target, 2026-07-20. `waymark-runtime::VmSupervisor` now implements the
host-native P1 lifecycle skeleton: a fresh root `StoneGuest` per dispatch, one
worker thread, generation-tagged process id, promoted serialized result,
non-cloneable domain owner, cleanliness report, and ready/poisoned gate. The
same code compiles for Hermit allocation domains. It is not yet wired into the
LibOS vsock task server, does not host child processes, and does not yet prove
the complete production cleanliness gate.

Gateway runtime configuration and the attached shared RPC client are now
thread-local rather than process-global, with a concurrency test proving that
two Stone process threads retain distinct attempt bindings. A future lease must
install its binding inside the new process thread; process environment fallback
remains only a compatibility bootstrap path.

## Decision

Stone execution inside Waymark LibOS should be forkable and cheaply disposable
by construction:

- one MicroVM and one address space;
- trusted, safe-Rust runtime and interpreted Stone programs;
- one ordinary Hermit thread per runnable Stone process;
- Stone frames, registers, values, and continuation state represented as data,
  rather than relying on a cloneable native Rust stack;
- Hermit's scheduler supplies thread scheduling; Waymark does not add an agent
  scheduler, actor runtime, or work-stealing layer;
- immutable state is shared, mutable process state is private or copy-on-write,
  and every resource handle has an explicit fork and cleanup disposition;
- Gateway remains the authority for attempts, external resources, credentials,
  budgets, and effects.

An attempt remains the agent-OS process abstraction. A Stone process is the
Waymark LibOS controller implementation for an attempt. Local process cloning
must not become a second attempt model or expose LibOS placement in Stone
programs.

This design borrows the state-duplication, per-process kernel-state, lazy-copy,
and Zygote lessons from *µFork: Supporting POSIX fork Within a
Single-Address-Space OS* (SOSP 2025), without adopting transparent POSIX fork
or CHERI-dependent arbitrary-pointer relocation.

## Why Stone Can Do Better Than Retrofitted RustPython

The prior Helix RustPython work established useful Hermit allocation domains,
domain-owned heaps, bulk release, and released-address quarantine. It also
exposed the main danger: a supposedly task-private domain was released while
RustPython globals and runtime roots still referenced it. Quarantine converted
that ownership bug into a repeatable fault. Page-level copy-on-write was then
blocked on proving root hygiene.

The lesson is not that allocation domains failed. They correctly identified
ownership and made cleanup testable. The lesson is that transparent cloning or
bulk release of an opaque runtime object graph is unsafe without explicit root
and resource ownership.

Stone is our language and runtime. We can require from the beginning that:

- process roots are explicit;
- VM values have known representations;
- mutable sharing is visible in types and APIs;
- runtime-owned resources do not hide in the value heap;
- safe points contain no untracked native-stack continuation state;
- process teardown follows a defined order;
- debug quarantine and generation checks catch stale handles.

Do not fork the complete `StoneGuest`, Nushell engine, Rust heap, or native
thread stack. Current Nu reuse is an implementation dependency, not the
forkable process boundary.

## Trust And Isolation Model

V0 assumes the complete guest software stack is trusted safe Rust, apart from
small audited unsafe/kernel boundaries. Stone source is untrusted but executes
through the interpreter and can reach external resources only through typed
capabilities.

This permits one address space without CHERI, page-table isolation, or software
fault isolation. Branch isolation is still a required correctness property:
one Stone process must not mutate another process's private values or use its
capabilities. Safe Rust, private ownership, immutable sharing, checked handles,
and tests enforce that property.

The assumption stops holding if Waymark permits arbitrary native extensions,
JIT-generated native code, raw pointers in forkable values, or unaudited unsafe
helpers inside the guest. Such features must remain outside the guest, run
through Gateway/Linux RPC, or introduce a separately reviewed isolation model.
A future verifier may validate admitted Stone modules or compiled bytecode, but
verification is not required for the first safe-Rust process implementation.

## Placement And Thread Model

One Waymark LibOS MicroVM may host a supervised tree of Stone processes:

```text
Waymark LibOS MicroVM
  shared safe-Rust runtime and Gateway transport
  Hermit thread -> root Stone process -> root attempt
  Hermit thread -> child Stone process -> child attempt
  Hermit thread -> child Stone process -> child attempt
```

V0 uses one Hermit thread for each running Stone process. Hermit performs normal
thread scheduling. Waymark only needs:

- a configured maximum number of live Stone processes;
- process state and cancel flags;
- spawn, join, and cleanup;
- VM safe-point checks for cancellation and budgets.

There is no in-guest priority scheduler, actor mailbox scheduler, work stealing,
or agent-specific scheduling policy. A process waiting on Gateway may block its
Hermit thread. Gateway queues and provider operation handles remain responsible
for external concurrency. Long-running native builtins must be bounded or check
for cancellation; ordinary Stone loops check at VM dispatch safe points.

## Reusable MicroVM Lifecycle

The MicroVM is a reusable execution appliance. It is not the lifetime container
of its first root attempt. A small supervisor boots once and survives a sequence
of unrelated root attempt trees:

```text
booting -> ready -> leased(root attempt tree) -> draining
                   ^                         |
                   |------- ready + clean ---|
                                             +-> poisoned -> terminate
```

The supervisor owns only VM-wide control state: the listener/transport,
immutable runtime and module caches, process registry, allocation-domain
manager, metrics, and the join handles used to reap process threads. A root
`StoneProcess` is created after a dispatch arrives, exactly like any other
disposable process. The first root process must not own the listener,
supervisor, VM exit decision, shared cache lifetime, or control allocation
domain.

V0 leases one MicroVM to one independent root attempt tree at a time. Children
within that tree may run concurrently on Hermit threads, but the supervisor does
not admit the next unrelated root until the previous tree has drained and
passed the cleanliness gate. This avoids inventing an in-guest scheduler or
cross-tree resource policy. Concurrent independent trees can use different
MicroVMs from the Gateway provider pool.

The provider-internal control sequence is:

1. the VM boots with no attempt identity and reports `ready`;
2. Gateway leases it and sends a one-use root attachment capability plus the
   admitted program/task reference;
3. the supervisor creates a process domain and a fresh root process/thread;
4. the root may create a supervised attempt tree through ordinary attempt
   syscalls and local Stone process fork;
5. the supervisor freezes the root outcome, drains and joins the complete tree,
   and releases its process domains;
6. the guest emits a structured cleanliness report;
7. only a clean guest returns to `ready`; a dirty or unverifiable guest becomes
   `poisoned` and is terminated and replaced.

`ready`, `leased`, `draining`, and `poisoned` are provider lifecycle states,
not agent-visible attempt states or new Stone operations. Boot and dispatch
messages are a provider control protocol, not attempt-program IR.

The existing task-server loop is a useful transport and measurement
predecessor, but not the desired ownership model. It repeatedly mutates and
partially resets one `StoneGuest`. The reusable design instead constructs a new
root `StoneProcess`, session, resource table, and process-local adapters for
every dispatch. Only the supervisor and audited immutable/bounded shared state
survive.

## Process State

The runtime should separate shared MicroVM state, forkable Stone state, and
per-process resource state.

```text
SharedRuntime
  builtin implementations
  immutable compiled module/function cache
  Gateway transport implementation
  immutable schemas and policy descriptions

StoneProcess
  process id and attempt binding
  ProcessImage
  Continuation
  ProcessResources
  ProcessControl
  allocation domain

ProcessImage
  admitted module and immutable function prototypes
  process globals/session roots
  typed value heap
  deterministic runtime state such as logical RNG state

Continuation
  VM frames
  program counters
  register windows
  pending structured exception/control flow

ProcessResources
  Gateway channel capability
  open operation/resource handle table
  cwd and declared execution view

ProcessControl
  lifecycle state
  cancellation flag
  result state
  child supervision set
  usage counters local to the process
```

This is runtime state, not a new program IR. Stone source continues to compile
to the existing planned register-VM function representation. Making frames and
register windows explicit is what prevents semantic execution state from being
trapped in an uncloneable Rust call stack.

Gateway-owned workspace, context, artifacts, evidence, effects, capabilities,
and tree budgets are not duplicated by copying `StoneProcess`. They are forked
or rebound by the attempt syscall and referenced through the child's new
attempt binding.

### No ambient process-global task state

Multiple Stone processes in one address space cannot safely use traditional
process-global state as though each were alone. The runtime must move these
behind `StoneProcess` or a process-scoped adapter:

- cwd and environment view;
- task specification and task input;
- current attempt and Gateway channel;
- `/app`, `/tmp`, and artifact namespace selection;
- stdout/stderr and last-result buffers;
- model conversation/context view;
- random state, logical clocks, and usage counters;
- open file, operation, and child-attempt tables.

In particular, `/app` cannot be one ambient guest mount when sibling processes
refer to different Gateway transactions. Stone file builtins must resolve paths
through the calling process's attempt-bound Gateway/VFS view. A separate vsock
connection per process is an acceptable V0; transport multiplexing is an
optimization. Shared caches are allowed only when immutable or semantically
transparent and incapable of retaining process-private values.

## Leak Resistance Through Ownership And Domains

Safe Rust prevents ordinary use-after-free and data races; it does not prove
that memory or resources are eventually reclaimed. `mem::forget`, `Box::leak`,
reference cycles, detached threads, unbounded caches, globals, and handles with
missing close paths can all leak in safe Rust. Rust lifetimes alone also cannot
describe the eventual cleanup of a spawned `'static` thread.

The design therefore combines static ownership with a dynamic allocation-domain
boundary. The intended Rust concepts are:

```text
VmSupervisor
  owns ControlDomain, ProcessRegistry, bounded SharedRuntime, idle/lease state

AttemptTreeLease
  non-cloneable RAII lease for exactly one root tree
  transitions ready -> leased -> draining

ProcessDomainOwner
  non-cloneable owner of one generation-tagged allocation domain
  release consumes the owner and is legal only after the process is joined

ProcessScope<'process>
  branded token created inside the process thread
  required to allocate or mutate process-local values

ProcessLocal<'process, T> / ProcessBox<'process, T>
  cannot be returned through the thread boundary or stored in shared state

ProcessId { slot, generation } / ResourceId { slot, generation }
  values used for cross-process references instead of Rust references

ControlOwned<T> / Promoted<T>
  allocated outside the process domain; the only result class that may cross
  a process join boundary

VmShared<T: FreezeSafe>
  immutable VM-wide data that cannot point into a process domain
```

These names are design targets, not a second runtime IR. Exact APIs may change
with the allocator support available on Hermit. The important invariant is that
code running for a process receives a branded scope and cannot export a
process-local reference:

```rust,ignore
domain.run(|scope: ProcessScope<'_>| -> Result<Promoted<ProcessExit>, ProcessFault> {
    // ProcessBox<'_, T> values cannot be returned from this closure.
    run_stone_process(scope, admitted_root)
})
```

Thread spawn still requires a `'static` closure. The `ProcessDomainOwner` and
join handle are therefore created and retained by the supervisor/control
domain, while the branded lifetime is introduced inside the thread. Cross-
thread APIs carry generation-tagged handles, owned messages, or promoted
immutable results—not `&'process T`.

Allocation-domain release provides the eventual memory bound that safe Rust
does not. Process-private value storage may be reclaimed wholesale even if an
interpreter value became unreachable without running an individual destructor.
This requires a strict heap split:

```text
bulk-releasable process heap
  Stone values and chunks implementing a sealed BulkResetSafe contract
  no external effects, locks, channels, file descriptors, or required Drop

normally-dropped resource state
  Gateway channels, operation handles, files, buffers with external ownership
  closed and dropped before the process domain can be released

control/shared state
  immutable or bounded; may never retain a process-local pointer or strong ref
```

`BulkResetSafe` must be a sealed, audited unsafe marker implemented only for
runtime-owned representations. It is not a blanket promise for arbitrary Rust
types or Stone native extensions. A result needed after exit is cloned or
serialized into `ControlOwned` storage before the process thread returns.

Ordinary `Arc` cycles are forbidden in process and supervisor topology. Parent,
child, resource, and waiter relationships use an owned DAG, weak back-references,
or generation-tagged registry indices. Shared caches must be bounded, must not
hold strong process references, and must expose their retained-byte count to the
cleanliness report.

RAII remains useful but is not sufficient by itself. Dropping an
`AttemptTreeLease` begins bounded drain; it must not silently transition the VM
to `ready`. Only the supervisor's explicit cleanup and audit operation can
produce a clean report and make the VM reusable.

## Fork Dispositions

Every process-state type and runtime-value variant must declare one disposition:

```text
copy_value       copy small plain values
share_immutable  share read-only modules, constants, data, and context refs
cow_clone        share initially; clone into the child domain on mutation
rebind           create a child-specific Gateway/capability binding
reset            initialize new lifecycle, log, result, and supervision state
close_then_reset settle a local resource before the fork barrier
unsupported      reject fork while the value or resource is semantically live
```

Initial examples:

| State | Disposition |
| --- | --- |
| compiled module and constants | `share_immutable` |
| integers, booleans, small immutable records | `copy_value` |
| lists, records, strings, session globals, value-heap chunks | eager copy first, later `cow_clone` where measured |
| Gateway attempt/channel capability | `rebind` |
| logs, result, child set, signal/cancel state | `reset` |
| open Stone file object | `unsupported` or explicitly close before fork |
| running Linux/model operation | `unsupported` under `require_idle` |
| attempt result snapshot | immutable value, subject to capability policy |
| attempt scope | `reset`; never share the parent's mutable supervision object |

An ordinary Rust `Clone` implementation is insufficient as a fork contract. It
may retain shared mutable `Arc`, raw pointer, file, lock, or allocator state.
Forkable types need an audited operation that receives the destination process
domain and child capability policy.

## Safe Points

Stone process fork occurs only at a VM safe point. At a safe point:

- the current instruction boundary is explicit;
- live Stone values are reachable from process roots, frames, or registers;
- no borrowed `&mut` reference into the process heap survives;
- temporary native values needed for continuation have been written back to
  VM state;
- no nonterminal Gateway operation exists under `require_idle`;
- resource handles can be classified without inspecting an arbitrary Rust
  stack;
- cancellation and budget state can be observed.

Function calls, loop backedges, and before/after builtin dispatch are natural
safe points. A builtin cannot call process fork while it holds a mutable borrow
or partially mutates a process value.

## Controller Modes

The normal agent-programming surface remains fork plus explicit entrypoint:

```stone
child = attempt_fork(
    entrypoint="investigate",
    input={"hypothesis": hypothesis},
    scope=scope,
)
```

The child shares the admitted module and forkable process image, resets its VM
frames, and starts `investigate(input)`. Explicit input keeps branch intent easy
for language models to generate and review.

A later exact-continuation mode may clone the complete `Continuation`, including
frames, PCs, and registers. It is primarily useful for suspension/recovery or a
true continuation fork. It must never silently fall back to entrypoint restart.
Its Stone surface should be designed only after the runtime can prove exact
continuation equivalence.

Both modes use the same attempt fork frontier. Controller mode changes only
the LibOS-local process continuation behavior.

## Cross-Layer Fork Sequence

The Stone runtime and Gateway must coordinate without moving authority into the
guest:

1. Stone reaches a fork safe point and classifies local state/resources.
2. The runtime issues `attempt.fork` on the attached parent channel.
3. Gateway authenticates the parent, reserves tree resources, and creates the
   child's workspace/context/effect frontier and durable pending attempt.
4. Gateway returns a child attempt handle plus a narrow one-use or derived
   controller attachment capability.
5. LibOS creates a child allocation domain.
6. The runtime builds the child process image using the declared dispositions.
7. The child gets a fresh resource table, lifecycle state, supervision set, and
   Gateway attachment; it never receives raw host credentials.
8. LibOS starts one Hermit thread for the child process.
9. The child attaches to Gateway and begins its entrypoint or admitted resumed
   continuation.
10. Startup failure is reported as child controller failure and cleaned through
    the ordinary attempt lifecycle; it does not erase the durable fork record.

The abstract attempt API must not expose "same MicroVM" placement. Gateway's
Waymark LibOS controller provider chooses local process fork when an eligible
parent MicroVM is alive and may use another placement when it is not.

Current controller startup launches a MicroVM for each attempt. Supporting this
design therefore requires one Waymark LibOS instance to accept multiple child
controller attachments, normally as separate vsock connections or multiplexed
logical channels.

## Allocation Domains And Copy-On-Write

Reuse the existing Helix/Hermit allocation-domain substrate:

- create and bind a domain to a Hermit thread;
- allocate process-private values from domain-owned spans;
- attribute allocations to a process/domain;
- release or quarantine spans after process teardown;
- inherit or explicitly select a domain at thread creation.

Do not begin with transparent page-level CoW. First implement an eager,
semantically audited process clone and measure its copied bytes and latency.
Then introduce CoW only for controlled Stone-owned structures.

The initial CoW candidates are immutable module objects and coarse typed value
heap chunks. `Arc` or persistent structures are acceptable only when mutation
cannot affect another process. Mutation should take a child-domain token and
perform the first private clone, analogous to the earlier Helix `DomainCow<T>`
design.

Never place resource-owning values behind bulk release. Split cleanup memory:

```text
normally dropped state
  Gateway/resource handles, buffers with external ownership, locks, channels

bulk-releasable state
  audited value storage whose destructors have no non-memory side effects
```

Bulk release is an optimization after logical teardown, not a replacement for
closing resources or proving that shared roots do not point into the domain.

## Cleanup Contract

Cleanup is part of process correctness. A Stone process exits only after:

1. its result or failure is frozen;
2. unresolved supervised children are cancel-then-joined;
3. nonterminal Gateway operations are cancelled and observed terminal;
4. process-local handles and resource-owning values are normally closed/dropped;
5. the Gateway channel is closed with an explicit controller outcome;
6. the Hermit thread has returned and been joined;
7. shared/process roots into the private domain are cleared;
8. the allocation domain is released, or quarantined in diagnostic builds;
9. the Gateway attempt lifecycle finishes according to accept/discard/rollback
   policy.

The domain must never be released while its Hermit thread can still run.
Generation-tagged process and domain handles should reject stale access.
Diagnostic runs should retain released-address quarantine until conformance
tests show no cross-domain references.

MicroVM shutdown is the final containment fallback: the root controller cannot
finish cleanly while child Stone threads or live domains remain. Gateway may
terminate the entire MicroVM after a bounded cleanup deadline, then recover the
attempt tree from durable state.

### MicroVM cleanliness gate

After a root tree finishes, the supervisor produces a typed
`VmCleanlinessReport`. A reusable VM must prove at least:

- the process registry, root/child supervision sets, and join-handle set are
  empty;
- no Hermit process thread, Gateway operation, controller channel, file/resource
  handle, checkpoint, or process allocation domain remains live;
- no current attempt, transaction, cwd, `/app`, `/tmp`, artifact, task input,
  output, or model-context binding remains installed;
- diagnostic root scanning finds no control/shared reference into a released
  process generation;
- process-domain live bytes returned to zero (or the domain was quarantined and
  the VM is not reusable);
- control-domain live objects/bytes are within a recorded steady-state bound;
- every surviving shared cache is both bounded and semantically transparent;
- Gateway reports the leased attempt tree terminal and clean.

The report contains counters and reasons, not just a Boolean. A failed,
incomplete, timed-out, or unsupported check transitions the VM to `poisoned`.
Gateway must never put that VM back in its idle pool. Planned recycling after a
maximum dispatch count, age, or control-memory watermark is also allowed even
when all reports are clean.

The guarantee is therefore not “Rust cannot leak.” It is: process-local memory
has a mechanically reclaimable ownership domain; resource cleanup is explicit
and ordered; shared growth is bounded and measured; and a VM with unproven
cleanliness is destroyed rather than reused.

## Initial Implementation Plan

### P0: Forkable state audit

1. Inventory `StoneSession`, evaluator scopes, `RuntimeValue`, VM registers,
   functions, files, agent controls, attempts, and scopes.
2. Inventory ambient cwd, environment, task, output, Gateway, and filesystem
   state that must become process-local.
3. Assign a disposition to every state and value variant.
4. Reject nonforkable values explicitly; never silently omit them.
5. Add tests for mutable-alias isolation and rejected live resources.

### P1: Reusable supervisor and fresh root process

1. Introduce `VmSupervisor`, `AttemptTreeLease`, `StoneProcess`, and
   `ProcessImage` as explicit ownership boundaries.
2. Make the existing server loop construct a fresh root process/session and
   process-local adapter set for every task instead of resetting one ambient
   `StoneGuest`.
3. Run and join one root at a time; promote its result, destroy its resource
   table, and emit a typed cleanliness report.
4. Execute many unrelated roots in one host-native server and prove a stable
   process count, resource count, and memory plateau.

### P2: Hermit threads and allocation domains

1. Add small safe wrappers for create/enter/restore/release domain operations.
2. Run each Stone process on one Hermit thread in its own domain.
3. Promote results out of the child domain before release.
4. Enable quarantine and generation checks in diagnostic builds.

### P3: Eager process fork

1. Assign an explicit fork disposition to every process-state field and runtime
   value variant.
2. Implement eager clone into a fresh process image at safe entrypoint
   boundaries.
3. Add mutable-alias isolation and rejected-live-resource tests.
4. Reuse the same join, domain release, and cleanliness path as fresh roots.

### P4: Gateway attempt integration

1. Let one LibOS instance host multiple attempt controller channels.
2. Bind a forked Stone process to the child attempt returned by Gateway.
3. Prove start, join, accept/discard, cancellation, and parent-death cleanup.
4. Keep controller placement behind the Gateway provider boundary.
5. Let Gateway lease a clean VM for another unrelated root attempt after the
   first root tree exits.

### P5: Measured object/chunk CoW

1. Record eager-clone bytes, latency, and mutation coverage.
2. Share immutable compiled modules and constants.
3. Add domain-aware CoW for one typed value-heap chunk.
4. Expand only when benchmarks show a win and quarantine remains clean.

### P6: Exact continuation

1. Make full VM frames/PC/register state the normal execution path.
2. Snapshot only at declared safe points.
3. Add exact-continuation conformance and crash recovery.
4. Expose a public continuation mode only after semantic equivalence is proven.

## Validation

Correctness experiments precede broad agent benchmarks:

1. **Branch isolation:** fork a large nested value, mutate disjoint and
   overlapping paths in parent/child, and prove no implicit sharing.
2. **Inherited progress:** prepare expensive structured session state once and
   prove every child reads it without rebuilding it.
3. **Resource rejection:** fork with open file, live Linux, and live model
   handles; receive structured `fork_busy`/unsupported results with no leak.
4. **Cleanup:** repeatedly fork, cancel, join, and discard; live domains, threads,
   handles, and bytes return to a stable plateau.
5. **Quarantine:** intentionally retain a stale child pointer/handle and prove a
   diagnostic fault or generation error rather than aliasing a later process.
6. **Gateway binding:** every child syscall is charged to and mutates only its
   own attempt frontier.
7. **Failure injection:** fail domain creation, clone, thread spawn, attach, and
   controller startup; recover or clean every durable child deterministically.
8. **Performance:** compare fresh controller startup, eager process clone, and
   CoW clone for latency, copied bytes, peak memory, cleanup time, and task
   throughput.
9. **Reusable root lifecycle:** dispatch hundreds of unrelated roots through one
   VM, including success, interpreter error, cancellation, panic containment,
   and child-cleanup cases; every accepted next dispatch must follow a clean
   report and memory must remain at a bounded plateau.
10. **Poisoning:** inject a leaked thread, resource handle, process-domain root,
    and growing shared-cache entry; prove the VM refuses the next lease and is
    replaced instead of claiming readiness.

Only after these pass should Terminal-Bench evaluate whether inherited live
computational state improves task success, cost, or recovery behavior.
