// SPDX-License-Identifier: MIT OR Apache-2.0

//! The `model` value and typed model calls (design 5.6).
//!
//! `model.ask(prompt, output=T)` asks the Gateway model for an answer of
//! type `T`. The JSON Schema derived from `T` goes in a system message, the
//! answer is parsed and converted (`stone_records::convert_json`), and a
//! wrong answer is sent back with the problems found, a bounded number of
//! times. Every call goes through the same model transition as
//! `model_call`, so traces and hooks see each one.
//!
//! The model is a value bound to the name `model` at the top of every
//! program, so a function can take it as a parameter. Budgets
//! (`model.limit`, `model.remaining`) will hang off this value.

use serde_json::{json, Value as JsonValue};

/// The Gateway model. Each value charges its calls to a budget meter:
/// the attempt's for `model`, a narrowed one for `model.limit(...)`.
#[derive(Clone)]
pub(super) struct ModelValue {
    pub(super) meter: std::rc::Rc<super::stone_budget::Meter>,
    /// `model.last`: the most recent `model.ask` made through this value
    /// (shared by copies of the value, not by `model.limit(...)` children).
    pub(super) last: std::rc::Rc<std::cell::RefCell<Option<JsonValue>>>,
    /// A hash of each message the previous `model.ask` through this value
    /// sent, system messages included, for `model.last.reused_messages`.
    prefix: std::rc::Rc<std::cell::RefCell<Vec<u64>>>,
}

/// What `model.last` reports about a request besides its outcome.
pub(super) struct AskInfo {
    /// `{message, name, type, source}` for each named input the request's
    /// messages carried (design 5.9).
    pub(super) inputs: Vec<JsonValue>,
    /// Messages sent on the first attempt, system messages included.
    pub(super) messages: usize,
    /// How many of them, from the start, were identical to the previous
    /// ask through the same model value: the part a provider's prompt cache
    /// can reuse.
    pub(super) reused_messages: usize,
}

impl ModelValue {
    pub(super) fn new(meter: std::rc::Rc<super::stone_budget::Meter>) -> Self {
        Self {
            meter,
            last: Default::default(),
            prefix: Default::default(),
        }
    }

    /// Compare a request's messages with the previous ask's and remember
    /// them. Returns how many leading messages are unchanged.
    pub(super) fn reuse_prefix(&self, messages: &[JsonValue]) -> usize {
        use std::hash::{Hash, Hasher};
        let hashes = messages
            .iter()
            .map(|message| {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                message.to_string().hash(&mut hasher);
                hasher.finish()
            })
            .collect::<Vec<_>>();
        let mut previous = self.prefix.borrow_mut();
        let reused = previous
            .iter()
            .zip(&hashes)
            .take_while(|(old, new)| old == new)
            .count();
        *previous = hashes;
        reused
    }

    /// Remember a finished `model.ask`: its transition ids (one per
    /// attempt, repairs included), the last reply text, and the request's
    /// inputs and reused prefix.
    pub(super) fn record_last(&self, transition_ids: Vec<String>, reply: &str, ok: bool, info: &AskInfo) {
        *self.last.borrow_mut() = Some(json!({
            "attempts": transition_ids.len(),
            "transition_ids": transition_ids,
            "reply": reply,
            "ok": ok,
            "inputs": info.inputs,
            "messages": info.messages,
            "reused_messages": info.reused_messages,
        }));
    }
}

/// How the answer's type is shown unless `type_format=` says otherwise.
pub(super) const DEFAULT_TYPE_FORMAT: TypeFormat = TypeFormat::Python;
/// Default number of repairs after a wrong answer.
pub(super) const DEFAULT_RETRIES: i64 = 2;
pub(super) const MAX_RETRIES: i64 = 4;
/// Largest schema sent to the model.
pub(super) const MAX_SCHEMA_BYTES: usize = 64 * 1024;
/// Longest wrong answer echoed back in a repair turn.
pub(super) const MAX_REPAIR_CONTENT_CHARS: usize = 32 * 1024;

/// How `model.ask` shows the answer's type to the model.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TypeFormat {
    /// Python (pydantic) class definitions, as the author wrote them.
    Python,
    /// A JSON Schema derived from the type.
    JsonSchema,
}

impl TypeFormat {
    pub(super) fn parse(text: &str) -> Option<Self> {
        match text {
            "python" => Some(Self::Python),
            "json_schema" => Some(Self::JsonSchema),
            _ => None,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::Python => "required type",
            Self::JsonSchema => "required JSON Schema",
        }
    }
}

