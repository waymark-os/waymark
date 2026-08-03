#!/usr/bin/env python3
"""Compare code-like Stone surfaces for authoring a staged task harness."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

import eval_stone_agent_authorship as base
import eval_stone_stage_syntax_ab as stage_ab


ROOT = Path(__file__).resolve().parents[2]
ARMS = ("decorator", "block", "contract")
TASK = """I have provided /app/doomgeneric/, the source code to doom. I've also
wrote a special doomgeneric_img.c that I want you to use which will write each
drawn frame to /tmp/frame.bmp. I've finally provided vm.js that will expect a
file called doomgeneric_mips and will run it. Please figure out the rest and
build the doomgeneric_mips ELF for me, so that I can run `node vm.js`. After
running `node vm.js` I expect that stdout will be printed appropriately, and
frames will be written to the file system."""
STANDARD_AGENT_LIBRARY = ROOT / "examples/scripts/standard_stage_agent.stone"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-5.6-terra")
    parser.add_argument("--reasoning-effort", default="low")
    parser.add_argument("--trials", type=int, default=1)
    parser.add_argument("--arm", action="append", choices=ARMS, dest="arms")
    parser.add_argument("--codex", default="codex")
    parser.add_argument(
        "--waymark-bin", type=Path, default=ROOT / "target/debug/waymark"
    )
    parser.add_argument(
        "--run-root",
        type=Path,
        default=ROOT / "target/runs/staged-harness-syntax-authorship-v1",
    )
    parser.add_argument("--timeout", type=float, default=600.0)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument("--reuse-existing", action="store_true")
    parser.add_argument(
        "--finding-evidence-predicates",
        action="store_true",
        help="Require resolved finding descriptors to use executable evidence predicates.",
    )
    parser.add_argument(
        "--stage-inputs",
        action="store_true",
        help="Require later stages to declare and consume inspection finding outputs.",
    )
    parser.add_argument(
        "--typed-stage-input-fields",
        action="store_true",
        help="Require stage inputs to select producer fields with matching kinds.",
    )
    parser.add_argument(
        "--relational-startup-finding",
        action="store_true",
        help="Require a startup_strategy finding that relates backend, VM, and CRT entry semantics.",
    )
    return parser.parse_args()


def common_prompt(
    require_finding_evidence: bool = False,
    require_stage_inputs: bool = False,
    require_typed_stage_inputs: bool = False,
    require_relational_startup_finding: bool = False,
) -> str:
    predicate_guidance = ""
    if require_finding_evidence:
        predicate_guidance = """
For every resolved inspection finding, use a descriptor with an executable
`evidence` predicate. The predicate supports exactly:
- `kind`: one of text, path, file, or command
- `tool`: one of read, find, search, run_linux, or run_complete
- `contains`: a string or list of required literal substrings
- `success`: a boolean required tool outcome

Example:
```
agent_loop()
ensure decision_recorded(resolved={
    "runtime_contract": {
        "kind": "command",
        "question": "How is the ELF loaded?",
        "evidence": {
            "tool": "read",
            "contains": ["entryPoint"],
            "success": True,
        },
    },
})
```

Use predicates for source layout, exact toolchain, frame backend dependencies,
and VM runtime/ABI requirements. They are evidence-acquisition obligations, not
claims that the fact is already known. Put descriptors only in
`decision_recorded(resolved={...})`; `agent_loop()` takes no arguments.
"""
    stage_input_guidance = ""
    if require_typed_stage_inputs:
        stage_input_guidance = """
When a later stage consumes findings declared by an earlier decision stage,
declare a field-typed input record in the later stage header. For example:
```
inputs={"inspect": {
    "source_layout": "path",
    "toolchain": "command",
}}
```
Every selected field must appear in the producer's
`decision_recorded(resolved={...})` contract and its kind must match exactly.
Select only fields the consumer needs. The runtime statically checks this
interface and exposes the selected findings under
`step.stage_outputs.inspect.findings`, preserving state, value, and basis. Do
not use the list form of inputs for this experiment.
"""
    elif require_stage_inputs:
        stage_input_guidance = """
