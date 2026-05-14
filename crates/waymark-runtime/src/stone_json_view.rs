// SPDX-License-Identifier: MIT OR Apache-2.0

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use nu_protocol::{ShellError, Span, Value};

use crate::json::json_to_nu_value;
use crate::stone_builtins::{
    normalize_index, record_method_builtin, subscript_builtin, value_to_string,
};

use super::stone_error;
use super::stone_runtime_value::RuntimeValue;

#[derive(Clone)]
pub(super) struct JsonlRows {
    pub(super) bytes: Arc<[u8]>,
    pub(super) lines: Arc<[JsonLineRange]>,
    pub(super) source: Arc<str>,
}

#[derive(Clone)]
pub(super) struct JsonLineRange {
    pub(super) range: Range<usize>,
    pub(super) line_number: usize,
}

#[derive(Clone)]
pub(super) struct JsonObjectView {
    pub(super) bytes: Arc<[u8]>,
    pub(super) range: Range<usize>,
    pub(super) source: Arc<str>,
    pub(super) line_number: usize,
}

#[derive(Clone)]
pub(super) struct JsonArrayView {
    pub(super) bytes: Arc<[u8]>,
    pub(super) range: Range<usize>,
}

#[derive(Clone)]
pub(super) struct JsonScalarView {
    pub(super) bytes: Arc<[u8]>,
    pub(super) range: Range<usize>,
    pub(super) source: Arc<str>,
    pub(super) line_number: usize,
}

pub(super) fn jsonl_rows_from_bytes(
    bytes: Vec<u8>,
    limit: Option<usize>,
    source: String,
) -> JsonlRows {
    let bytes: Arc<[u8]> = Arc::from(bytes);
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_number = 1usize;
    for end in memchr::memchr_iter(b'\n', &bytes) {
        push_jsonl_line_range(&bytes, start, end, line_number, limit, &mut lines);
        if limit.is_some_and(|limit| lines.len() >= limit) {
            break;
        }
        start = end + 1;
        line_number += 1;
    }
    if !limit.is_some_and(|limit| lines.len() >= limit) && start < bytes.len() {
        push_jsonl_line_range(&bytes, start, bytes.len(), line_number, limit, &mut lines);
    }
    JsonlRows {
        bytes,
        lines: Arc::from(lines),
        source: Arc::from(source),
    }
}

fn push_jsonl_line_range(
    bytes: &[u8],
    start: usize,
    end: usize,
    line_number: usize,
    limit: Option<usize>,
    lines: &mut Vec<JsonLineRange>,
) {
    if limit.is_some_and(|limit| lines.len() >= limit) {
        return;
    }
    let mut line_start = start;
    let mut line_end = end;
    while line_start < line_end && bytes[line_start].is_ascii_whitespace() {
        line_start += 1;
    }
    while line_end > line_start && bytes[line_end - 1].is_ascii_whitespace() {
        line_end -= 1;
    }
    if line_start < line_end {
        lines.push(JsonLineRange {
            range: line_start..line_end,
            line_number,
        });
    }
}

pub(super) fn jsonl_row_views(rows: &JsonlRows) -> Vec<RuntimeValue> {
    rows.lines
        .iter()
        .cloned()
        .map(|line| jsonl_row_view(rows, line))
        .collect()
}

pub(super) fn jsonl_row_view(rows: &JsonlRows, line: JsonLineRange) -> RuntimeValue {
    RuntimeValue::JsonObjectView(JsonObjectView {
        bytes: rows.bytes.clone(),
        range: line.range,
        source: rows.source.clone(),
        line_number: line.line_number,
    })
}

pub(super) fn eval_runtime_subscript(
    value: RuntimeValue,
    index: &Value,
) -> Result<RuntimeValue, ShellError> {
    match value {
        RuntimeValue::JsonObjectView(view) => json_object_view_subscript(&view, index),
        RuntimeValue::JsonArrayView(view) => json_array_view_subscript(&view, index),
        other => {
            let value = other.into_nu_value("subscript")?;
            subscript_builtin(&value, index).map(RuntimeValue::Nu)
        }
    }
}

