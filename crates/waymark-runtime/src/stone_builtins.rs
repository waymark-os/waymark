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

pub(crate) fn value_to_int(value: &Value) -> Result<Value, ShellError> {
    value_to_i64(value, "int").map(|value| Value::int(value, Span::unknown()))
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
            vals.iter()
                .position(|value| values_equal(value, needle))
                .map(|index| Value::int(index as i64, Span::unknown()))
                .ok_or_else(|| stone_error("index", "value is not in list"))
        }
        Value::String { val, .. } | Value::Glob { val, .. } => {
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
            val[prefix_len..]
                .find(&needle)
                .map(|index| Value::int((prefix_len + index) as i64, Span::unknown()))
                .ok_or_else(|| stone_error("index", "substring not found"))
        }
        other => Err(stone_error(
            "index",
            format!("{} has no index()", other.get_type()),
        )),
    }
}

pub(crate) fn find_method_builtin(receiver: &Value, args: &[Value]) -> Result<Value, ShellError> {
    let (Value::String { val, .. } | Value::Glob { val, .. }) = receiver else {
        return Err(stone_error(
            "find",
            format!("{} has no find()", receiver.get_type()),
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
        .map(|index| (prefix_len + index) as i64)
        .unwrap_or(-1);
    Ok(Value::int(index, Span::unknown()))
}

pub(crate) fn float_builtin(value: &Value) -> Result<Value, ShellError> {
    value_to_f64(value, "float").map(|value| Value::float(value, Span::unknown()))
}

pub(crate) fn format_builtin(template: &Value, args: &[Value]) -> Result<Value, ShellError> {
    let template = value_to_string(template, "format")?;
    let args = args
        .iter()
        .map(value_to_display_string)
        .collect::<Result<Vec<_>, _>>()?;
    format_template(&template, &args).map(|text| Value::string(text, Span::unknown()))
}

pub(crate) fn compare_values(
    left: &Value,
    op: CompareOp,
    right: &Value,
) -> Result<bool, ShellError> {
    match op {
        CompareOp::Eq => Ok(values_equal(left, right)),
        CompareOp::NotEq => Ok(!values_equal(left, right)),
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
        (Value::String { val: left, .. }, Value::String { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::String { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::String { val: right, .. }) => {
            Ok(Value::string(format!("{left}{right}"), Span::unknown()))
        }
        (Value::List { vals: left, .. }, Value::List { vals: right, .. }) => {
            let mut values = left.clone();
            values.extend(right.clone());
            Ok(Value::list(values, Span::unknown()))
        }
        _ => Err(stone_error(
            "addition",
            format!("cannot add {} and {}", left.get_type(), right.get_type()),
        )),
    }
}

pub(crate) fn bitwise_int_values(
    left: &Value,
    right: &Value,
    context: &str,
    op: impl FnOnce(i64, i64) -> i64,
) -> Result<Value, ShellError> {
    let left = value_to_i64(left, context)?;
    let right = value_to_i64(right, context)?;
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
                format!("cannot divide {} and {}", left.get_type(), right.get_type()),
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
            Ok(Value::int(left.div_euclid(*right), Span::unknown()))
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
            Ok(Value::int(left.rem_euclid(*right), Span::unknown()))
        }
        _ => {
            let left = value_to_f64(left, "modulo")?;
            let right = value_to_f64(right, "modulo")?;
            if right == 0.0 {
                return Err(stone_error("modulo", "modulo by zero"));
            }
            Ok(Value::float(left.rem_euclid(right), Span::unknown()))
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
                left.get_type(),
                right.get_type()
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
            format!("cannot negate {}", other.get_type()),
        )),
    }
}

pub(crate) fn shift_value(
    left: &Value,
    right: &Value,
    context: &str,
    op: impl FnOnce(i64, u32) -> Option<i64>,
) -> Result<Value, ShellError> {
    let left = value_to_i64(left, context)?;
    let right = value_to_i64(right, context)?;
    let shift = u32::try_from(right)
        .map_err(|_| stone_error(context, "shift count must be non-negative"))?;
    op(left, shift)
        .map(|value| Value::int(value, Span::unknown()))
        .ok_or_else(|| stone_error(context, "shift count is too large"))
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
                left.get_type(),
                right.get_type()
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
            format!("expected list, got {}", value.get_type()),
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
            format!("expected list, got {}", value.get_type()),
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
            format!("expected list, got {}", items.get_type()),
        ));
    };
    let mut parts = Vec::with_capacity(vals.len());
    for value in vals {
        parts.push(value_to_string(value, "join")?);
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
        other => Err(stone_error(
            "list",
            format!("expected list or record, got {}", other.get_type()),
        )),
    }
}

