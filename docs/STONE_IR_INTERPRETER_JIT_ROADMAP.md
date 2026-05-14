# Stone IR, Interpreter, and Future JIT Roadmap

Stone will eventually need a real IR and may later benefit from a LuaJIT-style
tracing path, but the next durable step is a small interpreted lowering layer.
The immediate goal is to reduce hot-loop AST/evaluator overhead while
preserving existing Stone semantics and fallback behavior.

The final target is a Lua-like Stone runtime:

```text
Stone source
  -> AST
  -> resolved/lowered function prototypes
  -> register VM bytecode
  -> interpreter
  -> optional trace/JIT backend later
```

In that target, the register interpreter is the normal optimized execution
substrate. The AST evaluator is the semantic reference and migration fallback,
not the permanent hot path.

The durable VM shape is drafted in
`docs/STONE_REGISTER_VM_DESIGN.md`. Future IR work should align with that
design: register slots, constant pools, blocks, typed VM values, accumulator
handles, explicit materialization boundaries, and trace plans compiled from VM
IR.

The current runtime split plan is tracked in
`docs/STONE_VM_RUNTIME_SPLIT_PLAN.md`. That plan keeps `stone_eval.rs` as the
AST evaluator and orchestration layer while moving VM runtime entry, bytecode
interpretation, and hot JSONL/native execution into `stone_vm/`.

## Architecture First, Workloads Second

The architecture is the Lua-like register interpreter. JSONL, CSV, and
jq-class transforms are important proving workloads because they expose the
current evaluator overhead clearly, but they should not define the VM shape.

Every workload-specific fast path should become one of these:

- a host iterator or host call feeding VM values
- a bytecode sequence over normal VM ops
- a guarded kernel derived from VM bytecode
- a future trace derived from VM bytecode

It should not become a permanent AST recognizer beside the runtime.

## jq-Class JSON And Table Transforms

Stone should internalize the common workload class that people currently reach
for `jq` to solve: inspect structured files, select rows, project fields,
group, sort, reduce, reshape nested JSON, and emit stable JSON or JSONL
output. This is not a goal to clone jq syntax. The goal is a model-friendly,
Python-shaped surface that lowers cleanly to Stone IR and can be optimized as a
normal execution path.

The design target is to cover the 80-90% of jq-like work that coding agents
repeatedly need on Linux workloads:

- `map`/project and select/filter over JSON arrays, JSONL rows, CSV rows, and
  lists of records
- reduce/fold over rows and nested arrays
- `group_by`, `count_by`, `unique_by`, `sort_by`, `min_by`, `max_by`, and
  top-N selection over typed keys
- JSON path operations such as get, set, delete, and recursive walk-style
  transforms
- streaming JSONL helpers that avoid full input materialization when the output
  can be produced incrementally
- stable pretty JSON output and raw/text extraction for command boundaries

The exact user-facing helper names can evolve, but the shape should remain
structured and lowerable instead of stringly typed. For example, Stone can grow
record projection helpers rather than asking the model to synthesize ad hoc
string filters:

```stone
rows = read_json("/app/users.json")
active = filter_eq(rows, "status", "active")
report = project(active, {
    "user_id": field("id"),
    "username": field("username"),
    "role_count": len_field("roles"),
    "primary_role": first_or_none("roles"),
})
write_json("/app/active_users_report.json", sort_by(report, "username"), indent=2)
```

Those helpers should lower to IR plans such as:

- `ReadJsonArray`, `ReadJsonlRows`, and `ReadCsvRows` iterator adapters
- fused scan/filter/project plans that keep row values as JSON or record views
- typed field operations such as `JsonGetStr`, `JsonGetI64`,
  `RecordProject`, `ListLen`, and `FirstOrNone`
- native `GroupBy`, `CountBy`, `UniqueBy`, `SortBy`, `TopN`, and `Reduce`
  kernels
- guarded JSON path get/set/delete operations with materialization only at
  semantic boundaries
- `WriteJson` and `WriteJsonl` sinks with fast stable formatting

This should be treated as a first-class IR target because it exercises the same
core strengths as the JSONL hot-loop work: shape inference, typed field access,
lazy row/object views, native accumulator handles, streaming input, and
explicit materialization boundaries.

