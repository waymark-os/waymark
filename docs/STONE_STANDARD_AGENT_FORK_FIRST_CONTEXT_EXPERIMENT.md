# Standard Agent Fork First Context

## Contract

Fork and prompt construction are separate operations:

- `attempt_fork` snapshots the parent's workspace and hot-memory revision;
- the child controller decides what to materialize for a model call;
- the fork request admits up to four required memory keys in a typed
  `context_prompt_view`;
- `agent_session()` exposes that view independently from child task input;
- the standard V8 controller prefers the admitted view and retains
  `initial_action_memory_required_keys` only as a compatibility fallback;
- those keys are required only in the first action projection after each
  controller start;
- a missing, duplicate, or over-budget key fails closed through
  `context_project`.

The current Stone shape is:

```stone
child = attempt_fork(
    entrypoint="worker",
    input={"strategy": "candidate-a"},
    context_prompt_view={
        "required_keys": ["requirement.fork_target"],
    },
    start=True,
)
```

The Gateway validates and persists this child-admission policy. It does not
render prompt text or auto-inject memory; the controller still performs the
bounded, observable `context_project` call.

## Canary

The parent writes an uncommitted `problem.txt` and a verified
`requirement.fork_target`, forks the child, then writes `parent.after_fork`.
The child:

1. sees the uncommitted file;
2. requires `requirement.fork_target` in its first projection;
3. never sees `parent.after_fork`;
4. makes exactly one model call and returns the opaque target;
5. leaves parent memory unchanged and closes cleanly.

The V8 fixture passed with the policy absent from child input, present in
`agent_session()`, and sourced as `attempt_admission` by the controller. The
earlier V7 fixture and `gpt-5.6-terra` runs also passed; the Terra call used 638
input tokens and 25 output tokens, with no validation retry.
Current artifacts:

- `target/runs/stone-standard-fork-first-context-v8/summary.json`
- `target/runs/stone-standard-fork-first-context-v7/summary.json`
- `target/runs/stone-standard-fork-first-context-v7-terra/summary.json`

Run:

```sh
python3 host/bench/eval_standard_agent_fork_first_context.py --overwrite
python3 host/bench/eval_standard_agent_fork_first_context.py \
  --provider codex-chatgpt \
  --run-dir target/runs/stone-standard-fork-first-context-v8-terra \
  --overwrite
```
