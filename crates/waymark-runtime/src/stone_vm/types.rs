// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::stone_ast::Stmt;

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

impl LoopIrFunction {
    pub(crate) fn local_name(&self, local: usize) -> Option<&str> {
        self.locals.get(local).map(String::as_str)
    }
}

pub(crate) type GenericVmFunction = LoopIrFunction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenericLoopIter {
    MaterializedList,
    OpenSplitlines,
    Range,
    ReadJsonl,
    ReadCsv,
}

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
    ModI64 { dst: usize, lhs: usize, rhs: usize },
    BitAndI64 { dst: usize, lhs: usize, rhs: usize },
    BitOrI64 { dst: usize, lhs: usize, rhs: usize },
    BitXorI64 { dst: usize, lhs: usize, rhs: usize },
    ShlI64 { dst: usize, lhs: usize, rhs: usize },
    ShrI64 { dst: usize, lhs: usize, rhs: usize },
    NegI64 { dst: usize, src: usize },
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
            GenericVmExprOp::ModI64 { .. } => 114,
            GenericVmExprOp::NegI64 { .. } => 115,
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

#[cfg(test)]
mod tests {
    use super::{
        AccId, ConstId, GenericParseNumber, GenericVmExprBody, GenericVmExprOp, GenericVmOp,
        LoopIrFusedKernel, Reg, StoneOp,
    };

    #[test]
    fn generic_vm_opcode_ids_are_stable() {
        let body = GenericVmExprBody {
            registers: 0,
            constants: Vec::new(),
            ops: Vec::new(),
        };

        assert_eq!(GenericVmOp::AddAssign { local: 0 }.opcode_id(), 1);
        assert_eq!(
            GenericVmOp::AddAssignParsed {
                local: 0,
                parse: GenericParseNumber::Int,
            }
            .opcode_id(),
            2
        );
        assert_eq!(
            GenericVmOp::MapAddI64Const { map: 0, addend: 1 }.opcode_id(),
            3
        );
        assert_eq!(
            GenericVmOp::MapAddI64ConstRecordField {
                map: 0,
                field: "field".to_string(),
                addend: 1,
            }
            .opcode_id(),
            4
        );
        assert_eq!(
            GenericVmOp::MapAddI64ConstRecordStringField {
                map: 0,
                field: "field".to_string(),
                strip: true,
                lower: true,
                addend: 1,
            }
            .opcode_id(),
            5
        );
        assert_eq!(
            GenericVmOp::ListAppend {
                list: 0,
                unique: false,
            }
            .opcode_id(),
            6
        );
        assert_eq!(GenericVmOp::ExprBody(body).opcode_id(), 7);
    }

    #[test]
    fn generic_expr_opcode_ids_are_stable() {
        let binary_ops = [
            (
                GenericVmExprOp::AddI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                104,
            ),
            (
                GenericVmExprOp::SubI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                105,
            ),
            (
                GenericVmExprOp::MulI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                106,
            ),
            (
                GenericVmExprOp::FloorDivI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                107,
            ),
            (
                GenericVmExprOp::BitAndI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                108,
            ),
            (
                GenericVmExprOp::BitOrI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                109,
            ),
            (
                GenericVmExprOp::BitXorI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                110,
            ),
            (
                GenericVmExprOp::ShlI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                111,
            ),
            (
                GenericVmExprOp::ShrI64 {
                    dst: 0,
                    lhs: 1,
                    rhs: 2,
                },
                112,
            ),
        ];

        assert_eq!(
            GenericVmExprOp::LoadLocal { dst: 0, local: 1 }.opcode_id(),
            101
        );
        assert_eq!(
            GenericVmExprOp::StoreLocal { local: 0, src: 1 }.opcode_id(),
            102
        );
        assert_eq!(
            GenericVmExprOp::LoadConst {
                dst: 0,
                constant: 1,
            }
            .opcode_id(),
            103
        );
        for (op, id) in binary_ops {
            assert_eq!(op.opcode_id(), id);
        }
        assert_eq!(
            GenericVmExprOp::BitNotI64 { dst: 0, src: 1 }.opcode_id(),
            113
        );
        assert_eq!(
            GenericVmExprOp::ModI64 {
                dst: 0,
                lhs: 1,
                rhs: 2,
            }
            .opcode_id(),
            114
        );
        assert_eq!(GenericVmExprOp::NegI64 { dst: 0, src: 1 }.opcode_id(), 115);
    }

    #[test]
    fn fused_kernel_type_assumptions_are_stable() {
        let map = LoopIrFusedKernel::MapAddI64Const.type_assumptions();
        assert_eq!(map.inputs, ["string_key", "i64_addend", "i64_map"]);
        assert_eq!(map.outputs, ["i64_map"]);

        let list = LoopIrFusedKernel::ListAppend.type_assumptions();
        assert_eq!(list.inputs, ["list", "item"]);
        assert_eq!(list.outputs, ["list"]);

        let jsonl = LoopIrFusedKernel::JsonlAggregation.type_assumptions();
        assert_eq!(
            jsonl.inputs,
            ["json_object_row", "f64_map", "i64_map", "string_list"]
        );
        assert_eq!(jsonl.outputs, ["f64_map", "i64_map", "string_list"]);
    }

    #[test]
    fn stone_opcode_ids_are_stable() {
        let object = Reg(0);
        let dst = Reg(1);
        let key = ConstId(0);
        let default = ConstId(1);
        let map = AccId(0);

        assert_eq!(
            StoneOp::JsonGetStrDefault {
                dst,
                object,
                key,
                default,
            }
            .opcode_id(),
            201
        );
        assert_eq!(StoneOp::JsonGetValue { dst, object, key }.opcode_id(), 202);
        assert_eq!(
            StoneOp::JsonGetF64Default {
                dst,
                object,
                key,
                default: 0.0,
            }
            .opcode_id(),
            203
        );
        assert_eq!(
            StoneOp::JsonGetI64Default {
                dst,
                object,
                key,
                default: 0,
            }
            .opcode_id(),
            204
        );
        assert_eq!(
            StoneOp::JsonGetArrayDefault { dst, object, key }.opcode_id(),
            205
        );
        assert_eq!(
            StoneOp::MapAddF64 {
                map,
                key: dst,
                value: object,
                append: None,
            }
            .opcode_id(),
            206
        );
        assert_eq!(
            StoneOp::MapAddI64 {
                map,
                key: dst,
                value: object,
                append: None,
            }
            .opcode_id(),
            207
        );
        assert_eq!(
            StoneOp::MapAddI64Const {
                map,
                key: dst,
                value: 1,
                append: Some(map),
            }
            .opcode_id(),
            208
        );
        assert_eq!(
            StoneOp::JsonGetF64Required { dst, object, key }.opcode_id(),
            209
        );
        assert_eq!(
            StoneOp::JsonGetI64Required { dst, object, key }.opcode_id(),
            210
        );
        assert_eq!(
            StoneOp::JsonGetArrayRequired { dst, object, key }.opcode_id(),
            211
        );
    }
}
