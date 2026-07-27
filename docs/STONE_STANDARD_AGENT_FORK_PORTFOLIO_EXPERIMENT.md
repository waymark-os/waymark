# Standard Agent Fork Portfolio

## Purpose

This is the first complete visible control-flow example above the fork syscall.
It composes:

- one meaningful parent workspace-and-memory prefix;
- a negative lifecycle-parented spawn baseline;
- two isolated standard-controller children;
- typed `context_prompt_view` admission;
- parent-side result and trace inspection before cleanup;
- deterministic verification and winner selection;
- explicit accept, discard, and scope reclamation.

The reusable extension is
`examples/references/standard_attempt_fork_portfolio.stone`. The driver
combines it with the definitions in
`examples/scripts/standard_attempt_agent.stone` because Stone does not yet have
module imports.

## Control Flow

The parent writes uncommitted `problem.txt`, `shared-prefix.txt`, and a
verifier, then records `requirement.portfolio_target`.

First it runs an ordinary spawned child with the same lifecycle parent. The
spawn sees the canonical README but none of the dirty files or hot memory.
This establishes that lifecycle parentage is supervision, not state
inheritance.

The parent then forks `uppercase` and `reverse` workers from one revision-1
memory frontier. Each child:

1. materializes its candidate in an isolated workspace;
2. receives `requirement.portfolio_target` through the typed prompt view;
3. runs standard controller V8 with a replaceable read-only dispatcher and
   verifier;
4. reports a compact candidate and evidence.

The parent joins both children, calls `attempt_inspect` while each transaction
is still retained, selects the sole verified candidate, accepts its workspace,
discards the loser, inspects both again as reclaimed, and closes the scope.
Child memory is never merged into the parent.

## Findings

Both fixture and `gpt-5.6-terra` runs passed with no open transactions.

The fixture used one model call per child. Terra used three calls per child:
both children initially attempted `run_linux`, received the specialized
dispatcher's recoverable read-only rejection, switched to `read("answer.txt")`,
then finished. This is useful evidence that the standard loop composes with
task-specific policy and can recover without bypassing it.

Artifacts:

- `target/runs/stone-standard-fork-portfolio-v1/summary.json`
- `target/runs/stone-standard-fork-portfolio-v1-terra/summary.json`

Run:

```sh
python3 host/bench/eval_standard_agent_fork_portfolio.py --overwrite
python3 host/bench/eval_standard_agent_fork_portfolio.py \
  --provider codex-chatgpt \
  --run-dir target/runs/stone-standard-fork-portfolio-v1-terra \
  --overwrite
```

## TBv2.1 Follow-up

The generic two-role portfolio was subsequently exercised on the moderate
`portfolio-optimization` task. Fork, typed memory inheritance, inspection,
selection, accept/discard, bounded reporting, and cleanup all conformed, but
the controller returned `complete=false` and did not reach the verifier.

The shared phase made no workspace change and retained counters rather than a
semantic frontier, so both children repeated source inspection and exhausted
their short repair budgets. The next layer is a bounded frontier contract with
verified findings, unresolved alternatives, branch hypotheses, and aggregate
budget reservation—not more children. See
`../waymark-gateway/docs/STANDARD_STONE_TBV21_FORK_PORTFOLIO_EXPERIMENT.md`.
