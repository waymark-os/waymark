<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Default Attempt Agent

[`default_attempt_agent.stone`](../examples/scripts/default_attempt_agent.stone)
is the initial reference control flow for a short-term, attempt-oriented agent:

```text
project retained state -> model decision -> checked action -> retain outcome
            ^                                      |
            +--------------------------------------+
```

It is ordinary Stone source, not a privileged or hidden loop. Gateway remains
authoritative for capabilities, credentials, transactions, lifecycle, memory
bounds, and final result fencing.

## Defaults

- a six-turn bound and one model action per turn;
- JSON `run` or `finish` decisions;
- a 256-token memory projection scoped to each model call;
- same-key replacement for the latest decision and tool outcome;
- retention of failed as well as successful model/tool transitions;
- typed controller lifecycle from `agent_session().attempt` on restart;
- explicit finish verification and `attempt_report(...)`.

The program rebuilds its local message list after a controller restart. Durable
attempt context supplies the compact task state needed by subsequent model
calls; it does not replay an unbounded transcript.

## Extension seams

Copy the program and change the smallest relevant policy:

| Seam | Typical changes |
| --- | --- |
| `prepare_decision` | projection focus, token budget, prompt placement |
| `check_action` | tool allowlists, argument normalization, task constraints |
| `record_decision` / `record_outcome` | memory keys, synthesis, retention rules |
| `verify_finish` | tests, artifact checks, verifier evidence |
| `max_turns` and prompt | stopping budget, action grammar, model class |

## Stone action recovery

This loop does not expose `stone_eval` to its inner model. If a controller does,
recovery belongs around that one `stone_eval` action-state pair, not around the
whole attempt loop.

[`attempt_correction_policy.stone`](../examples/references/attempt_correction_policy.stone)
is the reference policy. It permits one explicit retry only for an admission
failure with one high-confidence source edit. Evaluation-time failures,
ambiguous suggestions, repeated source/candidate pairs, cycles, and exhausted
budgets return `replan`. The controller still evaluates the corrected source
itself; no builtin silently retries.

The policy retains one replace-in-place `recovery.stone` context item with at
most four entries. Entries contain source hashes, the selected replacement,
decision, and compact outcome—not source, raw errors, or transcript text.
Gateway trace remains the detailed record.

For controller crashes during an action, use the
[in-flight restart state machine](STONE_INFLIGHT_RESTART_SEMANTICS.md).
It distinguishes prepared, possibly-started, durably completed, and terminal
states; only the proven pre-effect state may resume.

The default projection hook inserts memory before the newest caller message so
the current observation or decision instruction remains last. An adaptation
that appends retrieved state after that message can let old evidence override
the action requested for the current transition.

Keep failed-outcome retention deliberate: a completed nonzero `run` has
`step.outcome.ok == False` but still carries its structured run record in
`step.outcome.value`. Do not guard all outcome recording on success.

An outer harness that owns final result reporting may remove the example's
`attempt_report(...)` call. It should retain an equivalent terminal report so a
completed attempt cannot be restarted accidentally.