External tools such as `jq` remain useful through `run(...)` for compatibility,
obscure features, and user-provided snippets. Stone should become the preferred
path for agent-generated structured JSON/table transforms without adding
benchmark-specific builtins.

## Current Direction: Lua-Style Interpreter First

The next step is not another benchmark-shaped recognizer. The current JSONL
hot path proves that compact IR, typed registers, native accumulator handles,
and explicit materialization boundaries can close much of the AST evaluator
gap. The durable goal is to mimic the part of Lua's success that matters here:
a small, predictable register interpreter that is the normal optimized
execution substrate, with specialized traces or kernels derived from that IR
only after the generic bytecode shape is clear.

The interpreter needs the ordinary Lua-like runtime pieces, adapted to Stone:

- function prototypes containing bytecode, constants, locals, and metadata
- call frames with register windows
- VM-native tagged values
- explicit host-call ops for shell capabilities
- guard and snapshot metadata for fallback
- diagnostics that identify lowering, execution, and fallback decisions

This means the existing JSONL aggregation path should be treated as a
successful specialization prototype, not as the final architecture. It can stay
as the production fast path while the generic VM grows, but new loop work
should answer:

- what generic loop/register ops does this lower to?
- what iterator adapter feeds the loop?
- which values remain VM-native, and where do they materialize?
- what guard or unsupported op triggers fallback?

For example, model-generated code often uses:

```stone
for line in open(path).splitlines():
    record = json_loads(line)
    ...
```

The long-term fix is not to keep adding special cases for that exact surface
syntax. It is to lower `open(...).splitlines()`, `read_jsonl(...)`,
`read_csv(...)`, `range(...)`, and ordinary lists into a common loop VM
interface. JSONL row views and CSV records can then be optimized as iterator
adapters and typed value ops inside the same interpreter.

## Current Implementation Boundary

Waymark currently has two VM/IR tiers:

- `stone_vm/types.rs`, `lower.rs`, `optimize.rs`, and `jsonl_match.rs` own VM
  data types, lowering, canonicalization, optimization, and JSONL pattern
  matching.
- `stone_vm/interp.rs` owns generic VM instruction mechanics.
- `stone_vm/runtime.rs` owns the generic VM runtime adapter over evaluator
  state: compile/selection entry points, local load/store, diagnostics, and
  fallback signaling.
- `try_lower_hot_loop` recognizes selected `read_jsonl(...)` aggregation loops.
- The body matcher recognizes the JSONL aggregation shapes used by current
  structured-data workloads.
- The reference VM interpreter covers that JSONL aggregation op family where
  supported.
- `try_lower_generic_loop` recognizes a compact generic loop subset over
  materialized lists, `range(...)`, `open(...).splitlines()`,
  `read_jsonl(...)`, and `read_csv(...)`.
- The generic VM compiler emits a small opcode family for `+=`, parsed numeric
  `+=`, map count increments, record-field count increments, list append, and
  unique append.
- General expression bytecode is not complete yet. The AST evaluator remains
  the semantic baseline and fallback.

Hot JSONL native execution and the Stone IR JSONL VM are still partly resident
in `stone_eval.rs`. The next runtime split is to move those row-view structs,
slot helpers, accumulator materialization, native trace execution, and hot
JSONL entry methods into a dedicated `stone_vm/jsonl_runtime.rs`-style module
without changing supported syntax.

The environment flags are development controls, not a guarantee that every loop
lowers:

- `WAYMARK_STONE_HOT_LOOP` enables hot-loop lowering attempts.
- `WAYMARK_STONE_HOT_LOOP_VM` selects the VM interpreter path where available.
- `WAYMARK_STONE_HOT_LOOP_VALIDATE_SNAPSHOT` checks fallback snapshot behavior
  for supported lowered paths.

Diagnostics should continue to distinguish "flag enabled", "loop lowered",
"VM interpreted", "fused trace used", and "missed reason".

## Motivation

Structured data workloads can spend most of their time running data-parallel
aggregation as millions of small AST/evaluator operations over generic dynamic
values. The first optimization target is therefore an interpreted hot-loop
plan:

