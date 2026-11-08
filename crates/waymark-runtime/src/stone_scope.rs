// SPDX-License-Identifier: MIT OR Apache-2.0

//! Lexical scope resolution, run once after lowering.
//!
//! Stone follows Python's scoping with one simplification (design D-3):
//! closures read the variables of enclosing functions but never rebind them.
//!
//! - A function's locals are its parameters and every name it binds
//!   (assignment, `for`, `with ... as`, `except ... as`, nested `def`).
//! - Any other name a function reads is free. A free name that is a local of
//!   an enclosing function is captured; the rest are module globals or
//!   builtins, looked up when the function runs.
//! - Only captured variables need shared cells at run time; every other local
//!   stays a plain slot.
//! - `x += 1` on a name the function never binds otherwise is rejected here
//!   with the fix (Python raises UnboundLocalError when it runs), as are
//!   `nonlocal` and `global`, which Stone does not have.
//!
//! The results are stored on the AST: `FunctionDef::scope` and the
//! `captures` of each lambda.
//!
//! For a module (`resolve_module`), the same walk also qualifies the
//! module's top-level names (`helper` becomes `lib.helper`) wherever they are
//! not shadowed by a local, and rejects top-level code that would run when
//! the module is imported: modules only define functions, error classes, and
//! constants.

use std::collections::BTreeSet;

use nu_protocol::{shell_error::generic::GenericError, ShellError};

use crate::stone_ast::{
    AssignTarget, ComprehensionClause, Expr, FormattedStringPart, FunctionDef, FunctionScope,
    ImportKind, Program, Stmt,
};

/// The walk's context: the enclosing functions (innermost last) and, for a
/// module, the names to qualify.
#[derive(Default)]
struct Ctx {
    frames: Vec<Enclosing>,
    /// Comprehension targets in scope, which shadow module names.
    comprehension: Vec<Names>,
    module: Option<ModuleNames>,
}

struct ModuleNames {
    name: String,
    names: Names,
}

impl std::ops::Deref for Ctx {
    type Target = Vec<Enclosing>;

    fn deref(&self) -> &Self::Target {
        &self.frames
    }
}

impl std::ops::DerefMut for Ctx {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frames
    }
}

impl Ctx {
    /// Qualify `name` in place if it is a module top-level name not shadowed
    /// here.
    fn qualify(&self, name: &mut String) {
        let Some(module) = &self.module else {
            return;
        };
        if !module.names.contains(name.as_str())
            || self
                .frames
                .iter()
                .any(|frame| frame.locals.contains(name.as_str()))
            || self
                .comprehension
                .iter()
                .any(|targets| targets.contains(name.as_str()))
        {
            return;
        }
        *name = format!("{}.{name}", module.name);
    }

    /// The module whose top-level code is being walked: that code would run
    /// at import time.
    fn import_time_module(&self) -> Option<&str> {
        match &self.module {
            Some(module) if self.frames.is_empty() => Some(&module.name),
            _ => None,
        }
    }

    fn reject_import_time_call(&self, callee: &str) -> Result<(), ShellError> {
        match self.import_time_module() {
            Some(module) => Err(module_error(format!(
                "module `{module}` calls `{callee}(...)` at its top level, but importing a module runs no code: a module only defines functions, error classes, and constants. Move the call into a function, or write the value literally"
            ))),
            None => Ok(()),
        }
    }
}

fn module_error(message: String) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal("unsupported Stone module", message)
            .with_code("stone_script_unsupported"),
    )
}

type Names = BTreeSet<String>;

struct Enclosing {
    function: String,
    locals: Names,
}

pub(crate) fn resolve_program(program: &mut Program) -> Result<(), ShellError> {
    let mut ctx = Ctx::default();
    let mut reads = Names::new();
    resolve_stmts(&mut program.statements, &mut ctx, &mut reads)?;
    Ok(())
}

