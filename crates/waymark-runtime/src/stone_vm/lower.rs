// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{
    AccId, BlockId, ConstId, GenericLoopIter, GenericLoopOp, GenericLoopPlan, GenericParseNumber,
    GenericVmConst, GenericVmExprBody, GenericVmExprOp, GenericVmOp, HotJsonlAggregationBody,
    HotJsonlBodyOp, HotJsonlSlot, HotJsonlTracePlan, LocalId, LoopIrBlock, LoopIrDiagnostics,
    LoopIrFunction, LoopIrIteratorAdapter, LoopIrSnapshot, LoopIrSnapshotBoundary,
    LoopIrTerminator, Reg, SnapshotId, StoneAccumulatorKind, StoneAccumulatorSpec, StoneBlock,
    StoneConst, StoneFallbackTarget, StoneGuard, StoneGuardKind, StoneIrFunction, StoneLocal,
    StoneOp, StoneSnapshot, StoneSnapshotAccumulator, StoneSnapshotLocal, StoneTerminator,
};

use crate::stone_ast::{AssignTarget, AugOp, Expr, Stmt};
use crate::stone_vm::match_hot_jsonl_aggregation_ir_subgraph;

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