pub(crate) fn is_map_builtin_name(func_name: &str) -> bool {
    matches!(func_name, "int" | "float" | "json_dumps" | "str")
}

pub(crate) fn map_builtin_value(func_name: &str, value: &Value) -> Result<Value, ShellError> {
    match func_name {
        "int" => value_to_int(value),
        "float" => float_builtin(value),
        "json_dumps" => serde_json::to_string(&nu_to_json_value(value))
            .map(|text| Value::string(text, Span::unknown()))
            .map_err(|err| stone_error("map", err.to_string())),
        "str" => str_builtin(value),
        other => Err(stone_error(
            "map",
            format!("map() does not support `{other}` yet"),
        )),
    }
}

pub(crate) fn str_builtin(value: &Value) -> Result<Value, ShellError> {
    value_to_display_string(value).map(|text| Value::string(text, Span::unknown()))
}

pub(crate) fn sum_builtin(values: Vec<Value>) -> Result<Value, ShellError> {
    let mut int_total = 0i64;
    let mut float_total = 0.0f64;
    let mut has_float = false;
    for value in values {
        match value_to_sum_number(&value)? {
            SumNumber::Int(value) if has_float => {
                float_total += value as f64;
            }
            SumNumber::Int(value) => {
                int_total = int_total
                    .checked_add(value)
                    .ok_or_else(|| stone_error("sum", "integer sum overflow"))?;
            }
            SumNumber::Float(value) if has_float => {
                float_total += value;
            }
            SumNumber::Float(value) => {
                has_float = true;
                float_total = int_total as f64 + value;
            }
        }
    }
    if has_float {
        Ok(Value::float(float_total, Span::unknown()))
    } else {
        Ok(Value::int(int_total, Span::unknown()))
    }
}

pub(crate) fn set_builtin(values: Vec<Value>) -> Result<Value, ShellError> {
    unique_values(values, "set").map(|values| Value::list(values, Span::unknown()))
}

pub(crate) fn unique_builtin(value: &Value) -> Result<Value, ShellError> {
    let Value::List { vals, .. } = value else {
        return Err(stone_error(
            "unique",
            format!("expected list, got {}", value.get_type()),
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
            format!("expected list, got {}", values.get_type()),
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

pub(crate) fn value_identity_key(value: &Value, context: &str) -> Result<String, ShellError> {
    serde_json::to_string(&nu_to_json_value(value))
        .map_err(|err| stone_error(context, err.to_string()))
}

pub(crate) fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Bool { val, .. } => *val,
        Value::Nothing { .. } => false,
        value => !value.is_empty(),
    }
}

pub(crate) fn value_to_i64(value: &Value, context: &str) -> Result<i64, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(*val),
        Value::Float { val, .. } => Ok(*val as i64),
        Value::String { val, .. } | Value::Glob { val, .. } => val
            .trim()
            .parse::<i64>()
            .map_err(|err| stone_error(context, format!("failed to parse integer: {err}"))),
        other => Err(stone_error(
            context,
            format!("expected integer, got {}", other.get_type()),
        )),
    }
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

pub(crate) fn value_to_f64(value: &Value, context: &str) -> Result<f64, ShellError> {
    match value {
        Value::Int { val, .. } => Ok(*val as f64),
        Value::Float { val, .. } => Ok(*val),
        Value::String { val, .. } | Value::Glob { val, .. } => val
            .trim()
            .parse::<f64>()
            .map_err(|err| stone_error(context, format!("failed to parse float: {err}"))),
        other => Err(stone_error(
            context,
            format!("expected number, got {}", other.get_type()),
        )),
    }
}

pub(crate) fn value_to_bool(value: &Value, context: &str) -> Result<bool, ShellError> {
    match value {
        Value::Bool { val, .. } => Ok(*val),
        other => Err(stone_error(
            context,
            format!("expected bool, got {}", other.get_type()),
        )),
    }
}

pub(crate) fn value_to_string(value: &Value, context: &str) -> Result<String, ShellError> {
    match value {
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.clone()),
        other => Err(stone_error(
            context,
            format!("expected string, got {}", other.get_type()),
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
                        value.get_type()
                    ),
                ));
            };
            value_to_string(path, context)
        }
        _ => value_to_string(value, context),
    }
}

