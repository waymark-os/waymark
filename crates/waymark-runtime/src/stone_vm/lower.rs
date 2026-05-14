// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{
    AccId, BlockId, ConstId, GenericLoopIter, GenericLoopOp, GenericLoopPlan, GenericParseNumber,
    GenericVmConst, GenericVmExprBody, GenericVmExprOp, GenericVmOp, HotJsonlAggregationBody,
    HotJsonlBodyOp, HotJsonlSlot, HotJsonlTracePlan, HotLoopOp, HotLoopPlan, LocalId, LoopIrBlock,
    LoopIrDiagnostics, LoopIrFunction, LoopIrIteratorAdapter, LoopIrSnapshot,
    LoopIrSnapshotBoundary, LoopIrTerminator, OuterJsonlFileLoopBody, Reg, SnapshotId,
    StoneAccumulatorKind, StoneAccumulatorSpec, StoneBlock, StoneConst, StoneFallbackTarget,
    StoneGuard, StoneGuardKind, StoneIrFunction, StoneLocal, StoneOp, StoneSnapshot,
    StoneSnapshotAccumulator, StoneSnapshotLocal, StoneTerminator,
};

use crate::stone_ast::{AssignTarget, AugOp, Call, CompareOp, Expr, Stmt};
use crate::stone_vm::{match_hot_jsonl_aggregation_body, match_hot_jsonl_aggregation_ir_subgraph};

pub(crate) struct FusedMapUpdateIf {
    pub(crate) key_name: String,
    pub(crate) contains_map: String,
    pub(crate) updates: Vec<FusedMapUpdate>,
    pub(crate) append_list: Option<String>,
}

pub(crate) struct FusedMapUpdate {
    pub(crate) map_name: String,
    pub(crate) addend: Expr,
}

pub(crate) fn try_lower_hot_loop(
    targets: &[String],
    iter: &Expr,
    body: &[Stmt],
) -> Option<HotLoopPlan> {
    let [target] = targets else {
        return None;
    };
    if let Expr::Call(Call {
        name,
        positional,
        named,
    }) = iter
    {
        if name == "read_jsonl" && !positional.is_empty() && named.is_empty() {
            let ops = lower_prefix_ops(target, body);
            if ops.is_empty() {
                return None;
            }
            let body_start = ops.len();
            return Some(HotLoopPlan {
                target: target.clone(),
                iter: super::HotLoopIter::ReadJsonl {
                    path: super::JsonlPathExpr::Dynamic,
                },
                ops,
                body_start,
            });
        }
    }

    let (record_target, body_start) = lower_json_loads_line_prefix(target, body)?;
    Some(HotLoopPlan {
        target: record_target,
        iter: super::HotLoopIter::JsonlTextLines {
            line_target: target.clone(),
        },
        ops: Vec::new(),
        body_start,
    })
}

pub(crate) fn try_lower_generic_loop(
    targets: &[String],
    iter: &Expr,
    body: &[Stmt],
) -> Option<GenericLoopPlan> {
    let [target] = targets else {
        return None;
    };
    let iter = match iter {
        Expr::List(_) | Expr::Tuple(_) | Expr::Name(_) => GenericLoopIter::MaterializedList,
        Expr::MethodCall {
            method, positional, ..
        } if method == "splitlines" && positional.is_empty() => GenericLoopIter::OpenSplitlines,
        Expr::Call(Call {
            name,
            positional,
            named,
        }) if name == "range" && !positional.is_empty() && named.is_empty() => {
            GenericLoopIter::Range
        }
        Expr::Call(Call {
            name,
            positional,
            named,
        }) if name == "read_jsonl" && !positional.is_empty() && named.is_empty() => {
            GenericLoopIter::ReadJsonl
        }
        Expr::Call(Call {
            name,
            positional,
            named,
        }) if name == "read_csv" && !positional.is_empty() && named.is_empty() => {
            GenericLoopIter::ReadCsv
        }
        _ => return None,
    };
    if matches!(
        iter,
        GenericLoopIter::ReadJsonl | GenericLoopIter::MaterializedList
    ) {
        if let Some(body) = match_generic_jsonl_aggregation_body(target, body) {
            return Some(GenericLoopPlan {
                target: target.clone(),
                iter,
                ops: vec![GenericLoopOp::JsonlAggregation { body }],
            });
        }
    }
    if matches!(
        iter,
        GenericLoopIter::OpenSplitlines | GenericLoopIter::MaterializedList
    ) {
        if let Some((record_target, body_start)) = lower_json_loads_line_prefix(target, body) {
            if let Some(body) =
                match_hot_jsonl_aggregation_body(&record_target, body.get(body_start..)?)
            {
                return Some(GenericLoopPlan {
                    target: target.clone(),
                    iter,
                    ops: vec![GenericLoopOp::JsonlAggregation { body }],
                });
            }
        }
    }
    let op = lower_generic_record_field_count(target, body).or_else(|| {
        let [stmt] = body else {
            return None;
        };
        lower_generic_loop_stmt(target, stmt)
    });
    let op = op.or_else(|| {
        (!body.is_empty()).then(|| GenericLoopOp::ExprBody {
            body: body.to_vec(),
        })
    })?;
    Some(GenericLoopPlan {
        target: target.clone(),
        iter,
        ops: vec![op],
    })
}

pub(crate) fn generic_loop_compile_miss_reason(plan: &GenericLoopPlan) -> &'static str {
    let [op] = plan.ops.as_slice() else {
        return "generic_loop_multi_op";
    };
    match op {
        GenericLoopOp::JsonlAggregation { .. } => "jsonl_aggregation_wrong_input",
        GenericLoopOp::ExprBody { body }
            if match_outer_jsonl_file_loop_body(&plan.target, body).is_some() =>
        {
            "outer_jsonl_file_loop"
        }
        GenericLoopOp::ExprBody { .. } => "unsupported_expr_body",
        GenericLoopOp::AddAssign { item, .. }
        | GenericLoopOp::AddAssignParsedInt { item, .. }
        | GenericLoopOp::AddAssignParsedFloat { item, .. }
        | GenericLoopOp::ListAppend { item, .. }
        | GenericLoopOp::ListAppendUnique { item, .. }
        | GenericLoopOp::MapAddI64ConstRecordStringField { item, .. }
            if item != &plan.target =>
        {
            "loop_target_mismatch"
        }
        GenericLoopOp::MapAddI64Const { key, .. } if key != &plan.target => "loop_target_mismatch",
        _ => "unsupported_body_stmt",
    }
}

