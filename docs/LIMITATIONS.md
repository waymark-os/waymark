<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Preview Limitations

- The runtime code is still split between `crates/waymark` and
  `crates/waymark-runtime`; deeper crate cleanup is future work.
- Stone helpers are loaded from trusted local directories. They are not a
  security sandbox.
- The MCP server is an adapter for existing agents. Direct shell-agent
  integration remains a future direction.
