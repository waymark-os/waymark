# Standard Completion-Critic Quality Experiment

## Question

Given only a public task, a candidate finish claim, and bounded visible
evidence, can a real model distinguish supported completion from missing or
contradictory evidence?

This isolates the semantic critic. The deterministic completion-critique
experiment already establishes that rejection changes the action trajectory.

## Frozen Cases

`gpt-5.6-terra` at medium reasoning evaluated three constructed cases through
the Gateway-backed LibOS path. Each case used the same visible V3 critic,
schema, bounds, and composed source.

| Case | Expected | Result | Calls | Tokens | Retained items |
| --- | --- | --- | ---: | ---: | ---: |
| missing read-back | reject | rejected | 1 | 889 | 3 |
| complete read-back | approve | approved | 1 | 877 | 4 |
| failed RStan execution | reject | rejected | 1 | 1,153 | 4 |

No case needed a schema-repair retry.

For missing read-back, Terra observed that a successful write record did not
prove the exact requested bytes and that no read evidence existed. Its repair
objective required writing `READY\n`, reading the file, and recording the exact
returned bytes.

For complete evidence, it cited both the successful write and the later read
containing exactly `READY\n`, marked both public requirements satisfied, and
approved.

For failed execution, the visible run result had exit code 1 and reported that
RStan was unavailable. Terra rejected the completion claim and requested that
RStan be made available, the R script be rerun, and zero-exit execution plus
posterior output be recorded. This is the visible judgment missing from the
earlier MCMC trajectory.

Every attempt rolled back with zero open transactions or checkpoints.

## Integration Finding

The first V1 failed-execution probe exposed a generic status mismatch before
any model call: failed tool evidence used status `rejected`, while the
memory protocol names that state `contradicted`. The controller now uses the
protocol status and has a source regression check. All three scored cells were
then rerun under one corrected source hash.

The first V2 rerun exposed a critic-calibration edge: one sample rejected a
successful write followed by an exact read because the evidence did not prove
that the path was previously absent. The public requirement did not require
prior absence. V2 now states the intended evidence rule explicitly: a
successful write followed by an exact-content read supports ordinary file
creation/content, while a write alone still cannot prove exact content,
execution, or dependency loading. The final three cells were rerun under
source hash
`3062eae1e284fa613a83c199a6803713bbda8be0270eb165ad998e6e127a9372`.

## Interpretation

The semantic-quality gate passes for these three cases. Together with the
deterministic causal cells, the evidence now covers:

- bounded requirement/evidence retention;
- finish rejection and repair re-entry;
- deterministic contradiction handling;
- real-model approval of supported evidence;
- real-model rejection of missing and failed-execution evidence.

This remains a single model/sample on constructed tasks. It does not establish
calibration across long or ambiguous requirements.

## Artifacts

- Aggregate:
  `target/runs/stone-standard-critic-quality-terra-v3/aggregate.json`
- Admitted composed source:
  `target/runs/stone-standard-critic-quality-terra-v3/completion-critic-canary.stone`
- Missing read-back:
  `target/runs/stone-standard-critic-quality-terra-v3/missing-readback/summary.json`
- Complete read-back:
  `target/runs/stone-standard-critic-quality-terra-v3/complete-readback/summary.json`
- Failed execution:
  `target/runs/stone-standard-critic-quality-terra-v3/failed-execution/summary.json`

Run with:

```sh
python3 host/bench/eval_standard_completion_critic_quality.py \
  --run-root target/runs/stone-standard-critic-quality-terra-v3 \
  --overwrite
```
