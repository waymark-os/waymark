// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use nu_protocol::{Record, ShellError, Span, Value};
use serde_json::{json, Value as JsonValue};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SemanticFrontierMode {
    Parent,
    RetainedRepair,
}

impl SemanticFrontierMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::RetainedRepair => "retained",
        }
    }
}

#[derive(Clone)]
pub(super) struct SemanticFrontierValue {
    pub(super) frontier_id: u64,
    pub(super) checkpoint: String,
    pub(super) owner_attempt: String,
    pub(super) source_workspace: String,
    pub(super) task: String,
    pub(super) workspace: String,
    pub(super) mode: SemanticFrontierMode,
    pub(super) seal_duration_ms: u64,
    pub(super) storage_bytes: u64,
    pub(super) guidance_level: &'static str,
    record: Value,
    branch_count: Arc<AtomicU64>,
}

impl SemanticFrontierValue {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        frontier_id: u64,
        checkpoint: String,
        owner_attempt: String,
        source_workspace: String,
        task: String,
        workspace: String,
        mode: SemanticFrontierMode,
        seal_duration_ms: u64,
        storage_bytes: u64,
        guidance_level: &'static str,
        record: Value,
    ) -> Self {
        Self {
            frontier_id,
            checkpoint,
            owner_attempt,
            source_workspace,
            task,
            workspace,
            mode,
            seal_duration_ms,
            storage_bytes,
            guidance_level,
            record,
            branch_count: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(super) fn attribute(&self, attr: &str) -> Result<Value, ShellError> {
        if attr == "type" {
            return Ok(Value::string("semantic_frontier", Span::unknown()));
        }
        record_attribute(&self.record, attr, "semantic_frontier")
    }

    pub(super) fn mark_branched(&self) {
        self.branch_count.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn branch_count(&self) -> u64 {
        self.branch_count.load(Ordering::Relaxed)
    }

    pub(super) fn diagnostic(&self) -> JsonValue {
        json!({
            "frontier_id": self.frontier_id,
            "availability": self.mode.as_str(),
            "branch_count": self.branch_count(),
            "unused": self.branch_count() == 0,
            "seal_duration_ms": self.seal_duration_ms,
            "storage_bytes": self.storage_bytes,
            "guidance_level": self.guidance_level,
        })
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
        let result_status = attempt_metadata_string(&self.record, "controller_result_status")
            .unwrap_or_else(|| "unreported".to_string());
        let succeeded = self.joined && result_status == "succeeded";
        let mut outcome = Record::new();
        outcome.push("attempt", Value::string(self.attempt.clone(), span));
        outcome.push("joined", Value::bool(self.joined, span));
        outcome.push("timed_out", Value::bool(self.timed_out, span));
        outcome.push("ok", Value::bool(succeeded, span));
        outcome.push("succeeded", Value::bool(succeeded, span));
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
        result.push(
            "value",
            attempt_record_field(&self.record, "reported_result")
                .unwrap_or_else(|| Value::nothing(span)),
        );
        result.push(
            "error",
            attempt_record_field(&self.record, "reported_error")
                .unwrap_or_else(|| Value::nothing(span)),
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

fn attempt_record_field(value: &Value, key: &str) -> Option<Value> {
    let Value::Record { val, .. } = value else {
        return None;
    };
    val.get(key).cloned()
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
