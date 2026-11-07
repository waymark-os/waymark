<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Language

Stone is the LLM-oriented structured shell language used by Waymark Shell. It
intentionally looks Python-shaped because coding models already generate those
control-flow and literal forms reliably, but the builtins are shell and data
operations rather than Python standard-library APIs. Its primary author is an
agent generating or repairing task-specific code; human readability remains
valuable for review and debugging.

## Structured Correction Suggestions

Stone error responses may include an `error.correction` record when the
runtime can produce a bounded, context-aware recovery suggestion. Version 1
contains the correction phase and class, received and expected names, at most
three ranked candidates, an optional byte-range source edit, and explicit
recovery choices. `source_sha256` binds the suggestion to the exact failed
source. `execution_state` is `not_started` when conservative admission checks
rejected the program before any statement ran, and `started_or_unknown` for
evaluation-time failures.

Corrections are advisory: `auto_apply` is always `false` and `retry` is
`explicit_only`. Even a local spelling fix may be discovered after an earlier
effect, so the runtime never edits source or reruns a program silently. A
controller may inspect the edit, apply it deliberately, and evaluate it in the
appropriate transaction.

Name, keyword, and structured-field mistakes can receive local edit
suggestions. Semantic mismatches such as Python `global`/`nonlocal` or an unsupported
model message role are marked `requires_repair` and provide guidance without a
mechanical replacement.

In a persistent session, a controller can retrieve the prior error and request
a validated source transformation:

```python
failed = last_result()
preview = correction_apply(original_source, failed.error.correction)
emit(preview)
```

`correction_apply` accepts only a matching `suggest_only` correction and an
unambiguous advertised edit. It returns the corrected source with
`executed=False`; evaluating that source is a separate, explicit controller
decision.

Controllers that expose `stone_eval` as an action can use the bounded
[`attempt_correction_policy.stone`](../examples/references/attempt_correction_policy.stone)
reference. It remembers source/candidate hashes in attempt context, permits one
pre-effect high-confidence retry, and sends ambiguous or possibly executed
actions back for replanning.

Before evaluation, Stone checks direct callable names and the structured
keywords of `model_call`, the context APIs, and `correction_apply`. Source and
session definitions plus lexically bound dynamic callables remain valid. This
preflight is intentionally conservative rather than a general type checker.

## Running Code

```sh
waymark eval -c 'emit({"ok": True})'
waymark eval script.stone
waymark eval --stdin-script < script.stone
```

Stone is the frontend for `waymark eval -c`, script files, and `--stdin-script`.

## Session Semantics

One-shot CLI invocations start with a fresh Stone scope. Long-lived runtime
processes, such as the task-server stream used by the MCP adapter, behave like a
shell session: top-level Stone value and function bindings persist across eval
calls.

```python
rows = read_csv("data.csv")
```

A later eval call in the same runtime process can use the binding directly:

```python
errors = where(rows, "level", "ERROR")
recent = where(rows, "mtime_ms", ">", cutoff)
large_errors = where(rows, lambda r: r["level"] == "ERROR" and r["size"] > 1024)
emit(len(errors))
```

This is not a JSON result cache. Values are kept in the runtime session when they
are safe to carry across calls. Open file handles are task-owned and do not
persist across eval calls. For large values, keep the binding live and emit
small summaries with `len`, `head`/`first`, or `tail`/`last` unless the caller
explicitly asks for full output.

## Expression Semantics

Stone uses Python-shaped expression syntax for familiar agent-generated code,
but operators stay strongly typed. Automatic text-to-number parsing is not part
of operator evaluation. Use explicit conversions such as `int(value)` or
`float(value)` when input text should become a number.

Python `True`, `False`, and `None` are the canonical scalar literals. Stone also
accepts the JSON spellings `true`, `false`, and `null`. This narrow compatibility
rule keeps an otherwise valid agent-authored record from failing when generated
code crosses the Python/JSON boundary.

Truthiness follows Python's common data model: `False`, `None`, numeric zero,
empty strings, empty lists, and empty records are falsey; non-empty values and
non-zero numbers are truthy. Boolean operators short-circuit and, as in
Python, yield an operand: `a or b` is `a` when `a` is truthy and `b`
otherwise, and `a and b` is `a` when `a` is falsey and `b` otherwise. So
`name = given or "default"` works. Conditions only test truthiness.

Text built from values follows Python inside programs: `str()`, `repr()`,
f-string fields, and `format()` placeholders give `"None"`, `"True"`,
`"1.0"`, `"[1, 'x']"`, and `"{'a': None}"`. Text the model reads stays JSON:
`emit`, returned values, observations, error envelopes, and `print()` of a list
or record (`print([1, "x"])` writes `[1,"x"]`). `print()` of a scalar follows
Python (`print(None)` writes `None`). Use `json_dumps(value)` when a program
builds text that must be JSON, such as a model prompt that embeds a record.
`round(x)` returns an int and rounds halfway cases to even (`round(2.5)` is
2); `round(x, n)` returns a float and rounds the stored binary value
(`round(2.675, 2)` is 2.67), as in Python.