/// Resolve module `name`: qualify its top-level names and check that its top
/// level only defines things. Returns the names the module reads that are not
/// its own (builtins and error types, or names it fails to define).
pub(crate) fn resolve_module(
    program: &mut Program,
    name: &str,
) -> Result<BTreeSet<String>, ShellError> {
    let mut names = Names::new();
    for statement in &program.statements {
        check_module_statement(statement, name)?;
        match statement {
            Stmt::Assign { target, .. } => bind_target(target, &mut names),
            Stmt::FunctionDef(function) if function.error_base.is_none() => {
                names.insert(function.name.clone());
            }
            // Error classes and records keep their own (global) names.
            Stmt::Import {
                names: imported, ..
            } => names.extend(
                imported
                    .iter()
                    .filter(|import| {
                        !matches!(import.kind, ImportKind::ErrorClass | ImportKind::Record)
                    })
                    .map(|import| import.local.clone()),
            ),
            _ => {}
        }
    }
    let mut ctx = Ctx {
        module: Some(ModuleNames {
            name: name.to_string(),
            names,
        }),
        ..Ctx::default()
    };
    let mut reads = Names::new();
    resolve_stmts(&mut program.statements, &mut ctx, &mut reads)?;
    let prefix = format!("{name}.");
    reads.retain(|read| !read.starts_with(&prefix));
    Ok(reads)
}

/// A module's top level may only define functions, error classes, and
/// constants, import, or hold a docstring.
fn check_module_statement(statement: &Stmt, module: &str) -> Result<(), ShellError> {
    let what = match statement {
        Stmt::Assign {
            target: AssignTarget::Name(_) | AssignTarget::Tuple(_),
            ..
        }
        | Stmt::Import { .. }
        | Stmt::RecordDef(_)
        | Stmt::Pass
        | Stmt::Expr(Expr::String(_)) => return Ok(()),
        Stmt::FunctionDef(function) if function.stage.is_none() => return Ok(()),
        Stmt::FunctionDef(_) => "a `@stage` function",
        Stmt::Assign { .. } => "an item assignment",
        Stmt::AugAssign { .. } => "an augmented assignment",
        Stmt::For { .. } | Stmt::While { .. } => "a loop",
        Stmt::If { .. } => "an `if` statement",
        Stmt::With { .. } => "a `with` statement",
        Stmt::Try { .. } => "a `try` statement",
        Stmt::Raise(_) => "a `raise` statement",
        Stmt::Return(_) | Stmt::Break | Stmt::Continue => "a `return`, `break`, or `continue`",
        Stmt::Expr(_) => "an expression statement",
        Stmt::Module(_) => "a linked module",
    };
    Err(module_error(format!(
        "module `{module}` has {what} at its top level, but importing a module runs no code: a module only defines functions (`def`), records, error classes, and constants (`NAME = value`), and imports other modules. Move this into a function"
    )))
}

fn scope_error(message: String) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal("unsupported Stone rebinding", message)
            .with_code("stone_script_unsupported"),
    )
}

/// Resolve a function definition found inside `stack` (the enclosing
/// functions, innermost last). Returns the function's free names.
fn resolve_function(function: &mut FunctionDef, stack: &mut Ctx) -> Result<Names, ShellError> {
    let params = function
        .params
        .iter()
        .map(|param| param.name.clone())
        .collect::<Names>();
    let mut bound = params.clone();
    let mut augmented = Names::new();
    collect_bindings(&function.body, &mut bound, &mut augmented);

    // Report the first name that `+=` rebinds without the function binding it.
    if let Some(name) = augmented.difference(&bound).next() {
        let enclosing = stack.iter().rev().find(|scope| scope.locals.contains(name));
        let message = match enclosing {
            Some(scope) => format!(
                "`{name} += ...` inside `{}` would rebind `{name}`, a variable of the enclosing function `{}`. Stone closures read enclosing variables but do not rebind them (there is no nonlocal). Keep the state in a list or dict (`{name} = [0]` in `{}`, then `{name}[0] += 1`), or return the new value",
                function.name, scope.function, scope.function
            ),
            None => format!(
                "`{name} += ...` inside `{}` rebinds `{name}`, which the function never assigns, so it is not a local (Python raises UnboundLocalError here). Functions cannot rebind module variables (there is no global). Assign it first (`{name} = 0`), pass it in and return the new value, or keep shared state in a dict (`state[\"{name}\"] += 1`)",
                function.name
            ),
        };
        return Err(scope_error(message));
    }

    stack.push(Enclosing {
        function: function.name.clone(),
        locals: bound.clone(),
    });
    let mut reads = Names::new();
    let result = resolve_stmts(&mut function.body, stack, &mut reads);
    stack.pop();
    result?;

    let free = reads.difference(&bound).cloned().collect::<Names>();
    let captures = free
        .iter()
        .filter(|name| stack.iter().any(|scope| scope.locals.contains(*name)))
        .cloned()
        .collect::<Vec<_>>();
    function.scope = std::sync::Arc::new(FunctionScope {
        locals: bound.into_iter().collect(),
        captures,
    });
    Ok(free)
}

