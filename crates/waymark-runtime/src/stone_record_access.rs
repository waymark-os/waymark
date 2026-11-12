//! Pre-run check: `x["field"]` on a value known to be a record.
//!
//! Records (model-defined `class R(Record)` values and the typed records host
//! builtins return) are read with attributes, never subscripts. The evaluator
//! already rejects `x["field"]` on a record, but only when the line runs, which
//! for an agent program can be minutes and many model calls in. This pass
//! finds the cases that are visible in the source and rejects them before any
//! statement runs.
//!
//! A name is known to hold a record only when every assignment to it in its
//! scope is one of:
//!
//! - a record constructor call, `R(...)`;
//! - `model.ask(..., output=R)` (or any `.ask`), where `R` names a record or a
//!   top-level union of records (`Action = Read | Edit`);
//! - a host builtin whose result is a single typed record (`run`, `stat`, ...);
//! - a parameter annotated with a record (`def f(plan: Plan)`);
//! - the first parameter of a function the program hands to the host as a
//!   callback that receives a record (`workflow_stage(evidence=f)` gives `f` a
//!   `StageContext`; `transition_hooks(pre=f)` gives a `TransitionEvent`).
//!
//! Any other binding of the name (a loop variable, an `except ... as`, an
//! unpacking, another assignment) makes it unknown, so the check never
//! rejects a program whose value could be a dict. Linear in the program.

use std::collections::{HashMap, HashSet};

use nu_protocol::{shell_error::generic::GenericError, ShellError};

use crate::stone_ast::{AssignTarget, ComprehensionClause, Expr, FormattedStringPart, Program, Stmt, TypeExpr};

/// Host builtins whose result is one typed record, with the record's name.

/// Record types of results only the runtime makes. A program cannot define a
/// record with one of these names, and `isinstance(x, RunResult)` is true
/// only for a result the runtime returned.
pub(crate) const HOST_RECORD_TYPES: &[&str] = &[
    "RunResult", "TestRun", "FileStat", "DirEntry", "ProcessInfo", "SystemInfo", "SearchMatch",
    "Daemon", "WorkflowReport", "StageReport", "StageContext", "TransitionEvent", "Finding",
    "FindingSpec", "Probe", "StageOutput", "Snapshot", "TreeDiff", "Message",
];

fn host_record_result(builtin: &str) -> Option<&'static str> {
    Some(match builtin {
        "run" | "must_run" | "run_wait" | "run_complete" | "run_terminate" | "last_result" => "RunResult",
        "run_tests" => "TestRun",
        "snapshot" | "apply_patch" => "Snapshot",
        "start_daemon" | "daemon_status" | "stop_daemon" => "Daemon",
        "stat" => "FileStat",
        "message" => "Message",
        "sysinfo" | "sys_info" => "SystemInfo",
        "workflow_run" | "workflow_main" => "WorkflowReport",
        _ => return None,
    })
}

/// Host builtins that call a function the program passes by keyword, with a
/// record as its first argument.
fn callback_record(builtin: &str, keyword: &str) -> Option<&'static str> {
    match (builtin, keyword) {
        ("workflow_stage", "evidence" | "action" | "repair") => Some("StageContext"),
        ("transition_hooks", "pre" | "post") => Some("TransitionEvent"),
        _ => None,
    }
}

#[derive(Clone)]
struct Known {
    ty: String,
    origin: String,
}

type Env = HashMap<String, Known>;

struct Types {
    /// Model-defined record names, including those of linked modules.
    records: HashSet<String>,
    /// Top-level aliases that name a union of records.
    unions: HashSet<String>,
}

impl Types {
    fn is_record_type(&self, name: &str) -> bool {
        self.records.contains(name) || self.unions.contains(name)
    }
}

pub(crate) fn check_program(program: &Program) -> Result<(), ShellError> {
    let mut types = Types {
        records: HashSet::new(),
        unions: HashSet::new(),
    };
    collect_records(&program.statements, &mut types.records);
    collect_unions(&program.statements, &mut types);
    check_block(&program.statements, &Env::new(), Env::new(), &types)
}

fn collect_records(statements: &[Stmt], records: &mut HashSet<String>) {
    for statement in statements {
        match statement {
            Stmt::RecordDef(record) => {
                records.insert(record.name.clone());
            }
            Stmt::Module(module) => collect_records(&module.body, records),
            _ => {}
        }
    }
}