pub(crate) fn match_outer_jsonl_file_loop_body(
    file_target: &str,
    body: &[Stmt],
) -> Option<OuterJsonlFileLoopBody> {
    let [rows_stmt, row_loop] = body else {
        return None;
    };
    let Stmt::Assign {
        target: AssignTarget::Name(rows_name),
        value:
            Expr::Call(Call {
                name,
                positional,
                named,
            }),
    } = rows_stmt
    else {
        return None;
    };
    if name != "read_jsonl" || !named.is_empty() {
        return None;
    }
    let [path_expr] = positional.as_slice() else {
        return None;
    };
    if !matches_record_field_access(file_target, "path", path_expr) {
        return None;
    }
    let Stmt::For {
        targets,
        iter,
        body: row_body,
    } = row_loop
    else {
        return None;
    };
    let [row_target] = targets.as_slice() else {
        return None;
    };
    if iter != &Expr::Name(rows_name.clone()) {
        return None;
    }
    let body = match_generic_jsonl_aggregation_body(row_target, row_body)?;
    Some(OuterJsonlFileLoopBody {
        row_target: row_target.clone(),
        body,
    })
}

fn matches_record_field_access(record_name: &str, field_name: &str, expr: &Expr) -> bool {
    match expr {
        Expr::Attribute { value, attr } => {
            attr == field_name && value.as_ref() == &Expr::Name(record_name.to_owned())
        }
        Expr::Subscript { value, index } => {
            value.as_ref() == &Expr::Name(record_name.to_owned())
                && index.as_ref() == &Expr::String(field_name.to_owned())
        }
        _ => false,
    }
}

fn match_generic_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    if let Some(body) = match_hot_jsonl_aggregation_body(row_name, body) {
        return Some(body);
    }
    let Stmt::Assign {
        target: AssignTarget::Name(local),
        value: Expr::Subscript {
            value: receiver,
            index,
        },
    } = body.first()?
    else {
        return None;
    };
    if receiver.as_ref() != &Expr::Name(row_name.to_owned()) {
        return None;
    }
    let Expr::String(field) = index.as_ref() else {
        return None;
    };
    let body = match_hot_jsonl_aggregation_body(row_name, body.get(1..)?)?;
    if body.user_name == *local && body.user_key == *field {
        Some(body)
    } else {
        None
    }
}

fn lower_generic_loop_stmt(item: &str, stmt: &Stmt) -> Option<GenericLoopOp> {
    match stmt {
        Stmt::If {
            condition,
            then_branch,
            else_branch,
        } => lower_generic_unique_append_if(item, condition, then_branch, else_branch)
            .or_else(|| lower_generic_count_if(item, condition, then_branch, else_branch)),
        Stmt::Expr(Expr::MethodCall {
            receiver,
            method,
            positional,
        }) => lower_generic_list_append(item, receiver, method, positional),
        Stmt::AugAssign {
            target: AssignTarget::Name(local),
            op: AugOp::Add,
            value: Expr::Name(value),
        } if value == item => Some(GenericLoopOp::AddAssign {
            local: local.clone(),
            item: item.to_owned(),
        }),
        Stmt::AugAssign {
            target: AssignTarget::Name(local),
            op: AugOp::Add,
            value,
        } => lower_generic_parse_add_assign(local, item, value),
        Stmt::Assign {
            target: AssignTarget::Name(local),
            value: Expr::Add { left, right },
        } if matches_add_self_item(local, item, left, right) => Some(GenericLoopOp::AddAssign {
            local: local.clone(),
            item: item.to_owned(),
        }),
        _ => None,
    }
}

fn lower_generic_parse_add_assign(local: &str, item: &str, value: &Expr) -> Option<GenericLoopOp> {
    let Expr::Call(Call {
        name,
        positional,
        named,
    }) = value
    else {
        return None;
    };
    if !named.is_empty() || positional != &[Expr::Name(item.to_owned())] {
        return None;
    }
    match name.as_str() {
        "int" => Some(GenericLoopOp::AddAssignParsedInt {
            local: local.to_owned(),
            item: item.to_owned(),
        }),
        "float" => Some(GenericLoopOp::AddAssignParsedFloat {
            local: local.to_owned(),
            item: item.to_owned(),
        }),
        _ => None,
    }
}

fn lower_generic_record_field_count(item: &str, body: &[Stmt]) -> Option<GenericLoopOp> {
    let [assign, count_stmt] = body else {
        return None;
    };
    let Stmt::Assign {
        target: AssignTarget::Name(key_local),
        value,
    } = assign
    else {
        return None;
    };
    let field = match_record_string_field_expr(item, value)?;
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = count_stmt
    else {
        return None;
    };
    let update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if update.key_name != *key_local || update.append_list.is_some() {
        return None;
    }
    let [count] = update.updates.as_slice() else {
        return None;
    };
    if update.contains_map != count.map_name {
        return None;
    }
    let Expr::Int(value) = &count.addend else {
        return None;
    };
    let value = value.parse().ok()?;
    Some(GenericLoopOp::MapAddI64ConstRecordStringField {
        map: count.map_name.clone(),
        item: item.to_owned(),
        field: field.field,
        strip: field.strip,
        lower: field.lower,
        value,
    })
}

struct RecordStringFieldExpr {
    field: String,
    strip: bool,
    lower: bool,
}

