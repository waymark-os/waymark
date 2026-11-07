// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashSet;

use nu_protocol::{shell_error::generic::GenericError, ShellError};

use crate::stone_ast::{
    AssignTarget, ComprehensionClause, ExceptHandler, Expr, FormattedStringPart, Program, Stmt,
};
use crate::stone_correction::expected_keywords;
use crate::stone_errors::{
    builtin_error_type_list, host_only_error_message, is_builtin_error_type,
    is_host_only_error_type, rejected_error_type_message,
};
use crate::stone_eval::stone_builtin_names;

#[derive(Clone)]
struct AdmissionNames {
    callable: HashSet<String>,
    schema_shadow: HashSet<String>,
    async_context: bool,
}

pub(crate) fn validate_program(
    program: &Program,
    session_bound_names: &[String],
) -> Result<(), ShellError> {
    let mut bound_names = session_bound_names.iter().cloned().collect::<HashSet<_>>();
    collect_statement_bindings(&program.statements, &mut bound_names);

    let mut callable = stone_builtin_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect::<HashSet<_>>();
    callable.extend(["get", "items", "keys", "values", "isinstance"].map(str::to_string));
    callable.extend(
        crate::stone_errors::BUILTIN_ERROR_TYPES
            .iter()
            .map(|(name, _)| (*name).to_string()),
    );
    callable.extend(bound_names.iter().cloned());
    let names = AdmissionNames {
        callable,
        schema_shadow: bound_names,
        async_context: false,
    };

    validate_statements(&program.statements, &names)
}

fn validate_statements(statements: &[Stmt], names: &AdmissionNames) -> Result<(), ShellError> {
    for statement in statements {
        match statement {
            Stmt::Assign { value, .. } | Stmt::AugAssign { value, .. } => {
                validate_expr(value, names)?;
            }
            Stmt::For { iter, body, .. } => {
                validate_expr(iter, names)?;
                validate_statements(body, names)?;
            }
            Stmt::While { condition, body } => {
                validate_expr(condition, names)?;
                validate_statements(body, names)?;
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                validate_expr(condition, names)?;
                validate_statements(then_branch, names)?;
                validate_statements(else_branch, names)?;
            }
            Stmt::With {
                is_async,
                context,
                body,
                ..
            } => {
                if *is_async && !names.async_context {
                    return Err(admission_error("async with is only valid inside async def"));
                }
                validate_expr(context, names)?;
                validate_statements(body, names)?;
            }
            Stmt::Try { body, handlers } => {
                validate_statements(body, names)?;
                for handler in handlers {
                    validate_handler(handler, names)?;
                    validate_statements(&handler.body, names)?;
                }
            }
            Stmt::FunctionDef(function) => {
                if let Some(base) = &function.error_base {
                    if is_host_only_error_type(base) {
                        return Err(admission_error(format!(
                            "`class {}({base})`: {}",
                            function.name,
                            host_only_error_message(base)
                        )));
                    }
                    validate_error_type_name(
                        base,
                        names,
                        &format!("class {}({base})", function.name),
                    )?;
                }
                if let Some(stage) = &function.stage {
                    validate_expr(&stage.evidence, names)?;
                    if let Some(repair) = &stage.repair {
                        validate_expr(repair, names)?;
                    }
                    if let Some(max_attempts) = &stage.max_attempts {
                        validate_expr(max_attempts, names)?;
                    }
                }
                for parameter in &function.params {
                    if let Some(default) = &parameter.default {
                        validate_expr(default, names)?;
                    }
                }
                let mut function_names = names.clone();
                function_names.async_context = function.is_async;
                validate_statements(&function.body, &function_names)?;
            }
            Stmt::Raise(Some(Expr::Name(name)))
                if is_host_only_error_type(name) && !names.schema_shadow.contains(name) =>
            {
                return Err(admission_error(format!(
                    "`raise {name}`: {}",
                    host_only_error_message(name)
                )));
            }
            Stmt::Raise(Some(value)) | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                validate_expr(value, names)?;
            }
            Stmt::Import { .. } => {}
            Stmt::Module(module) => validate_statements(&module.body, names)?,
            Stmt::Raise(None) | Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Pass => {}
        }
    }
    Ok(())
}