fn json_object_view_subscript(
    view: &JsonObjectView,
    index: &Value,
) -> Result<RuntimeValue, ShellError> {
    let key = value_to_string(index, "subscript")?;
    json_object_view_get(view, &key)?
        .ok_or_else(|| stone_error("subscript", format!("record has no key `{key}`")))
}

fn json_array_view_subscript(
    view: &JsonArrayView,
    index: &Value,
) -> Result<RuntimeValue, ShellError> {
    let Value::Int { val: index, .. } = index else {
        return Err(stone_error(
            "subscript",
            "JSON array views require integer indexes",
        ));
    };
    let values = json_array_view_iter_values(view)?;
    let index = normalize_index(*index, values.len())?;
    values
        .into_iter()
        .nth(index)
        .ok_or_else(|| stone_error("subscript", format!("list index {index} is out of range")))
}

pub(super) fn eval_json_object_view_method(
    view: &JsonObjectView,
    method: &str,
    args: &[Value],
) -> Result<RuntimeValue, ShellError> {
    match method {
        "get" => {
            let [key, default] = args else {
                return Err(stone_error("get", "record.get() requires key and default"));
            };
            let key = value_to_string(key, "get")?;
            Ok(json_object_view_get(view, &key)?
                .unwrap_or_else(|| RuntimeValue::Nu(default.clone())))
        }
        "items" | "keys" | "values" => {
            let materialized = materialize_json_object_view(view)?;
            record_method_builtin(&materialized, method, args).map(RuntimeValue::Nu)
        }
        other => Err(stone_error(
            other,
            format!("JSON object views do not support method `{other}`"),
        )),
    }
}

pub(super) fn json_object_view_get(
    view: &JsonObjectView,
    key: &str,
) -> Result<Option<RuntimeValue>, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(None);
    };
    let absolute = (view.range.start + value_range.start)..(view.range.start + value_range.end);
    let value = &view.bytes[absolute.clone()];
    let value = trim_json_bytes(value);
    if value.starts_with(b"[") {
        return Ok(Some(RuntimeValue::JsonArrayView(JsonArrayView {
            bytes: view.bytes.clone(),
            range: absolute,
        })));
    }
    if value.starts_with(b"{") {
        return Ok(Some(RuntimeValue::JsonObjectView(JsonObjectView {
            bytes: view.bytes.clone(),
            range: absolute,
            source: view.source.clone(),
            line_number: view.line_number,
        })));
    }
    if !json_value_may_be_number(value) {
        let json = serde_json::from_slice::<serde_json::Value>(value).map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?;
        return Ok(Some(RuntimeValue::Nu(json_to_nu_value(
            json,
            Span::unknown(),
        ))));
    }
    Ok(Some(RuntimeValue::JsonScalarView(JsonScalarView {
        bytes: view.bytes.clone(),
        range: absolute,
        source: view.source.clone(),
        line_number: view.line_number,
    })))
}

pub(super) fn json_object_view_get_string_default(
    view: &JsonObjectView,
    key: &str,
    default: &str,
) -> Result<String, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(default.to_owned());
    };
    let value = trim_json_bytes(&bytes[value_range]);
    json_string_bytes_to_string(value).map_err(|err| {
        stone_error(
            "json view",
            format!("{} line {}: {}", view.source, view.line_number, err),
        )
    })
}

pub(super) fn json_object_view_get_f64_default(
    view: &JsonObjectView,
    key: &str,
    default: f64,
) -> Result<f64, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(default);
    };
    let value = trim_json_bytes(&bytes[value_range]);
    std::str::from_utf8(value)
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?
        .parse::<f64>()
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })
}

pub(super) fn json_object_view_get_i64_default(
    view: &JsonObjectView,
    key: &str,
    default: i64,
) -> Result<i64, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(default);
    };
    let value = trim_json_bytes(&bytes[value_range]);
    std::str::from_utf8(value)
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?
        .parse::<i64>()
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })
}