Numeric operators support integers and floats:

- `+`, `-`, and `*` work on numbers. `+` also concatenates strings with strings
  and lists with lists.
- `/` returns a float for numeric operands.
- `//` floors toward negative infinity, matching Python for signed operands:
  `-7 // 2 == -4` and `7 // -2 == -4`.
- `%` uses the same divisor-sign rule as Python: `-7 % 2 == 1` and
  `7 % -2 == -1`.
- Integer overflow is an error instead of wrapping.

Bitwise operators are integer-only: `&`, `|`, `^`, `<<`, `>>`, and `~` reject
strings and floats. Shift counts must be non-negative and within the supported
integer width.

List literals use `[]`. Tuple literals use Python syntax but are represented as
Stone list values, so `("left", "right")` and `["left", "right"]` have the same
runtime behavior.

Comparisons support numeric ordering across ints and floats, lexicographic
string ordering, and lexicographic list ordering. Equality is recursive for
lists and records, numeric across ints/floats, and strongly typed for booleans:
`2 == 2.0` is true, but `True == 1` is false. Ordering unrelated types, such as
`"1" < 2`, is an error.

Membership supports strings, lists, and record keys: `"mark" in "waymark"`,
`2 in [1, 2, 3]`, and `"path" in record`. Other containers are an error.

Conditional expressions use Python's `then if condition else otherwise` shape
and evaluate only the selected branch:

```python
size = row["size"] if "size" in row else 0
```

Augmented assignment is available for names and supported item targets with the
same typed operator semantics as the corresponding expression: `+=`, `-=`,
`*=`, `/=`, `//=`, `%=`, `&=`, `|=`, `^=`, `<<=`, and `>>=`.

List and record comprehensions support one or more `for` clauses, `if` filters,
and simple name or tuple targets:

```python
pairs = [name + ":" + str(count) for name, count in counts.items()]
flat = [item for row in rows for item in row["items"] if item]
lookup = {row["name"]: row["score"] for row in rows if row["score"]}
```

Generator expressions are intentionally narrow. They are accepted by `sum`,
`any`, and `all`, support filters, and use Stone truthiness. `any` and `all`
short-circuit generator evaluation.

Functions support positional parameters, optional type annotations, return
annotations, and default values:

```python
def head(path: str, limit: int = 10) -> list:
    return read_text(path).splitlines()[0:limit]
```

An omitted parameter or return annotation means `Any`, including for a
zero-argument function. Use an explicit `-> None` when a function is intended
to be a checked procedure. This preserves the Python-shaped expectation that
`def solve(): ...; return value` may return a value without an annotation.

Alongside `bool`, `float`, `int`, `list`, `None`, `record`/`dict`, and `str`,
Stone recognizes nominal attempt-control annotations:

```python
def choose(
    frontier: semantic_frontier,
    scope: attempt_scope,
    failed: attempt_failure,
    child: attempt_handle,
    outcome: attempt_outcome,
    accepted: attempt_acceptance,
) -> attempt_handle:
    return accepted.selected
```

The interpreter checks these types at function boundaries without flattening
the control value to a record. Their compact runtime tags are also a stable
input to future whole-function analysis and JIT specialization; Stone does not
yet claim static inference across arbitrary assignments.

A joined failed child can be coupled to its cleanup owner without flattening
either value:

```python
outcome = attempt_join(child)
failed = attempt_retain_failure(scope, outcome)
inspection = failed.inspect(include_details=True, trace_limit=40)
```

`attempt_failure` accepts only an unresolved joined failure owned by the same
scope. Its full typed `outcome` remains available during review. Calling
`failed.discard(...)`, accepting/resolving the child, or closing the scope
drops that full outcome while the archived trace remains available through
`failed.inspect(...)`. The object is lifecycle ownership, not permission to
weaken evidence or publish failed state.

Nominal attempt resources support lexical ownership:

```python
scope = attempt_scope(join_timeout_ms=30000)
with scope:
    with semantic_frontier(checkpoint) as frontier:
        child = scope.branch(frontier, entrypoint="worker", start=True)
        outcome = child.wait(timeout_ms=30000)
        child.discard(reason="candidate rejected")
```

Leaving an `attempt_scope` performs checked cancel-then-join cleanup. Leaving a
`semantic_frontier` releases its checkpoint authority. Both happen on normal
fallthrough, `return`, or an error; if the body and cleanup both fail, the body
error remains primary and cleanup is attached as related evidence. Explicit
close/release remains idempotent. Values introduced inside the lexical block
follow Python's block rule: `with` does not create a name scope. The target and
body assignments remain visible after exit, but owned resources are already
closed or released. A file target likewise remains bound as a closed file.
This separates value visibility from resource authority.