/// Names a function body binds, not descending into nested functions or
/// lambdas (their bindings are their own). Comprehension targets are local
/// to the comprehension, as in Python 3.
fn collect_bindings(statements: &[Stmt], bound: &mut Names, augmented: &mut Names) {
    for statement in statements {
        match statement {
            Stmt::Assign { target, .. } => bind_target(target, bound),
            Stmt::AugAssign { target, .. } => {
                if let AssignTarget::Name(name) = target {
                    augmented.insert(name.clone());
                }
            }
            Stmt::For { targets, body, .. } => {
                bound.extend(targets.iter().cloned());
                collect_bindings(body, bound, augmented);
            }
            Stmt::While { body, .. } => collect_bindings(body, bound, augmented),
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                collect_bindings(then_branch, bound, augmented);
                collect_bindings(else_branch, bound, augmented);
            }
            Stmt::With { target, body, .. } => {
                if let Some(target) = target {
                    bound.insert(target.clone());
                }
                collect_bindings(body, bound, augmented);
            }
            Stmt::Try { body, handlers } => {
                collect_bindings(body, bound, augmented);
                for handler in handlers {
                    if let Some(name) = &handler.name {
                        bound.insert(name.clone());
                    }
                    collect_bindings(&handler.body, bound, augmented);
                }
            }
            Stmt::FunctionDef(function) => {
                bound.insert(function.name.clone());
            }
            Stmt::Import { names, .. } => {
                bound.extend(names.iter().map(|import| import.local.clone()));
            }
            // Record names are global, like error types.
            Stmt::Module(_) | Stmt::RecordDef(_) => {}
            Stmt::Raise(_)
            | Stmt::Return(_)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Pass
            | Stmt::Expr(_) => {}
        }
    }
}

fn bind_target(target: &AssignTarget, bound: &mut Names) {
    match target {
        AssignTarget::Name(name) => {
            bound.insert(name.clone());
        }
        AssignTarget::Tuple(names) => bound.extend(names.iter().cloned()),
        // `x[k] = v` and `x.f = v` read x; they do not bind it.
        AssignTarget::Subscript { .. } | AssignTarget::Attribute { .. } => {}
    }
}

