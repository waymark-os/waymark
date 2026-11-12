//! `predicate(text)`: compile one pure Stone lambda from text.
//!
//! A program sometimes holds a check written by someone else, most often a
//! model filling in a typed record: "the result passes when ...". Text is
//! the only way such a check can arrive. `predicate` turns it into a function
//! value, but only when the lambda is a plain expression over its own
//! parameters: no effects, no names from the calling program, no calls
//! except a fixed set of pure builtins and methods. So the check cannot run
//! commands, call models, read files, or see the program's variables, and it
//! stays an ordinary expression the evaluator (and a later compiler) treats
//! like any other lambda.

use nu_protocol::ShellError;

use super::stone_error;
use crate::stone_ast::{lower_source, ComprehensionClause, Expr, FormattedStringPart, Stmt};

/// Builtins a predicate may call: they only compute from their arguments.
const PREDICATE_CALLS: &[&str] = &[
    "all", "any", "first", "float", "int", "isinstance", "last", "len", "list", "max", "min",
    "repr", "round", "set", "sorted", "str", "sum", "tuple", "unique", "abs", "reversed",
];

/// Methods a predicate may call: string, list, dict, and set queries.
const PREDICATE_METHODS: &[&str] = &[
    "count", "endswith", "find", "rfind", "get", "index", "isdigit", "items", "join", "keys", "lower",
    "lstrip", "replace", "rstrip", "split", "splitlines", "startswith", "strip", "upper",
    "values",
];

/// Parse `text` as one lambda and check that it is pure. Returns its
/// parameters and body.
pub(super) fn compile_predicate(text: &str) -> Result<(Vec<String>, Expr), ShellError> {
    let program = lower_source(text.trim()).map_err(|err| {
        predicate_error(format!(
            "the predicate text does not parse as Stone: {err}. Write one lambda, as in `lambda r: r.exit_code == 0`"
        ))
    })?;
    let [Stmt::Expr(Expr::Lambda { params, body, .. })] = program.statements.as_slice() else {
        return Err(predicate_error(
            "the predicate text must be exactly one lambda expression, as in `lambda r: r.exit_code == 0`".to_string(),
        ));
    };
    if params.is_empty() {
        return Err(predicate_error(
            "a predicate takes at least one parameter: the value it judges".to_string(),
        ));
    }
    let mut bound = params.clone();
    check_pure(body, &mut bound).map_err(predicate_error)?;
    Ok((params.clone(), (**body).clone()))
}

fn predicate_error(message: String) -> ShellError {
    stone_error("predicate", message)
}

fn not_allowed(what: &str) -> String {
    format!(
        "{what} is not allowed in a predicate. A predicate is one pure expression over its parameters: comparisons, and/or/not, arithmetic, indexing, attributes, f-strings, comprehensions, and the pure builtins {} and methods {}",
        PREDICATE_CALLS.join(", "),
        PREDICATE_METHODS.join(", ")
    )
}

fn check_pure(expr: &Expr, bound: &mut Vec<String>) -> Result<(), String> {
    match expr {
        Expr::None | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => Ok(()),
        Expr::FormattedString(parts) => {
            for part in parts {
                match part {
                    FormattedStringPart::Literal(_) => {}
                    FormattedStringPart::Expr(expr)
                    | FormattedStringPart::Formatted { expr, .. } => check_pure(expr, bound)?,
                }
            }
            Ok(())
        }
        Expr::List(items) | Expr::Tuple(items) => check_all(items, bound),
        Expr::Record(fields) => {
            for (_, value) in fields {
                check_pure(value, bound)?;
            }
            Ok(())
        }
        Expr::Name(name) => {
            if bound.iter().any(|bound| bound == name) {
                Ok(())
            } else {
                Err(format!(
                    "`{name}` is not available in a predicate: it sees only its own parameters and the names its comprehensions bind"
                ))
            }
        }
        Expr::Subscript { value, index } => {
            check_pure(value, bound)?;
            check_pure(index, bound)
        }
        Expr::Attribute { value, .. } => check_pure(value, bound),
        Expr::Slice { lower, upper, step } => {
            for part in [lower, upper, step].into_iter().flatten() {
                check_pure(part, bound)?;
            }
            Ok(())
        }
        Expr::Compare {
            left, comparators, ..
        } => {
            check_pure(left, bound)?;
            check_all(comparators, bound)
        }
        Expr::BoolOp { values, .. } => check_all(values, bound),
        Expr::Conditional {
            then_expr,
            condition,
            else_expr,
        } => {
            check_pure(then_expr, bound)?;
            check_pure(condition, bound)?;
            check_pure(else_expr, bound)
        }
        Expr::Not(value) | Expr::Neg(value) | Expr::Invert(value) => check_pure(value, bound),
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
            check_pure(left, bound)?;
            check_pure(right, bound)
        }
        Expr::ListComprehension { elt, clauses } | Expr::Generator { elt, clauses } => {
            with_clauses(clauses, bound, |bound| check_pure(elt, bound))
        }
        Expr::DictComprehension {
            key,
            value,
            clauses,
        } => with_clauses(clauses, bound, |bound| {
            check_pure(key, bound)?;
            check_pure(value, bound)
        }),
        Expr::MethodCall {
            receiver,
            method,
            positional,
            named,
        } => {
            if !PREDICATE_METHODS.contains(&method.as_str()) {
                return Err(not_allowed(&format!("the method `.{method}()`")));
            }
            check_pure(receiver, bound)?;
            check_all(positional, bound)?;
            for (_, value) in named {
                check_pure(value, bound)?;
            }
            Ok(())
        }
        Expr::Call(call) => {
            if !PREDICATE_CALLS.contains(&call.name.as_str())
                || bound.iter().any(|bound| bound == &call.name)
            {
                return Err(not_allowed(&format!("calling `{}()`", call.name)));
            }
            check_all(&call.positional, bound)?;
            for (_, value) in &call.named {
                check_pure(value, bound)?;
            }
            Ok(())
        }
        Expr::Lambda { .. } => Err(not_allowed("a nested lambda")),
        Expr::Await(_) => Err(not_allowed("`await`")),
        Expr::CallValue { .. } => Err(not_allowed("calling a computed value")),
    }
}

