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

/// Builtins whose result is evidence about the world: what a command or a
/// test run did. `must_run` is left out (it raises when the command fails),
/// and so is any call with `expect=` (the runtime checks it).
const EVIDENCE_CALLS: &[&str] = &["run", "run_wait", "run_complete", "run_tests"];

/// Warnings for evidence a program gets and never reads: a variable bound
/// to a command result, a test run, or a model answer that nothing reads
/// afterwards. No model call sees it, no condition or other call uses it,
/// and it is not printed, emitted, or returned.
///
/// Each function body is checked on its own. The top level is checked only
/// when it makes a model call: in a session an agent may bind `r = run(...)`
/// in one call and read `r` in the next, and such snippets rarely call a
/// model, while a workflow does. The top level's last statement is the
/// program's result, so it is read.
pub(crate) fn unread_evidence_warnings(statements: &[Stmt]) -> Vec<String> {
    let mut warnings = Vec::new();
    let top_level = Flow::over(statements, HashSet::new(), true).asks > 0;
    let (checked, result) = match statements.split_last() {
        Some((Stmt::Expr(result), rest)) => (rest, Some(result)),
        _ => (statements, None),
    };
    check_scope(statements, checked, result, top_level, &mut warnings);
    warnings
}

/// Check the evidence bound in `checked` (`body`, or `body` without its
/// final result expression) against every read in `body`; `result` is read.
/// With `report` false, only the functions inside are checked.
fn check_scope(
    body: &[Stmt],
    checked: &[Stmt],
    result: Option<&Expr>,
    report: bool,
    warnings: &mut Vec<String>,
) {
    let mut bound: Vec<(String, String, bool)> = Vec::new();
    let mut found = Vec::new();
    collect_evidence_bindings(checked, &mut bound, &mut found, warnings);
    if !report {
        return;
    }
    warnings.extend(found);
    let mut reported = HashSet::new();
    for (name, call, is_answer) in bound {
        if name.starts_with('_') || !reported.insert(name.clone()) {
            continue;
        }
        let mut flow = Flow::over(body, HashSet::from([name.clone()]), true);
        if let Some(result) = result {
            flow.used(result);
        }
        if flow.seen || flow.escaped {
            continue;
        }
        warnings.push(if is_answer {
            format!(
                "`{name}` holds the answer of {call}, but nothing reads it: no model call sees it, and no check, print, or return uses it. Use the answer (pass it on with inputs={{\"{name}\": {name}}}, or check it), or drop the call."
            )
        } else {
            format!(
                "`{name}` holds the result of {call}, but nothing reads it: no model call sees it, and no check, print, or return uses it. Show it to the step that judges it (inputs={{\"{name}\": {name}}}), or check it (`if not {name}.ok: ...`)."
            )
        });
    }
}

/// Names bound to evidence in one scope (nested blocks, not nested
/// functions, which are scopes of their own and are checked here).
fn collect_evidence_bindings(
    statements: &[Stmt],
    bound: &mut Vec<(String, String, bool)>,
    found: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for statement in statements {
        match statement {
            Stmt::Assign {
                target: AssignTarget::Name(name),
                value,
            } => {
                if let Some((call, is_answer)) = evidence_call(value) {
                    bound.push((name.clone(), call, is_answer));
                }
            }
            Stmt::Expr(value) => {
                // A test run whose result is thrown away.
                let value = match value {
                    Expr::Await(inner) => inner.as_ref(),
                    other => other,
                };
                if let Expr::Call(call) = value {
                    if call.name == "run_tests" && !call.named.iter().any(|(k, _)| k == "expect") {
                        found.push("the result of run_tests(...) is thrown away, so nothing sees whether the tests passed. Keep it (`tests = run_tests(...)`) and show it to the step that judges the change, or pass expect= to fail when they do not pass.".to_string());
                    }
                }
            }
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                collect_evidence_bindings(then_branch, bound, found, warnings);
                collect_evidence_bindings(else_branch, bound, found, warnings);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::With { body, .. } => {
                collect_evidence_bindings(body, bound, found, warnings)
            }
            Stmt::Try { body, handlers } => {
                collect_evidence_bindings(body, bound, found, warnings);
                for handler in handlers {
                    collect_evidence_bindings(&handler.body, bound, found, warnings);
                }
            }
            Stmt::FunctionDef(function) => {
                check_scope(&function.body, &function.body, None, true, warnings)
            }
            _ => {}
        }
    }
}

/// `run_tests(...)` or `model.ask(...)`, as shown in a warning, and whether
/// it is a model answer.
fn evidence_call(value: &Expr) -> Option<(String, bool)> {
    let value = match value {
        Expr::Await(inner) => inner.as_ref(),
        other => other,
    };
    match value {
        Expr::Call(call)
            if EVIDENCE_CALLS.contains(&call.name.as_str())
                && !call.named.iter().any(|(key, _)| key == "expect") =>
        {
            Some((format!("{}(...)", call.name), false))
        }
        Expr::MethodCall {
            receiver, method, ..
        } if method == "ask" => {
            let receiver = match receiver.as_ref() {
                Expr::Name(name) => name.as_str(),
                _ => "model",
            };
            Some((format!("{receiver}.ask(...)"), true))
        }
        _ => None,
    }
}