fn resolve_stmts(
    statements: &mut [Stmt],
    stack: &mut Ctx,
    reads: &mut Names,
) -> Result<(), ShellError> {
    for statement in statements {
        match statement {
            Stmt::Assign { target, value } => {
                qualify_target(target, stack);
                read_target(target, stack, reads)?;
                resolve_expr(value, stack, reads)?;
            }
            Stmt::AugAssign { target, value, .. } => {
                qualify_target(target, stack);
                if let AssignTarget::Name(name) = target {
                    reads.insert(name.clone());
                }
                read_target(target, stack, reads)?;
                resolve_expr(value, stack, reads)?;
            }
            Stmt::Import { names, .. } => {
                for import in names {
                    stack.qualify(&mut import.local);
                }
            }
            // A linked module was resolved on its own. A record's field
            // types and literal defaults name no variables.
            Stmt::Module(_) | Stmt::RecordDef(_) => {}
            Stmt::For { iter, body, .. } => {
                resolve_expr(iter, stack, reads)?;
                resolve_stmts(body, stack, reads)?;
            }
            Stmt::While { condition, body } => {
                resolve_expr(condition, stack, reads)?;
                resolve_stmts(body, stack, reads)?;
            }
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                resolve_expr(condition, stack, reads)?;
                resolve_stmts(then_branch, stack, reads)?;
                resolve_stmts(else_branch, stack, reads)?;
            }
            Stmt::With { context, body, .. } => {
                resolve_expr(context, stack, reads)?;
                resolve_stmts(body, stack, reads)?;
            }
            Stmt::Try { body, handlers } => {
                resolve_stmts(body, stack, reads)?;
                for handler in handlers {
                    // Handler types are names read in this scope (an error
                    // class may be a local of an enclosing function).
                    for name in &mut handler.types {
                        stack.qualify(name);
                    }
                    reads.extend(handler.types.iter().cloned());
                    resolve_stmts(&mut handler.body, stack, reads)?;
                }
            }
            Stmt::FunctionDef(function) => {
                // Defaults, decorator arguments, and the base class are
                // evaluated where the def runs.
                for param in &mut function.params {
                    if let Some(default) = &mut param.default {
                        resolve_expr(default, stack, reads)?;
                    }
                }
                if let Some(stage) = &mut function.stage {
                    for expr in [
                        Some(&mut stage.evidence),
                        stage.name.as_mut(),
                        stage.goal.as_mut(),
                        stage.inputs.as_mut(),
                        stage.agent_loop.as_mut(),
                        stage.repair.as_mut(),
                        stage.max_attempts.as_mut(),
                        stage.max_actions.as_mut(),
                        stage.checkpoint.as_mut(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        resolve_expr(expr, stack, reads)?;
                    }
                }
                if let Some(base) = &mut function.error_base {
                    stack.qualify(base);
                    reads.insert(base.clone());
                }
                stack.qualify(&mut function.name);
                let free = resolve_function(function, stack)?;
                reads.extend(free);
            }
            Stmt::Raise(Some(value)) | Stmt::Return(Some(value)) | Stmt::Expr(value) => {
                resolve_expr(value, stack, reads)?;
            }
            Stmt::Raise(None) | Stmt::Return(None) | Stmt::Break | Stmt::Continue | Stmt::Pass => {}
        }
    }
    Ok(())
}

/// Qualify the module names an assignment target binds or indexes into.
fn qualify_target(target: &mut AssignTarget, stack: &Ctx) {
    match target {
        AssignTarget::Name(name) => stack.qualify(name),
        AssignTarget::Tuple(names) => {
            for name in names {
                stack.qualify(name);
            }
        }
        AssignTarget::Subscript { value, .. } | AssignTarget::Attribute { value, .. } => {
            qualify_target(value, stack)
        }
    }
}

fn read_target(
    target: &mut AssignTarget,
    stack: &mut Ctx,
    reads: &mut Names,
) -> Result<(), ShellError> {
    match target {
        AssignTarget::Name(_) | AssignTarget::Tuple(_) => Ok(()),
        AssignTarget::Subscript { value, index } => {
            read_target_base(value, reads);
            read_target(value, stack, reads)?;
            resolve_expr(index, stack, reads)
        }
        AssignTarget::Attribute { value, .. } => {
            read_target_base(value, reads);
            read_target(value, stack, reads)
        }
    }
}

fn read_target_base(target: &AssignTarget, reads: &mut Names) {
    match target {
        AssignTarget::Name(name) => {
            reads.insert(name.clone());
        }
        AssignTarget::Tuple(_) => {}
        AssignTarget::Subscript { value, .. } | AssignTarget::Attribute { value, .. } => {
            read_target_base(value, reads)
        }
    }
}

fn resolve_exprs<'a>(
    exprs: impl IntoIterator<Item = &'a mut Expr>,
    stack: &mut Ctx,
    reads: &mut Names,
) -> Result<(), ShellError> {
    for expr in exprs {
        resolve_expr(expr, stack, reads)?;
    }
    Ok(())
}

