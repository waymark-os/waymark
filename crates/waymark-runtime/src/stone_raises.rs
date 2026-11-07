// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which tool errors a block of Stone code can raise, from the declared
//! raises of the builtin syscalls it calls (`stone_errors::builtin_call_raises`)
//! and the bodies of the user functions it calls.
//!
//! The only use is an informational warning for a handler that can never run,
//! such as `except FileNotFoundError:` around `run(...)`, which reports a
//! missing command in its result instead of raising. Declared raises inform;
//! they never force a handler. The analysis is conservative: anything it
//! cannot see through (a call through a variable, an unknown method, `await`,
//! a re-raise of a caught value) makes the block "may raise anything", and
//! then no warning is given.

use std::collections::{BTreeSet, HashMap, HashSet};

use std::rc::Rc;

use crate::stone_ast::{ComprehensionClause, Expr, FormattedStringPart, FunctionDef, Stmt};
use crate::stone_errors::{
    builtin_call_raises, error_type_matches, is_builtin_error_type, is_tool_error_type,
    HIGHER_ORDER_BUILTINS,
};

/// Methods on strings, lists, and dicts. They raise only ordinary errors
/// (`KeyError`, `ValueError`), never tool errors.
const PURE_METHODS: &[&str] = &[
    "append",
    "extend",
    "insert",
    "pop",
    "remove",
    "clear",
    "copy",
    "count",
    "index",
    "get",
    "keys",
    "values",
    "items",
    "update",
    "setdefault",
    "sort",
    "reverse",
    "split",
    "rsplit",
    "splitlines",
    "strip",
    "lstrip",
    "rstrip",
    "join",
    "replace",
    "lower",
    "upper",
    "title",
    "capitalize",
    "startswith",
    "endswith",
    "find",
    "rfind",
    "format",
    "isdigit",
    "isalpha",
    "isalnum",
    "isspace",
    "zfill",
    "partition",
    "rpartition",
    "casefold",
    "removeprefix",
    "removesuffix",
];

const MAX_CALL_DEPTH: usize = 8;

type Raises = Option<BTreeSet<String>>;

struct Analyzer<'a> {
    functions: &'a HashMap<String, Rc<FunctionDef>>,
    visiting: HashSet<String>,
}

/// Warnings for handlers in `statements` (including inside function bodies)
/// that nothing in their try block can trigger. `functions` holds the user
/// functions and error classes visible to the program.
pub(crate) fn unreachable_handler_warnings(
    statements: &[Stmt],
    functions: &HashMap<String, Rc<FunctionDef>>,
) -> Vec<String> {
    let mut all_functions = functions.clone();
    collect_functions(statements, &mut all_functions);
    let mut analyzer = Analyzer {
        functions: &all_functions,
        visiting: HashSet::new(),
    };
    let mut warnings = Vec::new();
    analyzer.visit(statements, &mut warnings);
    warnings.sort();
    warnings.dedup();
    warnings
}

fn collect_functions(statements: &[Stmt], functions: &mut HashMap<String, Rc<FunctionDef>>) {
    for statement in statements {
        match statement {
            Stmt::FunctionDef(function) => {
                functions.insert(function.name.clone(), Rc::new(function.clone()));
                collect_functions(&function.body, functions);
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::With { body, .. } => {
                collect_functions(body, functions)
            }
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                collect_functions(then_branch, functions);
                collect_functions(else_branch, functions);
            }
            Stmt::Try { body, handlers } => {
                collect_functions(body, functions);
                for handler in handlers {
                    collect_functions(&handler.body, functions);
                }
            }
            _ => {}
        }
    }
}