Nominal methods are intentionally thin spellings of the functional kernel:
`scope.branch` -> `attempt_branch`, `scope.wait_any`/`wait_all` -> the Gateway
wait-set operations, `child.wait` -> `attempt_join`, `child.inspect` ->
`attempt_inspect`, `root.accept` -> `attempt_accept`, `child.discard` ->
`attempt_discard`, and `frontier.release` -> `semantic_frontier_release`.
Ordinary records do not gain these lifecycle methods. An initial frozen pilot
failed because Stone hid nominal bindings after `with`, unlike Python. After
making block visibility Python-compatible, scoped methods passed 3/3 first
responses with no repairs and were slightly smaller than the typed-functional
baseline. They are therefore the preferred ownership spelling when the
lifetime is naturally block-shaped; the functional forms remain the explicit
kernel surface.

Stone also accepts a narrow async attempt-control form:

```python
async def explore(frontier):
    async with attempt_scope(join_timeout_ms=30000) as scope:
        child = scope.branch(frontier, entrypoint="worker", start=True)
        outcome = await child.wait(timeout_ms=30000)
    return {"outcome": outcome, "clean": scope.closed}
```

`await` is admitted only for owned resource effects: `run(...)`, an attempt
handle, child/scope wait methods, root acceptance, and their functional
`attempt_*` forms. `await run(...)` owns one bounded command through terminal
completion and lowers to the same lifecycle as `run_complete(...)`, including
internal observation slices and termination at the total timeout. Use ordinary
`run(..., background=True)` when the program intentionally needs to interleave
work or manage a service. `async with` remains limited to attempt scopes and
semantic frontiers.

The interpreter lowers these boundaries onto existing blocking Gateway
operations; children and Gateway runs still execute outside the controller,
but Stone does not expose general coroutine values or a local async scheduler.
Diagnostics report `lowering="blocking_resource_effects"`, total `awaits`, and
the `run_awaits` subset.

In a frozen three-pair authorship comparison, async passed 3/3 first responses
with no repairs, but did not beat corrected synchronous scoped syntax: both
averaged 27 lines, while async was about 2% larger. Keep async as an explicit
effect-oriented option, not as evidence that Stone needs general-purpose async.

Default expressions are evaluated when a call omits that argument. Mutable
default literals such as `[]` and `{}` are rejected to avoid shared mutable
state surprises. Calls to user-defined functions, stored named functions, and
lambdas accept positional and keyword arguments. Unknown keywords report the
accepted parameter names; duplicate positional/keyword bindings and missing
required arguments name the affected parameter.

This permits readable policy interfaces:

```python
result = explore(
    checkpoint=checkpoint,
    candidates=candidates,
    worker_entrypoint="worker",
    evaluate=evaluate_candidate,
    max_candidates=2,
)
```

An optional task-owned callable can be tested with `callback is None` or
`callback is not None` without serializing it. Opaque optimized
`agent_control` values still use their documented positional invocation.

Named functions are also first-class callable values. This supports visible
control adapters without lambda wrappers:

```python
def apply(adapter, value):
    return adapter(value)

def verify(candidate):
    return candidate["ok"]

emit(apply(verify, {"ok": True}))
```

### Names And Scope

Name lookup follows Python. A function sees its own variables, then the
variables of the functions it is written inside (closures), then module
variables and builtins. It never sees its caller's variables.

```python
def make_counter():
    count = [0]
    def inc():
        count[0] += 1
        return count[0]
    return inc

counter = make_counter()
counter()
emit(counter())  # 2
```

- A name that a function assigns anywhere in its body belongs to the whole
  body. Reading it before the assignment is an error; it does not fall back to
  a module variable with the same name.
- A closure reads the enclosing function's variables and sees assignments the
  enclosing function makes later. A lambda reads module variables when it
  runs, not when it is created.
- A closure cannot rebind an enclosing variable, and a function cannot rebind
  a module variable. `count += 1` inside `inc` above (with `count = 0`) is
  rejected before the script runs, with the fix: keep the state in a list or
  dict, or return the new value. Stone has no `global` or `nonlocal`.
- A `def` inside a function is a local value of that function, not a module
  function.
- Variables, functions, and builtins share one namespace. A variable hides a
  function or builtin with the same name, so `len = 5` followed by
  `len(items)` is an error that names the hidden builtin and suggests
  renaming the variable.

`transition_hooks(pre=..., post=...)` creates a first-class call-local policy
value. It may be assigned and passed through Stone functions before being used
as `hooks=` on `model_call`, `model_infer`, `run`, or `must_run`. Literal hook
records remain valid for one-off calls. Hook values are task-owned control
objects rather than JSON/data records, and a warm session retains them only
when captured values are persistable.

