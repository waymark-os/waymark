<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone MCP Adapter

This directory contains the host-side MCP adapter for exposing Waymark Shell/Stone
to coding agents.

## Server

Run the MCP server over stdio:

```sh
WAYMARK_STONE_BIN=/workspace/waymark/target/x86_64-unknown-linux-musl/release/waymark \
WAYMARK_STONE_CWD=/app \
WAYMARK_STONE_MCP_TRACE=/trace/stone-mcp.jsonl \
python3 /workspace/waymark/host/mcp/stone_mcp_server.py
```

The default backend is `warm-stdio`, which starts one long-lived
`waymark --task-server-stream` process and reuses it across `stone_eval`
calls. Warm evals keep the workspace durable across calls; work-dir reset is an
explicit guest protocol command, not part of normal task/eval handling.
Stone top-level value and function bindings also persist across warm eval calls,
so agents can bind expensive intermediate data once and reuse it in the next
call. Large emitted values are returned as a compact peek by default; use
`head`/`tail`/`len` summaries or set `allow_large_output` to force the full
value. Open file handles remain task-owned and are not carried across calls.
Set `WAYMARK_STONE_MCP_BACKEND=subprocess` for one-shot debugging.
Runtime state comes from Stone itself: call `stone_call` with `state` for cwd,
git, and tool snapshots, or `last_result` to recover the previous Waymark
response in the warm runtime.

## Codex Config

Generate a Codex `config.toml` for an eval container:

```sh
python3 /workspace/waymark/host/mcp/write_codex_stone_mcp_config.py \
  --out /tmp/codex-home/config.toml \
  --server /workspace/waymark/host/mcp/stone_mcp_server.py \
  --waymark-bin /workspace/waymark/target/x86_64-unknown-linux-musl/release/waymark \
  --cwd /app \
  --trace /trace/stone-mcp.jsonl
```

Use it by setting `CODEX_HOME=/tmp/codex-home` before launching Codex.

The generated config registers `[mcp_servers.stone]` with the Stone server command
and the environment variables needed by the selected backend.

For local or container dry-runs, use the launcher helper:

```sh
python3 /workspace/waymark/host/mcp/run_codex_stone_mcp.py \
  --workspace /app \
  --waymark-bin /workspace/waymark/target/debug/waymark \
  --check
```

Without `--exec`, this only prepares `CODEX_HOME`, optionally verifies
`codex mcp list`, and prints a JSON summary. Add `--exec` to run
`codex exec` with the generated config.

## Smoke

Protocol smoke:

```sh
python3 host/mcp/smoke_stone_mcp_server.py --waymark-bin target/debug/waymark
```