impl Analyzer<'_> {
    fn parent(&self, name: &str) -> Option<String> {
        self.functions
            .get(name)
            .and_then(|function| function.error_base.clone())
    }

    fn visit(&mut self, statements: &[Stmt], warnings: &mut Vec<String>) {
        for statement in statements {
            match statement {
                Stmt::Try { body, handlers } => {
                    if let Some(raised) = self.stmts(body) {
                        for handler in handlers {
                            for handler_type in &handler.types {
                                if let Some(warning) =
                                    self.check_handler(handler_type, &raised, body)
                                {
                                    warnings.push(warning);
                                }
                            }
                        }
                    }
                    self.visit(body, warnings);
                    for handler in handlers {
                        self.visit(&handler.body, warnings);
                    }
                }
                Stmt::FunctionDef(function) => self.visit(&function.body, warnings),
                Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::With { body, .. } => {
                    self.visit(body, warnings)
                }
                Stmt::If {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    self.visit(then_branch, warnings);
                    self.visit(else_branch, warnings);
                }
                _ => {}
            }
        }
    }

    fn check_handler(
        &self,
        handler_type: &str,
        raised: &BTreeSet<String>,
        body: &[Stmt],
    ) -> Option<String> {
        let parent = |name: &str| self.parent(name);
        if !is_tool_error_type(handler_type, parent) {
            // Ordinary errors (KeyError, ValueError, ...) can come from almost
            // any expression; only tool errors are tracked.
            return None;
        }
        let reachable = raised.iter().any(|actual| {
            error_type_matches(actual, handler_type, parent)
                || error_type_matches(handler_type, actual, parent)
        });
        if reachable {
            // Python's subprocess raises FileNotFoundError for a missing
            // executable; run() reports that in its result instead. The
            // handler is reachable (a missing cwd), but not for that.
            if handler_type == "FileNotFoundError" && calls_any(body, &["run", "run_complete"]) {
                return Some(
                    "`except FileNotFoundError` around run() does not catch a missing command: run() returns a result with ok=False instead of raising (this handler only catches a missing cwd). Check result.ok, or call resolve_command(name) first.".to_string(),
                );
            }
            return None;
        }
        let can_raise = raised
            .iter()
            .filter(|name| is_tool_error_type(name, parent))
            .cloned()
            .collect::<Vec<_>>();
        let can_raise = if can_raise.is_empty() {
            "no tool errors".to_string()
        } else {
            can_raise.join(", ")
        };
        let mut warning = format!(
            "`except {handler_type}` can never run: nothing in its try block raises {handler_type} (the calls there can raise: {can_raise})."
        );
        if calls_any(body, &["run", "run_complete"]) && handler_type != "ExpectationFailed" {
            warning.push_str(
                " run() does not raise when a command fails or is missing; it returns a result with ok=False and exit_code. Check result.ok, or pass expect=Expect(exit=0) to raise ExpectationFailed.",
            );
        } else if handler_type == "ExpectationFailed" {
            warning.push_str(
                " Only calls with expect=, must_run(), and edit() raise ExpectationFailed.",
            );
        } else {
            warning.push_str(" Remove the handler, or catch an error those calls raise.");
        }
        Some(warning)
    }

    fn stmts(&mut self, statements: &[Stmt]) -> Raises {
        let mut raised = BTreeSet::new();
        for statement in statements {
            raised.extend(self.stmt(statement)?);
        }
        Some(raised)
    }

    fn stmt(&mut self, statement: &Stmt) -> Raises {
        match statement {
            Stmt::Assign { value, .. } | Stmt::AugAssign { value, .. } | Stmt::Expr(value) => {
                self.expr(value)
            }
            Stmt::Return(Some(value)) => self.expr(value),
            Stmt::For { iter, body, .. } => union(self.expr(iter), self.stmts(body)),
            Stmt::While { condition, body } => union(self.expr(condition), self.stmts(body)),
            Stmt::If {
                condition,
                then_branch,
                else_branch,
            } => union(
                self.expr(condition),
                union(self.stmts(then_branch), self.stmts(else_branch)),
            ),
            Stmt::With {
                is_async: false,
                context,
                body,
                ..
            } => union(self.expr(context), self.stmts(body)),
            Stmt::With { is_async: true, .. } => None,
            Stmt::Try { body, handlers } => {
                // Over-approximate: whatever the inner block or its handlers
                // raise may escape.
                let mut raised = self.stmts(body)?;
                for handler in handlers {
                    raised.extend(self.stmts(&handler.body)?);
                }
                Some(raised)
            }
            Stmt::Raise(Some(Expr::Name(name))) if self.is_error_type(name) => {
                Some(BTreeSet::from([name.clone()]))
            }
            Stmt::Raise(Some(Expr::Call(call))) if self.is_error_type(&call.name) => {
                let mut raised = self.exprs(call.positional.iter())?;
                raised.extend(self.exprs(call.named.iter().map(|(_, value)| value))?);
                raised.insert(call.name.clone());
                Some(raised)
            }
            // Re-raising a caught value: its type is not known here.
            Stmt::Raise(_) => None,
            Stmt::FunctionDef(_)
            | Stmt::Return(None)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Pass
            | Stmt::Import { .. }
            | Stmt::Module(_) => Some(BTreeSet::new()),
        }
    }

    fn is_error_type(&self, name: &str) -> bool {
        is_builtin_error_type(name) || self.parent(name).is_some()
    }

    fn exprs<'e>(&mut self, exprs: impl IntoIterator<Item = &'e Expr>) -> Raises {
        let mut raised = BTreeSet::new();
        for expr in exprs {
            raised.extend(self.expr(expr)?);
        }
        Some(raised)
    }

    fn clauses(&mut self, clauses: &[ComprehensionClause]) -> Raises {
        let mut raised = BTreeSet::new();
        for clause in clauses {
            raised.extend(self.expr(&clause.iter)?);
            raised.extend(self.exprs(clause.filters.iter())?);
        }
        Some(raised)
    }

    fn expr(&mut self, expr: &Expr) -> Raises {
        match expr {
            Expr::None
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::String(_)
            | Expr::Name(_) => Some(BTreeSet::new()),
            Expr::FormattedString(parts) => {
                self.exprs(parts.iter().filter_map(|part| match part {
                    FormattedStringPart::Literal(_) => None,
                    FormattedStringPart::Expr(expr)
                    | FormattedStringPart::Formatted { expr, .. } => Some(expr),
                }))
            }
            Expr::List(values) | Expr::Tuple(values) => self.exprs(values.iter()),
            Expr::Record(fields) => self.exprs(fields.iter().map(|(_, value)| value)),
            Expr::ListComprehension { elt, clauses } | Expr::Generator { elt, clauses } => {
                union(self.expr(elt), self.clauses(clauses))
            }
            Expr::DictComprehension {
                key,
                value,
                clauses,
            } => union(
                union(self.expr(key), self.expr(value)),
                self.clauses(clauses),
            ),
            Expr::Subscript { value, index } => union(self.expr(value), self.expr(index)),
            Expr::Attribute { value, .. }
            | Expr::Not(value)
            | Expr::Neg(value)
            | Expr::Invert(value) => self.expr(value),
            Expr::Slice { lower, upper } => union(
                lower
                    .as_deref()
                    .map_or(Some(BTreeSet::new()), |e| self.expr(e)),
                upper
                    .as_deref()
                    .map_or(Some(BTreeSet::new()), |e| self.expr(e)),
            ),
            Expr::Compare {
                left, comparators, ..
            } => union(self.expr(left), self.exprs(comparators.iter())),
            Expr::BoolOp { values, .. } => self.exprs(values.iter()),
            Expr::Conditional {
                then_expr,
                condition,
                else_expr,
            } => union(
                self.expr(condition),
                union(self.expr(then_expr), self.expr(else_expr)),
            ),
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
            | Expr::RShift { left, right } => union(self.expr(left), self.expr(right)),
            // A lambda's body may run wherever the lambda is passed.
            Expr::Lambda { body, .. } => self.expr(body),
            Expr::Await(_) => None,
            // The callee is not known statically: it may raise anything.
            Expr::CallValue { .. } => None,
            Expr::MethodCall {
                receiver,
                method,
                positional,
                named,
            } => {
                if !PURE_METHODS.contains(&method.as_str()) {
                    return None;
                }
                union(
                    self.expr(receiver),
                    union(
                        self.exprs(positional.iter()),
                        self.exprs(named.iter().map(|(_, value)| value)),
                    ),
                )
            }
            Expr::Call(call) => {
                let args = union(
                    self.exprs(call.positional.iter()),
                    self.exprs(call.named.iter().map(|(_, value)| value)),
                );
                if let Some(function) = self.functions.get(&call.name) {
                    if function.error_base.is_some() {
                        // Constructing an error does not raise it.
                        return args;
                    }
                    if function.stage.is_some()
                        || function.is_async
                        || self.visiting.contains(&call.name)
                        || self.visiting.len() >= MAX_CALL_DEPTH
                    {
                        return None;
                    }
                    let function = function.clone();
                    self.visiting.insert(call.name.clone());
                    let body = self.stmts(&function.body);
                    let defaults = self.exprs(
                        function
                            .params
                            .iter()
                            .filter_map(|parameter| parameter.default.as_ref()),
                    );
                    self.visiting.remove(&call.name);
                    return union(args, union(body, defaults));
                }
                if HIGHER_ORDER_BUILTINS.contains(&call.name.as_str())
                    && call
                        .positional
                        .iter()
                        .chain(call.named.iter().map(|(_, value)| value))
                        .any(|argument| matches!(argument, Expr::Name(_)))
                {
                    // A bare name may be a function that does I/O.
                    return None;
                }
                let has_expect = call.named.iter().any(|(name, _)| name == "expect");
                let declared = builtin_call_raises(&call.name, has_expect)?;
                let mut raised = args?;
                raised.extend(declared.iter().map(|name| (*name).to_string()));
                Some(raised)
            }
        }
    }
}

