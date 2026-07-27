<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# Stone Attempt-Memory Authorship Experiment

## Question

Can an outer LLM author, in one response and without tools or repair, the
restart-aware Stone loop that already passed when written by hand?

The evaluator gives the author compact Stone semantics, live help for model,
run, context, file, and emit operations, and an observable N/T/M contract. The
opaque action token is absent from the prompt and source. An admitted source is
executed unchanged in two controller processes per attempt and must pass the
same causal restart gate as the hand-authored program.

## Result

`gpt-5.6-terra` produced a structurally complete source without tool calls in
all three iterative trials, but no first-response source passed the complete
unedited gate:

| Trial | Interface available to author | First blocking behavior |
| --- | --- | --- |
| v1 | original live help | guessed a provider-style JSON Schema response format; model transport rejected it |
| v2 | portable JSON-object format and nonzero-run outcome clarified | used `transition.id`; the actual hook field is `transition_id` |
| v3 | hook record fields clarified | treated `emit(seed)` as process exit and fell through into the restore loop |

The v3 source correctly used `context_write/read/project`, attached pre/post
hooks to every post-restart model/tool pair, handled the nonzero tool outcome,
and selected the hidden action when projected memory was present. But the first
controller also executed the decision loop after emitting its seed result. It
therefore performed both phases in one process and then repeated restore after
restart; M reached revision 9 rather than the required 5. This invalidates the
causal restart result even though the individual memory operations worked.

Every trial rolled its attempts back and left no open transaction. Evaluator-
only assumptions discovered in v1—a hyphenated transcript filename and an
incorrect preflight cwd—were fixed before judging the saved, unchanged source.

## Interface Findings

The experiment found three concrete teaching defects, now reflected in live
help:

- portable JSON-object response format is
  `response_format={"type": "json_object"}`;
- hook records use `transition_id`, and a completed nonzero run remains in
  `outcome.value`;
- `emit` publishes a value but does not terminate execution.

The memory primitives themselves appear learnable. The brittle part is the
combined controller-lifecycle protocol: infer restart from a marker, keep seed
and restore effects mutually exclusive, reconstruct hooks, and satisfy a large
result contract in one ad-hoc script.

## Decision

Do not claim restart-aware authorship has passed, and do not move to memory fork
semantics on this evidence. The next interface experiment should reduce
lifecycle boilerplate, preferably by exposing an authoritative controller run
index/restart phase and testing a conventional `main() -> result` shape. Then
repeat the same unedited gate without weakening its memory or transition
assertions.

## Reproduce

```sh
python3 host/bench/eval_stone_attempt_memory_authorship.py \
  --run-root target/runs/stone-attempt-memory-authorship-v4
```

Prior trial artifacts are under
`target/runs/stone-attempt-memory-authorship-v1`, `-v2`, and `-v3`.
