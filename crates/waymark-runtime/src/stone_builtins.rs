// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cmp::Ordering;
use std::collections::HashSet;

use nu_protocol::{shell_error::generic::GenericError, ShellError, Span, Value};

use crate::json::nu_to_json_value;

pub(crate) fn value_to_int(value: &Value) -> Result<Value, ShellError> {
    value_to_i64(value, "int").map(|value| Value::int(value, Span::unknown()))
}

pub(crate) fn int_builtin(value: &Value) -> Result<Value, ShellError> {
    value_to_int(value)
}

pub(crate) fn float_builtin(value: &Value) -> Result<Value, ShellError> {
    value_to_f64(value, "float").map(|value| Value::float(value, Span::unknown()))
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

pub(crate) fn str_builtin(value: &Value) -> Result<Value, ShellError> {
    value_to_display_string(value).map(|text| Value::string(text, Span::unknown()))
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

pub(crate) fn value_identity_key(value: &Value, context: &str) -> Result<String, ShellError> {
    serde_json::to_string(&nu_to_json_value(value))
        .map_err(|err| stone_error(context, err.to_string()))
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

pub(crate) fn starts_with_builtin(text: &Value, prefix: &Value) -> Result<Value, ShellError> {
    let text = value_to_string(text, "starts_with")?;
    let prefix = value_to_string(prefix, "starts_with")?;
    Ok(Value::bool(text.starts_with(&prefix), Span::unknown()))
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

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}
