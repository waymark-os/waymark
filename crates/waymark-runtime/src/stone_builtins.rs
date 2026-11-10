// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cmp::Ordering;
use std::collections::HashSet;

use nu_protocol::{shell_error::generic::GenericError, ShellError, Span, Value};

use crate::json::nu_to_json_value;
use crate::stone_ast::CompareOp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SortKeyKind {
    Number,
    Text,
    Composite,
}

/// `int(x)`: the one place text and floats become ints. A float is
/// truncated toward zero, as in Python; NaN, infinities, and floats outside
/// the 64-bit range raise instead of saturating.
pub(crate) fn value_to_int(value: &Value) -> Result<Value, ShellError> {
    let int = match value {
        Value::Int { val, .. } => *val,
        Value::Bool { val, .. } => i64::from(*val),
        Value::Float { val, .. } => float_to_i64(*val, "int")?,
        Value::String { val, .. } => {
            val.trim().parse::<i64>().map_err(|_| {
                typed_error(
                    "ValueError",
                    "int",
                    format!("invalid literal for int(): {}", python_quote(val)),
                )
            })?
        }
        other => {
            return Err(typed_error(
                "TypeError",
                "int",
                format!("int() argument must be a string or a number, not {}", value_type_name(other)),
            ))
        }
    };
    Ok(Value::int(int, Span::unknown()))
}

fn float_to_i64(value: f64, context: &str) -> Result<i64, ShellError> {
    if value.is_nan() {
        return Err(typed_error("ValueError", context, "cannot convert float NaN to integer"));
    }
    let truncated = value.trunc();
    // i64::MAX is not exactly representable; 2^63 is the first float past it.
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&truncated) {
        return Err(typed_error(
            "ValueError",
            context,
            format!("cannot convert float {value} to integer: Stone ints are 64-bit"),
        ));
    }
    Ok(truncated as i64)
}

/// The character offset of byte offset `byte` in `text`.
fn char_offset(text: &str, byte: usize) -> i64 {
    text[..byte].chars().count() as i64
}

fn python_quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn typed_error(error_type: &str, kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code(crate::stone_errors::typed_error_code(error_type)),
    )
}

/// The error for a non-int where an int is required (`range(2.7)`,
/// `xs[:n/2]`, `timeout_ms="500"`). Nothing is converted implicitly.
fn expected_int_error(value: &Value, context: &str) -> ShellError {
    let hint = match value {
        Value::Float { .. } => "; convert explicitly with int(x), round(x), or use // for integer division",
        Value::String { .. } => "; convert text explicitly with int(text)",
        Value::Bool { .. } => "; convert explicitly with int(flag)",
        _ => "",
    };
    typed_error(
        "TypeError",
        context,
        format!("{context}: expected int, got {}{hint}", value_type_name(value)),
    )
}

pub(crate) fn int_builtin(value: &Value) -> Result<Value, ShellError> {
    value_to_int(value)
}

pub(crate) fn index_method_builtin(receiver: &Value, args: &[Value]) -> Result<Value, ShellError> {
    match receiver {
        Value::List { vals, .. } => {
            let [needle] = args else {
                return Err(stone_error(
                    "index",
                    "list index() requires exactly one argument",
                ));
            };
            for (index, value) in vals.iter().enumerate() {
                if checked_values_equal(value, needle)? {
                    return Ok(Value::int(index as i64, Span::unknown()));
                }
            }
            Err(typed_error("ValueError", "index", "value is not in list"))
        }
        Value::String { val, .. } => {
            let ([needle] | [needle, _]) = args else {
                return Err(stone_error(
                    "index",
                    "string index() requires a substring and optional start",
                ));
            };
            let needle = value_to_string(needle, "index")?;
            let start = match args {
                [_] => 0,
                [_, start] => {
                    let start = value_to_i64(start, "index")?;
                    normalize_string_start(start, val.chars().count())?
                }
                _ => unreachable!(),
            };
            let prefix_len = val
                .char_indices()
                .nth(start)
                .map(|(index, _)| index)
                .unwrap_or_else(|| val.len());
            // Offsets count characters, as slicing does: "é-x".index("x") is 2.
            val[prefix_len..]
                .find(&needle)
                .map(|index| Value::int(char_offset(val, prefix_len + index), Span::unknown()))
                .ok_or_else(|| typed_error("ValueError", "index", "substring not found"))
        }
        other => Err(stone_error(
            "index",
            format!("{} has no index()", crate::stone_builtins::value_type_name(&other)),
        )),
    }
}

pub(crate) fn find_method_builtin(receiver: &Value, args: &[Value]) -> Result<Value, ShellError> {
    let Value::String { val, .. } = receiver else {
        return Err(stone_error(
            "find",
            format!("{} has no find()", crate::stone_builtins::value_type_name(&receiver)),
        ));
    };
    let ([needle] | [needle, _]) = args else {
        return Err(stone_error(
            "find",
            "string find() requires a substring and optional start",
        ));
    };
    let needle = value_to_string(needle, "find")?;
    let start = match args {
        [_] => 0,
        [_, start] => {
            let start = value_to_i64(start, "find")?;
            normalize_string_start(start, val.chars().count())?
        }
        _ => unreachable!(),
    };
    let prefix_len = val
        .char_indices()
        .nth(start)
        .map(|(index, _)| index)
        .unwrap_or_else(|| val.len());
    let index = val[prefix_len..]
        .find(&needle)
        .map(|index| char_offset(val, prefix_len + index))
        .unwrap_or(-1);
    Ok(Value::int(index, Span::unknown()))
}

pub(crate) fn float_builtin(value: &Value) -> Result<Value, ShellError> {
    convert_to_f64(value, "float").map(|value| Value::float(value, Span::unknown()))
}

pub(crate) fn format_builtin(template: &Value, args: &[Value]) -> Result<Value, ShellError> {
    let template = value_to_string(template, "format")?;
    format_template(&template, &args).map(|text| Value::string(text, Span::unknown()))
}

pub(crate) fn compare_values(
    left: &Value,
    op: CompareOp,
    right: &Value,
) -> Result<bool, ShellError> {
    match op {
        CompareOp::Eq => checked_values_equal(left, right),
        CompareOp::NotEq => checked_values_equal(left, right).map(|equal| !equal),
        CompareOp::Is => Ok(values_equal(left, right)),
        CompareOp::IsNot => Ok(!values_equal(left, right)),
        CompareOp::In => value_contains(right, left),
        CompareOp::NotIn => value_contains(right, left).map(|value| !value),
        CompareOp::Lt | CompareOp::LtE | CompareOp::Gt | CompareOp::GtE => {
            let ordering = value_ordering(left, right)?;
            Ok(match op {
                CompareOp::Lt => ordering == Ordering::Less,
                CompareOp::LtE => ordering != Ordering::Greater,
                CompareOp::Gt => ordering == Ordering::Greater,
                CompareOp::GtE => ordering != Ordering::Less,
                CompareOp::Eq
                | CompareOp::NotEq
                | CompareOp::In
                | CompareOp::NotIn
                | CompareOp::Is
                | CompareOp::IsNot => {
                    unreachable!()
                }
            })
        }
    }
}

pub(crate) fn add_values(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left
            .checked_add(*right)
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("addition", "integer addition overflow")),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(left + right, Span::unknown()))
        }
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(*left as f64 + right, Span::unknown()))
        }
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => {
            Ok(Value::float(left + *right as f64, Span::unknown()))
        }
        (Value::String { val: left, .. }, Value::String { val: right, .. }) => {
            Ok(Value::string(format!("{left}{right}"), Span::unknown()))
        }
        (Value::List { vals: left, .. }, Value::List { vals: right, .. }) => {
            let mut values = left.clone();
            values.extend(right.clone());
            Ok(Value::list(values, Span::unknown()))
        }
        _ => Err(stone_error(
            "addition",
            format!("cannot add {} and {}", crate::stone_builtins::value_type_name(&left), crate::stone_builtins::value_type_name(&right)),
        )),
    }
}

pub(crate) fn bitwise_int_values(
    left: &Value,
    right: &Value,
    context: &str,
    op: impl FnOnce(i64, i64) -> i64,
) -> Result<Value, ShellError> {
    let left = value_to_operator_i64(left, context)?;
    let right = value_to_operator_i64(right, context)?;
    Ok(Value::int(op(left, right), Span::unknown()))
}

