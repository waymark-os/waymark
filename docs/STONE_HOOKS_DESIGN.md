<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Hooks Design

## Goal

Stone hooks let the attempt program customize the Shell runtime around task
events, tool events, and verifier events.

The current implementation already has helper-file hooks for selected
`run(...)` post-processing. That proved the value of structured feedback:
instead of returning only stdout/stderr, Shell can attach observations,
diagnoses, and suggested next checks.

The next version should generalize that into:

- more hook points
- attempt/task-aware event records
- agent-writable custom callbacks
- scoped registration for one attempt or one Stone session
- predictable ordering, limits, and tracing

Hooks should make the computer easier for agents to use without giving hooks
new authority. A hook runs with the same Shell/Gateway capabilities as the
attempt program that registered it.

## Existing V0

See [Stone Helpers](STONE_HELPERS.md).

V0 helper files can register hooks such as:

```stone
def python_after_failure(event):
    return {
        "kind": "python_failure",
        "summary": event["stderr"],
        "next_checks": [["python3", "-m", "pip", "check"]],
    }

hook(
  "run.after_failure",
  family="python",
  argv0_prefix=["python"],
  handler="python.after_failure",
  priority=100,
)
```

V0 is intentionally narrow:

- hooks are centered on `run.after_success`, `run.after_failure`, and
  `run.after_timeout`
- matchers are mostly process-command family filters
- helper discovery is file based
- callbacks are used mainly for diagnostics

The design below keeps that compatibility but turns hooks into a first-class
Stone runtime concept.

## Hook Model

A hook registration binds:

```text
event name
matcher
handler callback
priority
scope
limits
```

Conceptual record:

```text
Hook {
  id
  event
  matcher
  handler
  priority
  scope
  limits
}
```

Handlers receive one event record and may return:

```text
None
observation record
list[observation record]
replacement/decision record for before hooks
```

Post hooks should usually return observations. Pre hooks may return decisions
such as `allow`, `deny`, `rewrite`, or `warn`, but Gateway remains the final
authority for access control.

## Registration APIs

Keep helper-file registration:

```stone
hook("run.after_failure", handler="python.after_failure", argv0_prefix=["python"])
```

Add runtime registration for agents:

```stone
def explain_test_failure(event):
    if contains(event.stderr, "AssertionError"):
        return {
            "kind": "test_failure_hint",
            "summary": "pytest assertion failed",
            "evidence": {"stderr_tail": event.stderr_tail},
            "next_checks": [["pytest", "-q", "-vv"]],
        }
    return None

register_hook(
  "run.after_failure",
  handler=explain_test_failure,
  matcher={"argv0": ["pytest"]},
  priority=100,
  scope="attempt",
)
```

Potential builtin forms:

```text
register_hook(event, handler, matcher={}, priority=0, scope="session", limits={}) -> hook_id
unregister_hook(hook_id) -> bool
hooks(event="") -> list[record]
hook_trace(limit=100) -> list[record]
```

`hook(...)` can remain the declarative helper-file form. `register_hook(...)`
is the programmatic form for attempt code.

## Scopes

Hooks need explicit lifetime:

```text
call       only for one operation
session    current warm Stone session
attempt    current Gateway attempt
task       current task runtime view
workspace  project helper file, loaded from .stone/helpers
user       user helper directory
system     checked-in or installed helper directory
```

Agent-written hooks should default to `attempt` or `session`, not user/system.
An attempt should not silently install persistent user hooks.

## Event Record

Every hook event should include common fields:

```json
{
  "event": "run.after_failure",
  "hook_phase": "after",
  "attempt": "attempt-...",
  "task": {"objective": "..."},
  "operation": "run",
  "timestamp_ms": 123,
  "cwd": "/app",
  "capabilities": {"network": false},
  "trace_id": "..."
}
```

Tool-specific fields extend that common envelope. For `run.after_failure`:

```json
{
  "argv": ["pytest", "-q"],
  "argv0": "pytest",
  "status": 1,
  "stdout": "...",
  "stderr": "...",
  "stdout_tail": "...",
  "stderr_tail": "...",
  "duration_ms": 2310,
  "timed_out": false,
  "suggested_actions": []
}
```

The event should include enough structured data for handlers to reason without
parsing human-formatted Shell output.

## Hook Points

Start with a small stable set and expand deliberately.

### Process Hooks

```text
run.before
run.after_success
run.after_failure
run.after_timeout
run.after_spawn_error
run.after_background_start
run.after_background_exit
```

Use cases:

