// SPDX-License-Identifier: MIT OR Apache-2.0

mod lower;
mod optimize;
mod types;

pub(crate) use crate::stone_ir::{
    generic_loop_compile_miss_reason, match_hot_jsonl_aggregation_body,
    match_outer_jsonl_file_loop_body, try_lower_generic_loop,
};
pub(crate) use lower::{
    compile_generic_vm_function, compile_hot_jsonl_loop_ir_function, compile_hot_jsonl_trace_plan,
    compile_hot_jsonl_trace_plan_from_ir, compile_hot_jsonl_vm_function,
    lower_json_loads_line_prefix, match_fused_map_update_if, match_insert_assignment,
    match_key_not_in_map, match_map_key_target, match_row_get, match_row_subscript,
    try_lower_hot_loop, validate_hot_jsonl_native_prefix,
};
pub(crate) use optimize::{
    match_hot_jsonl_aggregation_ir_subgraph, optimize_loop_ir, optimize_stone_loop_ir,
};
#[cfg(test)]
pub(crate) use optimize::{
    match_hot_jsonl_ir_subgraph, match_loop_ir_subgraph, select_hot_jsonl_fused_kernel_from_ir,
    select_loop_ir_fused_kernel,
};
pub(crate) use types::{
    AccId, BlockId, ConstId, GenericLoopIter, GenericLoopOp, GenericLoopPlan, GenericParseNumber,
    GenericVmConst, GenericVmExprBody, GenericVmExprOp, GenericVmFunction, GenericVmOp,
    HotJsonlAggregationBody, HotJsonlBodyOp, HotJsonlNestedUserTotals, HotJsonlSlot,
    HotJsonlTracePlan, HotLoopIter, HotLoopOp, HotLoopPlan, JsonlPathExpr, LocalId, LoopIrBlock,
    LoopIrDiagnostics, LoopIrFunction, LoopIrFusedKernel, LoopIrIteratorAdapter,
    LoopIrOptimizationDiagnostic, LoopIrOptimizationResult, LoopIrSnapshot, LoopIrSnapshotBoundary,
    LoopIrSubgraphKind, LoopIrTerminator, OuterJsonlFileLoopBody, Reg, SnapshotId,
    StoneAccumulatorKind, StoneAccumulatorSpec, StoneBlock, StoneConst, StoneFallbackTarget,
    StoneGuard, StoneGuardKind, StoneIrFunction, StoneLocal, StoneLoopIrOptimizationResult,
    StoneOp, StoneSnapshot, StoneSnapshotAccumulator, StoneSnapshotLocal, StoneTerminator,
};
