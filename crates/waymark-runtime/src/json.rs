// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{
    shell_error::{generic::GenericError, io::ErrorKind, ErrorSite},
    PipelineData, Record, ShellError, Span, Value,
};
use serde_json::{json, Map, Number, Value as JsonValue};

pub(crate) fn success_response(value: JsonValue, cwd: String) -> JsonValue {
    json!({
        "ok": true,
        "cwd": cwd,
        "value": value,
    })
}

pub(crate) fn success_response_with_output(
    value: JsonValue,
    cwd: String,
    stdout: String,
    stderr: String,
) -> JsonValue {
    json!({
        "ok": true,
        "cwd": cwd,
        "value": value,
        "output": {
            "stdout": stdout,
            "stderr": stderr,
        },
    })
}

pub(crate) fn error_response(err: &ShellError, cwd: Option<String>) -> JsonValue {
    let mut root = Map::new();
    root.insert("ok".into(), JsonValue::Bool(false));
    if let Some(cwd) = cwd {
        root.insert("cwd".into(), JsonValue::String(cwd));
    }
    root.insert("error".into(), shell_error_json(err));
    JsonValue::Object(root)
}

pub(crate) fn pipeline_to_json_value(
    data: PipelineData,
    span: Span,
) -> Result<JsonValue, ShellError> {
    let value = data.into_value(span)?;
    Ok(nu_to_json_value(&value))
}

pub(crate) fn pipeline_to_json_text(data: PipelineData, span: Span) -> Result<String, ShellError> {
    let json = pipeline_to_json_value(data, span)?;
    serde_json::to_string(&json).map_err(json_encode_error)
}

pub(crate) fn parse_json_bytes(bytes: &[u8], span: Span) -> Result<Value, ShellError> {
    let json = serde_json::from_slice::<JsonValue>(bytes).map_err(|err| {
        ShellError::Generic(GenericError::new("Invalid JSON", err.to_string(), span))
    })?;
    Ok(json_to_nu_value(json, span))
}

pub(crate) fn nu_to_json_value(value: &Value) -> JsonValue {
    match value {
        Value::Nothing { .. } => JsonValue::Null,
        Value::Bool { val, .. } => JsonValue::Bool(*val),
        Value::Int { val, .. } => JsonValue::Number(Number::from(*val)),
        Value::Float { val, .. } => Number::from_f64(*val).map_or_else(
            || {
                json!({
                    "$type": "float",
                    "value": val.to_string(),
                })
            },
            JsonValue::Number,
        ),
        Value::String { val, .. } | Value::Glob { val, .. } => JsonValue::String(val.clone()),
        Value::Filesize { val, .. } => json!({
            "$type": "filesize",
            "bytes": i64::from(*val),
        }),
        Value::Duration { val, .. } => json!({
            "$type": "duration",
            "nanos": val,
        }),
        Value::Date { val, .. } => JsonValue::String(val.to_rfc3339()),
        Value::Range { val, .. } => json!({
            "$type": "range",
            "value": val.to_string(),
        }),
        Value::Binary { val, .. } => json!({
            "$type": "binary",
            "hex": encode_hex(val),
        }),
        Value::CellPath { val, .. } => json!({
            "$type": "cell-path",
            "value": val.to_string(),
        }),
        Value::Closure { .. } => json!({
            "$type": "closure",
        }),
        Value::Custom { val, .. } => json!({
            "$type": "custom",
            "name": val.type_name(),
        }),
        Value::Error { error, .. } => json!({
            "$type": "error",
            "error": shell_error_json(error),
        }),
        Value::List { vals, .. } => JsonValue::Array(vals.iter().map(nu_to_json_value).collect()),
        Value::Record { val, .. } => record_to_json(val),
    }
}

pub(crate) fn json_to_nu_value(value: JsonValue, span: Span) -> Value {
    match value {
        JsonValue::Null => Value::nothing(span),
        JsonValue::Bool(val) => Value::bool(val, span),
        JsonValue::Number(val) => {
            if let Some(int) = val.as_i64() {
                Value::int(int, span)
            } else if let Some(uint) = val.as_u64() {
                match i64::try_from(uint) {
                    Ok(int) => Value::int(int, span),
                    Err(_) => Value::float(uint as f64, span),
                }
            } else if let Some(float) = val.as_f64() {
                Value::float(float, span)
            } else {
                Value::string(val.to_string(), span)
            }
        }
        JsonValue::String(val) => Value::string(val, span),
        JsonValue::Array(vals) => Value::list(
            vals.into_iter()
                .map(|value| json_to_nu_value(value, span))
                .collect(),
            span,
        ),
        JsonValue::Object(map) => {
            let mut record = Record::with_capacity(map.len());
            for (key, value) in map {
                record.push(key, json_to_nu_value(value, span));
            }
            Value::record(record, span)
        }
    }
}