pub(crate) fn value_to_display_string(value: &Value) -> Result<String, ShellError> {
    match value {
        Value::Nothing { .. } => Ok(String::new()),
        Value::Bool { val, .. } => Ok(val.to_string()),
        Value::Int { val, .. } => Ok(val.to_string()),
        Value::Float { val, .. } => Ok(val.to_string()),
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.clone()),
        other => serde_json::to_string(&nu_to_json_value(other))
            .map_err(|err| stone_error("str", err.to_string())),
    }
}

pub(crate) fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Nothing { .. } => "NoneType",
        Value::Bool { .. } => "bool",
        Value::Int { .. } => "int",
        Value::Float { .. } => "float",
        Value::String { .. } | Value::Glob { .. } => "str",
        Value::List { .. } => "list",
        Value::Record { .. } => "dict",
        Value::Binary { .. } => "bytes",
        _ => "value",
    }
}

pub(crate) fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Nothing { .. }, Value::Nothing { .. }) => true,
        (Value::Bool { val: left, .. }, Value::Bool { val: right, .. }) => left == right,
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => left == right,
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => left == right,
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => (*left as f64) == *right,
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => *left == (*right as f64),
        (Value::String { val: left, .. }, Value::String { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::String { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::String { val: right, .. }) => left == right,
        _ => false,
    }
}

pub(crate) fn value_ordering(left: &Value, right: &Value) -> Result<Ordering, ShellError> {
    match (left, right) {
        (Value::Int { val: left, .. }, Value::Int { val: right, .. }) => Ok(left.cmp(right)),
        (Value::Float { val: left, .. }, Value::Float { val: right, .. }) => left
            .partial_cmp(right)
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::Int { val: left, .. }, Value::Float { val: right, .. }) => (*left as f64)
            .partial_cmp(right)
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::Float { val: left, .. }, Value::Int { val: right, .. }) => left
            .partial_cmp(&(*right as f64))
            .ok_or_else(|| stone_error("comparison", "cannot compare NaN values")),
        (Value::String { val: left, .. }, Value::String { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::String { val: left, .. }, Value::Glob { val: right, .. })
        | (Value::Glob { val: left, .. }, Value::String { val: right, .. }) => Ok(left.cmp(right)),
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
            format!("cannot order {} and {}", left.get_type(), right.get_type()),
        )),
    }
}

fn value_contains(container: &Value, needle: &Value) -> Result<bool, ShellError> {
    match container {
        Value::String { val, .. } | Value::Glob { val, .. } => {
            let needle = value_to_string(needle, "membership")?;
            Ok(val.contains(&needle))
        }
        Value::List { vals, .. } => Ok(vals.iter().any(|value| values_equal(value, needle))),
        Value::Record { val, .. } => {
            let needle = value_to_string(needle, "membership")?;
            Ok(val.get(&needle).is_some())
        }
        other => Err(stone_error(
            "membership",
            format!("cannot test membership in {}", other.get_type()),
        )),
    }
}

