# Standard Agent Completion-Critique Experiment

## Question

Can bounded attempt memory plus a fresh visible completion critique prevent a
premature finish and return a useful repair objective without hidden verifier
information?

## Treatment

Standard control V3 retains:

- one bounded public-task requirement item;
- up to eight fixed-slot tool-evidence items;
- one same-key semantic requirement audit;
- one same-key progress item.

At finish, a fresh schema-checked model transition receives only the public
requirement, recorded evidence, prior audit, and candidate. It enumerates at
most twelve requirements as `satisfied` or `unsupported`. The control accepts
approval only when every enumerated requirement is satisfied. A rejection is
returned to the action loop as a concrete repair objective. The total
model-call budget reserves room for a completion critique. V2 also performs
one proactive audit at the start of a configurable finalization window, even
when the action model has not proposed `finish`. The audit is injected into
the action trajectory while calls remain for repair, explicit finish, and the
final completion audit.

## Constructed Causal Cells

The task required creating `result.txt`, reading it back to verify exact bytes,
and then finishing.

| Cell | Model calls | Action decisions | Critiques | Rejections | Workspace reads | Result |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| treatment | 6 | 4 | 2 | 1 | 1 | passed |
| no-critique ablation | 2 | 2 | 0 | 0 | 0 | passed prematurely |
| contradictory approval | 6 | 4 | 2 | 1 | 1 | contradiction rejected, then passed |
| proactive finalization | 8 | 6 | 2 | 1 | 1 | checkpoint induced repair, then passed |
| no-checkpoint limit ablation | 7 | 7 | 0 | 0 | 0 | budget exhausted, incomplete |

The treatment and ablation shared the same first two responses: write the file,
then claim finish. In treatment, the first audit marked read-back verification
unsupported and requested a read. The action loop performed the read and the
second audit approved. With critique disabled, the same finish was accepted
without a read.

In the third cell, the first audit claimed `approved=true` while retaining an
`unsupported` requirement. V3 deterministically normalized that inconsistency
to rejection before returning the repair objective.

The last pair models the failure exposed by the untouched
`make-doom-for-mips` V1 run. Both cells execute four actions without proposing
finish. The V3 checkpoint consumes call 5, reports missing read-back, and
leaves calls 6–8 for read, finish, and approval. With the checkpoint disabled,
seven action calls consume the usable budget and the result remains
incomplete.

Every cell ran through the Gateway-backed LibOS path and rolled back. Retained
latest-state memory ranged from three to nine items. No transcript or
unbounded evidence list was copied into memory.

## Interpretation

This is causal evidence for the control mechanism: the completion phase changed
the visible action trajectory and the bounded ledger supplied the missing
evidence. It is not evidence that a real model will reliably decompose complex
requirements, judge evidence, or choose a useful repair. The next gate is a
small real-model constructed comparison before freezing another untouched
Terminal-Bench task.

That semantic-quality gate now passes for three Terra cases: missing read-back
was rejected, complete read-back was approved, and a failed RStan execution was
rejected with a task-relevant repair objective. See
[Standard Completion-Critic Quality Experiment](STONE_STANDARD_COMPLETION_CRITIC_QUALITY_EXPERIMENT.md).

The process-global fixture sequence added for this experiment is test-only. It
lets independent fresh model calls consume deterministic responses in actual
effect order; the existing conversation-indexed fixture behavior remains the
default.

## Artifacts

- Aggregate summary:
  `target/runs/stone-standard-completion-critique-v3/aggregate.json`
- Treatment:
  `target/runs/stone-standard-completion-critique-v3/treatment/summary.json`
- No-critique ablation:
  `target/runs/stone-standard-completion-critique-v3/no-critique-ablation/summary.json`
- Contradictory approval:
  `target/runs/stone-standard-completion-critique-v3/inconsistent-approval/summary.json`
- Proactive finalization:
  `target/runs/stone-standard-completion-critique-v3/proactive-finalization/summary.json`
- No-checkpoint limit ablation:
  `target/runs/stone-standard-completion-critique-v3/no-checkpoint-limit-ablation/summary.json`

Run with:

```sh
python3 host/bench/eval_standard_agent_completion_critique.py \
  --run-root target/runs/stone-standard-completion-critique-v3 \
  --overwrite
```