fn match_record_string_field_expr(item: &str, value: &Expr) -> Option<RecordStringFieldExpr> {
    match value {
        Expr::Subscript {
            value: receiver,
            index,
        } if receiver.as_ref() == &Expr::Name(item.to_owned()) => {
            let Expr::String(field) = index.as_ref() else {
                return None;
            };
            Some(RecordStringFieldExpr {
                field: field.clone(),
                strip: false,
                lower: false,
            })
        }
        Expr::MethodCall {
            receiver,
            method,
            positional,
        } if positional.is_empty() && (method == "strip" || method == "lower") => {
            let mut field = match_record_string_field_expr(item, receiver)?;
            match method.as_str() {
                "strip" => field.strip = true,
                "lower" => field.lower = true,
                _ => return None,
            }
            Some(field)
        }
        _ => None,
    }
}

fn lower_generic_count_if(
    item: &str,
    condition: &Expr,
    then_branch: &[Stmt],
    else_branch: &[Stmt],
) -> Option<GenericLoopOp> {
    let update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if update.key_name != item || update.append_list.is_some() {
        return None;
    }
    let [count] = update.updates.as_slice() else {
        return None;
    };
    if update.contains_map != count.map_name {
        return None;
    }
    let Expr::Int(value) = &count.addend else {
        return None;
    };
    let value = value.parse().ok()?;
    Some(GenericLoopOp::MapAddI64Const {
        map: count.map_name.clone(),
        key: item.to_owned(),
        value,
    })
}

fn lower_generic_list_append(
    item: &str,
    receiver: &Expr,
    method: &str,
    positional: &[Expr],
) -> Option<GenericLoopOp> {
    if method != "append" || positional != &[Expr::Name(item.to_owned())] {
        return None;
    }
    let Expr::Name(list) = receiver else {
        return None;
    };
    Some(GenericLoopOp::ListAppend {
        list: list.clone(),
        item: item.to_owned(),
    })
}

fn lower_generic_unique_append_if(
    item: &str,
    condition: &Expr,
    then_branch: &[Stmt],
    else_branch: &[Stmt],
) -> Option<GenericLoopOp> {
    if !else_branch.is_empty() {
        return None;
    }
    let Expr::Not(condition) = condition else {
        return None;
    };
    let Expr::Compare {
        left,
        ops,
        comparators,
    } = condition.as_ref()
    else {
        return None;
    };
    let ([CompareOp::In], [Expr::Name(list)]) = (ops.as_slice(), comparators.as_slice()) else {
        return None;
    };
    if left.as_ref() != &Expr::Name(item.to_owned()) {
        return None;
    }
    let [stmt] = then_branch else {
        return None;
    };
    match lower_generic_loop_stmt(item, stmt)? {
        GenericLoopOp::ListAppend {
            list: append_list,
            item,
        } if append_list == *list => Some(GenericLoopOp::ListAppendUnique {
            list: append_list,
            item,
        }),
        _ => None,
    }
}

fn matches_add_self_item(local: &str, item: &str, left: &Expr, right: &Expr) -> bool {
    matches!((left, right), (Expr::Name(lhs), Expr::Name(rhs)) if lhs == local && rhs == item)
        || matches!((left, right), (Expr::Name(lhs), Expr::Name(rhs)) if lhs == item && rhs == local)
}

pub(crate) fn compile_generic_vm_function(plan: &GenericLoopPlan) -> Option<LoopIrFunction> {
    let [op] = plan.ops.as_slice() else {
        return None;
    };
    let mut locals = Vec::new();
    let mut registers = 0;
    let mut constants = Vec::new();
    let lowering_path = match op {
        GenericLoopOp::AddAssign { local, item } => {
            if item != &plan.target {
                return None;
            }
            let local = generic_vm_local(&mut locals, local);
            ("add_assign", GenericVmOp::AddAssign { local })
        }
        GenericLoopOp::AddAssignParsedInt { local, item } => {
            if item != &plan.target {
                return None;
            }
            let local = generic_vm_local(&mut locals, local);
            (
                "add_assign_parsed_int",
                GenericVmOp::AddAssignParsed {
                    local,
                    parse: GenericParseNumber::Int,
                },
            )
        }
        GenericLoopOp::AddAssignParsedFloat { local, item } => {
            if item != &plan.target {
                return None;
            }
            let local = generic_vm_local(&mut locals, local);
            (
                "add_assign_parsed_float",
                GenericVmOp::AddAssignParsed {
                    local,
                    parse: GenericParseNumber::Float,
                },
            )
        }
        GenericLoopOp::MapAddI64Const { map, key, value } => {
            if key != &plan.target {
                return None;
            }
            let map = generic_vm_local(&mut locals, map);
            (
                "map_add_i64_const",
                GenericVmOp::MapAddI64Const {
                    map,
                    addend: *value,
                },
            )
        }
        GenericLoopOp::MapAddI64ConstRecordStringField {
            map,
            item,
            field,
            strip,
            lower,
            value,
        } => {
            if item != &plan.target {
                return None;
            }
            let map = generic_vm_local(&mut locals, map);
            (
                "map_add_i64_const_record_string_field",
                GenericVmOp::MapAddI64ConstRecordStringField {
                    map,
                    field: field.clone(),
                    strip: *strip,
                    lower: *lower,
                    addend: *value,
                },
            )
        }
        GenericLoopOp::ListAppend { list, item } => {
            if item != &plan.target {
                return None;
            }
            let list = generic_vm_local(&mut locals, list);
            (
                "list_append",
                GenericVmOp::ListAppend {
                    list,
                    unique: false,
                },
            )
        }
        GenericLoopOp::ListAppendUnique { list, item } => {
            if item != &plan.target {
                return None;
            }
            let list = generic_vm_local(&mut locals, list);
            (
                "list_append_unique",
                GenericVmOp::ListAppend { list, unique: true },
            )
        }
        GenericLoopOp::JsonlAggregation { .. } => return None,
        GenericLoopOp::ExprBody { body } => {
            let body = compile_generic_vm_expr_body(&plan.target, body, &mut locals)?;
            registers = body.registers;
            constants = body.constants.clone();
            ("expr_body", GenericVmOp::ExprBody(body))
        }
    };
    let (lowering_path, op) = lowering_path;
    let ops = vec![op];
    let snapshots = vec![
        LoopIrSnapshot {
            locals: (0..locals.len()).collect(),
            boundary: LoopIrSnapshotBoundary::LoopEntry,
        },
        LoopIrSnapshot {
            locals: (0..locals.len()).collect(),
            boundary: LoopIrSnapshotBoundary::IterationEnd,
        },
    ];
    Some(LoopIrFunction {
        iter: plan.iter.clone(),
        adapter: loop_ir_iterator_adapter(plan),
        locals,
        registers,
        constants,
        blocks: vec![LoopIrBlock {
            ops: ops.clone(),
            terminator: LoopIrTerminator::Return,
        }],
        entry: 0,
        snapshots,
        diagnostics: LoopIrDiagnostics { lowering_path },
        ops,
    })
}

