// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;

use crate::stone_ast::{Expr, FunctionDef};

use super::stone_runtime_value::RuntimeValue;
use super::Scope;

#[derive(Clone)]
pub(super) struct CallableValue {
    pub(super) function_id: u64,
    pub(super) params: Vec<String>,
    pub(super) body: Box<Expr>,
    pub(super) captures: Vec<(String, RuntimeValue)>,
}

#[derive(Default)]
pub(crate) struct StoneSession {
    pub(super) locals: HashMap<String, RuntimeValue>,
    pub(super) functions: HashMap<String, FunctionDef>,
}

impl StoneSession {
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
}
