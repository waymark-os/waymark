// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pre-run warning: a function receives evidence but none of its model calls
//! see it (design 5.9, item 1).
//!
//! This is the most common defect Lean4Agent found in SWE workflows: a
//! verify step that starts a fresh conversation and never shows the model
//! the fix it is meant to judge. In Stone the data flow is in the source, so
//! the check is one linear pass per function:
//!
//! - a parameter annotated with a record type (`fix: Fix`, `result:
//!   RunResult`) is the evidence. A `list[Fix]` parameter is not: function
//!   parameters keep only `list`, and a list may be a conversation;
//! - values computed from it (assignments, string formatting, `message(...)`,
//!   appending to a list) carry it;
//! - it is seen when a value carrying it is an argument of a `.ask(...)` call.
//!
//! The check stays quiet whenever it cannot see what happens to the value:
//! passing it to any other function (a user helper, `run_tests`, `emit`),
//! returning it, raising it, or testing it in a condition all count as using
//! it. It also skips a function whose model call continues a conversation
//! handed in by the caller (an unannotated, list, or dict parameter), since
//! the evidence may already be in that history. So a warning means the value
//! is unused, or used only to print or to build values the model never sees.

use std::collections::HashSet;

use crate::stone_ast::{
    AssignTarget, ComprehensionClause, Expr, FormattedStringPart, FunctionDef, FunctionParam,
    ImportKind, Stmt, StoneType, TypeExpr,
};

/// Calls whose result carries nothing from their arguments.
const DISPLAY_CALLS: &[&str] = &["print", "log"];

/// Builtins whose result is computed from their arguments and that do
/// nothing else with them.
const TRANSPARENT_CALLS: &[&str] = &[
    "str",
    "repr",
    "len",
    "int",
    "float",
    "bool",
    "list",
    "dict",
    "tuple",
    "set",
    "sorted",
    "reversed",
    "enumerate",
    "zip",
    "min",
    "max",
    "sum",
    "any",
    "all",
    "json_dumps",
    "message",
];

/// Warnings for functions in `statements` (and functions nested in them)
/// that receive a record but never show it to their model calls.
pub(crate) fn unseen_evidence_warnings(statements: &[Stmt]) -> Vec<String> {
    let mut records: HashSet<String> = crate::stone_record_access::HOST_RECORD_TYPES
        .iter()
        .map(|name| name.to_string())
        .collect();
    collect_record_names(statements, &mut records);
    let mut warnings = Vec::new();
    visit_functions(statements, &records, &mut warnings);
    warnings
}

fn collect_record_names(statements: &[Stmt], records: &mut HashSet<String>) {
    for statement in statements {
        match statement {
            Stmt::RecordDef(record) => {
                records.insert(record.name.clone());
            }
            Stmt::Module(module) => collect_record_names(&module.body, records),
            Stmt::Import { names, .. } => {
                for name in names {
                    if name.kind == ImportKind::Record {
                        records.insert(name.local.clone());
                    }
                }
            }
            _ => {}
        }
    }
}

fn visit_functions(statements: &[Stmt], records: &HashSet<String>, warnings: &mut Vec<String>) {
    for statement in statements {
        match statement {
            Stmt::FunctionDef(function) => {
                check_function(function, records, warnings);
                visit_functions(&function.body, records, warnings);
            }
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                visit_functions(then_branch, records, warnings);
                visit_functions(else_branch, records, warnings);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::With { body, .. } => {
                visit_functions(body, records, warnings)
            }
            Stmt::Try { body, handlers } => {
                visit_functions(body, records, warnings);
                for handler in handlers {
                    visit_functions(&handler.body, records, warnings);
                }
            }
            _ => {}
        }
    }
}

/// The record type a parameter is annotated with, if it is evidence.
fn evidence_type(param: &FunctionParam, records: &HashSet<String>) -> Option<String> {
    match param.check.as_ref()? {
        TypeExpr::Named(name) if records.contains(name) => Some(name.clone()),
        _ => None,
    }
}

/// A parameter that may hold a conversation the caller started: no
/// annotation, a list, or a dict.
fn may_hold_history(param: &FunctionParam, records: &HashSet<String>) -> bool {
    if evidence_type(param, records).is_some() {
        return false;
    }
    match &param.check {
        None => matches!(param.ty, StoneType::Any | StoneType::List | StoneType::Record),
        Some(TypeExpr::List(_) | TypeExpr::Dict(_) | TypeExpr::Any) => true,
        Some(_) => false,
    }
}