pub(super) fn json_object_view_get_array_default(
    view: &JsonObjectView,
    key: &str,
) -> Result<RuntimeValue, ShellError> {
    let bytes = &view.bytes[view.range.clone()];
    let Some(value_range) = find_top_level_json_field(bytes, key)? else {
        return Ok(RuntimeValue::Nu(Value::list(Vec::new(), Span::unknown())));
    };
    let absolute = (view.range.start + value_range.start)..(view.range.start + value_range.end);
    let value = trim_json_bytes(&view.bytes[absolute.clone()]);
    if value.starts_with(b"[") {
        return Ok(RuntimeValue::JsonArrayView(JsonArrayView {
            bytes: view.bytes.clone(),
            range: absolute,
        }));
    }
    Err(stone_error("json view", "expected JSON array"))
}

pub(super) fn runtime_value_to_string_key(
    value: &RuntimeValue,
    context: &str,
) -> Result<String, ShellError> {
    match value {
        RuntimeValue::Nu(value) => value_to_string(value, context),
        RuntimeValue::JsonScalarView(view) => json_scalar_view_to_string(view).map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        }),
        other => {
            let value = other.clone().into_nu_value(context)?;
            value_to_string(&value, context)
        }
    }
}

pub(super) fn find_top_level_json_field(
    bytes: &[u8],
    key: &str,
) -> Result<Option<Range<usize>>, ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return Err(stone_error("json view", "JSONL row is not an object"));
    }
    index += 1;
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b'}') {
            return Ok(None);
        }
        if bytes.get(index) != Some(&b'"') {
            return Err(stone_error("json view", "expected object key string"));
        }
        let key_start = index + 1;
        let key_end = json_string_end(bytes, index)?;
        index = skip_json_ws(bytes, key_end + 1);
        if bytes.get(index) != Some(&b':') {
            return Err(stone_error("json view", "expected `:` after object key"));
        }
        index = skip_json_ws(bytes, index + 1);
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        if json_key_matches(bytes, key_start..key_end, key) {
            return Ok(Some(value_start..value_end));
        }
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok(None),
            _ => return Err(stone_error("json view", "expected `,` or `}` after value")),
        }
    }
}

pub(super) fn json_object_for_each_field(
    bytes: &[u8],
    mut f: impl FnMut(Range<usize>, Range<usize>) -> Result<(), ShellError>,
) -> Result<(), ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'{') {
        return Err(stone_error("json view", "JSONL row is not an object"));
    }
    index += 1;
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b'}') {
            return Ok(());
        }
        if bytes.get(index) != Some(&b'"') {
            return Err(stone_error("json view", "expected object key string"));
        }
        let key_start = index + 1;
        let key_end = json_string_end(bytes, index)?;
        index = skip_json_ws(bytes, key_end + 1);
        if bytes.get(index) != Some(&b':') {
            return Err(stone_error("json view", "expected `:` after object key"));
        }
        index = skip_json_ws(bytes, index + 1);
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        f(key_start..key_end, value_start..value_end)?;
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b'}') => return Ok(()),
            _ => return Err(stone_error("json view", "expected `,` or `}` after value")),
        }
    }
}

pub(super) fn json_string_bytes_to_cow(bytes: &[u8]) -> Result<Cow<'_, str>, String> {
    let bytes = trim_json_bytes(bytes);
    if bytes.len() < 2 || bytes.first() != Some(&b'"') || bytes.last() != Some(&b'"') {
        return Err("expected JSON string".to_owned());
    }
    let inner = &bytes[1..bytes.len() - 1];
    if inner.contains(&b'\\') {
        serde_json::from_slice::<String>(bytes)
            .map(Cow::Owned)
            .map_err(|err| err.to_string())
    } else {
        std::str::from_utf8(inner)
            .map(Cow::Borrowed)
            .map_err(|err| err.to_string())
    }
}

