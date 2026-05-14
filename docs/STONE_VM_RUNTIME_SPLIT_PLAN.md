# Stone VM Runtime Split Plan

This plan describes the next refactor after the generic VM extraction. The
goal is to make the Stone runtime split look more like Lua's durable
interpreter architecture: evaluator orchestration outside, VM execution inside
the VM module.

## Target Boundary

`stone_eval.rs` should own language evaluation and orchestration:

- program, block, statement, and expression evaluation
- scope and local binding lifecycle
- the baseline semantic fallback path
- decisions about when to attempt hot-loop lowering
- public eval entry points, diagnostics assembly, and profiler hooks
- session-state updates that are not VM-specific

`stone_vm/` should own VM representation and execution:

- VM and IR types
- lowering from loop/source shapes into VM plans
- canonicalization, optimization, and fused-kernel selection
- generic VM instruction execution
- VM runtime entry adapters that load/store evaluator state
- hot JSONL/native Stone VM execution
- guard, snapshot, and fallback materialization for optimized VM paths

The expected module map is:

- `stone_vm/types.rs`: VM/IR types and identifiers
- `stone_vm/lower.rs`: lowering and loop-shape recognition
- `stone_vm/optimize.rs`: canonicalization, optimization, and fused-kernel
  selection
- `stone_vm/jsonl_match.rs`: JSONL/native pattern matching
- `stone_vm/interp.rs`: generic VM instruction mechanics
- `stone_vm/runtime.rs`: generic VM runtime adapter over evaluator state
- `stone_vm/jsonl_runtime.rs`: hot JSONL/native Stone VM runtime execution

## Current State

The generic VM path is split:

- generic VM execution mechanics live in `stone_vm/interp.rs`
- generic VM runtime entry and evaluator-state adaptation live in
  `stone_vm/runtime.rs`
- `stone_eval.rs` calls `try_eval_for_*_generic_vm(...)` but no longer owns the
  generic VM execution loops

The hot JSONL/native path still has too much VM runtime code in
`stone_eval.rs`, including:

- `HotJsonlNativeAccumulators`
- `HotJsonlRowFields`
- `HotJsonlStringArray`
- `HotJsonlRowSlice`
- `HotJsonlNativeSlots`
- `StoneVmSlot`
- hot JSONL row access helpers
- hot JSONL accumulator load/materialization helpers
- Stone IR JSONL VM slot helpers and opcode execution
- native JSONL trace execution helpers
- hot JSONL entry methods such as `execute_hot_loop_prefix` and
  `eval_for_text_lines_hot_jsonl`

## Slice Plan

Keep every slice behavior-preserving and commit-sized.

### Slice 1: Add The Runtime Module Shell

Create `stone_vm/jsonl_runtime.rs` and wire it from `stone_eval.rs` with a
path module declaration. Move no behavior yet beyond small visibility changes
needed for compilation.

Checks:

```sh
cargo check -p waymark-runtime
cargo test -p waymark-runtime --lib generic_vm
cargo test -p waymark-runtime --lib hot_loop
```

### Slice 2: Move Hot JSONL Data Shapes

Move the data-only runtime structs/enums into `stone_vm/jsonl_runtime.rs`:

- `HotJsonlNativeAccumulators`
- `HotJsonlRowFields`
- `HotJsonlStringArray`
- `HotJsonlRowSlice`
- `HotJsonlNativeSlots`
- `StoneVmSlot`

This should be a mechanical move. Do not change layout or ownership semantics.

### Slice 3: Move Row Access Helpers

Move pure row/slot helper functions:

- JSONL row field extraction
- string/f64/i64/array required/default getters
- string-array iteration
- slot field access such as `hot_jsonl_fields` and `hot_jsonl_user`

These helpers are part of VM/native JSONL execution, not the AST evaluator.

### Slice 4: Move Accumulator Materialization

Move helpers that load and materialize native accumulator state for the hot
JSONL VM path, while keeping underlying evaluator local access explicit:

- accumulator load helpers
- nested accumulator load helpers
- snapshot materialization helpers
- row count application helpers if they are only used by the JSONL runtime

If an operation mutates evaluator locals, keep it as a method on `Evaluator`
inside the VM runtime module rather than as a free function with broad state
access.

### Slice 5: Move Stone IR JSONL VM Execution

Move the Stone IR JSONL VM execution methods:

- guard validation
- slot setters/getters
- VM function dispatch
- VM opcode execution
- VM map update helpers
- VM fallback handoff

This is the closest analogue to Lua's `lvm` execution path. It belongs under
`stone_vm/`, even when implemented as `impl Evaluator<'_>` adapter methods.

### Slice 6: Move Native JSONL Trace Execution

Move the native trace execution path:

- native trace body execution
- native op dispatch
- map add and foreach-string helpers
- tag count update helpers

This keeps specialized native kernels derived from VM/IR plans beside the VM
runtime instead of beside the AST evaluator.

### Slice 7: Move Hot JSONL Entry Methods

Move the top-level hot JSONL runtime entry methods:

- `execute_hot_loop_prefix`
- JSONL rows native/generic body entry methods
- text-lines hot JSONL entry methods
- outer JSONL file loop optimized entry if it is VM/native-specific

After this slice, `stone_eval.rs` should only decide whether a loop is a
candidate and invoke the VM runtime adapter.

### Slice 8: Final Import And Visibility Cleanup

Remove stale imports from `stone_eval.rs`, narrow any overly broad
`pub(super)` visibility introduced during the move, and check that
`stone_vm/jsonl_runtime.rs` has coherent local helper ordering.

## Visibility Rules

- Prefer private helpers inside `stone_vm/jsonl_runtime.rs`.
- Use `pub(super)` only for methods called by `stone_eval.rs` or sibling VM
  modules.
- Avoid exposing VM internals through `stone_vm/mod.rs` unless tests or public
  crate boundaries require it.
- Keep evaluator local/state mutation explicit through `impl Evaluator<'_>`
  adapter methods.

## Verification

Run focused checks after each slice:

```sh
cargo fmt -p waymark-runtime
cargo check -p waymark-runtime
cargo test -p waymark-runtime --lib generic_vm
cargo test -p waymark-runtime --lib hot_loop
```

Run the full runtime lib suite before committing larger moves:

```sh
cargo test -p waymark-runtime --lib
```

## Non-Goals

- Do not widen supported Stone syntax during this split.
- Do not change public command behavior or diagnostics schema.
- Do not replace fallback semantics.
- Do not add a JIT or tracing backend in this refactor.
- Do not move MCP, helper, file, or process-run logic into `stone_vm/`.
