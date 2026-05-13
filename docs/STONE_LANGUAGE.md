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
emit(len(errors))
```

This is not a JSON result cache. Values are kept in the runtime session when they
are safe to carry across calls. Open file handles are task-owned and do not
persist across eval calls. For large values, keep the binding live and emit
small summaries with `len`, `head`/`first`, or `tail`/`last` unless the caller
explicitly asks for full output.

## Core Builtins

- `emit(value)`: return a structured success value.
- `fail(message)`: return a structured failure.
- `read_text(path)`, `write_text(path, text)`: text file I/O.
- `read_json(path)`, `write_json(path, value)`: JSON file I/O.
- `read_jsonl(path)`, `write_jsonl(path, rows)`: JSONL file I/O.
- `read_csv(path)`: CSV records.
- `find(root, name_glob="*")`: file discovery.
- `split(text, separator=None)`, `join(items, separator="")`,
  `slice(value, start=None, end=None)`, `starts_with(text, prefix)`, and
  `format(template, ...)`: small text/list helpers for scripts and dynamic
  helper callbacks. String method forms such as `"a,b".split(",")` and
  `",".join(items)` are also supported.
- `run(argv, cwd=None, stdin=None, timeout_ms=None, env=None)`: Linux command
  execution with structured stdout, stderr, status, and helper observations.
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