fn record_to_json(record: &Record) -> JsonValue {
    let mut map = Map::with_capacity(record.len());
    for (key, value) in record {
        map.insert(key.clone(), nu_to_json_value(value));
    }
    JsonValue::Object(map)
}

fn json_encode_error(err: serde_json::Error) -> ShellError {
    ShellError::Generic(GenericError::new_internal(
        "Failed to encode JSON",
        err.to_string(),
    ))
}

fn shell_error_json(err: &ShellError) -> JsonValue {
    let (kind, code) = error_kind_and_code(err);
    let mut map = Map::new();
    map.insert("kind".into(), JsonValue::String(kind.into()));
    map.insert("code".into(), JsonValue::String(code));
    map.insert("message".into(), JsonValue::String(err.to_string()));
    map.insert("debug".into(), JsonValue::String(format!("{err:?}")));

    match err {
        ShellError::TypeMismatch { err_message, span } => {
            map.insert("detail".into(), JsonValue::String(err_message.clone()));
            insert_span(&mut map, *span);
        }
        ShellError::RuntimeTypeMismatch {
            expected,
            actual,
            span,
        } => {
            map.insert("expected".into(), JsonValue::String(expected.to_string()));
            map.insert("actual".into(), JsonValue::String(actual.to_string()));
            insert_span(&mut map, *span);
        }
        ShellError::MissingParameter { param_name, span } => {
            map.insert("parameter".into(), JsonValue::String(param_name.clone()));
            insert_span(&mut map, *span);
        }
        ShellError::NeedsPositiveValue { span } => {
            insert_span(&mut map, *span);
        }
        ShellError::PipelineEmpty { dst_span } => {
            insert_span(&mut map, *dst_span);
        }
        ShellError::ExternalNotSupported { span } => {
            insert_span(&mut map, *span);
        }
        ShellError::Io(err) => {
            map.insert(
                "io_kind".into(),
                JsonValue::String(io_kind_string(&err.kind)),
            );
            if let Some(path) = &err.path {
                map.insert("path".into(), JsonValue::String(path.display().to_string()));
            }
            if let Some(context) = &err.additional_context {
                map.insert("context".into(), JsonValue::String(context.to_string()));
            }
            if let Some(location) = &err.location {
                map.insert("location".into(), JsonValue::String(location.clone()));
            }
            insert_span(&mut map, err.span);
        }
        ShellError::Generic(err) => {
            map.insert("detail".into(), JsonValue::String(err.msg.to_string()));
            if let Some(help) = &err.help {
                map.insert("help".into(), JsonValue::String(help.to_string()));
            }
            match &err.site {
                ErrorSite::Span(span) => insert_span(&mut map, *span),
                ErrorSite::Location(location) => {
                    map.insert("location".into(), JsonValue::String(location.clone()));
                }
            }
            if !err.inner.is_empty() {
                map.insert(
                    "related".into(),
                    JsonValue::Array(err.inner.iter().map(shell_error_json).collect()),
                );
            }
            if let Some(source) = &err.source {
                map.insert("source".into(), JsonValue::String(source.to_string()));
            }
        }
        _ => {}
    }

    JsonValue::Object(map)
}

fn error_kind_and_code(err: &ShellError) -> (&'static str, String) {
    match err {
        ShellError::TypeMismatch { .. } | ShellError::RuntimeTypeMismatch { .. } => {
            ("type_mismatch", "type_mismatch".into())
        }
        ShellError::MissingParameter { .. } => ("missing_parameter", "missing_parameter".into()),
        ShellError::NeedsPositiveValue { .. } => ("invalid_value", "needs_positive_value".into()),
        ShellError::PipelineEmpty { .. } => ("pipeline", "pipeline_empty".into()),
        ShellError::ExternalNotSupported { .. } => ("unsupported", "external_not_supported".into()),
        ShellError::Io(_) => ("io", "io_error".into()),
        ShellError::Generic(err) => match err.code.as_ref() {
            "parse_error" => ("parse", "parse_error".into()),
            "compile_error" => ("compile", "compile_error".into()),
            "nu::shell::error" => ("generic", "generic_error".into()),
            other => ("generic", other.to_string()),
        },
        _ => ("shell_error", "shell_error".into()),
    }
}

fn insert_span(map: &mut Map<String, JsonValue>, span: Span) {
    if span != Span::unknown() {
        map.insert(
            "span".into(),
            json!({
                "start": span.start,
                "end": span.end,
            }),
        );
    }
}

fn io_kind_string(kind: &ErrorKind) -> String {
    match kind {
        ErrorKind::Std(kind, ..) => std_error_kind_string(kind),
        other => to_snake_case(&format!("{other:?}")),
    }
}

fn std_error_kind_string(kind: &std::io::ErrorKind) -> String {
    to_snake_case(&format!("{kind:?}"))
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn to_snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
