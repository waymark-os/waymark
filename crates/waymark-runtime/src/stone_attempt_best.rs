// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::{Arc, Mutex, MutexGuard};

use nu_protocol::{Record, ShellError, Span, Value};

use crate::stone_attempt_scope::AttemptScopeValue;
use crate::stone_attempt_value::AttemptOutcomeValue;
use crate::stone_eval::stone_error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttemptBestObjective {
    Max,
    Min,
}

impl AttemptBestObjective {
    pub(super) fn parse(value: &str) -> Result<Self, ShellError> {
        match value {
            "max" => Ok(Self::Max),
            "min" => Ok(Self::Min),
            _ => Err(stone_error(
                "attempt_best",
                "objective must be either \"max\" or \"min\"",
            )),
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Min => "min",
        }
    }

    fn prefers(self, candidate: f64, current: f64) -> bool {
        match self {
            Self::Max => candidate > current,
            Self::Min => candidate < current,
        }
    }
}

#[derive(Clone)]
pub(super) struct AttemptBestCandidate {
    pub(super) outcome: AttemptOutcomeValue,
    pub(super) score: f64,
    pub(super) summary: String,
    pub(super) evidence: Vec<String>,
    pub(super) artifacts: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptBestLifecycle {
    Open,
    Accepted,
    Discarded,
}

impl AttemptBestLifecycle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Accepted => "accepted",
            Self::Discarded => "discarded",
        }
    }
}

struct AttemptBestState {
    objective: AttemptBestObjective,
    considered: u64,
    replacements: u64,
    lifecycle: AttemptBestLifecycle,
    current: Option<AttemptBestCandidate>,
    final_attempt: Option<String>,
    final_score: Option<f64>,
}

#[derive(Clone)]
pub(super) struct AttemptBestValue {
    pub(super) selection_id: u64,
    pub(super) scope: AttemptScopeValue,
    inner: Arc<Mutex<AttemptBestState>>,
}

#[derive(Clone)]
pub(super) enum AttemptBestPlan {
    Retain,
    Replace(AttemptBestCandidate),
    Reject(AttemptBestCandidate),
}

