// SPDX-License-Identifier: MIT OR Apache-2.0

use super::{
    GenericVmOp, LoopIrFunction, LoopIrFusedKernel, LoopIrOptimizationDiagnostic,
    LoopIrOptimizationResult, LoopIrSubgraphKind, LoopIrTerminator,
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