When a later stage consumes findings declared by an earlier decision stage,
make that dataflow explicit with `inputs=["earlier_stage_name"]` in the later
stage header. The runtime projects only those completed stages into
`step.stage_outputs`, preserving each finding's kind, state, value, and basis.
For example, a stage declared with `inputs=["prepare"]` receives
`step.stage_outputs.prepare.findings`. Do not rely on generic memory retrieval
to rediscover a declared stage interface, and do not reference the current or
a later stage.
"""
    relational_finding_guidance = ""
    if require_relational_startup_finding:
        relational_finding_guidance = """
Inspection must resolve a separate field named `startup_strategy` with kind
`command`. Its question must ask for one concrete source/compiler/linker
strategy that reconciles all three observed constraints: the selected frame
backend's `__start`, vm.js selecting a function symbol named `main`, and the
compiler driver's CRT/startup-object policy. Give this field an executable
evidence predicate that requires observations containing `__start`, `main`,
and a CRT/startup-object marker. The build stage must consume
`startup_strategy` through its field-typed input record. Do not put a guessed
solution in the descriptor; the inspection agent must acquire and synthesize
it from the workspace and toolchain.
"""
    return f"""You are evaluating an LLM-oriented programming-language surface.
Write one complete Stone harness specification for the public task below. Do
not call tools, inspect files, solve the task, or return prose. Return only the
required JSON object containing `source`.

Stone is Python-shaped. A workflow is an ordered executable specification, not
a JSON plan or a DAG declaration. Each stage has a stable name, concise goal,
runtime evidence gate, and positive max_actions budget. A stage body may call
agent_loop() when model-guided action/state iteration is useful, or use ordinary
Stone for a deterministic operation. Later stages cannot start until current
evidence is satisfied. Use checkpoint="repairable" when an expensive successful
stage should be reusable during later repair. Every public task obligation must
be covered by some stage evidence. Prefer three to six purposeful stages and
leave repair budget after the first real execution attempt.

The admitted task workspace root is the Stone program's current directory and
is mounted at /app for Linux commands. A relative path such as
"doomgeneric_mips" therefore names /app/doomgeneric_mips. Do not place a
requested root output under a source subdirectory merely because inputs live
there. /tmp paths name the retained Linux tool environment rather than the
transactional workspace.

Available evidence expressions for this syntax experiment:
- artifact(path, format=None, arch=None)
- command_succeeded(argv)
- stdout_nonempty()
- decision_recorded(fields=[]) or decision_recorded(resolved=[])
- file_valid(path, format=None, nonempty=True)
- all_evidence(first, ...)

The surface lowers to Stone's existing typed workflow/stage IR. Do not define
these builtins yourself. A standard visible Stone library supplies the
one-decision agent_loop(step) callback; do not regenerate that library.
When an inspection or planning stage is meant to produce a decision rather
than a file, gate it with decision_recorded(). If downstream execution depends
on concrete facts, name them with resolved=["fact_name", ...]; the standard
agent's decide action must then record state, value, and an observation basis
for every field. An unknown finding keeps the stage open. Use fields= only when
a non-empty structural finding is sufficient.
Existing task inputs and generic prose are not evidence that planning happened.
{predicate_guidance}
{stage_input_guidance}
{relational_finding_guidance}

