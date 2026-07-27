// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{ShellError, Span, Value};

use super::stone_error;
use super::stone_functions::{
    CallableValue, TransitionHooksValue, WorkflowEvidenceSourceValue, WorkflowStageValue,
    WorkflowValue,
};
use super::stone_json_view::{
    materialize_json_array_view, materialize_json_object_view, materialize_json_scalar_view,
    materialize_jsonl_rows, JsonArrayView, JsonObjectView, JsonScalarView, JsonlRows,
};
use crate::stone_agent_control::AgentControlValue;
use crate::stone_attempt_scope::AttemptScopeValue;
use crate::stone_attempt_value::{AttemptHandleValue, AttemptOutcomeValue};

#[derive(Clone)]
pub(super) enum RuntimeValue {
    Nu(Value),
    File(FileHandle),
    TextLines(TextLines),
    JsonlRows(JsonlRows),
    JsonObjectView(JsonObjectView),
    JsonArrayView(JsonArrayView),
    JsonScalarView(JsonScalarView),
    Callable(CallableValue),
    TransitionHooks(TransitionHooksValue),
    WorkflowEvidenceSource(WorkflowEvidenceSourceValue),
    WorkflowStage(WorkflowStageValue),
    Workflow(WorkflowValue),
    AgentControl(AgentControlValue),
    AttemptScope(AttemptScopeValue),
    AttemptHandle(AttemptHandleValue),
    AttemptOutcome(AttemptOutcomeValue),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RuntimeValueTag {
    Nu,
    File,
    TextLines,
    JsonlRows,
    JsonObjectView,
    JsonArrayView,
    JsonScalarView,
    Callable,
    TransitionHooks,
    WorkflowEvidenceSource,
    WorkflowStage,
    Workflow,
    AgentControl,
    AttemptScope,
    AttemptHandle,
    AttemptOutcome,
}

impl RuntimeValueTag {
    #[allow(dead_code)]
    pub(super) fn id(self) -> u8 {
        match self {
            RuntimeValueTag::Nu => 1,
            RuntimeValueTag::File => 2,
            RuntimeValueTag::TextLines => 3,
            RuntimeValueTag::JsonlRows => 4,
            RuntimeValueTag::JsonObjectView => 5,
            RuntimeValueTag::JsonArrayView => 6,
            RuntimeValueTag::JsonScalarView => 7,
            RuntimeValueTag::Callable => 8,
            RuntimeValueTag::AgentControl => 9,
            RuntimeValueTag::AttemptScope => 10,
            RuntimeValueTag::AttemptHandle => 11,
            RuntimeValueTag::AttemptOutcome => 12,
            RuntimeValueTag::TransitionHooks => 13,
            RuntimeValueTag::WorkflowStage => 14,
            RuntimeValueTag::Workflow => 15,
            RuntimeValueTag::WorkflowEvidenceSource => 16,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct FileHandle {
    pub(super) scope_index: usize,
    pub(super) file_id: u64,
}

#[derive(Clone)]
pub(super) struct TextLines {
    pub(super) lines: Vec<String>,
    pub(super) source: String,
}

impl RuntimeValue {
    pub(super) fn is_session_persistable(&self) -> bool {
        match self {
            RuntimeValue::Nu(_) => true,
            RuntimeValue::TextLines(_) => true,
            RuntimeValue::JsonlRows(_) => true,
            RuntimeValue::JsonObjectView(_) => true,
            RuntimeValue::JsonArrayView(_) => true,
            RuntimeValue::JsonScalarView(_) => true,
            RuntimeValue::Callable(callable) => callable.captures_are_persistable(),
            RuntimeValue::TransitionHooks(hooks) => hooks.is_session_persistable(),
            RuntimeValue::WorkflowEvidenceSource(source) => source.is_session_persistable(),
            RuntimeValue::WorkflowStage(stage) => stage.is_session_persistable(),
            RuntimeValue::Workflow(workflow) => workflow.is_session_persistable(),
            RuntimeValue::AgentControl(_) => true,
            RuntimeValue::AttemptScope(_) => false,
            RuntimeValue::AttemptHandle(_) => false,
            RuntimeValue::AttemptOutcome(_) => true,
            RuntimeValue::File(_) => false,
        }
    }

    #[allow(dead_code)]
    pub(super) fn type_tag(&self) -> RuntimeValueTag {
        match self {
            RuntimeValue::Nu(_) => RuntimeValueTag::Nu,
            RuntimeValue::File(_) => RuntimeValueTag::File,
            RuntimeValue::TextLines(_) => RuntimeValueTag::TextLines,
            RuntimeValue::JsonlRows(_) => RuntimeValueTag::JsonlRows,
            RuntimeValue::JsonObjectView(_) => RuntimeValueTag::JsonObjectView,
            RuntimeValue::JsonArrayView(_) => RuntimeValueTag::JsonArrayView,
            RuntimeValue::JsonScalarView(_) => RuntimeValueTag::JsonScalarView,
            RuntimeValue::Callable(_) => RuntimeValueTag::Callable,
            RuntimeValue::TransitionHooks(_) => RuntimeValueTag::TransitionHooks,
            RuntimeValue::WorkflowEvidenceSource(_) => RuntimeValueTag::WorkflowEvidenceSource,
            RuntimeValue::WorkflowStage(_) => RuntimeValueTag::WorkflowStage,
            RuntimeValue::Workflow(_) => RuntimeValueTag::Workflow,
            RuntimeValue::AgentControl(_) => RuntimeValueTag::AgentControl,
            RuntimeValue::AttemptScope(_) => RuntimeValueTag::AttemptScope,
            RuntimeValue::AttemptHandle(_) => RuntimeValueTag::AttemptHandle,
            RuntimeValue::AttemptOutcome(_) => RuntimeValueTag::AttemptOutcome,
        }
    }

    pub(super) fn into_nu_value(self, context: &str) -> Result<Value, ShellError> {
        match self {
            RuntimeValue::Nu(value) => Ok(value),
            RuntimeValue::File(_) => Err(stone_error(
                context,
                "file objects are task-owned runtime values and cannot cross this boundary",
            )),
            RuntimeValue::TextLines(lines) => Ok(Value::list(
                lines
                    .lines
                    .into_iter()
                    .map(|line| Value::string(line, Span::unknown()))
                    .collect(),
                Span::unknown(),
            )),
            RuntimeValue::JsonlRows(rows) => materialize_jsonl_rows(&rows),
            RuntimeValue::JsonObjectView(view) => materialize_json_object_view(&view),
            RuntimeValue::JsonArrayView(view) => materialize_json_array_view(&view),
            RuntimeValue::JsonScalarView(view) => materialize_json_scalar_view(&view),
            RuntimeValue::Callable(callable) => Err(stone_error(
                context,
                format!(
                    "callable {} is a task-owned runtime value and cannot cross this boundary",
                    callable.display_name()
                ),
            )),
            RuntimeValue::TransitionHooks(_) => Err(stone_error(
                context,
                "transition hooks are task-owned runtime values and cannot cross this boundary",
            )),
            RuntimeValue::WorkflowEvidenceSource(_) => Err(stone_error(
                context,
                "workflow evidence specifications are task-owned control values and cannot cross this boundary",
            )),
            RuntimeValue::WorkflowStage(stage) => Err(stone_error(
                context,
                format!(
                    "workflow stage `{}` is a task-owned control value and cannot cross this boundary",
                    stage.name
                ),
            )),
            RuntimeValue::Workflow(workflow) => Err(stone_error(
                context,
                format!(
                    "workflow `{}` is a task-owned control value and cannot cross this boundary",
                    workflow.name
                ),
            )),
            RuntimeValue::AgentControl(control) => Err(stone_error(
                context,
                format!(
                    "agent control {}#{} is a task-owned callable and cannot cross this boundary",
                    control.name(),
                    control.control_id
                ),
            )),
            RuntimeValue::AttemptScope(scope) => Err(stone_error(
                context,
                format!(
                    "attempt scope #{} is a task-owned supervision value and cannot cross this boundary",
                    scope.scope_id
                ),
            )),
            RuntimeValue::AttemptHandle(handle) => Ok(handle.materialize()),
            RuntimeValue::AttemptOutcome(outcome) => Ok(outcome.materialize()),
        }
    }
}

#[cfg(test)]
mod tests {
    use nu_protocol::{Span, Value};

    use super::super::stone_functions::CallableValue;
    use super::{FileHandle, RuntimeValue, RuntimeValueTag, TextLines};
    use crate::stone_ast::Expr;
    use crate::stone_attempt_value::AttemptOutcomeValue;

    #[test]
    fn runtime_value_tags_have_stable_compact_ids() {
        assert_eq!(RuntimeValueTag::Nu.id(), 1);
        assert_eq!(RuntimeValueTag::File.id(), 2);
        assert_eq!(RuntimeValueTag::TextLines.id(), 3);
        assert_eq!(RuntimeValueTag::JsonlRows.id(), 4);
        assert_eq!(RuntimeValueTag::JsonObjectView.id(), 5);
        assert_eq!(RuntimeValueTag::JsonArrayView.id(), 6);
        assert_eq!(RuntimeValueTag::JsonScalarView.id(), 7);
        assert_eq!(RuntimeValueTag::Callable.id(), 8);
        assert_eq!(RuntimeValueTag::AgentControl.id(), 9);
        assert_eq!(RuntimeValueTag::AttemptScope.id(), 10);
        assert_eq!(RuntimeValueTag::AttemptHandle.id(), 11);
        assert_eq!(RuntimeValueTag::AttemptOutcome.id(), 12);
        assert_eq!(RuntimeValueTag::TransitionHooks.id(), 13);
        assert_eq!(RuntimeValueTag::WorkflowStage.id(), 14);
        assert_eq!(RuntimeValueTag::Workflow.id(), 15);
        assert_eq!(RuntimeValueTag::WorkflowEvidenceSource.id(), 16);
    }

    #[test]
    fn file_handles_are_not_session_persistable() {
        let value = RuntimeValue::File(FileHandle {
            scope_index: 1,
            file_id: 2,
        });

        assert!(!value.is_session_persistable());
        assert_eq!(value.type_tag(), RuntimeValueTag::File);
    }

    #[test]
    fn callables_are_persistable_only_when_captures_are_persistable() {
        let persistable = RuntimeValue::Callable(CallableValue::lambda(
            1,
            Vec::new(),
            Box::new(Expr::None),
            vec![(
                "answer".to_string(),
                RuntimeValue::Nu(Value::int(42, Span::unknown())),
            )],
        ));
        assert!(persistable.is_session_persistable());
        assert_eq!(persistable.type_tag(), RuntimeValueTag::Callable);

        let non_persistable = RuntimeValue::Callable(CallableValue::lambda(
            2,
            Vec::new(),
            Box::new(Expr::None),
            vec![(
                "file".to_string(),
                RuntimeValue::File(FileHandle {
                    scope_index: 0,
                    file_id: 7,
                }),
            )],
        ));
        assert!(!non_persistable.is_session_persistable());
    }

    #[test]
    fn text_lines_materialize_to_nu_list() {
        let value = RuntimeValue::TextLines(TextLines {
            lines: vec!["alpha".to_string(), "beta".to_string()],
            source: "memory".to_string(),
        })
        .into_nu_value("test")
        .expect("text lines should materialize");

        let list = value.as_list().expect("text lines become a list");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].as_str().expect("string"), "alpha");
        assert_eq!(list[1].as_str().expect("string"), "beta");
    }

    #[test]
    fn task_owned_runtime_values_cannot_cross_nu_boundary() {
        let file_error = RuntimeValue::File(FileHandle {
            scope_index: 0,
            file_id: 1,
        })
        .into_nu_value("file boundary")
        .expect_err("file handles cannot materialize");
        assert!(file_error.to_string().contains("file boundary"));

        let callable_error = RuntimeValue::Callable(CallableValue::lambda(
            99,
            Vec::new(),
            Box::new(Expr::None),
            Vec::new(),
        ))
        .into_nu_value("callable boundary")
        .expect_err("callables cannot materialize");
        assert!(callable_error.to_string().contains("callable boundary"));
    }

    #[test]
    fn attempt_outcome_materializes_separated_phase_views() {
        let span = Span::unknown();
        let mut metadata = nu_protocol::Record::new();
        metadata.push("controller_result_status", Value::string("succeeded", span));
        let mut attempt = nu_protocol::Record::new();
        attempt.push("metadata", Value::record(metadata, span));
        let mut reported_result = nu_protocol::Record::new();
        reported_result.push("candidate", Value::string("cobalt", span));
        attempt.push("reported_result", Value::record(reported_result, span));
        attempt.push("reported_error", Value::nothing(span));
        let value = RuntimeValue::AttemptOutcome(AttemptOutcomeValue {
            attempt: "attempt-1".to_string(),
            joined: true,
            timed_out: false,
            state: "active".to_string(),
            controller_state: "exited".to_string(),
            record: Value::record(attempt, span),
        })
        .into_nu_value("test")
        .expect("attempt outcome should materialize");
        let record = value.as_record().expect("outcome record");
        for (phase, expected) in [
            ("execution", "exited"),
            ("result", "succeeded"),
            ("evaluation", "not_evaluated"),
            ("selection", "pending"),
            ("cleanup", "pending"),
        ] {
            let phase = record.get(phase).unwrap().as_record().unwrap();
            assert_eq!(phase.get("status").unwrap().as_str().unwrap(), expected);
        }
        let result = record.get("result").unwrap().as_record().unwrap();
        assert_eq!(
            result
                .get("value")
                .unwrap()
                .as_record()
                .unwrap()
                .get("candidate")
                .unwrap()
                .as_str()
                .unwrap(),
            "cobalt"
        );
        assert!(result.get("error").unwrap().is_nothing());
    }
}