pub(crate) fn compile_hot_jsonl_loop_ir_function(
    plan: &GenericLoopPlan,
) -> Option<StoneIrFunction> {
    let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
        return None;
    };
    let mut function = compile_hot_jsonl_vm_function(body)?;
    function.adapter = Some(loop_ir_iterator_adapter(plan));
    Some(function)
}

pub(crate) fn compile_hot_jsonl_vm_function(
    body_plan: &HotJsonlAggregationBody,
) -> Option<StoneIrFunction> {
    let [HotJsonlBodyOp::JsonGetFields {
        user_key,
        amount_key,
        items_key,
        tags_key,
    }, HotJsonlBodyOp::MapAddF64 {
        map_name: user_amounts_map,
        key_slot: HotJsonlSlot::User,
        value_slot: HotJsonlSlot::Amount,
        append_list: users_list,
    }, HotJsonlBodyOp::MapAddI64 {
        map_name: user_items_map,
        key_slot: HotJsonlSlot::User,
        value_slot: HotJsonlSlot::Items,
    }, HotJsonlBodyOp::ForEachJsonString {
        array_slot: HotJsonlSlot::Tags,
        item_slot: HotJsonlSlot::Tag,
        body,
    }] = body_plan.ops.as_slice()
    else {
        return None;
    };

    let [HotJsonlBodyOp::MapAddI64Const {
        map_name: tag_counts_map,
        key_slot: HotJsonlSlot::Tag,
        value: 1,
        append_list: tags_list,
    }] = body.as_slice()
    else {
        return None;
    };

    if user_key != &body_plan.user_key
        || amount_key != &body_plan.user_amount_key
        || items_key != &body_plan.user_items_key
        || tags_key != &body_plan.tags_key
        || user_amounts_map != &body_plan.user_amounts_map
        || user_items_map != &body_plan.user_items_map
        || users_list.as_deref() != body_plan.users_list.as_deref()
        || tag_counts_map != &body_plan.tag_counts_map
        || tags_list.as_deref() != body_plan.tags_list.as_deref()
    {
        return None;
    }

    let row = Reg(0);
    let user = Reg(1);
    let amount = Reg(2);
    let items = Reg(3);
    let tags = Reg(4);
    let tag = Reg(5);

    let user_key_const = ConstId(0);
    let user_default_const = ConstId(1);
    let amount_key_const = ConstId(2);
    let items_key_const = ConstId(3);
    let tags_key_const = ConstId(4);
    let user_amounts_acc = AccId(0);
    let user_items_acc = AccId(1);
    let users_acc = AccId(2);
    let tag_counts_acc = AccId(3);
    let tags_acc = AccId(4);

    let row_block = BlockId(0);
    let tag_block = BlockId(1);
    let done_block = BlockId(2);
    let loop_snapshot = SnapshotId(0);

    Some(StoneIrFunction {
        adapter: None,
        registers: 6,
        constants: vec![
            StoneConst::String(user_key.clone()),
            StoneConst::String(body_plan.user_default.clone()),
            StoneConst::String(amount_key.clone()),
            StoneConst::String(items_key.clone()),
            StoneConst::String(tags_key.clone()),
            StoneConst::EmptyList,
        ],
        locals: vec![StoneLocal {
            name: body_plan.user_name.clone(),
        }],
        accumulators: vec![
            StoneAccumulatorSpec {
                name: user_amounts_map.clone(),
                kind: StoneAccumulatorKind::F64Map,
            },
            StoneAccumulatorSpec {
                name: user_items_map.clone(),
                kind: StoneAccumulatorKind::I64Map,
            },
            StoneAccumulatorSpec {
                name: users_list.clone().unwrap_or_default(),
                kind: StoneAccumulatorKind::StringList,
            },
            StoneAccumulatorSpec {
                name: tag_counts_map.clone(),
                kind: StoneAccumulatorKind::I64Map,
            },
            StoneAccumulatorSpec {
                name: tags_list.clone().unwrap_or_default(),
                kind: StoneAccumulatorKind::StringList,
            },
        ],
        guards: vec![
            StoneGuard {
                kind: StoneGuardKind::InputIsJsonObject { reg: row },
                snapshot: loop_snapshot,
            },
            StoneGuard {
                kind: StoneGuardKind::AccumulatorShape {
                    acc: user_amounts_acc,
                    kind: StoneAccumulatorKind::F64Map,
                },
                snapshot: loop_snapshot,
            },
            StoneGuard {
                kind: StoneGuardKind::AccumulatorShape {
                    acc: user_items_acc,
                    kind: StoneAccumulatorKind::I64Map,
                },
                snapshot: loop_snapshot,
            },
            StoneGuard {
                kind: StoneGuardKind::AccumulatorShape {
                    acc: users_acc,
                    kind: StoneAccumulatorKind::StringList,
                },
                snapshot: loop_snapshot,
            },
            StoneGuard {
                kind: StoneGuardKind::AccumulatorShape {
                    acc: tag_counts_acc,
                    kind: StoneAccumulatorKind::I64Map,
                },
                snapshot: loop_snapshot,
            },
            StoneGuard {
                kind: StoneGuardKind::AccumulatorShape {
                    acc: tags_acc,
                    kind: StoneAccumulatorKind::StringList,
                },
                snapshot: loop_snapshot,
            },
        ],
        snapshots: vec![StoneSnapshot {
            locals: vec![StoneSnapshotLocal {
                local: LocalId(0),
                reg: user,
            }],
            accumulators: vec![
                StoneSnapshotAccumulator {
                    local_name: user_amounts_map.clone(),
                    acc: user_amounts_acc,
                },
                StoneSnapshotAccumulator {
                    local_name: user_items_map.clone(),
                    acc: user_items_acc,
                },
                StoneSnapshotAccumulator {
                    local_name: users_list.clone().unwrap_or_default(),
                    acc: users_acc,
                },
                StoneSnapshotAccumulator {
                    local_name: tag_counts_map.clone(),
                    acc: tag_counts_acc,
                },
                StoneSnapshotAccumulator {
                    local_name: tags_list.clone().unwrap_or_default(),
                    acc: tags_acc,
                },
            ],
            resume: StoneFallbackTarget::LoopBody,
        }],
        blocks: vec![
            StoneBlock {
                ops: vec![
                    if body_plan.user_has_default {
                        StoneOp::JsonGetStrDefault {
                            dst: user,
                            object: row,
                            key: user_key_const,
                            default: user_default_const,
                        }
                    } else {
                        StoneOp::JsonGetValue {
                            dst: user,
                            object: row,
                            key: user_key_const,
                        }
                    },
                    if body_plan.user_amount_has_default {
                        StoneOp::JsonGetF64Default {
                            dst: amount,
                            object: row,
                            key: amount_key_const,
                            default: body_plan.user_amount_default,
                        }
                    } else {
                        StoneOp::JsonGetF64Required {
                            dst: amount,
                            object: row,
                            key: amount_key_const,
                        }
                    },
                    if body_plan.user_items_has_default {
                        StoneOp::JsonGetI64Default {
                            dst: items,
                            object: row,
                            key: items_key_const,
                            default: body_plan.user_items_default,
                        }
                    } else {
                        StoneOp::JsonGetI64Required {
                            dst: items,
                            object: row,
                            key: items_key_const,
                        }
                    },
                    if body_plan.tags_default_empty {
                        StoneOp::JsonGetArrayDefault {
                            dst: tags,
                            object: row,
                            key: tags_key_const,
                        }
                    } else {
                        StoneOp::JsonGetArrayRequired {
                            dst: tags,
                            object: row,
                            key: tags_key_const,
                        }
                    },
                    StoneOp::MapAddF64 {
                        map: user_amounts_acc,
                        key: user,
                        value: amount,
                        append: users_list.as_ref().map(|_| users_acc),
                    },
                    StoneOp::MapAddI64 {
                        map: user_items_acc,
                        key: user,
                        value: items,
                        append: None,
                    },
                ],
                terminator: StoneTerminator::JsonEachStrArray {
                    array: tags,
                    item: tag,
                    body: tag_block,
                    done: done_block,
                },
            },
            StoneBlock {
                ops: vec![StoneOp::MapAddI64Const {
                    map: tag_counts_acc,
                    key: tag,
                    value: 1,
                    append: tags_list.as_ref().map(|_| tags_acc),
                }],
                terminator: StoneTerminator::Jump { target: row_block },
            },
            StoneBlock {
                ops: Vec::new(),
                terminator: StoneTerminator::Return,
            },
        ],
        entry: row_block,
    })
}