pub(crate) fn div_values(left: &Value, right: &Value) -> Result<Value, ShellError> {
    let (left, right) = match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => {
            (*left as f64, *right as f64)
        }
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => (*left, *right),
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => (*left as f64, *right),
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => (*left, *right as f64),
        _ => {
            return Err(stone_error(
                "division",
                format!("cannot divide {} and {}", crate::stone_builtins::value_type_name(&left), crate::stone_builtins::value_type_name(&right)),
            ));
        }
    };
    if right == 0.0 {
        return Err(stone_error("division", "division by zero"));
    }
    Ok(Value::float(left / right, Span::unknown()))
}

pub(crate) fn enumerate_builtin(values: Vec<Value>, start: i64) -> Result<Value, ShellError> {
    let mut output = Vec::with_capacity(values.len());
    for (offset, value) in values.into_iter().enumerate() {
        let index = start
            .checked_add(
                i64::try_from(offset)
                    .map_err(|_| stone_error("enumerate", "index is too large"))?,
            )
            .ok_or_else(|| stone_error("enumerate", "index overflow"))?;
        output.push(Value::list(
            vec![Value::int(index, Span::unknown()), value],
            Span::unknown(),
        ));
    }
    Ok(Value::list(output, Span::unknown()))
}

pub(crate) fn floor_div_values(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => {
            if *right == 0 {
                return Err(stone_error("floor division", "division by zero"));
            }
            python_floor_div_i64(*left, *right).map(|value| Value::int(value, Span::unknown()))
        }
        _ => {
            let Value::Float { val, .. } = div_values(left, right)? else {
                unreachable!("div_values returns a float")
            };
            Ok(Value::float(val.floor(), Span::unknown()))
        }
    }
}

pub(crate) fn mod_values(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => {
            if *right == 0 {
                return Err(stone_error("modulo", "modulo by zero"));
            }
            python_mod_i64(*left, *right).map(|value| Value::int(value, Span::unknown()))
        }
        _ => {
            let (left, right) = match (left, right) {
                (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => {
                    (*left as f64, *right)
                }
                (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => {
                    (*left, *right as f64)
                }
                (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => {
                    (*left, *right)
                }
                _ => {
                    return Err(stone_error(
                        "modulo",
                        format!("cannot modulo {} and {}", crate::stone_builtins::value_type_name(&left), crate::stone_builtins::value_type_name(&right)),
                    ));
                }
            };
            if right == 0.0 {
                return Err(stone_error("modulo", "modulo by zero"));
            }
            Ok(Value::float(
                left - (left / right).floor() * right,
                Span::unknown(),
            ))
        }
    }
}

pub(crate) fn mul_values(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left
            .checked_mul(*right)
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("multiplication", "integer multiplication overflow")),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(left * right, Span::unknown()))
        }
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(*left as f64 * right, Span::unknown()))
        }
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => {
            Ok(Value::float(left * *right as f64, Span::unknown()))
        }
        _ => Err(stone_error(
            "multiplication",
            format!(
                "cannot multiply {} and {}",
                crate::stone_builtins::value_type_name(&left),
                crate::stone_builtins::value_type_name(&right)
            ),
        )),
    }
}

pub(crate) fn neg_value(value: &Value) -> Result<Value, ShellError> {
    match value {
        Value::Int { val, .. } => val
            .checked_neg()
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("unary minus", "integer negation overflow")),
        Value::Float { val, .. } => Ok(Value::float(-val, Span::unknown())),
        other => Err(stone_error(
            "unary minus",
            format!("cannot negate {}", crate::stone_builtins::value_type_name(&other)),
        )),
    }
}

pub(crate) fn shift_value(
    left: &Value,
    right: &Value,
    context: &str,
    op: impl FnOnce(i64, u32) -> Option<i64>,
) -> Result<Value, ShellError> {
    let left = value_to_operator_i64(left, context)?;
    let right = value_to_operator_i64(right, context)?;
    let shift = u32::try_from(right)
        .map_err(|_| stone_error(context, "shift count must be non-negative"))?;
    let value = op(left, shift).ok_or_else(|| stone_error(context, "shift count is too large"))?;
    // checked_shl only rejects counts >= 64; bits shifted out of an int are
    // an overflow, as for the other integer operators.
    if context == "left shift" && value >> shift != left {
        return Err(stone_error(context, "integer left shift overflow"));
    }
    Ok(Value::int(value, Span::unknown()))
}

pub(crate) fn sub_values(left: &Value, right: &Value) -> Result<Value, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left
            .checked_sub(*right)
            .map(|value| Value::int(value, Span::unknown()))
            .ok_or_else(|| stone_error("subtraction", "integer subtraction overflow")),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(left - right, Span::unknown()))
        }
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => {
            Ok(Value::float(*left as f64 - right, Span::unknown()))
        }
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => {
            Ok(Value::float(left - *right as f64, Span::unknown()))
        }
        _ => Err(stone_error(
            "subtraction",
            format!(
                "cannot subtract {} and {}",
                crate::stone_builtins::value_type_name(&left),
                crate::stone_builtins::value_type_name(&right)
            ),
        )),
    }
}

pub(crate) fn first_builtin(
    value: &Value,
    count: Option<usize>,
    name: &str,
) -> Result<Value, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            name,
            format!("expected list, got {}", crate::stone_builtins::value_type_name(&value)),
        ));
    };
    Ok(match count {
        Some(count) => Value::list(vals.iter().take(count).cloned().collect(), Span::unknown()),
        None => vals
            .first()
            .cloned()
            .unwrap_or_else(|| Value::nothing(Span::unknown())),
    })
}

pub(crate) fn last_builtin(
    value: &Value,
    count: Option<usize>,
    name: &str,
) -> Result<Value, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            name,
            format!("expected list, got {}", crate::stone_builtins::value_type_name(&value)),
        ));
    };
    Ok(match count {
        Some(count) => {
            let start = vals.len().saturating_sub(count);
            Value::list(vals.iter().skip(start).cloned().collect(), Span::unknown())
        }
        None => vals
            .last()
            .cloned()
            .unwrap_or_else(|| Value::nothing(Span::unknown())),
    })
}

pub(crate) fn join_builtin(first: &Value, second: Option<&Value>) -> Result<Value, ShellError> {
    let (items, separator) = match second {
        None => (first, String::new()),
        Some(second) => match (first, second) {
            (Value::List { .. }, _) => (first, value_to_string(second, "join")?),
            (_, Value::List { .. }) => (second, value_to_string(first, "join")?),
            _ => {
                return Err(stone_error(
                    "join",
                    "join() requires a list and optional separator",
                ));
            }
        },
    };
    let Value::List { vals, .. } = items else {
        return Err(stone_error(
            "join",
            format!("expected list, got {}", crate::stone_builtins::value_type_name(&items)),
        ));
    };
    let mut parts = Vec::with_capacity(vals.len());
    for (index, value) in vals.iter().enumerate() {
        let Value::String { val, .. } = value else {
            return Err(typed_error(
                "TypeError",
                "join",
                format!(
                    "join: item {index} is {}, not str; convert items first: \",\".join([str(x) for x in items])",
                    value_type_name(value)
                ),
            ));
        };
        parts.push(val.clone());
    }
    Ok(Value::string(parts.join(&separator), Span::unknown()))
}

pub(crate) fn len_builtin(value: &Value) -> Result<Value, ShellError> {
    value_len(value).map(|len| Value::int(len, Span::unknown()))
}

pub(crate) fn list_builtin(value: &Value) -> Result<Value, ShellError> {
    match value {
        Value::List { vals, .. } => Ok(Value::list(vals.clone(), Span::unknown())),
        Value::Record { .. } => record_method_builtin(value, "keys", &[]),
        // list("ab") is ["a", "b"], as in Python.
        Value::String { val, .. } => Ok(Value::list(
            val.chars().map(|ch| Value::string(ch.to_string(), Span::unknown())).collect(),
            Span::unknown(),
        )),
        other => Err(typed_error(
            "TypeError",
            "list",
            format!("{} is not iterable", value_type_name(other)),
        )),
    }
}

