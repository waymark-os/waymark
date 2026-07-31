# Stone Async Attempt Control Experiment

Status: mechanism passed; async was learnable but showed no authorship advantage
over corrected synchronous scoped syntax on 2026-07-31.

## Question

Do familiar `async`/`await` markers make attempt branching, waiting, acceptance,
and owned cleanup easier for an authoring model?

## Narrow Surface

```stone
async def main(input):
    async with attempt_scope(join_timeout_ms=30000) as scope:
        async with semantic_frontier(checkpoint) as frontier:
            child = scope.branch(frontier, entrypoint="worker", start=True)
            outcome = await child.wait(timeout_ms=30000)
            await root.accept(child)
    return {"outcome": outcome, "clean": scope.closed}
```

This is an attempt-effect model, not general Python concurrency:

- `scope.branch(...)` returns an attempt handle immediately; the child already
  runs concurrently under Gateway supervision.
- `await` accepts handles, wait/join operations, wait sets, and acceptance.
- `async with` accepts attempt scopes and semantic frontiers.
- the current interpreter drives an async Stone function to completion and
  lowers awaits to blocking Gateway operations;
- `@stage`, async iteration/comprehensions, arbitrary awaitables, coroutine
  values, and a local task scheduler remain unsupported.

`with` and `async with` follow Python block visibility. Names remain available
after exit while the resource they name is closed or released.

## Evidence

The standalone suite covers parsing, admission, deterministic cleanup,
post-block visibility, and rejection of arbitrary await targets. A real Gateway
canary created a forkable checkpoint, awaited a child, reclaimed its resources,
reported `release_origin="scope_exit"`, and left no open lifecycle state.

The frozen `gpt-5.6-terra`, low-reasoning comparison used the same three trials
and task for every arm:

| Arm | First pass | Eventual | Repairs | Mean lines | Mean bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Raw | 2/3 | 3/3 | 1 | 61 | 2,100 |
| Typed functional | 3/3 | 3/3 | 0 | 42 | 1,794 |
| Scoped methods | 3/3 | 3/3 | 0 | 27 | 1,780 |
| Async methods | 3/3 | 3/3 | 0 | 27 | 1,815 |

The async arm used the intended three awaited joins plus awaited acceptance in
all passing programs. Its frozen non-inferiority hypothesis failed only because
mean source bytes were 1.1% above typed-functional and 2.0% above synchronous
scoped syntax; success and repairs were identical.

## Decision

Keep the narrow syntax as an experimental explicit-effect surface. It is
coherent with the attempt process model and models already know how to use it,
but it is not the default yet: corrected synchronous scoped syntax is equally
reliable and slightly smaller. Expand async only when Waymark needs an actual
overlap/select capability—such as racing waits with model or tool operations—
not merely to imitate a general-purpose language.

Artifacts:

- `examples/scripts/async_attempt_resources_canary.stone`
- `examples/references/semantic_frontier_authorship_async_reference.stone`
- `target/runs/semantic-frontier-authorship-async-pilot-v1/aggregate.json`
