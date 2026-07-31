# Stone Scoped Attempt Resources Experiment

Status: mechanism passed; the initial language semantics failed the frozen
authorship hypothesis, and the Python-compatible correction passed it on
2026-07-31.

## Question

Does familiar `with` syntax plus methods on nominal attempt resources make
ownership visible to an authoring model while preserving the existing Gateway
lifecycle kernel?

## Surface

```stone
scope = attempt_scope(join_timeout_ms=30000)
with scope:
    with semantic_frontier(checkpoint) as frontier:
        child = scope.branch(frontier, entrypoint="worker", start=True)
        outcome = child.wait(timeout_ms=30000)
        child.discard(reason="candidate rejected")
```

The methods are thin runtime lowerings to the existing checked operations.
`with` closes an attempt scope or releases a semantic frontier on fallthrough,
return, and error. Evaluation-exit cleanup remains the final backstop.

## Evidence

The standalone suite proves scope closure at the lexical boundary and before
an exception handler runs. The Gateway canary created a real forkable
checkpoint and child, reported `release_origin="scope_exit"`, reclaimed the
child, and left no open lifecycle state.

A raw/typed-functional/scoped reference smoke passed all three arms with the
same branch, repair-frontier restoration, evidence, and cleanup behavior.

The initial one-pair `gpt-5.6-terra`, low-reasoning pilot passed every arm on
the first response. Extending the same frozen prompts to three pairs reversed
that tentative result:

| Arm | First pass | Eventual | Repairs | Mean lines | Mean bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Raw | 2/3 | 3/3 | 1 | 64.7 | 2,100 |
| Typed functional | 3/3 | 3/3 | 0 | 36.7 | 1,781 |
| Scoped methods | 1/3 | 2/3 | 2 | 40.0 | 2,006 |

One scoped draft passed a callable rather than the required entrypoint string
and repaired successfully. The other placed `fixture_finalize` after its
resource blocks, where scoped frontier and handle values were no longer in
scope; its repair tried to copy nominal values into outer `None` bindings, but
Stone deliberately did not let task-owned capabilities escape that way. This
isolated a language-design bug: unlike Python, Stone's `with` created a nested
name scope and prevented nominal bindings from remaining visible after exit.
Resource cleanup did not require that visibility rule.

The follow-up made `with` Python-compatible: block bindings remain visible,
while files, scopes, and frontiers are deterministically closed or released.
The unchanged three-pair task was then rerun with an additional async arm:

| Arm | First pass | Eventual | Repairs | Mean lines | Mean bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Raw | 2/3 | 3/3 | 1 | 61 | 2,100 |
| Typed functional | 3/3 | 3/3 | 0 | 42 | 1,794 |
| Scoped methods | 3/3 | 3/3 | 0 | 27 | 1,780 |
| Async methods | 3/3 | 3/3 | 0 | 27 | 1,815 |

Corrected scoped syntax passed its frozen non-inferiority hypothesis. This is
evidence that the original failure came from surprising block visibility, not
from ownership blocks or nominal methods.

## Decision

Keep lexical cleanup and nominal methods, and use synchronous scoped syntax as
the default when ownership is naturally block-shaped. Preserve the functional
API as the explicit kernel surface and for generated control that needs
non-lexical lifetime management. The key invariant is that leaving the block
revokes resource authority without hiding the value or evidence needed for
post-scope finalization.

Artifacts:

- `examples/scripts/scoped_attempt_resources_canary.stone`
- `examples/references/semantic_frontier_authorship_scoped_reference.stone`
- `target/runs/scoped-attempt-resources-reference-smoke/`
- `target/runs/semantic-frontier-authorship-scoped-pilot-v1/aggregate.json`
- `target/runs/semantic-frontier-authorship-async-pilot-v1/aggregate.json`
