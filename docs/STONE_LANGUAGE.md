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

## Core Builtins

- `emit(value)`: return a structured success value.
- `fail(message)`: return a structured failure.
- `read_text(path)`, `write_text(path, text)`: text file I/O.
- `read_json(path)`, `write_json(path, value)`: JSON file I/O.
- `read_jsonl(path)`, `write_jsonl(path, rows)`: JSONL file I/O.
- `read_csv(path)`: CSV records.
- `find(root, name_glob="*")`: file discovery.
- `run(argv, cwd=None, stdin=None, timeout_ms=None, env=None)`: Linux command
  execution with structured stdout, stderr, status, and helper observations.
- `help()`, `help("topic")`: builtin help.

## Result Shape

Successful CLI evaluation prints:

```json
{"ok":true,"cwd":"/path","value":{},"output":{"stdout":"","stderr":""}}
```

Failures print a JSON envelope to stderr with `ok: false` and structured error
fields such as `kind`, `code`, `message`, and path/span details when available.