/// Handler rules checked before anything runs: every handler type must exist,
/// and a handler for `Exception` or `ToolError` may not be only `pass`, since
/// that discards failed commands, missing files, and failed model calls
/// without a trace. Narrow handlers such as `except KeyError: pass` stay legal.
fn validate_handler(handler: &ExceptHandler, names: &AdmissionNames) -> Result<(), ShellError> {
    let shown = if handler.types.len() == 1 {
        handler.types[0].clone()
    } else {
        format!("({})", handler.types.join(", "))
    };
    for handler_type in &handler.types {
        validate_error_type_name(handler_type, names, &format!("except {shown}"))?;
    }
    let broad = handler
        .types
        .iter()
        .find(|handler_type| matches!(handler_type.as_str(), "Exception" | "ToolError"));
    if let (Some(broad), [Stmt::Pass]) = (broad, handler.body.as_slice()) {
        let binding = handler.name.as_deref().unwrap_or("error");
        return Err(admission_error(format!(
            "`except {shown}: pass` discards every {broad} without a trace, including failed commands, missing files, and failed model calls. Bind the error and use it (`except {shown} as {binding}:` then record {binding}.message, return a fallback, or `raise`), or catch only the error you expect, such as `except FileNotFoundError:`"
        )));
    }
    Ok(())
}

fn validate_error_type_name(
    name: &str,
    names: &AdmissionNames,
    context: &str,
) -> Result<(), ShellError> {
    if is_builtin_error_type(name) {
        return Ok(());
    }
    if let Some(message) = rejected_error_type_message(name) {
        return Err(admission_error(format!("`{context}`: {message}")));
    }
    if names.schema_shadow.contains(name) {
        // A class defined in this program or the session; the runtime checks
        // that it really is an error class when it is defined.
        return Ok(());
    }
    Err(admission_error(format!(
        "`{context}`: unknown error type `{name}`. Builtin error types: {}. Define a new one with `class {name}(Exception):` and optional typed fields",
        builtin_error_type_list()
    )))
}

fn validate_expr(expr: &Expr, names: &AdmissionNames) -> Result<(), ShellError> {
    match expr {
        Expr::None
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Name(_) => {}
        Expr::FormattedString(parts) => {
            for part in parts {
                match part {
                    FormattedStringPart::Literal(_) => {}
                    FormattedStringPart::Expr(expr)
                    | FormattedStringPart::Formatted { expr, .. } => {
                        validate_expr(expr, names)?;
                    }
                }
            }
        }
        Expr::List(values) | Expr::Tuple(values) => {
            validate_exprs(values, names)?;
        }
        Expr::ListComprehension { elt, clauses } => {
            validate_expr(elt, names)?;
            validate_clauses(clauses, names)?;
        }
        Expr::Record(fields) => {
            for (_, value) in fields {
                validate_expr(value, names)?;
            }
        }
        Expr::DictComprehension {
            key,
            value,
            clauses,
        } => {
            validate_expr(key, names)?;
            validate_expr(value, names)?;
            validate_clauses(clauses, names)?;
        }
        Expr::Subscript { value, index } => {
            validate_expr(value, names)?;
            validate_expr(index, names)?;
        }
        Expr::Attribute { value, .. } => {
            validate_expr(value, names)?;
        }
        Expr::Slice { lower, upper } => {
            if let Some(lower) = lower {
                validate_expr(lower, names)?;
            }
            if let Some(upper) = upper {
                validate_expr(upper, names)?;
            }
        }
        Expr::Compare {
            left, comparators, ..
        } => {
            validate_expr(left, names)?;
            validate_exprs(comparators, names)?;
        }
        Expr::BoolOp { values, .. } => {
            validate_exprs(values, names)?;
        }
        Expr::Conditional {
            then_expr,
            condition,
            else_expr,
        } => {
            validate_expr(then_expr, names)?;
            validate_expr(condition, names)?;
            validate_expr(else_expr, names)?;
        }
        Expr::Not(value) | Expr::Neg(value) | Expr::Invert(value) => {
            validate_expr(value, names)?;
        }
        Expr::Await(value) => {
            if !names.async_context {
                return Err(admission_error("await is only valid inside async def"));
            }
            let supported = match value.as_ref() {
                Expr::Name(_) => true,
                Expr::MethodCall { method, .. } => {
                    matches!(method.as_str(), "wait" | "wait_any" | "wait_all" | "accept")
                }
                Expr::Call(call) => matches!(
                    call.name.as_str(),
                    "run"
                        | "attempt_wait"
                        | "attempt_join"
                        | "attempt_wait_any"
                        | "attempt_wait_all"
                        | "attempt_accept"
                ),
                _ => false,
            };
            if !supported {
                return Err(admission_error(
                    "await currently supports run(), an attempt handle, child.wait(), scope.wait_any(), scope.wait_all(), root.accept(), and their functional attempt_* forms",
                ));
            }
            validate_expr(value, names)?;
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
            validate_expr(left, names)?;
            validate_expr(right, names)?;
        }
        Expr::Generator { elt, clauses } => {
            validate_expr(elt, names)?;
            validate_clauses(clauses, names)?;
        }
        Expr::Lambda { body, .. } => {
            validate_expr(body, names)?;
        }
        Expr::MethodCall {
            receiver,
            positional,
            named,
            ..
        } => {
            validate_expr(receiver, names)?;
            validate_exprs(positional, names)?;
            for (_, value) in named {
                validate_expr(value, names)?;
            }
        }
        Expr::CallValue {
            callee,
            positional,
            named,
        } => {
            validate_expr(callee, names)?;
            validate_exprs(positional, names)?;
            for (_, value) in named {
                validate_expr(value, names)?;
            }
        }
        Expr::Call(call) => {
            if !names.callable.contains(&call.name) {
                return Err(admission_error(format!(
                    "unknown Stone function `{}`; use help() for available Stone functions",
                    call.name
                )));
            }
            if is_host_only_error_type(&call.name) && !names.schema_shadow.contains(&call.name) {
                return Err(admission_error(format!(
                    "`{}(...)`: {}",
                    call.name,
                    host_only_error_message(&call.name)
                )));
            }
            if !names.schema_shadow.contains(&call.name) {
                if let Some(expected) = expected_keywords(&call.name) {
                    if let Some((received, _)) = call
                        .named
                        .iter()
                        .find(|(name, _)| !expected.contains(&name.as_str()))
                    {
                        return Err(admission_error(format!(
                            "unexpected {} keyword argument `{received}`",
                            call.name
                        )));
                    }
                }
            }
            validate_exprs(&call.positional, names)?;
            for (_, value) in &call.named {
                validate_expr(value, names)?;
            }
        }
    }
    Ok(())
}

