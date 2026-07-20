// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{Record, ShellError, Span, Value};

use crate::stone_eval::stone_error;

#[derive(Clone)]
pub(super) struct AttemptHandleValue {
    pub(super) attempt: String,
    record: Value,
}

impl AttemptHandleValue {
    pub(super) fn new(attempt: String, record: Value) -> Self {
        Self { attempt, record }
    }

    pub(super) fn materialize(&self) -> Value {
        self.record.clone()
    }

    pub(super) fn attribute(&self, attr: &str) -> Result<Value, ShellError> {
        if attr == "type" {
            return Ok(Value::string("attempt_handle", Span::unknown()));
        }
        if attr == "attempt" {
            return Ok(Value::string(self.attempt.clone(), Span::unknown()));
        }
        record_attribute(&self.record, attr, "attempt_handle")
    }
}

#[derive(Clone)]
pub(super) struct AttemptOutcomeValue {
    pub(super) attempt: String,
    pub(super) joined: bool,
    pub(super) timed_out: bool,
    pub(super) state: String,
    pub(super) controller_state: String,
    pub(super) record: Value,
}

impl AttemptOutcomeValue {
    pub(super) fn materialize(&self) -> Value {
        let span = Span::unknown();
        let mut outcome = Record::new();
        outcome.push("attempt", Value::string(self.attempt.clone(), span));
        outcome.push("joined", Value::bool(self.joined, span));
        outcome.push("timed_out", Value::bool(self.timed_out, span));
        outcome.push("state", Value::string(self.state.clone(), span));
        outcome.push(
            "controller_state",
            Value::string(self.controller_state.clone(), span),
        );
        let mut execution = Record::new();
        execution.push("status", Value::string(self.controller_state.clone(), span));
        execution.push("joined", Value::bool(self.joined, span));
        execution.push("timed_out", Value::bool(self.timed_out, span));
        outcome.push("execution", Value::record(execution, span));

        let result_status = attempt_metadata_string(&self.record, "controller_result_status")
            .unwrap_or_else(|| "unreported".to_string());
        let mut result = Record::new();
        result.push("status", Value::string(result_status, span));
        result.push(
            "reason",
            Value::string(
                attempt_metadata_string(&self.record, "controller_result_reason")
                    .unwrap_or_default(),
                span,
            ),
        );
        outcome.push("result", Value::record(result, span));

        let mut evaluation = Record::new();
        evaluation.push("status", Value::string("not_evaluated", span));
        outcome.push("evaluation", Value::record(evaluation, span));
        let mut selection = Record::new();
        selection.push(
            "status",
            Value::string(
                attempt_metadata_string(&self.record, "selection_state")
                    .unwrap_or_else(|| "pending".to_string()),
                span,
            ),
        );
        outcome.push("selection", Value::record(selection, span));
        let mut cleanup = Record::new();
        cleanup.push(
            "status",
            Value::string(
                if self.state == "active" {
                    "pending"
                } else {
                    "closed"
                },
                span,
            ),
        );
        outcome.push("cleanup", Value::record(cleanup, span));
        outcome.push("record", self.record.clone());
        Value::record(outcome, span)
    }

    pub(super) fn attribute(&self, attr: &str) -> Result<Value, ShellError> {
        if attr == "type" {
            return Ok(Value::string("attempt_outcome", Span::unknown()));
        }
        record_attribute(&self.materialize(), attr, "attempt_outcome")
    }
}

fn attempt_metadata_string(value: &Value, key: &str) -> Option<String> {
    let Value::Record { val, .. } = value else {
        return None;
    };
    let Value::Record { val: metadata, .. } = val.get("metadata")? else {
        return None;
    };
    metadata.get(key)?.as_str().ok().map(str::to_string)
}

fn record_attribute(value: &Value, attr: &str, kind: &str) -> Result<Value, ShellError> {
    let Value::Record { val, .. } = value else {
        return Err(stone_error(kind, "runtime value did not contain a record"));
    };
    val.get(attr)
        .cloned()
        .ok_or_else(|| stone_error("attribute", format!("{kind} has no attribute `{attr}`")))
}
