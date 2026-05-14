// SPDX-License-Identifier: MIT OR Apache-2.0

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Instant;

use nu_protocol::{PipelineData, Record, ShellError, Span, Value};

use super::stone_json_view::{
    find_top_level_json_field, json_array_bytes_for_each_range, json_key_matches,
    json_number_bytes_to_f64, json_number_bytes_to_i64, json_object_for_each_field,
    json_object_view_get, json_object_view_get_array_default, json_object_view_get_f64_default,
    json_object_view_get_i64_default, json_object_view_get_string_default,
    json_string_bytes_to_cow, trim_json_bytes, JsonObjectView, JsonlRows,
};
use super::stone_runtime_value::{RuntimeValue, TextLines};
use super::{
    stone_const_string, stone_error, value_to_f64, value_to_i64, value_to_string, EvalFlow,
    EvalProfileBucket, Evaluator,
};
use crate::stone_ast::Stmt;
use crate::stone_vm::{
    compile_hot_jsonl_loop_ir_function, compile_hot_jsonl_trace_plan,
    compile_hot_jsonl_trace_plan_from_ir, compile_hot_jsonl_vm_function,
    match_hot_jsonl_aggregation_body, optimize_stone_loop_ir, validate_hot_jsonl_native_prefix,
    AccId, GenericLoopOp, GenericLoopPlan, HotJsonlAggregationBody, HotJsonlBodyOp,
    HotJsonlNestedUserTotals, HotJsonlSlot, HotJsonlTracePlan, HotLoopIter, HotLoopOp, HotLoopPlan,
    LoopIrFusedKernel, Reg, SnapshotId, StoneAccumulatorKind, StoneFallbackTarget, StoneGuardKind,
    StoneIrFunction, StoneOp, StoneTerminator,
};

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

fn hot_jsonl_trace_plan_from_body_plan(body_plan: &HotJsonlAggregationBody) -> HotJsonlTracePlan {
    HotJsonlTracePlan {
        user_name: body_plan.user_name.clone(),
        user_key: body_plan.user_key.clone(),
        user_has_default: body_plan.user_has_default,
        user_default: body_plan.user_default.clone(),
        user_amounts_map: body_plan.user_amounts_map.clone(),
        user_amount_key: body_plan.user_amount_key.clone(),
        user_amount_has_default: body_plan.user_amount_has_default,
        user_amount_default: body_plan.user_amount_default,
        user_items_map: body_plan.user_items_map.clone(),
        user_items_key: body_plan.user_items_key.clone(),
        user_items_has_default: body_plan.user_items_has_default,
        user_items_default: body_plan.user_items_default,
        users_list: body_plan.users_list.clone(),
        tags_key: body_plan.tags_key.clone(),
        tags_default_empty: body_plan.tags_default_empty,
        tag_counts_map: body_plan.tag_counts_map.clone(),
        tags_list: body_plan.tags_list.clone(),
    }
}

struct StoneMaterializedSnapshot {
    locals: Vec<(String, RuntimeValue)>,
}

enum StoneVmExecutionResult {
    Completed,
    Fallback { snapshot: SnapshotId },
}