fn validate_exprs(exprs: &[Expr], names: &AdmissionNames) -> Result<(), ShellError> {
    for expr in exprs {
        validate_expr(expr, names)?;
    }
    Ok(())
}

fn validate_clauses(
    clauses: &[ComprehensionClause],
    names: &AdmissionNames,
) -> Result<(), ShellError> {
    for clause in clauses {
        validate_expr(&clause.iter, names)?;
        validate_exprs(&clause.filters, names)?;
    }
    Ok(())
}

fn collect_statement_bindings(statements: &[Stmt], names: &mut HashSet<String>) {
    for statement in statements {
        match statement {
            Stmt::Assign { target, value } | Stmt::AugAssign { target, value, .. } => {
                collect_assign_target(target, names);
                collect_expr_bindings(value, names);
            }
            Stmt::For {
                targets,
                iter,
                body,
            } => {
                names.extend(targets.iter().cloned());
                collect_expr_bindings(iter, names);
                collect_statement_bindings(body, names);
            }
            Stmt::While { condition, body } => {
                collect_expr_bindings(condition, names);
                collect_statement_bindings(body, names);
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                collect_expr_bindings(condition, names);
                collect_statement_bindings(then_branch, names);
                collect_statement_bindings(else_branch, names);
            }
            Stmt::With {
                target,
                context,
                body,
                ..
            } => {
                if let Some(target) = target {
                    names.insert(target.clone());
                }
                collect_expr_bindings(context, names);
                collect_statement_bindings(body, names);
            }
            Stmt::Try { body, handlers } => {
                collect_statement_bindings(body, names);
                for handler in handlers {
                    if let Some(name) = &handler.name {
                        names.insert(name.clone());
                    }
                    collect_statement_bindings(&handler.body, names);
                }
            }
            Stmt::FunctionDef(function) => {
                names.insert(function.name.clone());
                if let Some(stage) = &function.stage {
                    collect_expr_bindings(&stage.evidence, names);
                    if let Some(repair) = &stage.repair {
                        collect_expr_bindings(repair, names);
                    }
                    if let Some(max_attempts) = &stage.max_attempts {
                        collect_expr_bindings(max_attempts, names);
                    }
                }
                names.extend(
                    function
                        .params
                        .iter()
                        .map(|parameter| parameter.name.clone()),
                );
                for parameter in &function.params {
                    if let Some(default) = &parameter.default {
                        collect_expr_bindings(default, names);
                    }
                }
                collect_statement_bindings(&function.body, names);
            }
            Stmt::Raise(Some(value)) | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                collect_expr_bindings(value, names);
            }
            Stmt::Import {
                names: imported, ..
            } => {
                names.extend(imported.iter().map(|import| import.local.clone()));
            }
            Stmt::Module(module) => collect_statement_bindings(&module.body, names),
            Stmt::Raise(None) | Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Pass => {}
        }
    }
}