pub(crate) fn is_map_builtin_name(func_name: &str) -> bool {
    matches!(func_name, "int" | "float" | "json_dumps" | "repr" | "str")
}

pub(crate) fn map_builtin_value(func_name: &str, value: &Value) -> Result<Value, ShellError> {
    match func_name {
        "int" => value_to_int(value),
        "float" => float_builtin(value),
        "json_dumps" => serde_json::to_string(&nu_to_json_value(value))
            .map(|text| Value::string(text, Span::unknown()))
            .map_err(|err| stone_error("map", err.to_string())),
        "str" => str_builtin(value),
        "repr" => repr_builtin(value),
        other => Err(stone_error(
            "map",
            format!("map() does not support `{other}` yet"),
        )),
    }
}

/// Python `str()`: strings are unchanged; everything else is its repr.
pub(crate) fn str_builtin(value: &Value) -> Result<Value, ShellError> {
    python_str(value).map(|text| Value::string(text, Span::unknown()))
}

/// Python `repr()`.
pub(crate) fn repr_builtin(value: &Value) -> Result<Value, ShellError> {
    python_repr(value).map(|text| Value::string(text, Span::unknown()))
}

/// Text that a program builds from a value, following Python: `str()`,
/// f-string fields, and `format()` placeholders. Model-facing output
/// (`emit`, results, `print` of a container) stays JSON; see D-2 in
/// `docs/STONE_LANGUAGE_CONTROL_DESIGN.md`.
pub(crate) fn python_str(value: &Value) -> Result<String, ShellError> {
    match value {
        Value::String { val, .. } => Ok(val.clone()),
        other => python_repr(other),
    }
}

pub(crate) fn python_repr(value: &Value) -> Result<String, ShellError> {
    let mut out = String::new();
    write_python_repr(value, &mut out)?;
    Ok(out)
}

fn write_python_repr(value: &Value, out: &mut String) -> Result<(), ShellError> {
    match value {
        Value::Nothing { .. } => out.push_str("None"),
        Value::Bool { val, .. } => out.push_str(if *val { "True" } else { "False" }),
        Value::Int { val, .. } => out.push_str(&val.to_string()),
        Value::Float { val, .. } => out.push_str(&python_float_repr(*val)),
        Value::String { val, .. } => {
            write_python_string_repr(val, out);
        }
        Value::List { vals, .. } => {
            out.push('[');
            for (index, item) in vals.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_python_repr(item, out)?;
            }
            out.push(']');
        }
        Value::Record { val, .. } => {
            out.push('{');
            for (index, (key, item)) in val.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_python_string_repr(key, out);
                out.push_str(": ");
                write_python_repr(item, out)?;
            }
            out.push('}');
        }
        other => out.push_str(
            &serde_json::to_string(&nu_to_json_value(other))
                .map_err(|err| stone_error("str", err.to_string()))?,
        ),
    }
    Ok(())
}

/// Python's quoting: single quotes unless the text contains a single quote
/// and no double quote.
fn write_python_string_repr(text: &str, out: &mut String) {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch == quote => {
                out.push('\\');
                out.push(ch);
            }
            ch if ch.is_control() => {
                let code = ch as u32;
                if code <= 0xff {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xffff {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push(quote);
}

/// Python's float repr: the shortest round-trip digits, fixed notation when
/// the decimal exponent is in [-4, 16), otherwise `1e+16` style.
pub(crate) fn python_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_string();
    }
    // Rust's `{:e}` is the shortest round-trip form, like Python's repr.
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("float exponent formatting always has an exponent");
    let exponent: i32 = exponent.parse().expect("float exponent is an integer");
    let negative = mantissa.starts_with('-');
    let digits = mantissa
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    let body = if (-4..16).contains(&exponent) {
        if exponent >= 0 {
            let integer_len = exponent as usize + 1;
            if digits.len() <= integer_len {
                format!("{digits}{}.0", "0".repeat(integer_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..integer_len], &digits[integer_len..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        }
    } else {
        let mantissa = if digits.len() == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

/// `sum`: ints add exactly (overflow is an error); once a float appears the
/// total is a float added with Neumaier compensation, as Python 3.12 does,
/// so `sum([0.1] * 10) == 1.0`.
pub(crate) fn sum_builtin(values: Vec<Value>) -> Result<Value, ShellError> {
    let mut int_total = 0i64;
    let mut float_total = 0.0f64;
    let mut compensation = 0.0f64;
    let mut has_float = false;
    let add_float = |total: &mut f64, compensation: &mut f64, value: f64| {
        let next = *total + value;
        if total.abs() >= value.abs() {
            *compensation += (*total - next) + value;
        } else {
            *compensation += (value - next) + *total;
        }
        *total = next;
    };
    for value in values {
        match value_to_sum_number(&value)? {
            SumNumber::Int(value) if has_float => {
                add_float(&mut float_total, &mut compensation, value as f64);
            }
            SumNumber::Int(value) => {
                int_total = int_total
                    .checked_add(value)
                    .ok_or_else(|| stone_error("sum", "integer sum overflow"))?;
            }
            SumNumber::Float(value) if has_float => {
                add_float(&mut float_total, &mut compensation, value);
            }
            SumNumber::Float(value) => {
                has_float = true;
                float_total = int_total as f64;
                add_float(&mut float_total, &mut compensation, value);
            }
        }
    }
    if has_float {
        Ok(Value::float(float_total + compensation, Span::unknown()))
    } else {
        Ok(Value::int(int_total, Span::unknown()))
    }
}

pub(crate) fn unique_builtin(value: &Value) -> Result<Value, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            "unique",
            format!("expected list, got {}", crate::stone_builtins::value_type_name(&value)),
        ));
    };
    unique_values(vals.clone(), "unique").map(|values| Value::list(values, Span::unknown()))
}

pub(crate) fn where_builtin(
    values: &Value,
    key: &Value,
    expected: &Value,
) -> Result<Value, ShellError> {
    let key = value_to_string(key, "where key")?;
    let Value::List { vals, .. } = values else {
        return Err(stone_error(
            "where",
            format!("expected list, got {}", crate::stone_builtins::value_type_name(&values)),
        ));
    };
    let mut selected = Vec::new();
    for value in vals {
        if let Value::Record { val, .. } = value {
            if val
                .get(&key)
                .is_some_and(|candidate| values_equal(candidate, expected))
            {
                selected.push(value.clone());
            }
        }
    }
    Ok(Value::list(selected, Span::unknown()))
}

pub(crate) fn where_compare_builtin(
    values: &Value,
    key: &Value,
    op: &Value,
    expected: &Value,
) -> Result<Value, ShellError> {
    let key = value_to_string(key, "where key")?;
    let op = value_to_string(op, "where operator")?;
    let op = where_compare_op(&op)?;
    let Value::List { vals, .. } = values else {
        return Err(stone_error(
            "where",
            format!("expected list, got {}", crate::stone_builtins::value_type_name(&values)),
        ));
    };
    let mut selected = Vec::new();
    for value in vals {
        if let Value::Record { val, .. } = value {
            if let Some(candidate) = val.get(&key) {
                if compare_values(candidate, op, expected)? {
                    selected.push(value.clone());
                }
            }
        }
    }
    Ok(Value::list(selected, Span::unknown()))
}

fn where_compare_op(op: &str) -> Result<CompareOp, ShellError> {
    match op {
        "==" | "=" | "eq" => Ok(CompareOp::Eq),
        "!=" | "<>" | "ne" => Ok(CompareOp::NotEq),
        ">" | "gt" => Ok(CompareOp::Gt),
        ">=" | "gte" | "ge" => Ok(CompareOp::GtE),
        "<" | "lt" => Ok(CompareOp::Lt),
        "<=" | "lte" | "le" => Ok(CompareOp::LtE),
        "in" => Ok(CompareOp::In),
        "not in" | "not_in" => Ok(CompareOp::NotIn),
        other => Err(stone_error(
            "where",
            format!("unsupported operator `{other}`; expected ==, !=, >, >=, <, <=, in, or not in"),
        )),
    }
}

/// A key that is equal exactly when two values are `==` in Stone: dict key
/// order does not matter, and an integral float equals the int (`1 == 1.0`).
pub(crate) fn value_identity_key(value: &Value, context: &str) -> Result<String, ShellError> {
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries = map.into_iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                serde_json::Value::Object(
                    entries.into_iter().map(|(key, value)| (key, canonical(value))).collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(canonical).collect())
            }
            serde_json::Value::Number(number) => match number.as_f64() {
                Some(float)
                    if number.is_f64()
                        && float.fract() == 0.0
                        && (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(&float) =>
                {
                    serde_json::Value::from(float as i64)
                }
                _ => serde_json::Value::Number(number),
            },
            other => other,
        }
    }
    serde_json::to_string(&canonical(nu_to_json_value(value)))
        .map_err(|err| stone_error(context, err.to_string()))
}

