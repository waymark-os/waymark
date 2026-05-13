// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::stone_ir::GenericLoopIter;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum LoopIrIteratorAdapter {
    MaterializedValues,
    RangeValues,
    TextLines,
    JsonlRows { guarded: bool },
    CsvRows,
    JsonlFiles { path_field: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopIrFunction {
    pub(crate) iter: GenericLoopIter,
    pub(crate) adapter: LoopIrIteratorAdapter,
    pub(crate) locals: Vec<String>,
    pub(crate) registers: usize,
    pub(crate) constants: Vec<GenericVmConst>,
    pub(crate) blocks: Vec<LoopIrBlock>,
    pub(crate) entry: usize,
    pub(crate) snapshots: Vec<LoopIrSnapshot>,
    pub(crate) diagnostics: LoopIrDiagnostics,
    pub(crate) ops: Vec<GenericVmOp>,
}

pub(crate) type GenericVmFunction = LoopIrFunction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenericVmOp {
    AddAssign {
        local: usize,
    },
    AddAssignParsed {
        local: usize,
        parse: GenericParseNumber,
    },
    MapAddI64Const {
        map: usize,
        addend: i64,
    },
    MapAddI64ConstRecordField {
        map: usize,
        field: String,
        addend: i64,
    },
    MapAddI64ConstRecordStringField {
        map: usize,
        field: String,
        strip: bool,
        lower: bool,
        addend: i64,
    },
    ListAppend {
        list: usize,
        unique: bool,
    },
    ExprBody(GenericVmExprBody),
}

impl GenericVmOp {
    #[allow(dead_code)]
    pub(crate) fn opcode_id(&self) -> u16 {
        match self {
            GenericVmOp::AddAssign { .. } => 1,
            GenericVmOp::AddAssignParsed { .. } => 2,
            GenericVmOp::MapAddI64Const { .. } => 3,
            GenericVmOp::MapAddI64ConstRecordField { .. } => 4,
            GenericVmOp::MapAddI64ConstRecordStringField { .. } => 5,
            GenericVmOp::ListAppend { .. } => 6,
            GenericVmOp::ExprBody(_) => 7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenericParseNumber {
    Int,
    Float,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenericVmExprBody {
    pub(crate) registers: usize,
    pub(crate) constants: Vec<GenericVmConst>,
    pub(crate) ops: Vec<GenericVmExprOp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenericVmConst {
    I64(i64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenericVmExprOp {
    LoadLocal { dst: usize, local: usize },
    StoreLocal { local: usize, src: usize },
    LoadConst { dst: usize, constant: usize },
    AddI64 { dst: usize, lhs: usize, rhs: usize },
    SubI64 { dst: usize, lhs: usize, rhs: usize },
    MulI64 { dst: usize, lhs: usize, rhs: usize },
    FloorDivI64 { dst: usize, lhs: usize, rhs: usize },
    BitAndI64 { dst: usize, lhs: usize, rhs: usize },
    BitOrI64 { dst: usize, lhs: usize, rhs: usize },
    BitXorI64 { dst: usize, lhs: usize, rhs: usize },
    ShlI64 { dst: usize, lhs: usize, rhs: usize },
    ShrI64 { dst: usize, lhs: usize, rhs: usize },
    BitNotI64 { dst: usize, src: usize },
}

impl GenericVmExprOp {
    #[allow(dead_code)]
    pub(crate) fn opcode_id(&self) -> u16 {
        match self {
            GenericVmExprOp::LoadLocal { .. } => 101,
            GenericVmExprOp::StoreLocal { .. } => 102,
            GenericVmExprOp::LoadConst { .. } => 103,
            GenericVmExprOp::AddI64 { .. } => 104,
            GenericVmExprOp::SubI64 { .. } => 105,
            GenericVmExprOp::MulI64 { .. } => 106,
            GenericVmExprOp::FloorDivI64 { .. } => 107,
            GenericVmExprOp::BitAndI64 { .. } => 108,
            GenericVmExprOp::BitOrI64 { .. } => 109,
            GenericVmExprOp::BitXorI64 { .. } => 110,
            GenericVmExprOp::ShlI64 { .. } => 111,
            GenericVmExprOp::ShrI64 { .. } => 112,
            GenericVmExprOp::BitNotI64 { .. } => 113,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HotJsonlAggregationBody {
    pub(crate) ops: Vec<HotJsonlBodyOp>,
    pub(crate) nested_user_totals: Option<HotJsonlNestedUserTotals>,
    pub(crate) user_name: String,
    pub(crate) user_key: String,
    pub(crate) user_has_default: bool,
    pub(crate) user_default: String,
    pub(crate) user_amounts_map: String,
    pub(crate) user_amount_key: String,
    pub(crate) user_amount_has_default: bool,
    pub(crate) user_amount_default: f64,
    pub(crate) user_items_map: String,
    pub(crate) user_items_key: String,
    pub(crate) user_items_has_default: bool,
    pub(crate) user_items_default: i64,
    pub(crate) users_list: Option<String>,
    pub(crate) tags_key: String,
    pub(crate) tags_default_empty: bool,
    pub(crate) tag_counts_map: String,
    pub(crate) tags_list: Option<String>,
    pub(crate) row_count_local: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HotJsonlNestedUserTotals {
    pub(crate) map_name: String,
    pub(crate) amount_field: String,
    pub(crate) items_field: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HotJsonlTracePlan {
    pub(crate) user_name: String,
    pub(crate) user_key: String,
    pub(crate) user_has_default: bool,
    pub(crate) user_default: String,
    pub(crate) user_amounts_map: String,
    pub(crate) user_amount_key: String,
    pub(crate) user_amount_has_default: bool,
    pub(crate) user_amount_default: f64,
    pub(crate) user_items_map: String,
    pub(crate) user_items_key: String,
    pub(crate) user_items_has_default: bool,
    pub(crate) user_items_default: i64,
    pub(crate) users_list: Option<String>,
    pub(crate) tags_key: String,
    pub(crate) tags_default_empty: bool,
    pub(crate) tag_counts_map: String,
    pub(crate) tags_list: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HotJsonlBodyOp {
    JsonGetFields {
        user_key: String,
        amount_key: String,
        items_key: String,
        tags_key: String,
    },
    MapAddF64 {
        map_name: String,
        key_slot: HotJsonlSlot,
        value_slot: HotJsonlSlot,
        append_list: Option<String>,
    },
    MapAddI64 {
        map_name: String,
        key_slot: HotJsonlSlot,
        value_slot: HotJsonlSlot,
    },
    ForEachJsonString {
        array_slot: HotJsonlSlot,
        item_slot: HotJsonlSlot,
        body: Vec<HotJsonlBodyOp>,
    },
    MapAddI64Const {
        map_name: String,
        key_slot: HotJsonlSlot,
        value: i64,
        append_list: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HotJsonlSlot {
    User,
    Amount,
    Items,
    Tags,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopIrBlock {
    pub(crate) ops: Vec<GenericVmOp>,
    pub(crate) terminator: LoopIrTerminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopIrTerminator {
    Return,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopIrSnapshot {
    pub(crate) locals: Vec<usize>,
    pub(crate) boundary: LoopIrSnapshotBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoopIrSnapshotBoundary {
    LoopEntry,
    IterationEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopIrDiagnostics {
    pub(crate) lowering_path: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopIrFusedKernel {
    MapAddI64Const,
    ListAppend,
    JsonlAggregation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopIrSubgraphKind {
    MapAddI64Const,
    ListAppend,
    JsonlAggregation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopIrOptimizationResult {
    pub(crate) function: LoopIrFunction,
    pub(crate) selected_kernel: Option<LoopIrFusedKernel>,
    pub(crate) matched_subgraph: Option<LoopIrSubgraphKind>,
    pub(crate) diagnostics: Vec<LoopIrOptimizationDiagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoneLoopIrOptimizationResult {
    pub(crate) function: StoneIrFunction,
    pub(crate) selected_kernel: Option<LoopIrFusedKernel>,
    pub(crate) matched_subgraph: Option<LoopIrSubgraphKind>,
    pub(crate) diagnostics: Vec<LoopIrOptimizationDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum LoopIrOptimizationDiagnostic {
    Canonicalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoopIrFusedKernelAssumptions {
    pub(crate) inputs: &'static [&'static str],
    pub(crate) outputs: &'static [&'static str],
}

impl LoopIrFusedKernel {
    #[allow(dead_code)]
    pub(crate) fn type_assumptions(self) -> LoopIrFusedKernelAssumptions {
        match self {
            LoopIrFusedKernel::MapAddI64Const => LoopIrFusedKernelAssumptions {
                inputs: &["string_key", "i64_addend", "i64_map"],
                outputs: &["i64_map"],
            },
            LoopIrFusedKernel::ListAppend => LoopIrFusedKernelAssumptions {
                inputs: &["list", "item"],
                outputs: &["list"],
            },
            LoopIrFusedKernel::JsonlAggregation => LoopIrFusedKernelAssumptions {
                inputs: &["json_object_row", "f64_map", "i64_map", "string_list"],
                outputs: &["f64_map", "i64_map", "string_list"],
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Reg(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ConstId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) struct LocalId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AccId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BlockId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SnapshotId(pub(crate) u32);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoneIrFunction {
    pub(crate) adapter: Option<LoopIrIteratorAdapter>,
    pub(crate) registers: u32,
    pub(crate) constants: Vec<StoneConst>,
    pub(crate) locals: Vec<StoneLocal>,
    pub(crate) accumulators: Vec<StoneAccumulatorSpec>,
    pub(crate) guards: Vec<StoneGuard>,
    pub(crate) snapshots: Vec<StoneSnapshot>,
    pub(crate) blocks: Vec<StoneBlock>,
    pub(crate) entry: BlockId,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum StoneConst {
    String(String),
    F64(f64),
    I64(i64),
    EmptyList,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoneLocal {
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoneAccumulatorSpec {
    pub(crate) name: String,
    pub(crate) kind: StoneAccumulatorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoneAccumulatorKind {
    F64Map,
    I64Map,
    StringList,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoneGuard {
    pub(crate) kind: StoneGuardKind,
    pub(crate) snapshot: SnapshotId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoneGuardKind {
    InputIsJsonObject {
        reg: Reg,
    },
    AccumulatorShape {
        acc: AccId,
        kind: StoneAccumulatorKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoneSnapshot {
    pub(crate) locals: Vec<StoneSnapshotLocal>,
    pub(crate) accumulators: Vec<StoneSnapshotAccumulator>,
    pub(crate) resume: StoneFallbackTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoneSnapshotLocal {
    pub(crate) local: LocalId,
    pub(crate) reg: Reg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoneSnapshotAccumulator {
    pub(crate) local_name: String,
    pub(crate) acc: AccId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoneFallbackTarget {
    LoopBody,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoneBlock {
    pub(crate) ops: Vec<StoneOp>,
    pub(crate) terminator: StoneTerminator,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StoneOp {
    JsonGetStrDefault {
        dst: Reg,
        object: Reg,
        key: ConstId,
        default: ConstId,
    },
    JsonGetValue {
        dst: Reg,
        object: Reg,
        key: ConstId,
    },
    JsonGetF64Default {
        dst: Reg,
        object: Reg,
        key: ConstId,
        default: f64,
    },
    JsonGetF64Required {
        dst: Reg,
        object: Reg,
        key: ConstId,
    },
    JsonGetI64Default {
        dst: Reg,
        object: Reg,
        key: ConstId,
        default: i64,
    },
    JsonGetI64Required {
        dst: Reg,
        object: Reg,
        key: ConstId,
    },
    JsonGetArrayDefault {
        dst: Reg,
        object: Reg,
        key: ConstId,
    },
    JsonGetArrayRequired {
        dst: Reg,
        object: Reg,
        key: ConstId,
    },
    MapAddF64 {
        map: AccId,
        key: Reg,
        value: Reg,
        append: Option<AccId>,
    },
    MapAddI64 {
        map: AccId,
        key: Reg,
        value: Reg,
        append: Option<AccId>,
    },
    MapAddI64Const {
        map: AccId,
        key: Reg,
        value: i64,
        append: Option<AccId>,
    },
}

impl StoneOp {
    #[allow(dead_code)]
    pub(crate) fn opcode_id(&self) -> u16 {
        match self {
            StoneOp::JsonGetStrDefault { .. } => 201,
            StoneOp::JsonGetValue { .. } => 202,
            StoneOp::JsonGetF64Default { .. } => 203,
            StoneOp::JsonGetI64Default { .. } => 204,
            StoneOp::JsonGetArrayDefault { .. } => 205,
            StoneOp::MapAddF64 { .. } => 206,
            StoneOp::MapAddI64 { .. } => 207,
            StoneOp::MapAddI64Const { .. } => 208,
            StoneOp::JsonGetF64Required { .. } => 209,
            StoneOp::JsonGetI64Required { .. } => 210,
            StoneOp::JsonGetArrayRequired { .. } => 211,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoneTerminator {
    JsonEachStrArray {
        array: Reg,
        item: Reg,
        body: BlockId,
        done: BlockId,
    },
    Jump {
        target: BlockId,
    },
    Return,
}
