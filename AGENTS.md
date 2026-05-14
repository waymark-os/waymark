# AGENTS.md

This repo is the current Waymark Shell implementation. Keep it focused on the
shell, Stone language, structured I/O, helpers, MCP adapter, and public docs.

## Purpose

- make coding-agent access to computing resources easier and less error-prone
- keep the default `waymark` build lightweight and usable as a normal
  POSIX-oriented process on the host, in containers, and in agent sandboxes
- preserve typed values and structured outcomes across the CLI, runtime,
  helpers, task specs, and MCP adapter
- document design decisions that affect future agent work

Waymark is the broader agent OS direction. Waymark Shell is the current shell
surface. Stone is the script language. The public command is `waymark`.

## Repo Layout

- `crates/waymark`: CLI binary
- `crates/waymark-runtime`: Stone parser/runtime, tools, tasks, and server loop
- `crates/waymark-runtime-support`: startup/process support utilities
- `crates/asset-image`: small checked-in support crate
- `.stone/helpers`: dynamic Stone helper registrations for command diagnostics
- `host/mcp`: MCP adapter for coding-agent clients
- `adapters/mcp`: release-facing adapter boundary note
- `examples/scripts`: small Stone scripts
- `examples/tasks`: checked-in task specs
- `docs`: public architecture, language, helper, and MCP docs
- `vendor/dirs`: local patch for the `dirs` crate

## Design Boundary

- Add Stone syntax, runtime, helper, MCP, CLI, and public shell-doc changes here.
- Keep heavyweight execution substrates and sandbox/container integrations
  behind explicit adapter boundaries.
- Do not add RustPython back to the default shell path.
- Do not make MCP the core runtime. MCP is an adapter over the same Stone
  execution path used by the CLI.
- Do not turn Stone into a thin wrapper around string pipes. Prefer typed
  values, structured records, explicit effects, and recoverable diagnostics.
- Do not introduce a separate shell command name without a documented reason.
  The public command is `waymark`.

## Agent-Facing Constraints

Design for coding agents with bounded context and imperfect long-context
reasoning:

- make current state cheap to retrieve and summarize
- expose cwd, workspace roots, Git/task state, recent effects, and helper
  observations through structured APIs where possible
- reduce probing by providing schemas, state snapshots, and clear tool results
- avoid quote-heavy Bash, nested `python -c`, and JSON-in-string contracts when
  typed calls can express the same intent
- return structured cause, evidence, and suggested next action for recoverable
  errors
- make destructive actions explicit and auditable

## Editing Notes

- Prefer small, explicit changes that match existing runtime patterns.
- If changing public command names, helper search paths, env vars, task schema,
  or MCP names, update `README.md`, `docs/`, and relevant tests in the same
  change.
- Stone helpers are discovered from `.stone/helpers` under the command cwd,
  then `~/.stone/helpers`, then `/usr/share/waymark/stone/helpers`; set
  `WAYMARK_STONE_HELPER_DIRS` to override the helper search path.
- Keep public docs aligned with actual behavior. Do not document aspirational
  features as present.
- When adding or changing Stone builtins or language features, update `help()`
  in the same change and run `python3 host/check_stone_help_examples.py` after
  rebuilding `waymark`. Every builtin must have help and every help example
  must execute.
- Add focused tests for new behavior. Prefer assertions on structured values and
  error envelopes over terminal transcript matching.

## Useful Checks

```sh
cargo check -p waymark
cargo test -p waymark
cargo test -p waymark-runtime --lib
cargo test -p waymark-runtime-support
python3 host/check_stone_help_examples.py
python3 host/mcp/test_stone_mcp_server.py
python3 host/mcp/test_run_codex_stone_mcp.py
python3 host/mcp/test_write_codex_stone_mcp_config.py
target/debug/waymark eval examples/scripts/hello.stone
```
