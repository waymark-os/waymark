// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::stone_ast::{AssignTarget, AugOp, Call, Expr, Stmt};
use crate::stone_vm::{
    match_fused_map_update_if, match_insert_assignment, match_key_not_in_map, match_map_key_target,
    match_row_get, match_row_subscript, HotJsonlAggregationBody, HotJsonlBodyOp,
    HotJsonlNestedUserTotals, HotJsonlSlot,
};

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

pub(crate) fn hot_jsonl_aggregation_ops(
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
