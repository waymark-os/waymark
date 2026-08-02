#!/usr/bin/env python3

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name(
    "eval_staged_harness_syntax_authorship.py"
)
SPEC = importlib.util.spec_from_file_location("staged_harness_syntax", MODULE_PATH)
assert SPEC and SPEC.loader
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


DECORATOR_SOURCE = r'''
@stage(goal="inspect", evidence=decision_recorded(resolved=["source_layout", "toolchain"]), max_actions=4)
def inspect(step):
    return agent_loop(step)

@stage(goal="build", evidence=artifact("/app/doomgeneric_mips", format="elf", arch="mips"), max_actions=8, checkpoint="repairable")
def build(step):
    return agent_loop(step)

@stage(goal="execute", evidence=command_succeeded(["node", "/app/vm.js"], stdout="nonempty"), max_actions=4)
def execute(step):
    return agent_loop(step)

@stage(goal="verify", evidence=file_valid("/tmp/frame.bmp", format="bmp"), max_actions=2)
def verify(step):
    return verify_outputs(step)

emit(workflow_run(workflow("doom", inspect, build, execute, verify)))
'''


BLOCK_SOURCE = r'''
workflow doom:
    stage inspect(goal="inspect", evidence=decision_recorded(resolved=["source_layout", "toolchain"]), max_actions=4):
        agent_loop()

    stage build(goal="build", evidence=artifact("/app/doomgeneric_mips", format="elf", arch="mips"), max_actions=8, checkpoint="repairable"):
        agent_loop()

    stage execute(goal="execute", evidence=command_succeeded(["node", "/app/vm.js"], stdout="nonempty"), max_actions=4):
        agent_loop()

    stage verify(goal="verify", evidence=file_valid("/tmp/frame.bmp", format="bmp"), max_actions=2):
        verify_outputs()

run doom
'''


CONTRACT_SOURCE = r'''
workflow doom:
    stage inspect(goal="inspect", max_actions=4):
        agent_loop()
        ensure decision_recorded(resolved=["source_layout", "toolchain"])

    stage build(goal="build", max_actions=8, checkpoint="repairable"):
        agent_loop()
        ensure artifact("/app/doomgeneric_mips", format="elf", arch="mips")

    stage execute(goal="execute", max_actions=4):
        agent_loop()
        ensure command_succeeded(["node", "/app/vm.js"])
        ensure stdout_nonempty()

    stage verify(goal="verify", max_actions=2):
        verify_outputs()
        ensure file_valid("/tmp/frame.bmp", format="bmp")

run doom
'''


PREDICATE_CONTRACT_SOURCE = CONTRACT_SOURCE.replace(
    'resolved=["source_layout", "toolchain"]',
    '''resolved={
            "source_layout": {"question": "Where?", "evidence": {"tool": "find", "contains": "doomgeneric.c", "success": True}},
            "toolchain": {"question": "Which compiler?", "evidence": {"tool": "run_linux", "contains": "mips", "success": True}},
            "frame_backend": {"question": "Which backend?", "evidence": {"tool": "read", "contains": "/tmp/frame.bmp", "success": True}},
            "runtime_contract": {"question": "Which ABI?", "evidence": {"tool": "read", "contains": "entryPoint", "success": True}},
        }''',
)

HANDOFF_CONTRACT_SOURCE = CONTRACT_SOURCE.replace(
    'stage build(goal="build", max_actions=8, checkpoint="repairable"):',
    'stage build(goal="build", inputs=["inspect"], max_actions=8, checkpoint="repairable"):',
)


class StagedHarnessSyntaxAuthorshipTests(unittest.TestCase):
    def test_prompts_share_task_and_semantics(self) -> None:
        decorator = EXPERIMENT.arm_prompt("decorator")
        block = EXPERIMENT.arm_prompt("block")
        for prompt in (decorator, block):
            self.assertIn("doomgeneric_mips", prompt)
            self.assertIn("Every public task obligation", prompt)
            self.assertIn("mounted at /app", prompt)
            self.assertIn("requested root output under a source subdirectory", prompt)
            self.assertIn("resolved=[", prompt)
            self.assertNotIn("stage inspect", prompt)
        self.assertIn("@stage(", decorator)
        self.assertIn("workflow project:", block)
        self.assertIn("ensure expression", EXPERIMENT.arm_prompt("contract"))

    def test_predicate_prompt_and_gate_require_executable_evidence(self) -> None:
        prompt = EXPERIMENT.arm_prompt("contract", True)
        self.assertIn("descriptor with an executable", prompt)
        self.assertIn("`evidence` predicate", prompt)
        self.assertIn('"contains"', prompt)
        features = EXPERIMENT.source_features(
            "contract", PREDICATE_CONTRACT_SOURCE
        )
        self.assertEqual([], EXPERIMENT.semantic_gate(features, True))
        missing = EXPERIMENT.source_features("contract", CONTRACT_SOURCE)
        violations = EXPERIMENT.semantic_gate(missing, True)
        self.assertTrue(any("four finding evidence" in item for item in violations))
        misplaced = CONTRACT_SOURCE.replace(
            "agent_loop()",
            'agent_loop({"evidence": {"tool": "read", "contains": "x", "success": True}})',
            1,
        )
        misplaced_features = EXPERIMENT.source_features("contract", misplaced)
        self.assertEqual(0, misplaced_features["finding_evidence_count"])
        invalid_kind = PREDICATE_CONTRACT_SOURCE.replace(
            '"question": "Where?"',
            '"kind": "inspection", "question": "Where?"',
        )
        invalid_features = EXPERIMENT.source_features("contract", invalid_kind)
        invalid_violations = EXPERIMENT.semantic_gate(invalid_features, True)
        self.assertTrue(any("invalid finding kinds" in item for item in invalid_violations))

    def test_stage_input_prompt_and_gate_require_typed_handoff(self) -> None:
        prompt = EXPERIMENT.arm_prompt("contract", False, True)
        self.assertIn('inputs=["earlier_stage_name"]', prompt)
        self.assertIn("step.stage_outputs", prompt)
        features = EXPERIMENT.source_features(
            "contract", HANDOFF_CONTRACT_SOURCE
        )
        self.assertEqual([], EXPERIMENT.semantic_gate(features, False, True))
        self.assertEqual(["inspect"], features["stage_input_references"])
        missing = EXPERIMENT.source_features("contract", CONTRACT_SOURCE)
        violations = EXPERIMENT.semantic_gate(missing, False, True)
        self.assertTrue(any("input from the decision stage" in item for item in violations))

    def test_structural_gate_accepts_equivalent_examples(self) -> None:
        for arm, source in (
            ("decorator", DECORATOR_SOURCE),
            ("block", BLOCK_SOURCE),
            ("contract", CONTRACT_SOURCE),
        ):
            features = EXPERIMENT.source_features(arm, source)
            self.assertEqual([], EXPERIMENT.syntax_gate(arm, features))
            self.assertEqual([], EXPERIMENT.semantic_gate(features))

    def test_structural_gate_rejects_late_build_and_missing_evidence(self) -> None:
        source = BLOCK_SOURCE.replace("stage build", "stage explore").replace(
            'evidence=file_valid("/tmp/frame.bmp", format="bmp"), ', ""
        )
        features = EXPERIMENT.source_features("block", source)
        syntax_violations = EXPERIMENT.syntax_gate("block", features)
        semantic_violations = EXPERIMENT.semantic_gate(features)
        self.assertTrue(any("build stage" in item for item in semantic_violations))
        self.assertTrue(any("evidence_count" in item for item in syntax_violations))


if __name__ == "__main__":
    unittest.main()
