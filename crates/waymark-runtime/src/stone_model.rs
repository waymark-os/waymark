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
}

/// Default number of repairs after a wrong answer.
pub(super) const DEFAULT_RETRIES: i64 = 2;
pub(super) const MAX_RETRIES: i64 = 4;
/// Largest schema sent to the model.
pub(super) const MAX_SCHEMA_BYTES: usize = 64 * 1024;
/// Longest wrong answer echoed back in a repair turn.
pub(super) const MAX_REPAIR_CONTENT_CHARS: usize = 32 * 1024;

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
pub(super) fn repair_message(issues: &[JsonValue]) -> String {
    let mut message =
        String::from("Your answer does not match the required JSON Schema. Problems:\n");
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
        let message = repair_message(&[
            json!({"path": "$.steps[1]", "message": "expected string, got 3"}),
            json!({"path": "$", "message": "missing required field `risky` (boolean)"}),
        ]);
        assert!(message.contains("- $.steps[1]: expected string, got 3"));
        assert!(message.contains("missing required field `risky`"));
    }
}