`@stage(...)` declares an action function as a first-class deterministic stage
for procedures that should not be rediscovered by a model on every turn.
`file_nonempty(path)` supplies a lazy authoritative evidence specification and
generates its observed path/size reference automatically. The frontend lowers
this syntax to the same typed representation exposed explicitly by
`workflow_stage(...)`. `workflow_run(...)` checks evidence before and after
each action and optional repair, and advances only when it is satisfied. An
action's `ok` field is required input but is never completion proof by itself.
See
[Stone Typed Workflows](STONE_TYPED_WORKFLOWS.md) and the
[`typed_evidence_workflow.stone`](../examples/scripts/typed_evidence_workflow.stone)
canary.

### Errors And Handlers

`try` / `except` catches runtime evaluation errors, not parse or lowering
errors. Every handler names an error type:

```python
try:
    text = read_text(path)
except FileNotFoundError as e:
    text = ""
    emit({"warning": e.message, "code": e.code})
except (KeyError, IndexError) as e:
    text = ""
except Exception as e:
    raise
```

Handlers are tried in source order, and the first handler whose type (or one
of whose tuple types) matches runs. An error that no handler matches keeps
propagating. A bound error is a Stone record (data only) with stable `type`,
`kind`, `code`, and `message` fields. I/O errors also include `path` and
`operation` when available.

Builtin error types, with their parents:

| Type | Parent | Raised by |
|---|---|---|
| `Exception` | none | every catchable error |
| `ToolError` | `Exception` | failures from outside the program: commands (`must_run`), Gateway and model calls, files |
| `OSError` | `ToolError` | file and I/O failures |
| `FileNotFoundError`, `FileExistsError`, `PermissionError`, `IsADirectoryError`, `NotADirectoryError`, `TimeoutError` | `OSError` | the matching I/O failure; `TimeoutError` also for Gateway and model timeouts |
| `ExpectationFailed` | `ToolError` | an `expect=` check that was not met, `must_run`, or an `edit` whose old text did not match as required (host only) |
| `UndeclaredEffect` | `ToolError` | a call that attempted an effect it did not declare (host only; nothing raises it yet) |
| `ValueError` | `Exception` | bad values, such as `int("x")` |
| `TypeError` | `Exception` | wrong operand or argument types, such as `1 + "a"` |
| `LookupError` | `Exception` | |
| `KeyError`, `IndexError` | `LookupError` | missing record key, list index out of range |
| `ArithmeticError` | `Exception` | |
| `ZeroDivisionError` | `ArithmeticError` | division or modulo by zero |
| `NameError`, `AttributeError` | `Exception` | unknown name, missing record field |
| `RuntimeError` | `Exception` | other runtime errors |
| `NotImplementedError` | `RuntimeError` | |
| `AssertionError` | `Exception` | |

The tree follows Python except for `ToolError`, which groups every failure
that comes from outside the program so one handler covers recoverable effect
failures. Each builtin type is also a constructor that takes one optional
string message: `raise ValueError("bad row")` and `raise ValueError` both
work.

`ExpectationFailed` and `UndeclaredEffect` are host-only. A program can catch
them, test them with `isinstance`, and re-raise one it caught (`raise` or
`raise error`, unchanged), but it cannot construct, raise, or subclass them,
and a modified copy cannot be raised. `isinstance(x, ExpectationFailed)` is
true only for an error the host raised, never for a dict that merely has
`type="ExpectationFailed"`. A parent deciding what to do after a failure can
therefore trust that the failure came from a checked call.

Error types are values. A bare type name evaluates to the type's name, so
types can be stored in lists and records and passed to functions, and the
`type` field of a caught error compares equal to it:

```python
retry_on = [TimeoutError, ExpectationFailed]
try:
    must_run(["make", "test"])
except Exception as error:
    if not isinstance(error, retry_on):
        raise
    if error.type == ExpectationFailed:
        emit({"diff": error.diff})
```

`isinstance(value, T)` accepts `str`, `int`, `float`, `bool`, `list`,
`dict`, an error type, or a tuple or list of these. As in Python,
`isinstance(True, int)` is true. Stone has no separate tuple or set type, so
`isinstance(x, tuple)` is an error that suggests `list`. An unknown type name
is an error, so a misspelled type never silently fails to match. A record is
an error only if its `type` names a known error type, so
`isinstance({"type": "user"}, Exception)` is false. A deliberate deviation
from Python: `ValueError == "ValueError"` is true, because a type's value is
its name.

Models may define their own error types as field-only classes deriving from an
error type:

```python
class ConfigError(ValueError):
    path: str
    retries: int = 0

raise ConfigError("missing key", path="/etc/app.toml")
```

