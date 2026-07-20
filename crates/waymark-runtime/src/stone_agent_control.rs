// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::agent::AgentAction;

#[derive(Clone)]
pub(super) struct AgentControlValue {
    pub(super) control_id: u64,
    pub(super) kind: AgentControlKind,
    pub(super) max_rounds: usize,
    pub(super) max_turns: usize,
    pub(super) max_tool_ms: Option<u64>,
    pub(super) completion_path: Option<String>,
}

#[derive(Clone)]
pub(super) enum AgentControlKind {
    React { model: Option<String> },
    Scripted { actions: Vec<AgentAction> },
}

impl AgentControlValue {
    pub(super) fn name(&self) -> &'static str {
        match &self.kind {
            AgentControlKind::React { .. } => "react_json_v0",
            AgentControlKind::Scripted { .. } => "scripted_v0",
        }
    }
}