pub(crate) fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool { val, .. } => *val,
        Value::Nothing { .. } => false,
        Value::Int { val, .. } => *val != 0,
        Value::Float { val, .. } => *val != 0.0,
        value => !value.is_empty(),
    }
}

pub(crate) fn value_to_operator_i64(value: &Value, context: &str) -> Result<i64, ShellError> {
    value_to_i64(value, context)
}

/// An int argument. Floats, text, and bools are rejected rather than
/// truncated or parsed; `int(x)` is the explicit conversion.
pub(crate) fn value_to_i64(value: &Value, context: &str) -> Result<i64, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(*val),
        other => Err(expected_int_error(other, context)),
    }
}

pub(crate) fn python_floor_div_i64(left: i64, right: i64) -> Result<i64, ShellError> {
    let quotient = left
        .checked_div(right)
        .ok_or_else(|| stone_error("floor division", "integer floor division overflow"))?;
    let remainder = left
        .checked_rem(right)
        .ok_or_else(|| stone_error("floor division", "integer floor division overflow"))?;
    if remainder != 0 && ((remainder > 0) != (right > 0)) {
        quotient
            .checked_sub(1)
            .ok_or_else(|| stone_error("floor division", "integer floor division overflow"))
    } else {
        Ok(quotient)
    }
}

pub(crate) fn python_mod_i64(left: i64, right: i64) -> Result<i64, ShellError> {
    let quotient = python_floor_div_i64(left, right)?;
    left.checked_sub(
        quotient
            .checked_mul(right)
            .ok_or_else(|| stone_error("modulo", "integer modulo overflow"))?,
    )
    .ok_or_else(|| stone_error("modulo", "integer modulo overflow"))
}

pub(crate) fn value_to_limit(value: &Value, context: &str) -> Result<usize, ShellError> {
    let limit = value_to_i64(value, context)?;
    if limit < 0 {
        return Err(stone_error(context, "limit must be non-negative"));
    }
    usize::try_from(limit).map_err(|_| stone_error(context, "limit is too large"))
}

pub(crate) fn value_to_u64(value: &Value, context: &str) -> Result<u64, ShellError> {
    let value = value_to_i64(value, context)?;
    if value < 0 {
        return Err(stone_error(context, "value must be non-negative"));
    }
    u64::try_from(value).map_err(|_| stone_error(context, "value is too large"))
}

/// A numeric argument: an int or a float. Text is rejected rather than
/// parsed; `float(text)` is the explicit conversion.
pub(crate) fn value_to_f64(value: &Value, context: &str) -> Result<f64, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(*val as f64),
        Value::Float { val, .. } => Ok(*val),
        other => {
            let hint = match other {
                Value::String { .. } => "; convert text explicitly with float(text)",
                Value::Bool { .. } => "; convert explicitly with float(flag)",
                _ => "",
            };
            Err(typed_error(
                "TypeError",
                context,
                format!("{context}: expected a number, got {}{hint}", value_type_name(other)),
            ))
        }
    }
}

/// `float(x)` and `parse_float(text)`: text becomes a float only here.
fn convert_to_f64(value: &Value, context: &str) -> Result<f64, ShellError> {
    match value {
        Value::Bool { val, .. } => Ok(f64::from(u8::from(*val))),
        Value::String { val, .. } => {
            val.trim().parse::<f64>().map_err(|_| {
                typed_error(
                    "ValueError",
                    context,
                    format!("could not convert string to float: {}", python_quote(val)),
                )
            })
        }
        Value::Int { .. } | Value::Float { .. } => value_to_f64(value, context),
        other => Err(typed_error(
            "TypeError",
            context,
            format!("float() argument must be a string or a number, not {}", value_type_name(other)),
        )),
    }
}

pub(crate) fn value_to_bool(value: &Value, context: &str) -> Result<bool, ShellError> {
    match value {
        Value::Bool { val, .. } => Ok(*val),
        other => Err(stone_error(
            context,
            format!("expected bool, got {}", crate::stone_builtins::value_type_name(&other)),
        )),
    }
}

pub(crate) fn value_to_string(value: &Value, context: &str) -> Result<String, ShellError> {
    match value {
        Value::String { val, .. } => Ok(val.clone()),
        other => Err(stone_error(
            context,
            format!("expected string, got {}", crate::stone_builtins::value_type_name(&other)),
        )),
    }
}

pub(crate) fn value_to_path_string(value: &Value, context: &str) -> Result<String, ShellError> {
    match value {
        Value::Record { val, .. } => {
            let Some(path) = val.get("path") else {
                return Err(stone_error(
                    context,
                    format!(
                        "record path argument is missing `path`; got {}",
                        crate::stone_builtins::value_type_name(&value)
                    ),
                ));
            };
            value_to_string(path, context)
        }
        _ => value_to_string(value, context),
    }
}

/// Text for `print`: scalars follow Python (`None`, `True`, `1.0`), and
/// containers stay compact JSON because the model reads printed output.
pub(crate) fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Nothing { .. } => "NoneType",
        Value::Bool { .. } => "bool",
        Value::Int { .. } => "int",
        Value::Float { .. } => "float",
        Value::String { .. } => "str",
        Value::List { .. } => "list",
        Value::Record { .. } => "dict",
        Value::Binary { .. } => "bytes",
        Value::Filesize { .. } | Value::Duration { .. } => "int",
        Value::Date { .. } => "datetime",
        Value::Closure { .. } => "function",
        Value::Range { .. } => "range",
        _ => "value",
    }
}

/// Equality for program comparisons (`==`, `!=`, `in`, `index`, `count`).
/// Python says `True == 1`; Stone keeps bool and number apart, so a
/// comparison that mixes them, at any depth, raises instead of answering.
pub(crate) fn checked_values_equal(left: &Value, right: &Value) -> Result<bool, ShellError> {
    match (left, right) {
        (Value::Bool { .. }, Value::Int { .. } | Value::Float { .. })
        | (Value::Int { .. } | Value::Float { .. }, Value::Bool { .. }) => {
            Err(bool_number_comparison_error(left, right))
        }
        (Value::List { vals: left, .. }, Value::List { vals: right, .. }) => {
            if left.len() != right.len() {
                return Ok(false);
            }
            for (left, right) in left.iter().zip(right.iter()) {
                if !checked_values_equal(left, right)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Record { val: left, .. }, Value::Record { val: right, .. }) => {
            if left.len() != right.len() {
                return Ok(false);
            }
            for (key, left_value) in left.iter() {
                match right.get(key) {
                    Some(right_value) if checked_values_equal(left_value, right_value)? => {}
                    _ => return Ok(false),
                }
            }
            Ok(true)
        }
        _ => Ok(values_equal(left, right)),
    }
}

fn bool_number_comparison_error(left: &Value, right: &Value) -> ShellError {
    let show = |value: &Value| python_repr(value).unwrap_or_else(|_| "?".to_string());
    stone_error(
        "comparison",
        format!(
            "comparing the bool {} with the number {}: Python treats True as 1, but Stone keeps bool and int apart, so this comparison has no answer. Compare a bool with True/False and a number with a number, or convert first with int(flag) or bool(count)",
            show(if matches!(left, Value::Bool { .. }) { left } else { right }),
            show(if matches!(left, Value::Bool { .. }) { right } else { left }),
        ),
    )
}

/// Exact int-to-float comparison, as Python does: the int is not rounded
/// to a float first, so `2**53 + 1 == float(2**53)` is false. None for NaN.
pub(crate) fn cmp_int_float(int: i64, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    // Every i64 lies in [-2^63, 2^63).
    if float >= 9_223_372_036_854_775_808.0 {
        return Some(Ordering::Less);
    }
    if float < -9_223_372_036_854_775_808.0 {
        return Some(Ordering::Greater);
    }
    let floor = float.floor();
    match int.cmp(&(floor as i64)) {
        Ordering::Equal if float > floor => Some(Ordering::Less),
        ordering => Some(ordering),
    }
}

pub(crate) fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Nothing { .. }, Value::Nothing { .. }) => true,
        (Value::Bool { val: left, .. }, Value::Bool { val: right, .. }) => left == right,
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left == right,
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => left == right,
        (Value::Int { val: left, .. }, Value::Float { val: right, .. })
        | (Value::Float { val: right, .. }, Value::Int { val: left, .. }) => {
            cmp_int_float(*left, *right) == Some(Ordering::Equal)
        }
        (Value::String { val: left, .. }, Value::String { val: right, .. }) => left == right,
        (Value::List { vals: left, .. }, Value::List { vals: right, .. }) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| values_equal(left, right))
        }
        (Value::Record { val: left, .. }, Value::Record { val: right, .. }) => {
            left.len() == right.len()
                && left.iter().all(|(key, left_value)| {
                    right
                        .get(key)
                        .is_some_and(|right_value| values_equal(left_value, right_value))
                })
        }
        _ => false,
    }
}

