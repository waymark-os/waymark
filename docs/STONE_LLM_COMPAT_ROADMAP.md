<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone LLM Compatibility Roadmap

Stone uses Python-shaped syntax so coding agents and LLMs can write useful
scripts without learning a new surface language. The goal is not to become
Python. The goal is to accept the high-frequency Python shapes agents naturally
emit, while preserving Stone's typed values, structured errors, explicit effects,
and predictable shell/runtime behavior.

Every feature below should ship with focused tests that prove exact baseline
outputs, structured errors, and hot-loop behavior where optimized execution is
involved. Optimized paths must either match baseline semantics or fall back
before mutating visible state.

## Priority Order

### 1. Ternary Expressions

Support:

```python
value = row["size"] if "size" in row else 0
```

This is a common LLM pattern for defaults and projections.

Implementation notes:

- Add an AST expression for `then if condition else otherwise`.
- Evaluate only the selected branch.
- Use Stone truthiness, not implicit bool conversion.
- Keep it baseline-evaluated in hot loops until the VM has branch opcodes.

Tests:

- selected true branch returns exact value
- selected false branch returns exact value
- unselected branch can reference an unknown name without error
- nested ternary expression
- hot-loop path matches baseline or falls back safely

### 2. More Augmented Assignments

Support common Python augmented operators beyond `+=`:

```python
total -= 1
scale *= 2
ratio /= 2
bucket //= 10
bucket %= 10
mask |= flag
mask &= 7
mask ^= bit
mask <<= 1
mask >>= 1
```

Implementation notes:

- Extend `AugOp`.
- Reuse Stone's existing typed operator helpers.
- Support both name targets and currently supported item targets.
- Preserve checked integer overflow behavior.
- Preserve strongly typed bitwise/shift behavior.

Tests:

- exact results for each operator on name targets
- exact results for nested item targets where supported
- invalid type errors for each operator class
- division/modulo by zero
- integer overflow for arithmetic operators
- baseline-vs-hot-loop parity for supported integer loop forms

### 3. `any` And `all`

Support:

```python
if any(row["status"] == "failed" for row in rows):
    emit("failed")
```

These are common in generated filtering and validation code.

Implementation notes:

- Accept lists and generators.
- Use Stone truthiness.
- Short-circuit when evaluating generators.
- Keep generator support narrow and explicit.

Tests:

- `any([]) == False`
- `all([]) == True`
- truthy/falsey list cases
- generator cases over records and strings
- short-circuit avoids evaluating a failing expression
- structured errors for non-iterable input

### 4. Narrow `try` / `except`

Support common recovery around file/path/JSON/process operations:

```python
try:
    text = read_text(path)
except Exception as e:
    text = ""
    emit({"warning": e.message})
```

This should catch runtime/evaluation errors, not parse/lowering errors.

Implementation notes:

- Lower `try` statements with one or more `except` handlers.
- Initially support:
  - `except:`
  - `except Exception:`
  - `except Exception as e:`
- Bind `e` as a Stone record, not a Python exception object.
- Include at least `kind`, `code`, and `message`; include `path`,
  `operation`, or suggested next action when available from the underlying
  error envelope.
- Defer `finally` and typed Python exception classes until there is a clear
  need.

Tests:

- missing `read_text` path is caught
- invalid `read_json` is caught
- successful `try` skips handlers
- bound error record has stable fields
- unsupported handler forms fail at lowering with a clear error
- parse/lowering errors are not caught

### 5. Function Default Arguments

Support:

```python
def head(path, limit=10):
    return read_text(path).splitlines()[0:limit]
```

Implementation notes:

- Store default expressions or evaluated default values with function metadata.
- Prefer definition-time evaluation only if it is easy to explain and test.
- Consider rejecting mutable defaults initially to avoid Python's surprising
  shared mutable-default behavior.
- Preserve type checks on provided and defaulted arguments.

Tests:

- omitted default argument
- explicit argument overrides default
- multiple defaults
- missing required argument
- too many arguments
- default type mismatch reports a structured error
- mutable default rejection if that policy is chosen

### 6. Set Compatibility

Support high-frequency set-like shapes without committing to a broad set VM:

```python
seen = set()
seen.add(name)
if name not in seen:
    ...
```

Possible syntax expansion:

```python
seen = {"a", "b"}
unique = {row["name"] for row in rows}
```

Implementation notes:

- Keep `set(...)` compatibility simple.
- Represent sets as unique structured values unless/until a dedicated runtime
  set type is justified.
- Document equality and ordering behavior if sets are materialized as lists.

Tests:

- `set()` and `set([...])`
- `.add(...)`
- membership and non-membership
- deduplication preserves deterministic materialized output
- set literal if implemented
- set comprehension if implemented

### 7. Better Comprehensions And Generators

Support more generated Python data-shaping patterns:

```python
names = [name for name, count in counts.items()]
flat = [item for row in rows for item in row["items"]]
total = sum(int(x) for x in values if x)
```

Implementation notes:

- First add tuple targets in list/dict comprehensions.
- Add generator filters before general multi-generator support.
- Add multiple `for` clauses only when the evaluator behavior is clear and
  covered by tests.

Tests:

- tuple-target list comprehension over `.items()`
- tuple-target dict comprehension
- generator filter with `sum`
- multi-generator list comprehension if implemented
- clear lowering errors for unsupported nested targets or async clauses

## Lower Priority Or Deliberately Deferred

These are common in Python but lower value for Stone's shell role:

- `import` and `from ... import ...`: keep rejected with targeted guidance to
  use Stone builtins/helpers.
- `class`: likely not aligned with Stone's agent-shell role.
- decorators, async/await, yield: low current value.
- complex numbers: outside the current shell/data domain.
- broad Python exception hierarchy: prefer structured Stone error records.

## Testing Bar

For each accepted Python-shaped feature:

- Add lowering tests in `stone_ast.rs`.
- Add evaluator tests in `stone_eval.rs` with exact JSON output.
- Add negative tests for unsupported forms and invalid operand/type behavior.
- Add baseline-vs-hot-loop tests if the feature can appear inside a lowered
  loop shape.
- Update `docs/STONE_LANGUAGE.md` when the behavior becomes user-visible.

