// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{ShellError, Span, Value};

use super::stone_error;
use super::stone_functions::CallableValue;
use super::stone_json_view::{
    materialize_json_array_view, materialize_json_object_view, materialize_json_scalar_view,
    materialize_jsonl_rows, JsonArrayView, JsonObjectView, JsonScalarView, JsonlRows,
};

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
        }
    }
}
