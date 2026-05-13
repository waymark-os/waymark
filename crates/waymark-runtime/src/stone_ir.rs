// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::stone_ast::{AssignTarget, AugOp, Call, CompareOp, Expr, Stmt};
use crate::stone_vm::{
    AccId, BlockId, ConstId, GenericParseNumber, GenericVmConst, GenericVmExprBody,
    GenericVmExprOp, GenericVmOp, HotJsonlAggregationBody, HotJsonlBodyOp,
    HotJsonlNestedUserTotals, HotJsonlSlot, HotJsonlTracePlan, LocalId, LoopIrBlock,
    LoopIrDiagnostics, LoopIrFunction, LoopIrFusedKernel, LoopIrIteratorAdapter,
    LoopIrOptimizationDiagnostic, LoopIrOptimizationResult, LoopIrSnapshot, LoopIrSnapshotBoundary,
    LoopIrSubgraphKind, LoopIrTerminator, Reg, SnapshotId, StoneAccumulatorKind,
    StoneAccumulatorSpec, StoneBlock, StoneConst, StoneFallbackTarget, StoneGuard, StoneGuardKind,
    StoneIrFunction, StoneLocal, StoneLoopIrOptimizationResult, StoneOp, StoneSnapshot,
    StoneSnapshotAccumulator, StoneSnapshotLocal, StoneTerminator,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HotLoopPlan {
    pub(crate) target: String,
    pub(crate) iter: HotLoopIter,
    pub(crate) ops: Vec<HotLoopOp>,
    pub(crate) body_start: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GenericLoopPlan {
    pub(crate) target: String,
    pub(crate) iter: GenericLoopIter,
    pub(crate) ops: Vec<GenericLoopOp>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OuterJsonlFileLoopBody {
    pub(crate) row_target: String,
    pub(crate) body: HotJsonlAggregationBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenericLoopIter {
    MaterializedList,
    OpenSplitlines,
    Range,
    ReadJsonl,
    ReadCsv,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GenericLoopOp {
    AddAssign {
        local: String,
        item: String,
    },
    AddAssignParsedInt {
        local: String,
        item: String,
    },
    AddAssignParsedFloat {
        local: String,
        item: String,
    },
    MapAddI64Const {
        map: String,
        key: String,
        value: i64,
    },
    MapAddI64ConstRecordStringField {
        map: String,
        item: String,
        field: String,
        strip: bool,
        lower: bool,
        value: i64,
    },
    ListAppend {
        list: String,
        item: String,
    },
    ListAppendUnique {
        list: String,
        item: String,
    },
    JsonlAggregation {
        body: HotJsonlAggregationBody,
    },
    ExprBody {
        body: Vec<Stmt>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HotLoopIter {
    ReadJsonl { path: JsonlPathExpr },
    JsonlTextLines { line_target: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JsonlPathExpr {
    Dynamic,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HotLoopOp {
    #[allow(dead_code)]
    GenericFallback,
    JsonGetStrDefault {
        target: String,
        key: String,
        default: String,
    },
    JsonGetF64Default {
        target: String,
        key: String,
        default: f64,
    },
    JsonGetI64Default {
        target: String,
        key: String,
        default: i64,
    },
    JsonGetArrayDefault {
        target: String,
        key: String,
    },
    JsonGetValue {
        target: String,
        key: String,
    },
}

struct FusedMapUpdateIf {
    key_name: String,
    contains_map: String,
    updates: Vec<FusedMapUpdate>,
    append_list: Option<String>,
}

struct FusedMapUpdate {
    map_name: String,
    addend: Expr,
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
                iter: HotLoopIter::ReadJsonl {
                    path: JsonlPathExpr::Dynamic,
                },
                ops,
                body_start,
            });
        }
    }

    let (record_target, body_start) = lower_json_loads_line_prefix(target, body)?;
    Some(HotLoopPlan {
        target: record_target,
        iter: HotLoopIter::JsonlTextLines {
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

pub(crate) fn optimize_loop_ir(function: &LoopIrFunction) -> LoopIrOptimizationResult {
    let (function, diagnostics) = canonicalize_loop_ir(function);
    let matched_subgraph = match_loop_ir_subgraph(&function);
    let selected_kernel = matched_subgraph.map(|subgraph| match subgraph {
        LoopIrSubgraphKind::MapAddI64Const => LoopIrFusedKernel::MapAddI64Const,
        LoopIrSubgraphKind::ListAppend => LoopIrFusedKernel::ListAppend,
        LoopIrSubgraphKind::JsonlAggregation => LoopIrFusedKernel::JsonlAggregation,
    });
    LoopIrOptimizationResult {
        function,
        selected_kernel,
        matched_subgraph,
        diagnostics,
    }
}

#[allow(dead_code)]
pub(crate) fn select_loop_ir_fused_kernel(function: &LoopIrFunction) -> Option<LoopIrFusedKernel> {
    optimize_loop_ir(function).selected_kernel
}

fn canonicalize_loop_ir(
    function: &LoopIrFunction,
) -> (LoopIrFunction, Vec<LoopIrOptimizationDiagnostic>) {
    let mut function = function.clone();
    let mut diagnostics = Vec::new();
    let Some(canonical_ops) = canonicalize_loop_ir_ops(&function.ops) else {
        return (function, diagnostics);
    };
    if canonical_ops == function.ops {
        return (function, diagnostics);
    }
    let old_ops = std::mem::replace(&mut function.ops, canonical_ops.clone());
    for block in &mut function.blocks {
        if block.ops == old_ops {
            block.ops = canonical_ops.clone();
        }
    }
    diagnostics.push(LoopIrOptimizationDiagnostic::Canonicalized);
    (function, diagnostics)
}

fn canonicalize_loop_ir_ops(ops: &[GenericVmOp]) -> Option<Vec<GenericVmOp>> {
    let [GenericVmOp::MapAddI64ConstRecordStringField {
        map,
        field,
        strip: false,
        lower: false,
        addend,
    }] = ops
    else {
        return None;
    };
    Some(vec![GenericVmOp::MapAddI64ConstRecordField {
        map: *map,
        field: field.clone(),
        addend: *addend,
    }])
}

pub(crate) fn match_loop_ir_subgraph(function: &LoopIrFunction) -> Option<LoopIrSubgraphKind> {
    let block = function.blocks.get(function.entry)?;
    if block.terminator != LoopIrTerminator::Return || block.ops != function.ops {
        return None;
    }
    match block.ops.as_slice() {
        [GenericVmOp::MapAddI64Const { .. }]
        | [GenericVmOp::MapAddI64ConstRecordField { .. }]
        | [GenericVmOp::MapAddI64ConstRecordStringField { .. }] => {
            Some(LoopIrSubgraphKind::MapAddI64Const)
        }
        [GenericVmOp::ListAppend { .. }] => Some(LoopIrSubgraphKind::ListAppend),
        _ => None,
    }
}

#[allow(dead_code)]
pub(crate) fn select_hot_jsonl_fused_kernel_from_ir(
    function: &StoneIrFunction,
) -> Option<LoopIrFusedKernel> {
    optimize_stone_loop_ir(function).selected_kernel
}

pub(crate) fn optimize_stone_loop_ir(function: &StoneIrFunction) -> StoneLoopIrOptimizationResult {
    let function = function.clone();
    let matched_subgraph = match_hot_jsonl_ir_subgraph(&function);
    let selected_kernel = matched_subgraph.and_then(|subgraph| match subgraph {
        LoopIrSubgraphKind::JsonlAggregation => Some(LoopIrFusedKernel::JsonlAggregation),
        LoopIrSubgraphKind::MapAddI64Const | LoopIrSubgraphKind::ListAppend => None,
    });
    StoneLoopIrOptimizationResult {
        function,
        selected_kernel,
        matched_subgraph,
        diagnostics: Vec::new(),
    }
}

pub(crate) fn match_hot_jsonl_ir_subgraph(
    function: &StoneIrFunction,
) -> Option<LoopIrSubgraphKind> {
    match_hot_jsonl_aggregation_ir_subgraph(function).map(|_| LoopIrSubgraphKind::JsonlAggregation)
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

fn lower_json_loads_line_prefix(line_target: &str, body: &[Stmt]) -> Option<(String, usize)> {
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

fn match_row_get<'a>(row_target: &str, value: &'a Expr) -> Option<(String, &'a Expr)> {
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

fn match_row_subscript(row_target: &str, value: &Expr) -> Option<String> {
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

pub(crate) fn match_hot_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    if let Some(plan) = match_nested_totals_hot_jsonl_aggregation_body(row_name, body) {
        return Some(plan);
    }
    if let Some(plan) = match_init_then_add_hot_jsonl_aggregation_body(row_name, body) {
        return Some(plan);
    }
    if let Some(plan) = match_required_prefixed_hot_jsonl_aggregation_body(row_name, body) {
        return Some(plan);
    }
    if let Some(plan) = match_direct_hot_jsonl_aggregation_body(row_name, body) {
        return Some(plan);
    }
    if let Some(plan) = match_prefixed_hot_jsonl_aggregation_body(row_name, body) {
        return Some(plan);
    }
    match_prefixed_count_hot_jsonl_aggregation_body(row_name, body)
}

fn match_direct_hot_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    let [user_update, tag_loop] = body else {
        return None;
    };
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = user_update
    else {
        return None;
    };
    let user_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    let [first_update, second_update] = user_update.updates.as_slice() else {
        return None;
    };
    let user_key = user_update.key_name.clone();
    let first_key = match_row_subscript(row_name, &first_update.addend)?;
    let second_key = match_row_subscript(row_name, &second_update.addend)?;

    let Stmt::For {
        targets,
        iter,
        body: tag_body,
    } = tag_loop
    else {
        return None;
    };
    let [tag_name] = targets.as_slice() else {
        return None;
    };
    let tags_key = match_row_subscript(row_name, iter)?;
    let (tag_name, tag_update_stmt) = match_tag_update_body(tag_name, tag_body)?;
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = tag_update_stmt
    else {
        return None;
    };
    let tag_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if tag_update.key_name != tag_name {
        return None;
    }
    let [tag_count_update] = tag_update.updates.as_slice() else {
        return None;
    };
    if !matches!(tag_count_update.addend, Expr::Int(ref value) if value == "1") {
        return None;
    }

    Some(HotJsonlAggregationBody {
        ops: hot_jsonl_aggregation_ops(
            &user_key,
            &first_key,
            &second_key,
            &tags_key,
            &first_update.map_name,
            &second_update.map_name,
            user_update.append_list.as_deref(),
            &tag_update.contains_map,
            tag_update.append_list.as_deref(),
        ),
        nested_user_totals: None,
        user_name: user_update.key_name,
        user_key,
        user_has_default: false,
        user_default: String::new(),
        user_amounts_map: first_update.map_name.clone(),
        user_amount_key: first_key,
        user_amount_has_default: false,
        user_amount_default: 0.0,
        user_items_map: second_update.map_name.clone(),
        user_items_key: second_key,
        user_items_has_default: false,
        user_items_default: 0,
        users_list: user_update.append_list,
        tags_key,
        tags_default_empty: false,
        tag_counts_map: tag_update.contains_map,
        tags_list: tag_update.append_list,
        row_count_local: None,
    })
}

fn match_nested_totals_hot_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    let [user_stmt, init_stmt, amount_stmt, items_stmt, tag_loop] = body else {
        return None;
    };
    let (user_name, user_key) = match_required_string_prefix(row_name, user_stmt)?;
    let (totals_map, amount_field, items_field) = match_nested_totals_init(&user_name, init_stmt)?;
    let amount_key = match_nested_total_add(
        row_name,
        &totals_map,
        &user_name,
        amount_stmt,
        &amount_field,
        "float",
    )?;
    let items_key = match_nested_total_add(
        row_name,
        &totals_map,
        &user_name,
        items_stmt,
        &items_field,
        "int",
    )?;

    let Stmt::For {
        targets,
        iter,
        body: tag_body,
    } = tag_loop
    else {
        return None;
    };
    let [tag_name] = targets.as_slice() else {
        return None;
    };
    let tags_key = match_row_subscript(row_name, iter)?;
    let (tag_name, tag_update_stmt) = match_tag_update_body(tag_name, tag_body)?;
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = tag_update_stmt
    else {
        return None;
    };
    let tag_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if tag_update.key_name != tag_name {
        return None;
    }
    let [tag_count_update] = tag_update.updates.as_slice() else {
        return None;
    };
    if !matches!(tag_count_update.addend, Expr::Int(ref value) if value == "1") {
        return None;
    }

    Some(HotJsonlAggregationBody {
        ops: hot_jsonl_aggregation_ops(
            &user_key,
            &amount_key,
            &items_key,
            &tags_key,
            &totals_map,
            &totals_map,
            None,
            &tag_update.contains_map,
            tag_update.append_list.as_deref(),
        ),
        nested_user_totals: Some(HotJsonlNestedUserTotals {
            map_name: totals_map,
            amount_field,
            items_field,
        }),
        user_name,
        user_key,
        user_has_default: false,
        user_default: String::new(),
        user_amounts_map: String::new(),
        user_amount_key: amount_key,
        user_amount_has_default: false,
        user_amount_default: 0.0,
        user_items_map: String::new(),
        user_items_key: items_key,
        user_items_has_default: false,
        user_items_default: 0,
        users_list: None,
        tags_key,
        tags_default_empty: false,
        tag_counts_map: tag_update.contains_map,
        tags_list: tag_update.append_list,
        row_count_local: None,
    })
}

fn match_required_prefixed_hot_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    let (user_stmt, amount_stmt, items_stmt, user_update, tag_loop, row_count_local) =
        split_optional_row_count_body5(body)?;
    let (user_name, user_key) = match_required_string_prefix(row_name, user_stmt)?;
    let (amount_name, amount_key) = match_required_cast_prefix(row_name, amount_stmt, "float")?;
    let (items_name, items_key) = match_required_cast_prefix(row_name, items_stmt, "int")?;

    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = user_update
    else {
        return None;
    };
    let user_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if user_update.key_name != user_name {
        return None;
    }
    let [first_update, second_update] = user_update.updates.as_slice() else {
        return None;
    };
    if first_update.addend != Expr::Name(amount_name)
        || second_update.addend != Expr::Name(items_name)
    {
        return None;
    }

    let Stmt::For {
        targets,
        iter,
        body: tag_body,
    } = tag_loop
    else {
        return None;
    };
    let [tag_name] = targets.as_slice() else {
        return None;
    };
    let tags_key = match_row_subscript(row_name, iter)?;
    let (tag_name, tag_update_stmt) = match_tag_update_body(tag_name, tag_body)?;
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = tag_update_stmt
    else {
        return None;
    };
    let tag_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if tag_update.key_name != tag_name {
        return None;
    }
    let [tag_count_update] = tag_update.updates.as_slice() else {
        return None;
    };
    if !matches!(tag_count_update.addend, Expr::Int(ref value) if value == "1") {
        return None;
    }

    Some(HotJsonlAggregationBody {
        ops: hot_jsonl_aggregation_ops(
            &user_key,
            &amount_key,
            &items_key,
            &tags_key,
            &first_update.map_name,
            &second_update.map_name,
            user_update.append_list.as_deref(),
            &tag_update.contains_map,
            tag_update.append_list.as_deref(),
        ),
        nested_user_totals: None,
        user_name,
        user_key,
        user_has_default: false,
        user_default: String::new(),
        user_amounts_map: first_update.map_name.clone(),
        user_amount_key: amount_key,
        user_amount_has_default: false,
        user_amount_default: 0.0,
        user_items_map: second_update.map_name.clone(),
        user_items_key: items_key,
        user_items_has_default: false,
        user_items_default: 0,
        users_list: user_update.append_list,
        tags_key,
        tags_default_empty: false,
        tag_counts_map: tag_update.contains_map,
        tags_list: tag_update.append_list,
        row_count_local,
    })
}

fn match_init_then_add_hot_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    let (user_stmt, init_stmt, amount_stmt, items_stmt, tag_loop, row_count_local) =
        split_optional_row_count_body5(body)?;
    let (user_name, user_key) = match_required_string_prefix(row_name, user_stmt)?;
    let [(amounts_map, amount_zero), (items_map, items_zero)] =
        match_two_zero_insert_if(&user_name, init_stmt)?;
    if !matches!(amount_zero, Expr::Float(value) if value == 0.0)
        || !matches!(items_zero, Expr::Int(ref value) if value == "0")
    {
        return None;
    }
    let amount_key = match_map_add_cast(row_name, &amounts_map, &user_name, amount_stmt, "float")?;
    let items_key = match_map_add_cast(row_name, &items_map, &user_name, items_stmt, "int")?;

    let Stmt::For {
        targets,
        iter,
        body: tag_body,
    } = tag_loop
    else {
        return None;
    };
    let [tag_name] = targets.as_slice() else {
        return None;
    };
    let tags_key = match_row_subscript(row_name, iter)?;
    let tag_counts_map = match_tag_init_then_add_body(tag_name, tag_body)?;

    Some(HotJsonlAggregationBody {
        ops: hot_jsonl_aggregation_ops(
            &user_key,
            &amount_key,
            &items_key,
            &tags_key,
            &amounts_map,
            &items_map,
            None,
            &tag_counts_map,
            None,
        ),
        nested_user_totals: None,
        user_name,
        user_key,
        user_has_default: false,
        user_default: String::new(),
        user_amounts_map: amounts_map,
        user_amount_key: amount_key,
        user_amount_has_default: false,
        user_amount_default: 0.0,
        user_items_map: items_map,
        user_items_key: items_key,
        user_items_has_default: false,
        user_items_default: 0,
        users_list: None,
        tags_key,
        tags_default_empty: false,
        tag_counts_map,
        tags_list: None,
        row_count_local,
    })
}

fn match_prefixed_hot_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    let [user_stmt, amount_stmt, items_stmt, fourth_stmt, fifth_stmt, tag_loop] = body else {
        return None;
    };
    let (user_name, user_key, user_default) = match_string_get_prefix(row_name, user_stmt)?;
    let (amount_name, amount_key, amount_default) = match_f64_get_prefix(row_name, amount_stmt)?;
    let (items_name, items_key, items_default) = match_i64_get_prefix(row_name, items_stmt)?;
    let (tags_name, tags_key, user_update) =
        if let Some((tags_name, tags_key)) = match_array_get_prefix(row_name, fourth_stmt) {
            (tags_name, tags_key, fifth_stmt)
        } else {
            let (tags_name, tags_key) = match_array_get_prefix(row_name, fifth_stmt)?;
            (tags_name, tags_key, fourth_stmt)
        };

    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = user_update
    else {
        return None;
    };
    let user_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if user_update.key_name != user_name {
        return None;
    }
    let [first_update, second_update] = user_update.updates.as_slice() else {
        return None;
    };
    if first_update.addend != Expr::Name(amount_name.clone())
        || second_update.addend != Expr::Name(items_name.clone())
    {
        return None;
    }

    let Stmt::For {
        targets,
        iter,
        body: tag_body,
    } = tag_loop
    else {
        return None;
    };
    let [tag_name] = targets.as_slice() else {
        return None;
    };
    if iter != &Expr::Name(tags_name) {
        return None;
    }
    let (tag_name, tag_update_stmt) = match_tag_update_body(tag_name, tag_body)?;
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = tag_update_stmt
    else {
        return None;
    };
    let tag_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if tag_update.key_name != tag_name {
        return None;
    }
    let [tag_count_update] = tag_update.updates.as_slice() else {
        return None;
    };
    if !matches!(tag_count_update.addend, Expr::Int(ref value) if value == "1") {
        return None;
    }

    Some(HotJsonlAggregationBody {
        ops: hot_jsonl_aggregation_ops(
            &user_key,
            &amount_key,
            &items_key,
            &tags_key,
            &first_update.map_name,
            &second_update.map_name,
            user_update.append_list.as_deref(),
            &tag_update.contains_map,
            tag_update.append_list.as_deref(),
        ),
        nested_user_totals: None,
        user_name,
        user_key,
        user_has_default: true,
        user_default,
        user_amounts_map: first_update.map_name.clone(),
        user_amount_key: amount_key,
        user_amount_has_default: true,
        user_amount_default: amount_default,
        user_items_map: second_update.map_name.clone(),
        user_items_key: items_key,
        user_items_has_default: true,
        user_items_default: items_default,
        users_list: user_update.append_list,
        tags_key,
        tags_default_empty: true,
        tag_counts_map: tag_update.contains_map,
        tags_list: tag_update.append_list,
        row_count_local: None,
    })
}

fn match_required_string_prefix(row_name: &str, stmt: &Stmt) -> Option<(String, String)> {
    let Stmt::Assign {
        target: AssignTarget::Name(local),
        value,
    } = stmt
    else {
        return None;
    };
    let key = match_row_subscript(row_name, value)?;
    Some((local.clone(), key))
}

fn split_optional_row_count_body5(
    body: &[Stmt],
) -> Option<(&Stmt, &Stmt, &Stmt, &Stmt, &Stmt, Option<String>)> {
    match body {
        [a, b, c, d, tag_loop] => Some((a, b, c, d, tag_loop, None)),
        [a, b, c, d, count_stmt, tag_loop] => Some((
            a,
            b,
            c,
            d,
            tag_loop,
            Some(match_row_count_increment(count_stmt)?),
        )),
        _ => None,
    }
}

fn match_row_count_increment(stmt: &Stmt) -> Option<String> {
    match stmt {
        Stmt::AugAssign {
            target: AssignTarget::Name(local),
            op: AugOp::Add,
            value: Expr::Int(value),
        } if value == "1" => Some(local.clone()),
        Stmt::Assign {
            target: AssignTarget::Name(local),
            value: Expr::Add { left, right },
        } if matches_add_self_int_one(local, left, right) => Some(local.clone()),
        _ => None,
    }
}

fn matches_add_self_int_one(local: &str, left: &Expr, right: &Expr) -> bool {
    matches!((left, right), (Expr::Name(lhs), Expr::Int(rhs)) if lhs == local && rhs == "1")
        || matches!((left, right), (Expr::Int(lhs), Expr::Name(rhs)) if lhs == "1" && rhs == local)
}

fn match_required_cast_prefix(
    row_name: &str,
    stmt: &Stmt,
    cast_name: &str,
) -> Option<(String, String)> {
    let (target, value) = match_name_assignment(stmt)?;
    let key = match_cast_row_subscript(row_name, value, cast_name)?;
    Some((target.to_owned(), key))
}

fn match_nested_totals_init(user_name: &str, stmt: &Stmt) -> Option<(String, String, String)> {
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = stmt
    else {
        return None;
    };
    if !else_branch.is_empty() {
        return None;
    }
    let (key_name, map_name) = match_key_not_in_map(condition)?;
    if key_name != user_name {
        return None;
    }
    let [insert] = then_branch.as_slice() else {
        return None;
    };
    let Stmt::Assign {
        target,
        value: Expr::Record(fields),
    } = insert
    else {
        return None;
    };
    let (insert_map, insert_key) = match_map_key_target(target)?;
    if insert_map != map_name || insert_key != key_name {
        return None;
    }
    let amount_field = match_zero_record_field(fields, true)?;
    let items_field = match_zero_record_field(fields, false)?;
    Some((map_name, amount_field, items_field))
}

fn match_zero_record_field(fields: &[(String, Expr)], want_float: bool) -> Option<String> {
    fields
        .iter()
        .find_map(|(name, value)| match (want_float, value) {
            (true, Expr::Float(value)) if *value == 0.0 => Some(name.clone()),
            (false, Expr::Int(value)) if value == "0" => Some(name.clone()),
            _ => None,
        })
}

fn match_nested_total_add(
    row_name: &str,
    totals_map: &str,
    user_name: &str,
    stmt: &Stmt,
    field_name: &str,
    cast_name: &str,
) -> Option<String> {
    let Stmt::AugAssign {
        target,
        op: AugOp::Add,
        value,
    } = stmt
    else {
        return None;
    };
    let (target_map, target_key, target_field) = match_nested_map_field_target(target)?;
    if target_map != totals_map || target_key != user_name || target_field != field_name {
        return None;
    }
    match_cast_row_subscript(row_name, value, cast_name)
}

fn match_nested_map_field_target(target: &AssignTarget) -> Option<(String, String, String)> {
    let AssignTarget::Subscript { value, index } = target else {
        return None;
    };
    let Expr::String(field_name) = index else {
        return None;
    };
    let (map_name, key_name) = match_map_key_target(value)?;
    Some((map_name, key_name, field_name.clone()))
}

fn match_cast_row_subscript(row_name: &str, value: &Expr, cast_name: &str) -> Option<String> {
    let Expr::Call(Call {
        name,
        positional,
        named,
    }) = value
    else {
        return None;
    };
    if name != cast_name || !named.is_empty() {
        return None;
    }
    let [arg] = positional.as_slice() else {
        return None;
    };
    match_row_subscript(row_name, arg)
}

fn match_two_zero_insert_if(user_name: &str, stmt: &Stmt) -> Option<[(String, Expr); 2]> {
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = stmt
    else {
        return None;
    };
    if !else_branch.is_empty() {
        return None;
    }
    let (key_name, first_map) = match_key_not_in_map(condition)?;
    if key_name != user_name {
        return None;
    }
    let [first, second] = then_branch.as_slice() else {
        return None;
    };
    let (first_insert_map, first_key, first_value) = match_insert_assignment(first)?;
    if first_insert_map != first_map || first_key != key_name {
        return None;
    }
    let (second_insert_map, second_key, second_value) = match_insert_assignment(second)?;
    if second_key != key_name {
        return None;
    }
    Some([
        (first_insert_map, first_value),
        (second_insert_map, second_value),
    ])
}

fn match_map_add_cast(
    row_name: &str,
    map_name: &str,
    key_name: &str,
    stmt: &Stmt,
    cast_name: &str,
) -> Option<String> {
    let Stmt::AugAssign {
        target,
        op: AugOp::Add,
        value,
    } = stmt
    else {
        return None;
    };
    let (target_map, target_key) = match_map_key_target(target)?;
    if target_map != map_name || target_key != key_name {
        return None;
    }
    match_cast_row_subscript(row_name, value, cast_name)
}

fn match_tag_init_then_add_body(tag_name: &str, body: &[Stmt]) -> Option<String> {
    let [init_stmt, add_stmt] = body else {
        return None;
    };
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = init_stmt
    else {
        return None;
    };
    if !else_branch.is_empty() {
        return None;
    }
    let (key_name, map_name) = match_key_not_in_map(condition)?;
    if key_name != tag_name {
        return None;
    }
    let [insert_stmt] = then_branch.as_slice() else {
        return None;
    };
    let (insert_map, insert_key, insert_value) = match_insert_assignment(insert_stmt)?;
    if insert_map != map_name
        || insert_key != key_name
        || !matches!(insert_value, Expr::Int(ref value) if value == "0")
    {
        return None;
    }
    let Stmt::AugAssign {
        target,
        op: AugOp::Add,
        value: Expr::Int(value),
    } = add_stmt
    else {
        return None;
    };
    let (add_map, add_key) = match_map_key_target(target)?;
    if add_map != map_name || add_key != key_name || value != "1" {
        return None;
    }
    Some(map_name)
}

fn match_prefixed_count_hot_jsonl_aggregation_body(
    row_name: &str,
    body: &[Stmt],
) -> Option<HotJsonlAggregationBody> {
    let [user_stmt, amount_stmt, tags_stmt, user_update, tag_loop] = body else {
        return None;
    };
    let (user_name, user_key, user_default) = match_string_get_prefix(row_name, user_stmt)?;
    let (amount_name, amount_key, amount_default) = match_f64_get_prefix(row_name, amount_stmt)?;
    let (tags_name, tags_key) = match_array_get_prefix(row_name, tags_stmt)?;

    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = user_update
    else {
        return None;
    };
    let user_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if user_update.key_name != user_name {
        return None;
    }
    let [amount_update, count_update] = user_update.updates.as_slice() else {
        return None;
    };
    if amount_update.addend != Expr::Name(amount_name.clone())
        || !matches!(count_update.addend, Expr::Int(ref value) if value == "1")
    {
        return None;
    }

    let Stmt::For {
        targets,
        iter,
        body: tag_body,
    } = tag_loop
    else {
        return None;
    };
    let [tag_name] = targets.as_slice() else {
        return None;
    };
    if iter != &Expr::Name(tags_name) {
        return None;
    }
    let (tag_name, tag_update_stmt) = match_tag_update_body(tag_name, tag_body)?;
    let Stmt::If {
        condition,
        then_branch,
        else_branch,
    } = tag_update_stmt
    else {
        return None;
    };
    let tag_update = match_fused_map_update_if(condition, then_branch, else_branch)?;
    if tag_update.key_name != tag_name {
        return None;
    }
    let [tag_count_update] = tag_update.updates.as_slice() else {
        return None;
    };
    if !matches!(tag_count_update.addend, Expr::Int(ref value) if value == "1") {
        return None;
    }

    Some(HotJsonlAggregationBody {
        ops: hot_jsonl_aggregation_ops(
            &user_key,
            &amount_key,
            "",
            &tags_key,
            &amount_update.map_name,
            &count_update.map_name,
            user_update.append_list.as_deref(),
            &tag_update.contains_map,
            tag_update.append_list.as_deref(),
        ),
        nested_user_totals: None,
        user_name,
        user_key,
        user_has_default: true,
        user_default,
        user_amounts_map: amount_update.map_name.clone(),
        user_amount_key: amount_key,
        user_amount_has_default: true,
        user_amount_default: amount_default,
        user_items_map: count_update.map_name.clone(),
        user_items_key: String::new(),
        user_items_has_default: true,
        user_items_default: 1,
        users_list: user_update.append_list,
        tags_key,
        tags_default_empty: true,
        tag_counts_map: tag_update.contains_map,
        tags_list: tag_update.append_list,
        row_count_local: None,
    })
}

fn hot_jsonl_aggregation_ops(
    user_key: &str,
    amount_key: &str,
    items_key: &str,
    tags_key: &str,
    user_amounts_map: &str,
    user_items_map: &str,
    users_list: Option<&str>,
    tag_counts_map: &str,
    tags_list: Option<&str>,
) -> Vec<HotJsonlBodyOp> {
    vec![
        HotJsonlBodyOp::JsonGetFields {
            user_key: user_key.to_owned(),
            amount_key: amount_key.to_owned(),
            items_key: items_key.to_owned(),
            tags_key: tags_key.to_owned(),
        },
        HotJsonlBodyOp::MapAddF64 {
            map_name: user_amounts_map.to_owned(),
            key_slot: HotJsonlSlot::User,
            value_slot: HotJsonlSlot::Amount,
            append_list: users_list.map(str::to_owned),
        },
        HotJsonlBodyOp::MapAddI64 {
            map_name: user_items_map.to_owned(),
            key_slot: HotJsonlSlot::User,
            value_slot: HotJsonlSlot::Items,
        },
        HotJsonlBodyOp::ForEachJsonString {
            array_slot: HotJsonlSlot::Tags,
            item_slot: HotJsonlSlot::Tag,
            body: vec![HotJsonlBodyOp::MapAddI64Const {
                map_name: tag_counts_map.to_owned(),
                key_slot: HotJsonlSlot::Tag,
                value: 1,
                append_list: tags_list.map(str::to_owned),
            }],
        },
    ]
}

fn match_string_get_prefix(row_name: &str, stmt: &Stmt) -> Option<(String, String, String)> {
    let (target, value) = match_name_assignment(stmt)?;
    let (key, default) = match_row_get(row_name, value)?;
    let Expr::String(default) = default else {
        return None;
    };
    Some((target.to_owned(), key, default.clone()))
}

fn match_f64_get_prefix(row_name: &str, stmt: &Stmt) -> Option<(String, String, f64)> {
    let (target, value) = match_name_assignment(stmt)?;
    let Expr::Call(Call {
        name,
        positional,
        named,
    }) = value
    else {
        return None;
    };
    if name != "float" || !named.is_empty() {
        return None;
    }
    let [arg] = positional.as_slice() else {
        return None;
    };
    let (key, default) = match_row_get(row_name, arg)?;
    let default = match default {
        Expr::Float(default) => *default,
        Expr::Int(default) => default.parse::<i64>().ok()? as f64,
        _ => return None,
    };
    Some((target.to_owned(), key, default))
}

fn match_i64_get_prefix(row_name: &str, stmt: &Stmt) -> Option<(String, String, i64)> {
    let (target, value) = match_name_assignment(stmt)?;
    let Expr::Call(Call {
        name,
        positional,
        named,
    }) = value
    else {
        return None;
    };
    if name != "int" || !named.is_empty() {
        return None;
    }
    let [arg] = positional.as_slice() else {
        return None;
    };
    let (key, default) = match_row_get(row_name, arg)?;
    let Expr::Int(default) = default else {
        return None;
    };
    Some((target.to_owned(), key, default.parse().ok()?))
}

fn match_array_get_prefix(row_name: &str, stmt: &Stmt) -> Option<(String, String)> {
    let (target, value) = match_name_assignment(stmt)?;
    let (key, default) = match_row_get(row_name, value)?;
    let Expr::List(items) = default else {
        return None;
    };
    if !items.is_empty() {
        return None;
    }
    Some((target.to_owned(), key))
}

fn match_tag_update_body<'a>(tag_name: &str, body: &'a [Stmt]) -> Option<(String, &'a Stmt)> {
    match body {
        [stmt] => Some((tag_name.to_owned(), stmt)),
        [alias_stmt, update_stmt] => {
            let (alias, source) = match_str_alias_assignment(alias_stmt)?;
            if source != tag_name {
                return None;
            }
            Some((alias, update_stmt))
        }
        _ => None,
    }
}

fn match_str_alias_assignment(stmt: &Stmt) -> Option<(String, String)> {
    let (target, value) = match_name_assignment(stmt)?;
    let Expr::Call(Call {
        name,
        positional,
        named,
    }) = value
    else {
        return None;
    };
    if name != "str" || !named.is_empty() {
        return None;
    }
    let [Expr::Name(source)] = positional.as_slice() else {
        return None;
    };
    Some((target.to_owned(), source.clone()))
}

fn match_name_assignment(stmt: &Stmt) -> Option<(&str, &Expr)> {
    let Stmt::Assign {
        target: AssignTarget::Name(target),
        value,
    } = stmt
    else {
        return None;
    };
    Some((target.as_str(), value))
}

fn match_fused_map_update_if(
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

fn match_key_not_in_map(condition: &Expr) -> Option<(String, String)> {
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

fn match_insert_assignment(stmt: &Stmt) -> Option<(String, String, Expr)> {
    let Stmt::Assign { target, value } = stmt else {
        return None;
    };
    let (map_name, key_name) = match_map_key_target(target)?;
    Some((map_name, key_name, value.clone()))
}

fn match_map_key_target(target: &AssignTarget) -> Option<(String, String)> {
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

#[derive(Debug, Clone, Copy, PartialEq)]
struct HotJsonlAggregationIrSubgraph {
    user_has_default: bool,
    user_amount_has_default: bool,
    user_amount_default: f64,
    user_items_has_default: bool,
    user_items_default: i64,
    tags_default_empty: bool,
    users_append: Option<AccId>,
    tags_append: Option<AccId>,
}

fn match_hot_jsonl_aggregation_ir_subgraph(
    function: &StoneIrFunction,
) -> Option<HotJsonlAggregationIrSubgraph> {
    let [row_block, tag_block, done_block] = function.blocks.as_slice() else {
        return None;
    };
    let [user_op, amount_op, items_op, tags_op, user_amounts_op, user_items_op] =
        row_block.ops.as_slice()
    else {
        return None;
    };
    let (user_reg, user_has_default) = match user_op {
        StoneOp::JsonGetStrDefault {
            dst: Reg(1),
            object: Reg(0),
            key: ConstId(0),
            default: ConstId(1),
        } => (Reg(1), true),
        StoneOp::JsonGetValue {
            dst: Reg(1),
            object: Reg(0),
            key: ConstId(0),
        } => (Reg(1), false),
        _ => return None,
    };
    let (user_amount_has_default, user_amount_default) = match amount_op {
        StoneOp::JsonGetF64Default {
            dst: Reg(2),
            object: Reg(0),
            key: ConstId(2),
            default,
        } => (true, *default),
        StoneOp::JsonGetF64Required {
            dst: Reg(2),
            object: Reg(0),
            key: ConstId(2),
        } => (false, 0.0),
        _ => return None,
    };
    let (user_items_has_default, user_items_default) = match items_op {
        StoneOp::JsonGetI64Default {
            dst: Reg(3),
            object: Reg(0),
            key: ConstId(3),
            default,
        } => (true, *default),
        StoneOp::JsonGetI64Required {
            dst: Reg(3),
            object: Reg(0),
            key: ConstId(3),
        } => (false, 0),
        _ => return None,
    };
    let tags_default_empty = match tags_op {
        StoneOp::JsonGetArrayDefault {
            dst: Reg(4),
            object: Reg(0),
            key: ConstId(4),
        } => true,
        StoneOp::JsonGetArrayRequired {
            dst: Reg(4),
            object: Reg(0),
            key: ConstId(4),
        } => false,
        _ => return None,
    };
    let StoneOp::MapAddF64 {
        map: AccId(0),
        key,
        value: Reg(2),
        append: users_append,
    } = user_amounts_op
    else {
        return None;
    };
    if *key != user_reg || !matches!(users_append, None | Some(AccId(2))) {
        return None;
    }
    let StoneOp::MapAddI64 {
        map: AccId(1),
        key,
        value: Reg(3),
        append: None,
    } = user_items_op
    else {
        return None;
    };
    if *key != user_reg {
        return None;
    }
    if row_block.terminator
        != (StoneTerminator::JsonEachStrArray {
            array: Reg(4),
            item: Reg(5),
            body: BlockId(1),
            done: BlockId(2),
        })
    {
        return None;
    }
    let [StoneOp::MapAddI64Const {
        map: AccId(3),
        key: Reg(5),
        value: 1,
        append: tags_append,
    }] = tag_block.ops.as_slice()
    else {
        return None;
    };
    if !matches!(tags_append, None | Some(AccId(4)))
        || tag_block.terminator != (StoneTerminator::Jump { target: BlockId(0) })
        || done_block.terminator != StoneTerminator::Return
        || !done_block.ops.is_empty()
    {
        return None;
    }

    Some(HotJsonlAggregationIrSubgraph {
        user_has_default,
        user_amount_has_default,
        user_amount_default,
        user_items_has_default,
        user_items_default,
        tags_default_empty,
        users_append: *users_append,
        tags_append: *tags_append,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stone_ast::{lower_source, Stmt};

    fn required_jsonl_aggregation_vm() -> StoneIrFunction {
        let body = HotJsonlAggregationBody {
            ops: hot_jsonl_aggregation_ops(
                "customer_id",
                "revenue",
                "units",
                "labels",
                "customer_revenue",
                "customer_units",
                Some("customers"),
                "label_counts",
                Some("labels"),
            ),
            nested_user_totals: None,
            user_name: "customer_id".to_owned(),
            user_key: "customer_id".to_owned(),
            user_has_default: false,
            user_default: String::new(),
            user_amounts_map: "customer_revenue".to_owned(),
            user_amount_key: "revenue".to_owned(),
            user_amount_has_default: false,
            user_amount_default: 0.0,
            user_items_map: "customer_units".to_owned(),
            user_items_key: "units".to_owned(),
            user_items_has_default: false,
            user_items_default: 0,
            users_list: Some("customers".to_owned()),
            tags_key: "labels".to_owned(),
            tags_default_empty: false,
            tag_counts_map: "label_counts".to_owned(),
            tags_list: Some("labels".to_owned()),
            row_count_local: None,
        };
        compile_hot_jsonl_vm_function(&body).expect("JSONL loop IR")
    }

    #[test]
    fn lowers_single_target_read_jsonl_loop_shape() {
        let program = lower_source(
            r#"
for row in read_jsonl("records.jsonl"):
    x = row.get("user", "unknown")
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[0]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_hot_loop(targets, iter, body).expect("hot loop plan");
        assert_eq!(plan.target, "row");
        assert!(matches!(plan.iter, HotLoopIter::ReadJsonl { .. }));
        assert_eq!(
            plan.ops,
            vec![HotLoopOp::JsonGetStrDefault {
                target: "x".to_owned(),
                key: "user".to_owned(),
                default: "unknown".to_owned(),
            }]
        );
        assert_eq!(plan.body_start, 1);
    }

    #[test]
    fn rejects_non_jsonl_loop_shape() {
        let program = lower_source(
            r#"
for row in rows:
    x = row
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[0]
        else {
            panic!("expected for loop");
        };
        assert!(try_lower_hot_loop(targets, iter, body).is_none());
    }

    #[test]
    fn lowers_typed_json_get_prefix() {
        let program = lower_source(
            r#"
for row in read_jsonl("records.jsonl"):
    user = row.get("user", "unknown")
    amount = float(row.get("amount", 0.0))
    items = int(row.get("items", 0))
    tags = row.get("tags", [])
    keep = user
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[0]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_hot_loop(targets, iter, body).expect("hot loop plan");
        assert_eq!(plan.body_start, 4);
        assert_eq!(plan.ops.len(), 4);
        assert!(matches!(plan.ops[1], HotLoopOp::JsonGetF64Default { .. }));
        assert!(matches!(plan.ops[2], HotLoopOp::JsonGetI64Default { .. }));
        assert!(matches!(plan.ops[3], HotLoopOp::JsonGetArrayDefault { .. }));
    }

    #[test]
    fn lowers_direct_json_subscript_prefix() {
        let program = lower_source(
            r#"
for record in read_jsonl("records.jsonl"):
    user = record["user"]
    keep = user
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[0]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_hot_loop(targets, iter, body).expect("hot loop plan");
        assert_eq!(plan.body_start, 1);
        assert_eq!(
            plan.ops,
            vec![HotLoopOp::JsonGetValue {
                target: "user".to_owned(),
                key: "user".to_owned(),
            }]
        );
    }

    #[test]
    fn lowers_json_loads_text_line_loop_prefix() {
        let program = lower_source(
            r#"
for line in lines:
    if line.strip() == "":
        continue
    record = json_loads(line)
    user = record.get("user", "unknown")
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[0]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_hot_loop(targets, iter, body).expect("hot loop plan");
        assert_eq!(plan.target, "record");
        assert_eq!(
            plan.iter,
            HotLoopIter::JsonlTextLines {
                line_target: "line".to_owned()
            }
        );
        assert_eq!(plan.body_start, 2);
    }

    #[test]
    fn lowers_generic_numeric_list_add_assign_loop() {
        let program = lower_source(
            r#"
total = 0
for n in [1, 2, 3]:
    total += n
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[1]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.target, "n");
        assert_eq!(plan.iter, GenericLoopIter::MaterializedList);
        assert_eq!(
            plan.ops,
            vec![GenericLoopOp::AddAssign {
                local: "total".to_owned(),
                item: "n".to_owned(),
            }]
        );
    }

    #[test]
    fn lowers_generic_range_add_assign_loop() {
        let program = lower_source(
            r#"
total = 0
for n in range(5):
    total = total + n
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[1]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::Range);
    }

    #[test]
    fn lowers_generic_string_count_loop() {
        let program = lower_source(
            r#"
counts = {}
for tag in ["a", "b", "a"]:
    if tag in counts:
        counts[tag] += 1
    else:
        counts[tag] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[1]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(
            plan.ops,
            vec![GenericLoopOp::MapAddI64Const {
                map: "counts".to_owned(),
                key: "tag".to_owned(),
                value: 1,
            }]
        );
    }

    #[test]
    fn lowers_generic_unique_list_append_loop() {
        let program = lower_source(
            r#"
seen = []
for tag in ["a", "b", "a"]:
    if not tag in seen:
        seen.append(tag)
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[1]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::MaterializedList);
        assert_eq!(
            plan.ops,
            vec![GenericLoopOp::ListAppendUnique {
                list: "seen".to_owned(),
                item: "tag".to_owned(),
            }]
        );
    }

    #[test]
    fn lowers_generic_record_field_strip_lower_count_loop() {
        let program = lower_source(
            r#"
counts = {}
for row in read_csv("input.csv"):
    status = row["status"].strip().lower()
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[1]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(
            plan.ops,
            vec![GenericLoopOp::MapAddI64ConstRecordStringField {
                map: "counts".to_owned(),
                item: "row".to_owned(),
                field: "status".to_owned(),
                strip: true,
                lower: true,
                value: 1,
            }]
        );
    }

    #[test]
    fn lowers_generic_open_splitlines_parse_sum_loop() {
        let program = lower_source(
            r#"
total = 0
for line in open("numbers.txt").splitlines():
    total += int(line)
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[1]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::OpenSplitlines);
        assert_eq!(
            plan.ops,
            vec![GenericLoopOp::AddAssignParsedInt {
                local: "total".to_owned(),
                item: "line".to_owned(),
            }]
        );
    }

    #[test]
    fn lowers_generic_read_csv_record_field_count_loop() {
        let program = lower_source(
            r#"
counts = {}
for row in read_csv("input.csv"):
    status = row["status"]
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[1]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::ReadCsv);
        assert_eq!(
            plan.ops,
            vec![GenericLoopOp::MapAddI64ConstRecordStringField {
                map: "counts".to_owned(),
                item: "row".to_owned(),
                field: "status".to_owned(),
                strip: false,
                lower: false,
                value: 1,
            }]
        );
    }

    #[test]
    fn compiles_generic_numeric_sum_to_vm_ir() {
        let plan = lower_first_generic_loop(
            r#"
total = 0
for n in [1, 2, 3]:
    total += n
"#,
            1,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        assert_eq!(vm.iter, GenericLoopIter::MaterializedList);
        assert_eq!(vm.adapter, LoopIrIteratorAdapter::MaterializedValues);
        assert_eq!(vm.locals, vec!["total"]);
        assert_eq!(vm.ops, vec![GenericVmOp::AddAssign { local: 0 }]);
    }

    #[test]
    fn compiles_generic_parse_sum_to_vm_ir() {
        let plan = lower_first_generic_loop(
            r#"
total = 0
for line in open("numbers.txt").splitlines():
    total += float(line)
"#,
            1,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        assert_eq!(vm.iter, GenericLoopIter::OpenSplitlines);
        assert_eq!(vm.adapter, LoopIrIteratorAdapter::TextLines);
        assert_eq!(vm.locals, vec!["total"]);
        assert_eq!(
            vm.ops,
            vec![GenericVmOp::AddAssignParsed {
                local: 0,
                parse: GenericParseNumber::Float,
            }]
        );
    }

    #[test]
    fn compiles_generic_record_field_count_to_vm_ir() {
        let plan = lower_first_generic_loop(
            r#"
counts = {}
for row in read_csv("input.csv"):
    status = row["status"].strip().lower()
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
"#,
            1,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        assert_eq!(vm.iter, GenericLoopIter::ReadCsv);
        assert_eq!(vm.adapter, LoopIrIteratorAdapter::CsvRows);
        assert_eq!(vm.locals, vec!["counts"]);
        assert_eq!(vm.registers, 0);
        assert!(vm.constants.is_empty());
        assert_eq!(vm.entry, 0);
        assert_eq!(
            vm.ops,
            vec![GenericVmOp::MapAddI64ConstRecordStringField {
                map: 0,
                field: "status".to_owned(),
                strip: true,
                lower: true,
                addend: 1,
            }]
        );
        assert_eq!(
            vm.blocks,
            vec![LoopIrBlock {
                ops: vm.ops.clone(),
                terminator: LoopIrTerminator::Return,
            }]
        );
        assert_eq!(
            vm.snapshots,
            vec![
                LoopIrSnapshot {
                    locals: vec![0],
                    boundary: LoopIrSnapshotBoundary::LoopEntry,
                },
                LoopIrSnapshot {
                    locals: vec![0],
                    boundary: LoopIrSnapshotBoundary::IterationEnd,
                },
            ]
        );
        assert_eq!(
            vm.diagnostics,
            LoopIrDiagnostics {
                lowering_path: "map_add_i64_const_record_string_field",
            }
        );
        assert_eq!(
            select_loop_ir_fused_kernel(&vm),
            Some(LoopIrFusedKernel::MapAddI64Const)
        );
        assert_eq!(
            match_loop_ir_subgraph(&vm),
            Some(LoopIrSubgraphKind::MapAddI64Const)
        );
        assert_eq!(
            optimize_loop_ir(&vm),
            LoopIrOptimizationResult {
                function: vm.clone(),
                selected_kernel: Some(LoopIrFusedKernel::MapAddI64Const),
                matched_subgraph: Some(LoopIrSubgraphKind::MapAddI64Const),
                diagnostics: Vec::new(),
            }
        );
    }

    #[test]
    fn compiles_generic_unique_append_to_vm_ir() {
        let plan = lower_first_generic_loop(
            r#"
seen = []
for tag in ["a", "b", "a"]:
    if not tag in seen:
        seen.append(tag)
"#,
            1,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        assert_eq!(vm.locals, vec!["seen"]);
        assert_eq!(
            vm.ops,
            vec![GenericVmOp::ListAppend {
                list: 0,
                unique: true,
            }]
        );
        assert_eq!(
            select_loop_ir_fused_kernel(&vm),
            Some(LoopIrFusedKernel::ListAppend)
        );
        assert_eq!(
            match_loop_ir_subgraph(&vm),
            Some(LoopIrSubgraphKind::ListAppend)
        );
        assert_eq!(
            optimize_loop_ir(&vm),
            LoopIrOptimizationResult {
                function: vm.clone(),
                selected_kernel: Some(LoopIrFusedKernel::ListAppend),
                matched_subgraph: Some(LoopIrSubgraphKind::ListAppend),
                diagnostics: Vec::new(),
            }
        );
    }

    #[test]
    fn canonicalizes_plain_record_string_count_to_record_field_count() {
        let plan = lower_first_generic_loop(
            r#"
counts = {}
for row in read_csv("input.csv"):
    status = row["status"]
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
"#,
            1,
        );
        let mut vm = compile_generic_vm_function(&plan).expect("generic VM function");
        vm.ops = vec![GenericVmOp::MapAddI64ConstRecordStringField {
            map: 0,
            field: "status".to_owned(),
            strip: false,
            lower: false,
            addend: 1,
        }];
        vm.blocks[vm.entry].ops = vm.ops.clone();

        let optimized = optimize_loop_ir(&vm);
        assert_eq!(
            optimized.function.ops,
            vec![GenericVmOp::MapAddI64ConstRecordField {
                map: 0,
                field: "status".to_owned(),
                addend: 1,
            }]
        );
        assert_eq!(
            optimized.function.blocks[optimized.function.entry].ops,
            optimized.function.ops
        );
        assert_eq!(
            optimized.diagnostics,
            vec![LoopIrOptimizationDiagnostic::Canonicalized]
        );
        assert_eq!(
            optimized.selected_kernel,
            Some(LoopIrFusedKernel::MapAddI64Const)
        );
    }

    #[test]
    fn canonicalize_loop_ir_leaves_noncanonical_string_transform_count_intact() {
        let plan = lower_first_generic_loop(
            r#"
counts = {}
for row in read_csv("input.csv"):
    status = row["status"].strip().lower()
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
"#,
            1,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        let optimized = optimize_loop_ir(&vm);
        assert_eq!(optimized.function, vm);
        assert!(optimized.diagnostics.is_empty());
        assert_eq!(
            optimized.selected_kernel,
            Some(LoopIrFusedKernel::MapAddI64Const)
        );
    }

    #[test]
    fn canonicalize_loop_ir_leaves_non_equivalent_expr_body_unfused() {
        let plan = lower_first_generic_loop(
            r#"
total = 0
for n in [1, 2, 3]:
    total = total + n
"#,
            1,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        let optimized = optimize_loop_ir(&vm);
        assert_eq!(optimized.function, vm);
        assert_eq!(optimized.selected_kernel, None);
        assert_eq!(optimized.matched_subgraph, None);
        assert!(optimized.diagnostics.is_empty());
    }

    #[test]
    fn generic_loop_fusion_rejects_perturbed_ir_ops() {
        let plan = lower_first_generic_loop(
            r#"
counts = {}
for row in read_csv("input.csv"):
    status = row["status"].strip().lower()
    if status in counts:
        counts[status] += 1
    else:
        counts[status] = 1
"#,
            1,
        );
        let mut vm = compile_generic_vm_function(&plan).expect("generic VM function");
        vm.ops.clear();
        assert_eq!(match_loop_ir_subgraph(&vm), None);
        assert_eq!(select_loop_ir_fused_kernel(&vm), None);
        assert_eq!(
            optimize_loop_ir(&vm),
            LoopIrOptimizationResult {
                function: vm.clone(),
                selected_kernel: None,
                matched_subgraph: None,
                diagnostics: Vec::new(),
            }
        );
    }

    #[test]
    fn generic_loop_fusion_rejects_perturbed_ir_entry() {
        let plan = lower_first_generic_loop(
            r#"
items = []
for value in [1, 2, 3]:
    items.append(value)
"#,
            1,
        );
        let mut vm = compile_generic_vm_function(&plan).expect("generic VM function");
        vm.entry = vm.blocks.len();
        assert_eq!(match_loop_ir_subgraph(&vm), None);
        assert_eq!(select_loop_ir_fused_kernel(&vm), None);
        assert_eq!(
            optimize_loop_ir(&vm),
            LoopIrOptimizationResult {
                function: vm.clone(),
                selected_kernel: None,
                matched_subgraph: None,
                diagnostics: Vec::new(),
            }
        );
    }

    #[test]
    fn compiles_generic_integer_expression_body_to_vm_ir() {
        let plan = lower_first_generic_loop(
            r#"
total = 0
mask = 0
for n in [1, 2, 3]:
    total += n * 2
    mask = mask | (n << 1)
"#,
            2,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        assert_eq!(vm.locals, vec!["n", "total", "mask"]);
        let [GenericVmOp::ExprBody(body)] = vm.ops.as_slice() else {
            panic!("expected expression VM body");
        };
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::MulI64 { .. })));
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::ShlI64 { .. })));
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::BitOrI64 { .. })));
    }

    #[test]
    fn compiles_generic_bitwise_expression_body_to_vm_ir() {
        let plan = lower_first_generic_loop(
            r#"
mask = 0
for n in [1, 2, 3]:
    mask = (mask & 7) ^ (n >> 1)
    mask = mask + ~n
"#,
            1,
        );
        let vm = compile_generic_vm_function(&plan).expect("generic VM function");
        let [GenericVmOp::ExprBody(body)] = vm.ops.as_slice() else {
            panic!("expected expression VM body");
        };
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::BitAndI64 { .. })));
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::BitXorI64 { .. })));
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::ShrI64 { .. })));
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::BitNotI64 { .. })));
    }

    #[test]
    fn loop_ir_opcodes_have_stable_ids() {
        assert_eq!(GenericVmOp::AddAssign { local: 0 }.opcode_id(), 1);
        assert_eq!(
            GenericVmOp::ListAppend {
                list: 0,
                unique: true,
            }
            .opcode_id(),
            6
        );
        assert_eq!(
            GenericVmExprOp::MulI64 {
                dst: 0,
                lhs: 1,
                rhs: 2,
            }
            .opcode_id(),
            106
        );
        assert_eq!(
            StoneOp::MapAddI64Const {
                map: AccId(0),
                key: Reg(0),
                value: 1,
                append: None,
            }
            .opcode_id(),
            208
        );
        assert_eq!(
            LoopIrFusedKernel::JsonlAggregation
                .type_assumptions()
                .inputs,
            &["json_object_row", "f64_map", "i64_map", "string_list"]
        );
    }

    #[test]
    fn lowers_generic_read_jsonl_aggregation_loop() {
        let program = lower_source(
            r#"
customer_revenue = {}
customer_units = {}
customers = []
label_counts = {}
labels = []
for row in read_jsonl("records.jsonl"):
    customer_id = row["customer_id"]
    if customer_id in customer_revenue:
        customer_revenue[customer_id] = customer_revenue[customer_id] + row["revenue"]
        customer_units[customer_id] = customer_units[customer_id] + row["units"]
    else:
        customer_revenue[customer_id] = row["revenue"]
        customer_units[customer_id] = row["units"]
        customers.append(customer_id)
    for label in row["labels"]:
        if label in label_counts:
            label_counts[label] = label_counts[label] + 1
        else:
            label_counts[label] = 1
            labels.append(label)
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[5]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::ReadJsonl);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        assert_eq!(body.user_name, "customer_id");
        assert_eq!(body.user_amounts_map, "customer_revenue");
        assert_eq!(body.user_items_map, "customer_units");
        assert_eq!(body.tag_counts_map, "label_counts");
        let vm = compile_hot_jsonl_loop_ir_function(&plan).expect("jsonl loop IR");
        assert_eq!(
            vm.adapter,
            Some(LoopIrIteratorAdapter::JsonlRows { guarded: false })
        );
    }

    #[test]
    fn lowers_model_style_jsonl_count_aggregation_loop() {
        let program = lower_source(
            r#"
user_amounts = {}
user_items = {}
tag_counts = {}
for row in read_jsonl("records.jsonl"):
    user = row.get("user", "")
    amount = float(row.get("amount", 0))
    tags = row.get("tags", [])

    if user in user_amounts:
        user_amounts[user] += amount
        user_items[user] += 1
    else:
        user_amounts[user] = amount
        user_items[user] = 1

    for tag in tags:
        if tag in tag_counts:
            tag_counts[tag] += 1
        else:
            tag_counts[tag] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[3]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::ReadJsonl);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        assert_eq!(body.user_name, "user");
        assert_eq!(body.user_key, "user");
        assert_eq!(body.user_default, "");
        assert_eq!(body.user_amounts_map, "user_amounts");
        assert_eq!(body.user_amount_key, "amount");
        assert_eq!(body.user_amount_default, 0.0);
        assert_eq!(body.user_items_map, "user_items");
        assert_eq!(body.user_items_key, "");
        assert_eq!(body.user_items_default, 1);
        assert_eq!(body.tag_counts_map, "tag_counts");
        assert_eq!(body.tags_key, "tags");
    }

    #[test]
    fn lowers_nested_user_totals_jsonl_aggregation_loop() {
        let program = lower_source(
            r#"
user_totals = {}
tag_counts = {}
rows = read_jsonl("records.jsonl")
for row in rows:
    user = row.user
    if user not in user_totals:
        user_totals[user] = {"total_amount": 0.0, "total_items": 0}
    user_totals[user]["total_amount"] += float(row.amount)
    user_totals[user]["total_items"] += int(row.items)
    for tag in row.tags:
        if tag in tag_counts:
            tag_counts[tag] += 1
        else:
            tag_counts[tag] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[3]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::MaterializedList);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        let nested = body
            .nested_user_totals
            .as_ref()
            .expect("nested totals plan");
        assert_eq!(nested.map_name, "user_totals");
        assert_eq!(nested.amount_field, "total_amount");
        assert_eq!(nested.items_field, "total_items");
        assert_eq!(body.user_key, "user");
        assert_eq!(body.user_amount_key, "amount");
        assert_eq!(body.user_items_key, "items");
        assert_eq!(body.tags_key, "tags");
        assert_eq!(body.tag_counts_map, "tag_counts");
    }

    #[test]
    fn lowers_init_then_add_jsonl_aggregation_loop() {
        let program = lower_source(
            r#"
user_amounts = {}
user_items = {}
tag_counts = {}
rows = read_jsonl("records.jsonl")
for row in rows:
    user = row["user"]
    if user not in user_amounts:
        user_amounts[user] = 0.0
        user_items[user] = 0
    user_amounts[user] += float(row["amount"])
    user_items[user] += int(row["items"])
    for tag in row["tags"]:
        if tag not in tag_counts:
            tag_counts[tag] = 0
        tag_counts[tag] += 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[4]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::MaterializedList);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        assert_eq!(body.user_amounts_map, "user_amounts");
        assert_eq!(body.user_items_map, "user_items");
        assert_eq!(body.tag_counts_map, "tag_counts");
        assert_eq!(body.user_amount_key, "amount");
        assert_eq!(body.user_items_key, "items");
        assert_eq!(body.tags_key, "tags");
    }

    #[test]
    fn lowers_required_prefixed_jsonl_aggregation_loop() {
        let program = lower_source(
            r#"
user_amounts = {}
user_items = {}
tag_counts = {}
record_count = 0
rows = read_jsonl("records.jsonl")
for row in rows:
    user = row["user"]
    amount = float(row["amount"])
    items = int(row["items"])
    if user in user_amounts:
        user_amounts[user] += amount
        user_items[user] += items
    else:
        user_amounts[user] = amount
        user_items[user] = items
    record_count += 1
    for tag in row["tags"]:
        if tag in tag_counts:
            tag_counts[tag] += 1
        else:
            tag_counts[tag] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[5]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::MaterializedList);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        assert_eq!(body.user_amounts_map, "user_amounts");
        assert_eq!(body.user_items_map, "user_items");
        assert_eq!(body.tag_counts_map, "tag_counts");
        assert_eq!(body.user_amount_key, "amount");
        assert_eq!(body.user_items_key, "items");
        assert_eq!(body.tags_key, "tags");
        assert_eq!(body.row_count_local.as_deref(), Some("record_count"));
    }

    #[test]
    fn classifies_outer_jsonl_file_loop_compile_miss() {
        let program = lower_source(
            r#"
files = find(".", "records_*.jsonl")
user_amounts = {}
user_items = {}
tag_counts = {}
for f in files:
    rows = read_jsonl(f.path)
    for row in rows:
        user = row["user"]
        if user not in user_amounts:
            user_amounts[user] = 0.0
            user_items[user] = 0
        user_amounts[user] += float(row["amount"])
        user_items[user] += int(row["items"])
        for tag in row["tags"]:
            if tag not in tag_counts:
                tag_counts[tag] = 0
            tag_counts[tag] += 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[4]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert!(compile_generic_vm_function(&plan).is_none());
        assert_eq!(
            generic_loop_compile_miss_reason(&plan),
            "outer_jsonl_file_loop"
        );
    }

    #[test]
    fn lowers_model_style_jsonl_count_aggregation_over_named_rows() {
        let program = lower_source(
            r#"
user_amounts = {}
user_items = {}
tag_counts = {}
rows = read_jsonl("records.jsonl")
for row in rows:
    user = row.get("user", "")
    amount = float(row.get("amount", 0))
    tags = row.get("tags", [])

    if user in user_amounts:
        user_amounts[user] += amount
        user_items[user] += 1
    else:
        user_amounts[user] = amount
        user_items[user] = 1

    for tag in tags:
        if tag in tag_counts:
            tag_counts[tag] += 1
        else:
            tag_counts[tag] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[4]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::MaterializedList);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        assert_eq!(body.user_items_default, 1);
        assert_eq!(body.user_items_key, "");
    }

    #[test]
    fn lowers_model_style_jsonl_count_aggregation_over_named_splitlines() {
        let program = lower_source(
            r#"
user_amounts = {}
user_items = {}
tag_counts = {}
lines = open("records.jsonl").splitlines()
for line in lines:
    if line.strip() == "":
        continue
    record = json_loads(line)
    user = record.get("user", "")
    amount = float(record.get("amount", 0))
    tags = record.get("tags", [])

    if user in user_amounts:
        user_amounts[user] = user_amounts[user] + amount
        user_items[user] = user_items[user] + 1
    else:
        user_amounts[user] = amount
        user_items[user] = 1

    for tag in tags:
        tag_str = str(tag)
        if tag_str in tag_counts:
            tag_counts[tag_str] = tag_counts[tag_str] + 1
        else:
            tag_counts[tag_str] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[4]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::MaterializedList);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        assert_eq!(body.user_items_default, 1);
        assert_eq!(body.tag_counts_map, "tag_counts");
    }

    #[test]
    fn lowers_generic_open_splitlines_json_loads_aggregation_loop() {
        let program = lower_source(
            r#"
amounts = {}
items = {}
users = []
tag_counts = {}
tags_seen = []
for line in open("records.jsonl").splitlines():
    if line.strip() == "":
        continue
    record = json_loads(line)
    user = record.get("user", "unknown")
    amount = float(record.get("amount", 0.0))
    item_count = int(record.get("items", 0))
    tags = record.get("tags", [])
    if user in amounts:
        amounts[user] = amounts[user] + amount
        items[user] = items[user] + item_count
    else:
        amounts[user] = amount
        items[user] = item_count
        users.append(user)
    for tag in tags:
        if tag in tag_counts:
            tag_counts[tag] = tag_counts[tag] + 1
        else:
            tag_counts[tag] = 1
            tags_seen.append(tag)
"#,
        )
        .expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[5]
        else {
            panic!("expected for loop");
        };
        let plan = try_lower_generic_loop(targets, iter, body).expect("generic loop plan");
        assert_eq!(plan.iter, GenericLoopIter::OpenSplitlines);
        let [GenericLoopOp::JsonlAggregation { body }] = plan.ops.as_slice() else {
            panic!("expected generic JSONL aggregation op");
        };
        assert_eq!(body.user_name, "user");
        assert_eq!(body.user_amounts_map, "amounts");
        assert_eq!(body.user_items_map, "items");
        assert_eq!(body.tag_counts_map, "tag_counts");
        let vm = compile_hot_jsonl_loop_ir_function(&plan).expect("jsonl text-lines loop IR");
        assert_eq!(
            vm.adapter,
            Some(LoopIrIteratorAdapter::JsonlRows { guarded: true })
        );
    }

    #[test]
    fn lowers_renamed_direct_jsonl_aggregation_body() {
        let program = lower_source(
            r#"
for row in read_jsonl("records.jsonl"):
    customer_id = row["customer_id"]
    if customer_id in customer_revenue:
        customer_revenue[customer_id] = customer_revenue[customer_id] + row["revenue"]
        customer_units[customer_id] = customer_units[customer_id] + row["units"]
    else:
        customer_revenue[customer_id] = row["revenue"]
        customer_units[customer_id] = row["units"]
        customers.append(customer_id)
    for label in row["labels"]:
        if label in label_counts:
            label_counts[label] = label_counts[label] + 1
        else:
            label_counts[label] = 1
            labels.append(label)
"#,
        )
        .expect("lower source");
        let Stmt::For { body, .. } = &program.statements[0] else {
            panic!("expected for loop");
        };
        let plan =
            match_hot_jsonl_aggregation_body("row", &body[1..]).expect("renamed body should lower");
        assert_eq!(plan.user_name, "customer_id");
        assert_eq!(plan.user_key, "customer_id");
        assert!(!plan.user_has_default);
        assert_eq!(plan.user_default, "");
        assert_eq!(plan.user_amounts_map, "customer_revenue");
        assert_eq!(plan.user_amount_key, "revenue");
        assert!(!plan.user_amount_has_default);
        assert_eq!(plan.user_amount_default, 0.0);
        assert_eq!(plan.user_items_map, "customer_units");
        assert_eq!(plan.user_items_key, "units");
        assert!(!plan.user_items_has_default);
        assert_eq!(plan.user_items_default, 0);
        assert_eq!(plan.users_list.as_deref(), Some("customers"));
        assert_eq!(plan.tags_key, "labels");
        assert!(!plan.tags_default_empty);
        assert_eq!(plan.tag_counts_map, "label_counts");
        assert_eq!(plan.tags_list.as_deref(), Some("labels"));
        let trace = compile_hot_jsonl_trace_plan(&plan).expect("trace plan should compile");
        assert_eq!(trace.user_name, "customer_id");
        assert_eq!(trace.user_key, "customer_id");
        assert!(!trace.user_has_default);
        assert_eq!(trace.user_amounts_map, "customer_revenue");
        assert_eq!(trace.user_amount_key, "revenue");
        assert!(!trace.user_amount_has_default);
        assert_eq!(trace.user_items_map, "customer_units");
        assert_eq!(trace.user_items_key, "units");
        assert!(!trace.user_items_has_default);
        assert_eq!(trace.users_list.as_deref(), Some("customers"));
        assert_eq!(trace.tags_key, "labels");
        assert!(!trace.tags_default_empty);
        assert_eq!(trace.tag_counts_map, "label_counts");
        assert_eq!(trace.tags_list.as_deref(), Some("labels"));
        let vm = compile_hot_jsonl_vm_function(&plan).expect("VM function should compile");
        assert_eq!(vm.registers, 6);
        assert_eq!(vm.entry, BlockId(0));
        assert_eq!(vm.blocks.len(), 3);
        assert_eq!(
            vm.constants,
            vec![
                StoneConst::String("customer_id".to_owned()),
                StoneConst::String(String::new()),
                StoneConst::String("revenue".to_owned()),
                StoneConst::String("units".to_owned()),
                StoneConst::String("labels".to_owned()),
                StoneConst::EmptyList,
            ]
        );
        assert_eq!(
            vm.accumulators,
            vec![
                StoneAccumulatorSpec {
                    name: "customer_revenue".to_owned(),
                    kind: StoneAccumulatorKind::F64Map,
                },
                StoneAccumulatorSpec {
                    name: "customer_units".to_owned(),
                    kind: StoneAccumulatorKind::I64Map,
                },
                StoneAccumulatorSpec {
                    name: "customers".to_owned(),
                    kind: StoneAccumulatorKind::StringList,
                },
                StoneAccumulatorSpec {
                    name: "label_counts".to_owned(),
                    kind: StoneAccumulatorKind::I64Map,
                },
                StoneAccumulatorSpec {
                    name: "labels".to_owned(),
                    kind: StoneAccumulatorKind::StringList,
                },
            ]
        );
        assert_eq!(
            vm.guards,
            vec![
                StoneGuard {
                    kind: StoneGuardKind::InputIsJsonObject { reg: Reg(0) },
                    snapshot: SnapshotId(0),
                },
                StoneGuard {
                    kind: StoneGuardKind::AccumulatorShape {
                        acc: AccId(0),
                        kind: StoneAccumulatorKind::F64Map,
                    },
                    snapshot: SnapshotId(0),
                },
                StoneGuard {
                    kind: StoneGuardKind::AccumulatorShape {
                        acc: AccId(1),
                        kind: StoneAccumulatorKind::I64Map,
                    },
                    snapshot: SnapshotId(0),
                },
                StoneGuard {
                    kind: StoneGuardKind::AccumulatorShape {
                        acc: AccId(2),
                        kind: StoneAccumulatorKind::StringList,
                    },
                    snapshot: SnapshotId(0),
                },
                StoneGuard {
                    kind: StoneGuardKind::AccumulatorShape {
                        acc: AccId(3),
                        kind: StoneAccumulatorKind::I64Map,
                    },
                    snapshot: SnapshotId(0),
                },
                StoneGuard {
                    kind: StoneGuardKind::AccumulatorShape {
                        acc: AccId(4),
                        kind: StoneAccumulatorKind::StringList,
                    },
                    snapshot: SnapshotId(0),
                },
            ]
        );
        assert_eq!(
            vm.snapshots,
            vec![StoneSnapshot {
                locals: vec![StoneSnapshotLocal {
                    local: LocalId(0),
                    reg: Reg(1),
                }],
                accumulators: vec![
                    StoneSnapshotAccumulator {
                        local_name: "customer_revenue".to_owned(),
                        acc: AccId(0),
                    },
                    StoneSnapshotAccumulator {
                        local_name: "customer_units".to_owned(),
                        acc: AccId(1),
                    },
                    StoneSnapshotAccumulator {
                        local_name: "customers".to_owned(),
                        acc: AccId(2),
                    },
                    StoneSnapshotAccumulator {
                        local_name: "label_counts".to_owned(),
                        acc: AccId(3),
                    },
                    StoneSnapshotAccumulator {
                        local_name: "labels".to_owned(),
                        acc: AccId(4),
                    },
                ],
                resume: StoneFallbackTarget::LoopBody,
            }]
        );
        assert!(matches!(
            vm.blocks[0].ops[1],
            StoneOp::JsonGetF64Required {
                dst: Reg(2),
                object: Reg(0),
                key: ConstId(2),
            }
        ));
        assert!(matches!(
            vm.blocks[0].ops[2],
            StoneOp::JsonGetI64Required {
                dst: Reg(3),
                object: Reg(0),
                key: ConstId(3),
            }
        ));
        assert!(matches!(
            vm.blocks[0].ops[3],
            StoneOp::JsonGetArrayRequired {
                dst: Reg(4),
                object: Reg(0),
                key: ConstId(4),
            }
        ));
        assert!(matches!(
            vm.blocks[0].terminator,
            StoneTerminator::JsonEachStrArray {
                array: Reg(4),
                item: Reg(5),
                body: BlockId(1),
                done: BlockId(2),
            }
        ));
        assert_eq!(
            select_hot_jsonl_fused_kernel_from_ir(&vm),
            Some(LoopIrFusedKernel::JsonlAggregation)
        );
        assert_eq!(
            match_hot_jsonl_ir_subgraph(&vm),
            Some(LoopIrSubgraphKind::JsonlAggregation)
        );
        assert_eq!(
            plan.ops,
            vec![
                HotJsonlBodyOp::JsonGetFields {
                    user_key: "customer_id".to_owned(),
                    amount_key: "revenue".to_owned(),
                    items_key: "units".to_owned(),
                    tags_key: "labels".to_owned(),
                },
                HotJsonlBodyOp::MapAddF64 {
                    map_name: "customer_revenue".to_owned(),
                    key_slot: HotJsonlSlot::User,
                    value_slot: HotJsonlSlot::Amount,
                    append_list: Some("customers".to_owned()),
                },
                HotJsonlBodyOp::MapAddI64 {
                    map_name: "customer_units".to_owned(),
                    key_slot: HotJsonlSlot::User,
                    value_slot: HotJsonlSlot::Items,
                },
                HotJsonlBodyOp::ForEachJsonString {
                    array_slot: HotJsonlSlot::Tags,
                    item_slot: HotJsonlSlot::Tag,
                    body: vec![HotJsonlBodyOp::MapAddI64Const {
                        map_name: "label_counts".to_owned(),
                        key_slot: HotJsonlSlot::Tag,
                        value: 1,
                        append_list: Some("labels".to_owned()),
                    }],
                },
            ]
        );
    }

    #[test]
    fn jsonl_fused_selection_requires_named_ir_subgraph() {
        let vm = required_jsonl_aggregation_vm();
        let optimized = optimize_stone_loop_ir(&vm);
        assert_eq!(optimized.function, vm);
        assert_eq!(optimized.diagnostics, Vec::new());
        assert_eq!(
            optimized.matched_subgraph,
            Some(LoopIrSubgraphKind::JsonlAggregation)
        );
        assert_eq!(
            optimized.selected_kernel,
            Some(LoopIrFusedKernel::JsonlAggregation)
        );
        assert_eq!(
            match_hot_jsonl_ir_subgraph(&vm),
            Some(LoopIrSubgraphKind::JsonlAggregation)
        );
        assert_eq!(
            select_hot_jsonl_fused_kernel_from_ir(&vm),
            Some(LoopIrFusedKernel::JsonlAggregation)
        );
        assert!(compile_hot_jsonl_trace_plan_from_ir(&vm).is_some());
    }

    #[test]
    fn jsonl_fused_selection_rejects_perturbed_ir_op() {
        let mut vm = required_jsonl_aggregation_vm();
        vm.blocks[0].ops[5] = StoneOp::MapAddI64 {
            map: AccId(1),
            key: Reg(1),
            value: Reg(2),
            append: None,
        };
        assert_eq!(vm.blocks.len(), 3);
        assert_eq!(match_hot_jsonl_ir_subgraph(&vm), None);
        assert_eq!(select_hot_jsonl_fused_kernel_from_ir(&vm), None);
        let optimized = optimize_stone_loop_ir(&vm);
        assert_eq!(optimized.function, vm);
        assert_eq!(optimized.matched_subgraph, None);
        assert_eq!(optimized.selected_kernel, None);
        assert_eq!(optimized.diagnostics, Vec::new());
    }

    #[test]
    fn jsonl_fused_selection_rejects_perturbed_ir_terminator() {
        let mut vm = required_jsonl_aggregation_vm();
        vm.blocks[1].terminator = StoneTerminator::Return;
        assert_eq!(vm.blocks.len(), 3);
        assert_eq!(match_hot_jsonl_ir_subgraph(&vm), None);
        assert_eq!(select_hot_jsonl_fused_kernel_from_ir(&vm), None);
        let optimized = optimize_stone_loop_ir(&vm);
        assert_eq!(optimized.function, vm);
        assert_eq!(optimized.matched_subgraph, None);
        assert_eq!(optimized.selected_kernel, None);
        assert_eq!(optimized.diagnostics, Vec::new());
    }

    #[test]
    fn lowers_prefixed_jsonl_aggregation_body() {
        let program = lower_source(
            r#"
for row in read_jsonl("records.jsonl"):
    customer = row.get("customer", "unknown")
    revenue = float(row.get("revenue", 0.0))
    units = int(row.get("units", 0))
    labels = row.get("labels", [])
    if customer in revenue_by_customer:
        revenue_by_customer[customer] += revenue
        units_by_customer[customer] += units
    else:
        revenue_by_customer[customer] = revenue
        units_by_customer[customer] = units
    for label in labels:
        if label in label_counts:
            label_counts[label] += 1
        else:
            label_counts[label] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For { body, .. } = &program.statements[0] else {
            panic!("expected for loop");
        };
        let plan = match_hot_jsonl_aggregation_body("row", body)
            .expect("prefixed controlled-style body should lower");
        assert_eq!(plan.user_name, "customer");
        assert_eq!(plan.user_key, "customer");
        assert!(plan.user_has_default);
        assert_eq!(plan.user_default, "unknown");
        assert_eq!(plan.user_amounts_map, "revenue_by_customer");
        assert_eq!(plan.user_amount_key, "revenue");
        assert!(plan.user_amount_has_default);
        assert_eq!(plan.user_amount_default, 0.0);
        assert_eq!(plan.user_items_map, "units_by_customer");
        assert_eq!(plan.user_items_key, "units");
        assert!(plan.user_items_has_default);
        assert_eq!(plan.user_items_default, 0);
        assert_eq!(plan.users_list, None);
        assert_eq!(plan.tags_key, "labels");
        assert!(plan.tags_default_empty);
        assert_eq!(plan.tag_counts_map, "label_counts");
        assert_eq!(plan.tags_list, None);
        let trace = compile_hot_jsonl_trace_plan(&plan).expect("trace plan should compile");
        assert_eq!(trace.user_name, "customer");
        assert_eq!(trace.user_key, "customer");
        assert!(trace.user_has_default);
        assert_eq!(trace.user_default, "unknown");
        assert_eq!(trace.user_amount_key, "revenue");
        assert!(trace.user_amount_has_default);
        assert_eq!(trace.user_amount_default, 0.0);
        assert_eq!(trace.user_items_key, "units");
        assert!(trace.user_items_has_default);
        assert_eq!(trace.user_items_default, 0);
        assert_eq!(trace.tags_key, "labels");
        assert!(trace.tags_default_empty);
        let vm = compile_hot_jsonl_vm_function(&plan).expect("VM function should compile");
        assert_eq!(vm.registers, 6);
        assert_eq!(vm.blocks.len(), 3);
        assert!(matches!(
            vm.blocks[0].ops[0],
            StoneOp::JsonGetStrDefault {
                dst: Reg(1),
                object: Reg(0),
                key: ConstId(0),
                default: ConstId(1),
            }
        ));
        assert!(matches!(
            vm.blocks[0].ops[1],
            StoneOp::JsonGetF64Default {
                dst: Reg(2),
                object: Reg(0),
                key: ConstId(2),
                default: 0.0,
            }
        ));
        assert!(matches!(
            vm.blocks[0].ops[2],
            StoneOp::JsonGetI64Default {
                dst: Reg(3),
                object: Reg(0),
                key: ConstId(3),
                default: 0,
            }
        ));
        assert!(matches!(
            vm.blocks[0].ops[3],
            StoneOp::JsonGetArrayDefault {
                dst: Reg(4),
                object: Reg(0),
                key: ConstId(4),
            }
        ));
        assert!(matches!(
            vm.blocks[0].ops[4],
            StoneOp::MapAddF64 {
                map: AccId(0),
                key: Reg(1),
                value: Reg(2),
                append: None,
            }
        ));
        assert_eq!(plan.ops.len(), 4);
        assert!(matches!(
            plan.ops[3],
            HotJsonlBodyOp::ForEachJsonString { .. }
        ));
    }

    #[test]
    fn lowers_prefixed_jsonl_aggregation_body_with_late_tags() {
        let program = lower_source(
            r#"
for row in read_jsonl("records.jsonl"):
    customer = row.get("customer", "unknown")
    revenue = float(row.get("revenue", 0.0))
    units = int(row.get("units", 0))
    if customer in revenue_by_customer:
        revenue_by_customer[customer] += revenue
        units_by_customer[customer] += units
    else:
        revenue_by_customer[customer] = revenue
        units_by_customer[customer] = units
    labels = row.get("labels", [])
    for label in labels:
        if label in label_counts:
            label_counts[label] += 1
        else:
            label_counts[label] = 1
"#,
        )
        .expect("lower source");
        let Stmt::For { body, .. } = &program.statements[0] else {
            panic!("expected for loop");
        };
        let plan = match_hot_jsonl_aggregation_body("row", body)
            .expect("late-tags controlled-style body should lower");
        assert_eq!(plan.user_key, "customer");
        assert_eq!(plan.user_amount_key, "revenue");
        assert_eq!(plan.user_items_key, "units");
        assert_eq!(plan.tags_key, "labels");
        assert_eq!(plan.tag_counts_map, "label_counts");
    }

    fn lower_first_generic_loop(source: &str, index: usize) -> GenericLoopPlan {
        let program = lower_source(source).expect("lower source");
        let Stmt::For {
            targets,
            iter,
            body,
        } = &program.statements[index]
        else {
            panic!("expected for loop");
        };
        try_lower_generic_loop(targets, iter, body).expect("generic loop plan")
    }
}