fn check_all(items: &[Expr], bound: &mut Vec<String>) -> Result<(), String> {
    for item in items {
        check_pure(item, bound)?;
    }
    Ok(())
}

/// Check comprehension clauses in order, binding each clause's targets for
/// the clauses after it and for the body, then unbind them.
fn with_clauses(
    clauses: &[ComprehensionClause],
    bound: &mut Vec<String>,
    body: impl FnOnce(&mut Vec<String>) -> Result<(), String>,
) -> Result<(), String> {
    let depth = bound.len();
    let mut result = Ok(());
    for clause in clauses {
        result = check_pure(&clause.iter, bound);
        if result.is_err() {
            break;
        }
        bound.extend(clause.targets.iter().cloned());
        result = check_all(&clause.filters, bound);
        if result.is_err() {
            break;
        }
    }
    if result.is_ok() {
        result = body(bound);
    }
    bound.truncate(depth);
    result
}

#[cfg(test)]
mod tests {
    use super::compile_predicate;

    fn rejected(text: &str) -> String {
        match compile_predicate(text) {
            Ok(_) => panic!("expected `{text}` to be rejected"),
            Err(err) => format!("{err:?}"),
        }
    }

    #[test]
    fn accepts_pure_expressions_over_parameters() {
        for text in [
            "lambda r: r.exit_code == 0",
            "lambda r: r.exit_code == 0 and \"b'Hello'\" in r.stdout",
            "lambda r: not any(w in r.stderr for w in ['BytesWarning', 'Error'])",
            "lambda r: len([l for l in r.stdout.splitlines() if l.strip().startswith('SETUP')]) >= 2",
            "lambda a, b: a['n'] + 1 == b[0]",
            "lambda r: f\"{r.exit_code}\" == '0'",
        ] {
            let (params, _) = compile_predicate(text).unwrap_or_else(|err| panic!("{text}: {err:?}"));
            assert!(!params.is_empty());
        }
    }

    #[test]
    fn rejects_effects_and_outside_names() {
        assert!(rejected("lambda r: run(['ls']).exit_code == 0").contains("calling `run()`"));
        assert!(rejected("lambda r: r.exit_code == limit").contains("`limit` is not available"));
        assert!(rejected("lambda r: print(r)").contains("calling `print()`"));
        assert!(rejected("lambda r: r.items.append(1)").contains("the method `.append()`"));
        assert!(rejected("lambda r: (lambda: 1)()").contains("not allowed"));
        assert!(rejected("lambda r: r(1)").contains("not allowed"));
        assert!(rejected("lambda: True").contains("at least one parameter"));
        assert!(rejected("x = 1").contains("exactly one lambda"));
        assert!(rejected("lambda r: r.exit_code == 0\nprint(1)").contains("exactly one lambda"));
        assert!(rejected("lambda r: r ===").contains("does not parse"));
        // A comprehension variable is bound only inside its comprehension.
        assert!(rejected("lambda r: [x for x in r] == x").contains("`x` is not available"));
    }
}
