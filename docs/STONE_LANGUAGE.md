<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Language

Stone is the structured shell language used by Waymark Shell. It intentionally looks
Python-shaped for control flow and literals, but the builtins are shell and data
operations rather than Python standard-library APIs.

## Running Code

```sh
waymark eval -c 'emit({"ok": True})'
waymark eval script.stone
waymark eval --stdin-script < script.stone
```

Use `--nu` only for the compatibility frontend. Stone is the default for
`waymark eval -c`, `.stone` files, and `--stdin-script`.

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

Truthiness follows Python's common data model: `False`, `None`, numeric zero,
empty strings, empty lists, and empty records are falsey; non-empty values and
non-zero numbers are truthy. Boolean operators short-circuit and return booleans.

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

Default expressions are evaluated when a call omits that argument. Mutable
default literals such as `[]` and `{}` are rejected to avoid shared mutable
state surprises.

`try` / `except` catches runtime evaluation errors, not parse or lowering
errors:

```python
try:
    text = read_text(path)
except Exception as e:
    text = ""
    emit({"warning": e.message, "code": e.code})
```

Supported handlers are `except:`, `except Exception:`, and
`except Exception as name:`. A bound error is a Stone record with stable
`kind`, `code`, and `message` fields. I/O errors also include `path` and
`operation` when available.

## Core Builtins

- `emit(value)`: return a structured success value.
- `fail(message)`: return a structured failure.
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
- `str(value)` and `repr(value)`: convert a typed value to display text.
  `repr()` is a Python-compatibility alias; use `json_dumps(value)` when valid
  JSON text is required.
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
- `run(argv, cwd=None, stdin=None, timeout_ms=None, env=None)`: Linux command
  execution with structured stdout, stderr, status, and helper observations.
- `must_run(argv, cwd=None, stdin=None, timeout_ms=None, env=None)`: checked
  Linux command execution for `set -e`-style scripts. It returns the same
  structured record as `run()` on success, and raises a Stone error with the run
  record attached when the external process exits nonzero or times out.
- `wait_port(port, host="127.0.0.1", timeout_ms=30000, protocol="tcp")`:
  wait for a TCP port to accept connections, or for a UDP endpoint to accept a
  datagram send probe. UDP has no connection handshake, so use a real protocol
  client for final validation.
- `pwd()`, `cd(path)`: inspect or change the current Stone working directory
  for the current session.
- `state()`: typed runtime snapshot with `cwd`, git summary, and common tool
  availability/version probes.
- `last_result()`: previous Waymark command response as typed data, or `None`
  before any previous response in the current runtime process.
- `help()`, `help("topic")`: builtin help.

## Result Shape

Successful CLI evaluation prints:

```json
{"ok":true,"cwd":"/path","value":{},"output":{"stdout":"","stderr":""}}
```

Failures print a JSON envelope to stderr with `ok: false` and structured error
fields such as `kind`, `code`, `message`, and path/span details when available.