fn check_function(function: &FunctionDef, records: &HashSet<String>, warnings: &mut Vec<String>) {
    let asks = Flow::run(function, HashSet::new()).asks;
    if asks == 0 {
        return;
    }
    let continues_history = function
        .params
        .iter()
        .filter(|param| may_hold_history(param, records))
        .any(|param| Flow::run(function, HashSet::from([param.name.clone()])).seen);
    if continues_history {
        return;
    }
    for param in &function.params {
        if param.name.starts_with('_') {
            continue;
        }
        let Some(ty) = evidence_type(param, records) else {
            continue;
        };
        let flow = Flow::run(function, HashSet::from([param.name.clone()]));
        if !flow.seen && !flow.escaped {
            let name = &param.name;
            let calls = if asks == 1 {
                "its model call never sees it".to_string()
            } else {
                format!("none of its {asks} model calls sees it")
            };
            warnings.push(format!(
                "{}() receives {name}: {ty}, but {calls}. A model step that needs this evidence cannot judge without it: pass it in, as in model.ask(..., inputs={{\"{name}\": {name}}}), or remove the parameter if the step does not need it.",
                function.name
            ));
        }
    }
}

/// Where values computed from one parameter go, within one function body.
struct Flow {
    /// Names whose value may carry the parameter.
    carriers: HashSet<String>,
    /// A model call received a carrier.
    seen: bool,
    /// A carrier went somewhere this pass does not follow.
    escaped: bool,
    /// `.ask(...)` calls in the body.
    asks: usize,
}

impl Flow {
    fn run(function: &FunctionDef, carriers: HashSet<String>) -> Flow {
        let mut flow = Flow {
            carriers,
            seen: false,
            escaped: false,
            asks: 0,
        };
        // Flow-insensitive: repeat until no new name carries the value, so a
        // loop that appends to a list declared above it is covered.
        loop {
            let before = flow.carriers.len();
            flow.asks = 0;
            flow.block(&function.body);
            if flow.carriers.len() == before || flow.seen || flow.escaped {
                return flow;
            }
        }
    }

