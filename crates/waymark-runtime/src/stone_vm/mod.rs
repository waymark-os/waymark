// SPDX-License-Identifier: MIT OR Apache-2.0

mod optimize;
mod types;

pub(crate) use crate::stone_ir::{
    compile_generic_vm_function, compile_hot_jsonl_loop_ir_function, compile_hot_jsonl_trace_plan,
    compile_hot_jsonl_trace_plan_from_ir, compile_hot_jsonl_vm_function,
    generic_loop_compile_miss_reason, match_hot_jsonl_aggregation_body,
    match_outer_jsonl_file_loop_body, optimize_stone_loop_ir, try_lower_generic_loop,
    try_lower_hot_loop, validate_hot_jsonl_native_prefix, GenericLoopOp, GenericLoopPlan,
    HotLoopIter, HotLoopOp, HotLoopPlan,
};
pub(crate) use optimize::optimize_loop_ir;
#[cfg(test)]
pub(crate) use optimize::{match_loop_ir_subgraph, select_loop_ir_fused_kernel};
pub(crate) use types::{
    AccId, BlockId, ConstId, GenericLoopIter, GenericParseNumber, GenericVmConst,
    GenericVmExprBody, GenericVmExprOp, GenericVmFunction, GenericVmOp, HotJsonlAggregationBody,
    HotJsonlBodyOp, HotJsonlNestedUserTotals, HotJsonlSlot, HotJsonlTracePlan, LocalId,
    LoopIrBlock, LoopIrDiagnostics, LoopIrFunction, LoopIrFusedKernel, LoopIrIteratorAdapter,
    LoopIrOptimizationDiagnostic, LoopIrOptimizationResult, LoopIrSnapshot, LoopIrSnapshotBoundary,
    LoopIrSubgraphKind, LoopIrTerminator, Reg, SnapshotId, StoneAccumulatorKind,
    StoneAccumulatorSpec, StoneBlock, StoneConst, StoneFallbackTarget, StoneGuard, StoneGuardKind,
    StoneIrFunction, StoneLocal, StoneLoopIrOptimizationResult, StoneOp, StoneSnapshot,
    StoneSnapshotAccumulator, StoneSnapshotLocal, StoneTerminator,
};