/// Resolve comprehension clauses and the expressions they scope over. The
/// clause targets are local to the comprehension.
fn resolve_comprehension(
    clauses: &mut [ComprehensionClause],
    bodies: &mut [&mut Expr],
    stack: &mut Ctx,
    reads: &mut Names,
) -> Result<(), ShellError> {
    let mut inner = Names::new();
    let mut targets = Names::new();
    stack.comprehension.push(Names::new());
    let result = (|| -> Result<(), ShellError> {
        for clause in clauses.iter_mut() {
            resolve_expr(&mut clause.iter, stack, &mut inner)?;
            targets.extend(clause.targets.iter().cloned());
            if let Some(scope) = stack.comprehension.last_mut() {
                scope.extend(clause.targets.iter().cloned());
            }
            resolve_exprs(clause.filters.iter_mut(), stack, &mut inner)?;
        }
        for body in bodies.iter_mut() {
            resolve_expr(body, stack, &mut inner)?;
        }
        Ok(())
    })();
    stack.comprehension.pop();
    result?;
    reads.extend(inner.difference(&targets).cloned());
    Ok(())
}

fn resolve_expr(expr: &mut Expr, stack: &mut Ctx, reads: &mut Names) -> Result<(), ShellError> {
    match expr {
        Expr::None | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => Ok(()),
        Expr::Name(name) => {
            stack.qualify(name);
            reads.insert(name.clone());
            Ok(())
        }
        Expr::FormattedString(parts) => {
            for part in parts {
                match part {
                    FormattedStringPart::Literal(_) => {}
                    FormattedStringPart::Expr(expr)
                    | FormattedStringPart::Formatted { expr, .. } => {
                        resolve_expr(expr, stack, reads)?;
                    }
                }
            }
            Ok(())
        }
        Expr::List(values) | Expr::Tuple(values) => resolve_exprs(values.iter_mut(), stack, reads),
        Expr::Record(fields) => {
            resolve_exprs(fields.iter_mut().map(|(_, value)| value), stack, reads)
        }
        Expr::ListComprehension { elt, clauses } | Expr::Generator { elt, clauses } => {
            resolve_comprehension(clauses, &mut [elt.as_mut()], stack, reads)
        }
        Expr::DictComprehension {
            key,
            value,
            clauses,
        } => resolve_comprehension(clauses, &mut [key.as_mut(), value.as_mut()], stack, reads),
        Expr::Subscript { value, index } => {
            resolve_expr(value, stack, reads)?;
            resolve_expr(index, stack, reads)
        }
        Expr::Attribute { value, .. }
        | Expr::Not(value)
        | Expr::Neg(value)
        | Expr::Invert(value)
        | Expr::Await(value) => resolve_expr(value, stack, reads),
        Expr::Slice { lower, upper } => {
            if let Some(lower) = lower {
                resolve_expr(lower, stack, reads)?;
            }
            if let Some(upper) = upper {
                resolve_expr(upper, stack, reads)?;
            }
            Ok(())
        }
        Expr::Compare {
            left, comparators, ..
        } => {
            resolve_expr(left, stack, reads)?;
            resolve_exprs(comparators.iter_mut(), stack, reads)
        }
        Expr::BoolOp { values, .. } => resolve_exprs(values.iter_mut(), stack, reads),
        Expr::Conditional {
            then_expr,
            condition,
            else_expr,
        } => {
            resolve_expr(then_expr, stack, reads)?;
            resolve_expr(condition, stack, reads)?;
            resolve_expr(else_expr, stack, reads)
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
            resolve_expr(left, stack, reads)?;
            resolve_expr(right, stack, reads)
        }
        Expr::Lambda {
            params,
            body,
            captures,
        } => {
            let locals = params.iter().cloned().collect::<Names>();
            stack.push(Enclosing {
                function: "lambda".to_string(),
                locals: locals.clone(),
            });
            let mut inner = Names::new();
            let result = resolve_expr(body, stack, &mut inner);
            stack.pop();
            result?;
            let free = inner.difference(&locals).cloned().collect::<Names>();
            *captures = free
                .iter()
                .filter(|name| stack.iter().any(|scope| scope.locals.contains(*name)))
                .cloned()
                .collect();
            reads.extend(free);
            Ok(())
        }
        Expr::MethodCall {
            receiver,
            method,
            positional,
            named,
        } => {
            stack.reject_import_time_call(&format!(".{method}"))?;
            resolve_expr(receiver, stack, reads)?;
            resolve_exprs(positional.iter_mut(), stack, reads)?;
            resolve_exprs(named.iter_mut().map(|(_, value)| value), stack, reads)
        }
        Expr::CallValue {
            callee,
            positional,
            named,
        } => {
            stack.reject_import_time_call("a function value")?;
            resolve_expr(callee, stack, reads)?;
            resolve_exprs(positional.iter_mut(), stack, reads)?;
            resolve_exprs(named.iter_mut().map(|(_, value)| value), stack, reads)
        }
        Expr::Call(call) => {
            // The callee's name is looked up like any other name, so an inner
            // function can call a helper defined in its enclosing function.
            stack.reject_import_time_call(&call.name)?;
            stack.qualify(&mut call.name);
            reads.insert(call.name.clone());
            resolve_exprs(call.positional.iter_mut(), stack, reads)?;
            resolve_exprs(call.named.iter_mut().map(|(_, value)| value), stack, reads)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::stone_ast::{lower_source, Expr, Stmt};

    fn function<'a>(statements: &'a [Stmt], name: &str) -> &'a crate::stone_ast::FunctionDef {
        statements
            .iter()
            .find_map(|statement| match statement {
                Stmt::FunctionDef(function) if function.name == name => Some(function),
                _ => None,
            })
            .expect("function")
    }

    #[test]
    fn captures_only_enclosing_function_locals() {
        let program = lower_source(
            "limit = 3\ndef make(n):\n    k = 1\n    def add(x):\n        return x + n + k + limit + len([])\n    return add\n",
        )
        .expect("lower");
        let make = function(&program.statements, "make");
        assert!(make.scope.captures.is_empty());
        assert_eq!(make.scope.locals, vec!["add", "k", "n"]);
        let add = function(&make.body, "add");
        assert_eq!(add.scope.captures, vec!["k", "n"]);
        assert_eq!(add.scope.locals, vec!["x"]);
    }

    #[test]
    fn nested_captures_pass_through_the_middle_function() {
        let program = lower_source(
            "def outer(a):\n    def middle():\n        def inner():\n            return a\n        return inner\n    return middle\n",
        )
        .expect("lower");
        let outer = function(&program.statements, "outer");
        let middle = function(&outer.body, "middle");
        assert_eq!(middle.scope.captures, vec!["a"]);
        let inner = function(&middle.body, "inner");
        assert_eq!(inner.scope.captures, vec!["a"]);
    }

    #[test]
    fn lambdas_capture_enclosing_locals_but_not_globals() {
        let program = lower_source(
            "field = \"x\"\nsorted(rows, key=lambda r: r[field])\ndef pick(rows, key):\n    return sorted(rows, key=lambda r: [r[key] for r in [r]])\n",
        )
        .expect("lower");
        let Stmt::Expr(Expr::Call(call)) = &program.statements[1] else {
            panic!("call");
        };
        let Expr::Lambda { captures, .. } = &call.named[0].1 else {
            panic!("lambda");
        };
        assert!(captures.is_empty());
        let pick = function(&program.statements, "pick");
        let Stmt::Return(Some(Expr::Call(call))) = &pick.body[0] else {
            panic!("return");
        };
        let Expr::Lambda { captures, .. } = &call.named[0].1 else {
            panic!("lambda");
        };
        assert_eq!(captures, &vec!["key".to_string()]);
    }

    #[test]
    fn rebinding_a_captured_or_global_name_is_rejected_with_the_fix() {
        let error = lower_source(
            "def counter():\n    count = 0\n    def inc():\n        count += 1\n        return count\n    return inc\n",
        )
        .expect_err("closure rebind");
        let text = format!("{error:?}");
        assert!(text.contains("enclosing function `counter`"), "{text}");
        assert!(text.contains("count[0] += 1"), "{text}");

        let error =
            lower_source("total = 0\ndef add(x):\n    total += x\n").expect_err("global rebind");
        assert!(format!("{error:?}").contains("UnboundLocalError"));

        // Assigning first makes it an ordinary local.
        lower_source("def f():\n    n = 0\n    n += 1\n    return n\n").expect("local");
        // Mutating a captured container is fine.
        lower_source(
            "def counter():\n    c = [0]\n    def inc():\n        c[0] += 1\n    return inc\n",
        )
        .expect("container");
    }
}