fn collect_unions(statements: &[Stmt], types: &mut Types) {
    for statement in statements {
        match statement {
            Stmt::Assign {
                target: AssignTarget::Name(name),
                value: value @ Expr::BitOr { .. },
            } => {
                let mut members = Vec::new();
                if union_members(value, &mut members)
                    && members.iter().all(|member| types.records.contains(*member))
                {
                    types.unions.insert(name.clone());
                }
            }
            Stmt::Module(module) => collect_unions(&module.body, types),
            _ => {}
        }
    }
}

fn union_members<'a>(expr: &'a Expr, members: &mut Vec<&'a str>) -> bool {
    match expr {
        Expr::Name(name) => {
            members.push(name);
            true
        }
        Expr::BitOr { left, right } => union_members(left, members) && union_members(right, members),
        _ => false,
    }
}

/// Check one scope: a function body or the top level. `outer` is what the
/// enclosing scopes know; `params` is what this scope's parameters are.
fn check_block(statements: &[Stmt], outer: &Env, params: Env, types: &Types) -> Result<(), ShellError> {
    let mut callbacks = HashMap::new();
    find_callbacks(statements, &mut callbacks);

    // Every binding in this scope: Some(record) or None (unknown).
    let mut bindings: HashMap<String, Vec<Option<Known>>> = HashMap::new();
    collect_bindings(statements, types, &mut bindings);

    let mut env = outer.clone();
    for name in bindings.keys() {
        env.remove(name);
    }
    for (name, known) in params {
        if bindings.contains_key(&name) {
            continue;
        }
        env.insert(name, known);
    }
    for (name, sources) in &bindings {
        if let [Some(first), rest @ ..] = sources.as_slice() {
            if rest.iter().all(Option::is_some) {
                env.insert(name.clone(), first.clone());
            }
        }
    }

    check_statements(statements, &env, types)?;

    for function in nested_functions(statements) {
        let mut function_params = Env::new();
        for (index, param) in function.params.iter().enumerate() {
            if index == 0 {
                if let Some(ty) = callbacks.get(&function.name) {
                    function_params.insert(
                        param.name.clone(),
                        Known {
                            ty: (*ty).to_string(),
                            origin: format!("the host passes one to `{}` as its first argument", function.name),
                        },
                    );
                    continue;
                }
            }
            if let Some(TypeExpr::Named(ty)) = &param.check {
                if types.is_record_type(ty) {
                    function_params.insert(
                        param.name.clone(),
                        Known {
                            ty: ty.clone(),
                            origin: format!("its annotation says `{}: {ty}`", param.name),
                        },
                    );
                }
            }
        }
        let mut inner_outer = env.clone();
        for param in &function.params {
            inner_outer.remove(&param.name);
        }
        check_block(&function.body, &inner_outer, function_params, types)?;
    }
    Ok(())
}

/// The functions defined in this scope (not inside nested functions), and
/// the module bodies linked at the top level.
fn nested_functions(statements: &[Stmt]) -> Vec<&crate::stone_ast::FunctionDef> {
    let mut found = Vec::new();
    walk_scope(statements, &mut |statement| {
        if let Stmt::FunctionDef(function) = statement {
            found.push(function);
        }
    });
    found
}

/// Visit the statements of one scope, descending into blocks and linked
/// module bodies but not into function bodies.
fn walk_scope<'a>(statements: &'a [Stmt], visit: &mut dyn FnMut(&'a Stmt)) {
    for statement in statements {
        visit(statement);
        match statement {
            Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::With { body, .. } => walk_scope(body, visit),
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                walk_scope(then_branch, visit);
                walk_scope(else_branch, visit);
            }
            Stmt::Try { body, handlers } => {
                walk_scope(body, visit);
                for handler in handlers {
                    walk_scope(&handler.body, visit);
                }
            }
            Stmt::Module(module) => walk_scope(&module.body, visit),
            _ => {}
        }
    }
}

fn find_callbacks(statements: &[Stmt], callbacks: &mut HashMap<String, &'static str>) {
    walk_scope(statements, &mut |statement| {
        for_each_statement_expr(statement, &mut |expr| find_callbacks_in_expr(expr, callbacks));
    });
}