pub(crate) fn compile_hot_jsonl_trace_plan(
    body_plan: &HotJsonlAggregationBody,
) -> Option<HotJsonlTracePlan> {
    let vm = compile_hot_jsonl_vm_function(body_plan)?;
    compile_hot_jsonl_trace_plan_from_ir(&vm)
}

pub(crate) fn compile_hot_jsonl_trace_plan_from_ir(
    function: &StoneIrFunction,
) -> Option<HotJsonlTracePlan> {
    let vm_trace = HotJsonlVmTrace::from_function(function)?;

    Some(HotJsonlTracePlan {
        user_name: vm_trace.user_name,
        user_key: vm_trace.user_key,
        user_has_default: vm_trace.user_has_default,
        user_default: vm_trace.user_default,
        user_amounts_map: vm_trace.user_amounts_map,
        user_amount_key: vm_trace.user_amount_key,
        user_amount_has_default: vm_trace.user_amount_has_default,
        user_amount_default: vm_trace.user_amount_default,
        user_items_map: vm_trace.user_items_map,
        user_items_key: vm_trace.user_items_key,
        user_items_has_default: vm_trace.user_items_has_default,
        user_items_default: vm_trace.user_items_default,
        users_list: vm_trace.users_list,
        tags_key: vm_trace.tags_key,
        tags_default_empty: vm_trace.tags_default_empty,
        tag_counts_map: vm_trace.tag_counts_map,
        tags_list: vm_trace.tags_list,
    })
}

pub(crate) fn validate_hot_jsonl_native_prefix(
    prefix_plan: &HotLoopPlan,
    body_plan: &HotJsonlAggregationBody,
) -> bool {
    match prefix_plan.ops.as_slice() {
        [HotLoopOp::JsonGetValue { target, key }]
            if target == &body_plan.user_name && key == &body_plan.user_key =>
        {
            true
        }
        [HotLoopOp::JsonGetStrDefault {
            target: user_target,
            key: user_key,
            default: user_default,
        }, HotLoopOp::JsonGetF64Default {
            target: amount_target,
            key: amount_key,
            default: amount_default,
        }, HotLoopOp::JsonGetI64Default {
            target: items_target,
            key: items_key,
            default: items_default,
        }, HotLoopOp::JsonGetArrayDefault { key: tags_key, .. }]
            if user_target == &body_plan.user_name
                && user_key == &body_plan.user_key
                && user_default == &body_plan.user_default
                && amount_target != user_target
                && amount_key == &body_plan.user_amount_key
                && *amount_default == body_plan.user_amount_default
                && items_target != user_target
                && items_key == &body_plan.user_items_key
                && *items_default == body_plan.user_items_default
                && tags_key == &body_plan.tags_key =>
        {
            true
        }
        [HotLoopOp::JsonGetStrDefault {
            target: user_target,
            key: user_key,
            default: user_default,
        }, HotLoopOp::JsonGetF64Default {
            target: amount_target,
            key: amount_key,
            default: amount_default,
        }, HotLoopOp::JsonGetI64Default {
            target: items_target,
            key: items_key,
            default: items_default,
        }] if user_target == &body_plan.user_name
            && user_key == &body_plan.user_key
            && user_default == &body_plan.user_default
            && amount_target != user_target
            && amount_key == &body_plan.user_amount_key
            && *amount_default == body_plan.user_amount_default
            && items_target != user_target
            && items_key == &body_plan.user_items_key
            && *items_default == body_plan.user_items_default
            && body_plan.tags_default_empty =>
        {
            true
        }
        _ => false,
    }
}