    fn block(&mut self, statements: &[Stmt]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Assign { target, value } | Stmt::AugAssign { target, value, .. } => {
                let carries = self.expr(value);
                self.target(target, carries);
            }
            Stmt::For {
                targets,
                iter,
                body,
            } => {
                if self.expr(iter) {
                    self.carriers.extend(targets.iter().cloned());
                }
                self.block(body);
            }
            Stmt::While { condition, body } => {
                self.used(condition);
                self.block(body);
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.used(condition);
                self.block(then_branch);
                self.block(else_branch);
            }
            Stmt::With {
                target,
                context,
                body,
                ..
            } => {
                if self.expr(context) {
                    self.carriers.extend(target.iter().cloned());
                }
                self.block(body);
            }
            Stmt::Try { body, handlers } => {
                self.block(body);
                for handler in handlers {
                    self.block(&handler.body);
                }
            }
            // A nested function reads the enclosing names; follow its body
            // as if it ran here.
            Stmt::FunctionDef(function) => self.block(&function.body),
            Stmt::Raise(Some(value)) | Stmt::Return(Some(value)) => self.used(value),
            Stmt::Expr(value) => {
                self.expr(value);
            }
            _ => {}
        }
    }

    /// A value that leaves the function or decides control flow.
    fn used(&mut self, expr: &Expr) {
        if self.expr(expr) {
            self.escaped = true;
        }
    }

    fn target(&mut self, target: &AssignTarget, carries: bool) {
        match target {
            AssignTarget::Name(name) => {
                if carries {
                    self.carriers.insert(name.clone());
                }
            }
            AssignTarget::Tuple(names) => {
                if carries {
                    self.carriers.extend(names.iter().cloned());
                }
            }
            AssignTarget::Unpack(targets) => {
                for target in targets {
                    self.target(target, carries);
                }
            }
            AssignTarget::Subscript { value, index } => {
                self.expr(index);
                self.target(value, carries);
            }
            AssignTarget::Attribute { value, .. } => self.target(value, carries),
            // `d.setdefault(k, [])[0] = v` changes the object `d` holds.
            AssignTarget::Expr(base) => {
                self.expr(base);
                if carries {
                    let mut base = base;
                    loop {
                        match base {
                            Expr::Name(name) => {
                                self.carriers.insert(name.clone());
                                break;
                            }
                            Expr::Attribute { value, .. }
                            | Expr::Subscript { value, .. }
                            | Expr::MethodCall {
                                receiver: value, ..
                            } => base = value,
                            _ => break,
                        }
                    }
                }
            }
        }
    }

    fn all(&mut self, exprs: &[Expr]) -> bool {
        let mut carries = false;
        for expr in exprs {
            carries |= self.expr(expr);
        }
        carries
    }

    fn named(&mut self, named: &[(String, Expr)]) -> bool {
        let mut carries = false;
        for (_, expr) in named {
            carries |= self.expr(expr);
        }
        carries
    }

    fn clauses(&mut self, clauses: &[ComprehensionClause]) -> bool {
        let mut carries = false;
        for clause in clauses {
            if self.expr(&clause.iter) {
                carries = true;
                self.carriers.extend(clause.targets.iter().cloned());
            }
            carries |= self.all(&clause.filters);
        }
        carries
    }

    /// Whether the value of `expr` may carry the parameter. Records model
    /// calls that see it and other calls it escapes into.
    fn expr(&mut self, expr: &Expr) -> bool {
        match expr {
            Expr::None | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => false,
            Expr::Slice { lower, upper, step } => {
                let mut carries = false;
                for part in [lower, upper, step].into_iter().flatten() {
                    carries |= self.expr(part);
                }
                carries
            }
            Expr::Name(name) => self.carriers.contains(name),
            Expr::FormattedString(parts) => {
                let mut carries = false;
                for part in parts {
                    match part {
                        FormattedStringPart::Literal(_) => {}
                        FormattedStringPart::Expr(expr)
                        | FormattedStringPart::Formatted { expr, .. } => {
                            carries |= self.expr(expr);
                        }
                    }
                }
                carries
            }
            Expr::List(items) | Expr::Tuple(items) => self.all(items),
            Expr::Record(entries) => self.named(entries),
            Expr::ListComprehension { elt, clauses } | Expr::Generator { elt, clauses } => {
                let from_clauses = self.clauses(clauses);
                self.expr(elt) | from_clauses
            }
            Expr::DictComprehension {
                key,
                value,
                clauses,
            } => {
                let from_clauses = self.clauses(clauses);
                self.expr(key) | self.expr(value) | from_clauses
            }
            Expr::Subscript { value, index } => self.expr(value) | self.expr(index),
            Expr::Attribute { value, .. } => self.expr(value),
            Expr::Compare {
                left, comparators, ..
            } => self.expr(left) | self.all(comparators),
            Expr::BoolOp { values, .. } => self.all(values),
            Expr::Conditional {
                then_expr,
                condition,
                else_expr,
            } => {
                self.used(condition);
                self.expr(then_expr) | self.expr(else_expr)
            }
            Expr::Not(inner)
            | Expr::Neg(inner)
            | Expr::Invert(inner)
            | Expr::Await(inner) => self.expr(inner),
            Expr::Add { left, right }
            | Expr::Sub { left, right }
            | Expr::Mul { left, right }
            | Expr::Div { left, right }
            | Expr::FloorDiv { left, right }
            | Expr::Mod { left, right }
            | Expr::BitAnd { left, right }
            | Expr::BitOr { left, right }
            | Expr::BitXor { left, right }
            | Expr::LShift { left, right }
            | Expr::RShift { left, right } => self.expr(left) | self.expr(right),
            // A lambda body runs later, wherever the lambda goes.
            Expr::Lambda { body, .. } => {
                if self.expr(body) {
                    self.escaped = true;
                }
                false
            }
            Expr::MethodCall {
                receiver,
                method,
                positional,
                named,
            } => {
                let receiver_carries = self.expr(receiver);
                let args = self.all(positional) | self.named(named);
                if method == "ask" {
                    self.asks += 1;
                    if args {
                        self.seen = true;
                    }
                    return args;
                }
                // `history.append(message(..., inputs={"fix": fix}))`: the
                // list now carries the value.
                if args {
                    if let Expr::Name(name) = receiver.as_ref() {
                        self.carriers.insert(name.clone());
                    }
                }
                receiver_carries | args
            }
            Expr::Call(call) => {
                let args = self.all(&call.positional) | self.named(&call.named);
                let name = call.name.as_str();
                if DISPLAY_CALLS.contains(&name) {
                    false
                } else if TRANSPARENT_CALLS.contains(&name)
                    || name.starts_with(|c: char| c.is_ascii_uppercase())
                {
                    args
                } else {
                    // A user function, a check (`run_tests`, `verify`), or
                    // `emit`: what happens inside is not followed here.
                    if args {
                        self.escaped = true;
                    }
                    args
                }
            }
            Expr::CallValue {
                callee,
                positional,
                named,
            } => {
                let args = self.expr(callee) | self.all(positional) | self.named(named);
                if args {
                    self.escaped = true;
                }
                args
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stone_ast::lower_source;

    fn warnings(source: &str) -> Vec<String> {
        let program = lower_source(source).expect("program lowers");
        unseen_evidence_warnings(&program.statements)
    }

    const TYPES: &str = "class Fix(Record):\n    files: list[str]\n    why: str\n\nclass Verdict(Record):\n    ok: bool\n\n";

    #[test]
    fn warns_when_a_model_step_never_sees_its_record_parameter() {
        let found = warnings(&format!(
            "{TYPES}def review(issue: str, fix: Fix) -> Verdict:\n    print(fix.why)\n    return model.ask(f\"Is this issue fixed? {{issue}}\", output=Verdict)\n"
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("review() receives fix: Fix, but its model call never sees it."), "{}", found[0]);
        assert!(found[0].contains("inputs={\"fix\": fix}"), "{}", found[0]);

        // Runtime results are evidence too; the count of model calls is
        // named.
        let found = warnings(
            "def judge(result: RunResult, notes: list[str]):\n    a = model.ask(\"first\")\n    return model.ask(\"second\")\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("result: RunResult, but none of its 2 model calls sees it"), "{}", found[0]);

        // A value computed from the evidence that the model never sees.
        let found = warnings(&format!(
            "{TYPES}def review(fix: Fix):\n    summary = \", \".join(fix.files)\n    log(summary)\n    return model.ask(\"Is it fixed?\", output=Verdict)\n"
        ));
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn quiet_when_the_evidence_reaches_the_model() {
        let seen = [
            // named input
            "def review(fix: Fix):\n    return model.ask(\"Fixed?\", output=Verdict, inputs={\"fix\": fix})\n",
            // formatted into the prompt through a variable
            "def review(fix: Fix):\n    prompt = f\"Files: {fix.files}\"\n    prompt += \"\\nFixed?\"\n    return model.ask(prompt, output=Verdict)\n",
            // through a message appended to a history inside a loop
            "def review(fix: Fix):\n    history = [message(\"Review this.\")]\n    for f in fix.files:\n        history.append(message(\"File\", inputs={\"f\": f}))\n    return model.ask(history, output=Verdict)\n",
            // through a dict and json_dumps
            "def review(fix: Fix):\n    data = {\"why\": fix.why}\n    return model.ask(\"Fixed? \" + json_dumps(data), output=Verdict)\n",
            // through a comprehension
            "def review(fix: Fix):\n    lines = [f\"- {f}\" for f in fix.files]\n    return model.ask(\"\\n\".join(lines), output=Verdict)\n",
        ];
        for body in seen {
            let source = format!("{TYPES}{body}");
            assert!(warnings(&source).is_empty(), "{source}");
        }
    }

    #[test]
    fn quiet_when_it_cannot_follow_the_evidence() {
        let quiet = [
            // used by a check, a helper, or returned
            "def review(fix: Fix):\n    r = run_tests(fix.files)\n    return model.ask(\"ok?\", output=Verdict, inputs={\"r\": r})\n",
            "def review(fix: Fix):\n    helper(fix)\n    return model.ask(\"ok?\", output=Verdict)\n",
            "def review(fix: Fix):\n    v = model.ask(\"ok?\", output=Verdict)\n    return [v, fix]\n",
            "def review(fix: Fix):\n    if not fix.files:\n        return None\n    return model.ask(\"ok?\", output=Verdict)\n",
            // the conversation comes from the caller and may hold the evidence
            "def review(history, fix: Fix):\n    return model.ask(history, output=Verdict)\n",
            "def review(history: list, fix: Fix):\n    return model.ask(history + [message(\"ok?\")], output=Verdict)\n",
            // no model call, a scalar, or a name marked unused
            "def review(fix: Fix):\n    print(fix)\n",
            "def review(issue: str):\n    return model.ask(\"ok?\", output=Verdict)\n",
            "def review(_fix: Fix):\n    return model.ask(\"ok?\", output=Verdict)\n",
        ];
        for body in quiet {
            let source = format!("{TYPES}{body}");
            assert!(warnings(&source).is_empty(), "{source}");
        }
    }

    #[test]
    fn checks_nested_functions_and_functions_in_blocks() {
        let found = warnings(&format!(
            "{TYPES}def outer(task: str):\n    def inner(fix: Fix):\n        return model.ask(task, output=Verdict)\n    return inner\n"
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("inner() receives fix: Fix"), "{found:?}");
    }
}
