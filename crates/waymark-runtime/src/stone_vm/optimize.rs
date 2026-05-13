// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{
    AccId, BlockId, ConstId, GenericVmOp, LoopIrFunction, LoopIrFusedKernel,
    LoopIrOptimizationDiagnostic, LoopIrOptimizationResult, LoopIrSubgraphKind, LoopIrTerminator,
    Reg, StoneIrFunction, StoneLoopIrOptimizationResult, StoneOp, StoneTerminator,
};

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HotJsonlAggregationIrSubgraph {
    pub(crate) user_has_default: bool,
    pub(crate) user_amount_has_default: bool,
    pub(crate) user_amount_default: f64,
    pub(crate) user_items_has_default: bool,
    pub(crate) user_items_default: i64,
    pub(crate) tags_default_empty: bool,
    pub(crate) users_append: Option<AccId>,
    pub(crate) tags_append: Option<AccId>,
}

pub(crate) fn match_hot_jsonl_aggregation_ir_subgraph(
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