struct HotJsonlVmTrace {
    user_name: String,
    user_key: String,
    user_has_default: bool,
    user_default: String,
    user_amounts_map: String,
    user_amount_key: String,
    user_amount_has_default: bool,
    user_amount_default: f64,
    user_items_map: String,
    user_items_key: String,
    user_items_has_default: bool,
    user_items_default: i64,
    users_list: Option<String>,
    tags_key: String,
    tags_default_empty: bool,
    tag_counts_map: String,
    tags_list: Option<String>,
}

impl HotJsonlVmTrace {
    fn from_function(function: &StoneIrFunction) -> Option<Self> {
        let [StoneConst::String(user_key), StoneConst::String(user_default), StoneConst::String(amount_key), StoneConst::String(items_key), StoneConst::String(tags_key), StoneConst::EmptyList] =
            function.constants.as_slice()
        else {
            return None;
        };
        let [StoneLocal { name: user_name }] = function.locals.as_slice() else {
            return None;
        };
        let [StoneAccumulatorSpec {
            name: user_amounts_map,
            kind: StoneAccumulatorKind::F64Map,
        }, StoneAccumulatorSpec {
            name: user_items_map,
            kind: StoneAccumulatorKind::I64Map,
        }, StoneAccumulatorSpec {
            name: users_list,
            kind: StoneAccumulatorKind::StringList,
        }, StoneAccumulatorSpec {
            name: tag_counts_map,
            kind: StoneAccumulatorKind::I64Map,
        }, StoneAccumulatorSpec {
            name: tags_list,
            kind: StoneAccumulatorKind::StringList,
        }] = function.accumulators.as_slice()
        else {
            return None;
        };
        let subgraph = match_hot_jsonl_aggregation_ir_subgraph(function)?;

        Some(Self {
            user_name: user_name.clone(),
            user_key: user_key.clone(),
            user_has_default: subgraph.user_has_default,
            user_default: user_default.clone(),
            user_amounts_map: user_amounts_map.clone(),
            user_amount_key: amount_key.clone(),
            user_amount_has_default: subgraph.user_amount_has_default,
            user_amount_default: subgraph.user_amount_default,
            user_items_map: user_items_map.clone(),
            user_items_key: items_key.clone(),
            user_items_has_default: subgraph.user_items_has_default,
            user_items_default: subgraph.user_items_default,
            users_list: subgraph.users_append.map(|_| users_list.clone()),
            tags_key: tags_key.clone(),
            tags_default_empty: subgraph.tags_default_empty,
            tag_counts_map: tag_counts_map.clone(),
            tags_list: subgraph.tags_append.map(|_| tags_list.clone()),
        })
    }
}

pub(crate) fn loop_ir_iterator_adapter(plan: &GenericLoopPlan) -> LoopIrIteratorAdapter {
    match (&plan.iter, plan.ops.as_slice()) {
        (GenericLoopIter::OpenSplitlines, [GenericLoopOp::JsonlAggregation { .. }]) => {
            LoopIrIteratorAdapter::JsonlRows { guarded: true }
        }
        (GenericLoopIter::ReadJsonl, _) => LoopIrIteratorAdapter::JsonlRows { guarded: false },
        (GenericLoopIter::ReadCsv, _) => LoopIrIteratorAdapter::CsvRows,
        (GenericLoopIter::OpenSplitlines, _) => LoopIrIteratorAdapter::TextLines,
        (GenericLoopIter::Range, _) => LoopIrIteratorAdapter::RangeValues,
        (GenericLoopIter::MaterializedList, _) => LoopIrIteratorAdapter::MaterializedValues,
    }
}

struct GenericVmExprBuilder<'a> {
    locals: &'a mut Vec<String>,
    constants: Vec<GenericVmConst>,
    ops: Vec<GenericVmExprOp>,
    next_register: usize,
}

impl<'a> GenericVmExprBuilder<'a> {
    fn new(locals: &'a mut Vec<String>) -> Self {
        Self {
            locals,
            constants: Vec::new(),
            ops: Vec::new(),
            next_register: 0,
        }
    }

    fn finish(self) -> GenericVmExprBody {
        GenericVmExprBody {
            registers: self.next_register,
            constants: self.constants,
            ops: self.ops,
        }
    }

    fn reg(&mut self) -> usize {
        let reg = self.next_register;
        self.next_register += 1;
        reg
    }

    fn local(&mut self, name: &str) -> usize {
        generic_vm_local(self.locals, name)
    }

    fn constant(&mut self, constant: GenericVmConst) -> usize {
        if let Some(index) = self.constants.iter().position(|item| item == &constant) {
            return index;
        }
        let index = self.constants.len();
        self.constants.push(constant);
        index
    }