fn collect_assign_target(target: &AssignTarget, names: &mut HashSet<String>) {
    match target {
        AssignTarget::Name(name) => {
            names.insert(name.clone());
        }
        AssignTarget::Tuple(items) => {
            names.extend(items.iter().cloned());
        }
        AssignTarget::Subscript { value, index } => {
            collect_assign_target(value, names);
            collect_expr_bindings(index, names);
        }
    }
}

fn collect_expr_bindings(expr: &Expr, names: &mut HashSet<String>) {
    match expr {
        Expr::None
        | Expr::Bool(_)
        | Expr::Int(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Name(_) => {}
        Expr::FormattedString(parts) => {
            for part in parts {
                match part {
                    FormattedStringPart::Literal(_) => {}
                    FormattedStringPart::Expr(expr)
                    | FormattedStringPart::Formatted { expr, .. } => {
                        collect_expr_bindings(expr, names);
                    }
                }
            }
        }
        Expr::List(values) | Expr::Tuple(values) => {
            for value in values {
                collect_expr_bindings(value, names);
            }
        }
        Expr::ListComprehension { elt, clauses } | Expr::Generator { elt, clauses } => {
            collect_expr_bindings(elt, names);
            collect_clause_bindings(clauses, names);
        }
        Expr::Record(fields) => {
            for (_, value) in fields {
                collect_expr_bindings(value, names);
            }
        }
        Expr::DictComprehension {
            key,
            value,
            clauses,
        } => {
            collect_expr_bindings(key, names);
            collect_expr_bindings(value, names);
            collect_clause_bindings(clauses, names);
        }
        Expr::Subscript { value, index } => {
            collect_expr_bindings(value, names);
            collect_expr_bindings(index, names);
        }
        Expr::Attribute { value, .. } => {
            collect_expr_bindings(value, names);
        }
        Expr::Slice { lower, upper } => {
            if let Some(lower) = lower {
                collect_expr_bindings(lower, names);
            }
            if let Some(upper) = upper {
                collect_expr_bindings(upper, names);
            }
        }
        Expr::Compare {
            left, comparators, ..
        } => {
            collect_expr_bindings(left, names);
            for value in comparators {
                collect_expr_bindings(value, names);
            }
        }
        Expr::BoolOp { values, .. } => {
            for value in values {
                collect_expr_bindings(value, names);
            }
        }
        Expr::Conditional {
            then_expr,
            condition,
            else_expr,
        } => {
            collect_expr_bindings(then_expr, names);
            collect_expr_bindings(condition, names);
            collect_expr_bindings(else_expr, names);
        }
        Expr::Not(value) | Expr::Neg(value) | Expr::Invert(value) | Expr::Await(value) => {
            collect_expr_bindings(value, names);
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
            collect_expr_bindings(left, names);
            collect_expr_bindings(right, names);
        }
        Expr::Lambda { params, body, .. } => {
            names.extend(params.iter().cloned());
            collect_expr_bindings(body, names);
        }
        Expr::MethodCall {
            receiver,
            positional,
            named,
            ..
        } => {
            collect_expr_bindings(receiver, names);
            for value in positional {
                collect_expr_bindings(value, names);
            }
            for (_, value) in named {
                collect_expr_bindings(value, names);
            }
        }
        Expr::CallValue {
            callee,
            positional,
            named,
        } => {
            collect_expr_bindings(callee, names);
            for value in positional {
                collect_expr_bindings(value, names);
            }
            for (_, value) in named {
                collect_expr_bindings(value, names);
            }
        }
        Expr::Call(call) => {
            for value in &call.positional {
                collect_expr_bindings(value, names);
            }
            for (_, value) in &call.named {
                collect_expr_bindings(value, names);
            }
        }
    }
}

fn collect_clause_bindings(clauses: &[ComprehensionClause], names: &mut HashSet<String>) {
    for clause in clauses {
        names.extend(clause.targets.iter().cloned());
        collect_expr_bindings(&clause.iter, names);
        for filter in &clause.filters {
            collect_expr_bindings(filter, names);
        }
    }
}

fn admission_error(detail: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal("Stone admission error", detail.into())
            .with_code("stone_admission_error")
            .with_help("Correct the source before evaluating it; no Stone statement was run."),
    )
}

#[cfg(test)]
mod tests {
    use crate::stone_ast::lower_source;

    use super::validate_program;

