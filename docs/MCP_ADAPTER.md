<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# MCP Adapter

The MCP server is an adapter that exposes Stone to current coding-agent clients.
It is not the core runtime. Waymark Shell is the shell, Stone is the language, and
MCP is one transport.

## Build

```sh
cargo build --release -p waymark --target x86_64-unknown-linux-musl
```

## Run The Adapter

```sh
WAYMARK_STONE_BIN="$PWD/target/x86_64-unknown-linux-musl/release/waymark" \
WAYMARK_STONE_CWD="$PWD" \
WAYMARK_STONE_MCP_TRACE="$PWD/target/stone-mcp.jsonl" \
python3 host/mcp/stone_mcp_server.py
```

The default backend is `warm-stdio`, which keeps one shell process alive and
reuses it across calls. Set `WAYMARK_STONE_MCP_BACKEND=subprocess` for one-shot
debugging.

With the warm backend, Stone behaves like a shell session. Top-level value and
function bindings persist across eval calls, so agents can bind intermediate
records/lists once and refer to them later. One-shot subprocess mode starts fresh
for each call. MCP `stone_eval` previews large emitted values by default; bind
large values and emit `len`/`head`/`tail` summaries, or set
`allow_large_output` when the full structured value is required.

## Smoke

```sh
python3 host/mcp/smoke_stone_mcp_server.py --waymark-bin target/debug/waymark
```

See [../host/mcp/README.md](../host/mcp/README.md) for Codex-specific launcher
helpers and tracing options.