    fn lower_stmt(&mut self, stmt: &Stmt) -> Option<()> {
        match stmt {
            Stmt::Assign {
                target: AssignTarget::Name(local),
                value,
            } => {
                let src = self.lower_expr(value)?;
                let local = self.local(local);
                self.ops.push(GenericVmExprOp::StoreLocal { local, src });
                Some(())
            }
            Stmt::AugAssign {
                target: AssignTarget::Name(local),
                op: AugOp::Add,
                value,
            } => {
                let lhs = self.reg();
                let local_id = self.local(local);
                self.ops.push(GenericVmExprOp::LoadLocal {
                    dst: lhs,
                    local: local_id,
                });
                let rhs = self.lower_expr(value)?;
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::AddI64 { dst, lhs, rhs });
                self.ops.push(GenericVmExprOp::StoreLocal {
                    local: local_id,
                    src: dst,
                });
                Some(())
            }
            _ => None,
        }
    }

    fn lower_expr(&mut self, expr: &Expr) -> Option<usize> {
        match expr {
            Expr::Int(value) => {
                let value = value.parse().ok()?;
                let constant = self.constant(GenericVmConst::I64(value));
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::LoadConst { dst, constant });
                Some(dst)
            }
            Expr::Name(name) => {
                let local = self.local(name);
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::LoadLocal { dst, local });
                Some(dst)
            }
            Expr::Invert(value) => {
                let src = self.lower_expr(value)?;
                let dst = self.reg();
                self.ops.push(GenericVmExprOp::BitNotI64 { dst, src });
                Some(dst)
            }
            Expr::Add { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::AddI64 { dst, lhs, rhs }
            }),
            Expr::Sub { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::SubI64 { dst, lhs, rhs }
            }),
            Expr::Mul { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::MulI64 { dst, lhs, rhs }
            }),
            Expr::FloorDiv { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::FloorDivI64 { dst, lhs, rhs }
            }),
            Expr::BitAnd { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::BitAndI64 { dst, lhs, rhs }
            }),
            Expr::BitOr { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::BitOrI64 { dst, lhs, rhs }
            }),
            Expr::BitXor { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::BitXorI64 { dst, lhs, rhs }
            }),
            Expr::LShift { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::ShlI64 { dst, lhs, rhs }
            }),
            Expr::RShift { left, right } => self.lower_binary(left, right, |dst, lhs, rhs| {
                GenericVmExprOp::ShrI64 { dst, lhs, rhs }
            }),
            _ => None,
        }
    }

    fn lower_binary(
        &mut self,
        left: &Expr,
        right: &Expr,
        op: fn(usize, usize, usize) -> GenericVmExprOp,
    ) -> Option<usize> {
        let lhs = self.lower_expr(left)?;
        let rhs = self.lower_expr(right)?;
        let dst = self.reg();
        self.ops.push(op(dst, lhs, rhs));
        Some(dst)
    }
}

fn compile_generic_vm_expr_body(
    loop_target: &str,
    body: &[Stmt],
    locals: &mut Vec<String>,
) -> Option<GenericVmExprBody> {
    let mut builder = GenericVmExprBuilder::new(locals);
    builder.local(loop_target);
    for stmt in body {
        builder.lower_stmt(stmt)?;
    }
    Some(builder.finish())
}

fn generic_vm_local(locals: &mut Vec<String>, name: &str) -> usize {
    if let Some(index) = locals.iter().position(|local| local == name) {
        return index;
    }
    let index = locals.len();
    locals.push(name.to_owned());
    index
}

pub(crate) fn lower_json_loads_line_prefix(
    line_target: &str,
    body: &[Stmt],
) -> Option<(String, usize)> {
    let mut index = 0;
    if matches_blank_line_continue(line_target, body.first()?) {
        index = 1;
    }
    let Stmt::Assign {
        target: AssignTarget::Name(record_target),
        value:
            Expr::Call(Call {
                name,
                positional,
                named,
            }),
    } = body.get(index)?
    else {
        return None;
    };
    if name != "json_loads"
        || !named.is_empty()
        || positional != &[Expr::Name(line_target.to_owned())]
    {
        return None;
    }
    Some((record_target.clone(), index + 1))
}

fn matches_blank_line_continue(line_target: &str, stmt: &Stmt) -> bool {
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = stmt
    else {
        return false;
    };
    if !else_branch.is_empty() || then_branch != &[Stmt::Continue] {
        return false;
    }
    match condition {
        Expr::Compare {
            left,
            ops,
            comparators,
        } if ops == &[CompareOp::Eq] && comparators == &[Expr::String(String::new())] => {
            matches_line_strip_call(line_target, left)
        }
        Expr::Not(value) => matches_line_strip_call(line_target, value),
        _ => false,
    }
}

fn matches_line_strip_call(line_target: &str, expr: &Expr) -> bool {
    let Expr::MethodCall {
        receiver,
        method,
        positional,
    } = expr
    else {
        return false;
    };
    method == "strip"
        && positional.is_empty()
        && receiver.as_ref() == &Expr::Name(line_target.to_owned())
}

fn lower_prefix_ops(row_target: &str, body: &[Stmt]) -> Vec<HotLoopOp> {
    let mut ops = Vec::new();
    for stmt in body {
        let Some(op) = lower_prefix_stmt(row_target, stmt) else {
            break;
        };
        ops.push(op);
    }
    ops
}

fn lower_prefix_stmt(row_target: &str, stmt: &Stmt) -> Option<HotLoopOp> {
    let Stmt::Assign {
        target: AssignTarget::Name(target),
        value,
    } = stmt
    else {
        return None;
    };
    lower_get_assignment(row_target, target, value)
}

fn lower_get_assignment(row_target: &str, target: &str, value: &Expr) -> Option<HotLoopOp> {
    if let Some(key) = match_row_subscript(row_target, value) {
        return Some(HotLoopOp::JsonGetValue {
            target: target.to_owned(),
            key,
        });
    }

    if let Some((key, default)) = match_row_get(row_target, value) {
        return match default {
            Expr::String(default) => Some(HotLoopOp::JsonGetStrDefault {
                target: target.to_owned(),
                key,
                default: default.clone(),
            }),
            Expr::List(items) if items.is_empty() => Some(HotLoopOp::JsonGetArrayDefault {
                target: target.to_owned(),
                key,
            }),
            _ => None,
        };
    }

    let Expr::Call(Call {
        name,
        positional,
        named,
    }) = value
    else {
        return None;
    };
    if !named.is_empty() {
        return None;
    }
    let [arg] = positional.as_slice() else {
        return None;
    };
    let (key, default) = match_row_get(row_target, arg)?;
    match (name.as_str(), default) {
        ("float", Expr::Float(default)) => Some(HotLoopOp::JsonGetF64Default {
            target: target.to_owned(),
            key,
            default: *default,
        }),
        ("int", Expr::Int(default)) => Some(HotLoopOp::JsonGetI64Default {
            target: target.to_owned(),
            key,
            default: default.parse().ok()?,
        }),
        _ => None,
    }
}