pub(super) fn json_key_matches(bytes: &[u8], range: Range<usize>, key: &str) -> bool {
    let raw = &bytes[range];
    if !raw.contains(&b'\\') {
        return raw == key.as_bytes();
    }
    let mut quoted = Vec::with_capacity(raw.len() + 2);
    quoted.push(b'"');
    quoted.extend_from_slice(raw);
    quoted.push(b'"');
    serde_json::from_slice::<String>(&quoted).is_ok_and(|decoded| decoded == key)
}

pub(super) fn trim_json_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

pub(super) fn json_number_bytes_to_f64(
    bytes: &[u8],
    source: &str,
    line_number: usize,
) -> Result<f64, ShellError> {
    std::str::from_utf8(bytes)
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))?
        .parse::<f64>()
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))
}

pub(super) fn json_number_bytes_to_i64(
    bytes: &[u8],
    source: &str,
    line_number: usize,
) -> Result<i64, ShellError> {
    std::str::from_utf8(bytes)
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))?
        .parse::<i64>()
        .map_err(|err| stone_error("json view", format!("{source} line {line_number}: {err}")))
}

pub(super) fn json_array_bytes_for_each_range(
    bytes: &[u8],
    mut f: impl FnMut(Range<usize>) -> Result<(), ShellError>,
) -> Result<(), ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'[') {
        return Err(stone_error("json view", "JSON view is not an array"));
    }
    index += 1;
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b']') {
            return Ok(());
        }
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        f(value_start..value_end)?;
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b']') => return Ok(()),
            _ => {
                return Err(stone_error(
                    "json view",
                    "expected `,` or `]` after array value",
                ));
            }
        }
    }
}

pub(super) fn materialize_jsonl_rows(rows: &JsonlRows) -> Result<Value, ShellError> {
    let mut values = Vec::with_capacity(rows.lines.len());
    for row in jsonl_row_views(rows) {
        values.push(row.into_nu_value("read_jsonl")?);
    }
    Ok(Value::list(values, Span::unknown()))
}

pub(super) fn materialize_json_object_view(view: &JsonObjectView) -> Result<Value, ShellError> {
    let json = serde_json::from_slice::<serde_json::Value>(&view.bytes[view.range.clone()])
        .map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?;
    Ok(json_to_nu_value(json, Span::unknown()))
}

pub(super) fn materialize_json_array_view(view: &JsonArrayView) -> Result<Value, ShellError> {
    let json = serde_json::from_slice::<serde_json::Value>(&view.bytes[view.range.clone()])
        .map_err(|err| stone_error("json view", err.to_string()))?;
    Ok(json_to_nu_value(json, Span::unknown()))
}

pub(super) fn materialize_json_scalar_view(view: &JsonScalarView) -> Result<Value, ShellError> {
    let json = serde_json::from_slice::<serde_json::Value>(&view.bytes[view.range.clone()])
        .map_err(|err| json_scalar_view_error(view, err))?;
    Ok(json_to_nu_value(json, Span::unknown()))
}

pub(super) fn json_scalar_view_to_string(view: &JsonScalarView) -> Result<String, String> {
    json_string_bytes_to_string(&view.bytes[view.range.clone()])
}

pub(super) fn json_scalar_view_to_i64(view: &JsonScalarView) -> Result<i64, ShellError> {
    serde_json::from_slice::<i64>(&view.bytes[view.range.clone()])
        .map_err(|err| json_scalar_view_error(view, err))
}

pub(super) fn json_scalar_view_to_f64(view: &JsonScalarView) -> Result<f64, ShellError> {
    serde_json::from_slice::<f64>(&view.bytes[view.range.clone()])
        .map_err(|err| json_scalar_view_error(view, err))
}