pub(crate) fn value_ordering(left: &Value, right: &Value) -> Result<Ordering, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => Ok(left.cmp(right)),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => left
            .partial_cmp(right)
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => cmp_int_float(*left, *right)
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => cmp_int_float(*right, *left)
            .map(Ordering::reverse)
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::String { val: left, .. }, Value::String { val: right, .. }) => Ok(left.cmp(right)),
        (Value::List { vals: left, .. }, Value::List { vals: right, .. }) => {
            for (left, right) in left.iter().zip(right.iter()) {
                let ordering = value_ordering(left, right)?;
                if ordering != Ordering::Equal {
                    return Ok(ordering);
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        _ => Err(stone_error(
            "comparison",
            format!("cannot order {} and {}", crate::stone_builtins::value_type_name(&left), crate::stone_builtins::value_type_name(&right)),
        )),
    }
}

fn value_contains(container: &Value, needle: &Value) -> Result<bool, ShellError> {
    match container {
        Value::String { val, .. } => {
            let needle = value_to_string(needle, "membership")?;
            Ok(val.contains(&needle))
        }
        Value::List { vals, .. } => {
            for value in vals {
                if checked_values_equal(value, needle)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Record { val, .. } => {
            let needle = value_to_string(needle, "membership")?;
            Ok(val.get(&needle).is_some())
        }
        other => Err(stone_error(
            "membership",
            format!("cannot test membership in {}", crate::stone_builtins::value_type_name(&other)),
        )),
    }
}

pub(crate) fn record_method_builtin(
    receiver: &Value,
    method: &str,
    args: &[Value],
) -> Result<Value, ShellError> {
    let Value::Record { val, .. } = receiver else {
        return Err(typed_error(
            "AttributeError",
            method,
            format!("{} has no {method}()", value_type_name(receiver)),
        ));
    };
    match method {
        "get" => {
            let ([key] | [key, _]) = args else {
                return Err(stone_error(
                    "get",
                    "record get() requires key and optional default",
                ));
            };
            let key = value_to_string(key, "get")?;
            if let Some(value) = val.get(&key) {
                return Ok(value.clone());
            }
            match args {
                [_] => Ok(Value::nothing(Span::unknown())),
                [_, default] => Ok(default.clone()),
                _ => unreachable!(),
            }
        }
        "keys" => {
            let [] = args else {
                return Err(stone_error("keys", "keys() takes no arguments"));
            };
            Ok(Value::list(
                val.iter()
                    .map(|(key, _)| Value::string(key.clone(), Span::unknown()))
                    .collect(),
                Span::unknown(),
            ))
        }
        "values" => {
            let [] = args else {
                return Err(stone_error("values", "values() takes no arguments"));
            };
            Ok(Value::list(
                val.iter().map(|(_, value)| value.clone()).collect(),
                Span::unknown(),
            ))
        }
        "items" => {
            let [] = args else {
                return Err(stone_error("items", "items() takes no arguments"));
            };
            Ok(Value::list(
                val.iter()
                    .map(|(key, value)| {
                        Value::list(
                            vec![Value::string(key.clone(), Span::unknown()), value.clone()],
                            Span::unknown(),
                        )
                    })
                    .collect(),
                Span::unknown(),
            ))
        }
        _ => unreachable!("record method dispatch is validated by caller"),
    }
}

pub(crate) fn range_builtin(args: &[i64]) -> Result<Value, ShellError> {
    let (start, stop, step) = match args {
        [stop] => (0, *stop, 1),
        [start, stop] => (*start, *stop, 1),
        [start, stop, step] => (*start, *stop, *step),
        _ => {
            return Err(stone_error(
                "range",
                "range() requires one to three integer arguments",
            ));
        }
    };
    if step == 0 {
        return Err(stone_error("range", "range() step must not be zero"));
    }
    let mut values = Vec::new();
    let mut current = start;
    while if step > 0 {
        current < stop
    } else {
        current > stop
    } {
        if values.len() >= 100_000 {
            return Err(stone_error("range", "range() produced too many values"));
        }
        values.push(Value::int(current, Span::unknown()));
        current = current
            .checked_add(step)
            .ok_or_else(|| stone_error("range", "range() integer overflow"))?;
    }
    Ok(Value::list(values, Span::unknown()))
}

pub(crate) fn parse_float_builtin(value: &Value, default: Value) -> Result<Value, ShellError> {
    match convert_to_f64(value, "parse_float") {
        Ok(value) => Ok(Value::float(value, Span::unknown())),
        Err(_) => match default {
            Value::Float { .. } | Value::Int { .. } => Ok(default),
            other => Err(stone_error(
                "parse_float",
                format!("default must be int or float, got {}", crate::stone_builtins::value_type_name(&other)),
            )),
        },
    }
}

pub(crate) fn parse_int_builtin(value: &Value, default: Value) -> Result<Value, ShellError> {
    match value_to_int(value) {
        Ok(value) => Ok(value),
        Err(_) => match default {
            Value::Int { .. } => Ok(default),
            other => Err(stone_error(
                "parse_int",
                format!("default must be int, got {}", crate::stone_builtins::value_type_name(&other)),
            )),
        },
    }
}

/// Python `round()`. Without digits it returns an int, rounding halfway
/// cases to even. With digits it rounds the exact binary value, so
/// `round(2.675, 2)` is 2.67 as in Python (2.675 is stored just below).
pub(crate) fn round_builtin(value: &Value, digits: Option<i64>) -> Result<Value, ShellError> {
    let Some(digits) = digits else {
        if let Value::Int { val, .. } = value {
            return Ok(Value::int(*val, Span::unknown()));
        }
        let value = value_to_f64(value, "round")?;
        if !value.is_finite() {
            return Err(stone_error(
                "round",
                format!("cannot round {} to an integer", python_float_repr(value)),
            ));
        }
        let rounded = value.round_ties_even();
        if rounded < i64::MIN as f64 || rounded >= i64::MAX as f64 {
            return Err(stone_error(
                "round",
                "round() result does not fit in an int",
            ));
        }
        return Ok(Value::int(rounded as i64, Span::unknown()));
    };
    if !(0..=9).contains(&digits) {
        return Err(stone_error(
            "round",
            "round() digits must be between 0 and 9",
        ));
    }
    if let Value::Int { val, .. } = value {
        return Ok(Value::int(*val, Span::unknown()));
    }
    let value = value_to_f64(value, "round")?;
    if !value.is_finite() {
        return Ok(Value::float(value, Span::unknown()));
    }
    // Rust's fixed-precision formatting rounds the exact binary value
    // half-to-even, which is what Python's round() does.
    let text = format!("{value:.prec$}", prec = digits as usize);
    let rounded = text
        .parse::<f64>()
        .map_err(|err| stone_error("round", err.to_string()))?;
    Ok(Value::float(rounded, Span::unknown()))
}

pub(crate) fn slice_builtin(
    value: &Value,
    lower: Option<i64>,
    upper: Option<i64>,
) -> Result<Value, ShellError> {
    match value {
        Value::List { vals, .. } => {
            let (start, end) = normalize_slice_bounds(lower, upper, vals.len())?;
            Ok(Value::list(vals[start..end].to_vec(), Span::unknown()))
        }
        Value::String { val, .. } => {
            let chars = val.chars().collect::<Vec<_>>();
            let (start, end) = normalize_slice_bounds(lower, upper, chars.len())?;
            Ok(Value::string(
                chars[start..end].iter().collect::<String>(),
                Span::unknown(),
            ))
        }
        other => Err(stone_error(
            "slice",
            format!("cannot slice {}", crate::stone_builtins::value_type_name(&other)),
        )),
    }
}

pub(crate) fn split_builtin(
    text: &Value,
    separator: Option<&Value>,
    maxsplit: Option<&Value>,
) -> Result<Value, ShellError> {
    let text = value_to_string(text, "split")?;
    let maxsplit = split_maxsplit(maxsplit)?;
    split_text(&text, separator, maxsplit)
}

fn split_maxsplit(maxsplit: Option<&Value>) -> Result<Option<usize>, ShellError> {
    maxsplit
        .map(|value| {
            let value = value_to_i64(value, "split maxsplit")?;
            if value < 0 {
                Ok(None)
            } else {
                usize::try_from(value)
                    .map(Some)
                    .map_err(|_| stone_error("split", "maxsplit is too large"))
            }
        })
        .transpose()
        .map(Option::flatten)
}

fn split_text(
    text: &str,
    separator: Option<&Value>,
    maxsplit: Option<usize>,
) -> Result<Value, ShellError> {
    let separator = match separator {
        Some(Value::Nothing { .. }) => None,
        other => other,
    };
    let parts: Vec<String> = match separator {
        None => match maxsplit {
            None => text.split_whitespace().map(str::to_owned).collect(),
            Some(limit) => split_whitespace_limited(text, limit),
        },
        Some(separator) => {
            let separator = value_to_string(separator, "split")?;
            if separator.is_empty() {
                return Err(stone_error("split", "empty separator is not supported"));
            }
            match maxsplit {
                None => text.split(&separator).map(str::to_owned).collect(),
                Some(limit) => text
                    .splitn(limit + 1, &separator)
                    .map(str::to_owned)
                    .collect(),
            }
        }
    };
    Ok(Value::list(
        parts
            .into_iter()
            .map(|part| Value::string(part, Span::unknown()))
            .collect(),
        Span::unknown(),
    ))
}

fn rsplit_text(
    text: &str,
    separator: Option<&Value>,
    maxsplit: Option<usize>,
) -> Result<Value, ShellError> {
    let separator = match separator {
        Some(Value::Nothing { .. }) => None,
        other => other,
    };
    let split_from_right = separator.is_some() && maxsplit.is_some();
    let mut parts: Vec<String> = match separator {
        None => match maxsplit {
            None => text.split_whitespace().map(str::to_owned).collect(),
            Some(limit) => split_whitespace_right_limited(text, limit),
        },
        Some(separator) => {
            let separator = value_to_string(separator, "rsplit")?;
            if separator.is_empty() {
                return Err(stone_error("rsplit", "empty separator is not supported"));
            }
            match maxsplit {
                None => text.split(&separator).map(str::to_owned).collect(),
                Some(limit) => text
                    .rsplitn(limit + 1, &separator)
                    .map(str::to_owned)
                    .collect(),
            }
        }
    };
    if split_from_right {
        parts.reverse();
    }
    Ok(Value::list(
        parts
            .into_iter()
            .map(|part| Value::string(part, Span::unknown()))
            .collect(),
        Span::unknown(),
    ))
}

fn split_whitespace_limited(text: &str, limit: usize) -> Vec<String> {
    let mut rest = text.trim_start_matches(char::is_whitespace);
    if rest.is_empty() {
        return Vec::new();
    }
    if limit == 0 {
        return vec![rest.to_owned()];
    }

    let mut parts = Vec::new();
    for _ in 0..limit {
        let Some((start, _)) = rest.char_indices().find(|(_, ch)| ch.is_whitespace()) else {
            parts.push(rest.to_owned());
            return parts;
        };
        parts.push(rest[..start].to_owned());
        let after_word = &rest[start..];
        rest = after_word.trim_start_matches(char::is_whitespace);
        if rest.is_empty() {
            return parts;
        }
    }
    parts.push(rest.to_owned());
    parts
}

fn split_whitespace_right_limited(text: &str, limit: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if limit == 0 {
        return vec![trimmed.to_owned()];
    }
    let fields = trimmed.split_whitespace().collect::<Vec<_>>();
    if fields.len() <= limit + 1 {
        return fields.into_iter().map(str::to_owned).collect();
    }
    let split_at = fields.len() - limit;
    let head = fields[..split_at].join(" ");
    std::iter::once(head)
        .chain(fields[split_at..].iter().map(|part| (*part).to_owned()))
        .collect()
}

pub(crate) fn sort_key_for_value(value: &Value, field: Option<&str>) -> Result<Value, ShellError> {
    match field {
        None => Ok(value.clone()),
        Some(field) => match value {
            Value::Record { val, .. } => val
                .get(field)
                .cloned()
                .ok_or_else(|| stone_error("sort", format!("dict has no key `{field}`"))),
            other => Err(stone_error(
                "sort",
                format!("key= requires record rows, got {}", crate::stone_builtins::value_type_name(&other)),
            )),
        },
    }
}

pub(crate) fn sort_key_kind(value: &Value) -> Result<SortKeyKind, ShellError> {
    match value {
        Value::Int { .. } => Ok(SortKeyKind::Number),
        Value::Float { val, .. } if !val.is_nan() => Ok(SortKeyKind::Number),
        Value::Float { .. } => Err(stone_error("sort", "cannot sort NaN values")),
        Value::String { .. } => Ok(SortKeyKind::Text),
        Value::List { .. } => Ok(SortKeyKind::Composite),
        other => Err(stone_error(
            "sort",
            format!("cannot sort by {}", crate::stone_builtins::value_type_name(&other)),
        )),
    }
}

pub(crate) fn sort_builtin_values(
    values: Vec<Value>,
    reverse: bool,
    key_for_value: impl FnMut(&Value) -> Result<Value, ShellError>,
) -> Result<Value, ShellError> {
    Ok(Value::list(
        sort_keyed(values, reverse, key_for_value)?,
        Span::unknown(),
    ))
}

/// Stable sort of any items by a computed key, with Stone's key rules (all
/// keys of compatible types).
pub(crate) fn sort_keyed<T>(
    values: Vec<T>,
    reverse: bool,
    mut key_for_value: impl FnMut(&T) -> Result<Value, ShellError>,
) -> Result<Vec<T>, ShellError> {
    let mut keyed = Vec::with_capacity(values.len());
    let mut key_kind = None;
    for value in values {
        let sort_key = key_for_value(&value)?;
        let next_key_kind = sort_key_kind(&sort_key)?;
        if let Some(key_kind) = key_kind {
            if key_kind != next_key_kind {
                return Err(typed_error(
                    "TypeError",
                    "sort",
                    "all sort keys must have compatible types: ints and floats sort together, but strings, numbers, and lists cannot be ordered against each other",
                ));
            }
        } else {
            key_kind = Some(next_key_kind);
        }
        keyed.push((sort_key, value));
    }
    keyed.sort_by(|(left_key, _), (right_key, _)| {
        let ordering =
            value_ordering(left_key, right_key).expect("sort keys are validated before sorting");
        if reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });
    Ok(keyed.into_iter().map(|(_, value)| value).collect())
}

pub(crate) fn starts_with_builtin(text: &Value, prefix: &Value) -> Result<Value, ShellError> {
    let text = value_to_string(text, "starts_with")?;
    let prefix = value_to_string(prefix, "starts_with")?;
    Ok(Value::bool(text.starts_with(&prefix), Span::unknown()))
}

pub(crate) fn string_method_builtin(
    receiver: &Value,
    method: &str,
    args: &[Value],
) -> Result<Value, ShellError> {
    let Value::String { val: text, .. } = receiver else {
        return Err(typed_error(
            "AttributeError",
            method,
            format!("{} has no {method}()", value_type_name(receiver)),
        ));
    };
    match method {
        "strip" => {
            let stripped = match args {
                [] => text.trim().to_owned(),
                [chars] => {
                    let chars = value_to_string(chars, "strip")?;
                    text.trim_matches(|ch| chars.contains(ch)).to_owned()
                }
                _ => return Err(stone_error("strip", "strip() takes at most one argument")),
            };
            Ok(Value::string(stripped, Span::unknown()))
        }
        "lstrip" => {
            let stripped = match args {
                [] => text.trim_start().to_owned(),
                [chars] => {
                    let chars = value_to_string(chars, "lstrip")?;
                    text.trim_start_matches(|ch| chars.contains(ch)).to_owned()
                }
                _ => return Err(stone_error("lstrip", "lstrip() takes at most one argument")),
            };
            Ok(Value::string(stripped, Span::unknown()))
        }
        "rstrip" => {
            let stripped = match args {
                [] => text.trim_end().to_owned(),
                [chars] => {
                    let chars = value_to_string(chars, "rstrip")?;
                    text.trim_end_matches(|ch| chars.contains(ch)).to_owned()
                }
                _ => return Err(stone_error("rstrip", "rstrip() takes at most one argument")),
            };
            Ok(Value::string(stripped, Span::unknown()))
        }
        "isdigit" => {
            let [] = args else {
                return Err(stone_error("isdigit", "isdigit() takes no arguments"));
            };
            Ok(Value::bool(
                !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit()),
                Span::unknown(),
            ))
        }
        "isalpha" => {
            let [] = args else {
                return Err(stone_error("isalpha", "isalpha() takes no arguments"));
            };
            Ok(Value::bool(
                !text.is_empty() && text.chars().all(|ch| ch.is_alphabetic()),
                Span::unknown(),
            ))
        }
        "isalnum" => {
            let [] = args else {
                return Err(stone_error("isalnum", "isalnum() takes no arguments"));
            };
            Ok(Value::bool(
                !text.is_empty() && text.chars().all(|ch| ch.is_alphanumeric()),
                Span::unknown(),
            ))
        }
        "count" => {
            let [needle] = args else {
                return Err(stone_error(
                    "count",
                    "count() requires exactly one argument",
                ));
            };
            let needle = value_to_string(needle, "count")?;
            if needle.is_empty() {
                let count = text.chars().count().saturating_add(1);
                let count =
                    i64::try_from(count).map_err(|_| stone_error("count", "count is too large"))?;
                return Ok(Value::int(count, Span::unknown()));
            }
            let count = text.match_indices(&needle).count();
            let count =
                i64::try_from(count).map_err(|_| stone_error("count", "count is too large"))?;
            Ok(Value::int(count, Span::unknown()))
        }
        "split" => {
            let (separator, maxsplit) = match args {
                [] => (None, None),
                [separator] => (Some(separator), None),
                [separator, maxsplit] => (Some(separator), Some(maxsplit)),
                _ => {
                    return Err(stone_error(
                        "split",
                        "split() takes separator and optional maxsplit",
                    ));
                }
            };
            split_text(text, separator, split_maxsplit(maxsplit)?)
        }
        "rsplit" => {
            let (separator, maxsplit) = match args {
                [] => (None, None),
                [separator] => (Some(separator), None),
                [separator, maxsplit] => (Some(separator), Some(maxsplit)),
                _ => {
                    return Err(stone_error(
                        "rsplit",
                        "rsplit() takes separator and optional maxsplit",
                    ));
                }
            };
            rsplit_text(text, separator, split_maxsplit(maxsplit)?)
        }
        "splitlines" => {
            let [] = args else {
                return Err(stone_error(
                    "splitlines",
                    "splitlines() takes no arguments for now",
                ));
            };
            Ok(Value::list(
                text.lines()
                    .map(|part| Value::string(part.to_owned(), Span::unknown()))
                    .collect(),
                Span::unknown(),
            ))
        }
        "replace" => {
            let [old, new] = args else {
                return Err(stone_error(
                    "replace",
                    "replace() requires old and new arguments",
                ));
            };
            let old = value_to_string(old, "replace")?;
            let new = value_to_string(new, "replace")?;
            Ok(Value::string(text.replace(&old, &new), Span::unknown()))
        }
        "join" => {
            let [items] = args else {
                return Err(stone_error("join", "join() requires exactly one iterable"));
            };
            let Value::List { vals, .. } = items else {
                return Err(stone_error(
                    "join",
                    format!("expected list, got {}", crate::stone_builtins::value_type_name(&items)),
                ));
            };
            let mut parts = Vec::with_capacity(vals.len());
            for value in vals {
                parts.push(value_to_string(value, "join")?);
            }
            Ok(Value::string(parts.join(text), Span::unknown()))
        }
        "lower" => {
            let [] = args else {
                return Err(stone_error("lower", "lower() takes no arguments"));
            };
            Ok(Value::string(text.to_lowercase(), Span::unknown()))
        }
        "upper" => {
            let [] = args else {
                return Err(stone_error("upper", "upper() takes no arguments"));
            };
            Ok(Value::string(text.to_uppercase(), Span::unknown()))
        }
        "zfill" => {
            let [width] = args else {
                return Err(stone_error("zfill", "zfill() requires exactly one width"));
            };
            let width = value_to_i64(width, "zfill width")?;
            if width < 0 {
                return Err(stone_error("zfill", "width must be non-negative"));
            }
            let width =
                usize::try_from(width).map_err(|_| stone_error("zfill", "width is too large"))?;
            Ok(Value::string(zfill_text(text, width), Span::unknown()))
        }
        "startswith" => {
            let [prefix] = args else {
                return Err(stone_error(
                    "startswith",
                    "startswith() requires exactly one argument",
                ));
            };
            let prefix = value_to_string(prefix, "startswith")?;
            Ok(Value::bool(text.starts_with(&prefix), Span::unknown()))
        }
        "endswith" => {
            let [suffix] = args else {
                return Err(stone_error(
                    "endswith",
                    "endswith() requires exactly one argument",
                ));
            };
            let suffix = value_to_string(suffix, "endswith")?;
            Ok(Value::bool(text.ends_with(&suffix), Span::unknown()))
        }
        _ => unreachable!("string method dispatch is validated by caller"),
    }
}

pub(crate) fn subscript_builtin(value: &Value, index: &Value) -> Result<Value, ShellError> {
    match (value, index) {
        (Value::Record { val, .. }, Value::String { val: key, .. }) => val
            .get(key)
            .cloned()
            .ok_or_else(|| stone_error("subscript", format!("dict has no key `{key}`"))),
        (Value::List { vals, .. }, Value::Int { val: index, .. }) => {
            let index = normalize_index(*index, vals.len())?;
            vals.get(index).cloned().ok_or_else(|| {
                stone_error("subscript", format!("list index {index} is out of range"))
            })
        }
        (Value::String { val, .. }, Value::Int { val: index, .. }) => {
            let chars = val.chars().collect::<Vec<_>>();
            let index = normalize_index(*index, chars.len())?;
            chars
                .get(index)
                .map(|ch| Value::string(ch.to_string(), Span::unknown()))
                .ok_or_else(|| {
                    stone_error("subscript", format!("string index {index} is out of range"))
                })
        }
        (value, index) => Err(stone_error(
            "subscript",
            format!(
                "cannot index {} with {}",
                crate::stone_builtins::value_type_name(&value),
                crate::stone_builtins::value_type_name(&index)
            ),
        )),
    }
}

fn value_len(value: &Value) -> Result<i64, ShellError> {
    let len = match value {
        Value::List { vals, .. } => vals.len(),
        Value::Record { val, .. } => val.len(),
        Value::String { val, .. } => val.chars().count(),
        other => {
            return Err(stone_error(
                "len",
                format!("len() does not support {}", crate::stone_builtins::value_type_name(&other)),
            ));
        }
    };
    i64::try_from(len).map_err(|_| stone_error("len", "value is too large"))
}

pub(crate) fn normalize_slice_bounds(
    lower: Option<i64>,
    upper: Option<i64>,
    len: usize,
) -> Result<(usize, usize), ShellError> {
    let len_i64 =
        i64::try_from(len).map_err(|_| stone_error("slice", "collection is too large"))?;
    let start = lower.unwrap_or(0);
    let end = upper.unwrap_or(len_i64);
    let start = if start < 0 { len_i64 + start } else { start }.clamp(0, len_i64);
    let end = if end < 0 { len_i64 + end } else { end }.clamp(0, len_i64);
    let start = usize::try_from(start).map_err(|_| stone_error("slice", "start is too large"))?;
    let end = usize::try_from(end).map_err(|_| stone_error("slice", "end is too large"))?;
    if start > end {
        Ok((start, start))
    } else {
        Ok((start, end))
    }
}

pub(crate) fn normalize_index(index: i64, len: usize) -> Result<usize, ShellError> {
    let len =
        i64::try_from(len).map_err(|_| stone_error("subscript", "collection is too large"))?;
    let normalized = if index < 0 { len + index } else { index };
    if normalized < 0 || normalized >= len {
        return Err(stone_error(
            "subscript",
            format!("index {index} is out of range"),
        ));
    }
    usize::try_from(normalized).map_err(|_| stone_error("subscript", "index is too large"))
}

fn normalize_string_start(index: i64, len: usize) -> Result<usize, ShellError> {
    let len = i64::try_from(len).map_err(|_| stone_error("index", "string is too large"))?;
    let normalized = if index < 0 { len + index } else { index };
    let clamped = normalized.clamp(0, len);
    usize::try_from(clamped).map_err(|_| stone_error("index", "index is too large"))
}

pub(crate) fn zfill_text(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        return text.to_owned();
    }
    let (sign, digits) = if let Some(rest) = text.strip_prefix('-') {
        ("-", rest)
    } else if let Some(rest) = text.strip_prefix('+') {
        ("+", rest)
    } else {
        ("", text)
    };
    format!("{sign}{}{}", "0".repeat(width - len), digits)
}

fn format_template(template: &str, args: &[Value]) -> Result<String, ShellError> {
    let mut output = String::new();
    let mut chars = template.chars().peekable();
    let mut arg_index = 0;
    let mut used_explicit_index = false;
    let mut used_implicit_index = false;
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                output.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                output.push('}');
            }
            '{' => {
                let mut spec = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == '}' {
                        closed = true;
                        break;
                    }
                    spec.push(ch);
                }
                if !closed {
                    return Err(stone_error("format", "format() found an unmatched `{`"));
                }
                let (explicit_index, spec) = split_format_index_and_spec(&spec)?;
                let value_index = match explicit_index {
                    Some(index) => {
                        used_explicit_index = true;
                        index
                    }
                    None => {
                        used_implicit_index = true;
                        let index = arg_index;
                        arg_index += 1;
                        index
                    }
                };
                if used_explicit_index && used_implicit_index {
                    return Err(stone_error(
                        "format",
                        "format() cannot mix numbered placeholders like {0} with automatic {} placeholders",
                    ));
                }
                let Some(value) = args.get(value_index) else {
                    return Err(stone_error(
                        "format",
                        "format() has fewer arguments than placeholders",
                    ));
                };
                output.push_str(&format_value(value, spec)?);
            }
            '}' => {
                return Err(stone_error(
                    "format",
                    "format() supports `{}`, simple fixed decimal specs like `{:.2f}`, and escaped `{{` or `}}`",
                ));
            }
            _ => output.push(ch),
        }
    }
    if !used_explicit_index && arg_index != args.len() {
        return Err(stone_error(
            "format",
            "format() received more arguments than placeholders",
        ));
    }
    Ok(output)
}

