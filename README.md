<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Waymark Shell

Waymark Shell is a structured shell for coding agents and automation. Stone is
the language primarily designed for LLMs to program Waymark attempts: programs
read and write files, run Linux commands, transform structured data, call
models, manage context, compose child attempts, and return typed results.
Humans remain important readers and debuggers, but are not Stone's primary
author.

This repository contains the current `waymark` shell implementation. It is
designed to run as an ordinary POSIX-oriented process on the host, in containers,
and inside agent sandboxes that provide the expected filesystem and process
interfaces.

Waymark Shell is still a preview implementation. The current runtime deliberately
reuses selected Nushell components for typed values and JSON conversion while
Stone and the public `waymark` surface evolve.

## Design Philosophy

An operating system exists to make computing resources easy to use. The earliest
interfaces were mostly syscall and ABI contracts for programs. Shells emerged as
the user-facing layer for humans: an interactive language for composing files,
processes, devices, and programs.

Waymark starts from the same principle, but changes the assumed user. The
primary user is a coding agent, not a human sitting at a terminal. That changes
the shell contract: the interface should make context easy to retrieve, reduce
probing, avoid fragile quoting and escaping, expose typed state, and make common
errors recoverable.

In the larger Waymark architecture, an attempt plays the role of an OS process
and Stone is the language an LLM uses to program that process abstraction. A
task-specific agent harness is therefore an ordinary Stone attempt program,
not a separate runtime layer.

Stone uses Python-shaped syntax for the parts agents and humans write most often:
literals, records, lists, loops, conditionals, and small functions. The goal is
familiar control flow without inheriting a full general-purpose runtime or
guessing at host-language standard-library behavior. The builtins are shell and
data operations: read files, write files, parse JSON/CSV/JSONL, run commands,
and emit structured results.

Expression behavior is intentionally Python-like where that avoids surprises:
zero and empty values are falsey, signed `//` and `%` follow Python's floor and
remainder rules, comparisons and membership use familiar syntax, and boolean
expressions short-circuit. Operators remain strongly typed: text is not
implicitly parsed as a number for arithmetic or bitwise operations; use explicit
conversions such as `int(...)` or `float(...)` at data boundaries.

Waymark Shell favors typed values over string pipelines. Commands and scripts pass
records, lists, booleans, numbers, and nulls directly instead of repeatedly
serializing and reparsing text. Text still matters at process boundaries, but it
is not the only internal transport. This keeps agent work inspectable: a file
write returns a record, a command returns stdout/stderr/status plus diagnostics,
and `emit(...)` returns a JSON value rather than a scraped terminal transcript.

Structured I/O is the integration contract. The CLI, MCP adapter, and helpers
all use the same result envelopes, so observations can be attached without
changing the task script. Helpers register lifecycle hooks for events such as
`run.after_failure` and add targeted diagnostics to command results. That makes
project-specific debugging extensible while keeping Stone scripts small and
portable.

## Quick Start

Build and test the shell:

```sh
cargo build -p waymark
cargo test -p waymark
```

Run Stone directly:

```sh
target/debug/waymark eval -c 'emit({"ok": True})'
```

Run a script file:

```sh
target/debug/waymark eval examples/scripts/hello.stone
```

Use an explicit writable start directory when running from environments that
prefer `/workspace`:

```sh
WAYMARK_START_DIR="$PWD" target/debug/waymark eval -c 'write_json("out.json", {"ok": True})'
```

The command prints one JSON result envelope to stdout on success and one JSON
error envelope to stderr on failure.

## Stone Example

```python
write_text("target/example.txt", "hello\n")
text = read_text("target/example.txt")
result = run(["wc", "-c", "target/example.txt"])
emit({
    "text": text,
    "wc_stdout": result["stdout"],
})
```

Save that as `example.stone`, then run:

```sh
target/debug/waymark eval example.stone
```

Use `state()` when an agent needs the current cwd, workspace root, git summary,
or common tool availability without ad hoc probes. Use `last_result()` to
recover the previous Waymark response in a long-lived runtime process.

In long-lived runtime processes, Stone top-level value and function bindings
persist across eval calls. This lets an agent bind intermediate data once,
inspect it, and continue in the next call without rereading or JSON-caching it.
For large values, bind the value and emit compact summaries such as
`emit({"count": len(rows), "sample": head(rows, 5)})`; MCP `stone_eval`
previews large emitted values unless `allow_large_output` is set. One-shot CLI
invocations still start fresh.

## Helpers

Stone helpers are dynamic extension scripts. The shell loads helper registrations
from these directories, in order:

- `<current Stone cwd>/.stone/helpers`
- `~/.stone/helpers`
- `/usr/share/waymark/stone/helpers`

Set `WAYMARK_STONE_HELPER_DIRS` to an OS path list to replace that default search
path. Helper observations are attached to `run(...)` results under `helpers`.

Checked-in helper examples live in [.stone/helpers](./.stone/helpers). See
[docs/STONE_HELPERS.md](./docs/STONE_HELPERS.md) for the public helper guide.

## MCP Adapter

MCP is an adapter for current coding-agent ecosystems. It is not the shell
runtime. The adapter calls the same `waymark`/Stone execution path used by the
CLI.

See [docs/MCP_ADAPTER.md](./docs/MCP_ADAPTER.md) and
[host/mcp/README.md](./host/mcp/README.md).

## Runtime Boundary

Build the shell with Cargo:

```sh
cargo build -p waymark
cargo build --release -p waymark --target x86_64-unknown-linux-musl
```

Use the musl target as the default release artifact for distribution. The normal
debug build remains useful for local development.

The default build should stay lightweight and run anywhere a normal POSIX-style
process can run. Container, sandbox, and bridge integrations should remain
explicit adapter boundaries rather than becoming required for `cargo build -p
waymark`.

See [docs/BUILD_INSTALL.md](./docs/BUILD_INSTALL.md) for dependencies, release
builds, and installation notes.

## Repository Layout

- `crates/waymark`: CLI binary
- `crates/waymark-runtime`: shared shell runtime
- `crates/waymark-runtime-support`: small process/startup support utilities
- `.stone/helpers`: checked-in helper examples
- `host/mcp`: MCP adapter for coding agents
- `docs`: public shell, language, helper, and adapter docs

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE))
- MIT license ([LICENSE-MIT](./LICENSE-MIT))

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work shall be dual licensed as above, without any
additional terms or conditions.