- add diagnostics after a failed compiler/test command
- detect missing libraries, missing Python modules, bad permissions
- suggest longer timeout or background daemon APIs
- attach concise feedback to long outputs

### Filesystem Hooks

```text
fs.before_read
fs.after_read
fs.before_write
fs.after_write
fs.before_remove
fs.after_remove
fs.after_diff
```

Use cases:

- warn when reading huge or binary files
- normalize generated output
- check writes against declared task outputs
- flag accidental edits to declared inputs

Filesystem hooks must not become a bypass around Gateway transaction policy.
Gateway still owns canonical commit/rollback semantics.

### Task Hooks

These depend on Gateway providing `task_spec` and `TaskRuntimeView`.

```text
task.before_start
task.after_start
task.before_check
task.after_check
task.before_result
task.after_result
```

Use cases:

- render task-specific prompt/context
- generate task-specific Shell affordances
- run cheap local checkers before an expensive verifier
- transform a result into the declared schema

### Attempt Hooks

These are useful when Shell runs inside a Gateway attempt.

```text
attempt.after_spawn
attempt.before_finish
attempt.after_finish
attempt.after_child_result
attempt.after_state
```

Use cases:

- summarize child attempts for a parent
- inspect dirty state before finish
- attach artifact summaries
- apply task-specific commit warnings

### Model/Agent Hooks

These should be added only when model calls are mediated by Shell or Gateway:

```text
model.before_request
model.after_response
agent.before_turn
agent.after_turn
agent.after_tool_observation
```

Use cases:

- render task spec into prompt fragments
- compress observations
- add structured warnings when the context is stale or near budget

## Handler Results

Post-hook observations should be regular records:

```json
{
  "kind": "pytest_failure",
  "summary": "3 tests failed; first failure is test_parse_dates",
  "severity": "info",
  "evidence": {
    "stderr_tail": "..."
  },
  "next_checks": [
    ["pytest", "-q", "-vv", "tests/test_dates.py"]
  ],
  "suggested_actions": [
    "Inspect the first failing assertion before editing unrelated files."
  ]
}
```

Pre-hook decisions should be explicit:

```json
{
  "decision": "warn",
  "message": "input.txt is declared read-only by task_spec"
}
```

Allowed decisions:

```text
allow
warn
deny
rewrite
```

`deny` and `rewrite` are Shell-level decisions. Gateway may still deny an
operation after a hook allows it.

## Ordering And Limits

Hooks run in deterministic order:

```text
higher priority first
then narrower matcher
then registration time
then hook id
```

Runtime must enforce:

- max hooks per event
- max handler runtime
- max observation size
- recursion guard
- no hook invocation for hook-internal diagnostic operations unless explicitly
  requested
- trace every hook invocation and error

Hook failure should not crash the original operation by default. The result
should include a `hook_errors` list unless the hook was registered as
`required=true`.

## Safety

Hooks are not a security boundary.

Security comes from:

- microVM/container isolation
- Gateway capability APIs
- transactional workspace mutation
- commit-time review and approval
- trace/audit

Hooks improve control flow and feedback. They must not receive ambient host
authority, unscoped secrets, or direct canonical workspace write access.

Agent-written hooks should be serialized into attempt/session state so their
effects are auditable and rollback/commit semantics are clear.

## Relationship To TaskSpec

`task_spec` should customize the runtime by installing task-scoped hooks or
hook data.

Examples:

- declared inputs register `fs.before_write` warnings
- declared outputs register `fs.after_write` observations
- success criteria register `task.before_check` and `task.after_check`
- constraints register `attempt.before_finish` warnings

This makes `task_spec` more than LLM prompt text. It becomes a structured
contract that Shell can use to shape the attempt's tools and feedback.

## First Milestone

`Stone Hooks V1`

Scope:

1. Keep existing helper-file `hook(...)` compatibility.
2. Add programmatic `register_hook`, `unregister_hook`, and `hooks`.
3. Support attempt/session scoped hooks.
4. Add event envelopes for `run.before`, `run.after_*`, and `task.after_check`.
5. Add recursion and size limits.
6. Trace hook invocations in MCP and Gateway-backed attempts.
7. Add one agent-written hook smoke:

```text
Stone program registers run.after_failure for pytest
  -> runs failing pytest command
  -> hook attaches structured observation
  -> result contains helpers/observations
```

Then extend to task-spec driven hooks:

```text
task_spec declares input.txt read-only
  -> Shell registers fs.before_write warning
  -> attempt tries to write input.txt
  -> task_diff/task_check reports constraint violation
```
