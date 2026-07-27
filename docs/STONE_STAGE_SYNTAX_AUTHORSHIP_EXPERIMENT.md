# Stone Stage Syntax Authorship Experiment

Status: narrow hypothesis supported on 2026-07-27.

## Question

Does an author-facing `@stage` declaration with typed evidence reduce the work
required for an LLM to author an evidence-gated workflow without reducing
correctness?

The callback core remains the semantic kernel:

```stone
artifact = workflow_stage(
    "artifact",
    evidence=check_artifact,
    action=build_artifact,
    repair=repair_artifact,
    max_attempts=1,
)
```

The compared syntax arm lowers:

```stone
@stage(
    evidence=file_nonempty("artifact.txt"),
    repair=repair_artifact,
    max_attempts=1,
)
def artifact(step):
    return run(["sh", "-c", "exit 7"])
```

to the same typed stage representation. `file_nonempty` is a lazy evidence
specification; the runtime performs the workspace probe and generates the
path/size evidence reference.

## Frozen Comparison

Three paired `gpt-5.6-terra` low-reasoning trials received the same behavioral
task and live help for only their assigned surface. Every generated program was
executed unchanged in a fresh writable directory. The external gate required:

- the initial evidence check to be unmet;
- the primary action to exit 7 without creating the artifact;
- exactly one repair;
- a fresh satisfied evidence check after repair;
- three total evidence checks;
- a non-empty evidence reference;
- exact `artifact.txt` bytes equal to `ready`.

One diagnostic-guided repair was available but was not used.

## Result

| Metric | Callback core | `@stage` syntax |
|---|---:|---:|
| First-response passes | 3/3 | 3/3 |
| Eventual passes | 3/3 | 3/3 |
| Repair attempts | 0 | 0 |
| Mean function definitions | 3 | 2 |
| Mean source lines | 14 | 9 |
| Mean source bytes | 558 | 307 |

All six authorship calls made zero tool calls. The syntax arm reduced emitted
source by 251 bytes, or 45.0%, while preserving first-response and behavioral
success. Its successful outputs used 110 model output tokens each versus 181
for the callback arm.

This supports the narrow authoring-effort hypothesis, not a general reliability
claim: the task was small, all three outputs within each arm were identical,
and one model/configuration was tested. The next meaningful test is whether
the same lowering reduces errors in a multi-stage Caffe specialization with
structured repair branches and resource constraints.

## Reproduction

```sh
python3 host/bench/eval_stone_stage_syntax_ab.py \
  --model gpt-5.6-terra \
  --reasoning-effort low \
  --trials 3 \
  --max-repairs 1 \
  --run-root target/runs/stone-stage-syntax-ab-v1-terra \
  --overwrite
```

The aggregate is
`target/runs/stone-stage-syntax-ab-v1-terra/aggregate.json`. Prompts, generated
sources, model event streams, execution envelopes, and per-arm summaries are
retained beneath that run directory.
