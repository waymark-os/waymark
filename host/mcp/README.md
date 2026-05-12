# Stone MCP Adapter

This directory contains the host-side MCP adapter for exposing Waymark Shell/Stone
to coding agents.

## Server

Run the MCP server over stdio:

```sh
WAYMARK_STONE_BIN=/workspace/waymark/target/debug/waymark \
WAYMARK_STONE_CWD=/app \
WAYMARK_STONE_MCP_TRACE=/trace/stone-mcp.jsonl \
python3 /workspace/waymark/host/mcp/stone_mcp_server.py
```

The default backend is `warm-stdio`, which starts one long-lived
`waymark --task-server-stream` process and reuses it across `stone_eval`
calls. Warm evals keep the workspace durable across calls; work-dir reset is an
explicit guest protocol command, not part of normal task/eval handling.
Set `WAYMARK_STONE_MCP_BACKEND=subprocess` for one-shot debugging.

## Codex Config

Generate a Codex `config.toml` for an eval container:

```sh
python3 /workspace/waymark/host/mcp/write_codex_stone_mcp_config.py \
  --out /tmp/codex-home/config.toml \
  --server /workspace/waymark/host/mcp/stone_mcp_server.py \
  --waymark-bin /workspace/waymark/target/debug/waymark \
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