pub(crate) fn record_method_builtin(
    receiver: &Value,
    method: &str,
    args: &[Value],
) -> Result<Value, ShellError> {
    let Value::Record { val, .. } = receiver else {
        return Err(stone_error(
            method,
            format!("{} has no {method}()", receiver.get_type()),
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

pub(crate) fn min_max_builtin(values: Vec<Value>, name: &str) -> Result<Value, ShellError> {
    let mut best: Option<Value> = None;
    for value in values {
        if !matches!(value, Value::Int { .. } | Value::Float { .. }) {
            return Err(stone_error(
                name,
                format!("{name}() expected numbers, got {}", value.get_type()),
            ));
        }
        let Some(current) = &best else {
            best = Some(value);
            continue;
        };
        let ordering = value_ordering(&value, current)?;
        let should_replace = match name {
            "min" => ordering.is_lt(),
            "max" => ordering.is_gt(),
            _ => unreachable!("min/max dispatch is validated by caller"),
        };
        if should_replace {
            best = Some(value);
        }
    }
    best.ok_or_else(|| {
        stone_error(
            name,
            format!("{name}() requires at least one numeric argument"),
        )
    })
}

pub(crate) fn parse_float_builtin(value: &Value, default: Value) -> Result<Value, ShellError> {
    match value_to_f64(value, "parse_float") {
        Ok(value) => Ok(Value::float(value, Span::unknown())),
        Err(_) => match default {
            Value::Float { .. } | Value::Int { .. } => Ok(default),
            other => Err(stone_error(
                "parse_float",
                format!("default must be int or float, got {}", other.get_type()),
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
                format!("default must be int, got {}", other.get_type()),
            )),
        },
    }
}

pub(crate) fn round_builtin(value: &Value, digits: i64) -> Result<Value, ShellError> {
    let value = value_to_f64(value, "round")?;
    if !(0..=9).contains(&digits) {
        return Err(stone_error(
            "round",
            "round() digits must be between 0 and 9",
        ));
    }
    let factor = 10_f64.powi(digits as i32);
    Ok(Value::float(
        (value * factor).round() / factor,
        Span::unknown(),
    ))
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
        Value::String { val, .. } | Value::Glob { val, .. } => {
            let chars = val.chars().collect::<Vec<_>>();
            let (start, end) = normalize_slice_bounds(lower, upper, chars.len())?;
            Ok(Value::string(
                chars[start..end].iter().collect::<String>(),
                Span::unknown(),
            ))
        }
        other => Err(stone_error(
            "slice",
            format!("cannot slice {}", other.get_type()),
        )),
    }
}

pub(crate) fn split_builtin(text: &Value, separator: Option<&Value>) -> Result<Value, ShellError> {
    let text = value_to_string(text, "split")?;
    let parts = match separator {
        None => text.split_whitespace().collect::<Vec<_>>(),
        Some(separator) => {
            let separator = value_to_string(separator, "split")?;
            text.split(&separator).collect::<Vec<_>>()
        }
    };
    Ok(Value::list(
        parts
            .into_iter()
            .map(|part| Value::string(part.to_owned(), Span::unknown()))
            .collect(),
        Span::unknown(),
    ))
}

pub(crate) fn sort_key_for_value(value: &Value, field: Option<&str>) -> Result<Value, ShellError> {
    match field {
        None => Ok(value.clone()),
        Some(field) => match value {
            Value::Record { val, .. } => val
                .get(field)
                .cloned()
                .ok_or_else(|| stone_error("sort", format!("record has no key `{field}`"))),
            other => Err(stone_error(
                "sort",
                format!("key= requires record rows, got {}", other.get_type()),
            )),
        },
    }
}

pub(crate) fn sort_key_kind(value: &Value) -> Result<SortKeyKind, ShellError> {
    match value {
        Value::Int { .. } => Ok(SortKeyKind::Number),
        Value::Float { val, .. } if !val.is_nan() => Ok(SortKeyKind::Number),
        Value::Float { .. } => Err(stone_error("sort", "cannot sort NaN values")),
        Value::String { .. } | Value::Glob { .. } => Ok(SortKeyKind::Text),
        Value::List { .. } => Ok(SortKeyKind::Composite),
        other => Err(stone_error(
            "sort",
            format!("cannot sort by {}", other.get_type()),
        )),
    }
}

pub(crate) fn sort_builtin_values(
    values: Vec<Value>,
    reverse: bool,
    mut key_for_value: impl FnMut(&Value) -> Result<Value, ShellError>,
) -> Result<Value, ShellError> {
    let mut keyed = Vec::with_capacity(values.len());
    let mut key_kind = None;
    for value in values {
        let sort_key = key_for_value(&value)?;
        let next_key_kind = sort_key_kind(&sort_key)?;
        if let Some(key_kind) = key_kind {
            if key_kind != next_key_kind {
                return Err(stone_error(
                    "sort",
                    "all sort keys must have compatible types",
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
    Ok(Value::list(
        keyed.into_iter().map(|(_, value)| value).collect(),
        Span::unknown(),
    ))
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
    let (Value::String { val: text, .. } | Value::Glob { val: text, .. }) = receiver else {
        return Err(stone_error(
            method,
            format!("{} has no {method}()", receiver.get_type()),
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
        "split" => {
            let parts = match args {
                [] => text.split_whitespace().collect::<Vec<_>>(),
                [separator] => {
                    let separator = value_to_string(separator, "split")?;
                    text.split(&separator).collect::<Vec<_>>()
                }
                _ => return Err(stone_error("split", "split() takes at most one argument")),
            };
            Ok(Value::list(
                parts
                    .into_iter()
                    .map(|part| Value::string(part.to_owned(), Span::unknown()))
                    .collect(),
                Span::unknown(),
            ))
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
                    format!("expected list, got {}", items.get_type()),
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
            Ok(Value::string(zfill(text, width), Span::unknown()))
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
        (Value::Record { val, .. }, Value::String { val: key, .. })
        | (Value::Record { val, .. }, Value::Glob { val: key, .. }) => val
            .get(key)
            .cloned()
            .ok_or_else(|| stone_error("subscript", format!("record has no key `{key}`"))),
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
                value.get_type(),
                index.get_type()
            ),
        )),
    }
}

fn value_len(value: &Value) -> Result<i64, ShellError> {
    let len = match value {
        Value::List { vals, .. } => vals.len(),
        Value::Record { val, .. } => val.len(),
        Value::String { val, .. } | Value::Glob { val, .. } => val.chars().count(),
        other => {
            return Err(stone_error(
                "len",
                format!("len() does not support {}", other.get_type()),
            ));
        }
    };
    i64::try_from(len).map_err(|_| stone_error("len", "value is too large"))
}

fn normalize_slice_bounds(
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

fn zfill(text: &str, width: usize) -> String {
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

fn format_template(template: &str, args: &[String]) -> Result<String, ShellError> {
    let mut output = String::new();
    let mut chars = template.chars().peekable();
    let mut arg_index = 0;
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
            '{' if chars.peek() == Some(&'}') => {
                chars.next();
                let Some(value) = args.get(arg_index) else {
                    return Err(stone_error(
                        "format",
                        "format() has fewer arguments than placeholders",
                    ));
                };
                output.push_str(value);
                arg_index += 1;
            }
            '{' | '}' => {
                return Err(stone_error(
                    "format",
                    "format() supports only `{}` placeholders and escaped `{{` or `}}`",
                ));
            }
            _ => output.push(ch),
        }
    }
    if arg_index != args.len() {
        return Err(stone_error(
            "format",
            "format() received more arguments than placeholders",
        ));
    }
    Ok(output)
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
        Value::String { val, .. } | Value::Glob { val, .. } => {
            let text = val.trim();
            if let Ok(value) = text.parse::<i64>() {
                return Ok(SumNumber::Int(value));
            }
            text.parse::<f64>()
                .map(SumNumber::Float)
                .map_err(|err| stone_error("sum", format!("failed to parse number: {err}")))
        }
        other => Err(stone_error(
            "sum",
            format!("expected number, got {}", other.get_type()),
        )),
    }
}

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}