The class body may hold a docstring, `pass`, and typed fields with optional
immutable defaults. Fields are checked like function parameters. Every error
already has `type` and `message`, so fields may not redefine them. Methods,
decorators, and classes that do not derive from an error type are rejected
with a message naming the fix; use records for plain data. Error classes
persist in the session like functions.

Rules checked before anything runs:

- Bare `except:` is rejected. Use `except Exception as error:` or a narrower
  type. `BaseException` does not exist in Stone: budget exhaustion and policy
  stops end the attempt and are not catchable. `IOError` is spelled `OSError`.
- A handler for `Exception` or `ToolError` whose body is only `pass` is
  rejected, because it discards failed commands, missing files, and failed
  model calls without a trace. Bind the error and use it, or catch the narrow
  type you expect. `except KeyError: pass` stays legal.
- A handler naming an unknown type is rejected with the list of builtin types.
- `except error:` (a lowercase name) does not bind `error` in Python or
  Stone. Stone rejects it with a correction to `except Exception as error:`.

Each tool's `help()` entry lists what it raises. `run()` does not raise for
a failed, timed-out, or missing command (unlike Python's `subprocess`): it
returns a result with `ok=False`. Before a program runs, Stone works out
which tool errors each `try` block can raise, from those declarations and the
bodies of the functions it calls, and reports a handler that can never run in
`diagnostics.warnings`. Examples are `except OSError:` around pure
computation, `except ExpectationFailed:` around a call without `expect=`,
and `except FileNotFoundError:` around `run()`, which does not catch a
missing command. These are warnings only; no handler is ever required. When
a block calls something the analysis cannot see through (a function passed in
a variable, an unknown method, `await`), no warning is given.

At run time, a handler that catches a `ToolError` (or a subclass) without
binding it and without re-raising it is counted. The count and the last such
error appear under `diagnostics.errors` in the command response, and so in
`last_result()`.

`raise` also accepts a string or a record, which keep the structured failure
model:

```python
raise "candidate failed"
raise {
    "message": "candidate failed its evidence gate",
    "code": "candidate_failed",
    "detail": {"candidate": candidate.name},
}
```

A string receives declared code `stone_raised` and type `Exception`. A record
requires `message` and may carry `code` and `detail`; a caught record error
exposes its `detail` field. Bare `raise` is valid only inside an active
`except` handler and rethrows its bounded structured error; a re-raised typed
error keeps its type and fields. `raise ... from ...` remains unsupported.
Prefer `fail(...)` when the harness already knows the stable policy/evidence
failure code.

### Call Contracts

Calls that cross into the outside world (commands, files, tools) can carry a
small contract: an optional expectation checked after the call, and, for
commands, the effects that cannot be undone. Everything else about a call's
effects is observed, not declared.

**Expectations (`expect=`).** `run`, `run_complete`, and `must_run` accept
`expect=Expect(...)` (or a dict with the same keys) with these checks:
`exit` (an int or a list of ints), `stdout_contains`, `stdout_matches` (a
regular expression), `stderr_empty`, and `max_seconds` (which also sets the
timeout when `timeout_ms` is not given). `edit` accepts
`expect=Expect(occurrences=N)`. The keys are a closed set; an unknown key is an
error that lists them. A met expectation returns the normal result and adds
nothing to it. An unmet one raises `ExpectationFailed` with:

- `message`: the facts needed for a fix first, for example
  `exit: expected 0, got 1` and a tail of stdout or stderr;
- `expected`, `observed`: records with the checked keys;
- `diff`: the unmet checks, each `{check, expected, observed}`;
- `result`: the call's full result, so the command never has to be re-run to
  see its output.

```python
try:
    run(["pytest", "tests/test_parser.py", "-x", "-q"],
        expect=Expect(exit=0, stdout_contains="passed"))
except ExpectationFailed as error:
    emit({"diff": error.diff, "stdout": error.result.stdout})
```

`edit(path, old, new)` requires the old text to match exactly once.
`all=True` replaces every match (at least one), and
`expect=Expect(occurrences=N)` requires exactly N matches and replaces them
all. Otherwise the file is unchanged and `ExpectationFailed` reports the
match count and the matching lines, or, when the old text is not found, the
lines where its first line appears.

**Observed effects.** Every finished `run`, `run_complete`, and `must_run`
result has `result.effects.observed`: the files under the project root (the
nearest directory with `.git`, else `cwd`) that were created or changed while
the command ran, collapsed by directory (`"build/ (14 files)"`), with
`changed_count`. Directories that lost entries without gaining any are listed
under `entries_removed_in`. Large tool folders (`.git`, `node_modules`,
`.venv`, caches, and Cargo's `target/`) are not walked and are listed under
`not_scanned`. The walk is bounded; a cut-short walk has `complete: False`.
Changes made by other processes at the same time are included, and `/` and
the home directory are never scanned. Commands that run through the Gateway,
or are still running in the background, report why nothing was observed.

**Declared effects (`effects=`).** A command declares only what cannot be
undone: `effects=Effects(network=["pypi.org"], irreversible=["push"],
background=True)`. `network` is `False` or a list of host names;
`irreversible` kinds are `push`, `publish`, `send`, `deploy`, and
`external_write`. The declaration is validated and recorded in
`result.effects.declared`. In local mode it is not enforced, and the result
says so with `result.effects.enforced == False`.

## Core Builtins

- `emit(value)`: return a structured success value.
- `fail(message)`: return a structured failure.
- `workflow_evidence(satisfied_or_result, summary, references=[])`: construct
  bounded stage evidence from a boolean or a typed action result. Passing a
  result record derives satisfaction from `ok`, retains a bounded failure
  diagnostic, and clears success references on failure. Satisfied evidence
  requires at least one reference.
- `@stage(evidence=..., repair=None, max_attempts=1, checkpoint="none")`:
  declare the following one-argument action function as a named typed stage.
  `checkpoint="workspace"` preserves a freshly proved workspace and bounded
  attempt-memory frontier in Gateway mode and reports an opaque checkpoint
  reference. `checkpoint="forkable"` also requires the active provider to
  seal or reconstruct every semantically relevant tool-environment plane;
  unsupported mutable roots fail closed. `checkpoint="repairable"` captures
  the same planes and retains only the newest verified frontier across a late
  attempt failure. A later `attempt_spawn` whose
  `workspace_source.checkpoint` is that reference restores workspace, attempt
  memory, and tool environment; failed repairs may reuse it, while acceptance
  or explicit rollback, commit, or publication consumes it.
- `attempt_fork(checkpoint=..., ...)`: create an isolated child from an opaque
  verified stage checkpoint owned by the parent. Omitting `checkpoint` forks
  the current parent frontier. A workspace checkpoint does not capture the
  tool environment; a forkable checkpoint may rematerialize its opaque
  immutable-image generation.
- `file_nonempty(path)`: declare a lazy non-empty regular-file proof whose
  evidence reference is generated from the authoritative workspace probe.
- `workflow_stage(name, evidence=..., action=..., repair=None,
  max_attempts=1, checkpoint="none")`: define one bounded
  evidence/action/repair stage.
- `workflow(name, stage, ...)`, `workflow_run(plan)`: compose and execute
  sequential evidence-gated stages, returning a compact structured report.
- `read_text(path)`, `write_text(path, text)`: text file I/O.
- `read_json(path)`, `write_json(path, value)`: JSON file I/O.
- `read_jsonl(path)`, `write_jsonl(path, rows)`: JSONL file I/O.
- `read_csv(path)`: headered CSV records, including multiline quoted fields.
- `write_csv(path, rows, columns=None)`: write headered CSV from record rows
  with standard CSV quoting.
- `find(root, name_glob="*", path_glob=None, type=None, min_size=None,
  max_size=None, modified_after_ms=None, modified_before_ms=None)`: file
  discovery. Names support simple wildcard globs, `path_glob` can match
  recursive patterns such as `**/*.py`, and `type` may be `file`, `dir`,
  `symlink`, or `any`.
- `diff(path_a, path_b)`: compare two text files and return structured hunks
  with line numbers and inserted/deleted lines.
- `search(root, needle, regex=False)`: content search. Literal search is the
  default fast path; pass `regex=True` for Rust-regex patterns. Results remain
  structured records with `path`, `line`, and `text`.
- `where(rows, key, expected)`, `where(rows, key, op, expected)`, or
  `where(rows, lambda r: ...)`: filter record lists by equality, comparison
  (`==`, `!=`, `>`, `>=`, `<`, `<=`, `in`, `not in`), or a predicate.
- `any(iterable)`, `all(iterable)`, `sum(iterable)`, `join(iterable)`, and
  `set(iterable)`: aggregate lists or supported generator expressions with
  Stone truthiness, typed numeric semantics, or deterministic first-seen
  uniqueness.
- `set()` and `set(iterable)`: materialize a deterministic unique list. The
  `.add(value)` method appends only values not already present.
- `str(value)` and `repr(value)`: Python's text forms (`str(None)` is
  `"None"`, `repr("x")` is `"'x'"`, `str({"a": 1})` is `"{'a': 1}"`); use
  `json_dumps(value)` when valid JSON text is required.
- `json_dumps(value, indent=None, separators=None)`: serialize typed Stone
  values as JSON text. The common Python-shaped calls `indent=2` and
  `separators=(",", ":")` are supported.
- `split(text, separator=None, maxsplit=None)`, `join(items, separator="")`,
  `slice(value, start=None, end=None)`, `starts_with(text, prefix)`, and
  `format(template, ...)`: small text/list helpers for scripts and dynamic
  helper callbacks. `format` supports `{}`, numbered placeholders such as
  `{0}`, and simple fixed decimal specs like `{:.2f}`. String method forms such
  as `"a,b".split(",")`, `line.split(":", 1)`,
  `line.split(":", maxsplit=1)`, `line.rsplit("/", 1)`, `line.strip()`,
  `line.lstrip()`, `line.rstrip()`, `text.isdigit()`, `text.isalpha()`,
  `text.isalnum()`,
  `text.count("x")`, `",".join(items)`, `open(path).readlines()`,
  `items.extend(other)`, `items.count(value)`, and mutating
  `items.sort(key=..., reverse=...)` are also supported.
- `list(value)` and `tuple(value)`: materialize a list view. In Stone,
  `tuple()` is an agent-compatibility alias for `list()`.
- `run(argv, cwd=None, stdin=None, timeout_ms=None, env=None, expect=None,
  effects=None, background=False, stdout="capture", stderr="capture",
  max_stdout_bytes=1048576, max_stderr_bytes=1048576, hooks={})`: Linux command
  execution with structured stdout, stderr, status, truncation flags, observed
  effects, and helper observations. See "Call Contracts" below for `expect=`,
  `effects=`, and `result.effects`. Agent loops should
  normally set much smaller output bounds (for example 16 KiB/8 KiB) so one
  noisy command cannot dominate every later model call. Set `stdout` or
  `stderr` to `"suppress"`/`"discard"` when content is not evidence, or set
  `stderr="stdout"` to merge it. A timeout can return a `still_running` handle;
  terminate or wait for that handle explicitly before finishing the attempt.
  A call-local pre hook may veto or replace `argv`; a post hook observes the
  structured success/failure outcome.
- `must_run(argv, cwd=None, stdin=None, timeout_ms=None, env=None,
  expect=None, effects=None, stdout="capture", stderr="capture",
  max_stdout_bytes=1048576, max_stderr_bytes=1048576)`: checked Linux command
  execution for `set -e`-style scripts. `must_run(argv)` is
  `run(argv, expect=Expect(exit=0))`: it returns the run record on success and
  raises `ExpectationFailed`, with the run record in `error.result`, when the
  process exits nonzero, times out, or cannot start. Extra checks go in
  `expect=`.
- `model_call(messages, model_class="agent", hooks={}, ...)`: one explicit Gateway-backed
  model effect. The Stone program supplies the ordered structured messages and
  owns conversation state, retry, tool dispatch, and stopping policy. The
  `role` and `content` fields of every message are strings; use `json_dumps`
  before placing a structured observation in `content`. The
  result contains normalized `content`, assistant `messages`, provider/model,
  finish reason, latency, usage, and metadata. Provider credentials and
  endpoints remain in Gateway. See `help("model_call")` and
  [`model_two_turn.stone`](../examples/scripts/model_two_turn.stone). With the
  current Gateway protobuf, explicit `top_p` and `seed` values must be greater
  than zero; omit them to use provider defaults. Call-local pre/post hooks may
  project attempt memory into this one request and record its outcome. See
  [Stone Transition Hooks](STONE_HOOKS_DESIGN.md).
- `model_infer(messages, schema, retries=0, repair_prompt="",
  schema_prompt="", hooks={}, ...)`:
  a schema-validated JSON decision built from ordinary model-call
  transitions. By default Stone injects the declared schema into the visible
  request. A bounded `schema_prompt` may instead provide a compact equivalent
  model-facing contract while the runtime continues to validate against the
  complete schema. Stone uses portable JSON-object response mode and
  independently validates the returned value. A failed validation never enters control as
  `inference.value`; an allowed repair is another traced and charged model
  call. The result includes the final typed `value`, final raw `response`,
  `validation_attempts`, bounded failure summaries, and aggregate `usage`.
  Schemas and repair state are size-bounded, retries are capped at four, and
  unsupported schema keywords fail before a model effect. See
  `help("model_infer")`,
  [`model_infer_repair.stone`](../examples/scripts/model_infer_repair.stone),
  and the reusable
  [`typed_react_agent.stone`](../examples/scripts/typed_react_agent.stone).
- `workflow_patch(plan, target, replacement)` or
  `workflow_patch(plan, patch_record, allowed_replacement, ...)`: construct a
  new workflow with exactly one named stage replaced by a `workflow_stage`,
  preserving order and recording bounded patch provenance. The data-driven
  form requires exactly `{target, replacement}` and resolves the replacement
  only against explicitly supplied candidate stages.
- The checked-in
  [`standard_attempt_agent.stone`](../examples/scripts/standard_attempt_agent.stone)
  composes typed inference with replaceable named-function adapters for
  dispatch, finish verification, and progress retention. It bounds messages,
  reads, process output, time, rounds, and actions, and permits exactly one
  action per model transition. See
  [Standard Visible Agent Control](STONE_STANDARD_AGENT_CONTROL.md).
- A `run(...)` pre hook may reject one command without aborting the surrounding
  agent loop. The command is skipped, the result has `ok=false`,
  `kind="policy_rejected"`, and `policy_reason`, and the post hook still
  observes the bounded failed outcome so the next model decision can recover.
- `context_write(...)`, `context_read(...)`, and `context_project(...)`:
  experimental attempt-context operations for model-authored write/manage/read
  loops. In an attached attempt they use bounded, revisioned Gateway state and
  survive controller restart; standalone evaluation uses the bounded warm
  session fallback. Raw effects remain in transition trace; retain an
  important outcome by writing it under a meaningful key.
  `context_project(..., required_keys=[...])` reserves exact critical keys
  before relevance-ranked fill and fails if they are missing or cannot fit the
  token budget. Fork snapshots attempt memory, accept does not merge it, and
  parent promotion is an explicit write. Inspect each operation with
  `help("context_write")` and the related help entries before use. The
  commented [`default_attempt_agent.stone`](../examples/scripts/default_attempt_agent.stone)
  composes these operations with transition hooks into a bounded reference
  loop.
- `attempt_join(child)` returns a typed outcome whose `result.value` is the
  child's compact named-entrypoint return value. Use
  `attempt_inspect(child, include_details=False, trace_limit=20,
  max_bytes=32768)` only when the parent needs a bounded trace tail, the
  optional full controller envelope, or execution-resource status. Inspection
  remains available after `attempt_accept` or `attempt_discard`: archives
  persist while the child controller, operations, runs, and transaction are
  reclaimed.
- `with attempt_scope(...)` and `with semantic_frontier(...)` provide lexical
  cleanup for attempt trees and reusable checkpoints. Their nominal methods
  are described above and exercised by
  [`scoped_attempt_resources_canary.stone`](../examples/scripts/scoped_attempt_resources_canary.stone).
- `async def`, `async with`, and restricted `await` make attempt wait and
  acceptance boundaries explicit without introducing general coroutines. See
  [`async_attempt_resources_canary.stone`](../examples/scripts/async_attempt_resources_canary.stone).
- `task_spec()`: read the attached attempt's admitted objective, named inputs
  and outputs, success criteria, constraints, and metadata as a structured
  record. This is a read-only view, not a rendered prompt.
- `task_input()`: read the attempt's separate dynamic JSON input as an ordinary
  Stone value, or `None` when none was admitted. Gateway persists this value in
  the attempt control block; it is not passed through environment variables.
  See [`react_agent.stone`](../examples/scripts/react_agent.stone) for a
  reusable model/tool loop built from these task views.
- `fail(message, code="...")`: stop intentionally with the stable outer
  category `task_failure`. The task response also exposes the program-selected
  `declared_code`, preserving policy-specific reasons such as
  `turn_limit_exceeded` without losing the task-failure classification.
- `wait_port(port, host="127.0.0.1", timeout_ms=30000, protocol="tcp")`:
  wait for a TCP port to accept connections, or for a UDP endpoint to accept a
  datagram send probe. UDP has no connection handshake, so use a real protocol
  client for final validation.
- `wait_for(lambda: condition, timeout_ms=30000, interval_ms=100,
  ignore_errors=False)`: poll an arbitrary zero-argument predicate until it is
  truthy or the timeout expires. Returns a structured record with `ok`, `kind`,
  `attempts`, `duration_ms`, and the last observed value or error.
- `pwd()`, `cd(path)`: inspect or change the current Stone working directory
  for the current session.
- `ps(interval_ms=0)`: typed process list with pid, parent pid, command,
  status, cwd, CPU, memory, virtual memory, and owner uid fields.
- `sysinfo(section="all")`: typed host system snapshot. Sections include
  `"os"`, `"cpu"`, `"cpu_long"`, `"mem"`, `"disks"`, `"net"`, `"temp"`,
  and `"users"`. `sys()` is an alias.
- `state()`: typed runtime snapshot with `cwd`, workspace root/relative path,
  git summary, and common tool availability/version probes.
- `last_result()`: previous Waymark command response as typed data, or `None`
  before any previous response in the current runtime process.
- `correction_apply(source, correction, candidate=0)`: validate one
  source-bound `suggest_only` correction and return corrected, unexecuted
  source for an explicit later evaluation.
- `help()`, `help("topic")`: builtin help.

## Result Shape

Successful CLI evaluation prints:

```json
{"ok":true,"cwd":"/path","value":{},"output":{"stdout":"","stderr":""}}
```

Failures print a JSON envelope to stderr with `ok: false` and structured error
fields such as `kind`, `code`, `message`, and path/span details when available.
