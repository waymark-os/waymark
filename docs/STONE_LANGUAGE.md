<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Language

Stone is the LLM-oriented structured shell language used by Waymark Shell. It
intentionally looks Python-shaped because coding models already generate those
control-flow and literal forms reliably, but the builtins are shell and data
operations rather than Python standard-library APIs. Its primary author is an
agent generating or repairing task-specific code; human readability remains
valuable for review and debugging.

## Where an Error Came From

An error that ends a program carries `error.trace`, a list of frames,
outermost first. Each frame names the file (`<program>` for the program
itself, the module's path for an imported module), the line, the code on that
line, the function the statement was in (no `function` at top level), and the
values of the variables the line reads:

```json
"trace": [
  {"file": "<program>", "line": 10, "code": "emit(summarize(rows))",
   "values": {"rows": "[{'n': 1, 'v': 2}, {'n': 5}]"}},
  {"file": "<program>", "line": 7, "code": "result = pick(rows, \"v\")", "function": "summarize",
   "values": {"rows": "[{'n': 1, 'v': 2}, {'n': 5}]"}},
  {"file": "lib.stone", "line": 8, "code": "total += item[key]", "function": "lib.pick",
   "values": {"total": "0", "item": "{'n': 5}", "key": "'v'"}}
]
```

`values` holds each value's Python text (`repr`), cut to about 160
characters: a long list or dict shows its first items and `... N more`, a long
string its start and `... (N chars)`. It lists at most eight variables per
line and leaves out functions, types, the model, a plain assignment's target
(its old value is not what the line computes), and lambda or comprehension
variables. For a compound statement only its own line counts: an `if`'s
condition, a `for`'s iterable.

Errors found before the program runs (syntax errors, unsupported Python
forms such as `global` or `del`, scope errors, unknown functions or error
types, missing modules or module names) carry a one-frame `trace` naming the
statement, in the program or in the module where it is. A syntax error at
the end of the file, such as an unclosed bracket, names the last line with
code.

The last frame is where the error happened. Locations are statements, not
expressions: an error inside a long expression names the statement's first
line. A program that uses `workflow` block syntax is numbered after the
blocks are expanded. A caught error carries no trace; the error record a
handler sees is unchanged.

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

A program has no implicit input. It reads data explicitly, from files
(`open`, `read_json`, `read_jsonl`) or, in a task spec, as the argument of the
function named by `script.entrypoint` (`def main(input): ...`). A task that
sets `input` or `input_path` without an entrypoint is rejected.

`--budget model_calls=40,output_tokens=60000,tool_calls=200,wall_time_ms=600000`
gives the run a budget (any subset of those limits). See
[Budgets](#budgets).

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

Builtins follow the same rule: an argument that must be an int (`range`,
slice bounds, `enumerate` start, `timeout_ms=`) rejects floats, text, and
bools with a `TypeError`, and numeric builtins (`sum`, `round`) reject text.
`int(x)` and `float(x)` are the conversions: `int` truncates a float toward
zero, and `int(1e20)` or `int(float("nan"))` raises `ValueError` instead of
saturating.

Python `True`, `False`, and `None` are the canonical scalar literals. Stone also
accepts the JSON spellings `true`, `false`, and `null`. This narrow compatibility
rule keeps an otherwise valid agent-authored record from failing when generated
code crosses the Python/JSON boundary.

Some Python forms are not in Stone: `del`, `assert`, `match`, `:=`, `**`,
`@`, `*` unpacking in calls and literals, `yield`, `...`, bytes literals, and
annotations outside record fields (`x: int = 5`). Each is rejected before the
program runs with one sentence saying what to write instead.

Truthiness follows Python's common data model: `False`, `None`, numeric zero,
empty strings, empty lists, and empty records are falsey; non-empty values and
non-zero numbers are truthy. Boolean operators short-circuit and, as in
Python, yield an operand: `a or b` is `a` when `a` is truthy and `b`
otherwise, and `a and b` is `a` when `a` is falsey and `b` otherwise. So
`name = given or "default"` works. Conditions only test truthiness.

Text built from values follows Python: `str()`, `repr()`, f-string fields,
`format()` placeholders, and `print()` give `"None"`, `"True"`, `"1.0"`,
`"[1, 'x']"`, and `"{'a': None}"` (`print([1, "x"])` writes `[1, 'x']`).
Structured output stays JSON: `emit`, returned values, observations, and error
envelopes. Use `json_dumps(value)` when a program builds text that must be
JSON, such as a model prompt that embeds a record.
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
lists and records, and numeric across ints/floats: `2 == 2.0` is true.
Booleans are not numbers. Python answers `True == 1` with `True`; Stone raises
for any comparison that mixes a bool with an int or float, at any depth
(`[True] == [1]`, `1 in [True, 2]`, `[1, True].count(1)`, `.index(True)`), and
the error says to convert with `int(flag)` or `bool(count)`. Ordering
unrelated types, such as `"1" < 2`, is an error.

Membership supports strings, lists, dict keys, sets, and record fields:
`"mark" in "waymark"`, `2 in [1, 2, 3]`, `"path" in d`, and
`"exit_code" in result` (true when the record has that field). Other
containers are an error.

Conditional expressions use Python's `then if condition else otherwise` shape
and evaluate only the selected branch:

```python
size = row["size"] if "size" in row else 0
```

Augmented assignment is available for names and supported item targets with the
same typed operator semantics as the corresponding expression: `+=`, `-=`,
`*=`, `/=`, `//=`, `%=`, `&=`, `|=`, `^=`, `<<=`, and `>>=`.

List, set, and record comprehensions support one or more `for` clauses, `if`
filters, and name or tuple targets, including nested tuples
(`for i, (k, v) in enumerate(d.items())`, in loops too):

```python
pairs = [name + ":" + str(count) for name, count in counts.items()]
flat = [item for row in rows for item in row["items"] if item]
lookup = {row["name"]: row["score"] for row in rows if row["score"]}
```

A generator expression can be passed straight to any call:
`max(len(x) for x in xs)`, `list(...)`, `sorted(...)`, `dict((k, v) for ...)`,
or a function of your own. The call receives the values as a list, so it sees
what Python's would. `any` and `all` stop at the first deciding value, as in
Python. A generator expression anywhere else (`g = (x for x in xs)`) is an
error that says to write a list comprehension.

Functions support positional parameters, optional type annotations, return
annotations, and default values:

```python
def first_lines(path: str, limit: int = 10) -> list:
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
This separates value visibility from resource authority. Only `open(...)`,
`attempt_scope()`, and `semantic_frontier()` can be used in `with`; any other
value (`with 5:`, `with some_dict as d:`) is an error, as in Python.

As in Python, `for line in f` and `f.readlines()` keep each line's `"\n"`
(`["a\n", "b\n", "c"]`), so strip it with `line.rstrip("\n")` or
`line.strip()`. `f.splitlines()` and `text.splitlines()` drop it.

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
work or manage a service. Background runs need the Gateway-backed runtime; a
local `waymark eval` rejects `background=True` (use `start_daemon()` for a
local service). `must_run` always waits, so it does not take `background=`. `async with` remains limited to attempt scopes and
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

Default values are evaluated once, when the `def` statement runs, as in
Python: `def g(x=i)` inside a loop keeps the `i` of that pass, and a call that
omits `x` gets that stored value. Mutable default literals such as `[]` and
`{}` are rejected to avoid shared mutable state surprises; use `None` and
build the value in the body. Calls to user-defined functions, stored named functions, and
lambdas accept positional and keyword arguments. Unknown keywords report the
accepted parameter names; duplicate positional/keyword bindings and missing
required arguments name the affected parameter. Builtins take their parameters by the names
Python uses, as well as by position: `enumerate(xs, start=1)`,
`zip(xs, ys, strict=True)`,
`round(x, ndigits=2)`, `text.split(sep=",", maxsplit=1)`,
`get(d, key, default=0)`. An unknown keyword to a builtin is a `TypeError`
that lists the keywords it takes.

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
- Top-level definitions bind a name once, for the whole run. These are
  rejected before the program runs: a top-level `def` or an import that
  reuses a builtin's name (`def len(x): ...`, `from lib import f as len`), a
  name defined twice at the top level, and a `def` or `class` inside a
  top-level `if`, loop, `with`, or `try`. A function's own nested `def` is a
  local and may reuse any name, as in Python.
- `true`, `false`, and `null` are reserved: they always read as `True`,
  `False`, and `None`, and cannot name a variable, parameter, or function.
- Inside a `predicate()`, calls always mean the builtins, whatever the
  program binds.

### Lists And Dicts

Dict keys are strings, because dicts are JSON objects. A dict literal may
compute a key (`{name: 1, f"{a}-{b}": 2}`) as long as it is a string when
the program runs. `{1: "a"}` and `{True: "a"}` are rejected before the
program runs (Python would key them by the number or bool). A non-string key
at run time (`d[1] = "x"`, `{i: x for ...}`, `d.get(1)`, `(a, b) in d`) is a
`TypeError` that says what to write instead: `str(key)` for a number, used
both when storing and when looking up, and a joined string key such as
`f"{a}|{b}"` or nested dicts `d[a][b]` for a pair.

Lists and dicts are shared objects, as in Python. Assigning one to another
name, passing it to a function, or storing it inside another list or dict
does not copy it, so a change made through one name is visible through every
other name for the same object.

```python
def tag(row):
    row["seen"] = True

rows = [{"n": 2}, {"n": 1}]
for row in sorted(rows, key=lambda r: r["n"]):
    tag(row)
emit(rows)  # both rows now have "seen": True
```

- A dict's keys are read and written with brackets: `d["a"]`,
  `d["a"] = 1`, `d.get("a")`. `d.a` is an `AttributeError` that says to write
  `d["a"]`; attributes are for records (see Records). This holds for every
  dict: literals, `json_loads` and `read_json` results, `task_input()`, and the
  JSON a model returns. Dict methods (`d.keys()`, `d.items()`, `d.get(k)`) are
  calls as usual.
- A `for` loop over a list walks the live list, as in Python: items appended
  during the loop are visited (`for n in queue: queue.extend(...)` drains a
  worklist), and writes to later items are seen. Changing the size of a dict
  or set while looping over it raises `RuntimeError`; loop over a copy
  (`for k in list(d):`) or apply the changes after the loop.
- A string iterates by character, as in Python: `for ch in word`,
  `list("ab")`, `set("aab")`, and `xs.extend("ab")` see `"a"` and `"b"`.
  Use `text.splitlines()` or `for line in open(path)` for lines.
- `xs += ys` extends `xs` in place, including `d["xs"] += ys`.
- Lists have Python's `append`, `extend`, `insert`, `pop`, `remove`,
  `clear`, `reverse`, `copy`, `count`, `index`, and `sort`. Dicts have
  `get`, `keys`, `values`, `items`, `pop`, `setdefault`, `update` (a dict, a
  list of `[key, value]` pairs, or `key=value` arguments), `clear`, and
  `copy`. `copy()` is shallow, as in Python: the copy shares nested lists and
  dicts. Missing items raise as Python does: `[].pop()` is an `IndexError`,
  `xs.remove(x)` without `x` is a `ValueError`, `d.pop(k)` without `k` is a
  `KeyError`.
- `xs * n` and `"ab" * n` repeat a list or string, as in Python. The
  repeated elements are shared: `grid = [[]] * 2` holds one inner list twice,
  so `grid[0].append(1)` gives `[[1], [1]]`. Build independent rows with a
  comprehension: `[[] for _ in range(2)]`.
- Slices take Python's three parts, `xs[start:stop:step]`, on lists,
  tuples, and strings: `xs[::-1]` reverses and `xs[::2]` takes every other
  item. A step of 0 is a `ValueError`.
- `list(xs)`, `xs[:]`, and comprehensions make a new outer list; the elements
  are still shared. `{k: v for k, v in d.items()}` copies a dict the same way.
- `a is b` is true only when both names refer to the same list, dict, or
  record; `a == b` compares contents. `is` also works with None, True,
  False, and types (`type(error) is ValueError`). `is` with any other
  literal (`x is 1`, `x is "a"`, `x is []`), which Python warns about, is
  rejected before the program runs, and `is` between two numbers or strings
  raises; both errors say to use `==`.
- `x or y` and `x and y` return one of the operands itself, not a copy.
- `for`, `enumerate`, `sorted`, `filter`, `where`, `first`, `last`,
  slicing, and a dict's `.get`, `.values`, and `.items` hand out the same
  elements, and a list method such as `.append` changes the list it is
  called on even when that list came from a call: `groups()[0].append(x)`.
  Assignment works the same way: `f()[0]["k"] = v` and
  `by_name.get("a").score = 1` change the object the call returned. Each
  expression in an assignment target runs once, so `xs[i()] += 1` calls
  `i()` once, as in Python.
  The function forms `get(d, k)`, `values(d)`, `items(d)`, `slice`,
  `unique`, `map`, `min`, and `max` hand out the same elements too. The JSON
  helpers return copies.
- Dicts keep insertion order everywhere: `keys()`, `str()`, `json_dumps`,
  `emit`, and JSON read from files all keep the order the keys were written.
- Lists and dicts can hold functions and lambdas. Any expression that gives a
  function can be called: `handlers["save"](path)`, `fs[0](x)`,
  `make_adder(2)(3)`. Calling a value that is not a function is an error that
  names the value's type.
- Errors the host reports (`ExpectationFailed`, `UndeclaredEffect`) are
  read-only, all the way down. Changing one is an error; build a modified copy
  with a dict comprehension instead.

### Records

A record is a value with fixed, typed fields. Records are how a program
states the shape of its data: a function's parameters, the state a loop
carries, and what the model must answer (`model.ask(..., output=Plan)`).

```python
from typing import Literal, Optional

class Step(Record):
    """One step of a plan."""
    title: str
    done: bool = False

class Plan(Record):
    steps: list[Step]
    risky: bool = False
    note: Optional[str] = None

plan = Plan(steps=[Step("read the failing test"), Step(title="fix it")])
plan.steps[0].done = True
emit(str(plan.steps[0]))  # Step(title='read the failing test', done=True)
```

- `class Name(Record):` defines a record. The Python spellings
  `class Name(BaseModel):` and `@dataclass class Name:` define the same thing.
  `from typing import Any, Dict, List, Literal, Optional, Union`,
  `from pydantic import BaseModel, Field`, `from dataclasses import dataclass`, and
  `from __future__ import annotations` are accepted and do nothing; any other
  name from those modules is an error that lists the accepted ones.
- The class body holds a docstring and typed fields with optional defaults.
  A default is a literal (`0`, `""`, `None`, `[]`, `{}`, ...) and is created
  fresh for each record, so a default list is never shared. Methods,
  inheritance, and class keywords are rejected; write functions that take the
  record.
- pydantic's `Field()` as a default adds limits:
  `timeout_ms: Optional[int] = Field(None, ge=1, le=120000)`,
  `command: str = Field(min_length=1, description="Shell command")`,
  `paths: list[str] = Field(default_factory=list, max_length=8)`.
  The accepted options are the default (positional or `default=`; `...` or
  none means required), `default_factory=list` or `dict`, `ge`, `gt`, `le`,
  `lt`, `min_length`, `max_length`, and `description`; any other option is
  an error that lists these. Number limits apply to `int` and `float`
  values, length limits to strings (in characters), lists, and dicts; other
  values, such as `None` in an `Optional` field, pass. A limit that cannot
  apply to the field's type, or a default that breaks its own limits, is an
  error where the record is defined. Limits are checked when a record is
  built or a field assigned (`Run() field command: expected at least 1
  character, got 0`) and when a model answer is read, and `model.ask` sends
  them in the JSON Schema as `minimum`, `exclusiveMinimum`, `maximum`,
  `exclusiveMaximum`, `minLength`/`maxLength`, `minItems`/`maxItems`, or
  `minProperties`/`maxProperties`, with the description. `Annotated[...]` is
  not supported; its error shows the `Field()` form.
- Field types: `str`, `int`, `float`, `bool`, `None`, `Any`, `list[T]`,
  `dict[str, T]`, `Literal["a", "b"]`, `Optional[T]`, unions (`A | B`,
  `Union[A, B]`), `Model`, and other records. A quoted name such as
  `list["Tree"]` refers to a record defined later, or to the record itself.
  The same types work in function annotations (`def run(p: Plan) -> Step:`)
  and are checked when the function is called and returns.
- A top-level `Action = Read | Edit` names a type. It can be used in
  annotations, in `isinstance(x, Action)`, and as `output=`.
- Build a record with keywords, or positionally in field order. A missing
  field, an unknown field, a repeated field, or a value of the wrong type is
  a `TypeError` that names the field and its path, such as
  ``Plan() field steps[0]: expected Step, got dict; build the record with `Step(field=value, ...)` ``.
  Nothing is converted: a dict is never turned
  into a record, though an `int` is accepted where a `float` is declared.
- Fields are read and assigned as attributes. Assignment is checked:
  `plan.risky = "no"` is `cannot set Plan.risky: expected bool, got str`.
  `plan["note"]` is an error that names the attribute to use, and a record
  cannot be iterated. When the source shows that a name holds a record, the
  error comes before the program runs: the name is only ever assigned a
  record constructor call, `model.ask(..., output=R)` with `R` a record or a
  union of records, or a host builtin that returns one record (`run`,
  `must_run`, `run_tests`, `stat`, ...); or it is a parameter annotated with a record
  (`def f(plan: Plan)`), or the first parameter of a function passed to
  `workflow_stage(evidence=..., action=..., repair=...)` (a `StageContext`)
  or `transition_hooks(pre=..., post=...)` (a `TransitionEvent`). A name that
  may also hold a dict is left to the runtime check.
- A record is shared like a list or dict: `same = plan` refers to the same
  record, and a change through either name is seen by both.
- Lists and dicts held by a record are checked when assigned, not when later
  changed in place: `plan.steps.append(3)` is not caught, as in pydantic.
- `==` is true for two records of the same type with equal fields; a record
  never equals a dict. `type(plan)` is the class `Plan`, so
  `type(plan) == Plan` is true; comparing it with the string `"Plan"` raises,
  since a type never equals its name. A record is always true.
- `str()`, `repr()`, f-strings, and `print` give Python's dataclass form,
  `Plan(steps=[...], risky=False, note=None)`. `emit`, JSON, and builtins
  such as `len` see a plain dict of the fields.
- Builtins that report on the host return records too, so their fields are
  attributes and are listed in errors: `run(...)` gives a `RunResult`
  (`result.exit_code`, `result.stdout`), `stat(path)` a `FileStat`, `ls()` a
  list of `DirEntry`, `ps()` a list of `ProcessInfo`, `model_call(...)` and
  `model_infer(...)` their own result records, and transition hooks and
  workflow stage handlers receive a `TransitionEvent` or `StageContext`.
  Subscripting one (`result["exit_code"]`) is an error naming the attribute.
  A field that carries program data stays plain data: `inference.value` (the
  model's parsed JSON), a hook's `step.input`, a stage's `inputs` and
  `outputs`, and other `value`/`data`/`payload` fields are dicts and lists,
  read with brackets.
- Only the runtime makes these result records, and they record what
  happened, so they are read-only, data fields included:
  `result.exit_code = 0` raises, and `dict(inference.value)` gives a copy to
  change. A program cannot define a record named after one (`RunResult`,
  `TestRun`, `FileStat`, `DirEntry`, `ProcessInfo`, `SystemInfo`,
  `SearchMatch`, `Daemon`, `WorkflowReport`, `StageReport`, `StageContext`,
  `TransitionEvent`, `Finding`, `FindingSpec`, `Probe`, `StageOutput`, `Snapshot`,
  `TreeDiff`), and
  `isinstance(x, RunResult)` is true only for a result the runtime returned,
  never for a dict or record with the same fields. A library can therefore
  accept a `RunResult` or `TestRun` as evidence that a command ran. Name-keyed maps such as `findings` and `stage_outputs`
  are dicts whose values are records:
  `step.stage_outputs["inspect"].findings["layout"].value`.
- Record names are global, like error classes. A module may define records;
  they are imported under their own name, cannot be renamed with `as`, and
  two modules may not define the same record. Record classes persist in the
  session like functions.

### Budgets

A run can have a budget for model calls, tokens, commands, and wall-clock
time. Budgets are checked only when the program calls a model or starts a
command, so other code costs nothing extra.

```python
def calls_to_spend(model: Model) -> int:
    left = model.remaining.calls
    if left is None:
        return 10
    return left - 1    # keep one call for the final report

helper = model.limit(calls=5, output_tokens=4000)
emit({"plan": calls_to_spend(helper), "spent": model.usage})
```

- The host sets the run's budget with
  `waymark eval --budget model_calls=40,input_tokens=...,output_tokens=...,tool_calls=200,wall_time_ms=600000`,
  or through the Gateway attempt's resource limits (the same names). When
  both are given, the tighter limit wins. `tool_calls` counts commands
  started with `run`, `run_complete`, and `must_run`.
- `model.limit(calls=, input_tokens=, output_tokens=)` returns a new `Model`
  value. Its calls are charged to it and to every budget above it, so it can
  never spend more than its parent has left. Pass it to a function as a
  `model: Model` parameter.
- `model.usage` is what that model value has spent (`calls`,
  `input_tokens`, `output_tokens`). `model.remaining` is the smallest amount
  left along its chain of budgets, plus the attempt's `tool_calls` (commands
  left) and `seconds` of wall-clock time; a value is `None` when nothing
  limits it. Both are fresh snapshots.
- An `output_tokens` budget also caps each call's `max_output_tokens` to what
  is left, and `wall_time_ms` caps each command's `timeout_ms`. Token counts
  are those the model provider reports.
- **Running out of any budget ends the run.** The call that would go over
  does not happen, `try` cannot catch the stop, nothing after it runs, and
  later evaluations in the same session do not run either. The result is
  not a program error: it has `"status": "unfinished"` and an `unfinished`
  record with the limit that ran out, the
  usage, the last few model calls and commands (with the files each command
  changed), and the end of the printed output. `error.type` is
  `"Unfinished"` for clients that only read `ok` and `error`.
- To stop on the program's own terms, check `model.remaining` in the loop
  and return a partial result while there is budget left.

### Modules

`from lib import name` imports from the Stone file `lib.stone`. Dotted names
map to folders: `from pkg.util import clean` reads `pkg/util.stone`.

```python
# textutil.stone
SEP = ", "

class ParseError(Exception):
    line: int

def _clean(s):
    return s.strip()

def join(items):
    return SEP.join([_clean(x) for x in items])
```

```python
# main.stone
from textutil import join, SEP as default_sep, ParseError
emit(join([" a ", "b "]))  # "a, b"
```

- Modules are found in the command's working directory, then `.stone/lib`
  under it, then `~/.stone/lib`, then `/usr/share/waymark/stone/lib`.
  `WAYMARK_STONE_LIB_DIRS` (an OS path list) replaces this search path.
- Imports are linked before the program runs. A missing module, a name the
  module does not define, an import cycle, or a module problem is reported
  before any statement executes. Imports go at the top level of a file.
- A module's top level only defines things: functions, records, error
  classes, and constants (`NAME = value`), plus imports and a docstring. It cannot call
  functions, loop, or branch at its top level, because importing a module
  runs no code.
- A module's functions see only the module's own names, its imports,
  builtins, error types, and the runtime's result types (`RunResult`,
  `TestRun`, ..., for `isinstance`). They never see the importing program's
  variables; using a name the module neither defines nor imports is an error.
  The importer's names never collide with the module's: a module's private
  `_clean` and the importer's `_clean` are different functions.
- Module constants are read-only all the way down. `DEFAULTS["tags"].append(x)`
  on an imported `DEFAULTS` is an error; copy it first.
- Imported functions and constants are ordinary names in the importer
  and can be renamed with `as`. Records and error classes are global and known
  by name, so they are imported under their own name, and two modules may not
  define the same one.
- A module's functions cannot read the program's `model`; pass it as a
  parameter (`def solve(task: str, model: Model)`).
- Not supported: `import lib` (write `from lib import name`), relative imports
  (`from . import x`), and `from lib import *`. Python's standard library is
  not available; the error names the Stone replacement.
- Successful results list each linked module's name, path, and SHA-256 under
  `diagnostics.modules`.

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
| `ModelOutputError` | `ToolError` | `model.ask(..., output=T)` got no answer of type `T` within its retries (host only) |
| `ModelProviderError` | `ToolError` | the model provider failed a call: a refusal such as HTTP 400, or a temporary failure (HTTP 429/5xx, a dropped connection) that lasted through 3 retries; fields `status`, `retryable`, `attempts` (host only) |
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

`ExpectationFailed`, `UndeclaredEffect`, `ModelOutputError`, and `ModelProviderError` are host-only. A program can catch
them, test them with `isinstance`, and re-raise one it caught (`raise` or
`raise error`, unchanged), but it cannot construct, raise, or subclass them,
and a modified copy cannot be raised. A caught host-only error is read-only:
`error["observed"] = ...` is an error. `isinstance(x, ExpectationFailed)` is
true only for an error the host raised, never for a dict that merely has
`type="ExpectationFailed"`. A parent deciding what to do after a failure can
therefore trust that the failure came from a checked call.

Error types are values. A bare type name such as `ValueError` is the type
itself, so types can be stored in lists and records and passed to functions.
As `type(error)` is in Python, `error.type` is the caught error's type, and it
compares equal only to that type:

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
`isinstance(True, int)` is true. `tuple` and `set` are their own types, so
`isinstance((1, 2), list)` is false. An unknown type name
is an error, so a misspelled type never silently fails to match. A record is
an error only if its `type` names a known error type, so
`isinstance({"type": "user"}, Exception)` is false.

A type never equals its name. Python answers `ValueError == "ValueError"` and
`error.type in ["KeyError"]` with a silent `False`; Stone raises, saying to
drop the quotes or use `isinstance`. Where a type leaves the program (emitted
JSON, an error record sent to another attempt, `error["type"]`) it is written
as its name, `"ValueError"`.

A caught error prints as Python prints it: `str(error)` and `f"{error}"` are
the message, and `repr(error)` is `ValueError('message')`. Its fields
(`error.message`, `error.code`, and any declared fields) stay available.

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
  type. `BaseException` does not exist in Stone (see "Control stops and
  denials" below). `IOError` is spelled `OSError`.
- A handler for `Exception` or `ToolError` whose body is only `pass` is
  rejected, because it discards failed commands, missing files, and failed
  model calls without a trace. Bind the error and use it, or catch the narrow
  type you expect. `except KeyError: pass` stays legal.
- A handler naming an unknown type is rejected with the list of builtin types.
- `except error:` (a lowercase name) does not bind `error` in Python or
  Stone. Stone rejects it with a correction to `except Exception as error:`.

Control stops and denials. The test is who decided:

- **Control stop: the host ends the attempt.** A budget or deadline is used
  up, the parent or host cancelled the attempt, or the attempt has already
  finished. The program stops, and no `except` handler runs, however broad.
  Runtime cleanup still happens: open attempt scopes are cancelled and
  joined, and frontiers are released. The Gateway refuses every further
  effect of a stopped attempt, so a program could not continue anyway.
- **Denial: one call is refused.** A pre-hook vetoes a `run(...)` or
  `model.ask(...)`, which returns `ok=False` with `kind="policy_rejected"`,
  or the host refuses one effect. Nothing ran, and the program can handle it
  like any other failure. Every denial is counted in the result's
  `diagnostics.denials` (`count` and the `last` one), whether or not the
  program noticed it. If the program fails, the count is attached to its
  error.

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

- `emit(value)`: set the program's structured result. The emitted value is the
  result even if statements run after it, and a second `emit` raises an error,
  because a program has one result. `emit` does not stop the program. A program
  that never emits returns the value of its final top-level expression
  statement (`1 + 1` gives 2); a final loop, branch, assignment, or `print`
  gives None. A function without `return` returns None. A function, the
  `model`, or an open file standing alone as a statement is not an error; as
  the final statement it gives its Python repr text (`<function f>`).
- `print(*values, sep=" ", end="\n")`: write `str()` of each value to stdout
  and return None, as Python does. What a program printed before it failed is
  kept in the error response's `output.stdout`.
- `fail(message, code=None, detail=None)`: raise an `Exception` that stops the
  program with the outer code `task_failure` (see below). Caught with
  `except Exception as e`, `e.code` is the declared `code`.
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
- `snapshot(path=None)`: save the state of a directory tree (the cwd by
  default) and return a `Snapshot` record with `root` and `id`. The id names
  the content (a Git tree id), so the same files give the same id. Files the
  tree's `.gitignore` files ignore (build outputs, caches) are not part of a
  snapshot and are never changed by one. Snapshots are kept in a Git object
  store Waymark owns, where commands run: `WAYMARK_SNAPSHOT_STORE`, default
  `~/.local/state/waymark/snapshots.git`. The store is used only as an object
  database through Git's plumbing commands; the directory's own `.git`
  (index, refs, HEAD, config, hooks) is never read or changed, apart from
  borrowing its objects so nothing is copied, and the directory need not be a
  Git repository. Needs the `git` program where commands run. A repeat
  snapshot of the Django repository (6644 files) takes about 0.13 s. A
  `Snapshot` also has `seq` (see `run()`).
- `diff(since=snapshot, to=None)`: what changed in the snapshot's root since
  it was taken (or up to the snapshot `to=`), as a `TreeDiff` record: `root`,
  `since` and `to` (snapshot ids), `files` (each `path` with `change`
  `"added"`, `"modified"`, `"deleted"`, or `"type_changed"`), and `patch`
  (Git-style patch text).
- `apply_patch(snapshot, patch)`: the `Snapshot` of `snapshot` with `patch` (a
  `diff(since=...)` result, patch text, or `None`) applied, built in the
  snapshot store without writing any file. `snapshot(root).id ==
  apply_patch(base, patch).id` shows that a directory is exactly `base` plus
  `patch` (ignored files aside). Raises `ValueError` if the patch does not
  apply.
- `search(root, needle, regex=False)`: content search. Literal search is the
  default fast path; pass `regex=True` for Rust-regex patterns. Results remain
  structured records with `path`, `line`, and `text`.
- `where(rows, key, expected)`, `where(rows, key, op, expected)`, or
  `where(rows, lambda r: ...)`: filter record lists by equality, comparison
  (`==`, `!=`, `>`, `>=`, `<`, `<=`, `in`, `not in`), or a predicate.
- `predicate(text)`: compile one lambda written as text, for checks that
  arrive as data (for example a field a model filled in), and return it as a
  function. The lambda must be pure: it may use only its own parameters (and
  names its comprehensions bind), literals, comparisons, `and`/`or`/`not`,
  arithmetic, indexing, attributes, f-strings, comprehensions, the builtins
  `abs all any first float int isinstance last len list max min repr
  reversed round set sorted str sum tuple unique`, and the methods `count
  endswith find get index isdigit items join keys lower lstrip replace rfind
  rstrip split splitlines startswith strip upper values`. It cannot run commands, call models,
  read files, or see the program's variables: run the command in the program
  and pass the result in. Text that does not parse, is not exactly one
  lambda, or uses anything else raises `ValueError` naming what is not
  allowed.

  ```python
  passes = predicate("lambda r: r.exit_code == 0 and 'ok' in r.stdout")
  result = run(["sh", "-c", "echo ok"])
  if passes(result):
      print("check passed")
  ```
- `abs(x)`: the absolute value of an int or float, as in Python; text and
  bools raise `TypeError`.
- `reversed(xs)`: a new list of the same items in reverse order (a dict gives
  its keys, a string its characters). Python returns an iterator; looping,
  `list(...)`, and indexing behave the same. A set raises `TypeError`.
- `sorted(xs, key=None, reverse=False)`: a sorted list, as in Python;
  `sorted(d)` gives a dict's keys in order and `sorted(text)` its characters.
- `any(iterable)`, `all(iterable)`, `sum(iterable)`, `join(iterable)`, and
  `set(iterable)`: aggregate lists or supported generator expressions with
  Stone truthiness, typed numeric semantics, or deterministic first-seen
  uniqueness.
- `set()` and `set(iterable)`: a set, as in Python. Members must be hashable
  (None, bools, numbers, strings, and tuples of those); a list member raises.
  Sets have `add`, `discard`, `remove`, `update`, `pop`, `clear`, `copy`,
  `union`, `intersection`, `difference`, `symmetric_difference`, `issubset`,
  `issuperset`, `isdisjoint`, the operators `|`, `&`, `-`, `^`, and `<=`/`<`
  for subsets. Iteration follows first-insertion order. A set cannot be
  indexed, and mixing bools with numbers in one set raises. `{1, 2}` and
  `{x for x in xs}` make sets too; `{}` is an empty dict, as in Python.
  `unique(xs)` gives a de-duplicated list instead. Lists have no `.add()`.
- `dict()`, `dict(d)`, `dict(pairs)`, and `dict(d, k=v)`: a dict, as in
  Python; `dict(d)` is a shallow copy, `pairs` is a list of `[key, value]`
  pairs, and keyword arguments add entries (later ones win). A record is not
  a dict: `dict(record)` raises `TypeError`; read its fields as attributes.
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
  `text.isalnum()`, `text.find("x")`, `text.rfind("/")`,
  `text.count("x")`, `",".join(items)`, `open(path).readlines()`,
  `items.extend(other)`, `items.count(value)`, and mutating
  `items.sort(key=..., reverse=...)` are also supported.
- `list(value)` makes a new list and `tuple(value)` an immutable tuple.
  Tuples (`(1, 2)`, `return a, b`) index, slice, iterate, unpack, compare
  element by element, join with `+`, and can be set members; they have
  `count` and `index` and cannot change. A tuple or set never equals a list:
  `(1, 2) == [1, 2]` raises (Python says `False`), and the error says to
  convert with `list(t)`. Emitted JSON and builtins that take plain data see
  tuples and sets as arrays; `enumerate`, `zip`, and `d.items()` give lists.
- Unpacking assigns to names, items, and fields alike, left to right after
  the value is computed, as in Python: `a, d["k"], plan.x = 1, 2, 3` and
  `i, xs[i] = 1, 5` (which sets `xs[1]`). Starred (`a, *rest = xs`) and
  nested (`(a, b), c = ...`) unpacking are rejected with what to write
  instead.
- `run(argv, cwd=None, stdin=None, files=None, timeout_ms=None, env=None,
  expect=None, effects=None, background=False, stdout="capture",
  stderr="capture", max_stdout_bytes=1048576, max_stderr_bytes=1048576,
  hooks={})`: Linux command execution with structured stdout, stderr, status,
  truncation flags, observed effects, and helper observations.
  `files={path: text}` writes those files (and their directories) just before
  the command, where it runs (locally or in the Gateway's container), relative
  paths against the command's cwd; the result's `files` lists each one's
  absolute `path`, `bytes`, and `sha256`, so a result records what the command
  ran on, and the files are not reported as the command's effects. Through the
  Gateway, where every command is a round trip, files up to 100 KB each (1 MB
  in all) are written by a step inside the command's own call; larger files
  take a call each. A file that cannot be written raises `OSError` naming it,
  and the command does not run. `must_run`, `run_complete`, and `run_tests`
  take `files=` too.
  Every result has `seq`, a number that increases with each result the
  runtime makes (`Snapshot` too), so results can be put in order. See "Call Contracts" below for `expect=`,
  `effects=`, and `result.effects`. Agent loops should
  normally set much smaller output bounds (for example 16 KiB/8 KiB) so one
  noisy command cannot dominate every later model call. Set `stdout` or
  `stderr` to `"suppress"`/`"discard"` when content is not evidence, or set
  `stderr="stdout"` to merge it. A timeout can return a `still_running` handle;
  terminate or wait for that handle explicitly before finishing the attempt.
  A call-local pre hook may veto or replace `argv`; a post hook observes the
  structured success/failure outcome.
- `must_run(argv, cwd=None, stdin=None, files=None, timeout_ms=None, env=None,
  expect=None, effects=None, stdout="capture", stderr="capture",
  max_stdout_bytes=1048576, max_stderr_bytes=1048576)`: checked Linux command
  execution for `set -e`-style scripts. `must_run(argv)` is
  `run(argv, expect=Expect(exit=0))`: it returns the run record on success and
  raises `ExpectationFailed`, with the run record in `error.result`, when the
  process exits nonzero, times out, or cannot start. Extra checks go in
  `expect=`.
- `run_tests(argv, runner="pytest", cwd=None, stdin=None, files=None, timeout_ms=None,
  env=None, max_stdout_bytes=1048576, max_stderr_bytes=1048576)`: runs a test
  command and returns its outcome as a `TestRun` record instead of text to
  scrape. `runner` is `"pytest"`, `"django"` (`tests/runtests.py`), or
  `"sympy"` (`bin/test`); the record reads the combined output, so a wrapper
  such as `["bash", "-lc", ...]` or `docker exec` works. Fields: `ok`,
  `runner`, `command`, `exit_code`, `timed_out`, `complete`, `passed`,
  `failed` (failures and errors), `skipped`, `xfailed`, `passed_tests`,
  `failed_tests`, `skipped_tests`, `problem`, and `output` (the last 4000
  characters). Counts come from the runner's final summary; `complete` is
  `False` when that summary is missing (a crash, a timeout, or a command that
  is not a run of that runner). `ok` is `True` only when the run is complete,
  at least one test passed, none failed, and the command exited 0; otherwise
  `problem` says why. Test names are the runner's own (pytest node ids,
  Django `name (dotted.path)`, SymPy `path::name`); passing tests are named
  when the runner lists them (`-rA`, `--verbosity 2`, `-v`).
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
- `model.ask(prompt, output=T, inputs=None, system="", retries=2, validate=None, hooks={}, ...)`: ask the
  Gateway model for an answer of type `T`, where `T` is a record, a union of
  records, or any annotation type (`list[str]`, `Literal["yes", "no"]`, ...).
  `prompt` is a string or a message list; `system` adds a system message, and
  the other `model_call` options (`model_class`, `model`, `temperature`,
  `max_output_tokens`, ...) pass through. Stone describes `T` to the model in
  a system message: by default as the pydantic class definitions of the
  records it uses (each after the records it uses, with docstrings, defaults,
  and `Field(...)` limits), or with `type_format="json_schema"` as a JSON
  Schema (nested records under `$defs`, docstrings as descriptions, no extra
  fields), about twice the size. On a live trial with Qwen3.8-27B the
  Python form was valid on the first try more often (44/44 against 40/44)
  with a third fewer input tokens. The answer is
  parsed (a Markdown code fence is tolerated) and converted: objects become
  records, a union of records is chosen by its `Literal` field (such as
  `kind: Literal["read"]`) or else by the first member that matches. A type
  whose values are not all objects is wrapped as `{"value": ...}` for the
  model, and the inner value is returned. A wrong answer is sent back with
  each problem and its path (`$.steps[1]: expected string, got 3`), up to
  `retries` times (at most 4); every attempt is an ordinary traced model call
  that hooks see. `validate=f` adds the program's own check of a well-typed
  answer: `f(answer)` returns a list of problem strings, empty (or `None`)
  when the answer is acceptable. Problems go back to the model as "The
  program checked your answer and found problems: ...", in the same retry
  loop and budget as type errors, so a program does not hand-write its own
  ask-check-repair loop. `f` is ordinary Stone code and may run commands
  (check a test against the repository, say); unlike `Expect(check=...)`, it
  need not be pure. An exception in `f` propagates; a non-list result is a
  `TypeError`. When no answer matches, `ModelOutputError` carries
  `expected` (the type), `issues` (each with `path` and `message`; a
  `validate=` problem also has `check: "validate"`), `attempts`, and the last
  `answer`.
  Every model call (`model.ask`, `model_call`) retries a temporary provider
  failure (HTTP 429 or 5xx, a dropped connection) up to 3 times, after 1, 4,
  and 16 seconds; a response that needed retries has `provider_retries`. A
  failure that persists, or a refusal such as HTTP 400, raises
  `ModelProviderError` with `status` (or `None`), `retryable`, and
  `attempts`.
  Without `output=`, `model.ask` returns the reply text. `model` is a value
  bound in every program; functions take it as a `model: Model` parameter.
  Every attempt, including repairs, counts against the model's budget
  ([Budgets](#budgets)). `model.last` is the most recent `model.ask` made through that model value,
  `{attempts, transition_ids, reply, ok, inputs, messages, reused_messages}`,
  or `None` before the first one; each program starts with a fresh `model`,
  and a `model.limit(...)` value keeps its own. `messages` counts the
  request's messages on the first attempt, system messages included, and
  `reused_messages` how many of them, from the start, were identical to the
  previous ask's: the part a provider's prompt cache can reuse. See
  `help("model.ask")`.
- **Named inputs.** `model.ask(text, inputs={"issue": issue, "fix": fix})`
  and `message(text, inputs={...}, role="user")` take the data a model step
  works from as named values, and the runtime turns them into text: one
  `<input name="..." type="...">` block per input, in the order given, then
  the text. A string is shown as it is; records, dicts, lists, and runtime
  results as indented JSON, with record fields in declaration order, dict
  keys in insertion order, and the fields of runtime results that differ
  from run to run (`seq`, `transition_id`, `duration_ms`, `sha256`) left out.
  So the same values always give the same bytes, and a provider that reuses
  a prompt's processed start (a prompt cache) can reuse it across calls.
  `message(...)` returns a read-only `Message` (`role`, `content`, `inputs`)
  for a message list; its text is fixed when it is built, so changing a dict
  afterwards changes nothing already built. Plain
  `{"role": ..., "content": ...}` dicts still work beside it; a dict message
  cannot carry `inputs`. With a message list, `inputs=` is an error: give
  each message its own. Names are identifiers, at most 32 inputs per message,
  and a rendered message is at most 1 MB.
  Each input is recorded in the `Message` and in `model.last.inputs` as
  `{name, type, source}` (plus `message`, its index in the prompt), where
  `source` is `{"kind": "runtime", "type": ...}` for a result only the
  runtime makes (or a list of them), `{"kind": "model", "model": ...,
  "transition_id": ...}` for a typed `model.ask` answer or a record or
  container inside one, and `{"kind": "program"}` otherwise. Sources do not
  follow computation: a dict the program builds from a model answer's
  fields is `program`, and so is a plain string or number a model returned.
  Keep `system=` a fixed instruction and put task data in inputs; append to
  a history rather than editing it. Both keep the start of the prompt the
  same from call to call. See `help("message")`.
  Before the program runs, Stone warns in `diagnostics.warnings` when a
  function takes a record parameter (`def review(fix: Fix)`, or a runtime
  result such as `RunResult`) and makes a model call, but nothing computed
  from that parameter reaches any of its model calls: "review() receives
  fix: Fix, but its model call never sees it". This is the usual defect of a
  review step that starts a fresh prompt. The warning is not given when the
  parameter goes anywhere the check does not follow (another function, a
  check such as `run_tests`, a condition, `return`), or when the function's
  model call continues a conversation the caller passed in (an unannotated,
  list, or dict parameter).

  ```python
  class Verdict(Record):
      correct: bool
      reason: str

  tests = run_tests(["pytest", "-q", "tests/test_parse.py"])
  verdict = model.ask("Does the fix resolve the issue?", output=Verdict,
                      system="You review fixes.",
                      inputs={"issue": issue, "fix": fix, "tests": tests})
  # model.last.inputs: issue (program), fix (model, if it came from
  # model.ask), tests (runtime, TestRun)
  ```

  ```python
  class Read(Record):
      kind: Literal["read"]
      path: str

  class Finish(Record):
      kind: Literal["finish"]
      summary: str

  action = model.ask(history, output=Read | Finish,
                     system="Choose the next action.")
  if isinstance(action, Read):
      history.append({"role": "user", "content": read_text(action.path)})
  ```
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
  and `"users"`. `sys()` is an alias. Sizes are ints in bytes and
  `load_average` is a list of three floats.
- `echo(value, ...)`: returns its argument (a list for several) and prints
  nothing; it is not shell `echo`. Use `print` to write text.
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
