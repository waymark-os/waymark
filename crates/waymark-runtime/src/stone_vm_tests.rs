#[cfg(test)]
mod tests {
    use crate::stone_ast::{lower_source, Stmt};
    use crate::stone_vm::{
        compile_generic_vm_function, compile_hot_jsonl_loop_ir_function,
        compile_hot_jsonl_trace_plan, compile_hot_jsonl_trace_plan_from_ir,
        compile_hot_jsonl_vm_function, generic_loop_compile_miss_reason, hot_jsonl_aggregation_ops,
        match_hot_jsonl_aggregation_body, match_hot_jsonl_ir_subgraph, match_loop_ir_subgraph,
        optimize_loop_ir, optimize_stone_loop_ir, select_hot_jsonl_fused_kernel_from_ir,
        select_loop_ir_fused_kernel, try_lower_generic_loop, try_lower_hot_loop, AccId, BlockId,
        ConstId, GenericLoopIter, GenericLoopOp, GenericLoopPlan, GenericParseNumber,
        GenericVmExprOp, GenericVmOp, HotJsonlAggregationBody, HotJsonlBodyOp, HotJsonlSlot,
        HotLoopIter, HotLoopOp, LocalId, LoopIrBlock, LoopIrDiagnostics, LoopIrFusedKernel,
        LoopIrIteratorAdapter, LoopIrOptimizationDiagnostic, LoopIrOptimizationResult,
        LoopIrSnapshot, LoopIrSnapshotBoundary, LoopIrSubgraphKind, LoopIrTerminator, Reg,
        SnapshotId, StoneAccumulatorKind, StoneAccumulatorSpec, StoneConst, StoneFallbackTarget,
        StoneGuard, StoneGuardKind, StoneIrFunction, StoneOp, StoneSnapshot,
        StoneSnapshotAccumulator, StoneSnapshotLocal, StoneTerminator,
    };

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
    total = total + (-n % 4)
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
            .any(|op| matches!(op, GenericVmExprOp::NegI64 { .. })));
        assert!(body
            .ops
            .iter()
            .any(|op| matches!(op, GenericVmExprOp::ModI64 { .. })));
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
