# Stone Runtime-owned Run Await

## Question

Can Stone remove model-driven polling for long foreground commands without
introducing general coroutine objects or a second scheduler?

## Surface

```stone
async def build():
    return await run(["make", "all"], timeout_ms=600000)
```

`await run(...)` is an owned resource effect. It lowers to `run_complete`:

- one command and one action transition;
- repeated bounded Gateway observations inside the runtime;
- one terminal structured outcome;
- termination and a diagnostic outcome when the total timeout expires.

Ordinary `run(..., background=True)` remains the handle-producing spelling for
intentional overlap and long-lived services. Arbitrary awaitables and a local
coroutine scheduler remain unsupported.

The standard V12 action controller exposes the same mechanism as
`run_complete`. It keeps `run_start`, `run_status`, `run_wait`, and
`run_terminate` for cases that actually require interleaving, but its prompt
directs bounded foreground work to the runtime-owned path.

## Initial Evidence

Admission accepts `await run` and still rejects unrelated await targets. The
runtime canary reaches terminal output, reports `done=true` and
`still_running=false`, records one `run_await` transition, and exposes
`run_awaits=1`. The checked-in example is
`examples/scripts/async_run_await.stone`.

A Gateway-backed canary forced the initial observation budget to 100 ms around
a 500 ms command. `await run` reached terminal success with
`completion_waits >= 1`, proving that the runtime—not another model turn—made
the follow-up observation. Each internal observation was published live as a
bounded `linux.exec.wait` trace event keyed by the owned `run_id`. The existing
manual status/wait/terminate cases in the same canary still passed.

The standard V12 fixture then completed a create/read/audit task with one
`run_complete` action, no `run_start`, status, or wait action, zero active run
handles, and clean rollback. It used three action decisions plus one completion
critic call. Its retained audit and both evidence items were verified.

In a separate live learnability probe, `gpt-5.6-terra` at low reasoning chose
`run_complete` on its first response without being given the tool name. It then
read the exact output and finished: three model calls, one terminal run action,
no validation repair, no run polling, and zero active handles. A preliminary
prompt using an escaped `printf` newline produced `READYn`; replacing that
shell-quoting ambiguity with `echo READY` passed without changing the control
surface.

Artifacts:

- `host/rpc/smoke_waymark_runtime_gateway_timeout.py`
- `host/bench/eval_standard_agent_runtime_owned_run.py`
- `target/runs/stone-standard-runtime-owned-run-v1/aggregate.json`
- `../waymark-gateway/target/runs/stone-standard-runtime-owned-run-v12-terra-v2/summary.json`

## Frozen Terminal-Bench Comparison

A matched `mcmc-sampling-stan` pair used the same task, model, reasoning level,
32-round budget, runtime binaries, and publication policy. V11 made 31 model
calls, including 14 run observations, then exhausted its budget after
installing a missing dependency but before rerunning the failed analysis. The
official verifier did not run.

V12 made 14 model calls and zero model-driven run observations. It selected
`run_complete` for all 12 commands; only three needed a follow-up Gateway wait,
and those waits remained runtime-owned trace events. It repaired three failed
commands, completed the required sampling and evidence reads, safely published,
and earned official reward `1.0`. Total model tokens fell from 132,395 to
68,497, and agent wall time fell from 617.7 to 533.7 seconds. Both cells closed
with no nonterminal attempts, transactions, or checkpoints.

This one matched pair supports the interface hypothesis but is not a broad
causal estimate. The artifacts are:

- `../waymark-gateway/target/runs/standard-stone-tbv21-await-ab-mcmc-v11/cell/cell.json`
- `../waymark-gateway/target/runs/standard-stone-tbv21-await-ab-mcmc-v12/cell/cell.json`

## Boundary of the Improvement

The unchanged committed V12 source did not solve `make-doom-for-mips`. It made
zero model-driven polls and required zero internal follow-up waits, but spent
29 actions inspecting the environment and attempted its first build only on
the final action. That build reached two actionable linker errors with no
repair budget remaining. The prior V11 cell had used only two polling actions,
so waiting was never the dominant failure on this task.

Runtime-owned waiting should remain as infrastructure, but Doom requires a
higher control layer: bounded inspection, build, execute, and verify stages;
evidence gates at each boundary; and retained repair frontiers after expensive
successful stages. The result separates action-lifecycle efficiency from
task-progress policy.
