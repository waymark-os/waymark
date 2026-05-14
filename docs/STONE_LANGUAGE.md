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

Comparisons support numeric ordering across ints and floats, lexicographic
string ordering, and lexicographic list ordering. Equality is recursive for
lists and records, numeric across ints/floats, and strongly typed for booleans:
`2 == 2.0` is true, but `True == 1` is false. Ordering unrelated types, such as
`"1" < 2`, is an error.

Membership supports strings, lists, and record keys: `"mark" in "waymark"`,
`2 in [1, 2, 3]`, and `"path" in record`. Other containers are an error.

## Core Builtins

- `emit(value)`: return a structured success value.
- `fail(message)`: return a structured failure.
- `read_text(path)`, `write_text(path, text)`: text file I/O.
- `read_json(path)`, `write_json(path, value)`: JSON file I/O.
- `read_jsonl(path)`, `write_jsonl(path, rows)`: JSONL file I/O.
- `read_csv(path)`: CSV records.
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
- `split(text, separator=None)`, `join(items, separator="")`,
  `slice(value, start=None, end=None)`, `starts_with(text, prefix)`, and
  `format(template, ...)`: small text/list helpers for scripts and dynamic
  helper callbacks. String method forms such as `"a,b".split(",")` and
  `",".join(items)` are also supported.
- `run(argv, cwd=None, stdin=None, timeout_ms=None, env=None)`: Linux command
  execution with structured stdout, stderr, status, and helper observations.
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
