<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

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

This is a preview implementation. It currently reuses selected Nushell crates
for typed values, engine state, and JSON conversion. Stone and the public
`waymark` command are the product surface; Nushell reuse is an implementation
choice, not a separate user-facing shell contract.