pub(crate) fn match_row_get<'a>(row_target: &str, value: &'a Expr) -> Option<(String, &'a Expr)> {
    let Expr::MethodCall {
        receiver,
        method,
        positional,
    } = value
    else {
        return None;
    };
    if method != "get" {
        return None;
    }
    let Expr::Name(receiver) = receiver.as_ref() else {
        return None;
    };
    if receiver != row_target {
        return None;
    }
    let [Expr::String(key), default] = positional.as_slice() else {
        return None;
    };
    Some((key.clone(), default))
}

pub(crate) fn match_row_subscript(row_target: &str, value: &Expr) -> Option<String> {
    match value {
        Expr::Subscript { value, index } => {
            let Expr::Name(receiver) = value.as_ref() else {
                return None;
            };
            if receiver != row_target {
                return None;
            }
            let Expr::String(key) = index.as_ref() else {
                return None;
            };
            Some(key.clone())
        }
        Expr::Attribute { value, attr } => {
            let Expr::Name(receiver) = value.as_ref() else {
                return None;
            };
            if receiver != row_target {
                return None;
            }
            Some(attr.clone())
        }
        _ => None,
    }
}

pub(crate) fn match_fused_map_update_if(
    condition: &Expr,
    then_branch: &[Stmt],
    else_branch: &[Stmt],
) -> Option<FusedMapUpdateIf> {
    let (key_name, contains_map) = match_key_in_map(condition)?;
    if then_branch.is_empty() || else_branch.len() < then_branch.len() {
        return None;
    }
    let mut updates = Vec::with_capacity(then_branch.len());
    for (then_stmt, else_stmt) in then_branch.iter().zip(else_branch.iter()) {
        let (map_name, then_key, addend) = match_increment_assignment(then_stmt)?;
        if then_key != key_name {
            return None;
        }
        let (else_map, else_key, else_value) = match_insert_assignment(else_stmt)?;
        if else_map != map_name || else_key != key_name || else_value != addend {
            return None;
        }
        updates.push(FusedMapUpdate { map_name, addend });
    }
    let append_tail = &else_branch[then_branch.len()..];
    let append_list = match append_tail {
        [] => None,
        [stmt] => Some(match_append_key(stmt, &key_name)?),
        _ => return None,
    };
    Some(FusedMapUpdateIf {
        key_name,
        contains_map,
        updates,
        append_list,
    })
}

pub(crate) fn match_key_not_in_map(condition: &Expr) -> Option<(String, String)> {
    if let Expr::Not(condition) = condition {
        return match_key_in_map(condition);
    }
    let Expr::Compare {
        left,
        ops,
        comparators,
    } = condition
    else {
        return None;
    };
    let ([CompareOp::NotIn], [Expr::Name(map_name)]) = (ops.as_slice(), comparators.as_slice())
    else {
        return None;
    };
    let Expr::Name(key_name) = left.as_ref() else {
        return None;
    };
    Some((key_name.clone(), map_name.clone()))
}

fn match_key_in_map(condition: &Expr) -> Option<(String, String)> {
    let Expr::Compare {
        left,
        ops,
        comparators,
    } = condition
    else {
        return None;
    };
    let ([CompareOp::In], [Expr::Name(map_name)]) = (ops.as_slice(), comparators.as_slice()) else {
        return None;
    };
    let Expr::Name(key_name) = left.as_ref() else {
        return None;
    };
    Some((key_name.clone(), map_name.clone()))
}

fn match_increment_assignment(stmt: &Stmt) -> Option<(String, String, Expr)> {
    match stmt {
        Stmt::Assign { target, value } => {
            let (target_map, target_key) = match_map_key_target(target)?;
            let Expr::Add { left, right } = value else {
                return None;
            };
            let (left_map, left_key) = match_map_key_expr(left)?;
            if left_map != target_map || left_key != target_key {
                return None;
            }
            Some((target_map, target_key, right.as_ref().clone()))
        }
        Stmt::AugAssign {
            target,
            op: AugOp::Add,
            value,
        } => {
            let (target_map, target_key) = match_map_key_target(target)?;
            Some((target_map, target_key, value.clone()))
        }
        _ => None,
    }
}

pub(crate) fn match_insert_assignment(stmt: &Stmt) -> Option<(String, String, Expr)> {
    let Stmt::Assign { target, value } = stmt else {
        return None;
    };
    let (map_name, key_name) = match_map_key_target(target)?;
    Some((map_name, key_name, value.clone()))
}

pub(crate) fn match_map_key_target(target: &AssignTarget) -> Option<(String, String)> {
    let AssignTarget::Subscript { value, index } = target else {
        return None;
    };
    let AssignTarget::Name(map_name) = value.as_ref() else {
        return None;
    };
    let Expr::Name(key_name) = index else {
        return None;
    };
    Some((map_name.clone(), key_name.clone()))
}

fn match_map_key_expr(value: &Expr) -> Option<(String, String)> {
    let Expr::Subscript { value, index } = value else {
        return None;
    };
    let Expr::Name(map_name) = value.as_ref() else {
        return None;
    };
    let Expr::Name(key_name) = index.as_ref() else {
        return None;
    };
    Some((map_name.clone(), key_name.clone()))
}

fn match_append_key(stmt: &Stmt, key_name: &str) -> Option<String> {
    let Stmt::Expr(Expr::MethodCall {
        receiver,
        method,
        positional,
    }) = stmt
    else {
        return None;
    };
    if method != "append" {
        return None;
    }
    let Expr::Name(list_name) = receiver.as_ref() else {
        return None;
    };
    let [Expr::Name(arg_name)] = positional.as_slice() else {
        return None;
    };
    if arg_name != key_name {
        return None;
    }
    Some(list_name.clone())
}
