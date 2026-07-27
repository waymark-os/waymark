# Fork-Required Attempt Canary

`examples/scripts/attempt_fork_needed_canary.stone` is the minimal task used to
debug fork semantics.

The parent first creates an uncommitted `problem.txt` and a verified
`requirement.fork_target` memory item. It then forks two candidate workers.
Both children must see that exact workspace-and-memory frontier, while their
conflicting `answer.txt` files and candidate memory remain isolated. The
parent accepts the sole passing branch, discards the loser, and verifies that:

- the accepted answer entered the parent workspace;
- the losing answer did not;
- child hot memory did not merge into parent memory;
- both child resource views were reclaimed.

This example actually needs fork. An ordinary spawn starts from a canonical
workspace source and does not inherit memory merely because it names a
lifecycle parent, so it cannot see the parent's in-progress frontier.

Run it with:

```sh
python3 host/bench/smoke_stone_attempt_fork_needed.py
```

The fixture uses `stressed`, candidates `deserts` and `desserts`, and accepts
only `desserts`. Its trace must contain two `attempt.fork` events, one
`attempt.accept`, and a rolled-back losing child with the explicit discard
reason.

The companion
[Fork First-Context Experiment](STONE_STANDARD_AGENT_FORK_FIRST_CONTEXT_EXPERIMENT.md)
uses the same in-progress-frontier property for a child's first model decision.
The
[Standard Agent Fork Portfolio](STONE_STANDARD_AGENT_FORK_PORTFOLIO_EXPERIMENT.md)
builds the full visible inspect-select-accept-discard control flow above these
semantics.
