// SPDX-License-Identifier: MIT OR Apache-2.0

use std::borrow::Cow;
use std::collections::HashMap;

use nu_protocol::{Record, ShellError, Span, Value};

use super::{
    find_top_level_json_field, json_array_bytes_for_each_range, json_key_matches,
    json_number_bytes_to_f64, json_number_bytes_to_i64, json_object_for_each_field,
    json_string_bytes_to_cow, stone_error, trim_json_bytes,
};
use crate::stone_vm::HotJsonlTracePlan;

pub(super) struct HotJsonlNativeAccumulators {
    pub(super) user_amounts: HashMap<String, f64>,
    pub(super) user_items: HashMap<String, i64>,
    pub(super) users: Vec<String>,
    pub(super) tag_counts: HashMap<String, i64>,
    pub(super) tags: Vec<String>,
}

pub(super) struct HotJsonlRowFields<'a> {
    pub(super) user: Cow<'a, str>,
    pub(super) amount: f64,
    pub(super) items: i64,
    pub(super) tags: HotJsonlStringArray<'a>,
}

#[derive(Clone, Copy)]
pub(super) struct HotJsonlStringArray<'a> {
    pub(super) bytes: &'a [u8],
}

pub(super) struct HotJsonlRowSlice<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) source: &'a str,
    pub(super) line_number: usize,
}

#[derive(Default)]
pub(super) struct HotJsonlNativeSlots<'a> {
    pub(super) fields: Option<HotJsonlRowFields<'a>>,
}

#[derive(Clone)]
pub(super) enum StoneVmSlot<'a> {
    Empty,
    Row(&'a HotJsonlRowSlice<'a>),
    String(String),
    F64(f64),
    I64(i64),
    StringArray(HotJsonlStringArray<'a>),
}

