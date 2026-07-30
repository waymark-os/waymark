// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use crate::stone_ast::{Expr, FunctionDef};

use super::stone_runtime_value::RuntimeValue;
use super::{ContextState, Scope};

#[derive(Clone)]
pub(super) enum CallableValue {
    Lambda {
        function_id: u64,
        params: Vec<String>,
        body: Box<Expr>,
        captures: Vec<(String, RuntimeValue)>,
    },
    Named {
        function_id: u64,
        function: FunctionDef,
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
pub(super) enum WorkflowEvidenceSourceValue {
    Handler(WorkflowHandlerValue),
    FileNonempty { path: String },
}

impl WorkflowEvidenceSourceValue {
    pub(super) fn is_session_persistable(&self) -> bool {
        match self {
            Self::Handler(handler) => handler.is_session_persistable(),
            Self::FileNonempty { .. } => true,
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
pub(super) struct WorkflowStageValue {
    pub(super) name: String,
    pub(super) evidence: WorkflowEvidenceSourceValue,
    pub(super) action: WorkflowHandlerValue,
    pub(super) repair: Option<WorkflowHandlerValue>,
    pub(super) max_attempts: u32,
    pub(super) checkpoint: WorkflowCheckpointPolicyValue,
}

impl WorkflowStageValue {
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
        captures: Vec<(String, RuntimeValue)>,
    ) -> Self {
        Self::Lambda {
            function_id,
            params,
            body,
            captures,
        }
    }

    pub(super) fn named(function_id: u64, function: FunctionDef) -> Self {
        Self::Named {
            function_id,
            function,
        }
    }

    pub(super) fn display_name(&self) -> String {
        match self {
            Self::Lambda { function_id, .. } => format!("lambda#{function_id}"),
            Self::Named {
                function_id,
                function,
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
            Self::Lambda { captures, .. } => captures
                .iter()
                .all(|(_, value)| value.is_session_persistable()),
            Self::Named { .. } => true,
        }
    }
}

#[derive(Default)]
pub(crate) struct StoneSession {
    pub(super) locals: HashMap<String, RuntimeValue>,
    pub(super) functions: HashMap<String, FunctionDef>,
    pub(super) context: ContextState,
    pub(super) next_transition_id: u64,
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
            files: HashMap::new(),
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

    pub(super) fn update_functions(&mut self, functions: &HashMap<String, FunctionDef>) {
        self.functions = functions.clone();
    }

    pub(super) fn update_context(&mut self, context: &ContextState) {
        self.context = context.clone();
    }

    pub(super) fn update_transition_id(&mut self, next_transition_id: u64) {
        self.next_transition_id = next_transition_id;
    }
}
