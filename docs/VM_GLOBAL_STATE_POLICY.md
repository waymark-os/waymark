<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# VM Global State Policy

## Invariant

Attempt-owned memory must never become reachable from VM-global state.

Waymark LibOS deliberately runs multiple disposable Stone processes in one
safe-Rust address space. A Rust global that retains an allocation made while a
process domain is active would survive domain teardown and become a stale root.
This was the central failure mode in the earlier RustPython reset experiments.

Stone top-level bindings are not VM globals. They belong to a
`StoneProcess::ProcessImage`, persist only for that Stone process, and receive an
explicit copy/share/rebind/reset disposition when process fork is implemented.

## Compile-Time Gate

Every `waymark-runtime` build runs `waymark-global-audit` from the crate build
script. The audit parses all runtime Rust sources with `syn` and compares every
`static` and `thread_local!` declaration against
`crates/waymark-runtime/global_state.allow`.

Both an unlisted declaration and a stale allow-list entry fail compilation.
`static mut` and `lazy_static!` always fail. A policy class also constrains the
Rust type, so classifying a raw `OnceLock<String>` as `vm_once` does not bypass
the check.

Approved runtime classes are:

| Class | Required type | Contract |
| --- | --- | --- |
| `vm_atomic` | `VmAtomicU64` | Scalar identity/counter only; cannot retain a pointer. |
| `vm_frozen` | `VmFrozen<T: FreezeSafe>` | Immutable, no process pointer, resource, interior mutation, or lazy initialization. |
| `vm_once` | `VmOnce` | Retains only completion state; the closure and its captures are not stored. |
| `process_tls` | `ProcessTls<T: ProcessLocalState>` | Owned and dropped by exactly one Stone process thread. |

`FreezeSafe` and `ProcessLocalState` are unsafe marker traits because Rust
cannot derive these semantic promises. Each implementation requires a local
safety explanation. The source audit prevents bypass through an ordinary raw
global in the runtime crate.

A private `ControlScope` token is constructed by `VmSupervisor` before process
domains start. Future VM-wide lazy initialization must require that token.
Stone process code receives only `ProcessScope<'process>` and therefore cannot
initialize an approved VM-global cache through the typed API.

## Current Runtime Inventory

The allow-list contains only:

- three scalar id counters;
- one process-wide cleanup `Once` that retains no closure data;
- one immutable zero-sized filesystem adapter;
- process-thread TLS for Gateway configuration, its attached client, and a
  diagnostic byte buffer.

Gateway TLS is explicitly cleared before a fresh root begins and again before
its allocation domain is released. Thread exit remains the final TLS destructor
backstop.

## Nu Dependency Audit

Waymark pins the active Nu runtime crates to 0.112.2. On the Linux/Hermit path,
the reviewed global roots are:

| Crate | Global | Classification |
| --- | --- | --- |
| `nu-protocol` | `DEFAULT_OVERLAY_NAME: &'static str` | immutable static text |
| `nu-system` | function-local `Mutex<()>` for `umask` | synchronization only; retains no process data |
| `nu-experimental` | static experimental-option atomics and immutable marker refs | scalar VM configuration; Waymark does not mutate them per attempt |
| `nu-utils` | quoting regex cache | patched from process-wide `LazyLock` to process-thread TLS |

The resolved `nu-utils` 0.112.2 source is imported under `vendor/nu-utils` and
patched by Waymark. Its source is audited on every `waymark-runtime` build using
`nu_utils_global_state.allow`; any new vendored global fails compilation.
Waymark and Waymark LibOS both select this patched crate through
`[patch.crates-io]`.

The Windows-only Nu name cache is already TLS and is outside the initial Hermit
target. Other Nu crates containing HTTP clients, Rayon pools, SQLite anchors,
or command-level lazy regex tables are not linked into the current Stone
runtime. Adding one of those crates requires a new resolved-source audit before
it may enter the LibOS dependency graph.

Exact version pins and the committed Cargo lock protect the reviewed external
Nu inventory. Upgrading a pinned Nu crate is a global-state review event even
when its public API is unchanged.

## Dynamic Backstops

The compile gate covers source-visible roots but cannot prove behavior inside
the standard library, allocator, unsafe code, or an unreviewed dependency. The
runtime therefore still requires:

- one allocation domain and Hermit thread per Stone process;
- normal close/drop of resource-owning state before domain release;
- join before release;
- released-domain quarantine and owner tracking in diagnostic builds;
- bounded shared caches and control-domain memory plateau checks;
- poisoning rather than reuse after an exhausted domain or failed cleanliness
  check;
- repeated-root and injected-stale-root tests.

No single type, lint, or allocator supplies the guarantee. The guarantee comes
from the compile-time root inventory, typed ownership boundaries, allocation
domains, ordered teardown, and the reuse cleanliness gate together.

## Adding Global State

1. Prefer making the value a field of `StoneProcess` or `VmSupervisor`.
2. If it truly has VM lifetime, select the narrowest audited wrapper.
3. Add a safety explanation and the exact allow-list entry.
4. Add a concurrency, cleanup, or bounded-growth test appropriate to the class.
5. Never add an entry merely to make the audit pass; the policy diff is the
   security review surface.
