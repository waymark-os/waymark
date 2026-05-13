# Stone Register VM Design

Stone should move from a recursive AST evaluator to a compact register VM. The
end state is that Stone source compiles to VM functions as the normal runtime
path. AST evaluation remains the compatibility and reference path during
migration, but it should not remain the center of hot execution.

The goal is not to clone Lua, but to borrow the parts that made Lua's
interpreter a durable execution substrate: compiled function prototypes,
register slots, constant pools, call frames, tight bytecode dispatch, and a
runtime value model designed for the language.

Waymark can still use host process boundaries, POSIX commands, structured I/O,
and MCP adapters around Stone, but those adapters should not define the hot
Stone execution model. Stone has its own syntax and should have its own VM.

## Lua Interpreter Success Pattern

The useful Lua lesson is not "add a JIT first." It is that a small bytecode
interpreter can be fast, simple, and durable when the language runtime is built
around it:

- source lowers once into compact function prototypes
- locals are numbered registers, not hash lookups
- constants are pooled
- loops are bytecode control flow, not recursive tree walking
- common operations have direct opcodes
- generic dynamic behavior still exists, but it is not the default hot path

Stone should follow that shape. Specialized fused traces, including the current
JSONL aggregation backend, are valuable only when they are derived from the VM
program or guarded as an explicit backend choice. The VM interpreter should be
the semantic center of optimized Stone execution.

The long-term execution pipeline should look like this:

```text
Stone source
  -> AST
  -> resolved/lowered function prototypes
  -> register VM bytecode
  -> interpreter
  -> optional trace/JIT backend later
```

The AST remains valuable for parsing, diagnostics, and fallback. The compiled
function prototype is the durable execution artifact.

## Design Goals

- Compile Stone source to a stable intermediate function form before execution.
- Execute hot paths from numeric registers, not string-keyed scope lookup.
- Keep constants in pools, not embedded expression trees.
- Represent calls with explicit frames and register windows.
- Make compiled function prototypes cacheable artifacts when the surrounding
  runtime can safely reuse them.
- Keep typed runtime values in registers instead of eagerly materializing JSON
  or process-boundary values.
- Treat records, lists, JSON views, strings, numbers, and accumulator maps as
  Stone VM values with explicit conversion boundaries.
- Keep blocks, guards, and materialization points explicit from the first VM
  shape, even if early lowering only uses a small subset.
- Let future tracing or JIT work start from VM bytecode, not from the AST.

## Non-Goals

- Do not map every Stone operation immediately.
- Do not build a second generic AST interpreter with renamed nodes.
- Do not require process-boundary values for internal arithmetic, JSON field
  access, loop iteration, or accumulator updates.
- Do not add native-code JIT before the register interpreter is stable and
  measured.
- Do not make MCP the core runtime. MCP remains an adapter over the same Stone
  execution path used by the CLI.
- Do not let structured shell builtins define the VM. `run`, file APIs,
  helpers, and MCP-facing operations are host capabilities callable from VM
  bytecode, not the VM core.

## Function Prototype

Stone should compile source into function prototypes containing bytecode,
constants, locals, and nested functions:

```rust
struct StoneIrFunction {
    registers: u32,
    constants: Vec<StoneConst>,
    locals: Vec<StoneLocal>,
    blocks: Vec<StoneBlock>,
    entry: BlockId,
}
```

This makes the compiled form the normal execution unit. The AST remains a
frontend artifact and fallback reference during migration, not the long-term
runtime representation.

## Register Slots

Instructions read and write numeric registers:

```text
r0 = row
r1 = JsonGetStrDefault r0, k_user, k_unknown
r2 = JsonGetF64Default r0, k_amount, 0.0
MapAddF64 acc0, r1, r2
```

The VM must not repeatedly resolve `"row"`, `"user"`, or `"amount"` through a
scope map inside hot loops. Local names are metadata mapped to slots at compile
time.

Each call frame should own a register window for its function. Function calls
pass arguments into the callee's registers and return values through explicit
destination registers or a small return-value convention. This keeps ordinary
Stone calls on the same execution substrate as loops and expression bytecode.