fn union(left: Raises, right: Raises) -> Raises {
    let mut left = left?;
    left.extend(right?);
    Some(left)
}

fn calls_any(statements: &[Stmt], names: &[&str]) -> bool {
    fn expr_calls(expr: &Expr, names: &[&str]) -> bool {
        match expr {
            Expr::Call(call) => {
                names.contains(&call.name.as_str())
                    || call.positional.iter().any(|e| expr_calls(e, names))
                    || call.named.iter().any(|(_, e)| expr_calls(e, names))
            }
            Expr::Await(inner) | Expr::Attribute { value: inner, .. } => expr_calls(inner, names),
            Expr::Subscript { value, index } => {
                expr_calls(value, names) || expr_calls(index, names)
            }
            _ => false,
        }
    }
    statements.iter().any(|statement| match statement {
        Stmt::Assign { value, .. } | Stmt::AugAssign { value, .. } | Stmt::Expr(value) => {
            expr_calls(value, names)
        }
        Stmt::Return(Some(value)) => expr_calls(value, names),
        Stmt::For { iter, body, .. } => expr_calls(iter, names) || calls_any(body, names),
        Stmt::While { condition, body } => expr_calls(condition, names) || calls_any(body, names),
        Stmt::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_calls(condition, names)
                || calls_any(then_branch, names)
                || calls_any(else_branch, names)
        }
        Stmt::With { context, body, .. } => expr_calls(context, names) || calls_any(body, names),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stone_ast::lower_source;

    fn warnings(source: &str) -> Vec<String> {
        let program = lower_source(source).expect("lower");
        unreachable_handler_warnings(&program.statements, &HashMap::new())
    }

    #[test]
    fn handler_around_run_for_missing_command_is_flagged() {
        let found = warnings(
            "try:\n    r = run([\"tool\"])\nexcept FileNotFoundError as e:\n    print(e.message)\n",
        );
        // Reachable (a missing cwd) but not for a missing command.
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("does not catch a missing command"),
            "{found:?}"
        );
        let found = warnings(
            "try:\n    r = run([\"tool\"])\nexcept PermissionError as e:\n    print(e.message)\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("run() does not raise"), "{found:?}");
    }

    #[test]
    fn expectation_failed_needs_expect_or_checked_calls() {
        let found = warnings(
            "try:\n    r = run([\"make\"])\nexcept ExpectationFailed as e:\n    print(e.message)\n",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("Only calls with expect="), "{found:?}");
        for source in [
            "try:\n    r = run([\"make\"], expect=Expect(exit=0))\nexcept ExpectationFailed as e:\n    print(e.message)\n",
            "try:\n    must_run([\"make\"])\nexcept ExpectationFailed as e:\n    print(e.message)\n",
            "try:\n    edit(\"a.py\", \"x\", \"y\")\nexcept ExpectationFailed as e:\n    print(e.message)\n",
        ] {
            assert!(warnings(source).is_empty(), "{source}");
        }
    }

    #[test]
    fn pure_code_and_user_functions_are_seen_through() {
        let found =
            warnings("try:\n    n = int(text)\nexcept OSError as e:\n    print(e.message)\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("no tool errors"), "{found:?}");
        // Through a user function that reads a file: reachable.
        let source = "def load(p):\n    return read_text(p)\n\ntry:\n    t = load(\"x\")\nexcept FileNotFoundError as e:\n    print(e.message)\n";
        assert!(warnings(source).is_empty());
        // Ordinary error handlers are never flagged.
        assert!(
            warnings("try:\n    n = int(text)\nexcept ValueError as e:\n    n = 0\n").is_empty()
        );
    }

    #[test]
    fn unknown_calls_suppress_the_warning() {
        for source in [
            "try:\n    f()\nexcept OSError as e:\n    print(e.message)\n",
            "try:\n    h.wait()\nexcept OSError as e:\n    print(e.message)\n",
            "try:\n    xs = map(loader, paths)\nexcept OSError as e:\n    print(e.message)\n",
            "try:\n    raise err\nexcept OSError as e:\n    print(e.message)\n",
        ] {
            assert!(warnings(source).is_empty(), "{source}");
        }
    }

    #[test]
    fn user_tool_error_classes_are_reachable_only_when_raised() {
        let source = "class Flaky(ToolError):\n    pass\n\ntry:\n    n = len(xs)\nexcept Flaky as e:\n    print(e.message)\n";
        assert_eq!(warnings(source).len(), 1);
        let source = "class Flaky(ToolError):\n    pass\n\ntry:\n    raise Flaky(\"x\")\nexcept Flaky as e:\n    print(e.message)\n";
        assert!(warnings(source).is_empty());
    }
}