fn find_callbacks_in_expr(expr: &Expr, callbacks: &mut HashMap<String, &'static str>) {
    if let Expr::Call(call) = expr {
        for (keyword, value) in &call.named {
            if let (Some(ty), Expr::Name(function)) = (callback_record(&call.name, keyword), value) {
                callbacks.insert(function.clone(), ty);
            }
        }
    }
    for_each_child(expr, &mut |child, _| find_callbacks_in_expr(child, callbacks));
}

fn collect_bindings(statements: &[Stmt], types: &Types, bindings: &mut HashMap<String, Vec<Option<Known>>>) {
    walk_scope(statements, &mut |statement| {
        let mut bind = |name: &str, known: Option<Known>| {
            bindings.entry(name.to_string()).or_default().push(known);
        };
        match statement {
            Stmt::Assign {
                target: AssignTarget::Name(name),
                value,
            } => bind(name, record_source(value, types)),
            Stmt::Assign { target, .. } | Stmt::AugAssign { target, .. } => {
                for name in target_names(target) {
                    bind(&name, None);
                }
            }
            Stmt::For { targets, .. } => {
                for name in targets {
                    bind(name, None);
                }
            }
            Stmt::With {
                target: Some(name), ..
            } => bind(name, None),
            Stmt::Try { handlers, .. } => {
                for handler in handlers {
                    if let Some(name) = &handler.name {
                        bind(name, None);
                    }
                }
            }
            Stmt::FunctionDef(function) => bind(&function.name, None),
            Stmt::Import { names, .. } => {
                for import in names {
                    bind(&import.local, None);
                }
            }
            _ => {}
        }
    });
}

fn target_names(target: &AssignTarget) -> Vec<String> {
    match target {
        AssignTarget::Name(name) => vec![name.clone()],
        AssignTarget::Tuple(names) => names.clone(),
        AssignTarget::Unpack(targets) => targets.iter().flat_map(target_names).collect(),
        AssignTarget::Subscript { .. } | AssignTarget::Attribute { .. } | AssignTarget::Expr(_) => Vec::new(),
    }
}

/// The record an expression is known to produce, if any.
fn record_source(expr: &Expr, types: &Types) -> Option<Known> {
    match expr {
        Expr::Call(call) if types.records.contains(&call.name) => Some(Known {
            ty: call.name.clone(),
            origin: format!("it is built by `{}(...)`", call.name),
        }),
        Expr::Call(call) => host_record_result(&call.name).map(|ty| Known {
            ty: ty.to_string(),
            origin: format!("`{}()` returns one", call.name),
        }),
        Expr::MethodCall { method, named, .. } if method == "ask" => {
            named.iter().find_map(|(keyword, value)| match (keyword.as_str(), value) {
                ("output", Expr::Name(ty)) if types.is_record_type(ty) => Some(Known {
                    ty: ty.clone(),
                    origin: format!("`.ask(output={ty})` returns one"),
                }),
                _ => None,
            })
        }
        _ => None,
    }
}

fn check_statements(statements: &[Stmt], env: &Env, types: &Types) -> Result<(), ShellError> {
    let mut result = Ok(());
    walk_scope(statements, &mut |statement| {
        if result.is_err() {
            return;
        }
        if let Stmt::FunctionDef(function) = statement {
            // Defaults are evaluated in this scope; the body is its own.
            for param in &function.params {
                if let Some(default) = &param.default {
                    if let Err(error) = check_expr(default, env, types) {
                        result = Err(error);
                        return;
                    }
                }
            }
            return;
        }
        if let Some(target) = statement_target(statement) {
            if let Err(error) = check_target(target, env, types) {
                result = Err(error);
                return;
            }
        }
        for_each_statement_expr(statement, &mut |expr| {
            if result.is_ok() {
                result = check_expr(expr, env, types);
            }
        });
    });
    result
}

fn statement_target(statement: &Stmt) -> Option<&AssignTarget> {
    match statement {
        Stmt::Assign { target, .. } | Stmt::AugAssign { target, .. } => Some(target),
        _ => None,
    }
}