impl Evaluator<'_> {
    pub(super) fn execute_hot_loop_prefix(
        &mut self,
        plan: &HotLoopPlan,
        row: &JsonObjectView,
    ) -> Result<(), ShellError> {
        for op in &plan.ops {
            match op {
                HotLoopOp::GenericFallback => {
                    return Err(stone_error(
                        "hot loop",
                        "generic fallback op cannot execute",
                    ));
                }
                HotLoopOp::JsonGetStrDefault {
                    target,
                    key,
                    default,
                } => {
                    let value = json_object_view_get_string_default(row, key, default)?;
                    self.state.set_local(
                        target.clone(),
                        RuntimeValue::Nu(Value::string(value, Span::unknown())),
                    );
                }
                HotLoopOp::JsonGetF64Default {
                    target,
                    key,
                    default,
                } => {
                    let value = json_object_view_get_f64_default(row, key, *default)?;
                    self.state.set_local(
                        target.clone(),
                        RuntimeValue::Nu(Value::float(value, Span::unknown())),
                    );
                }
                HotLoopOp::JsonGetI64Default {
                    target,
                    key,
                    default,
                } => {
                    let value = json_object_view_get_i64_default(row, key, *default)?;
                    self.state.set_local(
                        target.clone(),
                        RuntimeValue::Nu(Value::int(value, Span::unknown())),
                    );
                }
                HotLoopOp::JsonGetArrayDefault { target, key } => {
                    let value = json_object_view_get_array_default(row, key)?;
                    self.state.set_local(target.clone(), value);
                }
                HotLoopOp::JsonGetValue { target, key } => {
                    let value = json_object_view_get(row, key)?.ok_or_else(|| {
                        stone_error("hot loop", format!("record has no key `{key}`"))
                    })?;
                    self.state.set_local(target.clone(), value);
                }
            }
        }
        Ok(())
    }

    pub(super) fn eval_for_jsonl_rows_hot_native_body(
        &mut self,
        targets: &[String],
        rows: &JsonlRows,
        prefix_plan: &HotLoopPlan,
        body_plan: &HotJsonlAggregationBody,
    ) -> Result<(), ShellError> {
        if !validate_hot_jsonl_native_prefix(prefix_plan, body_plan) {
            return Err(stone_error(
                "hot loop",
                "native JSONL aggregation prefix does not match body",
            ));
        }
        let vm_function = compile_hot_jsonl_vm_function(body_plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native VM body ops"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan_from_ir(vm_function)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native loop IR"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_row = None;
        let execution_started = Instant::now();
        if self.state.profiler.enabled {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                let started = self.state.profiler.start();
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                self.state
                    .profiler
                    .finish(EvalProfileBucket::ForJsonlBody, started);
                last_row = Some(line);
            }
        } else {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                last_row = Some(line);
            }
        }
        if let Some(line) = last_row {
            let row = JsonObjectView {
                bytes: rows.bytes.clone(),
                range: line.range,
                source: rows.source.clone(),
                line_number: line.line_number,
            };
            if let Some(target) = targets.first() {
                self.state
                    .set_local(target.clone(), RuntimeValue::JsonObjectView(row.clone()));
            }
            let snapshot = self.materialize_hot_jsonl_vm_snapshot_row_locals(
                vm_function,
                SnapshotId(0),
                &row,
            )?;
            if self.state.hot_loop_validate_snapshot {
                self.validate_stone_materialized_snapshot(&snapshot)?;
            }
            self.apply_stone_materialized_snapshot(snapshot);
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        self.apply_hot_jsonl_row_count(body_plan, rows.lines.len())?;
        Ok(())
    }

    pub(super) fn eval_for_jsonl_rows_generic_native_body(
        &mut self,
        targets: &[String],
        rows: &JsonlRows,
        plan: &GenericLoopPlan,
    ) -> Result<(), ShellError> {
        let [GenericLoopOp::JsonlAggregation { body: body_plan }] = plan.ops.as_slice() else {
            return Err(stone_error(
                "hot loop",
                "expected JSONL aggregation loop IR",
            ));
        };
        if body_plan.nested_user_totals.is_some() {
            return self.eval_for_jsonl_rows_nested_totals_native_body(targets, rows, body_plan);
        }
        let vm_function = compile_hot_jsonl_loop_ir_function(plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native loop IR"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan_from_ir(vm_function)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL native loop IR"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_row = None;
        let execution_started = Instant::now();
        if self.state.profiler.enabled {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                let started = self.state.profiler.start();
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                self.state
                    .profiler
                    .finish(EvalProfileBucket::ForJsonlBody, started);
                last_row = Some(line);
            }
        } else {
            for line in rows.lines.iter().cloned() {
                let row = HotJsonlRowSlice {
                    bytes: &rows.bytes[line.range.clone()],
                    source: rows.source.as_ref(),
                    line_number: line.line_number,
                };
                if self.state.hot_loop_vm_interpreter {
                    self.execute_hot_jsonl_vm_function_or_fallback(
                        vm_function,
                        &row,
                        &mut accumulators,
                    )?;
                } else {
                    self.execute_hot_jsonl_aggregation_native_body(
                        &trace_plan,
                        &row,
                        &mut accumulators,
                    )?;
                }
                last_row = Some(line);
            }
        }
        if let Some(line) = last_row {
            let row = JsonObjectView {
                bytes: rows.bytes.clone(),
                range: line.range,
                source: rows.source.clone(),
                line_number: line.line_number,
            };
            if let Some(target) = targets.first() {
                self.state
                    .set_local(target.clone(), RuntimeValue::JsonObjectView(row.clone()));
            }
            let snapshot = self.materialize_hot_jsonl_vm_snapshot_row_locals(
                vm_function,
                SnapshotId(0),
                &row,
            )?;
            if self.state.hot_loop_validate_snapshot {
                self.validate_stone_materialized_snapshot(&snapshot)?;
            }
            self.apply_stone_materialized_snapshot(snapshot);
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        self.apply_hot_jsonl_row_count(body_plan, rows.lines.len())?;
        Ok(())
    }

    fn eval_for_jsonl_rows_nested_totals_native_body(
        &mut self,
        targets: &[String],
        rows: &JsonlRows,
        body_plan: &HotJsonlAggregationBody,
    ) -> Result<(), ShellError> {
        let nested = body_plan
            .nested_user_totals
            .as_ref()
            .ok_or_else(|| stone_error("hot loop", "missing nested totals plan"))?;
        let trace_plan = hot_jsonl_trace_plan_from_body_plan(body_plan);
        self.state.hot_loop_diagnostics.loop_ir_lowered();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_selected(LoopIrFusedKernel::JsonlAggregation);
        let mut accumulators =
            self.load_nested_hot_jsonl_native_accumulators(&trace_plan, nested)?;
        let mut last_row = None;
        let execution_started = Instant::now();
        for line in rows.lines.iter().cloned() {
            let row = HotJsonlRowSlice {
                bytes: &rows.bytes[line.range.clone()],
                source: rows.source.as_ref(),
                line_number: line.line_number,
            };
            let fields = json_object_view_get_hot_jsonl_row_fields(
                &row,
                &trace_plan,
                &body_plan.user_key,
                &body_plan.user_amount_key,
                &body_plan.user_items_key,
                &body_plan.tags_key,
            )?;
            let user_key = fields.user.into_owned();
            match accumulators.user_amounts.get_mut(&user_key) {
                Some(total) => *total += fields.amount,
                None => {
                    accumulators
                        .user_amounts
                        .insert(user_key.clone(), fields.amount);
                    accumulators.users.push(user_key.clone());
                }
            }
            *accumulators.user_items.entry(user_key).or_insert(0) += fields.items;
            hot_jsonl_string_array_for_each_string(&fields.tags, |tag| {
                let tag_key = tag.as_ref();
                if let Some(count) = accumulators.tag_counts.get_mut(tag_key) {
                    *count += 1;
                } else {
                    accumulators.tag_counts.insert(tag_key.to_owned(), 1);
                    accumulators.tags.push(tag_key.to_owned());
                }
                Ok(())
            })?;
            last_row = Some(line);
        }

        if let Some(line) = last_row {
            let row = JsonObjectView {
                bytes: rows.bytes.clone(),
                range: line.range,
                source: rows.source.clone(),
                line_number: line.line_number,
            };
            if let Some(target) = targets.first() {
                self.state
                    .set_local(target.clone(), RuntimeValue::JsonObjectView(row.clone()));
            }
            let row = HotJsonlRowSlice {
                bytes: &row.bytes[row.range.clone()],
                source: row.source.as_ref(),
                line_number: row.line_number,
            };
            let user = if body_plan.user_has_default {
                hot_jsonl_row_get_string_default(
                    &row,
                    &body_plan.user_key,
                    &body_plan.user_default,
                )?
            } else {
                hot_jsonl_row_get_string_required(&row, &body_plan.user_key)?
            };
            self.state.set_local(
                body_plan.user_name.clone(),
                RuntimeValue::Nu(Value::string(user.into_owned(), Span::unknown())),
            );
        }

        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        self.state.set_local(
            nested.map_name.clone(),
            RuntimeValue::Nu(Value::record(
                nested_totals_record_from_native_maps(
                    &accumulators.users,
                    &accumulators.user_amounts,
                    &accumulators.user_items,
                    &nested.amount_field,
                    &nested.items_field,
                ),
                Span::unknown(),
            )),
        );
        self.state.set_local(
            body_plan.tag_counts_map.clone(),
            RuntimeValue::Nu(Value::record(
                i64_record_from_native_map(&accumulators.tags, &accumulators.tag_counts),
                Span::unknown(),
            )),
        );
        self.apply_hot_jsonl_row_count(body_plan, rows.lines.len())?;
        Ok(())
    }

    fn apply_hot_jsonl_row_count(
        &mut self,
        body_plan: &HotJsonlAggregationBody,
        row_count: usize,
    ) -> Result<(), ShellError> {
        let Some(local) = body_plan.row_count_local.as_deref() else {
            return Ok(());
        };
        let addend = i64::try_from(row_count)
            .map_err(|_| stone_error("hot loop", "row count is too large"))?;
        let value = self
            .state
            .get_local_mut(local)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{local}`")))?;
        let RuntimeValue::Nu(value) = value else {
            return Err(stone_error(
                "hot loop",
                format!("{local} is not an integer counter"),
            ));
        };
        let current = value_to_i64(value, "hot loop")?;
        *value = Value::int(
            current
                .checked_add(addend)
                .ok_or_else(|| stone_error("hot loop", "row count overflow"))?,
            Span::unknown(),
        );
        Ok(())
    }

    pub(super) fn eval_for_text_lines_jsonl_generic_native_body(
        &mut self,
        targets: &[String],
        lines: &TextLines,
        plan: &GenericLoopPlan,
    ) -> Result<(), ShellError> {
        let [GenericLoopOp::JsonlAggregation { .. }] = plan.ops.as_slice() else {
            return Err(stone_error("hot loop", "expected JSONL text-lines loop IR"));
        };
        let vm_function = compile_hot_jsonl_loop_ir_function(plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines loop IR"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan_from_ir(vm_function)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines loop IR"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_line = None;
        let execution_started = Instant::now();
        for (index, line) in lines.lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row = HotJsonlRowSlice {
                bytes: line.as_bytes(),
                source: lines.source.as_str(),
                line_number: index + 1,
            };
            let started = self.state.profiler.start();
            if self.state.hot_loop_vm_interpreter {
                self.execute_hot_jsonl_vm_function_or_fallback(
                    vm_function,
                    &row,
                    &mut accumulators,
                )?;
            } else {
                self.execute_hot_jsonl_aggregation_native_body(
                    &trace_plan,
                    &row,
                    &mut accumulators,
                )?;
            }
            self.state
                .profiler
                .finish(EvalProfileBucket::ForJsonlBody, started);
            last_line = Some(line.clone());
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        if let Some(line) = last_line {
            if let Some(target) = targets.first() {
                self.state.set_local(
                    target.clone(),
                    RuntimeValue::Nu(Value::string(line, Span::unknown())),
                );
            }
        }
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        Ok(())
    }

    pub(super) fn eval_for_text_lines_hot_jsonl(
        &mut self,
        targets: &[String],
        lines: TextLines,
        body: &[Stmt],
        plan: &HotLoopPlan,
    ) -> Result<EvalFlow, ShellError> {
        let remaining_body = body.get(plan.body_start..).unwrap_or(&[]);
        let Some(body_plan) = match_hot_jsonl_aggregation_body(&plan.target, remaining_body) else {
            self.state
                .hot_loop_diagnostics
                .lowering_miss("unsupported_body_stmt");
            let values = lines
                .lines
                .into_iter()
                .map(|line| RuntimeValue::Nu(Value::string(line, Span::unknown())));
            return self.eval_for_values(targets, values, body);
        };
        let vm_function = compile_hot_jsonl_vm_function(&body_plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines VM body ops"))?;
        let optimization = optimize_stone_loop_ir(&vm_function);
        let vm_function = &optimization.function;
        self.validate_hot_jsonl_vm_guards(vm_function)?;
        self.record_hot_jsonl_ir_fused_selection(&optimization)?;
        let trace_plan = compile_hot_jsonl_trace_plan(&body_plan)
            .ok_or_else(|| stone_error("hot loop", "unsupported JSONL text-lines body ops"))?;
        let mut accumulators = self.load_hot_jsonl_native_accumulators(&trace_plan)?;
        let mut last_line = None;
        let execution_started = Instant::now();
        let line_target = match &plan.iter {
            HotLoopIter::JsonlTextLines { line_target } => line_target,
            _ => return Err(stone_error("hot loop", "unsupported_iterator")),
        };
        for (index, line) in lines.lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let row = HotJsonlRowSlice {
                bytes: line.as_bytes(),
                source: lines.source.as_str(),
                line_number: index + 1,
            };
            let started = self.state.profiler.start();
            if self.state.hot_loop_vm_interpreter {
                self.execute_hot_jsonl_vm_function_or_fallback(
                    vm_function,
                    &row,
                    &mut accumulators,
                )?;
            } else {
                self.execute_hot_jsonl_aggregation_native_body(
                    &trace_plan,
                    &row,
                    &mut accumulators,
                )?;
            }
            self.state
                .profiler
                .finish(EvalProfileBucket::ForJsonlBody, started);
            last_line = Some(line.clone());
        }
        let execution_duration = execution_started.elapsed();
        self.state
            .hot_loop_diagnostics
            .loop_vm_time(execution_duration);
        self.state.hot_loop_diagnostics.loop_vm_executed();
        self.state
            .hot_loop_diagnostics
            .fused_kernel_time(execution_duration);
        self.state
            .hot_loop_diagnostics
            .fused_kernel_executed(LoopIrFusedKernel::JsonlAggregation);
        if let Some(line) = last_line {
            if let Some(target) = targets.first() {
                self.state.set_local(
                    target.clone(),
                    RuntimeValue::Nu(Value::string(line.clone(), Span::unknown())),
                );
            }
            self.state.set_local(
                line_target.clone(),
                RuntimeValue::Nu(Value::string(line, Span::unknown())),
            );
        }
        let snapshot = self.materialize_hot_jsonl_vm_snapshot_accumulators(
            vm_function,
            SnapshotId(0),
            accumulators,
        )?;
        if self.state.hot_loop_validate_snapshot {
            self.validate_stone_materialized_snapshot(&snapshot)?;
        }
        self.apply_stone_materialized_snapshot(snapshot);
        Ok(EvalFlow::Output(PipelineData::empty()))
    }

    fn execute_hot_jsonl_aggregation_native_body(
        &mut self,
        plan: &HotJsonlTracePlan,
        row: &HotJsonlRowSlice<'_>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        self.execute_hot_jsonl_native_trace_body(plan, row, accumulators)
    }

    fn validate_hot_jsonl_vm_guards(&self, function: &StoneIrFunction) -> Result<(), ShellError> {
        for guard in &function.guards {
            if function.snapshots.get(guard.snapshot.0 as usize).is_none() {
                return Err(stone_error("hot loop", "VM guard snapshot is out of range"));
            }
            match guard.kind {
                StoneGuardKind::InputIsJsonObject { reg } => {
                    if reg != Reg(0) {
                        return Err(stone_error("hot loop", "unsupported VM input guard"));
                    }
                }
                StoneGuardKind::AccumulatorShape { acc, kind } => {
                    let spec = function.accumulators.get(acc.0 as usize).ok_or_else(|| {
                        stone_error("hot loop", "VM accumulator guard is out of range")
                    })?;
                    if spec.kind != kind {
                        return Err(stone_error(
                            "hot loop",
                            "VM accumulator guard does not match accumulator shape",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn materialize_hot_jsonl_vm_snapshot_accumulators(
        &self,
        function: &StoneIrFunction,
        snapshot_id: SnapshotId,
        accumulators: HotJsonlNativeAccumulators,
    ) -> Result<StoneMaterializedSnapshot, ShellError> {
        let snapshot = function
            .snapshots
            .get(snapshot_id.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM snapshot is out of range"))?;
        if snapshot.resume != StoneFallbackTarget::LoopBody {
            return Err(stone_error("hot loop", "unsupported VM snapshot target"));
        }

        let mut locals = Vec::new();
        for accumulator in &snapshot.accumulators {
            let spec = function
                .accumulators
                .get(accumulator.acc.0 as usize)
                .ok_or_else(|| {
                    stone_error("hot loop", "VM snapshot accumulator is out of range")
                })?;
            match (accumulator.acc, spec.kind) {
                (AccId(0), StoneAccumulatorKind::F64Map) => {
                    locals.push((
                        accumulator.local_name.clone(),
                        RuntimeValue::Nu(Value::record(
                            f64_record_from_native_map(
                                &accumulators.users,
                                &accumulators.user_amounts,
                            ),
                            Span::unknown(),
                        )),
                    ));
                }
                (AccId(1), StoneAccumulatorKind::I64Map) => {
                    locals.push((
                        accumulator.local_name.clone(),
                        RuntimeValue::Nu(Value::record(
                            i64_record_from_native_map(
                                &accumulators.users,
                                &accumulators.user_items,
                            ),
                            Span::unknown(),
                        )),
                    ));
                }
                (AccId(2), StoneAccumulatorKind::StringList) => {
                    if !accumulator.local_name.is_empty() {
                        locals.push((
                            accumulator.local_name.clone(),
                            RuntimeValue::Nu(string_list_from_ordered_keys(&accumulators.users)),
                        ));
                    }
                }
                (AccId(3), StoneAccumulatorKind::I64Map) => {
                    locals.push((
                        accumulator.local_name.clone(),
                        RuntimeValue::Nu(Value::record(
                            i64_record_from_native_map(
                                &accumulators.tags,
                                &accumulators.tag_counts,
                            ),
                            Span::unknown(),
                        )),
                    ));
                }
                (AccId(4), StoneAccumulatorKind::StringList) => {
                    if !accumulator.local_name.is_empty() {
                        locals.push((
                            accumulator.local_name.clone(),
                            RuntimeValue::Nu(string_list_from_ordered_keys(&accumulators.tags)),
                        ));
                    }
                }
                _ => {
                    return Err(stone_error(
                        "hot loop",
                        "unsupported VM snapshot accumulator materialization",
                    ));
                }
            }
        }

        Ok(StoneMaterializedSnapshot { locals })
    }

    fn materialize_hot_jsonl_vm_snapshot_row_locals(
        &self,
        function: &StoneIrFunction,
        snapshot_id: SnapshotId,
        row: &JsonObjectView,
    ) -> Result<StoneMaterializedSnapshot, ShellError> {
        let snapshot = function
            .snapshots
            .get(snapshot_id.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM snapshot is out of range"))?;
        let row = HotJsonlRowSlice {
            bytes: &row.bytes[row.range.clone()],
            source: row.source.as_ref(),
            line_number: row.line_number,
        };
        let mut locals = Vec::new();
        for local in &snapshot.locals {
            let local_name = function
                .locals
                .get(local.local.0 as usize)
                .ok_or_else(|| stone_error("hot loop", "VM snapshot local is out of range"))?
                .name
                .clone();
            match local.reg {
                Reg(1) => {
                    let value = self.materialize_hot_jsonl_vm_user_reg(function, &row)?;
                    locals.push((
                        local_name,
                        RuntimeValue::Nu(Value::string(value.into_owned(), Span::unknown())),
                    ));
                }
                _ => {
                    return Err(stone_error(
                        "hot loop",
                        "unsupported VM snapshot local register",
                    ));
                }
            }
        }
        Ok(StoneMaterializedSnapshot { locals })
    }

    fn apply_stone_materialized_snapshot(&mut self, snapshot: StoneMaterializedSnapshot) {
        for (name, value) in snapshot.locals {
            self.state.set_local(name, value);
        }
    }

    fn validate_stone_materialized_snapshot(
        &self,
        snapshot: &StoneMaterializedSnapshot,
    ) -> Result<(), ShellError> {
        for (name, value) in &snapshot.locals {
            if name.is_empty() {
                return Err(stone_error(
                    "hot loop",
                    "materialized VM snapshot contains an empty local name",
                ));
            }
            if !matches!(value, RuntimeValue::Nu(_) | RuntimeValue::JsonObjectView(_)) {
                return Err(stone_error(
                    "hot loop",
                    "materialized VM snapshot contains a non-materialized value",
                ));
            }
        }
        Ok(())
    }

    fn materialize_hot_jsonl_vm_user_reg<'a>(
        &self,
        function: &StoneIrFunction,
        row: &HotJsonlRowSlice<'a>,
    ) -> Result<Cow<'a, str>, ShellError> {
        let row_block = function
            .blocks
            .get(function.entry.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM entry block is out of range"))?;
        let Some(user_op) = row_block.ops.first() else {
            return Err(stone_error("hot loop", "VM row block has no user op"));
        };
        match user_op {
            StoneOp::JsonGetStrDefault {
                dst: Reg(1),
                object: Reg(0),
                key,
                default,
            } => {
                let key = stone_const_string(function, *key)?;
                let default = stone_const_string(function, *default)?;
                hot_jsonl_row_get_string_default(row, key, default)
            }
            StoneOp::JsonGetValue {
                dst: Reg(1),
                object: Reg(0),
                key,
            } => {
                let key = stone_const_string(function, *key)?;
                hot_jsonl_row_get_string_required(row, key)
            }
            _ => Err(stone_error("hot loop", "unsupported VM user snapshot op")),
        }
    }

    fn execute_hot_jsonl_vm_function_or_fallback<'a>(
        &mut self,
        function: &StoneIrFunction,
        row: &'a HotJsonlRowSlice<'a>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        match self.execute_hot_jsonl_vm_function(function, row, accumulators)? {
            StoneVmExecutionResult::Completed => Ok(()),
            StoneVmExecutionResult::Fallback { snapshot } => {
                self.state.hot_loop_diagnostics.loop_fallback();
                Err(stone_error(
                    "hot loop",
                    format!(
                        "VM requested fallback to snapshot {}, but AST resume is not implemented",
                        snapshot.0
                    ),
                ))
            }
        }
    }

    fn execute_hot_jsonl_vm_function<'a>(
        &self,
        function: &StoneIrFunction,
        row: &'a HotJsonlRowSlice<'a>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<StoneVmExecutionResult, ShellError> {
        self.validate_hot_jsonl_vm_accumulators(function)?;
        let mut slots = vec![StoneVmSlot::Empty; function.registers as usize];
        self.set_stone_vm_slot(&mut slots, Reg(0), StoneVmSlot::Row(row))?;
        let mut block = function.entry;
        loop {
            let block_ref = function
                .blocks
                .get(block.0 as usize)
                .ok_or_else(|| stone_error("hot loop", "VM block is out of range"))?;
            if let Some(snapshot) =
                self.execute_hot_jsonl_vm_ops(function, &block_ref.ops, &mut slots, accumulators)?
            {
                return Ok(StoneVmExecutionResult::Fallback { snapshot });
            }
            match &block_ref.terminator {
                StoneTerminator::JsonEachStrArray {
                    array,
                    item,
                    body,
                    done,
                } => {
                    let tags = self.stone_vm_string_array(&slots, *array)?;
                    let mut fallback = None;
                    hot_jsonl_string_array_for_each_string(&tags, |tag| {
                        if fallback.is_some() {
                            return Ok(());
                        }
                        self.set_stone_vm_slot(
                            &mut slots,
                            *item,
                            StoneVmSlot::String(tag.into_owned()),
                        )?;
                        let body_ref = function.blocks.get(body.0 as usize).ok_or_else(|| {
                            stone_error("hot loop", "VM tag block is out of range")
                        })?;
                        if let Some(snapshot) = self.execute_hot_jsonl_vm_ops(
                            function,
                            &body_ref.ops,
                            &mut slots,
                            accumulators,
                        )? {
                            fallback = Some(snapshot);
                            return Ok(());
                        }
                        match body_ref.terminator {
                            StoneTerminator::Jump { target } if target == block => Ok(()),
                            _ => {
                                fallback = Some(SnapshotId(0));
                                Ok(())
                            }
                        }
                    })?;
                    if let Some(snapshot) = fallback {
                        return Ok(StoneVmExecutionResult::Fallback { snapshot });
                    }
                    block = *done;
                }
                StoneTerminator::Jump { target } => {
                    block = *target;
                }
                StoneTerminator::Return => return Ok(StoneVmExecutionResult::Completed),
            }
        }
    }

    fn execute_hot_jsonl_vm_ops<'a>(
        &self,
        function: &StoneIrFunction,
        ops: &[StoneOp],
        slots: &mut [StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<Option<SnapshotId>, ShellError> {
        for op in ops {
            match op {
                StoneOp::JsonGetStrDefault {
                    dst,
                    object,
                    key,
                    default,
                } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let default = stone_const_string(function, *default)?;
                    let value = hot_jsonl_row_get_string_default(row, key, default)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::String(value.into_owned()))?;
                }
                StoneOp::JsonGetValue { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_string_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::String(value.into_owned()))?;
                }
                StoneOp::JsonGetF64Default {
                    dst,
                    object,
                    key,
                    default,
                } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_f64_default(row, key, *default)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::F64(value))?;
                }
                StoneOp::JsonGetF64Required { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_f64_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::F64(value))?;
                }
                StoneOp::JsonGetI64Default {
                    dst,
                    object,
                    key,
                    default,
                } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_i64_default(row, key, *default)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::I64(value))?;
                }
                StoneOp::JsonGetI64Required { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_i64_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::I64(value))?;
                }
                StoneOp::JsonGetArrayDefault { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_array_default(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::StringArray(value))?;
                }
                StoneOp::JsonGetArrayRequired { dst, object, key } => {
                    let row = self.stone_vm_row(slots, *object)?;
                    let key = stone_const_string(function, *key)?;
                    let value = hot_jsonl_row_get_array_required(row, key)?;
                    self.set_stone_vm_slot(slots, *dst, StoneVmSlot::StringArray(value))?;
                }
                StoneOp::MapAddF64 {
                    map,
                    key,
                    value,
                    append,
                } => {
                    self.execute_hot_jsonl_vm_map_add_f64(
                        *map,
                        *key,
                        *value,
                        *append,
                        slots,
                        accumulators,
                    )?;
                }
                StoneOp::MapAddI64 {
                    map,
                    key,
                    value,
                    append,
                } => {
                    if append.is_some() {
                        return Err(stone_error("hot loop", "unsupported i64 append map op"));
                    }
                    self.execute_hot_jsonl_vm_map_add_i64(*map, *key, *value, slots, accumulators)?;
                }
                StoneOp::MapAddI64Const {
                    map,
                    key,
                    value,
                    append,
                } => {
                    self.execute_hot_jsonl_vm_map_add_i64_const(
                        *map,
                        *key,
                        *value,
                        *append,
                        slots,
                        accumulators,
                    )?;
                }
            }
        }
        Ok(None)
    }

    fn validate_hot_jsonl_vm_accumulators(
        &self,
        function: &StoneIrFunction,
    ) -> Result<(), ShellError> {
        let [user_amounts, user_items, users, tag_counts, tags] = function.accumulators.as_slice()
        else {
            return Err(stone_error("hot loop", "unsupported VM accumulator layout"));
        };
        if user_amounts.kind != StoneAccumulatorKind::F64Map
            || user_items.kind != StoneAccumulatorKind::I64Map
            || users.kind != StoneAccumulatorKind::StringList
            || tag_counts.kind != StoneAccumulatorKind::I64Map
            || tags.kind != StoneAccumulatorKind::StringList
        {
            return Err(stone_error("hot loop", "unsupported VM accumulator kind"));
        }
        Ok(())
    }

    fn execute_hot_jsonl_vm_map_add_f64<'a>(
        &self,
        map: AccId,
        key: Reg,
        value: Reg,
        append: Option<AccId>,
        slots: &[StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        if map != AccId(0) || !matches!(append, None | Some(AccId(2))) {
            return Err(stone_error("hot loop", "unsupported VM f64 map add"));
        }
        let value = self.stone_vm_f64(slots, value)?;
        let key = self.stone_vm_string(slots, key)?;
        if let Some(total) = accumulators.user_amounts.get_mut(key.as_str()) {
            *total += value;
        } else {
            accumulators.user_amounts.insert(key.clone(), value);
            if append.is_some() {
                accumulators.users.push(key);
            }
        }
        Ok(())
    }

    fn execute_hot_jsonl_vm_map_add_i64<'a>(
        &self,
        map: AccId,
        key: Reg,
        value: Reg,
        slots: &[StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        if map != AccId(1) {
            return Err(stone_error("hot loop", "unsupported VM i64 map add"));
        }
        let value = self.stone_vm_i64(slots, value)?;
        let key = self.stone_vm_string(slots, key)?;
        if let Some(total) = accumulators.user_items.get_mut(key.as_str()) {
            *total += value;
        } else {
            accumulators.user_items.insert(key, value);
        }
        Ok(())
    }

    fn execute_hot_jsonl_vm_map_add_i64_const<'a>(
        &self,
        map: AccId,
        key: Reg,
        value: i64,
        append: Option<AccId>,
        slots: &[StoneVmSlot<'a>],
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        if map != AccId(3) || !matches!(append, None | Some(AccId(4))) {
            return Err(stone_error("hot loop", "unsupported VM i64 const map add"));
        }
        let key = self.stone_vm_string(slots, key)?;
        if let Some(total) = accumulators.tag_counts.get_mut(key.as_str()) {
            *total += value;
        } else {
            accumulators.tag_counts.insert(key.clone(), value);
            if append.is_some() {
                accumulators.tags.push(key);
            }
        }
        Ok(())
    }

    fn set_stone_vm_slot<'a>(
        &self,
        slots: &mut [StoneVmSlot<'a>],
        reg: Reg,
        value: StoneVmSlot<'a>,
    ) -> Result<(), ShellError> {
        let slot = slots
            .get_mut(reg.0 as usize)
            .ok_or_else(|| stone_error("hot loop", "VM register is out of range"))?;
        *slot = value;
        Ok(())
    }

    fn stone_vm_row<'a>(
        &self,
        slots: &[StoneVmSlot<'a>],
        reg: Reg,
    ) -> Result<&'a HotJsonlRowSlice<'a>, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::Row(row)) => Ok(*row),
            _ => Err(stone_error("hot loop", "VM register is not a row")),
        }
    }

    fn stone_vm_string(&self, slots: &[StoneVmSlot<'_>], reg: Reg) -> Result<String, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::String(value)) => Ok(value.clone()),
            _ => Err(stone_error("hot loop", "VM register is not a string")),
        }
    }

    fn stone_vm_f64(&self, slots: &[StoneVmSlot<'_>], reg: Reg) -> Result<f64, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::F64(value)) => Ok(*value),
            _ => Err(stone_error("hot loop", "VM register is not an f64")),
        }
    }

    fn stone_vm_i64(&self, slots: &[StoneVmSlot<'_>], reg: Reg) -> Result<i64, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::I64(value)) => Ok(*value),
            _ => Err(stone_error("hot loop", "VM register is not an i64")),
        }
    }

    fn stone_vm_string_array<'a>(
        &self,
        slots: &[StoneVmSlot<'a>],
        reg: Reg,
    ) -> Result<HotJsonlStringArray<'a>, ShellError> {
        match slots.get(reg.0 as usize) {
            Some(StoneVmSlot::StringArray(value)) => Ok(*value),
            _ => Err(stone_error("hot loop", "VM register is not a string array")),
        }
    }

    fn execute_hot_jsonl_native_trace_body(
        &mut self,
        plan: &HotJsonlTracePlan,
        row: &HotJsonlRowSlice<'_>,
        accumulators: &mut HotJsonlNativeAccumulators,
    ) -> Result<(), ShellError> {
        let fields = json_object_view_get_hot_jsonl_row_fields(
            row,
            plan,
            &plan.user_key,
            &plan.user_amount_key,
            &plan.user_items_key,
            &plan.tags_key,
        )?;

        if let Some(total) = accumulators.user_amounts.get_mut(fields.user.as_ref()) {
            *total += fields.amount;
            *accumulators
                .user_items
                .get_mut(fields.user.as_ref())
                .ok_or_else(|| {
                    stone_error("hot loop", "user item accumulator is inconsistent")
                })? += fields.items;
        } else {
            let user_key = fields.user.into_owned();
            accumulators
                .user_amounts
                .insert(user_key.clone(), fields.amount);
            accumulators
                .user_items
                .insert(user_key.clone(), fields.items);
            accumulators.users.push(user_key);
        }

        hot_jsonl_string_array_for_each_string(&fields.tags, |tag_key| {
            if let Some(count) = accumulators.tag_counts.get_mut(tag_key.as_ref()) {
                *count += 1;
            } else {
                let tag_key = tag_key.into_owned();
                accumulators.tag_counts.insert(tag_key.clone(), 1);
                accumulators.tags.push(tag_key);
            }
            Ok(())
        })?;

        Ok(())
    }

    #[allow(dead_code)]
    fn execute_hot_jsonl_native_ops<'a>(
        &mut self,
        ops: &[HotJsonlBodyOp],
        plan: &HotJsonlAggregationBody,
        row: &'a JsonObjectView,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &mut HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        for op in ops {
            match op {
                HotJsonlBodyOp::JsonGetFields {
                    user_key,
                    amount_key,
                    items_key,
                    tags_key,
                } => {
                    let trace_plan = compile_hot_jsonl_trace_plan(plan).ok_or_else(|| {
                        stone_error("hot loop", "unsupported JSONL native body ops")
                    })?;
                    let row_slice = HotJsonlRowSlice {
                        bytes: &row.bytes[row.range.clone()],
                        source: row.source.as_ref(),
                        line_number: row.line_number,
                    };
                    slots.fields = Some(json_object_view_get_hot_jsonl_row_fields(
                        &row_slice,
                        &trace_plan,
                        user_key,
                        amount_key,
                        items_key,
                        tags_key,
                    )?);
                }
                HotJsonlBodyOp::MapAddF64 {
                    map_name,
                    key_slot,
                    value_slot,
                    append_list,
                } => {
                    self.execute_hot_jsonl_map_add_f64(
                        map_name,
                        *key_slot,
                        *value_slot,
                        append_list.as_deref(),
                        plan,
                        accumulators,
                        slots,
                    )?;
                }
                HotJsonlBodyOp::MapAddI64 {
                    map_name,
                    key_slot,
                    value_slot,
                } => {
                    self.execute_hot_jsonl_map_add_i64(
                        map_name,
                        *key_slot,
                        *value_slot,
                        plan,
                        accumulators,
                        slots,
                    )?;
                }
                HotJsonlBodyOp::ForEachJsonString {
                    array_slot,
                    item_slot,
                    body,
                } => {
                    self.execute_hot_jsonl_for_each_string(
                        *array_slot,
                        *item_slot,
                        body,
                        plan,
                        accumulators,
                        slots,
                    )?;
                }
                HotJsonlBodyOp::MapAddI64Const { .. } => {
                    return Err(stone_error(
                        "hot loop",
                        "constant map update requires a string iteration slot",
                    ));
                }
            }
        }
        Ok(())
    }

    fn execute_hot_jsonl_map_add_f64<'a>(
        &self,
        _map_name: &str,
        key_slot: HotJsonlSlot,
        value_slot: HotJsonlSlot,
        append_list: Option<&str>,
        _plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        if key_slot != HotJsonlSlot::User {
            return Err(stone_error("hot loop", "unsupported f64 map update"));
        }
        let value = match value_slot {
            HotJsonlSlot::Amount => hot_jsonl_fields(slots)?.amount,
            _ => return Err(stone_error("hot loop", "unsupported f64 value slot")),
        };
        let append_user = append_list.is_some();
        let user = hot_jsonl_user(slots)?;
        if let Some(total) = accumulators.user_amounts.get_mut(user.as_ref()) {
            *total += value;
        } else {
            let user_key = user.into_owned();
            accumulators.user_amounts.insert(user_key.clone(), value);
            if append_user {
                accumulators.users.push(user_key);
            }
        }
        Ok(())
    }

    fn execute_hot_jsonl_map_add_i64<'a>(
        &self,
        _map_name: &str,
        key_slot: HotJsonlSlot,
        value_slot: HotJsonlSlot,
        _plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        if key_slot != HotJsonlSlot::User {
            return Err(stone_error("hot loop", "unsupported i64 map update"));
        }
        let value = match value_slot {
            HotJsonlSlot::Items => hot_jsonl_fields(slots)?.items,
            _ => return Err(stone_error("hot loop", "unsupported i64 value slot")),
        };
        let user = hot_jsonl_user(slots)?;
        if let Some(total) = accumulators.user_items.get_mut(user.as_ref()) {
            *total += value;
        } else {
            accumulators.user_items.insert(user.into_owned(), value);
        }
        Ok(())
    }

    fn execute_hot_jsonl_for_each_string<'a>(
        &self,
        array_slot: HotJsonlSlot,
        item_slot: HotJsonlSlot,
        body: &[HotJsonlBodyOp],
        plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        slots: &HotJsonlNativeSlots<'a>,
    ) -> Result<(), ShellError> {
        if array_slot != HotJsonlSlot::Tags || item_slot != HotJsonlSlot::Tag {
            return Err(stone_error(
                "hot loop",
                "unsupported string iteration slots",
            ));
        }
        let tags = &hot_jsonl_fields(slots)?.tags;
        hot_jsonl_string_array_for_each_string(tags, |tag_key| {
            for op in body {
                match op {
                    HotJsonlBodyOp::MapAddI64Const {
                        map_name,
                        key_slot,
                        value,
                        append_list,
                    } => {
                        self.execute_hot_jsonl_tag_count_update(
                            map_name,
                            *key_slot,
                            *value,
                            append_list.as_deref(),
                            plan,
                            accumulators,
                            tag_key.as_ref(),
                        )?;
                    }
                    _ => {
                        return Err(stone_error(
                            "hot loop",
                            "unsupported string iteration body op",
                        ));
                    }
                }
            }
            Ok(())
        })
    }

    fn execute_hot_jsonl_tag_count_update(
        &self,
        _map_name: &str,
        key_slot: HotJsonlSlot,
        value: i64,
        append_list: Option<&str>,
        _plan: &HotJsonlAggregationBody,
        accumulators: &mut HotJsonlNativeAccumulators,
        tag_key: &str,
    ) -> Result<(), ShellError> {
        if key_slot != HotJsonlSlot::Tag {
            return Err(stone_error("hot loop", "unsupported tag count update"));
        }
        if let Some(count) = accumulators.tag_counts.get_mut(tag_key) {
            *count += value;
        } else {
            accumulators.tag_counts.insert(tag_key.to_owned(), value);
            if append_list.is_some() {
                accumulators.tags.push(tag_key.to_owned());
            }
        }
        Ok(())
    }

    fn load_hot_jsonl_native_accumulators(
        &self,
        plan: &HotJsonlTracePlan,
    ) -> Result<HotJsonlNativeAccumulators, ShellError> {
        Ok(HotJsonlNativeAccumulators {
            user_amounts: self.load_f64_record_map(&plan.user_amounts_map)?,
            user_items: self.load_i64_record_map(&plan.user_items_map)?,
            users: match &plan.users_list {
                Some(name) => self.load_string_list(name)?,
                None => Vec::new(),
            },
            tag_counts: self.load_i64_record_map(&plan.tag_counts_map)?,
            tags: match &plan.tags_list {
                Some(name) => self.load_string_list(name)?,
                None => Vec::new(),
            },
        })
    }

    fn load_nested_hot_jsonl_native_accumulators(
        &self,
        plan: &HotJsonlTracePlan,
        nested: &HotJsonlNestedUserTotals,
    ) -> Result<HotJsonlNativeAccumulators, ShellError> {
        let (users, user_amounts, user_items) = self.load_nested_totals_record_map(nested)?;
        Ok(HotJsonlNativeAccumulators {
            user_amounts,
            user_items,
            users,
            tag_counts: self.load_i64_record_map(&plan.tag_counts_map)?,
            tags: match &plan.tags_list {
                Some(name) => self.load_string_list(name)?,
                None => Vec::new(),
            },
        })
    }

    fn load_nested_totals_record_map(
        &self,
        nested: &HotJsonlNestedUserTotals,
    ) -> Result<(Vec<String>, HashMap<String, f64>, HashMap<String, i64>), ShellError> {
        let value = self.state.get_local(&nested.map_name).ok_or_else(|| {
            stone_error("hot loop", format!("unknown name `{}`", nested.map_name))
        })?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = value else {
            return Err(stone_error(
                "hot loop",
                format!("{} is not a record", nested.map_name),
            ));
        };
        let mut users = Vec::with_capacity(val.len());
        let mut amounts = HashMap::with_capacity(val.len());
        let mut items = HashMap::with_capacity(val.len());
        for (user, value) in val.iter() {
            let Value::Record { val: totals, .. } = value else {
                return Err(stone_error(
                    "hot loop",
                    format!("{}[{user}] is not a record", nested.map_name),
                ));
            };
            let amount = totals
                .get(&nested.amount_field)
                .ok_or_else(|| {
                    stone_error(
                        "hot loop",
                        format!(
                            "{}[{user}] has no `{}`",
                            nested.map_name, nested.amount_field
                        ),
                    )
                })
                .and_then(|value| value_to_f64(value, "hot loop"))?;
            let item_count = totals
                .get(&nested.items_field)
                .ok_or_else(|| {
                    stone_error(
                        "hot loop",
                        format!(
                            "{}[{user}] has no `{}`",
                            nested.map_name, nested.items_field
                        ),
                    )
                })
                .and_then(|value| value_to_i64(value, "hot loop"))?;
            users.push(user.clone());
            amounts.insert(user.clone(), amount);
            items.insert(user.clone(), item_count);
        }
        Ok((users, amounts, items))
    }

    fn load_f64_record_map(&self, name: &str) -> Result<HashMap<String, f64>, ShellError> {
        let value = self
            .state
            .get_local(name)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{name}`")))?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = value else {
            return Err(stone_error("hot loop", format!("{name} is not a record")));
        };
        let mut map = HashMap::with_capacity(val.len());
        for (key, value) in val.iter() {
            map.insert(key.clone(), value_to_f64(value, "hot loop")?);
        }
        Ok(map)
    }

    pub(super) fn load_i64_record_map(
        &self,
        name: &str,
    ) -> Result<HashMap<String, i64>, ShellError> {
        let value = self
            .state
            .get_local(name)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{name}`")))?;
        let RuntimeValue::Nu(Value::Record { val, .. }) = value else {
            return Err(stone_error("hot loop", format!("{name} is not a record")));
        };
        let mut map = HashMap::with_capacity(val.len());
        for (key, value) in val.iter() {
            map.insert(key.clone(), value_to_i64(value, "hot loop")?);
        }
        Ok(map)
    }

    pub(super) fn store_i64_record_map(&mut self, name: &str, map: &HashMap<String, i64>) {
        self.state.set_local(
            name.to_owned(),
            RuntimeValue::Nu(Value::record(
                i64_record_from_native_map(&[], map),
                Span::unknown(),
            )),
        );
    }

    fn load_string_list(&self, name: &str) -> Result<Vec<String>, ShellError> {
        let value = self
            .state
            .get_local(name)
            .ok_or_else(|| stone_error("hot loop", format!("unknown name `{name}`")))?;
        let RuntimeValue::Nu(Value::List { vals, .. }) = value else {
            return Err(stone_error("hot loop", format!("{name} is not a list")));
        };
        vals.iter()
            .map(|value| value_to_string(value, "hot loop"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashMap;

    use super::{
        f64_record_from_native_map, hot_jsonl_fields, hot_jsonl_row_get_array_default,
        hot_jsonl_row_get_array_required, hot_jsonl_row_get_f64_default,
        hot_jsonl_row_get_f64_required, hot_jsonl_row_get_i64_default,
        hot_jsonl_row_get_i64_required, hot_jsonl_row_get_string_default,
        hot_jsonl_row_get_string_required, hot_jsonl_string_array_for_each_string, hot_jsonl_user,
        i64_record_from_native_map, json_object_view_get_hot_jsonl_row_fields,
        nested_totals_record_from_native_maps, string_list_from_ordered_keys, HotJsonlNativeSlots,
        HotJsonlRowFields, HotJsonlRowSlice, HotJsonlStringArray,
    };
    use crate::stone_vm::HotJsonlTracePlan;

    #[test]
    fn hot_jsonl_row_getters_return_values_defaults_and_errors() {
        let row = row(br#"{"user":"Ada","amount":2.5,"items":3,"tags":["rust","stone"]}"#);

        assert_eq!(
            hot_jsonl_row_get_string_required(&row, "user").expect("user"),
            Cow::Borrowed("Ada")
        );
        assert_eq!(
            hot_jsonl_row_get_string_default(&row, "missing", "fallback").expect("default"),
            Cow::<str>::Owned("fallback".to_string())
        );
        assert_eq!(
            hot_jsonl_row_get_f64_required(&row, "amount").expect("amount"),
            2.5
        );
        assert_eq!(
            hot_jsonl_row_get_f64_default(&row, "missing", 4.5).expect("default"),
            4.5
        );
        assert_eq!(
            hot_jsonl_row_get_i64_required(&row, "items").expect("items"),
            3
        );
        assert_eq!(
            hot_jsonl_row_get_i64_default(&row, "missing", 9).expect("default"),
            9
        );
        assert_eq!(
            hot_jsonl_row_get_array_required(&row, "tags")
                .expect("tags")
                .bytes,
            br#"["rust","stone"]"#
        );
        assert_eq!(
            hot_jsonl_row_get_array_default(&row, "missing")
                .expect("default tags")
                .bytes,
            b"[]"
        );

        assert!(hot_jsonl_row_get_string_required(&row, "missing").is_err());
        assert!(hot_jsonl_row_get_array_required(&row, "user").is_err());
    }

    #[test]
    fn hot_jsonl_row_fields_apply_plan_defaults() {
        let plan = trace_plan();
        let defaulted_row = row(br#"{"amount":1.25,"tags":["a"]}"#);
        let fields = json_object_view_get_hot_jsonl_row_fields(
            &defaulted_row,
            &plan,
            "user",
            "amount",
            "items",
            "tags",
        )
        .expect("fields");

        assert_eq!(fields.user, Cow::<str>::Owned("anonymous".to_string()));
        assert_eq!(fields.amount, 1.25);
        assert_eq!(fields.items, 0);
        assert_eq!(fields.tags.bytes, br#"["a"]"#);

        let bad_tags = row(br#"{"user":"Ada","amount":1,"items":2,"tags":"nope"}"#);
        assert!(json_object_view_get_hot_jsonl_row_fields(
            &bad_tags, &plan, "user", "amount", "items", "tags",
        )
        .is_err());
    }

    #[test]
    fn hot_jsonl_slots_require_initialized_fields() {
        let empty = HotJsonlNativeSlots::default();
        assert!(hot_jsonl_fields(&empty).is_err());

        let slots = HotJsonlNativeSlots {
            fields: Some(HotJsonlRowFields {
                user: Cow::Borrowed("Ada"),
                amount: 1.0,
                items: 2,
                tags: HotJsonlStringArray { bytes: b"[]" },
            }),
        };
        assert_eq!(hot_jsonl_user(&slots).expect("user"), Cow::Borrowed("Ada"));
    }

    #[test]
    fn hot_jsonl_string_array_iterates_strings() {
        let array = HotJsonlStringArray {
            bytes: br#"["A","B\nC"]"#,
        };
        let mut values = Vec::new();

        hot_jsonl_string_array_for_each_string(&array, |value| {
            values.push(value.into_owned());
            Ok(())
        })
        .expect("iterate");

        assert_eq!(values, ["A", "B\nC"]);
        assert!(hot_jsonl_string_array_for_each_string(
            &HotJsonlStringArray { bytes: br#"[1]"# },
            |_| Ok(())
        )
        .is_err());
    }

    #[test]
    fn native_map_materializers_preserve_ordered_keys_then_extras() {
        let ordered = vec!["b".to_string(), "a".to_string()];
        let mut f64s = HashMap::new();
        f64s.insert("a".to_string(), 1.0);
        f64s.insert("b".to_string(), 2.0);
        f64s.insert("c".to_string(), 3.0);
        let f64_record = f64_record_from_native_map(&ordered, &f64s);
        assert_eq!(
            f64_record.get("b").expect("b").as_float().expect("f64"),
            2.0
        );
        assert_eq!(
            f64_record.get("a").expect("a").as_float().expect("f64"),
            1.0
        );

        let mut i64s = HashMap::new();
        i64s.insert("a".to_string(), 1);
        i64s.insert("b".to_string(), 2);
        let i64_record = i64_record_from_native_map(&ordered, &i64s);
        assert_eq!(i64_record.get("b").expect("b").as_int().expect("i64"), 2);

        let list = string_list_from_ordered_keys(&ordered);
        assert_eq!(list.as_list().expect("list").len(), 2);
    }

    #[test]
    fn nested_totals_materializer_requires_both_maps() {
        let ordered = vec!["ada".to_string()];
        let mut amounts = HashMap::new();
        amounts.insert("ada".to_string(), 10.0);
        amounts.insert("missing-items".to_string(), 99.0);
        let mut items = HashMap::new();
        items.insert("ada".to_string(), 3);
        items.insert("missing-amount".to_string(), 4);

        let record =
            nested_totals_record_from_native_maps(&ordered, &amounts, &items, "amount", "items");
        assert!(record.get("ada").is_some());
        assert!(record.get("missing-items").is_none());
        assert!(record.get("missing-amount").is_none());
    }

    fn row(bytes: &'static [u8]) -> HotJsonlRowSlice<'static> {
        HotJsonlRowSlice {
            bytes,
            source: "rows.jsonl",
            line_number: 1,
        }
    }

    fn trace_plan() -> HotJsonlTracePlan {
        HotJsonlTracePlan {
            user_name: "user".to_string(),
            user_key: "user".to_string(),
            user_has_default: true,
            user_default: "anonymous".to_string(),
            user_amounts_map: "amounts".to_string(),
            user_amount_key: "amount".to_string(),
            user_amount_has_default: true,
            user_amount_default: 0.0,
            user_items_map: "items".to_string(),
            user_items_key: "items".to_string(),
            user_items_has_default: true,
            user_items_default: 0,
            users_list: Some("users".to_string()),
            tags_key: "tags".to_string(),
            tags_default_empty: true,
            tag_counts_map: "tags".to_string(),
            tags_list: Some("tags_list".to_string()),
        }
    }
}
