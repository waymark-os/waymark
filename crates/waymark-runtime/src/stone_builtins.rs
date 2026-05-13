// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cmp::Ordering;

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

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}