fn split_format_index_and_spec(spec: &str) -> Result<(Option<usize>, &str), ShellError> {
    if spec.is_empty() || spec.starts_with(':') {
        return Ok((None, spec));
    }
    let (index_text, rest) = match spec.split_once(':') {
        Some((index, rest)) => (index, Some(rest)),
        None => (spec, None),
    };
    if !index_text.chars().all(|ch| ch.is_ascii_digit()) {
        return Ok((None, spec));
    }
    let index = index_text
        .parse::<usize>()
        .map_err(|_| stone_error("format", "format() placeholder index is too large"))?;
    let spec = rest.map(|_| &spec[index_text.len()..]).unwrap_or("");
    Ok((Some(index), spec))
}

fn format_value(value: &Value, spec: &str) -> Result<String, ShellError> {
    if spec.is_empty() {
        return python_str(value);
    }
    if let Some(precision) = parse_fixed_decimal_format_spec(spec) {
        let value = value_to_f64(value, "format")?;
        return Ok(format!("{value:.precision$}"));
    }
    Err(stone_error(
        "format",
        format!(
            "unsupported format spec `{{{spec}}}`; supported specs are {{}}, {{N}}, and {{:.Nf}}"
        ),
    ))
}

fn parse_fixed_decimal_format_spec(spec: &str) -> Option<usize> {
    let digits = spec.strip_prefix(":.")?.strip_suffix('f')?;
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok()
}