pub(super) fn json_object_view_get_hot_jsonl_row_fields<'a>(
    view: &HotJsonlRowSlice<'a>,
    plan: &HotJsonlTracePlan,
    user_key: &str,
    amount_key: &str,
    items_key: &str,
    tags_key: &str,
) -> Result<HotJsonlRowFields<'a>, ShellError> {
    let bytes = view.bytes;
    let mut user = None;
    let mut amount = None;
    let mut items = None;
    let mut tags = None;

    json_object_for_each_field(bytes, |key_range, value_range| {
        if user.is_none() && json_key_matches(bytes, key_range.clone(), user_key) {
            user = Some(value_range);
        } else if amount.is_none() && json_key_matches(bytes, key_range.clone(), amount_key) {
            amount = Some(value_range);
        } else if items.is_none() && json_key_matches(bytes, key_range.clone(), items_key) {
            items = Some(value_range);
        } else if tags.is_none() && json_key_matches(bytes, key_range, tags_key) {
            tags = Some(value_range);
        }
        Ok(())
    })?;

    let user = match user {
        Some(range) => json_string_bytes_to_cow(trim_json_bytes(&bytes[range])).map_err(|err| {
            stone_error(
                "json view",
                format!("{} line {}: {}", view.source, view.line_number, err),
            )
        })?,
        None if plan.user_has_default => Cow::Owned(plan.user_default.clone()),
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{user_key}`"),
            ));
        }
    };
    let amount = match amount {
        Some(range) => json_number_bytes_to_f64(
            trim_json_bytes(&bytes[range]),
            view.source,
            view.line_number,
        )?,
        None if plan.user_amount_has_default => plan.user_amount_default,
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{amount_key}`"),
            ));
        }
    };
    let items = match items {
        Some(range) => json_number_bytes_to_i64(
            trim_json_bytes(&bytes[range]),
            view.source,
            view.line_number,
        )?,
        None if plan.user_items_has_default => plan.user_items_default,
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{items_key}`"),
            ));
        }
    };
    let tags = match tags {
        Some(range) => {
            let value = trim_json_bytes(&bytes[range]);
            if !value.starts_with(b"[") {
                return Err(stone_error("json view", "expected JSON array"));
            }
            HotJsonlStringArray { bytes: value }
        }
        None if plan.tags_default_empty => HotJsonlStringArray { bytes: b"[]" },
        None => {
            return Err(stone_error(
                "json view",
                format!("record has no key `{tags_key}`"),
            ));
        }
    };

    Ok(HotJsonlRowFields {
        user,
        amount,
        items,
        tags,
    })
}

pub(super) fn hot_jsonl_row_get_string_default<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
    default: &str,
) -> Result<Cow<'a, str>, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(Cow::Owned(default.to_owned()));
    };
    json_string_bytes_to_cow(trim_json_bytes(&row.bytes[value_range])).map_err(|err| {
        stone_error(
            "json view",
            format!("{} line {}: {}", row.source, row.line_number, err),
        )
    })
}

pub(super) fn hot_jsonl_row_get_string_required<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
) -> Result<Cow<'a, str>, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    json_string_bytes_to_cow(trim_json_bytes(&row.bytes[value_range])).map_err(|err| {
        stone_error(
            "json view",
            format!("{} line {}: {}", row.source, row.line_number, err),
        )
    })
}

pub(super) fn hot_jsonl_row_get_f64_default(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
    default: f64,
) -> Result<f64, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(default);
    };
    json_number_bytes_to_f64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

pub(super) fn hot_jsonl_row_get_f64_required(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
) -> Result<f64, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    json_number_bytes_to_f64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

pub(super) fn hot_jsonl_row_get_i64_default(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
    default: i64,
) -> Result<i64, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(default);
    };
    json_number_bytes_to_i64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

pub(super) fn hot_jsonl_row_get_i64_required(
    row: &HotJsonlRowSlice<'_>,
    key: &str,
) -> Result<i64, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    json_number_bytes_to_i64(
        trim_json_bytes(&row.bytes[value_range]),
        row.source,
        row.line_number,
    )
}

pub(super) fn hot_jsonl_row_get_array_default<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
) -> Result<HotJsonlStringArray<'a>, ShellError> {
    let Some(value_range) = find_top_level_json_field(row.bytes, key)? else {
        return Ok(HotJsonlStringArray { bytes: b"[]" });
    };
    let value = trim_json_bytes(&row.bytes[value_range]);
    if !value.starts_with(b"[") {
        return Err(stone_error("json view", "expected JSON array"));
    }
    Ok(HotJsonlStringArray { bytes: value })
}

pub(super) fn hot_jsonl_row_get_array_required<'a>(
    row: &HotJsonlRowSlice<'a>,
    key: &str,
) -> Result<HotJsonlStringArray<'a>, ShellError> {
    let value_range = find_top_level_json_field(row.bytes, key)?
        .ok_or_else(|| stone_error("json view", format!("record has no key `{key}`")))?;
    let value = trim_json_bytes(&row.bytes[value_range]);
    if !value.starts_with(b"[") {
        return Err(stone_error("json view", "expected JSON array"));
    }
    Ok(HotJsonlStringArray { bytes: value })
}

pub(super) fn hot_jsonl_fields<'a>(
    slots: &'a HotJsonlNativeSlots<'_>,
) -> Result<&'a HotJsonlRowFields<'a>, ShellError> {
    slots
        .fields
        .as_ref()
        .ok_or_else(|| stone_error("hot loop", "JSONL field slots are not initialized"))
}

pub(super) fn hot_jsonl_user<'a>(
    slots: &'a HotJsonlNativeSlots<'_>,
) -> Result<Cow<'a, str>, ShellError> {
    Ok(match &hot_jsonl_fields(slots)?.user {
        Cow::Borrowed(user) => Cow::Borrowed(*user),
        Cow::Owned(user) => Cow::Owned(user.clone()),
    })
}

pub(super) fn hot_jsonl_string_array_for_each_string(
    view: &HotJsonlStringArray<'_>,
    mut f: impl for<'a> FnMut(Cow<'a, str>) -> Result<(), ShellError>,
) -> Result<(), ShellError> {
    json_array_bytes_for_each_range(view.bytes, |range| {
        let value = trim_json_bytes(&view.bytes[range]);
        let text = json_string_bytes_to_cow(value)
            .map_err(|err| stone_error("json view", err.to_string()))?;
        f(text)
    })
}

pub(super) fn f64_record_from_native_map(keys: &[String], map: &HashMap<String, f64>) -> Record {
    let mut record = Record::with_capacity(map.len());
    for key in keys {
        if let Some(value) = map.get(key) {
            record.push(key.clone(), Value::float(*value, Span::unknown()));
        }
    }
    for (key, value) in map {
        if !keys.contains(key) {
            record.push(key.clone(), Value::float(*value, Span::unknown()));
        }
    }
    record
}

pub(super) fn nested_totals_record_from_native_maps(
    keys: &[String],
    amounts: &HashMap<String, f64>,
    items: &HashMap<String, i64>,
    amount_field: &str,
    items_field: &str,
) -> Record {
    let mut record = Record::with_capacity(amounts.len().max(items.len()));
    for key in keys {
        if let Some(value) = nested_totals_value(key, amounts, items, amount_field, items_field) {
            record.push(key.clone(), value);
        }
    }
    for key in amounts.keys().chain(items.keys()) {
        if keys.contains(key) || record.get(key).is_some() {
            continue;
        }
        if let Some(value) = nested_totals_value(key, amounts, items, amount_field, items_field) {
            record.push(key.clone(), value);
        }
    }
    record
}

fn nested_totals_value(
    key: &str,
    amounts: &HashMap<String, f64>,
    items: &HashMap<String, i64>,
    amount_field: &str,
    items_field: &str,
) -> Option<Value> {
    let amount = amounts.get(key)?;
    let item_count = items.get(key)?;
    let mut totals = Record::with_capacity(2);
    totals.push(
        amount_field.to_owned(),
        Value::float(*amount, Span::unknown()),
    );
    totals.push(
        items_field.to_owned(),
        Value::int(*item_count, Span::unknown()),
    );
    Some(Value::record(totals, Span::unknown()))
}

pub(super) fn i64_record_from_native_map(keys: &[String], map: &HashMap<String, i64>) -> Record {
    let mut record = Record::with_capacity(map.len());
    for key in keys {
        if let Some(value) = map.get(key) {
            record.push(key.clone(), Value::int(*value, Span::unknown()));
        }
    }
    for (key, value) in map {
        if !keys.contains(key) {
            record.push(key.clone(), Value::int(*value, Span::unknown()));
        }
    }
    record
}

pub(super) fn string_list_from_ordered_keys(keys: &[String]) -> Value {
    Value::list(
        keys.iter()
            .map(|key| Value::string(key.clone(), Span::unknown()))
            .collect(),
        Span::unknown(),
    )
}