/// `x["field"] = v` on a known record.
fn check_target(target: &AssignTarget, env: &Env, types: &Types) -> Result<(), ShellError> {
    match target {
        AssignTarget::Subscript { value, index } => {
            if let (AssignTarget::Name(name), Expr::String(field)) = (value.as_ref(), index) {
                if let Some(known) = env.get(name) {
                    return Err(subscript_error(name, field, known));
                }
            }
            check_target(value, env, types)?;
            check_expr(index, env, types)
        }
        AssignTarget::Attribute { value, .. } => check_target(value, env, types),
        AssignTarget::Unpack(targets) => {
            for target in targets {
                check_target(target, env, types)?;
            }
            Ok(())
        }
        AssignTarget::Expr(expr) => check_expr(expr, env, types),
        AssignTarget::Name(_) | AssignTarget::Tuple(_) => Ok(()),
    }
}

fn check_expr(expr: &Expr, env: &Env, types: &Types) -> Result<(), ShellError> {
    if let Expr::Subscript { value, index } = expr {
        if let Expr::String(field) = index.as_ref() {
            match value.as_ref() {
                Expr::Name(name) => {
                    if let Some(known) = env.get(name) {
                        return Err(subscript_error(name, field, known));
                    }
                }
                other => {
                    if let Some(known) = record_source(other, types) {
                        return Err(subscript_error("the result", field, &known));
                    }
                }
            }
        }
    }
    let mut result = Ok(());
    for_each_child(expr, &mut |child, shadowed| {
        if result.is_err() {
            return;
        }
        if shadowed.is_empty() {
            result = check_expr(child, env, types);
        } else {
            let mut inner = env.clone();
            for name in shadowed {
                inner.remove(name);
            }
            result = check_expr(child, &inner, types);
        }
    });
    result
}

fn subscript_error(name: &str, field: &str, known: &Known) -> ShellError {
    let shown = if name == "the result" { "value".to_string() } else { name.to_string() };
    let subject = if name == "the result" { "the call".to_string() } else { format!("`{name}`") };
    ShellError::Generic(
        GenericError::new_internal(
            "Stone admission error",
            format!(
                "`{shown}[{field:?}]`: {subject} holds a record of type {ty} ({origin}); records are read with attributes, not subscripts: write `{shown}.{field}`",
                ty = known.ty,
                origin = known.origin,
            ),
        )
        .with_code("stone_admission_error")
        .with_help("Correct the source before evaluating it; no Stone statement was run."),
    )
}

fn for_each_statement_expr<'a>(statement: &'a Stmt, visit: &mut dyn FnMut(&'a Expr)) {
    match statement {
        Stmt::Assign { value, .. } | Stmt::AugAssign { value, .. } => visit(value),
        Stmt::For { iter, .. } => visit(iter),
        Stmt::While { condition, .. } | Stmt::If { condition, .. } => visit(condition),
        Stmt::With { context, .. } => visit(context),
        Stmt::Raise(Some(value)) | Stmt::Return(Some(value)) | Stmt::Expr(value) => visit(value),
        Stmt::FunctionDef(_)
        | Stmt::Try { .. }
        | Stmt::Raise(None)
        | Stmt::Return(None)
        | Stmt::Break
        | Stmt::Continue
        | Stmt::Pass
        | Stmt::Import { .. }
        | Stmt::Module(_)
        | Stmt::RecordDef(_) => {}
    }
}