If Stone closures remain part of the language long term, captured bindings
should become explicit upvalues or environment cells attached to function
prototypes. They should not force every local lookup back through a string-keyed
scope map.

## Constant Pool

Strings, field names, static paths, and larger literals should live in a
constant pool:

```rust
enum StoneConst {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    EmptyList,
    EmptyRecord,
}
```

An instruction references `ConstId`, not a syntax node.

## Core IR Shape

Use basic blocks from the beginning:

```rust
struct StoneBlock {
    ops: Vec<StoneOp>,
    terminator: StoneTerminator,
}

enum StoneTerminator {
    Return { src: Reg },
    Jump { target: BlockId },
    Branch { cond: Reg, then_block: BlockId, else_block: BlockId },
    ForEach {
        iterator: Reg,
        item: Reg,
        body: BlockId,
        done: BlockId,
    },
    Guard {
        kind: GuardKind,
        snapshot: SnapshotId,
        success: BlockId,
        failure: FallbackTarget,
    },
}
```

Even if the first implementation only uses a row block and a tag block, blocks
and terminators prevent the VM from growing into a flat special-case trace
format.

## General Loop Model

The VM must optimize loops as loops, not just one JSONL surface form. The
iterator source should be an adapter that yields VM values into the same
`ForEach` control-flow shape:

```rust
enum StoneIteratorSource {
    Range { start: Reg, stop: Reg, step: Reg },
    List { src: Reg },
    OpenSplitlines { path: Reg },
    JsonlRows { path: Reg },
    CsvRows { path: Reg },
}
```

Once an iterator yields a value, the body runs as normal VM blocks. This lets
all of these common Stone shapes share the same interpreter machinery:

```stone
for row in read_jsonl(path):
    ...

for line in open(path).splitlines():
    row = json_loads(line)
    ...

for row in read_csv(path):
    ...

for item in values:
    ...
```

Specialization may recognize that `open(...).splitlines()` followed by
`json_loads(line)` can use a JSONL row-view iterator, but that should be an
iterator-adapter optimization feeding the general loop VM, not a separate
JSONL-only loop architecture.

## Initial Op Families

Only implement op families when they serve a measured hot path or a required
materialization boundary.

### Locals And Constants

```rust
LoadLocal  { dst: Reg, local: LocalId }
StoreLocal { local: LocalId, src: Reg }
LoadConst  { dst: Reg, constant: ConstId }
Move       { dst: Reg, src: Reg }
```

### JSON Views

```rust
JsonGetValue        { dst: Reg, object: Reg, key: ConstId }
JsonGetStrDefault   { dst: Reg, object: Reg, key: ConstId, default: ConstId }
JsonGetF64Default   { dst: Reg, object: Reg, key: ConstId, default: f64 }
JsonGetI64Default   { dst: Reg, object: Reg, key: ConstId, default: i64 }
JsonGetArrayDefault { dst: Reg, object: Reg, key: ConstId }
JsonEachStrArray    { array: Reg, item: Reg, body: BlockId }
```

These should operate directly on Stone JSON view values and borrowed bytes
where possible.

### Typed Accumulators

```rust
MapAddF64      { map: AccId, key: Reg, value: Reg, append: Option<AccId> }
MapAddI64      { map: AccId, key: Reg, value: Reg, append: Option<AccId> }
MapAddI64Const { map: AccId, key: Reg, value: i64, append: Option<AccId> }
```

Accumulator handles are VM objects. They materialize to Stone records and lists
only at explicit boundaries.

### Arithmetic And Comparison

Add typed expression ops after the accumulator and compact generic loop paths
are stable:

```rust
AddI64    { dst: Reg, lhs: Reg, rhs: Reg }
AddF64    { dst: Reg, lhs: Reg, rhs: Reg }
BitAndI64 { dst: Reg, lhs: Reg, rhs: Reg }
BitOrI64  { dst: Reg, lhs: Reg, rhs: Reg }
BitXorI64 { dst: Reg, lhs: Reg, rhs: Reg }
ShlI64    { dst: Reg, lhs: Reg, rhs: Reg }
ShrI64    { dst: Reg, lhs: Reg, rhs: Reg }
BitNotI64 { dst: Reg, src: Reg }
CmpIn     { dst: Reg, key: Reg, map: AccId }
CmpEq     { dst: Reg, lhs: Reg, rhs: Reg }
```

