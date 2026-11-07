// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::stone_ast::{Expr, FunctionDef};

use super::stone_runtime_value::RuntimeValue;
use super::{ContextState, Scope};

/// A variable shared between a function call and the closures it created.
/// Only variables that some closure reads become cells (the scope resolver
/// says which); every other local stays a plain slot. Closures never rebind
/// a cell, but the defining function may assign it after the closure is
/// made, and the closure sees that (Python's late binding). Empty means
/// "not assigned yet".
pub(super) type Cell = Rc<RefCell<Option<RuntimeValue>>>;

#[derive(Clone)]
pub(super) enum CallableValue {
    Lambda {
        function_id: u64,
        params: Vec<String>,
        body: Box<Expr>,
        captures: Vec<(String, Cell)>,
    },
    Named {
        function_id: u64,
        // Shared, so passing a function around does not copy its body.
        function: Rc<FunctionDef>,
        /// Cells for the enclosing-function variables the body reads; empty
        /// for module-level functions.
        captures: Vec<(String, Cell)>,
    },
}

#[derive(Clone)]
pub(super) enum TransitionHookHandlerValue {
    Callable(CallableValue),
    NamedFunction(String),
}

impl TransitionHookHandlerValue {
    pub(super) fn is_session_persistable(&self) -> bool {
        match self {
            Self::Callable(callable) => callable.captures_are_persistable(),
            Self::NamedFunction(_) => true,
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct TransitionHooksValue {
    pub(super) pre: Option<TransitionHookHandlerValue>,
    pub(super) post: Option<TransitionHookHandlerValue>,
}

impl TransitionHooksValue {
    pub(super) fn is_session_persistable(&self) -> bool {
        self.pre
            .iter()
            .chain(self.post.iter())
            .all(TransitionHookHandlerValue::is_session_persistable)
    }
}

#[derive(Clone)]
pub(super) enum WorkflowHandlerValue {
    Callable(CallableValue),
    NamedFunction(String),
}

impl WorkflowHandlerValue {
    pub(super) fn is_session_persistable(&self) -> bool {
        match self {
            Self::Callable(callable) => callable.captures_are_persistable(),
            Self::NamedFunction(_) => true,
        }
    }
}

#[derive(Clone)]
pub(super) struct WorkflowFindingEvidencePredicateValue {
    pub(super) tool: Option<String>,
    pub(super) contains: Vec<String>,
    pub(super) success: Option<bool>,
}

#[derive(Clone)]
pub(super) struct WorkflowFindingSpecValue {
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) question: String,
    pub(super) evidence: Option<WorkflowFindingEvidencePredicateValue>,
}

#[derive(Clone)]
pub(super) enum WorkflowEvidenceSourceValue {
    Handler(WorkflowHandlerValue),
    FileNonempty {
        path: String,
    },
    Artifact {
        path: String,
        format: Option<String>,
        arch: Option<String>,
    },
    FileValid {
        path: String,
        format: Option<String>,
        nonempty: bool,
    },
    CommandSucceeded {
        argv: Vec<String>,
    },
    StdoutNonempty,
    DecisionRecorded {
        fields: Vec<WorkflowFindingSpecValue>,
        resolved: bool,
    },
    All(Vec<WorkflowEvidenceSourceValue>),
}

impl WorkflowEvidenceSourceValue {
    pub(super) fn is_session_persistable(&self) -> bool {
        match self {
            Self::Handler(handler) => handler.is_session_persistable(),
            Self::FileNonempty { .. }
            | Self::Artifact { .. }
            | Self::FileValid { .. }
            | Self::CommandSucceeded { .. }
            | Self::StdoutNonempty
            | Self::DecisionRecorded { .. } => true,
            Self::All(sources) => sources.iter().all(Self::is_session_persistable),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkflowCheckpointPolicyValue {
    None,
    Workspace,
    Forkable,
    Repairable,
    Auto,
}

impl WorkflowCheckpointPolicyValue {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "workspace" => Some(Self::Workspace),
            "forkable" => Some(Self::Forkable),
            "repairable" => Some(Self::Repairable),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Workspace => "workspace",
            Self::Forkable => "forkable",
            Self::Repairable => "repairable",
            Self::Auto => "auto",
        }
    }
}

#[derive(Clone)]
pub(super) struct WorkflowStageInputFieldValue {
    pub(super) name: String,
    pub(super) kind: String,
}

#[derive(Clone)]
pub(super) struct WorkflowStageInputValue {
    pub(super) stage: String,
    pub(super) fields: Option<Vec<WorkflowStageInputFieldValue>>,
}

#[derive(Clone)]
pub(super) struct WorkflowStageMetadataValue {
    goal: String,
    inputs: Vec<WorkflowStageInputValue>,
}

#[derive(Clone)]
pub(super) struct WorkflowStageValue {
    pub(super) name: String,
    pub(super) metadata: Box<WorkflowStageMetadataValue>,
    pub(super) agent_loop: bool,
    pub(super) evidence: WorkflowEvidenceSourceValue,
    pub(super) action: WorkflowHandlerValue,
    pub(super) repair: Option<WorkflowHandlerValue>,
    pub(super) max_attempts: u32,
    pub(super) max_actions: u32,
    pub(super) checkpoint: WorkflowCheckpointPolicyValue,
}

impl WorkflowStageValue {
    pub(super) fn new_metadata(
        goal: String,
        inputs: Vec<WorkflowStageInputValue>,
    ) -> Box<WorkflowStageMetadataValue> {
        Box::new(WorkflowStageMetadataValue { goal, inputs })
    }

    pub(super) fn goal(&self) -> &str {
        &self.metadata.goal
    }

    pub(super) fn inputs(&self) -> &[WorkflowStageInputValue] {
        &self.metadata.inputs
    }

    pub(super) fn is_session_persistable(&self) -> bool {
        self.evidence.is_session_persistable()
            && self.action.is_session_persistable()
            && self
                .repair
                .iter()
                .all(WorkflowHandlerValue::is_session_persistable)
    }
}

#[derive(Clone)]
pub(super) struct WorkflowValue {
    pub(super) name: String,
    pub(super) stages: Vec<WorkflowStageValue>,
    pub(super) patches: Vec<WorkflowPatchValue>,
}

#[derive(Clone)]
pub(super) struct WorkflowPatchValue {
    pub(super) target: String,
    pub(super) replacement: String,
}

impl WorkflowValue {
    pub(super) fn is_session_persistable(&self) -> bool {
        self.stages
            .iter()
            .all(WorkflowStageValue::is_session_persistable)
    }
}

impl CallableValue {
    pub(super) fn lambda(
        function_id: u64,
        params: Vec<String>,
        body: Box<Expr>,
        captures: Vec<(String, Cell)>,
    ) -> Self {
        Self::Lambda {
            function_id,
            params,
            body,
            captures,
        }
    }

    pub(super) fn named(function_id: u64, function: Rc<FunctionDef>) -> Self {
        Self::closure(function_id, function, Vec::new())
    }

    pub(super) fn closure(
        function_id: u64,
        function: Rc<FunctionDef>,
        captures: Vec<(String, Cell)>,
    ) -> Self {
        Self::Named {
            function_id,
            function,
            captures,
        }
    }

    pub(super) fn display_name(&self) -> String {
        match self {
            Self::Lambda { function_id, .. } => format!("lambda#{function_id}"),
            Self::Named {
                function_id,
                function,
                ..
            } => format!("{}#{function_id}", function.name),
        }
    }

    pub(super) fn accepts_arity(&self, arity: usize) -> bool {
        match self {
            Self::Lambda { params, .. } => params.len() == arity,
            Self::Named { function, .. } => {
                let required = function
                    .params
                    .iter()
                    .filter(|param| param.default.is_none())
                    .count();
                (required..=function.params.len()).contains(&arity)
            }
        }
    }

    pub(super) fn captures_are_persistable(&self) -> bool {
        match self {
            Self::Lambda { captures, .. } | Self::Named { captures, .. } => {
                captures.iter().all(|(_, cell)| {
                    cell.borrow()
                        .as_ref()
                        .is_none_or(RuntimeValue::is_session_persistable)
                })
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct StoneSession {
    pub(super) locals: HashMap<String, RuntimeValue>,
    pub(super) functions: HashMap<String, Rc<FunctionDef>>,
    pub(super) context: ContextState,
    pub(super) next_transition_id: u64,
    /// Host-only errors (such as `ExpectationFailed`) this session has caught.
    /// Re-raising one, or `isinstance(x, ExpectationFailed)`, checks the value
    /// against this list, so a program cannot forge one from a plain dict.
    pub(super) host_errors: Vec<nu_protocol::Value>,
}

impl StoneSession {
    pub(crate) fn admission_bound_names(&self) -> Vec<String> {
        let mut names = self
            .locals
            .keys()
            .chain(self.functions.keys())
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }

    pub(super) fn root_scope(&self) -> Scope {
        Scope {
            locals: self.locals.clone(),
            ..Scope::default()
        }
    }

    pub(super) fn update_from_root_scope(&mut self, scope: &Scope) {
        self.locals = scope
            .locals
            .iter()
            .filter(|(_, value)| value.is_session_persistable())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }

    pub(super) fn update_functions(&mut self, functions: &HashMap<String, Rc<FunctionDef>>) {
        self.functions = functions.clone();
    }

    pub(super) fn update_context(&mut self, context: &ContextState) {
        self.context = context.clone();
    }

    pub(super) fn update_transition_id(&mut self, next_transition_id: u64) {
        self.next_transition_id = next_transition_id;
    }

    pub(super) fn update_host_errors(&mut self, host_errors: &[nu_protocol::Value]) {
        self.host_errors = host_errors.to_vec();
    }
}