fn unique_values(values: Vec<Value>, context: &str) -> Result<Vec<Value>, ShellError> {
    let mut seen = HashSet::new();
    let mut unique_values = Vec::new();
    for value in values {
        let key = value_identity_key(&value, context)?;
        if seen.insert(key) {
            unique_values.push(value);
        }
    }
    Ok(unique_values)
}

enum SumNumber {
    Int(i64),
    Float(f64),
}

fn value_to_sum_number(value: &Value) -> Result<SumNumber, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(SumNumber::Int(*val)),
        Value::Float { val, .. } => Ok(SumNumber::Float(*val)),
        other => Err(typed_error(
            "TypeError",
            "sum",
            format!(
                "sum(): unsupported operand type {}; sum adds ints and floats (convert text with int(x) or float(x))",
                value_type_name(other)
            ),
        )),
    }
}

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}

#[cfg(test)]
mod python_text_tests {
    use super::*;
    use nu_protocol::Record;

    fn text(value: Value) -> String {
        python_str(&value).expect("str")
    }

    #[test]
    fn str_and_repr_follow_python() {
        let span = Span::unknown();
        assert_eq!(text(Value::nothing(span)), "None");
        assert_eq!(text(Value::bool(true, span)), "True");
        assert_eq!(text(Value::string("it's", span)), "it's");
        assert_eq!(
            python_repr(&Value::string("it's", span)).expect("repr"),
            "\"it's\""
        );
        assert_eq!(
            python_repr(&Value::string("a\n'b'\\", span)).expect("repr"),
            "\"a\\n'b'\\\\\""
        );
        let mut record = Record::new();
        record.push("a", Value::nothing(span));
        record.push(
            "b",
            Value::list(vec![Value::int(1, span), Value::float(2.5, span)], span),
        );
        assert_eq!(
            text(Value::record(record, span)),
            "{'a': None, 'b': [1, 2.5]}"
        );
        assert_eq!(text(Value::list(Vec::new(), span)), "[]");
    }