- avoid recursive AST walking for each row, field, and tag
- keep locals and temporaries in register-like slots
- keep JSON rows and array elements as views
- use typed map accumulators where defaults give strong type signals
- materialize back to generic Stone values only at semantic boundaries

## Stages

### Stage 0: Runtime Module Separation

Before widening the VM, split the current evaluator into smaller modules so
the VM has a clean home:

- keep the AST evaluator and scope/session state in `stone_eval.rs`
- move helper registry and helper execution to `stone_helpers.rs`
- move `run(...)`, process capture, daemon lifecycle, and command explanations
  to `stone_run.rs`
- move file/data operations such as `open`, `read_json`, `read_csv`, `find`,
  `search`, and `diff` to `stone_file_ops.rs`
- create `stone_vm/` for the register VM, interpreter, lowering, guards, and
  fallback materialization

This is a refactor stage. It should not change public Stone behavior.

### Stage 1: Stable VM Function Shape

Make `StoneIrFunction`, blocks, registers, constants, accumulator handles, and
iterator sources live behind the VM module boundary. Keep the existing tests
green while preserving the AST evaluator as the reference path.

This stage should also define the call-frame model, register window ownership,
and the long-term place for locals/upvalues. Even if early bytecode only covers
loops, the function shape should be able to become the normal compiled form for
Stone functions.

### Stage 2: Move Existing Hot-Loop Paths Behind The VM Boundary

Move current JSONL and generic loop lowering/execution into the VM module
without widening supported syntax. The goal is to make existing optimized paths
look like clients of a durable VM, not peers of the AST evaluator.

The generic VM portion of this stage is mostly in place: generic VM execution
mechanics live in `stone_vm/interp.rs`, and evaluator-state adaptation lives in
`stone_vm/runtime.rs`. The remaining work in this stage is the hot JSONL native
and Stone IR runtime split described in
`docs/STONE_VM_RUNTIME_SPLIT_PLAN.md`.

### Stage 3: Iterator Adapters

Unify these loop sources behind a common iterator adapter interface:

- materialized lists
- `range(...)`
- `open(path).splitlines()`
- `read_jsonl(path)`
- `read_csv(path)`

The loop body should not care whether the current row came from a file, list,
CSV reader, or JSONL reader once the adapter yields a VM value.

Iterator adapters are host capabilities feeding the VM. They should not make
the VM a JSONL-specific engine.

### Stage 4: Expression Op Coverage

Add expression bytecode where it pays down real evaluator overhead:

- typed arithmetic and comparisons
- string operations used in filters and keys
- record/list access with typed defaults
- simple boolean branching
- bitwise integer ops only when fallback and coercion behavior are clear

The rule is to add op families with tests and fallback behavior, not to bulk
port the AST.

As coverage grows, ordinary Stone functions should compile to VM bytecode by
default, with unsupported operations falling back through explicit
materialization boundaries.

### Stage 5: Guards And Snapshots

Make guard failure explicit. A guard failure should produce enough information
to materialize live registers and resume through the AST evaluator without
changing observable behavior.

Early fallback can be coarse. Fine-grained deoptimization can wait until the VM
interpreter is stable and measured.

### Stage 6: Structured Transform Kernels

Build higher-level structured transform kernels from VM plans:

- scan/filter/project fusion
- grouped counts and reductions
- top-N and typed sort keys
- stable JSON/JSONL writers

These should be derived from lowerable Stone helpers and VM ops, not from
benchmark-specific recognizers.

### Stage 7: Future JIT Or Trace Backend

Only consider JIT or trace compilation after the register interpreter is the
normal optimized substrate and measurements show dispatch overhead is the next
real bottleneck. A future trace backend should compile from VM IR and preserve
the same guard/materialization model.

The gate for JIT work is:

- stable bytecode/function format
- broad interpreter coverage for normal Stone code
- tested guard failure and fallback materialization
- measurements showing VM dispatch or typed-op overhead is the bottleneck
- trace plans derived from VM bytecode rather than AST recognizers

## Design Rule

Every optimization proposal should answer four questions before implementation:

- what Stone syntax or helper shape lowers to this path?
- what VM ops represent it?
- where do values materialize back to generic Stone values?
- what tests prove fallback preserves AST semantics?