impl AttemptBestValue {
    pub(super) fn new(
        selection_id: u64,
        scope: AttemptScopeValue,
        objective: AttemptBestObjective,
    ) -> Self {
        Self {
            selection_id,
            scope,
            inner: Arc::new(Mutex::new(AttemptBestState {
                objective,
                considered: 0,
                replacements: 0,
                lifecycle: AttemptBestLifecycle::Open,
                current: None,
                final_attempt: None,
                final_score: None,
            })),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, AttemptBestState>, ShellError> {
        self.inner
            .lock()
            .map_err(|_| stone_error("attempt_best", "best-candidate state is unavailable"))
    }

    pub(super) fn plan(&self, score: f64) -> Result<AttemptBestPlan, ShellError> {
        let state = self.lock()?;
        if state.lifecycle != AttemptBestLifecycle::Open {
            return Err(stone_error(
                "attempt_best_consider",
                format!(
                    "attempt_best #{} is already {}",
                    self.selection_id,
                    state.lifecycle.as_str()
                ),
            ));
        }
        let Some(current) = state.current.clone() else {
            return Ok(AttemptBestPlan::Retain);
        };
        if state.objective.prefers(score, current.score) {
            Ok(AttemptBestPlan::Replace(current))
        } else {
            Ok(AttemptBestPlan::Reject(current))
        }
    }

    pub(super) fn retain(
        &self,
        candidate: AttemptBestCandidate,
        replaced: bool,
    ) -> Result<(), ShellError> {
        let mut state = self.lock()?;
        if state.lifecycle != AttemptBestLifecycle::Open {
            return Err(stone_error(
                "attempt_best_consider",
                "cannot retain a candidate after selection has closed",
            ));
        }
        state.considered = state.considered.saturating_add(1);
        if replaced {
            state.replacements = state.replacements.saturating_add(1);
        }
        state.current = Some(candidate);
        Ok(())
    }

    pub(super) fn record_rejected(&self) -> Result<(), ShellError> {
        let mut state = self.lock()?;
        if state.lifecycle != AttemptBestLifecycle::Open {
            return Err(stone_error(
                "attempt_best_consider",
                "cannot reject a candidate after selection has closed",
            ));
        }
        state.considered = state.considered.saturating_add(1);
        Ok(())
    }

    pub(super) fn current(&self, context: &str) -> Result<AttemptBestCandidate, ShellError> {
        let state = self.lock()?;
        if state.lifecycle != AttemptBestLifecycle::Open {
            return Err(stone_error(
                context,
                format!(
                    "attempt_best #{} is already {}",
                    self.selection_id,
                    state.lifecycle.as_str()
                ),
            ));
        }
        state.current.clone().ok_or_else(|| {
            stone_error(
                context,
                format!(
                    "attempt_best #{} has no retained candidate",
                    self.selection_id
                ),
            )
        })
    }

    pub(super) fn current_optional(&self) -> Result<Option<AttemptBestCandidate>, ShellError> {
        Ok(self.lock()?.current.clone())
    }

    pub(super) fn mark_accepted(&self, candidate: &AttemptBestCandidate) -> Result<(), ShellError> {
        self.finalize(candidate, AttemptBestLifecycle::Accepted)
    }

    pub(super) fn mark_discarded(
        &self,
        candidate: Option<&AttemptBestCandidate>,
    ) -> Result<(), ShellError> {
        let mut state = self.lock()?;
        if state.lifecycle == AttemptBestLifecycle::Accepted {
            return Err(stone_error(
                "attempt_best_discard",
                "cannot discard an already accepted selection",
            ));
        }
        if state.lifecycle == AttemptBestLifecycle::Discarded {
            return Ok(());
        }
        if let Some(candidate) = candidate {
            state.final_attempt = Some(candidate.outcome.attempt.clone());
            state.final_score = Some(candidate.score);
        }
        state.current = None;
        state.lifecycle = AttemptBestLifecycle::Discarded;
        Ok(())
    }

    fn finalize(
        &self,
        candidate: &AttemptBestCandidate,
        lifecycle: AttemptBestLifecycle,
    ) -> Result<(), ShellError> {
        let mut state = self.lock()?;
        if state.lifecycle != AttemptBestLifecycle::Open {
            return Err(stone_error(
                "attempt_best_accept",
                format!(
                    "attempt_best #{} is already {}",
                    self.selection_id,
                    state.lifecycle.as_str()
                ),
            ));
        }
        state.final_attempt = Some(candidate.outcome.attempt.clone());
        state.final_score = Some(candidate.score);
        // Drop the full outcome, evidence, and artifact vectors once ownership
        // has transferred. A closed selector retains only bounded diagnostics.
        state.current = None;
        state.lifecycle = lifecycle;
        Ok(())
    }

    pub(super) fn typed_outcome(&self) -> Result<AttemptOutcomeValue, ShellError> {
        Ok(self.current("attempt_best outcome")?.outcome)
    }

    pub(super) fn attribute(&self, attr: &str) -> Result<Value, ShellError> {
        let state = self.lock()?;
        let span = Span::unknown();
        let scope_closed = self.scope.lock()?.closed;
        let status = if scope_closed && state.lifecycle == AttemptBestLifecycle::Open {
            "scope_closed"
        } else if state.lifecycle == AttemptBestLifecycle::Open && state.current.is_some() {
            "retaining"
        } else {
            state.lifecycle.as_str()
        };
        let current = state.current.as_ref();
        match attr {
            "type" => Ok(Value::string("attempt_best", span)),
            "id" => Ok(Value::int(self.selection_id as i64, span)),
            "scope_id" => Ok(Value::int(self.scope.scope_id as i64, span)),
            "objective" => Ok(Value::string(state.objective.as_str(), span)),
            "status" => Ok(Value::string(status, span)),
            "empty" => Ok(Value::bool(current.is_none(), span)),
            "considered" => Ok(Value::int(state.considered as i64, span)),
            "replacements" => Ok(Value::int(state.replacements as i64, span)),
            "attempt" => Ok(current
                .map(|candidate| Value::string(candidate.outcome.attempt.clone(), span))
                .or_else(|| {
                    state
                        .final_attempt
                        .as_ref()
                        .map(|attempt| Value::string(attempt.clone(), span))
                })
                .unwrap_or_else(|| Value::nothing(span))),
            "score" => Ok(current
                .map(|candidate| Value::float(candidate.score, span))
                .or_else(|| state.final_score.map(|score| Value::float(score, span)))
                .unwrap_or_else(|| Value::nothing(span))),
            "summary" => Ok(current
                .map(|candidate| Value::string(candidate.summary.clone(), span))
                .unwrap_or_else(|| Value::nothing(span))),
            "evidence" => Ok(Value::list(
                current
                    .map(|candidate| {
                        candidate
                            .evidence
                            .iter()
                            .map(|item| Value::string(item.clone(), span))
                            .collect()
                    })
                    .unwrap_or_default(),
                span,
            )),
            "artifacts" => Ok(Value::list(
                current
                    .map(|candidate| {
                        candidate
                            .artifacts
                            .iter()
                            .map(|item| Value::string(item.clone(), span))
                            .collect()
                    })
                    .unwrap_or_default(),
                span,
            )),
            "outcome" => Err(stone_error(
                "attribute",
                "attempt_best.outcome is a typed value and must be read through the evaluator",
            )),
            _ => Err(stone_error(
                "attribute",
                format!("attempt_best has no attribute `{attr}`"),
            )),
        }
    }

    pub(super) fn decision_record(
        &self,
        candidate: &AttemptBestCandidate,
        selected: bool,
        replaced_attempt: Option<&str>,
        discarded_attempt: Option<&str>,
    ) -> Result<Value, ShellError> {
        let state = self.lock()?;
        let span = Span::unknown();
        let mut record = Record::new();
        record.push("type", Value::string("attempt_best_decision", span));
        record.push("selected", Value::bool(selected, span));
        record.push(
            "attempt",
            Value::string(candidate.outcome.attempt.clone(), span),
        );
        record.push("score", Value::float(candidate.score, span));
        record.push(
            "replaced_attempt",
            replaced_attempt
                .map(|attempt| Value::string(attempt, span))
                .unwrap_or_else(|| Value::nothing(span)),
        );
        record.push(
            "discarded_attempt",
            discarded_attempt
                .map(|attempt| Value::string(attempt, span))
                .unwrap_or_else(|| Value::nothing(span)),
        );
        record.push("considered", Value::int(state.considered as i64, span));
        record.push("replacements", Value::int(state.replacements as i64, span));
        if let Some(best) = state.current.as_ref() {
            record.push(
                "best_attempt",
                Value::string(best.outcome.attempt.clone(), span),
            );
            record.push("best_score", Value::float(best.score, span));
        }
        Ok(Value::record(record, span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(attempt: &str) -> AttemptOutcomeValue {
        AttemptOutcomeValue {
            attempt: attempt.to_string(),
            joined: true,
            timed_out: false,
            state: "active".to_string(),
            controller_state: "exited".to_string(),
            record: Value::record(Record::new(), Span::unknown()),
        }
    }

    fn candidate(attempt: &str, score: f64) -> AttemptBestCandidate {
        AttemptBestCandidate {
            outcome: outcome(attempt),
            score,
            summary: attempt.to_string(),
            evidence: vec![format!("evidence:{attempt}")],
            artifacts: vec![format!("artifact:{attempt}")],
        }
    }

    #[test]
    fn selector_retains_only_the_best_candidate() {
        let scope = AttemptScopeValue::new(7, "cancel_then_join".to_string(), 1000);
        let best = AttemptBestValue::new(9, scope, AttemptBestObjective::Max);

        assert!(matches!(best.plan(0.5).unwrap(), AttemptBestPlan::Retain));
        best.retain(candidate("first", 0.5), false).unwrap();

        let AttemptBestPlan::Reject(current) = best.plan(0.4).unwrap() else {
            panic!("lower score should be rejected");
        };
        assert_eq!(current.outcome.attempt, "first");
        best.record_rejected().unwrap();

        let AttemptBestPlan::Replace(current) = best.plan(0.8).unwrap() else {
            panic!("higher score should replace the current candidate");
        };
        assert_eq!(current.outcome.attempt, "first");
        best.retain(candidate("third", 0.8), true).unwrap();

        assert_eq!(
            best.attribute("attempt").unwrap().as_str().unwrap(),
            "third"
        );
        assert_eq!(best.attribute("considered").unwrap().as_int().unwrap(), 3);
        assert_eq!(best.attribute("replacements").unwrap().as_int().unwrap(), 1);
        assert_eq!(best.typed_outcome().unwrap().attempt, "third");
    }

    #[test]
    fn minimizing_selector_and_finalization_are_bounded() {
        let scope = AttemptScopeValue::new(1, "cancel_then_join".to_string(), 1000);
        let best = AttemptBestValue::new(2, scope, AttemptBestObjective::Min);
        best.retain(candidate("small", 12.0), false).unwrap();
        assert!(matches!(
            best.plan(10.0).unwrap(),
            AttemptBestPlan::Replace(_)
        ));
        let selected = best.current("test").unwrap();
        best.mark_accepted(&selected).unwrap();

        assert_eq!(
            best.attribute("status").unwrap().as_str().unwrap(),
            "accepted"
        );
        assert_eq!(
            best.attribute("attempt").unwrap().as_str().unwrap(),
            "small"
        );
        assert!(best.typed_outcome().is_err());
        assert!(best.plan(9.0).is_err());
    }
}