/// The system message for `type_format="python"`: the type's name and the
/// pydantic definitions of the records it uses.
pub(super) fn python_instruction(type_name: &str, classes: &str, wrapped: bool) -> String {
    let shape = if wrapped {
        format!("Answer with exactly one JSON object of the form {{\"value\": V}}, where V has the Python type `{type_name}`.")
    } else {
        format!("Answer with exactly one JSON object of type `{type_name}`.")
    };
    if classes.is_empty() {
        return format!("{shape} Do not add prose or Markdown.");
    }
    format!(
        "{shape} The record types are these pydantic models; write each as a JSON object with its field names, and leave a field out to use its default. Do not add prose or Markdown.\n\n{classes}"
    )
}

/// The system message that states the answer's shape.
pub(super) fn output_instruction(schema: &JsonValue, wrapped: bool) -> String {
    let schema = serde_json::to_string(schema).unwrap_or_else(|_| "{}".to_string());
    let shape = if wrapped {
        "Answer with exactly one JSON object of the form {\"value\": ...}, where the value matches this JSON Schema"
    } else {
        "Answer with exactly one JSON object that matches this JSON Schema"
    };
    format!("{shape}. Do not add prose or Markdown.\nJSON Schema: {schema}")
}

/// The user turn that asks for a corrected answer.
pub(super) fn repair_message(issues: &[JsonValue], format: TypeFormat) -> String {
    let mut message = format!(
        "Your answer does not match the {}. Problems:\n",
        format.noun()
    );
    for issue in issues {
        let path = issue.get("path").and_then(JsonValue::as_str).unwrap_or("$");
        let text = issue
            .get("message")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        message.push_str(&format!("- {path}: {text}\n"));
    }
    message.push_str("Return only the corrected JSON object.");
    message
}

/// The repair request after the program's `validate=` function rejected a
/// well-typed answer.
pub(super) fn validate_message(issues: &[JsonValue]) -> String {
    let mut message = String::from("The program checked your answer and found problems:\n");
    for issue in issues {
        let text = issue.get("message").and_then(JsonValue::as_str).unwrap_or("");
        message.push_str(&format!("- {text}\n"));
    }
    message.push_str("Fix them and return only the corrected JSON object.");
    message
}

/// Parse the model's text as one JSON value. A Markdown code fence around
/// the object is tolerated, since models add one out of habit.
pub(super) fn parse_answer(content: &str) -> Result<JsonValue, String> {
    let trimmed = content.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|rest| rest.trim_end().strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    serde_json::from_str(body).map_err(|error| format!("the answer is not valid JSON: {error}"))
}

/// The `issues` entry for an answer that is not JSON at all.
pub(super) fn parse_issue(message: String) -> JsonValue {
    json!({"path": "$", "message": message})
}

/// Scripted model replies for unit tests, held by the session: each model
/// call takes the next reply instead of calling the Gateway.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestModel {
    pub(crate) replies: std::collections::VecDeque<String>,
    pub(crate) requests: Vec<JsonValue>,
}

#[cfg(test)]
impl TestModel {
    pub(super) fn reply(&mut self, request: &JsonValue) -> nu_protocol::Value {
        self.requests.push(request.clone());
        let reply = self.replies.pop_front().unwrap_or_default();
        crate::json::json_to_nu_value(
            json!({
                "ok": true,
                "text": reply.clone(),
                "content": reply,
                "provider": "test",
                "messages": [],
                "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15},
            }),
            nu_protocol::Span::unknown(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_answer, repair_message};
    use serde_json::json;

    #[test]
    fn answers_may_be_fenced() {
        assert_eq!(
            parse_answer("```json\n{\"a\": 1}\n```").unwrap(),
            json!({"a": 1})
        );
        assert_eq!(parse_answer(" {\"a\": 1} ").unwrap(), json!({"a": 1}));
        assert!(parse_answer("sure! {\"a\": 1}").is_err());
    }

    #[test]
    fn repair_lists_each_problem() {
        let message = repair_message(
            &[
                json!({"path": "$.steps[1]", "message": "expected string, got 3"}),
                json!({"path": "$", "message": "missing required field `risky` (boolean)"}),
            ],
            super::TypeFormat::JsonSchema,
        );
        assert!(message.contains("- $.steps[1]: expected string, got 3"));
        assert!(message.contains("missing required field `risky`"));
    }
}