/// Call `visit` on each direct subexpression, with the names that are bound
/// inside it (comprehension targets, lambda parameters) and so shadow the
/// enclosing scope's.
fn for_each_child<'a>(expr: &'a Expr, visit: &mut dyn FnMut(&'a Expr, &[String])) {
    const NONE: &[String] = &[];
    match expr {
        Expr::None | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) | Expr::Name(_) => {}
        Expr::FormattedString(parts) => {
            for part in parts {
                match part {
                    FormattedStringPart::Literal(_) => {}
                    FormattedStringPart::Expr(expr) | FormattedStringPart::Formatted { expr, .. } => visit(expr, NONE),
                }
            }
        }
        Expr::List(values) | Expr::Tuple(values) | Expr::BoolOp { values, .. } => {
            for value in values {
                visit(value, NONE);
            }
        }
        Expr::Record(fields) => {
            for (_, value) in fields {
                visit(value, NONE);
            }
        }
        Expr::ListComprehension { elt, clauses } | Expr::Generator { elt, clauses } => {
            let bound = clause_targets(clauses);
            visit(elt, &bound);
            visit_clauses(clauses, &bound, visit);
        }
        Expr::DictComprehension { key, value, clauses } => {
            let bound = clause_targets(clauses);
            visit(key, &bound);
            visit(value, &bound);
            visit_clauses(clauses, &bound, visit);
        }
        Expr::Subscript { value, index } => {
            visit(value, NONE);
            visit(index, NONE);
        }
        Expr::Attribute { value, .. } | Expr::Not(value) | Expr::Neg(value) | Expr::Invert(value) | Expr::Await(value) => {
            visit(value, NONE)
        }
        Expr::Slice { lower, upper, step } => {
            for part in [lower, upper, step].into_iter().flatten() {
                visit(part, NONE);
            }
        }
        Expr::Compare { left, comparators, .. } => {
            visit(left, NONE);
            for comparator in comparators {
                visit(comparator, NONE);
            }
        }
        Expr::Conditional {
            then_expr,
            condition,
            else_expr,
        } => {
            visit(then_expr, NONE);
            visit(condition, NONE);
            visit(else_expr, NONE);
        }
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
        | Expr::RShift { left, right } => {
            visit(left, NONE);
            visit(right, NONE);
        }
        Expr::Lambda { params, body, .. } => visit(body, params),
        Expr::MethodCall {
            receiver,
            positional,
            named,
            ..
        } => {
            visit(receiver, NONE);
            for value in positional {
                visit(value, NONE);
            }
            for (_, value) in named {
                visit(value, NONE);
            }
        }
        Expr::CallValue {
            callee,
            positional,
            named,
        } => {
            visit(callee, NONE);
            for value in positional {
                visit(value, NONE);
            }
            for (_, value) in named {
                visit(value, NONE);
            }
        }
        Expr::Call(call) => {
            for value in &call.positional {
                visit(value, NONE);
            }
            for (_, value) in &call.named {
                visit(value, NONE);
            }
        }
    }
}

fn clause_targets(clauses: &[ComprehensionClause]) -> Vec<String> {
    clauses.iter().flat_map(|clause| clause.targets.iter().cloned()).collect()
}

fn visit_clauses<'a>(clauses: &'a [ComprehensionClause], bound: &[String], visit: &mut dyn FnMut(&'a Expr, &[String])) {
    for clause in clauses {
        visit(&clause.iter, bound);
        for filter in &clause.filters {
            visit(filter, bound);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::stone_admission::validate_program;
    use crate::stone_ast::lower_source;

    fn check(source: &str) -> Result<(), String> {
        let program = lower_source(source).expect("lower");
        validate_program(&program, &[]).map_err(|error| format!("{error:?}"))
    }

    #[test]
    fn subscripting_a_known_record_is_rejected_before_the_run() {
        let rejected = [
            ("r = run([\"true\"])\nemit(r[\"stdout\"])", "write `r.stdout`"),
            ("emit(stat(\".\")[\"size\"])", "write `value.size`"),
            ("class P(Record):\n    a: int = 1\ndef f(p: P):\n    return p[\"a\"]", "annotation says `p: P`"),
            ("class P(Record):\n    a: int = 1\np = P()\np[\"a\"] = 2", "write `p.a`"),
            (
                "from typing import Literal\nclass A(Record):\n    kind: Literal[\"a\"]\nclass B(Record):\n    kind: Literal[\"b\"]\nU = A | B\ndef g(m):\n    x = m.ask(\"q\", output=U)\n    return x[\"kind\"]",
                "record of type U",
            ),
            (
                "def solve():\n    def check(step):\n        return step[\"phase\"] == \"pre\"\n    return workflow_stage(\"s\", evidence=check)",
                "StageContext",
            ),
        ];
        for (source, needle) in rejected {
            let error = check(source).expect_err(source);
            assert!(error.contains("stone_admission_error") && error.contains(needle), "{source}: {error}");
        }
    }

    #[test]
    fn values_that_may_be_dicts_are_left_alone() {
        let accepted = [
            "r = {\"stdout\": 1}\nemit(r[\"stdout\"])",
            "r = run([\"true\"])\nr = {\"stdout\": 2}\nemit(r[\"stdout\"])",
            "class P(Record):\n    a: int = 1\np = P()\nrows = [{\"a\": 3}]\nemit([p[\"a\"] for p in rows])",
            "def f(step):\n    return step[\"phase\"]\nemit(f({\"phase\": 1}))",
            "r = run([\"true\"])\ndef f(r):\n    return r[\"x\"]\nemit(f({\"x\": 1}))",
        ];
        for source in accepted {
            check(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        }
    }
}
