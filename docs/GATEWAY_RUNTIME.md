<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Gateway Runtime

Waymark can run as an unprivileged shell/runtime client of the host Gateway.
In that placement, the Gateway owns workspace transactions, Linux execution,
model credentials, and policy. The Waymark process receives only a narrow
runtime config and calls Gateway RPC.

## Activation

Gateway runtime mode is active when the process has a Gateway endpoint and
transaction:

- `WAYMARK_GATEWAY_SOCKET`, or `WAYMARK_GATEWAY_VSOCK_CID` plus
  `WAYMARK_GATEWAY_VSOCK_PORT`
- `WAYMARK_GATEWAY_TX`

Optional config:

- `WAYMARK_GATEWAY_ATTEMPT_ID`: attempt identity for the runtime; when present,
  the runtime calls `attempt.channel_attach` after opening the Gateway RPC
  stream
- `WAYMARK_GATEWAY_CONTROLLER_RUN`: controller-run identity for
  `attempt.channel_attach`; if omitted, the runtime falls back to
  `WAYMARK_ATTEMPT_PROCESS_RUN` when launched by Gateway's process supervisor
- `WAYMARK_GATEWAY_IMAGE`: Linux sidecar image for `run`/agent tool execution
- `WAYMARK_GATEWAY_CONTAINER`: existing container target, when used
- `WAYMARK_GATEWAY_WORKSPACE_MOUNT`: guest/container workspace mount, default
  `/app`
- `WAYMARK_GATEWAY_MODEL_CLASS`: default model class for agent tasks, default
  `agent`
- `WAYMARK_GATEWAY_MODEL_CAPABILITY_PROFILE`: provider-neutral model capability
  selector

## Agent Tasks

Model-backed `frontend: "agent"` tasks use the active Gateway runtime as their
model gateway when no explicit in-process test or stream gateway was supplied.
The runtime converts the agent request into Gateway `model.call` protobuf:

- chat messages become `ModelMessage`
- `model` becomes a provider hint
- `model_class` or `WAYMARK_GATEWAY_MODEL_CLASS` selects the policy class
- `temperature`, `top_p`, `seed`, and `max_output_tokens`/`max_tokens` map to
  sampling fields
- `response_format` is passed through as a string
- string metadata is preserved with `source=waymark-runtime`

The model provider key stays in the host Gateway. Waymark receives only the
provider-neutral response text, resolved model metadata, finish reason, latency,
and token usage when the provider reports it.

## Tool Calls

In Gateway runtime mode, agent tool calls use the same host-authority boundary:

- workspace `read`, `write`, and `list` call Gateway transaction RPCs for the
  active `WAYMARK_GATEWAY_TX`
- Linux `run` calls Gateway `linux.exec` with the configured image/container
  and workspace mount

This preserves the existing agent tool response shape while moving authority
for files, subprocesses, and credentials out of the unprivileged runtime.

The built-in agent-loop helper keeps one Gateway RPC client open across a model
episode. That matters for LibOS placement because Firecracker guest-initiated
vsock connections are relatively expensive and repeated connect/close cycles
can be less reliable than reusing the task-runtime channel.

When `WAYMARK_GATEWAY_ATTEMPT_ID` is set, that persistent client first attaches
the stream to the attempt. Gateway then treats the stream as the attempt
control channel: scoped calls default to the attached attempt/transaction, and
non-matching attempt or tx fields are rejected by Gateway policy.

## Task Server

The task-server stream still supports `model_request`, `workspace_request`, and
`linux_request` frames for non-Gateway harnesses. When Gateway runtime mode is
active, the task server handles model-backed agent tasks through direct Gateway
RPC instead of relaying model/tool requests over the task stream.
