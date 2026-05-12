# Architecture

Waymark Shell is split into three layers:

- Shell runtime: Stone parsing, evaluation, structured values, file APIs,
  command execution, and helper dispatch.
- CLI: `waymark eval` and `waymark help`.
- Adapters: MCP and other agent-facing integrations that call the same runtime
  path.

`crates/waymark-runtime` hosts most runtime code. The public
`crates/waymark` package provides the CLI boundary, and
`crates/waymark-runtime-support` contains small startup/process utilities shared
by the CLI and runtime.
