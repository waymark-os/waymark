// SPDX-License-Identifier: MIT OR Apache-2.0

use std::sync::{Arc, Mutex, MutexGuard};

use nu_protocol::ShellError;

use crate::stone_eval::stone_error;

#[derive(Clone)]
pub(super) struct AttemptScopeValue {
    pub(super) scope_id: u64,
    inner: Arc<Mutex<AttemptScopeState>>,
}

#[derive(Clone, Debug)]
pub(super) struct AttemptScopeChild {
    pub(super) attempt: String,
    pub(super) joined: bool,
    pub(super) resolved: bool,
}

#[derive(Debug)]
pub(super) struct AttemptScopeState {
    pub(super) exit_policy: String,
    pub(super) join_timeout_ms: u32,
    pub(super) children: Vec<AttemptScopeChild>,
    pub(super) closed: bool,
}

impl AttemptScopeValue {
    pub(super) fn new(scope_id: u64, exit_policy: String, join_timeout_ms: u32) -> Self {
        Self {
            scope_id,
            inner: Arc::new(Mutex::new(AttemptScopeState {
                exit_policy,
                join_timeout_ms,
                children: Vec::new(),
                closed: false,
            })),
        }
    }

    pub(super) fn lock(&self) -> Result<MutexGuard<'_, AttemptScopeState>, ShellError> {
        self.inner
            .lock()
            .map_err(|_| stone_error("attempt scope", "attempt scope state is unavailable"))
    }

    pub(super) fn register(&self, attempt: String) -> Result<(), ShellError> {
        let mut state = self.lock()?;
        if state.closed {
            return Err(stone_error(
                "attempt scope",
                format!("attempt scope #{} is already closed", self.scope_id),
            ));
        }
        if !state.children.iter().any(|child| child.attempt == attempt) {
            state.children.push(AttemptScopeChild {
                attempt,
                joined: false,
                resolved: false,
            });
        }
        Ok(())
    }

    pub(super) fn mark_joined(&self, attempt: &str) -> Result<(), ShellError> {
        let mut state = self.lock()?;
        if let Some(child) = state
            .children
            .iter_mut()
            .find(|child| child.attempt == attempt)
        {
            child.joined = true;
        }
        Ok(())
    }

    pub(super) fn mark_resolved(&self, attempt: &str) -> Result<(), ShellError> {
        let mut state = self.lock()?;
        if let Some(child) = state
            .children
            .iter_mut()
            .find(|child| child.attempt == attempt)
        {
            child.resolved = true;
        }
        Ok(())
    }

    pub(super) fn owns_joined(&self, attempt: &str) -> Result<bool, ShellError> {
        let state = self.lock()?;
        Ok(state
            .children
            .iter()
            .any(|child| child.attempt == attempt && child.joined && !child.resolved))
    }
}
