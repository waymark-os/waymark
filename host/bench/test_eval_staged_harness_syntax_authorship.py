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
@stage(goal="inspect", evidence=artifact("plan"), max_actions=4)
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
    stage inspect(goal="inspect", evidence=artifact("plan"), max_actions=4):
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
        ensure artifact("plan")

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


class StagedHarnessSyntaxAuthorshipTests(unittest.TestCase):
    def test_prompts_share_task_and_semantics(self) -> None:
        decorator = EXPERIMENT.arm_prompt("decorator")
        block = EXPERIMENT.arm_prompt("block")
        for prompt in (decorator, block):
            self.assertIn("doomgeneric_mips", prompt)
            self.assertIn("Every public task obligation", prompt)
            self.assertNotIn("stage inspect", prompt)
        self.assertIn("@stage(", decorator)
        self.assertIn("workflow project:", block)
        self.assertIn("ensure expression", EXPERIMENT.arm_prompt("contract"))

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
