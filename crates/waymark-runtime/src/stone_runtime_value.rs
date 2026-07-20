// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{ShellError, Span, Value};

use super::stone_error;
use super::stone_functions::CallableValue;
use super::stone_json_view::{
    materialize_json_array_view, materialize_json_object_view, materialize_json_scalar_view,
    materialize_jsonl_rows, JsonArrayView, JsonObjectView, JsonScalarView, JsonlRows,
};
use crate::stone_agent_control::AgentControlValue;
use crate::stone_attempt_scope::AttemptScopeValue;

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
    AgentControl(AgentControlValue),
    AttemptScope(AttemptScopeValue),
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
    AgentControl,
    AttemptScope,
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
            RuntimeValue::Callable(callable) => callable
                .captures
                .iter()
                .all(|(_, value)| value.is_session_persistable()),
            RuntimeValue::AgentControl(_) => true,
            RuntimeValue::AttemptScope(_) => false,
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
            RuntimeValue::AgentControl(_) => RuntimeValueTag::AgentControl,
            RuntimeValue::AttemptScope(_) => RuntimeValueTag::AttemptScope,
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
                    "callable lambda#{} is a task-owned runtime value and cannot cross this boundary",
                    callable.function_id
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
        }
    }
}

#[cfg(test)]
mod tests {
    use nu_protocol::{Span, Value};

    use super::super::stone_functions::CallableValue;
    use super::{FileHandle, RuntimeValue, RuntimeValueTag, TextLines};
    use crate::stone_ast::Expr;

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
        let persistable = RuntimeValue::Callable(CallableValue {
            function_id: 1,
            params: Vec::new(),
            body: Box::new(Expr::None),
            captures: vec![(
                "answer".to_string(),
                RuntimeValue::Nu(Value::int(42, Span::unknown())),
            )],
        });
        assert!(persistable.is_session_persistable());
        assert_eq!(persistable.type_tag(), RuntimeValueTag::Callable);

        let non_persistable = RuntimeValue::Callable(CallableValue {
            function_id: 2,
            params: Vec::new(),
            body: Box::new(Expr::None),
            captures: vec![(
                "file".to_string(),
                RuntimeValue::File(FileHandle {
                    scope_index: 0,
                    file_id: 7,
                }),
            )],
        });
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

        let callable_error = RuntimeValue::Callable(CallableValue {
            function_id: 99,
            params: Vec::new(),
            body: Box::new(Expr::None),
            captures: Vec::new(),
        })
        .into_nu_value("callable boundary")
        .expect_err("callables cannot materialize");
        assert!(callable_error.to_string().contains("callable boundary"));
    }
}