Avoid broad generic arithmetic until typed ops and fallback behavior are clear.
The Stone AST evaluator supports integer bitwise `&`, `|`, `^`, `<<`, `>>`,
and `~`; those operators should become VM bytecode only after typed fallback
rules are explicit.

### Strings, Lists, And Records

The generic loop VM needs enough non-JSON operations to cover agent-generated
data processing code:

```rust
StrLower     { dst: Reg, src: Reg }
StrStrip     { dst: Reg, src: Reg }
StrContains  { dst: Reg, haystack: Reg, needle: Reg }
JsonLoads    { dst: Reg, src: Reg }
RecordGet    { dst: Reg, record: Reg, key: ConstId, default: Reg }
ListAppend   { list: AccId, value: Reg }
UniqueAppend { set: AccId, list: AccId, value: Reg }
```

This is enough to start turning common structured transforms into VM programs
without making the VM a second full evaluator on day one.

## Runtime Values

Stone VM values should be cheap to move through registers and explicit about
ownership:

```rust
enum StoneVmValue<'a> {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    Str(Cow<'a, str>),
    List(ListHandle),
    Record(RecordHandle),
    JsonView(JsonView<'a>),
    Accumulator(AccId),
}
```

The exact layout can evolve. The key rule is that JSON rows, record views, and
accumulators do not have to materialize into generic runtime values on every
field access or update.

The interpreter should dispatch over compact tagged VM values, not over AST
nodes. Generic dynamic values are still necessary for shell boundaries and
fallback, but the common path should use VM-native value tags for nulls,
booleans, integers, floats, strings, lists, records, views, handles, and
accumulators.

## Host Capabilities

Stone is still a shell language, so the VM must call host capabilities:

- `run(...)` and process capture
- file reads, writes, stats, search, find, and diff
- JSON, JSONL, and CSV adapters
- helper dispatch and helper observations
- MCP adapter entry points around the same runtime

These capabilities should be exposed to bytecode through explicit host-call or
builtin-call ops with structured inputs and outputs. They should not leak their
implementation details into the VM core. For example, `read_jsonl(path)` can
compile to a host iterator adapter that yields VM row views, while `run(...)`
can compile to a host call that returns a structured Stone record.

## Guards And Fallback

Borrow the deoptimization idea, but keep it interpreted first:

- guard that an iterator adapter is still valid
- guard that a row is a JSON object view when JSON field ops are used
- guard that an accumulator remains a typed map
- guard that typed default assumptions still hold

If a guard fails, materialize live slots and fall back to the existing AST
evaluator. The first fallback can be coarse and loop-level; finer-grained
snapshots can come later.

## Current Waymark Boundary

Today, Waymark already has an early optimized loop path:

- `crates/waymark-runtime/src/stone_ir.rs` contains current Stone IR, JSONL
  lowering, generic loop lowering, optimization helpers, and VM execution
  support.
- `crates/waymark-runtime/src/stone_eval.rs` contains the AST evaluator and
  currently drives the hot-loop path when `WAYMARK_STONE_HOT_LOOP` is enabled.
- `WAYMARK_STONE_HOT_LOOP_VM` selects the VM interpreter path where supported.
- `WAYMARK_STONE_HOT_LOOP_VALIDATE_SNAPSHOT` validates fallback materialization
  behavior for supported paths.

This document describes the target shape for making that VM path durable. It is
not a promise that every Stone program currently lowers to VM bytecode.

## JIT Gate

A future trace or native-code backend should wait until all of these are true:

- the bytecode/function format is stable enough to compile from
- the register interpreter covers enough normal Stone code to be representative
- guard failure and fallback materialization are tested against AST semantics
- profiling shows interpreter dispatch or typed-op overhead is the next
  bottleneck
- traces compile from VM bytecode, not from AST recognizers

Until then, the register interpreter is the performance project.
