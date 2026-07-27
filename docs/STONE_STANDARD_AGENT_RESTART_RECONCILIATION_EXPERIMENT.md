# Standard Agent Restart Reconciliation

V6 treats `progress.active_runs` as a bounded model-facing index and Gateway
operation state as authority.

At every controller start, before the first decision,
`standard_reconcile_active_runs` calls bounded `attempt_inspect`, replaces the
local handle list with the returned active run IDs, and rewrites the same-key
memory item. It does not replay an uncertain operation.

The canary exercises the narrow lost-handle window:

1. controller run 1 starts a background operation;
2. it exits before writing `progress.active_runs`;
3. Gateway still reports exactly one attempt-owned run;
4. controller run 2 reconstructs the handle, terminates and reaps it;
5. Gateway and memory both settle to zero active runs;
6. rollback leaves no open transaction.

V6 introduced the behavior and V7 preserves it. The current passing artifact
is `target/runs/stone-standard-restart-reconciliation-v8/summary.json`.

Run it with:

```sh
python3 host/bench/eval_standard_agent_restart_reconciliation.py --overwrite
```

This proves controller-process restart recovery while Gateway remains the live
resource authority. Durable Linux-operation recovery across a Gateway process
restart is a separate provider-lifecycle problem.
