// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{Record, Span, Value};

struct StoneHelpEntry {
    name: &'static str,
    signature: &'static str,
    use_when: &'static str,
    examples: &'static [&'static str],
    avoid: &'static [&'static str],
    aliases: &'static [&'static str],
}

struct StoneHelpTopic {
    name: &'static str,
    summary: &'static str,
    bullets: &'static [&'static str],
}

const STONE_HELP_ENTRIES: &[StoneHelpEntry] = &[
    StoneHelpEntry {
        name: "help",
        signature: r#"help(name: str? = None) -> record"#,
        use_when: "Use to inspect Stone syntax, constraints, and examples before writing scripts.",
        examples: &[r#"emit(help())"#, r#"emit(help("save"))"#],
        avoid: &["Do not assume Python stdlib or Nu pipe syntax; ask help for the Stone function."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "transition_hooks",
        signature: r#"transition_hooks(pre: callable? = None, post: callable? = None) -> transition_hooks"#,
        use_when: "Create a reusable first-class hook value that can be bound to a local, passed through ordinary Stone functions, and supplied as hooks= to model_call, model_infer, run, or run_complete.",
        examples: &[
            r#"def observe(step):
    return context_write("outcome.last", "outcome", {"ok": step.outcome.ok})
hooks = transition_hooks(post=observe)
result = run(["printf", "ok"], hooks=hooks)"#,
        ],
        avoid: &[
            "Do not place transition_hooks values inside ordinary JSON/data records; pass them as first-class function arguments.",
            "Handlers still cannot recursively invoke model or run effects.",
            "A warm session retains the hook value only when every captured value is session-persistable.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "workflow_evidence",
        signature: r#"workflow_evidence(satisfied_or_result: bool | record{ok: bool}, summary: str, evidence: list[str] = []) -> record"#,
        use_when: "Return bounded, typed completion evidence from a workflow stage evidence handler. Pass a run/tool result record to retain its bounded failure diagnostic automatically.",
        examples: &[
            r#"def check_artifact(step):
    return workflow_evidence(True, "artifact is non-empty", ["stat:artifact.txt"])
evidence = check_artifact({})"#,
            r#"def check_probe(step):
    probe = {"ok": False, "kind": "not_found"}
    return workflow_evidence(probe, "artifact validation")
evidence = check_probe({})"#,
        ],
        avoid: &[
            "Do not mark evidence satisfied without at least one compact evidence reference.",
            "Do not place logs or artifact contents in evidence; use a bounded reference.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "file_nonempty",
        signature: r#"file_nonempty(path: str) -> workflow_evidence_spec"#,
        use_when: "Declare an authoritative lazy stage-evidence probe that is satisfied only by a non-empty regular file in the current transactional workspace.",
        examples: &[
            r#"def repair_artifact(step):
    write_text("artifact.txt", "ready")
    return {"ok": True}

@stage(evidence=file_nonempty("artifact.txt"), repair=repair_artifact, max_attempts=2)
def build_artifact(step):
    return {"ok": False}

report = workflow_run(workflow("build", build_artifact))"#,
        ],
        avoid: &[
            "Do not call file_nonempty as a boolean; it returns a typed lazy specification evaluated by workflow_run.",
            "Do not manufacture the evidence reference yourself; the runtime records the observed path and size.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "all_evidence",
        signature: r#"all_evidence(contract: workflow_evidence_spec, ...contracts) -> workflow_evidence_spec"#,
        use_when: "Require every lazy stage contract and retain a bounded summary naming each contract that is still missing.",
        examples: &[r#"proof = all_evidence(file_nonempty("binary"), file_nonempty("report.txt"))"#],
        avoid: &[
            "Do not flatten contracts into one boolean; separate typed contracts produce better repair feedback.",
            "Use one to 16 contracts.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "artifact",
        signature: r#"artifact(path: str, format: "elf"? = None, arch: "mips" | "x86_64" | "aarch64" | "riscv64"? = None) -> workflow_evidence_spec"#,
        use_when: "Require a non-empty workspace artifact and optionally validate its executable format and architecture from the file header.",
        examples: &[r#"ensure artifact("doomgeneric_mips", format="elf", arch="mips")"#],
        avoid: &[
            "Use a path relative to the task workspace unless the public task requires an absolute workspace path.",
            "Do not infer architecture from the filename; the runtime checks the ELF machine field.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "command_succeeded",
        signature: r#"command_succeeded(argv: list[str]) -> workflow_evidence_spec"#,
        use_when: "Require the latest stage action to be the exact command and to have completed successfully.",
        examples: &[r#"ensure command_succeeded(["node", "vm.js"])"#],
        avoid: &[
            "This is action-scoped evidence and is unsatisfied before a stage action runs.",
            "Keep stdout_nonempty in a separate ensure when observable output is also required.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "stdout_nonempty",
        signature: r#"stdout_nonempty() -> workflow_evidence_spec"#,
        use_when: "Require the latest stage action to have produced at least one captured stdout byte.",
        examples: &[r#"ensure stdout_nonempty()"#],
        avoid: &[
            "This contract checks the same latest stage action as adjacent outcome contracts; a prior stage's stdout does not satisfy it.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "decision_recorded",
        signature: r#"decision_recorded(fields: list[str]? = [], resolved: list[str]? = []) -> workflow_evidence_spec"#,
        use_when: "Gate an inspection, planning, or selection stage on an explicit non-empty stage decision. fields= requires named non-empty strings. resolved= instead requires state/value/basis records and advances only when every state is resolved. The two keywords are mutually exclusive.",
        examples: &[
            r#"ensure decision_recorded()"#,
            r#"ensure decision_recorded(fields=["source_layout", "toolchain"])"#,
            r#"ensure decision_recorded(resolved=["source_layout", "toolchain"])"#,
        ],
        avoid: &[
            "Do not use the existence of task inputs as proof that inspection or planning produced a decision.",
            "Use fields= when downstream execution depends on concrete findings; a prose-only decision cannot satisfy those fields.",
            "This is action-scoped evidence and remains unsatisfied until the stage agent explicitly records a non-empty decision and every required finding.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "file_valid",
        signature: r#"file_valid(path: str, format: "bmp"? = None, nonempty: bool = True) -> workflow_evidence_spec"#,
        use_when: "Require a regular file with optional non-empty and format checks. /tmp paths are checked in the retained Linux tool environment in Gateway mode.",
        examples: &[r#"ensure file_valid("/tmp/frame.bmp", format="bmp", nonempty=True)"#],
        avoid: &[
            "Use artifact(...) for workspace build products whose executable format or architecture matters.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "stage",
        signature: r#"@stage(evidence: workflow_evidence_spec | callable, name: str? = None, goal: str? = None, repair: callable? = None, max_attempts: int = 1, max_actions: int = 1, checkpoint: str = "none")
def name(step): ... -> workflow_stage"#,
        use_when: "Declare a named evidence-gated workflow stage with its action body directly below the decorator. Use checkpoint=\"workspace\" for workspace plus memory, checkpoint=\"forkable\" for an attempt-scoped reconstructable tool environment, or checkpoint=\"repairable\" when a verified frontier must survive a late harness failure.",
        examples: &[
            r#"def repair_artifact(step):
    return run(["sh", "-c", "printf ready > artifact.txt"])

@stage(evidence=file_nonempty("artifact.txt"), repair=repair_artifact, max_attempts=1)
def artifact(step):
    return run(["sh", "-c", "exit 7"])

report = workflow_run(workflow("build", artifact))
emit(report)"#,
        ],
        avoid: &[
            "Use @stage(...) immediately above a one-argument def; stage(...) is not an ordinary runtime constructor.",
            "The decorated action and optional repair must return records with boolean ok fields.",
            "Stage advancement still depends only on evidence, never the action ok field.",
            "A checkpoint is created only after fresh satisfied evidence; an already-satisfied stage is not checkpointed again.",
            "Forkable checkpoints fail closed when the attempt uses an attached or mutable provider root; preserve setup in an immutable image or wait for a mutable snapshot provider.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "workflow_stage",
        signature: r#"workflow_stage(name: str, evidence: callable, action: callable, repair: callable? = None, max_attempts: int = 1, checkpoint: str = "none") -> workflow_stage"#,
        use_when: "Define one typed stage whose action may run only within a bounded evidence-check and optional repair cycle.",
        examples: &[
            r#"stage = workflow_stage(
    "artifact",
    evidence=lambda step: workflow_evidence(True, "ready", ["artifact"]),
    action=lambda step: {"ok": True},
)"#,
        ],
        avoid: &[
            "Do not infer completion from action.ok; the evidence handler alone gates advancement.",
            "Every handler must accept one structured workflow context record.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "workflow",
        signature: r#"workflow(name: str, stage: workflow_stage, ...) -> workflow"#,
        use_when: "Compose one or more uniquely named workflow stages into a first-class deterministic control value.",
        examples: &[r#"build_stage = workflow_stage(
    "build",
    evidence=lambda step: workflow_evidence(True, "built", ["build"]),
    action=lambda step: {"ok": True},
)
verify_stage = workflow_stage(
    "verify",
    evidence=lambda step: workflow_evidence(True, "verified", ["verify"]),
    action=lambda step: {"ok": True},
)
plan = workflow("build-and-check", build_stage, verify_stage)"#],
        avoid: &[
            "Do not place workflows inside ordinary JSON/data records; pass them as first-class function arguments.",
            "This initial primitive is sequential; parallel and cross-attempt stages are not implied.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "workflow_main",
        signature: r#"workflow_main(plan: workflow) -> workflow_report"#,
        use_when: "Execute the top-level workflow selected by `run name`. In an attached attempt, retain a typed requirement audit and report succeeded or failed from the evidence-gated workflow result; local evaluation simply returns the report.",
        examples: &[r#"emit(workflow_main(plan))"#],
        avoid: &[
            "Use workflow_run(plan) for nested/library evaluation that must not finalize the current attempt's controller result.",
            "A succeeded report does not bypass Gateway publication or official verifier authority.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "workflow_patch",
        signature: r#"workflow_patch(plan: workflow, target: str, replacement: workflow_stage) -> workflow
workflow_patch(plan: workflow, patch: record, allowed_replacement: workflow_stage, ...) -> workflow"#,
        use_when: "Produce a new workflow by replacing exactly one named stage while retaining stage order and recording patch provenance in the workflow report. The data-driven form resolves an exact {target, replacement} record only against explicitly passed candidate stages.",
        examples: &[r#"compile_broken = workflow_stage(
    "compile_broken",
    evidence=lambda step: workflow_evidence(False, "not ready"),
    action=lambda step: {"ok": False},
)
compile_fixed = workflow_stage(
    "compile_fixed",
    evidence=lambda step: workflow_evidence(True, "ready", ["fixed"]),
    action=lambda step: {"ok": True},
)
plan = workflow("compile", compile_broken)
selected = workflow_patch(
    plan,
    {"target": "compile_broken", "replacement": "compile_fixed"},
    compile_fixed,
)"#],
        avoid: &[
            "Do not use a missing target or a replacement whose name duplicates another remaining stage.",
            "Do not dynamically evaluate model-authored source when a bounded patch record and explicit candidate stages can express the repair.",
            "The input workflow is immutable; retain the returned workflow and run that value.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "workflow_run",
        signature: r#"workflow_run(plan: workflow) -> workflow_report"#,
        use_when: "Run stages sequentially with pre/post evidence checks, bounded action attempts, and optional repair checks. Each stage report retains compact status plus bounded stdout/stderr tails and an explanation summary from its latest action or repair.",
        examples: &[r#"stage = workflow_stage(
    "verify",
    evidence=lambda step: workflow_evidence(True, "verified", ["verify"]),
    action=lambda step: {"ok": True},
)
plan = workflow("check", stage)
report = workflow_run(plan)
emit({"ok": report.ok, "failed_stage": report.failed_stage, "stages": report.stages})"#],
        avoid: &[
            "Do not recursively invoke workflow_run from a workflow handler.",
            "A normal unmet-evidence outcome returns a compact failed report; callback contract errors still fail the Stone program.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "model_call",
        signature: r#"model_call(messages: list[record], model_class: str = "agent", model: str = "", temperature: float? = None, top_p: float? = None, seed: int? = None, max_output_tokens: int = 0, response_format: record | str | None = None, metadata: record[str, str] = {}, hooks: transition_hooks | {pre: callable?, post: callable?} = {}) -> record"#,
        use_when: "Gateway mode only. Use as the explicit low-level model effect when a Stone program owns the prompt, conversation, retry, stopping policy, and optional per-call transition hooks.",
        examples: &[
            r#"emit(model_call([{"role": "system", "content": "Answer concisely."}, {"role": "user", "content": "Return the word ready."}], model_class="agent", max_output_tokens=64).content)"#,
            r#"response = model_call([{"role": "user", "content": "Return one JSON object with a ready field."}], response_format={"type": "json_object"}, max_output_tokens=64)
emit(json_loads(response.content))"#,
        ],
        avoid: &[
            "Do not pass provider credentials, endpoints, or secret environment-variable names; Gateway owns them.",
            "Do not assume an automatic retry, memory, tool dispatcher, or stopping rule; model_call performs exactly one model effect.",
            "Every message record requires a role and string content; serialize structured observations with json_dumps before placing them in content.",
            "For portable JSON-object output, use response_format={\"type\": \"json_object\"}; do not guess a provider-specific JSON Schema shape.",
            "With the current Gateway protobuf, explicit top_p and seed values must be greater than zero; omit them for provider defaults.",
            "Keep messages and options structured; do not encode the request as shell text.",
            "A hook transition record uses transition_id (not id), kind, phase, and input; post hooks also receive outcome.",
            "A pre hook may return None, bool, or a record patch containing messages; a post hook observes outcome.ok plus outcome.value or outcome.error.",
            "Transition hooks may read or write context, but cannot recursively call model_call, run, run_complete, or must_run.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "model_infer",
        signature: r#"model_infer(messages: list[record], schema: record, retries: int = 0, repair_prompt: str = "", schema_prompt: str = "", model_class: str = "agent", model: str = "", temperature: float? = None, top_p: float? = None, seed: int? = None, max_output_tokens: int = 0, metadata: record[str, str] = {}, hooks: transition_hooks | {pre: callable?, post: callable?} = {}) -> {value: Any, response: record, validation_attempts: int, errors: list, usage: record}"#,
        use_when: "Gateway mode only. Use for one schema-validated JSON decision with an optional explicit bounded repair policy; each attempt remains a separately traced model_call transition.",
        examples: &[
            r#"schema = {"type": "object", "properties": {"ready": {"type": "boolean"}}, "required": ["ready"], "additionalProperties": False}
inference = model_infer([{"role": "user", "content": "Report readiness."}], schema, retries=1, max_output_tokens=64)
emit(inference.value.ready)"#,
        ],
        avoid: &[
            "Do not pass response_format; model_infer owns portable JSON-object mode and independently validates the result.",
            "Supported schema keywords are type, properties, required, additionalProperties (boolean), items, minItems, maxItems, minLength, maxLength, minimum, maximum, enum, const, oneOf, and anyOf; unsupported keywords fail before a model effect.",
            "Retries are explicit, separately traced model calls and are capped at four; validation failure does not silently restart an agent loop.",
            "Schema validation proves shape, not factual correctness, tool authority, or task completion.",
            "By default the complete schema is sent to the model. schema_prompt may replace only that model-facing instruction with a bounded compact equivalent; runtime validation still uses the complete schema.",
            "The schema instruction and repair messages are ordinary model-call inputs visible to hooks and tracing.",
            "Transition hooks apply to each underlying model_call pair and cannot recursively invoke model_infer, model_call, run, run_complete, or must_run.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "context_write",
        signature: r#"context_write(key: str, kind: str, content: Any, status: str = "active", evidence: list = []) -> record"#,
        use_when: "Experimental Stone context surface. Add or revise stable task state under one key for later reads and bounded prompt projection.",
        examples: &[r#"item = context_write("requirement.output", "requirement", {"text": "preserve the binary"}, status="verified", evidence=["trace:probe-1"])
emit({"key": item.key, "revision": item.revision})"#],
        avoid: &[
            "Do not copy large logs or files into context; store a concise claim and an evidence reference.",
            "Attached attempts use durable Gateway state; standalone Stone uses a bounded session-local fallback. Neither defines a fork contract.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "context_read",
        signature: r#"context_read(query: str = "", keys: list[str] = [], kinds: list[str] = [], limit: int = 20) -> list[record]"#,
        use_when: "Experimental Stone context surface. Explicitly inspect current retained items by key, kind, or text query before a decision.",
        examples: &[r#"context_write("risk.cleanup", "risk", "do not delete the generated library")
emit(context_read(query="generated library", kinds=["risk"], limit=5))"#],
        avoid: &[
            "Do not use an empty unbounded read as a substitute for selecting relevant context.",
            "Use context_project when constructing a token-bounded model prompt.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "context_project",
        signature: r#"context_project(focus: str = "", max_tokens: int = 512, required_keys: list[str] = []) -> record"#,
        use_when: "Experimental Stone context surface. Reserve explicitly required stable keys, then select a deterministic token-bounded relevance projection for a model-call prompt.",
        examples: &[r#"context_write("goal.finish", "goal", "run the public checks before finish")
projection = context_project(focus="finish verification", max_tokens=128, required_keys=["goal.finish"])
emit({"text": projection.text, "items": len(projection.items)})"#],
        avoid: &[
            "Do not assume projection verifies a memory claim; workspace and verifier evidence remain authoritative.",
            "Required keys fail closed when missing or when their encoded items do not fit the projection budget.",
            "Do not replay a full transcript alongside the projection and defeat the context budget.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "correction_apply",
        signature: "correction_apply(source: str, correction: record, candidate: int = 0) -> record",
        use_when: "Use after a failed Stone evaluation to validate and apply one advertised local edit to the exact failed source without executing it.",
        examples: &[r#"source = "context_projet()"
correction = {
    "version": 1,
    "mode": "suggest",
    "safety": "suggest_only",
    "auto_apply": False,
    "retry": "explicit_only",
    "source_sha256": sha256(source),
    "received": "context_projet",
    "candidates": [{
        "replacement": "context_project",
        "edit": {"start": 0, "end": 14, "replacement": "context_project"},
    }],
}
emit(correction_apply(source, correction).source)"#],
        avoid: &[
            "Do not pass requires_repair guidance; only suggest_only candidates with an unambiguous edit can be applied.",
            "The returned source has executed=false. Review it and evaluate it separately in the appropriate transaction.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "task_spec",
        signature: "task_spec() -> record",
        use_when: "Gateway mode only. Read the admitted task objective, named inputs and outputs, success criteria, constraints, and metadata as structured data for a reusable Stone program.",
        examples: &[r#"spec = task_spec()
emit({"id": spec.id, "objective": spec.objective, "outputs": spec.outputs})"#],
        avoid: &[
            "Do not reconstruct or scrape a hidden task prompt; use the structured fields directly.",
            "Do not mutate the returned record and assume Gateway task authority changed; this is a read-only runtime view.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "task_input",
        signature: "task_input() -> Any",
        use_when: "Gateway mode only. Read the attempt's admitted dynamic JSON input as an ordinary Stone value; returns None when no input was supplied.",
        examples: &[r#"input = task_input()
emit({"input": input})"#],
        avoid: &[
            "Do not use environment variables or provider metadata as an implicit task-input channel.",
            "Treat task_input() as read-only input; write task artifacts through declared workspace paths.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "agent_session",
        signature: "agent_session() -> record",
        use_when: "Gateway mode only. Build the standard structured session argument for a Stone AgentControl function from the attached task, input, attempt, limits, fresh time_budget, and available Shell tool families. session.time_budget includes elapsed_ms and remaining_ms when the attempt declares a wall-time limit or deadline. Calling agent_session() again refreshes that snapshot. session.context_prompt_view carries typed child-admission projection policy independently from task input; session.attempt includes typed controller_run_count, controller_restarted, and controller_phase fields.",
        examples: &[r#"def control(session):
    return {"attempt": session.attempt.attempt, "objective": session.task.objective}
emit(control(agent_session()))"#],
        avoid: &[
            "Do not treat the tool-name lists as authority; Gateway capabilities decide which protected calls are allowed.",
            "Do not require agent_session() for a small direct Stone program; it is the reusable AgentControl convention, not new control syntax.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "current_program",
        signature: r#"current_program(entrypoint: str = current_entrypoint) -> record"#,
        use_when: "Use inside a Stone module to obtain the current source as a structured Stone program for child attempt spawning; optionally select a named entrypoint without embedding or escaping source text.",
        examples: &[r#"worker_program = current_program(entrypoint="worker")"#],
        avoid: &[
            "Do not emit current_program() merely to inspect it; the record contains the complete current source.",
            "Named-entrypoint modules currently permit only top-level def and pass statements; put executable work inside entrypoint functions.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "react_control",
        signature: r#"react_control(model: str = "", max_rounds: int = 16, max_turns: int = 16, max_tool_ms: int? = None, completion_path: str = "") -> agent_control"#,
        use_when: "Gateway mode only. Create the optimized builtin JSON-action ReAct control as a first-class callable Stone value, then invoke it with an agent_session() record.",
        examples: &[r#"control = react_control(max_rounds=4, max_turns=8)
emit(control(agent_session()))"#],
        avoid: &[
            "Do not treat this builtin as privileged policy; it uses the attached attempt's ordinary model and Shell capabilities.",
            "Do not use react_control when a small direct Stone loop expresses the task-specific policy more clearly.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "scripted_control",
        signature: r#"scripted_control(actions: list[record], max_turns: int = 16, max_tool_ms: int? = None, completion_path: str = "") -> agent_control"#,
        use_when: "Use for deterministic fixtures or pre-authored action sequences through the same first-class AgentControl callable contract as optimized model controls.",
        examples: &[r#"control = scripted_control([{"final": {"answer": "ok"}}])
session = {"task": {"objective": "fixture"}, "input": None}
emit(control(session).value)"#],
        avoid: &[
            "Do not use scripted_control as a hidden workflow format; ordinary Stone is the expressive control language.",
            "Do not embed unbounded generated action lists; use a bounded Stone loop or react_control for dynamic decisions.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "emit",
        signature: "emit(value: Any? = pipeline_value) -> Any",
        use_when: "Use to publish a structured result from a Stone script or MCP call. emit returns the value to the running program and does not terminate execution.",
        examples: &[r#"emit({"ok": True, "path": "/app/out.json"})"#],
        avoid: &[
            "Do not print final structured results; emit them.",
            "Do not treat emit as exit; use mutually exclusive control flow or return from a function when later effects must not run.",
            "Do not emit large lists just to inspect them; bind the list and emit len/head/tail summaries.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "fail",
        signature: "fail(message: str, code: str? = None, detail: Any? = None) -> never",
        use_when: "Use to intentionally mark a task as failed with a clear message.",
        examples: &[r#"fail("missing required input", code="missing_input")"#],
        avoid: &["Do not use fail for ordinary recoverable probes; return/emit diagnostics instead."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "echo",
        signature: "echo(value: Any, ...values: Any) -> Any | list",
        use_when: "Use for quick literal values in small probes.",
        examples: &[r#"emit(echo("hello"))"#, r#"emit(echo("name", 3))"#],
        avoid: &["Prefer emit(value) for final task results."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "int",
        signature: "int(value: Any) -> int",
        use_when: "Use to explicitly convert strings or floats before integer arithmetic.",
        examples: &[r#"qty = int(row["qty"])"#],
        avoid: &["Do not rely on automatic string-number coercion."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "float",
        signature: "float(value: Any) -> float",
        use_when: "Use to explicitly convert strings or integers before floating-point arithmetic.",
        examples: &[r#"amount = float(row["amount"])"#],
        avoid: &["Use parse_float(value, default) when malformed input should not fail the script."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "str",
        signature: "str(value: Any) -> str",
        use_when: "Use to explicitly convert values before string concatenation or formatted text output.",
        examples: &[r#"line = row["name"] + "," + str(count)"#],
        avoid: &["Do not concatenate strings and numbers without str()."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "repr",
        signature: "repr(value: Any) -> str",
        use_when: "Use as a Python-compatibility alias for str(value) when generated code wants a printable representation.",
        examples: &[r#"debug = repr(["ok", 2])"#],
        avoid: &["Use json_dumps(value) when the output must be valid JSON."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "len",
        signature: "len(value: str | list | record) -> int",
        use_when: "Use for counts and compact summaries of large values.",
        examples: &[r#"emit({"rows": len(rows), "sample": head(rows, 5)})"#],
        avoid: &["Do not emit a full large list just to learn its length."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "list",
        signature: "list(value: list | record) -> list",
        use_when: "Use to materialize a list view of an existing list or record keys.",
        examples: &[r#"names = list(counts)"#],
        avoid: &["Use keys(record), values(record), or items(record) when the intended record view is specific."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "tuple",
        signature: "tuple(value: list | record) -> list",
        use_when: "Use as a Python-compatibility alias for list(value); Stone represents tuples as list values.",
        examples: &[r#"names = tuple(counts)"#],
        avoid: &["Use list(value) when you do not need Python compatibility for generated code."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "min",
        signature: "min(value: Any, ...values: Any) -> Any",
        use_when: "Use for the smallest comparable value.",
        examples: &[r#"lowest = min(a, b, c)"#],
        avoid: &["Do not compare unrelated types such as strings and numbers."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "max",
        signature: "max(value: Any, ...values: Any) -> Any",
        use_when: "Use for the largest comparable value.",
        examples: &[r#"highest = max(a, b, c)"#],
        avoid: &["Use sort(rows, key=...) for top-N records."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "round",
        signature: "round(value: int | float, digits: int = 0) -> int | float",
        use_when: "Use for rounded numeric outputs, especially task-required decimal precision.",
        examples: &[r#"avg = round(total / count, 2)"#],
        avoid: &["Do not pass strings; convert with float() first."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "parse_int",
        signature: "parse_int(value: Any, default: Any) -> int | Any",
        use_when: "Use when bad integer input should fall back instead of failing.",
        examples: &[r#"qty = parse_int(row["qty"], 0)"#],
        avoid: &["Use int(value) when malformed input should be treated as an error."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "parse_float",
        signature: "parse_float(value: Any, default: Any) -> float | Any",
        use_when: "Use when bad floating-point input should fall back instead of failing.",
        examples: &[r#"amount = parse_float(row["amount"], 0.0)"#],
        avoid: &["Use float(value) when malformed input should be treated as an error."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "pwd",
        signature: "pwd() -> str",
        use_when: "Use to inspect the current Stone working directory.",
        examples: &[r#"emit(pwd())"#],
        avoid: &["Use absolute /app paths for task inputs when possible."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "cd",
        signature: "cd(path: str) -> str",
        use_when: "Use to change the current Stone working directory for later session calls.",
        examples: &[r#"cd("/app/subdir")"#],
        avoid: &["For one command only, prefer run(argv, cwd=...) instead of changing session cwd."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "ls",
        signature: "ls(path: str? = cwd) -> list[record]",
        use_when: "Use for shallow directory inspection. Alias for list_dir.",
        examples: &[r#"entries = ls("/app")"#],
        avoid: &["Use find(root, glob) for recursive discovery."],
        aliases: &["list_dir"],
    },
    StoneHelpEntry {
        name: "open",
        signature: r#"open(path: str, mode: "r" | "w" | "a" = "r") -> file"#,
        use_when: "Use for streaming/line-oriented text reads or simple text writes.",
        examples: &[
            r#"text = open("/app/input.txt").read()"#,
            r#"lines = []
for line in open("/app/input.txt"):
    lines.append(line.strip())"#,
            r#"open("/app/out.txt", "w").write("done\n")"#,
        ],
        avoid: &[
            "Do not emit/return file objects; read them first.",
            "For JSON/CSV/JSONL, prefer the structured helpers.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "cat",
        signature: "cat(path: str | file_record) -> str",
        use_when: "Use for quick whole-file UTF-8 text reads.",
        examples: &[r#"text = cat("/app/report.txt")"#],
        avoid: &["Use read_file(path, max_bytes=...) for bounded reads and structured helpers for JSON/CSV/JSONL."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_file",
        signature: "read_file(path: str, max_bytes: int? = None) -> str",
        use_when: "Use for bounded plain-text reads. Alias for read_text.",
        examples: &[r#"text = read_file("/app/report.txt")"#],
        avoid: &["Do not parse JSON/CSV manually if read_json/read_jsonl/read_csv fits."],
        aliases: &["read_text"],
    },
    StoneHelpEntry {
        name: "write_file",
        signature: "write_file(path: str, text: str, append: bool = False) -> record",
        use_when: "Use for writing final text outputs. Alias for write_text.",
        examples: &[r#"write_file("/app/report.txt", "ok\n")"#],
        avoid: &[
            "Do not json_dumps then write_file for JSON outputs; prefer write_json.",
            "Pass append=True only when the task explicitly needs append behavior.",
        ],
        aliases: &["write_text"],
    },
    StoneHelpEntry {
        name: "find",
        signature: "find(root: str, name_glob: str = '*', path_glob: str? = None, type: str? = None) -> list[record]",
        use_when: "Use to discover task input files by name/path glob and optional type, size, or modified-time filters.",
        examples: &[
            r#"files = find("/app", "*.jsonl")"#,
            r#"py = find("/app", path_glob="**/*.py", type="file")"#,
            r#"rows = read_jsonl(files[0])"#,
        ],
        avoid: &["Do not import glob/pathlib/os; use find instead."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "diff",
        signature: "diff(path_a: str, path_b: str) -> record",
        use_when: "Use to compare two text files and inspect structured hunks with line numbers.",
        examples: &[r#"changes = diff("expected.txt", "actual.txt")"#],
        avoid: &["For binary files or very large files, use run([\"diff\", ...]) explicitly."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "search",
        signature: "search(root: str, needle: str) -> list[record]",
        use_when: "Use for bounded literal text search across UTF-8 files.",
        examples: &[r#"matches = search("/app", "ERROR")"#],
        avoid: &["Use read_json/read_csv/read_jsonl for structured data filtering."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "stat",
        signature: "stat(path: str, follow_symlinks: bool = False) -> record",
        use_when: "Use to inspect file type, size, and timestamps.",
        examples: &[r#"info = stat("/app/input.txt")"#],
        avoid: &["Use ls/list_dir when you need multiple directory entries."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_csv",
        signature: "read_csv(path_or_file: str | record, limit: int? = None) -> list[record]",
        use_when: "Use for headered CSV. Values are strings. Quoted fields may contain commas, quotes, and newlines.",
        examples: &[
            r#"rows = read_csv("/app/input.csv")"#,
            r#"sample = read_csv("/app/input.csv", limit=5)"#,
        ],
        avoid: &["Convert with int()/float() before arithmetic; Stone does not coerce strings."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "write_csv",
        signature: "write_csv(path: str, rows: list[record], columns: list[str]? = None) -> record",
        use_when: "Use to write headered CSV output from record rows with standard CSV quoting.",
        examples: &[r#"write_csv("/app/out.csv", [{"name": "ada", "score": 10}])"#],
        avoid: &["Do not hand-roll CSV quoting with string concatenation unless the format is intentionally nonstandard."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_json",
        signature: "read_json(path_or_file: str | record) -> Any",
        use_when: "Use for JSON files.",
        examples: &[r#"data = read_json("/app/config.json")"#],
        avoid: &["Do not import json; use read_json/json_loads/json_dumps."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_jsonl",
        signature: "read_jsonl(path_or_file: str | record, limit: int? = None) -> list[Any]",
        use_when: "Use for JSON Lines data. Prefer this over manual line parsing.",
        examples: &[
            r#"rows = read_jsonl("/app/events.jsonl")"#,
            r#"sample = read_jsonl("/app/events.jsonl", limit=5)"#,
        ],
        avoid: &["Do not emit huge row lists to inspect them; use a limit for samples."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "json_loads",
        signature: "json_loads(text: str) -> Any",
        use_when: "Use to parse JSON text already held in a string.",
        examples: &[r#"value = json_loads(text)"#],
        avoid: &["Use read_json(path) for JSON files."],
        aliases: &["from_json"],
    },
    StoneHelpEntry {
        name: "json_dumps",
        signature: "json_dumps(value: Any, indent: int? = None, separators: list? = None) -> str",
        use_when: "Use to serialize a value to JSON text. Supports compact output, indent=2, and separators=(\",\", \":\") for Python-shaped agent code.",
        examples: &[
            r#"text = json_dumps({"ok": True})"#,
            r#"pretty = json_dumps({"ok": True}, indent=2)"#,
            r#"compact = json_dumps({"ok": True}, separators=(",", ":"))"#,
        ],
        avoid: &["Use write_json(path, value) for final JSON files."],
        aliases: &["to_json"],
    },
    StoneHelpEntry {
        name: "md5",
        signature: "md5(text: str) -> str",
        use_when: "Use to compute a lowercase hexadecimal MD5 digest of text.",
        examples: &[r#"digest = md5("abcdefghijklmnopqrstuvwxyz")"#],
        avoid: &["Do not import hashlib for MD5 hashing."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sha1",
        signature: "sha1(text: str) -> str",
        use_when: "Use to compute a lowercase hexadecimal SHA-1 digest of text.",
        examples: &[r#"digest = sha1("abcdefghijklmnopqrstuvwxyz")"#],
        avoid: &["Do not import hashlib for SHA-1 hashing."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sha256",
        signature: "sha256(text: str) -> str",
        use_when: "Use to compute a lowercase hexadecimal SHA-256 digest of text.",
        examples: &[r#"digest = sha256("abcdefghijklmnopqrstuvwxyz")"#],
        avoid: &["Do not import hashlib for SHA-256 hashing."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "write_json",
        signature: "write_json(path: str, value: Any) -> int",
        use_when: "Use for final JSON outputs from dictionaries/lists.",
        examples: &[r#"write_json("/app/out.json", {"ok": True, "items": rows})"#],
        avoid: &["Do not wrap values in json_dumps before write_json."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "write_jsonl",
        signature: "write_jsonl(path: str, rows: list[Any]) -> int",
        use_when: "Use for JSON Lines output files.",
        examples: &[r#"write_jsonl("/app/out.jsonl", rows)"#],
        avoid: &["Pass a list of row values, not pre-joined JSONL text."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "to_jsonl",
        signature: "to_jsonl(rows: list[Any]) -> str",
        use_when: "Use to serialize row values to JSON Lines text already held in memory.",
        examples: &[r#"text = to_jsonl(rows)"#],
        avoid: &["Use write_jsonl(path, rows) for final JSONL files."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "save",
        signature: "save(value: Any, path: str, append: bool = False, force: bool = False) -> record",
        use_when: "Use to write an explicit value to a file when write_file/write_json do not fit.",
        examples: &[r#"save(to_json(rows), "/app/rows.json", force=True)"#],
        avoid: &["Do not rely on Nu pipeline input; pass the value explicitly as the first argument."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "edit",
        signature: "edit(path: str, old: str, new: str, all: bool = False) -> record",
        use_when: "Use for exact text replacement in a UTF-8 file.",
        examples: &[r#"edit("/app/config.txt", "debug=false", "debug=true")"#],
        avoid: &["Do not pass empty old text; read a sample first if the replacement is risky."],
        aliases: &["edit_file"],
    },
    StoneHelpEntry {
        name: "mkdir",
        signature: "mkdir(path: str, ...paths: str) -> None",
        use_when: "Use to create directories, including parents.",
        examples: &[r#"mkdir("/app/out/logs")"#],
        avoid: &["Use write_file/write_json when only parent creation is needed for a file."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "rm",
        signature: "rm(path: str, ...paths: str) -> None",
        use_when: "Use to remove explicit files or directories.",
        examples: &[r#"rm("/app/tmp.txt")"#],
        avoid: &["Avoid broad cleanup patterns; pass explicit paths."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "where",
        signature: "where(rows: list[record], key: str, expected: Any) | where(rows, key, op, expected) | where(rows, predicate) -> list[record]",
        use_when: "Use for equality, comparison, or lambda predicate filtering without pipeline syntax.",
        examples: &[
            r#"west = where(rows, "region", "west")"#,
            r#"large = where(rows, "size", ">", 1024)"#,
            r#"open_west = where(rows, lambda r: r["status"] == "open" and r["region"] == "west")"#,
        ],
        avoid: &["Use explicit loops when filtering needs side effects or expensive setup."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "keys",
        signature: "keys(record: record) -> list[str]",
        use_when: "Use to inspect or iterate record field names.",
        examples: &[r#"names = keys(row)"#],
        avoid: &["Use row.keys() when method syntax is clearer."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "values",
        signature: "values(record: record) -> list[Any]",
        use_when: "Use to inspect or iterate record values.",
        examples: &[r#"vals = values(row)"#],
        avoid: &["Use items(record) when keys are needed too."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "items",
        signature: "items(record: record) -> list[list[Any]]",
        use_when: "Use to iterate key/value pairs from a record.",
        examples: &[r#"pairs = []
for key, value in items(counts):
    pairs.append(key + ":" + str(value))"#],
        avoid: &["Initialize dictionary counters before incrementing missing keys."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "get",
        signature: "get(record: record, key: str, default: Any = None) -> Any",
        use_when: "Use to read optional record fields with a fallback.",
        examples: &[r#"score = get(row, "score", 0)"#],
        avoid: &["Use row[key] when a missing key should be an error."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sort",
        signature: "sort(values, key: str | lambda? = None, reverse: bool = False) -> list",
        use_when: "Use for sorted copies and top-N record lists.",
        examples: &[
            r#"top = sort(rows, key="amount", reverse=True)[:5]"#,
            r#"top = sort(rows, key=lambda r: (-r["count"], r["name"]))[:5]"#,
            r#"rows.sort(key=lambda r: r["name"], reverse=True)"#,
            r#"names = sort(names)"#,
        ],
        avoid: &["Remember list.sort(...) mutates in place and returns None; use top-level sort(...) when you need a sorted copy."],
        aliases: &["sorted"],
    },
    StoneHelpEntry {
        name: "map",
        signature: "map(lambda_or_builtin, values: iterable) -> list",
        use_when: "Use for compact per-item transforms when a lambda is clearer than an explicit loop.",
        examples: &[r#"names = map(lambda r: r["name"], rows)"#],
        avoid: &["Use explicit loops when the transform needs statements or mutation."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "filter",
        signature: "filter(lambda, values: iterable) -> list",
        use_when: "Use for compact per-item filtering when a lambda is clearer than an explicit loop.",
        examples: &[r#"errors = filter(lambda r: r["status"] == 404, rows)"#],
        avoid: &["Use where(rows, key, expected) for simple equality on one record field."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "all",
        signature: "all(values: iterable | generator) -> bool",
        use_when: "Use to test whether every value is truthy, with generator short-circuiting.",
        examples: &[r#"ok = all("score" in row for row in rows)"#],
        avoid: &["Use explicit loops when you need to collect diagnostics for failed items."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "any",
        signature: "any(values: iterable | generator) -> bool",
        use_when: "Use to test whether any value is truthy, with generator short-circuiting.",
        examples: &[r#"has_error = any("ERROR" in line for line in lines)"#],
        avoid: &["Use search(root, needle) for file content search."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sum",
        signature: "sum(values: iterable | generator) -> int | float",
        use_when: "Use for numeric totals over lists or generator expressions.",
        examples: &[r#"total = sum(int(row["qty"]) for row in rows)"#],
        avoid: &["Convert strings to numbers before summing."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "set",
        signature: "set(values: iterable | generator? = None) -> list",
        use_when: "Use for Python-shaped ordered uniqueness. The result is a list with unique values.",
        examples: &[
            r#"seen = set()"#,
            r#"seen.add(user)"#,
            r#"unique_names = set(names)"#,
            r#"unique_names = set(row["name"] for row in rows)"#,
        ],
        avoid: &["Do not rely on hash-set ordering; Stone preserves first-seen order."],
        aliases: &["unique"],
    },
    StoneHelpEntry {
        name: "type",
        signature: "type(value: Any) -> str",
        use_when: "Use for lightweight validation when checking task outputs or nominal control values. Attempt control reports attempt_handle, attempt_outcome, attempt_scope, semantic_frontier, and attempt_acceptance without flattening them to record.",
        examples: &[
            r#"ok = type(row["name"]) == "str""#,
            r#"scope = attempt_scope()
ok = type(scope) == "attempt_scope""#,
        ],
        avoid: &["Prefer direct conversions like int()/float() when you need numeric values."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "first",
        signature: "first(values: list, count: int? = None) -> Any | list",
        use_when: "Use to inspect or keep the first item(s) of a list.",
        examples: &[r#"sample = first(rows, 5)"#, r#"sample = head(rows, 5)"#],
        avoid: &["Use slicing when it is clearer, such as rows[:5]."],
        aliases: &["head"],
    },
    StoneHelpEntry {
        name: "last",
        signature: "last(values: list, count: int? = None) -> Any | list",
        use_when: "Use to inspect or keep the last item(s) of a list.",
        examples: &[r#"tail_sample = last(rows, 5)"#, r#"tail_sample = tail(rows, 5)"#],
        avoid: &["Use slicing when it is clearer."],
        aliases: &["tail"],
    },
    StoneHelpEntry {
        name: "range",
        signature: "range(stop: int) | range(start: int, stop: int, step: int = 1) -> list[int]",
        use_when: "Use for numeric loops and index generation.",
        examples: &[r#"seen = []
for i in range(3):
    seen.append(i)"#, r#"indexes = range(1, 10, 2)"#],
        avoid: &["Use enumerate(values) when you need indexes and values together."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "enumerate",
        signature: "enumerate(values: iterable, start: int = 0) -> list[list[Any]]",
        use_when: "Use to iterate indexes and values together.",
        examples: &[r#"labels = []
for i, row in enumerate(rows):
    labels.append(str(i) + ":" + row["name"])"#],
        avoid: &["Use range(len(values)) only when you specifically need index-only access."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "split",
        signature: "split(text: str, separator: str? = None, maxsplit: int? = None) -> list[str]",
        use_when: "Use for top-level text splitting; string method syntax also works.",
        examples: &[r#"parts = split(line, ",")"#, r#"key, rest = "name:value:extra".split(":", 1)"#, r#"words = split(line)"#],
        avoid: &["For line splitting, prefer text.splitlines() when operating on a string."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "join",
        signature: "join(items: iterable | generator, separator: str = \"\") -> str",
        use_when: "Use for top-level list-to-text joining; string method syntax also works.",
        examples: &[
            r#"line = join(fields, ",")"#,
            r#"initials = "".join(word[0] for word in names)"#,
        ],
        avoid: &["Convert non-string items with map(str, items) or explicit str() first."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "slice",
        signature: "slice(value: str | list, start: int? = None, end: int? = None) -> str | list",
        use_when: "Use for dynamic slicing when bracket syntax is awkward.",
        examples: &[r#"top = slice(rows, 0, 5)"#],
        avoid: &["Use rows[:5] when bounds are simple literals."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "starts_with",
        signature: "starts_with(text: str, prefix: str) -> bool",
        use_when: "Use for prefix tests; startswith is an alias.",
        examples: &[r#"level = "other"
if starts_with(line, "ERROR"):
    level = "error""#],
        avoid: &["Use string method line.startswith(prefix) when method syntax is clearer."],
        aliases: &["startswith"],
    },
    StoneHelpEntry {
        name: "format",
        signature: "format(template: str, ...values: Any) -> str",
        use_when: "Use for small positional text templates, numbered placeholders, and simple fixed decimal specs.",
        examples: &[
            r#"line = format("{}:{}", name, count)"#,
            r#"line = format("{1}:{0}", name, count)"#,
            r#"amount = format("{:.2f}", total)"#,
        ],
        avoid: &["Use f-strings when they are clearer and do not need format specs."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "print",
        signature: "print(...values: Any) -> Any",
        use_when: "Use only for diagnostic stdout during local probes.",
        examples: &[r#"print("debug:", count)"#],
        avoid: &["Use emit(value) for structured results and write_file/write_json for task outputs."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run",
        signature: r#"run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, background: bool = False, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576, hooks: transition_hooks | {pre: callable?, post: callable?} = {}) -> record"#,
        use_when: "Use only when the task explicitly needs a POSIX program. Nonzero exits return ok=false with stdout, stderr, and an explanation record. Use run_complete for a bounded task command that must finish before the program continues; use background=True when the program intentionally manages a live run_id.",
        examples: &[
            r#"result = run(["wc", "-l", "/app/input.txt"])"#,
            r#"result = run(["printf", "ok"], timeout_ms=5000)"#,
            r#"result = run(["printf", "ok"], hooks={"pre": lambda step: {"argv": step.input.argv}})"#,
            r#"result = run(["printf", "not executed"], hooks={"pre": lambda step: {"allow": False, "reason": "command denied"}})"#,
            r#"async def build():
    return await run(["sh", "-c", "sleep 0.01; printf done"], timeout_ms=5000)
result = build()"#,
            r#"def record_run(step):
    result = step.outcome.value
    return context_write("outcome.last_run", "outcome", {"ok": step.outcome.ok, "exit_code": result.exit_code, "stderr": result.stderr})
result = run(["sh", "-c", "printf failed >&2; exit 7"], hooks={"post": record_run})"#,
            r#"result = run(["sh", "-c", "sleep 0.01 && printf done"], cwd="/app", timeout_ms=5000)"#,
            r#"result = run(["sh", "-c", "printf warning >&2"], stdout="suppress", stderr="capture", max_stderr_bytes=12000)"#,
            r#"if not result.ok:
    emit({"exit_code": result.exit_code, "stderr": result.stderr, "explanation": result.explanation})"#,
        ],
        avoid: &[
            "Do not pass shell strings; pass argv lists.",
            "Do not use run for normal file/JSON/CSV work.",
            "Use run_complete for long task commands that must finish before the next Stone statement, such as builds, tests, installs, downloads, benchmarks, or data processing.",
            "Inside async def, `await run(...)` is the explicit-effect spelling of run_complete and owns the command through terminal completion.",
            "Do not use shell backgrounding, nohup, or `&`; use background=True for long task commands, or start_daemon() for servers/services that must stay running while tests execute.",
            "For noisy commands, suppress or cap output explicitly instead of flooding stdout/stderr.",
            "Do not ignore result.ok; inspect stderr, exit_code, timed_out, and explanation before retrying.",
            "If result.still_running is true and result.run_id is present, use run_status(result.run_id), run_wait(result.run_id, timeout_ms=...), or run_terminate(result.run_id).",
            "After run_wait returns still_running=false or done=true, do not call run_wait for that run_id again.",
            "If result.timed_out is true without a run_id, inspect partial output first; rerun with a larger timeout_ms only when the command is expected to be slow.",
            "Use call-local hooks for one action transition; pre may replace argv or veto without executing it. A veto returns ok=false, kind=policy_rejected, and policy_reason so the agent can revise its action; the post hook still records that outcome.",
            "A hook transition record uses transition_id (not id), kind, phase, and input; post hooks also receive outcome.",
            "A completed nonzero run gives its post hook outcome.ok=false and the structured run record in outcome.value; outcome.error is for an effect that produced no run record.",
            "Transition hooks cannot recursively call model_call, run, run_complete, or must_run.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "must_run",
        signature: r#"must_run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576) -> record"#,
        use_when: "Use for set -e style process steps: it returns the same run record on success and raises a Stone error when the external process exits nonzero or times out.",
        examples: &[
            r#"must_run(["mkdir", "-p", "target/out"])"#,
            r#"step = must_run(["printf", "ok"], timeout_ms=5000)"#,
            r#"must_run(["sh", "-c", "printf input"], stdout="suppress", stderr="capture", max_stderr_bytes=12000)"#,
        ],
        avoid: &[
            "Use run() instead when a nonzero exit is expected and should be handled as data.",
            "Do not use must_run for normal file/JSON/CSV work.",
            "Do not pass shell strings; pass argv lists.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run_complete",
        signature: r#"run_complete(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576, hooks: transition_hooks | {pre: callable?, post: callable?} = {}) -> record"#,
        use_when: "Use for a bounded task command that may outlive one Gateway observation window but must reach a terminal result before the Stone program continues. It lowers to run plus bounded waits, owns the run_id, and terminates the process if the total timeout expires.",
        examples: &[
            r#"built = run_complete(["sh", "-c", "printf built"], cwd="/app", timeout_ms=360000, max_stdout_bytes=4096, max_stderr_bytes=8192)"#,
            r#"trained = run_complete(["python3", "train.py"], timeout_ms=600000, stdout="suppress", max_stderr_bytes=4096)"#,
        ],
        avoid: &[
            "Use run(..., background=True) when the program intentionally needs to inspect or interleave with a live process.",
            "Do not pass background=True; run_complete owns the lifecycle through terminal completion.",
            "Do not use unbounded shell retry or polling loops around run_complete.",
            "Inspect result.ok, stderr, exit_code, timed_out, and explanation before retrying.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run_status",
        signature: "run_status(run_id: str) -> record",
        use_when: "Gateway mode only. Use after Gateway-backed run() returns still_running=true and a run_id when you want an immediate per-run status check without waiting.",
        examples: &[r#"if "still_running" in result and result.still_running:
    status = run_status(result.run_id)"#],
        avoid: &[
            "Do not use for general workspace state; use state() for runtime and transaction state.",
            "Do not call for normal completed run() results without a run_id.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run_wait",
        signature: "run_wait(run_id: str, timeout_ms: int = 30000) -> record",
        use_when: "Gateway mode only. Use after Gateway-backed run() returns timed_out=true, still_running=true, and a run_id when you intentionally want to wait; timeout_ms=0 waits until finish.",
        examples: &[r#"while "still_running" in result and result.still_running:
    status = run_status(result.run_id)
    result = run_wait(result.run_id, timeout_ms=30000)"#],
        avoid: &[
            "Do not call for normal completed run() results without a run_id.",
            "Do not use long waits through MCP when you need interactive progress; use run_status() and short run_wait() calls.",
            "Do not call again after run_wait returns still_running=false or done=true; inspect the result and continue the task.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run_terminate",
        signature: "run_terminate(run_id: str) -> record",
        use_when: "Use to stop a Gateway-backed run() that returned still_running=true when the command should not continue.",
        examples: &[r#"if "still_running" in result and result.still_running:
    stopped = run_terminate(result.run_id)"#],
        avoid: &["Prefer run_wait() if the command is expected to finish soon."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "resolve_command",
        signature: "resolve_command(name: str) -> record",
        use_when: "Use to explain how Stone would resolve an external executable name without starting a process.",
        examples: &[
            r#"info = resolve_command("python3")"#,
            r#"info = resolve_command("definitely-not-a-real-command")
if not info.ok:
    emit(info.explanation)"#,
        ],
        avoid: &[
            "Use run() when you need to execute the command.",
            "Do not use shell-specific command lookup probes when this Stone helper is available.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "ps",
        signature: "ps(interval_ms: int = 0) -> list[record]",
        use_when: "Use to inspect live processes as typed records without scraping /proc or shelling out to ps.",
        examples: &[
            r#"procs = ps()"#,
            r#"python = where(ps(), lambda p: p["command"].find("python") >= 0)"#,
        ],
        avoid: &[
            "Do not parse `ps aux` text when process id, command, status, cwd, CPU, and memory fields are needed.",
            "Pass a nonzero interval_ms only when cpu_percent matters; it samples over that interval.",
        ],
        aliases: &["process_list"],
    },
    StoneHelpEntry {
        name: "sysinfo",
        signature: r#"sysinfo(section: "os" | "cpu" | "cpu_long" | "mem" | "disks" | "net" | "temp" | "users" | "all" = "all") -> record | list"#,
        use_when: "Use to inspect typed host system facts without shelling out to uname, free, df, ip, or sysctl-style commands.",
        examples: &[
            r#"host = sysinfo("os")"#,
            r#"mem = sysinfo("mem")"#,
            r#"emit({"os": sysinfo("os").os, "cpus": len(sysinfo("cpu"))})"#,
        ],
        avoid: &[
            "Do not parse platform command text when a sysinfo section has the needed fields.",
            "Use sysinfo(\"cpu_long\") only when sampled CPU usage is needed; it waits briefly to sample.",
        ],
        aliases: &["sys", "sys_info"],
    },
    StoneHelpEntry {
        name: "state",
        signature: "state() -> record",
        use_when: "Use to retrieve cheap agent-facing runtime state such as cwd, workspace root, git status, common tool availability, and Gateway transaction state when active.",
        examples: &[r#"snapshot = state()"#, r#"emit(state().workspace)"#],
        avoid: &["Do not shell out to git status or which/version probes when this structured snapshot is enough."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_info",
        signature: r#"attempt_info(attempt: str = "") -> record"#,
        use_when: "Use in Gateway mode to inspect the current task attempt, or a specific attempt when an id is passed. The record exposes attempt, task, workspace, typed controller_run_count (1 for the first running controller), controller_restarted, controller_phase (pending, initial, or restart), memory_ref, and memory_revision.",
        examples: &[
            r#"me = attempt_info()"#,
            r#"emit({"attempt": me.attempt, "task": me.task, "workspace": me.workspace})"#,
            r#"emit(attempt_info().state)"#,
            r#"lifecycle = attempt_info()
if lifecycle.controller_run_count == 1:
    emit({"phase": "initial"})
else:
    emit({"phase": "restart", "run": lifecycle.controller_run_count})"#,
        ],
        avoid: &[
            "Do not infer attempt identity from transaction ids; use the structured attempt record.",
            "Do not infer controller restart from a workspace marker; use controller_run_count or controller_restarted.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_state",
        signature: r#"attempt_state(attempt: str = "", sample_limit: int = 100) -> record"#,
        use_when: "Use in Gateway mode to inspect an attempt plus its transaction diff state.",
        examples: &[r#"state = attempt_state()"#, r#"emit(attempt_state(sample_limit=25).clean)"#],
        avoid: &["Do not treat attempt state as a commit; finish the attempt explicitly when work is resolved."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_inspect",
        signature: r#"attempt_inspect(attempt: str = "", include_details: bool = False, trace_limit: int = 20, max_bytes: int = 32768) -> record"#,
        use_when: "Gateway mode only. Page in a bounded child summary, optional full controller envelope, relevant trace tail, and authoritative execution-resource state before or after candidate cleanup.",
        examples: &[r#"inspection = attempt_inspect(child.attempt, include_details=True, trace_limit=20)"#],
        avoid: &[
            "Do not inject full details by default; attempt_join(child).result.value is the compact comparison report.",
            "Inspection does not select or clean a candidate; call attempt_accept or attempt_discard after deciding.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempts",
        signature: r#"attempts(task: str = "", workspace: str = "", state: str = "") -> list[record]"#,
        use_when: "Use in Gateway mode to list task attempts with optional task, workspace, or lifecycle-state filters.",
        examples: &[r#"active = attempts(state="active")"#],
        avoid: &["Do not scan gateway storage directories directly to discover attempts."],
        aliases: &["attempt_list"],
    },
    StoneHelpEntry {
        name: "attempt_spawn",
        signature: r#"attempt_spawn(task: str = "", workspace: str = "", task_spec: record = {}, task_input: Any = None, context_prompt_view: {"required_keys": [str, ...]}? = None, program: record = {}, entrypoint: str = "", workspace_source: record = {}, context_source: record = {}, capabilities: record = {}, start: bool = false, scope: attempt_scope? = None, controller: str = "", capability_profile: str = "", container: str = "", workspace_mount: str = "", parent_attempt: str = "", resource_limits: record = {}, metadata: record = {}) -> attempt_handle"#,
        use_when: "Use in Gateway mode when a controller needs to create a new top-level task attempt with its own transaction and explicit task/control-flow definition.",
        examples: &[r#"child = attempt_spawn(task_spec={"id": "task-debug", "objective": "write hello.txt"}, workspace_source={"workspace": "repo"}, program={"kind": "stone", "source": "write_file(\"hello.txt\", \"hello\")"})"#],
        avoid: &["Do not spawn attempts for ordinary file edits inside the current attempt; use workspace builtins directly."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_start",
        signature: r#"attempt_start(attempt: str = "") -> record"#,
        use_when: "Use in Gateway mode to start an existing attempt's recorded controller program after inspecting or preparing the attempt.",
        examples: &[r#"child = attempt_spawn(task_spec={"id": "task-debug"}, workspace_source={"workspace": "repo"}, program={"kind": "stone", "source": "write_file(\"hello.txt\", \"hello\")"})"#, r#"started = attempt_start(child.attempt)"#],
        avoid: &["Do not use attempt_start for ordinary shell commands inside the current attempt; use run() or workspace builtins."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_wait",
        signature: r#"attempt_wait(attempt: str = "", timeout_ms: int? = None) -> record"#,
        use_when: "Use in Gateway mode after starting an asynchronous child controller; it returns the updated attempt record.",
        examples: &[r#"done = attempt_wait(child.attempt, timeout_ms=30000)"#],
        avoid: &["Do not poll in a tight loop; use a bounded wait and then inspect attempt_state or controller logs."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_join",
        signature: r#"attempt_join(attempt: attempt_handle | str | record, timeout_ms: int? = None) -> attempt_outcome"#,
        use_when: "Gateway mode only. Wait for a child and return an immutable typed outcome. outcome.ok and outcome.succeeded are true only when the controller joined and reported succeeded; result.value is the child's compact returned summary.",
        examples: &[r#"outcome = attempt_join(child, timeout_ms=30000)"#],
        avoid: &["Joining observes child completion; it does not accept, merge, publish, or discard the child workspace."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_wait_any",
        signature: r#"attempt_wait_any(children: attempt_scope | list[attempt_handle | str | record], timeout_ms: int? = None) -> attempt_outcome"#,
        use_when: "Gateway mode only. Block in one Gateway wait-set syscall until any supervised child controller exits or the timeout expires; a timeout returns an outcome with timed_out=True and an empty attempt id.",
        examples: &[r#"first = attempt_wait_any(scope, timeout_ms=30000)"#],
        avoid: &["Do not recreate wait-any with sequential polling; it biases child order and wastes Gateway calls."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_wait_all",
        signature: r#"attempt_wait_all(children: attempt_scope | list[attempt_handle | str | record], timeout_ms: int? = None) -> record"#,
        use_when: "Gateway mode only. Block in one Gateway wait-set syscall until every selected child exits or the timeout expires; returns completed, timed_out, outcomes, and pending.",
        examples: &[r#"batch = attempt_wait_all(scope, timeout_ms=30000)"#],
        avoid: &["Do not assume wait-all selects child workspaces; explicitly accept one candidate and discard the others."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_terminate",
        signature: r#"attempt_terminate(attempt: str | record) -> record"#,
        use_when: "Gateway mode only. Request termination of a running child controller before joining it.",
        examples: &[r#"attempt_terminate(child)"#],
        avoid: &["Do not use termination as workspace cleanup; join the controller and discard or accept the child afterward."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_scope",
        signature: r#"attempt_scope(exit_policy: "cancel_then_join" = "cancel_then_join", join_timeout_ms: int = 5000) -> attempt_scope"#,
        use_when: "Create a task-owned supervision scope before spawning child attempts. Use `with scope:` when every consumer fits naturally inside one block; otherwise call attempt_scope_close explicitly. `scope.branch(frontier, ...)`, `scope.wait_any(...)`, and `scope.wait_all(...)` are thin method forms of the existing lifecycle operations.",
        examples: &[r#"scope = attempt_scope(join_timeout_ms=2000)
with scope:
    marker = "supervised"
emit(scope.closed)"#,
            r#"async def explore():
    async with attempt_scope(join_timeout_ms=2000) as scope:
        marker = "supervised"
    return {"marker": marker, "clean": scope.closed}
result = explore()"#,
        ],
        avoid: &[
            "Do not persist an open attempt scope across Stone evaluations; it is closed automatically at the current evaluation boundary.",
            "Do not treat scope cleanup as candidate selection; explicitly accept the winner and discard known losers.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_scope_add",
        signature: r#"attempt_scope_add(scope: attempt_scope, child: str | record) -> attempt_scope"#,
        use_when: "Gateway mode only. Register an already-created child with a supervision scope; prefer passing scope=scope directly to attempt_spawn or attempt_fork.",
        examples: &[r#"scope = attempt_scope_add(scope, child)"#],
        avoid: &["Do not add unrelated or non-child attempts to a scope."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_scope_close",
        signature: r#"attempt_scope_close(scope: attempt_scope, reason: str = "attempt scope closed") -> record"#,
        use_when: "Close a supervision scope explicitly and inspect its structured per-child cancel, join, discard, and cleanup report.",
        examples: &[r#"scope = attempt_scope()
emit(attempt_scope_close(scope).clean)"#],
        avoid: &["Do not ignore clean=False; inspect children[].errors and resolve the remaining attempt state."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_fork",
        signature: r#"attempt_fork(parent_attempt: attempt_handle | str | record = "", checkpoint: str = "", task: str = "", input: Any = None, context_prompt_view: {"required_keys": [str, ...]}? = None, program: record? = None, entrypoint: str = "", start: bool = False, scope: attempt_scope? = None, controller: str = "", capability_profile: str = "", container: str = "", workspace_mount: str = "", resource_limits: record = {}, metadata: record = {}) -> attempt_handle"#,
        use_when: "Use in Gateway mode to create an isolated child from the current parent frontier, or from an opaque verified stage checkpoint owned by that parent.",
        examples: &[
            r#"branch = attempt_fork(task="try-alt-fix", metadata={"strategy": "alternate"})"#,
            r#"branch = attempt_fork(checkpoint=report.stages[0].checkpoint.reference, input={"strategy": "alternate"})"#,
            r#"branch = attempt_fork(input={"strategy": "alternate"}, context_prompt_view={"required_keys": ["requirement.target"]}, program=current_program(), entrypoint="worker", start=True, scope=scope)"#,
        ],
        avoid: &[
            "Do not assume a fork mutates the parent attempt; it returns a separate attempt and transaction.",
            "Do not treat a workspace checkpoint as a full tool-environment snapshot; forkable provider state is a separate checkpoint plane.",
            "A reconstructed child becomes conservatively non-joinable after it starts a mutable provider container until mutable tool-environment snapshot/join support exists.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "semantic_frontier",
        signature: r#"semantic_frontier(checkpoint: workflow_checkpoint, owner: attempt_handle | attempt_outcome | record? = None) -> semantic_frontier"#,
        use_when: "Gateway mode only. Turn a successfully sealed forkable or repairable workflow checkpoint into a nominal task-owned branch capability. Omit owner for a checkpoint owned by the current attempt; pass the joined failed owner for a retained repair checkpoint. The value carries cost guidance and hides raw checkpoint ownership and restoration ABI.",
        examples: &[
            r#"frontier = semantic_frontier(report.stages[0].checkpoint)"#,
            r#"frontier = semantic_frontier(failed_report.stages[1].checkpoint, owner=failed_child)"#,
            r#"with semantic_frontier(report.stages[0].checkpoint) as frontier:
    child = scope.branch(frontier, input={"strategy": "alternate"})"#,
        ],
        avoid: &[
            "Do not pass only checkpoint.reference; the full checkpoint record carries policy, planes, and measured seal cost.",
            "Do not serialize or reconstruct a semantic frontier; it is a task-owned authority value.",
            "A frontier reported as unused in evaluation diagnostics paid seal cost without feeding attempt_branch during that evaluation.",
            "Leaving the block releases frontier authority. Names remain visible afterward for evidence and finalization, but cannot be used to create another branch.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_branch",
        signature: r#"attempt_branch(frontier: semantic_frontier, task: str = "", input: Any = None, context_prompt_view: {"required_keys": [str, ...]}? = None, program: record? = None, entrypoint: str = "", start: bool = False, scope: attempt_scope? = None, controller: str = "", capability_profile: str = "", container: str = "", workspace_mount: str = "", resource_limits: record = {}, metadata: record = {}) -> attempt_handle"#,
        use_when: "Gateway mode only. Create a child from a semantic frontier without deciding whether the checkpoint is owned by the current parent or retained by a failed owner. The runtime lowers to the authoritative fork or repair-restore operation and preserves the same supervision interface.",
        examples: &[r#"frontier = semantic_frontier(report.stages[0].checkpoint)
scope = attempt_scope()
child = attempt_branch(frontier, input={"strategy": "alternate"}, program=current_program(), entrypoint="worker", start=True, scope=scope)"#],
        avoid: &[
            "Do not pass raw checkpoint ids or workspace_source records; construct a semantic_frontier once and reuse it.",
            "Do not omit scope for started branches unless another bounded cleanup owner is explicit.",
            "Branching does not select the result; join, evaluate evidence, then accept or discard it.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "semantic_frontier_release",
        signature:
            "semantic_frontier_release(frontier: semantic_frontier) -> record",
        use_when: "Gateway mode only. Explicitly release a semantic frontier after selection or exhaustion. The runtime uses the frontier's opaque authority, Gateway verifies the owner relationship, and repeated release is safe.",
        examples: &[r#"released = semantic_frontier_release(frontier)"#],
        avoid: &[
            "Do not release a frontier while child attempts still depend on it; close their supervision scope first.",
            "Do not branch from a released frontier; create a new evidenced checkpoint if more exploration is required.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_finish",
        signature: r#"attempt_finish(action: "commit" | "rollback" | "fail" | "kill", attempt: str = "", message: str = "", reason: str = "", allow_risky: bool = False) -> record"#,
        use_when: "Gateway mode only. Compatibility lifecycle operation for closing an attempt by committing, rolling back, failing, or killing it.",
        examples: &[r#"attempt_finish("rollback", reason="debug branch done")"#],
        avoid: &[
            "For normal candidate selection use attempt_report plus attempt_accept or attempt_discard.",
            "Do not finish a parent attempt from a child controller unless that capability was explicitly delegated.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_report",
        signature: r#"attempt_report(status: "succeeded" | "failed", result: Any = {}, error: Any = {}, reason: str = "", metadata: record = {}, attempt: str = "") -> record"#,
        use_when: "Gateway mode only. Use from an attached attempt to record that same attempt's candidate result and evidence. Completed LibOS task programs are normally reported automatically by the runtime.",
        examples: &[r#"report = attempt_report(status="succeeded", result={"tests": "pass"})"#],
        avoid: &["Reporting does not merge or publish workspace state; select the candidate explicitly afterward."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_accept",
        signature:
            "attempt_accept(parent: attempt_handle | str | record, child: attempt_handle | attempt_outcome | str | record) -> attempt_acceptance",
        use_when: "Use in Gateway mode to import one successfully reported direct child's workspace state into its unchanged parent. The nominal result exposes status=\"accepted\", attempt, a typed selected attempt_handle, and the structured import report.",
        examples: &[
            r#"accepted = attempt_accept(root.attempt, child)
emit({"attempt": accepted.attempt, "changes": accepted.file_changes})"#,
            r#"selected = attempt_accept(root.attempt, child).selected"#,
        ],
        avoid: &[
            "Do not accept an unreported or failed child.",
            "The parent must not change between fork and accept; fork candidates from the same stable parent and select before editing it.",
            "An attempt_acceptance is a lifecycle transition result, not an attempt_handle; use accepted.selected when a later typed helper requires the selected handle.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_discard",
        signature: r#"attempt_discard(attempt: str, reason: str = "") -> record"#,
        use_when: "Use in Gateway mode to roll back a failed, risky, or unwanted candidate and release its transaction state.",
        examples: &[r#"attempt_discard(bad.attempt, reason="probe failed")"#],
        avoid: &["Do not leave rejected child attempts active after making a selection."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_publish",
        signature: r#"attempt_publish(attempt: str, expected_generation: str, message: str = "", allow_risky: bool = False) -> record"#,
        use_when: "Gateway mode only. Use only from an authorized root controller after the root has reported success; expected_generation prevents stale publication.",
        examples: &[r#"published = attempt_publish(root.attempt, root.base_generation, message="selected candidate")"#],
        avoid: &[
            "A built-in model agent should normally return finish and let its outer attempt controller report and publish the root.",
            "Never publish a child attempt directly.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_run_process",
        signature: r#"attempt_run_process(attempt: str = "", argv: list[str], env: record = {}) -> record"#,
        use_when: "Use in Gateway mode to ask the host Gateway to run a delegated process inside the currently attached attempt transaction.",
        examples: &[r#"run = attempt_run_process(child.attempt, ["/path/to/controller"], env={"HELIX_ROOT": "/path/to/helix"})"#],
        avoid: &[
            "An attached parent cannot run a process in a child directly; fork the child with a program and start it so the child receives its own scoped channel.",
            "Do not use for ordinary Linux commands inside the current workspace; use run() for POSIX tools.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_state",
        signature: "env_state(sample_limit: int = 100) -> record",
        use_when: "Use in Gateway mode to inspect uncommitted transaction changes, warnings, and a bounded structured diff.",
        examples: &[r#"changes = env_state()"#, r#"emit(env_state(sample_limit=25).clean)"#],
        avoid: &["Do not wait until the final answer to inspect risky file changes after running commands."],
        aliases: &["env_diff"],
    },
    StoneHelpEntry {
        name: "env_tx_info",
        signature: r#"env_tx_info(tx: str = "") -> record"#,
        use_when: "Use in Gateway mode to inspect transaction metadata such as parent checkpoint and retained checkpoint-run purpose.",
        examples: &[r#"info = env_tx_info()"#, r#"info = env_tx_info(debug.branch_tx)"#],
        avoid: &["Do not infer retained branch lifecycle from path names; use structured metadata."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_txs",
        signature: r#"env_txs(workspace: str = "", purpose: str = "") -> list[record]"#,
        use_when: "Use in Gateway mode to discover open transactions, especially retained checkpoint-run branches.",
        examples: &[r#"debug_branches = env_txs(purpose="checkpoint-run")"#],
        avoid: &["Do not assume retained branches are gone until they disappear from this list."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_finish",
        signature: "env_finish() -> record",
        use_when: "Use before the final answer in Gateway mode to verify the transaction is clean or already closed.",
        examples: &[r#"finish = env_finish()"#, r#"if not finish.ok:
    emit(finish.next_actions)"#],
        avoid: &["Do not leave a dirty transaction unresolved; commit intended changes or restore/rollback unwanted changes."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_restore",
        signature: "env_restore(paths: list[str] | str = []) -> record",
        use_when: "Use in Gateway mode to discard unwanted uncommitted changes for specific paths, or all changes when no paths are passed.",
        examples: &[r#"env_restore(["tmp.log", "build/"])"#, r#"env_restore()"#],
        avoid: &["Do not restore intended task outputs; commit them after review instead."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_checkpoint",
        signature: r#"env_checkpoint(reason: str = "") -> record"#,
        use_when: "Use in Gateway mode to save the current transaction state before a risky repair, verifier attempt, or alternate branch.",
        examples: &[r#"cp = env_checkpoint(reason="before verifier")"#],
        avoid: &["Do not use checkpoints as commits; commit intended final changes as a generation."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_fork",
        signature: "env_fork(checkpoint: str) -> record",
        use_when: "Use in Gateway mode to open an independent transaction branch from a checkpoint.",
        examples: &[r#"branch = env_fork(cp.checkpoint)"#],
        avoid: &["Do not assume the current transaction changes after forking; env_fork returns a new tx id."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_restore_checkpoint",
        signature: "env_restore_checkpoint(checkpoint: str) -> record",
        use_when: "Use in Gateway mode to restore the current transaction back to a named checkpoint state.",
        examples: &[r#"env_restore_checkpoint(cp.checkpoint)"#],
        avoid: &["Do not restore to a checkpoint while expecting long-running commands in the same tx to keep running."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_checkpoints",
        signature: r#"env_checkpoints(workspace: str = "", include_discarded: bool = False) -> list[record]"#,
        use_when: "Use in Gateway mode to inspect active checkpoint branches and storage metrics.",
        examples: &[r#"checkpoints = env_checkpoints()"#],
        avoid: &["Do not parse checkpoint directories directly; use the structured list."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_checkpoint_gc",
        signature: "env_checkpoint_gc(apply: bool = False) -> record",
        use_when: "Use in Gateway mode to inspect checkpoint storage reachability, or remove reclaimable orphan payloads only when apply is true.",
        examples: &[r#"gc = env_checkpoint_gc()"#, r#"env_checkpoint_gc(apply=True)"#],
        avoid: &["Do not pass apply=True without reviewing the dry-run entries first."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_discard_checkpoint",
        signature: "env_discard_checkpoint(checkpoint: str, force: bool = False) -> record",
        use_when: "Use in Gateway mode to discard an unneeded checkpoint branch.",
        examples: &[r#"env_discard_checkpoint(cp.checkpoint)"#],
        avoid: &["Do not discard a checkpoint that may still be needed as a parent unless force is intentional."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_run_checkpoint",
        signature: r#"env_run_checkpoint(checkpoint: str, image: str, argv: list[str], workspace_mount: str = "/app", workdir: str = "/app", timeout_ms: int = 300000, env: record? = None, stdin: str = "", user: str = "", keep_tx: bool = False) -> record"#,
        use_when: "Use in Gateway mode to fork a checkpoint into a branch, run a Linux command there, inspect output and diff, and roll the branch back unless keep_tx is true.",
        examples: &[
            r#"result = env_run_checkpoint(cp.checkpoint, "python:3.12-slim", ["python", "-c", "print('ok')"])"#,
            r#"debug = env_run_checkpoint(cp.checkpoint, "python:3.12-slim", ["pytest", "-q"], keep_tx=True)"#,
        ],
        avoid: &[
            "Do not use retained branches as commits; commit intended final changes explicitly.",
            "Do not use hidden benchmark verifier output as stock pass@1 feedback.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_commit",
        signature: r#"env_commit(message: str = "agent commit", allow_risky: bool = False) -> record"#,
        use_when: "Use in Gateway mode to publish intended transaction changes as a new immutable generation. In blind Gateway agent surface mode, Waymark authorizes intended commits because change inspection is hidden.",
        examples: &[r#"env_commit(message="solve task")"#],
        avoid: &["Do not pass allow_risky=True in full surface mode unless you reviewed warnings for deletes, binary changes, or risky paths."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_rollback",
        signature: "env_rollback() -> record",
        use_when: "Use in Gateway mode to discard and close the whole transaction when the attempted work should not be kept.",
        examples: &[r#"env_rollback()"#],
        avoid: &["Do not rollback after producing the intended answer; use env_commit instead."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "last_result",
        signature: "last_result() -> record | None",
        use_when: "Use to recover the previous Waymark command response after the caller's conversation context dropped it.",
        examples: &[r#"previous = last_result()"#, r#"emit(last_result())"#],
        avoid: &["Do not use as long-term storage; it only tracks the immediately previous command response."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "start_daemon",
        signature: "start_daemon(argv: list[str], cwd: str? = None, env: record? = None, stdout: str? = None, stderr: str? = None) -> record",
        use_when: "Use for servers and background services that must still be running when tests execute.",
        examples: &[
            r#"daemon = start_daemon(["sh", "-c", "sleep 0.1"], cwd="/app", stderr="server.err")"#,
            r#"ready = wait_port(9, timeout_ms=1)"#,
            r#"status = daemon_status(daemon, log="server.err")"#,
        ],
        avoid: &[
            "Use run() instead for commands expected to finish.",
            "After starting a daemon, call wait_port() or daemon_status() before assuming it is ready.",
            "Keep stdout/stderr paths when startup logs may explain failures.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "daemon_status",
        signature: "daemon_status(daemon: record | int, port: int? = None, host: str = \"127.0.0.1\", log: str? = None, max_log_bytes: int = 4000) -> record",
        use_when: "Use to check whether a daemon is still alive, whether an expected TCP port is open, and to include recent logs.",
        examples: &[r#"status = daemon_status(daemon)"#],
        avoid: &["Do not treat a spawn result as ready until daemon_status() or wait_port() confirms it."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "stop_daemon",
        signature: "stop_daemon(daemon: record | int, timeout_ms: int = 5000) -> record",
        use_when: "Use to cleanly stop a daemon started by start_daemon().",
        examples: &[r#"stop = stop_daemon(daemon, timeout_ms=2000)"#],
        avoid: &["Do not use for normal foreground commands; run() already waits and cleans up timed-out children."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "wait_port",
        signature: "wait_port(port: int, host: str = \"127.0.0.1\", timeout_ms: int = 30000, protocol: str = \"tcp\") -> record",
        use_when: "Use after start_daemon() when service readiness is represented by a TCP port accepting connections or a UDP endpoint accepting datagrams.",
        examples: &[
            r#"ready = wait_port(9, host="127.0.0.1", timeout_ms=1)"#,
            r#"udp_ready = wait_port(9, protocol="udp", timeout_ms=1)"#,
        ],
        avoid: &[
            "If wait_port() times out, call daemon_status() with a log path before retrying blindly.",
            "UDP has no connection handshake; protocol=\"udp\" only verifies that Stone can send a datagram to the endpoint.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "wait_for",
        signature: "wait_for(predicate: lambda, timeout_ms: int = 30000, interval_ms: int = 100, ignore_errors: bool = False) -> record",
        use_when: "Use after start_daemon() or asynchronous setup when readiness is represented by an arbitrary Stone predicate, such as log text, file contents, or structured status.",
        examples: &[
            r#"ready = wait_for(lambda: True, timeout_ms=1000)"#,
            r#"ready = wait_for(lambda: read_file("missing.log").find("READY") >= 0, timeout_ms=10, interval_ms=5, ignore_errors=True)"#,
        ],
        avoid: &[
            "Use wait_port() instead when readiness is just a TCP or UDP port probe.",
            "Keep ignore_errors=False unless transient predicate errors are expected, such as a log file that may not exist yet.",
        ],
        aliases: &[],
    },
];

const STONE_HELP_TOPICS: &[StoneHelpTopic] = &[
    StoneHelpTopic {
        name: "staged_workflow",
        summary: "Author a sequential evidence-gated task harness with optional one-decision agent control inside each bounded stage.",
        bullets: &[
            "Write `workflow name:` at top level, indent `stage name(goal=..., max_actions=..., checkpoint=...):` by four spaces, place direct `ensure <typed evidence>` contracts in the stage body, and execute with `run name` or workflow_run(name).",
            "A deterministic stage may contain ordinary Stone effects. `agent_loop()` must be its only executable action and resolves to a visible `def agent_loop(step)` callback, such as examples/scripts/standard_stage_agent.stone.",
            "The kernel calls that callback once per decision, checks every ensure contract after each returned action, and stops after max_actions; the callback receives goal, current missing evidence, previous outcome, and completed stages in step.",
            "A finish claim is only another returned action. It cannot advance a stage while evidence remains missing.",
            "A requested checkpoint is created only after fresh satisfied evidence. Later stages may therefore resume or branch from a verified semantic frontier.",
            "Workflow blocks lower to the same typed workflow_stage/workflow/workflow_run kernel as @stage declarations; there is no second workflow engine or author-facing DAG.",
        ],
    },
    StoneHelpTopic {
        name: "agent_control",
        summary: "Write an ad-hoc or reusable agent as an ordinary Stone function over one structured session; optimized builtins follow the same control contract.",
        bullets: &[
            "Define def control(session): ... and invoke it with control(agent_session()); return the structured result and emit it at the program boundary.",
            "session.task and session.input are structured task data; session.attempt is the current attempt record; session.limits exposes admitted resource limits.",
            "session.tools lists discoverable Shell tool families, while the actual operations remain ordinary Stone builtins such as model_call, run, read_file, and attempt_fork.",
            "A tool name is not authority. The attached attempt's Gateway capabilities mediate protected file, Linux, model, context, and attempt effects.",
            "Compose controls with ordinary Stone functions: named def functions and lambdas are first-class callable values, so a control may accept replaceable dispatch, verification, progress, or fallback adapters.",
            "Use a child attempt when work needs independent workspace/context state, fate, authority, budget, or candidate acceptance; a model call alone is not a child attempt.",
            "The builtin ReAct controller is an optimized AgentControl implementation, not a privileged Gateway mode or Stone language semantic.",
        ],
    },
    StoneHelpTopic {
        name: "attempt_workflow",
        summary: "Branch, inspect, select, and clean candidate attempts while keeping Stone as the control language.",
        bullets: &[
            "Get the current root with root = attempt_info(); inspect cheap structured state with attempt_state(root.attempt).",
            "Create scope = attempt_scope() before branching. Pass scope=scope to every attempt_fork or attempt_spawn so unresolved children are automatically cancel-then-join cleaned at evaluation exit.",
            "Fork candidates only while the parent is stable. Prefer one definition-only module and named entrypoints: child = attempt_fork(root.attempt, program=current_program(), entrypoint=\"worker\", start=True, scope=scope).",
            "Each child receives its own scoped LibOS channel. Use attempt_join(child) for one child, attempt_wait_any(scope) for a race, or attempt_wait_all(scope) for a barrier; these are Gateway waits, not sequential Stone polling.",
            "After attempt_join, compare the compact outcome.result.value reports. Use attempt_state for workspace state and bounded attempt_inspect only when the summary is insufficient; an attached parent cannot report on the child's behalf.",
            "attempt_inspect remains available after accept/discard: archived report details and trace survive, while resource_state confirms whether controller/process/transaction resources were reclaimed.",
            "Select exactly one useful child with attempt_accept(root.attempt, child.attempt); close every rejected child with attempt_discard(child.attempt, reason=\"...\").",
            "Call attempt_scope_close(scope) and require clean=True. It is also called automatically on normal or exceptional Stone evaluation exit.",
            "After selection, verify the imported files in the parent. A built-in model agent should return finish; its outer controller reports and publishes the root.",
            "Before final, use attempts(state=\"active\") and ensure no child attempts remain active. Never use compatibility attempt_finish(commit) for candidate selection.",
        ],
    },
    StoneHelpTopic {
        name: "workflow",
        summary: "Recommended LLM workflow for solving tasks in Stone.",
        bullets: &[
            "Use help() first, then help(\"name\") for a primitive before guessing syntax.",
            "In long-lived task-server and MCP sessions, top-level value and function bindings persist across eval calls; bind intermediate data once and reuse names later.",
            "Stone eval source can be a multi-line script like python -c or bash -c; use assignments, loops, helpers, and emit(value) when a structured return is useful.",
            "For large values, bind them by name and emit compact summaries such as {\"count\": len(rows), \"sample\": head(rows, 5)}; force full output only when necessary.",
            "Use /app paths for task inputs and write exactly the requested output files.",
            "Use stone/data primitives for file, CSV, JSON, JSONL, text, and sorting work.",
            "Use small probes with limit=5 or bounded reads before writing a large final script.",
            "Finish by reading/describing the output file to verify it exists and has the right shape.",
        ],
    },
    StoneHelpTopic {
        name: "session",
        summary: "Long-lived eval session behavior for agents.",
        bullets: &[
            "One-shot CLI evals are fresh, but task-server stream and MCP warm evals behave like a real shell session.",
            "Top-level value and function bindings persist across eval calls; this is live name binding, not a JSON result cache.",
            "Assignment-only evals return null and compact session diagnostics such as bound names instead of echoing large bound values.",
            "Prefer rows = read_csv(...), inspect rows, then reuse rows in later eval calls instead of rereading the file.",
            "Avoid emitting entire large lists; use head()/tail()/first()/last() samples unless the caller explicitly requests full output.",
            "Open file handles do not persist across eval calls; persist paths, text, records, lists, and functions instead.",
        ],
    },
    StoneHelpTopic {
        name: "syntax",
        summary: "Python-like syntax subset that Stone accepts.",
        bullets: &[
            "Assignments: name = value; counters[key] += 1 works after initialization.",
            "Blocks: if/elif/else, for, while, break, continue, pass use indentation.",
            "Values: lists, tuple literals as list values, records/dicts, slices, indexing, item assignment, True, False, None.",
            "Record fields can be read as row[\"name\"] or row.name when the field name is identifier-shaped.",
            "Operators: +, -, *, /, //, &, |, <<, >>, comparisons, and/or/not, membership, is None.",
            "Conditional expressions use Python's value if condition else fallback shape.",
            "Functions: def name(arg) works; omitted parameter and return annotations mean Any, optional annotations like def name(arg: str) -> str are checked, and immutable default values are supported. Attempt controls also support attempt_handle, attempt_outcome, attempt_scope, semantic_frontier, and attempt_acceptance annotations. Named functions are first-class callable values and may be passed to another function. Use -> None for a checked procedure. @stage(...) is the one supported declaration decorator.",
            "try/except catches runtime evaluation errors; supported handlers are except:, except Exception:, and except Exception as e:.",
            "with follows Python block visibility: targets and body assignments remain visible after exit. For files, attempt_scope, and semantic_frontier values it performs checked cleanup on fallthrough, return, or error. Nominal lifecycle methods include scope.branch/wait_any/wait_all, child.wait/inspect/discard, root.accept, and frontier.release.",
            "Lambdas: expression-only callbacks work in sort/map/filter, e.g. lambda r: r[\"name\"].",
            "String methods include strip/lstrip/rstrip, isdigit/isalpha/isalnum, count, split/rsplit/splitlines, replace, join, lower/upper, zfill, startswith, and endswith; split and rsplit accept optional maxsplit and default whitespace splitting.",
            "File handles support read(), readlines()/splitlines(), write(text), and close().",
            "List variables support append(value), extend(values), count(value), mutating sort(key=..., reverse=...), and set-style add(value) for unique append.",
            "Use emit(value) when you want structured data returned to the caller.",
        ],
    },
    StoneHelpTopic {
        name: "unsupported",
        summary: "Common Python habits that fail in Stone, with replacements.",
        bullets: &[
            "No imports/modules/os/pathlib/glob/json; use find/read_json/json_loads/json_dumps.",
            "No isinstance(value, type); use type(value) == \"list\"/\"str\"/\"int\"/\"float\"/\"record\" or direct structural checks.",
            "Lambda is expression-only; use explicit loops when callback logic needs statements or mutation.",
            "No classes, async, nested functions, or general Python decorators; only Stone's @stage(...) declaration decorator is supported.",
            "No mutable default args, *args, **kwargs, or keyword calls to user functions.",
            "No try/finally, try/else, except*, or exception classes other than Exception.",
            "Method keyword arguments are intentionally narrow: split(maxsplit=...) and sort(key=..., reverse=...) are supported; most other methods take positional arguments only.",
            "No automatic string-number coercion; use int(), float(), and str().",
            "No missing-key arithmetic; initialize dictionary counters before incrementing.",
        ],
    },
    StoneHelpTopic {
        name: "counters",
        summary: "Safe dictionary counter pattern.",
        bullets: &[
            "if key in counts:",
            "    counts[key] += 1",
            "else:",
            "    counts[key] = 1",
        ],
    },
];

#[cfg(test)]
pub(crate) fn stone_help_documented_names_for_tests() -> std::collections::BTreeSet<&'static str> {
    let mut names = std::collections::BTreeSet::new();
    for entry in STONE_HELP_ENTRIES {
        names.insert(entry.name);
        for alias in entry.aliases {
            names.insert(*alias);
        }
    }
    names
}

#[cfg(test)]
pub(crate) fn stone_help_entries_without_examples_for_tests() -> Vec<&'static str> {
    STONE_HELP_ENTRIES
        .iter()
        .filter(|entry| entry.examples.is_empty())
        .map(|entry| entry.name)
        .collect()
}

pub(crate) fn stone_help_overview(span: Span) -> Value {
    let mut record = Record::with_capacity(7);
    record.push("language", Value::string("Stone", span));
    record.push(
        "for_llm",
        Value::string("This help is written for LLM agents generating Stone. Stone eval source can be a multi-line script like python -c or bash -c. In MCP/task-server stream mode, top-level value and function bindings persist across eval calls; reuse named intermediates instead of rereading files. For large values, return len/head/tail summaries unless full output is explicitly required. Prefer these primitives and examples over guessing Python APIs.", span),
    );
    record.push("workflow", topic_bullets("workflow", span));
    record.push(
        "topics",
        Value::list(
            STONE_HELP_TOPICS
                .iter()
                .map(|topic| Value::string(topic.name, span))
                .collect(),
            span,
        ),
    );
    record.push(
        "builtins",
        Value::list(
            STONE_HELP_ENTRIES
                .iter()
                .map(|entry| {
                    let mut item = Record::with_capacity(3);
                    item.push("name", Value::string(entry.name, span));
                    item.push("signature", Value::string(entry.signature, span));
                    item.push("use_when", Value::string(entry.use_when, span));
                    Value::record(item, span)
                })
                .collect(),
            span,
        ),
    );
    record.push("syntax", topic_bullets("syntax", span));
    record.push("unsupported", topic_bullets("unsupported", span));
    record.push(
        "examples",
        string_list(
            &[
                r#"rows = read_csv("/app/input.csv")"#,
                r#"files = find("/app", "*.jsonl")"#,
                r#"write_file("/app/out.txt", "done\n")"#,
                r#"write_json("/app/out.json", {"ok": True})"#,
            ],
            span,
        ),
    );
    Value::record(record, span)
}

pub(crate) fn stone_help_topic(name: &str, span: Span) -> Value {
    let normalized = match name {
        "read_text" => "read_file",
        "write_text" => "write_file",
        "edit_file" => "edit",
        "list_dir" => "ls",
        "head" => "first",
        "tail" => "last",
        "from_json" => "json_loads",
        "to_json" => "json_dumps",
        "env_diff" => "env_state",
        "attempt_list" => "attempts",
        "sys" | "sys_info" => "sysinfo",
        "sorted" => "sort",
        "gotchas" | "constraints" => "unsupported",
        other => other,
    };
    if let Some(entry) = STONE_HELP_ENTRIES
        .iter()
        .find(|entry| entry.name == normalized)
    {
        let mut record = Record::with_capacity(7);
        record.push("name", Value::string(entry.name, span));
        record.push("signature", Value::string(entry.signature, span));
        record.push("use_when", Value::string(entry.use_when, span));
        record.push("examples", string_list(entry.examples, span));
        record.push("avoid", string_list(entry.avoid, span));
        record.push("aliases", string_list(entry.aliases, span));
        record.push("found", Value::bool(true, span));
        Value::record(record, span)
    } else if let Some(topic) = STONE_HELP_TOPICS
        .iter()
        .find(|topic| topic.name == normalized)
    {
        let mut record = Record::with_capacity(4);
        record.push("name", Value::string(topic.name, span));
        record.push("summary", Value::string(topic.summary, span));
        record.push("bullets", string_list(topic.bullets, span));
        record.push("found", Value::bool(true, span));
        Value::record(record, span)
    } else {
        let mut record = Record::with_capacity(4);
        record.push("name", Value::string(name, span));
        record.push("found", Value::bool(false, span));
        record.push(
            "message",
            Value::string(
                "No detailed Stone help for this topic. Use help() for the available surface.",
                span,
            ),
        );
        record.push(
            "available",
            Value::list(
                STONE_HELP_ENTRIES
                    .iter()
                    .map(|entry| Value::string(entry.name, span))
                    .collect(),
                span,
            ),
        );
        Value::record(record, span)
    }
}

fn topic_bullets(name: &str, span: Span) -> Value {
    STONE_HELP_TOPICS
        .iter()
        .find(|topic| topic.name == name)
        .map(|topic| string_list(topic.bullets, span))
        .unwrap_or_else(|| Value::list(Vec::new(), span))
}

fn string_list(items: &[&str], span: Span) -> Value {
    Value::list(
        items
            .iter()
            .map(|item| Value::string(*item, span))
            .collect(),
        span,
    )
}