    #[test]
    fn handler_rules_run_before_evaluation() {
        let reject = |source: &str, needle: &str| {
            let program = lower_source(source).expect("lower");
            let error = validate_program(&program, &[]).expect_err(source);
            let detail = format!("{error:?}");
            assert!(detail.contains("stone_admission_error"), "{detail}");
            assert!(detail.contains(needle), "{source}: {detail}");
        };
        reject(
            "try:\n    x = 1\nexcept Exception:\n    pass",
            "discards every Exception",
        );
        reject(
            "try:\n    x = 1\nexcept (KeyError, ToolError):\n    pass",
            "discards every ToolError",
        );
        reject(
            "try:\n    x = 1\nexcept ConfigEror as e:\n    x = 2",
            "unknown error type `ConfigEror`",
        );
        reject(
            "try:\n    x = 1\nexcept BaseException as e:\n    x = 2",
            "`BaseException` does not exist",
        );
        reject(
            "try:\n    x = 1\nexcept IOError as e:\n    x = 2",
            "spelled `OSError`",
        );
        reject(
            "class E(Missing):\n    pass",
            "unknown error type `Missing`",
        );

        for source in [
            "try:\n    x = 1\nexcept KeyError:\n    pass",
            "try:\n    x = 1\nexcept Exception as error:\n    x = error.message",
            "class ConfigError(Exception):\n    path: str\ntry:\n    x = 1\nexcept ConfigError as e:\n    x = 2",
        ] {
            let program = lower_source(source).expect("lower");
            validate_program(&program, &[]).expect(source);
        }
        // A class defined in an earlier session command is a known type.
        let program =
            lower_source("try:\n    x = 1\nexcept ConfigError:\n    x = 2").expect("lower");
        validate_program(&program, &["ConfigError".to_string()]).expect("session class");
    }

    #[test]
    fn rejects_unknown_calls_and_structured_keywords() {
        let unknown = lower_source("context_projet()").expect("lower");
        let error = validate_program(&unknown, &[]).expect_err("unknown call");
        assert!(format!("{error:?}").contains("stone_admission_error"));

        let keyword = lower_source("context_project(max_token=32)").expect("lower");
        let error = validate_program(&keyword, &[]).expect_err("unknown keyword");
        assert!(format!("{error:?}").contains("max_token"));

        let workflow_keyword =
            lower_source(r#"workflow_stage("build", evidnce=check, action=run_stage)"#)
                .expect("lower");
        let error = validate_program(&workflow_keyword, &[]).expect_err("unknown workflow keyword");
        assert!(format!("{error:?}").contains("evidnce"));

        let frontier_keyword =
            lower_source("attempt_branch(frontier=value, entypoint=\"worker\")").expect("lower");
        let error = validate_program(&frontier_keyword, &[]).expect_err("unknown frontier keyword");
        assert!(format!("{error:?}").contains("entypoint"));
    }

    #[test]
    fn admits_only_narrow_async_resource_effects() {
        let valid = lower_source(
            r#"async def main():
    async with attempt_scope() as scope:
        outcome = await child.wait(timeout_ms=1000)
        command = await run(["true"], timeout_ms=1000)
    return outcome
"#,
        )
        .expect("lower valid async resource control");
        validate_program(&valid, &[]).expect("admit valid async resource control");

        let unsupported = lower_source(
            r#"async def main():
    return await print("not an owned resource effect")
"#,
        )
        .expect("lower unsupported await target");
        let error = validate_program(&unsupported, &[]).expect_err("reject broad await");
        assert!(format!("{error:?}").contains("await currently supports"));

        let sync_with = lower_source(
            r#"def main():
    async with attempt_scope() as scope:
        pass
"#,
        )
        .expect("lower misplaced async with");
        let error = validate_program(&sync_with, &[]).expect_err("reject misplaced async with");
        assert!(format!("{error:?}").contains("async with is only valid"));
    }

    #[test]
    fn accepts_source_and_dynamic_callable_bindings() {
        let program = lower_source(
            r#"def invoke(callback):
    return callback(1)
double = lambda value: value * 2
emit(invoke(double))"#,
        )
        .expect("lower");
        validate_program(&program, &[]).expect("bound callables");
    }

    #[test]
    fn accepts_names_bound_by_a_prior_session() {
        let program = lower_source("emit(prior(2))").expect("lower");
        validate_program(&program, &["prior".to_string()]).expect("session callable");
    }

    #[test]
    fn does_not_apply_builtin_keyword_schemas_to_shadowed_names() {
        let program = lower_source(
            r#"def context_project(max_token):
    return max_token
emit(context_project(max_token=32))"#,
        )
        .expect("lower");
        validate_program(&program, &[]).expect("shadowed builtin");
    }
}