    #[test]
    fn float_repr_matches_python() {
        for (value, expected) in [
            (1.0, "1.0"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (1e-5, "1e-05"),
            (0.0001, "0.0001"),
            (-2.5, "-2.5"),
            (123456789.0, "123456789.0"),
            (1.5e300, "1.5e+300"),
            (-0.0, "-0.0"),
            (f64::INFINITY, "inf"),
            (f64::NAN, "nan"),
        ] {
            assert_eq!(python_float_repr(value), expected, "{value}");
        }
    }

    #[test]
    fn round_follows_python() {
        let span = Span::unknown();
        let round = |value: f64, digits: Option<i64>| {
            round_builtin(&Value::float(value, span), digits).expect("round")
        };
        assert_eq!(round(2.5, None), Value::int(2, span));
        assert_eq!(round(3.5, None), Value::int(4, span));
        assert_eq!(round(-0.5, None), Value::int(0, span));
        assert_eq!(round(2.675, Some(2)), Value::float(2.67, span));
        assert_eq!(round(0.125, Some(2)), Value::float(0.12, span));
        assert_eq!(
            round_builtin(&Value::int(7, span), Some(2)).expect("int"),
            Value::int(7, span)
        );
        assert!(round_builtin(&Value::float(f64::NAN, span), None).is_err());
    }
}