fn json_string_bytes_to_string(bytes: &[u8]) -> Result<String, String> {
    let bytes = trim_json_bytes(bytes);
    if bytes.len() < 2 || bytes.first() != Some(&b'"') || bytes.last() != Some(&b'"') {
        return Err("expected JSON string".to_owned());
    }
    let inner = &bytes[1..bytes.len() - 1];
    if inner.contains(&b'\\') {
        serde_json::from_slice::<String>(bytes).map_err(|err| err.to_string())
    } else {
        std::str::from_utf8(inner)
            .map(str::to_owned)
            .map_err(|err| err.to_string())
    }
}

fn json_string_end(bytes: &[u8], quote: usize) -> Result<usize, ShellError> {
    let mut index = quote + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Ok(index),
            _ => index += 1,
        }
    }
    Err(stone_error("json view", "unterminated string"))
}

fn json_value_end(bytes: &[u8], start: usize) -> Result<usize, ShellError> {
    let mut index = start;
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => index = json_string_end(bytes, index)? + 1,
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' if depth > 0 => {
                depth -= 1;
                index += 1;
            }
            b',' | b'}' | b']' if depth == 0 => return Ok(index),
            _ => index += 1,
        }
    }
    Ok(index)
}

fn skip_json_ws(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

fn json_value_may_be_number(bytes: &[u8]) -> bool {
    matches!(bytes.first(), Some(b'-' | b'0'..=b'9'))
}

pub(super) fn json_array_view_iter_values(
    view: &JsonArrayView,
) -> Result<Vec<RuntimeValue>, ShellError> {
    json_array_view_element_ranges(&view.bytes[view.range.clone()])?
        .into_iter()
        .map(|range| json_array_view_value_from_relative_range(view, range))
        .collect()
}

fn json_array_view_value_from_relative_range(
    view: &JsonArrayView,
    range: Range<usize>,
) -> Result<RuntimeValue, ShellError> {
    let absolute = (view.range.start + range.start)..(view.range.start + range.end);
    let value = trim_json_bytes(&view.bytes[absolute.clone()]);
    if value.starts_with(b"[") {
        return Ok(RuntimeValue::JsonArrayView(JsonArrayView {
            bytes: view.bytes.clone(),
            range: absolute,
        }));
    }
    if value.starts_with(b"{") {
        return Ok(RuntimeValue::Nu(json_to_nu_value(
            serde_json::from_slice::<serde_json::Value>(value)
                .map_err(|err| stone_error("json view", err.to_string()))?,
            Span::unknown(),
        )));
    }
    if json_value_may_be_number(value) || value.starts_with(b"\"") {
        return Ok(RuntimeValue::JsonScalarView(JsonScalarView {
            bytes: view.bytes.clone(),
            range: absolute,
            source: Arc::from("<json-array>"),
            line_number: 0,
        }));
    }
    Ok(RuntimeValue::Nu(json_to_nu_value(
        serde_json::from_slice::<serde_json::Value>(value)
            .map_err(|err| stone_error("json view", err.to_string()))?,
        Span::unknown(),
    )))
}

fn json_array_view_element_ranges(bytes: &[u8]) -> Result<Vec<Range<usize>>, ShellError> {
    let mut index = skip_json_ws(bytes, 0);
    if bytes.get(index) != Some(&b'[') {
        return Err(stone_error("json view", "JSON view is not an array"));
    }
    index += 1;
    let mut ranges = Vec::new();
    loop {
        index = skip_json_ws(bytes, index);
        if bytes.get(index) == Some(&b']') {
            return Ok(ranges);
        }
        let value_start = index;
        let value_end = json_value_end(bytes, value_start)?;
        ranges.push(value_start..value_end);
        index = skip_json_ws(bytes, value_end);
        match bytes.get(index) {
            Some(b',') => index += 1,
            Some(b']') => return Ok(ranges),
            _ => {
                return Err(stone_error(
                    "json view",
                    "expected `,` or `]` after array value",
                ));
            }
        }
    }
}

fn json_scalar_view_error(view: &JsonScalarView, err: serde_json::Error) -> ShellError {
    stone_error(
        "json view",
        format!("{} line {}: {}", view.source, view.line_number, err),
    )
}