Public task:
{TASK}
"""


def arm_prompt(
    arm: str,
    require_finding_evidence: bool = False,
    require_stage_inputs: bool = False,
    require_typed_stage_inputs: bool = False,
    require_relational_startup_finding: bool = False,
) -> str:
    if arm == "decorator":
        interface = r'''
Use only this declaration form:

@stage(
    goal="produce the packaged artifact",
    evidence=artifact("/app/output.tar"),
    max_actions=6,
    checkpoint="repairable",
)
def package(step):
    return agent_loop(step)

Define every stage at top level, then execute the ordered workflow with:
report = workflow_run(workflow("project", prepare, package, verify))
emit(report)

The decorated function name is the stage value. Do not use `workflow name:` or
`stage name(...):` blocks.
'''
    elif arm == "block":
        interface = r'''
Use only this block declaration form:

workflow project:
    stage package(
        goal="produce the packaged artifact",
        evidence=artifact("/app/output.tar"),
        max_actions=6,
        checkpoint="repairable",
    ):
        agent_loop()

    stage verify(
        goal="verify the final artifact",
        evidence=file_valid("/app/output.tar", nonempty=True),
        max_actions=2,
    ):
        verify_outputs()

run project

Do not use decorators, stage functions, workflow(...), or workflow_run(...).
'''
    elif arm == "contract":
        interface = r'''
Use only this contract-block declaration form:

workflow project:
    stage package(
        goal="produce the packaged artifact",
        max_actions=6,
        checkpoint="repairable",
    ):
        agent_loop()
        ensure artifact("/app/output.tar", format="tar")

    stage verify(goal="verify the final artifact", max_actions=2):
        verify_outputs()
        ensure file_valid("/app/output.tar", nonempty=True)

run project

Every stage must contain one or more `ensure expression` postconditions. The
runtime checks them before execution when possible and after each action. For
command output use a separate `ensure stdout_nonempty()` rather than mentioning
stdout only in a goal. Do not use evidence=, decorators, stage functions,
workflow(...), or workflow_run(...).
'''
    else:
        raise ValueError(f"unknown arm: {arm}")
    return (
        common_prompt(
            require_finding_evidence,
            require_stage_inputs,
            require_typed_stage_inputs,
            require_relational_startup_finding,
        )
        + "\nSyntax arm:\n"
        + interface
    )


def output_schema() -> dict[str, Any]:
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"source": {"type": "string", "minLength": 1}},
        "required": ["source"],
        "additionalProperties": False,
    }


def codex_command(
    args: argparse.Namespace, run_dir: Path, prompt: str
) -> list[str]:
    return [
        args.codex,
        "exec",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--json",
        "--model",
        args.model,
        "--config",
        f'model_reasoning_effort="{args.reasoning_effort}"',
        "--cd",
        str(run_dir),
        "--output-schema",
        str(run_dir / "output-schema.json"),
        "--output-last-message",
        str(run_dir / "last-message.json"),
        prompt,
    ]


def stage_names(arm: str, source: str) -> list[str]:
    if arm == "decorator":
        return re.findall(r"(?m)^def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(step\)\s*:", source)
    return re.findall(
        r"(?m)^\s+stage\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", source
    )


def source_features(arm: str, source: str) -> dict[str, Any]:
    names = stage_names(arm, source)
    decision_stage_names = [
        name
        for name in names
        if any(token in name.lower() for token in ("inspect", "plan", "select", "decide"))
    ]
    list_stage_input_references = [
        reference
        for body in re.findall(r"\binputs\s*=\s*\[([^\]]*)\]", source)
        for reference in re.findall(r"['\"]([A-Za-z_][A-Za-z0-9_]*)['\"]", body)
    ]
    typed_stage_input_references = re.findall(
        r"\binputs\s*=\s*\{\s*['\"]([A-Za-z_][A-Za-z0-9_]*)['\"]\s*:",
        source,
    )
    stage_input_references = [
        *list_stage_input_references,
        *typed_stage_input_references,
    ]
    lowered = source.lower()
    resolved_descriptors = resolved_descriptor_source(source)
    startup_strategy_descriptor = named_record_descriptor_source(
        resolved_descriptors, "startup_strategy"
    )
    startup_strategy_evidence = named_record_descriptor_source(
        startup_strategy_descriptor, "evidence"
    )
    finding_kinds = re.findall(
        r"(?:['\"]kind['\"]|\bkind)\s*:\s*['\"]([^'\"]+)['\"]",
        resolved_descriptors,
    )
    build_index = next(
        (index for index, name in enumerate(names) if "build" in name.lower()),
        None,
    )
    return {
        "stage_names": names,
        "stage_count": len(names),
        "goal_count": len(re.findall(r"\bgoal\s*=", source)),
        "evidence_count": len(re.findall(r"\bevidence\s*=", source)),
        "max_actions_count": len(re.findall(r"\bmax_actions\s*=", source)),
        "agent_loop_count": source.count("agent_loop("),
        "ensure_count": len(re.findall(r"(?m)^\s+ensure\s+", source)),
        "repairable_checkpoint": 'checkpoint="repairable"' in source
        or "checkpoint='repairable'" in source,
        "artifact_path": bool(
            re.search(
                r"\bartifact\(\s*(['\"])(?:/app/)?doomgeneric_mips\1",
                source,
            )
        ),
        "vm_command": bool(
            re.search(
                r"(?:['\"]node['\"]\s*,\s*['\"](?:/app/)?vm\.js['\"]|node\s+(?:\.\./|/app/)?vm\.js)",
                source,
            )
        ),
        "stdout_evidence": bool(
            re.search(
                r"\bstdout\s*=\s*(['\"])(?:(?!\1).)+\1",
                source,
            )
        )
        or "stdout_nonempty(" in source,
        "frame_path": "/tmp/frame.bmp" in source,
        "elf_evidence": "elf" in lowered,
        "build_stage_index": build_index,
        "execute_stage": any(
            token in name.lower()
            for name in names
            for token in ("execute", "run", "launch")
        ),
        "verify_stage": any(
            token in name.lower()
            for name in names
            for token in ("verify", "validate", "check")
        ),
        "decision_stage": any(
            token in name.lower()
            for name in names
            for token in ("inspect", "plan", "select", "decide")
        ),
        "stage_inputs_count": len(list_stage_input_references)
        + len(typed_stage_input_references),
        "field_typed_stage_inputs_count": len(typed_stage_input_references),
        "stage_input_references": stage_input_references,
        "typed_stage_handoff": any(
            reference in decision_stage_names
            for reference in stage_input_references
        ),
        "field_typed_stage_handoff": any(
            reference in decision_stage_names
            for reference in typed_stage_input_references
        ),
        "decision_evidence": "decision_recorded(" in source,
        "typed_decision_fields": bool(
            re.search(r"decision_recorded\s*\([^)]*\b(?:fields|resolved)\s*=", source)
        ),
        "resolved_decision_fields": bool(
            re.search(r"decision_recorded\s*\([^)]*\bresolved\s*=", source)
        ),
        "finding_evidence_count": len(
            re.findall(
                r"(?:['\"]evidence['\"]|\bevidence)\s*:",
                resolved_descriptors,
            )
        ),
        "finding_evidence_tool": bool(
            re.search(
                r"(?:['\"]tool['\"]|\btool)\s*:", resolved_descriptors
            )
        ),
        "finding_evidence_contains": bool(
            re.search(
                r"(?:['\"]contains['\"]|\bcontains)\s*:",
                resolved_descriptors,
            )
        ),
        "finding_evidence_success": bool(
            re.search(
                r"(?:['\"]success['\"]|\bsuccess)\s*:",
                resolved_descriptors,
            )
        ),
        "startup_strategy_finding": bool(startup_strategy_descriptor),
        "startup_strategy_command_kind": bool(
            re.search(
                r"(?:['\"]kind['\"]|\bkind)\s*:\s*['\"]command['\"]",
                startup_strategy_descriptor,
            )
        ),
        "startup_strategy_relational": (
            "__start" in startup_strategy_descriptor
            and bool(re.search(r"\bmain\b", startup_strategy_descriptor))
            and bool(
                re.search(
                    r"\bcrt\b|startup[-_ ]?object",
                    startup_strategy_descriptor,
                    re.IGNORECASE,
                )
            )
        ),
        "startup_strategy_evidence_markers": (
            "__start" in startup_strategy_evidence
            and bool(re.search(r"\bmain\b", startup_strategy_evidence))
            and "crt" in startup_strategy_evidence.lower()
        ),
        "startup_strategy_typed_input": bool(
            re.search(
                r"\binputs\s*=\s*\{.*?['\"]startup_strategy['\"]\s*:\s*['\"]command['\"]",
                source,
                re.DOTALL,
            )
        ),
        "finding_kinds": finding_kinds,
        "invalid_finding_kinds": [
            kind
            for kind in finding_kinds
            if kind not in ("text", "path", "file", "command")
        ],
        "decorator_count": source.count("@stage("),
        "workflow_call": "workflow(" in source and "workflow_run(" in source,
        "workflow_block": bool(
            re.search(r"(?m)^workflow\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*$", source)
        ),
        "block_run": bool(
            re.search(r"(?m)^run\s+[A-Za-z_][A-Za-z0-9_]*\s*$", source)
        ),
        "lines": len(source.splitlines()),
        "bytes": len(source.encode("utf-8")),
    }


def resolved_descriptor_source(source: str) -> str:
    match = re.search(
        r"decision_recorded\s*\(\s*resolved\s*=\s*\{", source
    )
    if match is None:
        return ""
    start = match.end() - 1
    depth = 0
    quote: str | None = None
    escaped = False
    for index in range(start, len(source)):
        char = source[index]
        if quote is not None:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if char in ("'", '"'):
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    return ""


def named_record_descriptor_source(mapping: str, field: str) -> str:
    match = re.search(
        rf"(?:['\"]{re.escape(field)}['\"]|\b{re.escape(field)}\b)\s*:\s*\{{",
        mapping,
    )
    if match is None:
        return ""
    start = match.end() - 1
    depth = 0
    quote: str | None = None
    escaped = False
    for index in range(start, len(mapping)):
        char = mapping[index]
        if quote is not None:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if char in ("'", '"'):
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return mapping[start : index + 1]
    return ""


def syntax_gate(arm: str, features: dict[str, Any]) -> list[str]:
    violations: list[str] = []
    count = int(features["stage_count"])
    if count < 3 or count > 6:
        violations.append(f"stage_count={count}, expected 3..6")
    required_counts = (
        ("goal_count", "ensure_count", "max_actions_count")
        if arm == "contract"
        else ("goal_count", "evidence_count", "max_actions_count")
    )
    for field in required_counts:
        if int(features[field]) < count:
            violations.append(f"{field}={features[field]}, expected at least {count}")
    if int(features["agent_loop_count"]) < 2:
        violations.append("expected agent_loop in at least two stages")
    if arm == "decorator":
        if int(features["decorator_count"]) != count:
            violations.append("every decorator stage must use @stage")
        if not features["workflow_call"]:
            violations.append("missing workflow/workflow_run composition")
        if features["workflow_block"]:
            violations.append("decorator arm used workflow block syntax")
    else:
        if not features["workflow_block"] or not features["block_run"]:
            violations.append("block arm requires workflow block and run declaration")
        if int(features["decorator_count"]) != 0 or features["workflow_call"]:
            violations.append("block arm used decorator/call composition")
        if arm == "contract" and int(features["evidence_count"]) != 0:
            violations.append("contract arm used evidence= instead of ensure")
    return violations


def semantic_gate(
    features: dict[str, Any],
    require_finding_evidence: bool = False,
    require_stage_inputs: bool = False,
    require_typed_stage_inputs: bool = False,
    require_relational_startup_finding: bool = False,
) -> list[str]:
    violations: list[str] = []
    for field in (
        "repairable_checkpoint",
        "artifact_path",
        "vm_command",
        "stdout_evidence",
        "frame_path",
        "elf_evidence",
    ):
        if not features[field]:
            violations.append(f"missing {field}")
    build_index = features["build_stage_index"]
    if build_index is None or build_index > 1:
        violations.append(
            f"build stage index={build_index}, expected first or second stage"
        )
    if features["decision_stage"] and not features["decision_evidence"]:
        violations.append("inspection/planning stage lacks decision_recorded evidence")
    if features["decision_stage"] and not features["typed_decision_fields"]:
        violations.append("inspection/planning decision lacks typed finding fields")
    if features["decision_stage"] and not features["resolved_decision_fields"]:
        violations.append("inspection/planning findings do not require resolved state")
    if require_finding_evidence and features["decision_stage"]:
        if int(features["finding_evidence_count"]) < 4:
            violations.append("expected at least four finding evidence predicates")
        for field in (
            "finding_evidence_tool",
            "finding_evidence_contains",
            "finding_evidence_success",
        ):
            if not features[field]:
                violations.append(f"missing {field}")
        invalid_kinds = features.get("invalid_finding_kinds", [])
        if invalid_kinds:
            violations.append(
                "invalid finding kinds: " + ", ".join(invalid_kinds)
            )
    if require_stage_inputs and features["decision_stage"]:
        if not features["typed_stage_handoff"]:
            violations.append(
                "later stages do not declare an input from the decision stage"
            )
    if require_typed_stage_inputs and features["decision_stage"]:
        if not features["field_typed_stage_handoff"]:
            violations.append(
                "later stages do not declare field-typed inputs from the decision stage"
            )
    if require_relational_startup_finding and features["decision_stage"]:
        for field in (
            "startup_strategy_finding",
            "startup_strategy_command_kind",
            "startup_strategy_relational",
            "startup_strategy_evidence_markers",
            "startup_strategy_typed_input",
        ):
            if not features[field]:
                violations.append(f"missing {field}")
    return violations


def evaluate_once(
    args: argparse.Namespace,
    *,
    arm: str,
    prompt: str,
    run_dir: Path,
) -> dict[str, Any]:
    run_dir.mkdir(parents=True, exist_ok=args.reuse_existing)
    if args.reuse_existing:
        source_path = run_dir / "harness.stone"
        prior_path = run_dir / "summary.json"
        if not source_path.is_file() or not prior_path.is_file():
            raise RuntimeError(f"missing existing authorship cell: {run_dir}")
        prior = json.loads(prior_path.read_text(encoding="utf-8"))
        source = source_path.read_text(encoding="utf-8")
        features = source_features(arm, source)
        syntax_violations = syntax_gate(arm, features)
        admission_ok, admission_error = admit_block_source(
            args, arm, source, run_dir
        )
        if not admission_ok:
            syntax_violations.append(f"Stone admission failed: {admission_error}")
        semantic_violations = semantic_gate(
            features,
            args.finding_evidence_predicates,
            args.stage_inputs,
            args.typed_stage_input_fields,
            args.relational_startup_finding,
        )
        result = {
            **prior,
            "ok": not syntax_violations and not semantic_violations,
            "syntax_ok": not syntax_violations,
            "admission_ok": admission_ok,
            "admission_error": admission_error,
            "semantic_ok": not semantic_violations,
            "features": features,
            "syntax_violations": syntax_violations,
            "semantic_violations": semantic_violations,
            "violations": [*syntax_violations, *semantic_violations],
        }
        prior_path.write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        return result
    (run_dir / "prompt.txt").write_text(prompt, encoding="utf-8")
    (run_dir / "output-schema.json").write_text(
        json.dumps(output_schema(), indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    command = codex_command(args, run_dir, prompt)
    started = time.monotonic()
    try:
        completed = base.run_capture(command, cwd=run_dir, timeout=args.timeout)
        timed_out = False
    except subprocess.TimeoutExpired as error:
        completed = subprocess.CompletedProcess(
            command, 124, error.stdout or "", error.stderr or ""
        )
        timed_out = True
    duration = time.monotonic() - started
    (run_dir / "codex.stdout.jsonl").write_text(completed.stdout, encoding="utf-8")
    (run_dir / "codex.stderr").write_text(completed.stderr, encoding="utf-8")
    source, output_error = base.parse_last_message(run_dir / "last-message.json")
    if source is not None:
        (run_dir / "harness.stone").write_text(
            source.rstrip() + "\n", encoding="utf-8"
        )
    features = source_features(arm, source or "")
    syntax_violations = syntax_gate(arm, features)
    admission_ok, admission_error = admit_block_source(
        args, arm, source or "", run_dir
    )
    if not admission_ok:
        syntax_violations.append(f"Stone admission failed: {admission_error}")
    semantic_violations = semantic_gate(
        features,
        args.finding_evidence_predicates,
        args.stage_inputs,
        args.typed_stage_input_fields,
        args.relational_startup_finding,
    )
    violations = [*syntax_violations, *semantic_violations]
    result = {
        "ok": (
            completed.returncode == 0
            and output_error is None
            and base.tool_call_count(completed.stdout) == 0
            and not violations
        ),
        "arm": arm,
        "model": args.model,
        "codex_exit_code": completed.returncode,
        "codex_tool_calls": base.tool_call_count(completed.stdout),
        "timed_out": timed_out,
        "duration_seconds": duration,
        "output_error": output_error,
        "syntax_ok": not syntax_violations,
        "admission_ok": admission_ok,
        "admission_error": admission_error,
        "semantic_ok": not semantic_violations,
        "features": features,
        "syntax_violations": syntax_violations,
        "semantic_violations": semantic_violations,
        "violations": violations,
        "usage": stage_ab.codex_usage(completed.stdout),
    }
    (run_dir / "summary.json").write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return result


def admit_block_source(
    args: argparse.Namespace, arm: str, source: str, run_dir: Path
) -> tuple[bool, str | None]:
    if arm == "decorator":
        return True, None
    library = STANDARD_AGENT_LIBRARY.read_text(encoding="utf-8")
    declarations = re.sub(
        r"(?m)^run\s+[A-Za-z_][A-Za-z0-9_]*\s*$", "", source
    )
    admission_source = (
        library.rstrip()
        + "\n\n"
        + declarations.rstrip()
        + '\n\nemit({"admitted": True})\n'
    )
    admission_path = run_dir / "admission.stone"
    admission_path.write_text(admission_source, encoding="utf-8")
    work = run_dir / "admission-work"
    work.mkdir(exist_ok=True)
    completed = base.run_capture(
        [str(args.waymark_bin.resolve()), "eval", str(admission_path)],
        cwd=work,
        timeout=60,
    )
    payload = base.response_payload(completed)
    if (
        completed.returncode == 0
        and isinstance(payload, dict)
        and payload.get("ok") is True
    ):
        return True, None
    detail = completed.stderr.strip()
    if isinstance(payload, dict):
        error = payload.get("error")
        if isinstance(error, dict):
            detail = str(error.get("detail") or error.get("message") or detail)
    return False, detail or f"waymark exited {completed.returncode}"


def summarize(results: list[dict[str, Any]]) -> dict[str, Any]:
    syntax_passing = [result for result in results if result["syntax_ok"]]
    semantic_passing = [result for result in results if result["ok"]]
    return {
        "trials": len(results),
        "syntax_passes": len(syntax_passing),
        "requirement_evidence_passes": len(semantic_passing),
        "mean_source_bytes": (
            sum(result["features"]["bytes"] for result in syntax_passing)
            / len(syntax_passing)
            if syntax_passing
            else None
        ),
        "mean_source_lines": (
            sum(result["features"]["lines"] for result in syntax_passing)
            / len(syntax_passing)
            if syntax_passing
            else None
        ),
        "mean_stage_count": (
            sum(result["features"]["stage_count"] for result in syntax_passing)
            / len(syntax_passing)
            if syntax_passing
            else None
        ),
        "syntax_violations": [
            violation
            for result in results
            for violation in result.get("syntax_violations", [])
        ],
        "semantic_violations": [
            violation
            for result in results
            for violation in result.get("semantic_violations", [])
        ],
    }


def main() -> int:
    args = parse_args()
    if args.trials < 1 or args.trials > 10:
        raise SystemExit("--trials must be between 1 and 10")
    run_root = args.run_root.resolve()
    if run_root.exists() and any(run_root.iterdir()) and not (
        args.overwrite or args.reuse_existing
    ):
        raise SystemExit(f"refusing to overwrite non-empty run root: {run_root}")
    if run_root.exists() and args.overwrite:
        shutil.rmtree(run_root)
    run_root.mkdir(parents=True, exist_ok=True)
    selected_arms = tuple(args.arms or ARMS)
    prompts = {
        arm: arm_prompt(
            arm,
            args.finding_evidence_predicates,
            args.stage_inputs,
            args.typed_stage_input_fields,
            args.relational_startup_finding,
        )
        for arm in selected_arms
    }
    results = {arm: [] for arm in selected_arms}
    for trial in range(1, args.trials + 1):
        for arm in selected_arms:
            results[arm].append(
                evaluate_once(
                    args,
                    arm=arm,
                    prompt=prompts[arm],
                    run_dir=run_root / f"trial-{trial}" / arm,
                )
            )
    arms = {arm: summarize(results[arm]) for arm in selected_arms}
    complete = all(
        result["codex_exit_code"] == 0
        and result["output_error"] is None
        and result["codex_tool_calls"] == 0
        for arm_results in results.values()
        for result in arm_results
    )
    aggregate = {
        "schema": "waymark.staged-harness-syntax-authorship.v1",
        "complete": complete,
        "model": args.model,
        "reasoning_effort": args.reasoning_effort,
        "trials": args.trials,
        "finding_evidence_predicates": args.finding_evidence_predicates,
        "stage_inputs": args.stage_inputs,
        "typed_stage_input_fields": args.typed_stage_input_fields,
        "relational_startup_finding": args.relational_startup_finding,
        "arms": arms,
        "run_root": str(run_root),
    }
    aggregate_path = run_root / "aggregate.json"
    aggregate_path.write_text(
        json.dumps(aggregate, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps({**aggregate, "aggregate": str(aggregate_path)}, indent=2))
    return 0 if complete else 1


if __name__ == "__main__":
    raise SystemExit(main())