/// The variable an object expression starts from: `d` in `d["k"].items`
/// or `d.setdefault(k, [])`.
fn base_name(mut expr: &Expr) -> Option<&str> {
    loop {
        match expr {
            Expr::Name(name) => return Some(name),
            Expr::Attribute { value, .. }
            | Expr::Subscript { value, .. }
            | Expr::MethodCall {
                receiver: value, ..
            } => expr = value,
            _ => return None,
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
    /// Whether `print` and `log` count as using a carrier. They do for the
    /// unread-evidence check, since a program's output goes back to the agent
    /// that ran it; they do not for a model step's own evidence.
    display_reads: bool,
}

impl Flow {
    fn run(function: &FunctionDef, carriers: HashSet<String>) -> Flow {
        Flow::over(&function.body, carriers, false)
    }

    fn over(body: &[Stmt], carriers: HashSet<String>, display_reads: bool) -> Flow {
        let mut flow = Flow {
            carriers,
            seen: false,
            escaped: false,
            asks: 0,
            display_reads,
        };
        // Flow-insensitive: repeat until no new name carries the value, so a
        // loop that appends to a list declared above it is covered.
        loop {
            let before = flow.carriers.len();
            flow.asks = 0;
            flow.block(body);
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
                    if let Some(name) = base_name(base) {
                        self.carriers.insert(name.to_string());
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
                // `out[k].append(x)` changes the object `out` holds.
                if args {
                    if let Some(name) = base_name(receiver) {
                        self.carriers.insert(name.to_string());
                    }
                }
                receiver_carries | args
            }
            Expr::Call(call) => {
                let args = self.all(&call.positional) | self.named(&call.named);
                let name = call.name.as_str();
                if DISPLAY_CALLS.contains(&name) {
                    if args && self.display_reads {
                        self.escaped = true;
                    }
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

    fn unread(source: &str) -> Vec<String> {
        let program = lower_source(source).expect("program lowers");
        unread_evidence_warnings(&program.statements)
    }

    #[test]
    fn warns_when_evidence_is_never_read() {
        // The verify step asks without the test result it just ran.
        let found = unread(&format!(
            "{TYPES}tests = run_tests([\"pytest\", \"-q\"])\nv = model.ask(\"Did the tests pass?\", output=Verdict)\nemit(v)\n"
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("`tests` holds the result of run_tests(...), but nothing reads it"), "{}", found[0]);
        assert!(found[0].contains("inputs={\"tests\": tests}"), "{}", found[0]);

        // Inside a function; a model answer nobody reads.
        let found = unread(&format!(
            "{TYPES}def verify():\n    r = run([\"pytest\"])\n    plan = model.ask(\"Plan.\")\n    return model.ask(\"ok?\", output=Verdict)\n"
        ));
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().any(|w| w.starts_with("`r` holds the result of run(...)")), "{found:?}");
        assert!(found.iter().any(|w| w.starts_with("`plan` holds the answer of model.ask(...)")), "{found:?}");

        // A thrown-away test run in a workflow.
        let found = unread(&format!(
            "{TYPES}run_tests([\"pytest\"])\nv = model.ask(\"ok?\", output=Verdict)\nemit(v)\n"
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("the result of run_tests(...) is thrown away"), "{}", found[0]);
    }

    #[test]
    fn quiet_when_evidence_is_read() {
        let read = [
            // shown to the model
            "tests = run_tests([\"pytest\"])\nv = model.ask(\"ok?\", output=Verdict, inputs={\"tests\": tests})\nemit(v)\n",
            "r = run([\"pytest\"])\nv = model.ask(f\"Output: {r.stdout}\", output=Verdict)\nemit(v)\n",
            // checked, printed, emitted, or the program's result
            "r = run([\"pytest\"])\nif not r.ok:\n    fail(\"tests\")\nemit(model.ask(\"ok?\", output=Verdict))\n",
            "r = run([\"pytest\"])\nprint(r.stdout)\nemit(model.ask(\"ok?\", output=Verdict))\n",
            "v = model.ask(\"ok?\", output=Verdict)\nr = run([\"pytest\"])\nemit({\"v\": v, \"r\": r})\n",
            "v = model.ask(\"ok?\", output=Verdict)\nr = run([\"pytest\"])\n[v, r]\n",
            "out = {\"a\": []}\nfor i in range(2):\n    a = model.ask(\"ok?\", output=Verdict)\n    out[\"a\"].append(a.ok)\nemit(out)\n",
            // checked by the runtime, or marked unused
            "r = run([\"pytest\"], expect={\"exit_code\": 0})\nemit(model.ask(\"ok?\", output=Verdict))\n",
            "_r = run([\"pytest\"])\nemit(model.ask(\"ok?\", output=Verdict))\n",
            // a session snippet with no model call: the agent may read r next
            "r = run([\"pytest\"])\nx = 1\n",
            "run_tests([\"pytest\"])\nx = 1\n",
            // the final statement is the program's result
            "v = model.ask(\"ok?\", output=Verdict)\nemit(v)\nrun_tests([\"pytest\"])\n",
        ];
        for body in read {
            let source = format!("{TYPES}{body}");
            assert!(unread(&source).is_empty(), "{source}: {:?}", unread(&source));
        }
    }
}
