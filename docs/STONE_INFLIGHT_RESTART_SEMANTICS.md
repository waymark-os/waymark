<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# In-flight Action Restart Semantics

## Scope

This is the v1 short-term memory policy for one action interrupted by a Stone
controller restart. It is visible controller code built from transition hooks
and context operations, not a hidden retry mechanism.

The reference and executable experiment are
[`attempt_inflight_restart_experiment.stone`](../examples/scripts/attempt_inflight_restart_experiment.stone).

## State machine

| Retained state | Durable receipt | Restart decision |
| --- | --- | --- |
| `prepared` / `not_started` | none | execute once |
| `started_or_unknown` | none | do not replay; replan |
| `started_or_unknown` | matching `completed` receipt | record outcome without replay |
| `terminal` | archived | no-op |

The controller writes `prepared` before constructing the dynamic transition.
The action's pre hook replaces it with `started_or_unknown` before the external
effect. Its post hook writes a compact receipt containing transition identity,
input hash, status, exit code, and output hashes. It does not retain raw output.

Terminal consolidation replaces `action.inflight` and archives the temporary
receipt, leaving one hot item. Re-running a terminal controller performs no
write and no effect, so the memory revision remains stable.

This policy does not promise that an arbitrary external effect is exactly-once.
It promises that Waymark will only replay when its retained state proves the
effect transition did not start. Ambiguous starts are delegated to the model or
task-specific reconciliation.

## Transition identity

Standalone Stone sessions retain `transition-N` IDs. Gateway-attached
controllers use compact IDs scoped by the surrounding attempt:

```text
run-<controller-run>-transition-<sequence>
```

The controller-run component prevents a restarted process from reusing the
prior process's transition IDs. The sequence remains local and bounded. A
globally unique reference is `(attempt_id, transition_id)`; the attempt ID is
not duplicated into every hot memory item.

## Experiment

Run:

```sh
python3 host/bench/eval_stone_inflight_restart.py \
  --run-dir target/runs/stone-inflight-restart-v1
```

The Gateway-backed gate starts every attempt three times:

- **prepared:** the second controller executes once and consolidates success;
- **started:** the second controller replans and does not duplicate the effect;
- **completed:** the second controller consolidates the durable receipt without
  replay;
- the third controller is a terminal no-op in every cell.

All three cells retain exactly one external effect and one terminal hot-memory
item. Expected final memory revisions are five, three, and five respectively;
the third run leaves each revision unchanged. The v1 result is
`target/runs/stone-inflight-restart-v1/summary.json`.
