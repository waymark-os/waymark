<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Transition Hooks

## Status

Stone supports call-local transition hooks on `model_call(...)` and `run(...)`.
`transition_hooks(pre=..., post=...)` constructs a first-class, reusable hook
value. This is the attempt-agent control interface. A previously proposed
global `register_hook(...)` registry is deferred.

Stone helper files remain a separate diagnostic facility. They may attach
observations to selected `run(...)` results, but they do not define the agent
loop's action-state semantics.

## Model

An agent trajectory is a sequence of individual transitions:

```text
s_t
  -> pre(s_t, action_t)
  -> execute(action_t)
  -> post(s_t, action_t, outcome_t)
  -> s_t+1
```

Hooks belong to one dynamic effect invocation, not to a global event name:

```stone
decision_hooks = transition_hooks(
    pre=prepare_decision,
    post=record_decision,
)
response = model_call(
    messages,
    hooks=decision_hooks,
)

action_hooks = transition_hooks(
    pre=check_action,
    post=record_outcome,
)
result = run(
    ["pytest", "-q"],
    hooks=action_hooks,
)
```

Handlers may be named Stone functions, assigned callable values, or lambdas.
Each receives one transition record. Hook values may be bound, retained in a
warm session when their captures are persistable, and passed through ordinary
Stone functions. Literal `hooks={"pre": ..., "post": ...}` remains supported
for one-off call sites. Hook values are task-owned control objects, not JSON:
they cannot be nested in ordinary data records or cross the task authority
boundary.

## Transition Record

Pre-hook input:

```json
{
  "transition_id": "transition-7",
  "kind": "model_call",
  "phase": "pre",
  "input": {}
}
```

Post-hook input adds a structured outcome:

```json
{
  "transition_id": "transition-7",
  "kind": "model_call",
  "phase": "post",
  "input": {},
  "outcome": {
    "ok": true,
    "value": {}
  }
}
```

Failures use `outcome.ok=false`. A model transport failure supplies
`outcome.error`; a completed `run` supplies its structured record as
`outcome.value`, with `outcome.ok` matching `value.ok`. A transition ID is
attached to the returned model/run record. Standalone IDs are unique within a
Stone session. Gateway-attached IDs include the controller-run ordinal and are
scoped by the attempt, so a restarted process cannot reuse an earlier
controller's ID. Use `(attempt_id, transition_id)` for global identity.

For `model_call`, `input` is the effective request record. For `run`, it is:

```json
{
  "argv": ["pytest", "-q"],
  "arguments": [],
  "options": {"timeout_ms": 30000}
}
```

## Pre-Hook Results

A pre hook may return:

```text
None or True       continue unchanged
False              reject the transition
{"allow": false}  reject, optionally with reason
patch record       continue with the supported input patch
```

The initial patch surface is deliberately narrow:

```stone
# model_call: replace the visible messages
return {"messages": step.input.messages + memory_messages}

# run: replace argv
return {"argv": ["pytest", "-q", "tests/test_api.py"]}
```

Gateway capability and credential policy remains authoritative after a pre
hook permits or rewrites a request.

For `run`, rejection is a recoverable action outcome: the command is not
executed, `run` returns `ok=false`, `kind="policy_rejected"`, and a bounded
`policy_reason`, and the post hook observes that record. This lets an agent
revise an invalid argv on its next decision without losing the trajectory.
Malformed hooks and hook execution failures still fail the Stone call.
`model_call` rejection remains call-fatal because there is no model response
for the surrounding decision step to consume.

## Post-Hook Results

Post-hook return values do not replace the effect outcome. Post hooks normally
update attempt-local context:

```stone
def record_outcome(step):
    return context_write(
        "outcome.last_test",
        "outcome",
        {
            "transition_id": step.transition_id,
            "ok": step.outcome.ok,
            "result": step.outcome.value,
        },
    )
```

Post hooks run for successful and failed effects. A required post-hook failure
fails the Stone call even though the underlying effect may already have
occurred.

## Execution Rules

- Hook ordering is always `pre`, effect, `post`; a rejected `run` records the
  effect as skipped and still invokes `post`.
- Each phase runs at most once for a dynamic transition.
- Hooks may use ordinary deterministic Stone control and context operations.
- Hooks cannot recursively invoke `model_call`, `run`, or `must_run`.
- Hooks receive only the attempt's existing capabilities; they add no
  authority.
- Raw inputs and outputs remain owned by normal effect tracing. Stone emits a
  bounded transition diagnostic containing IDs, phases, hook presence, patch
  status, and success/failure.

## Attempt Memory Use

The first intended policy is short-term and attempt-local:

```text
run.post        -> context_write important outcome
model_call.pre  -> context_project relevant state
model effect    -> decides with the bounded projection visible
model_call.post -> context_write decision/outcome
```

The memory data and the hook program are separate. Hooks are reconstructed
from Stone program source; retained data lives in the context state. Attached
attempts now keep that data in Gateway-owned memory and restore it when a local
Stone controller restarts. Standalone evaluation retains only bounded local
state. Context fork semantics remain deferred and are not implied by this
interface.

An in-flight action needs a narrower restart policy than ordinary retained
facts. The reference [in-flight restart semantics](STONE_INFLIGHT_RESTART_SEMANTICS.md)
uses a pre-hook `started_or_unknown` marker and a compact post-hook receipt.
Only `prepared/not_started` may resume; an unknown start without a receipt must
replan, while a matching completed receipt can be consolidated without replay.

## Deferred

- global/session hook registration
- arbitrary filesystem/task/attempt event subscriptions
- priority and matcher systems
- nested model/tool effects
- cross-attempt memory promotion
- context checkpoint/fork policy
