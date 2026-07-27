// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{json, Value as JsonValue};

const MAX_CONTEXT_ITEMS: usize = 256;
const MAX_CONTEXT_ITEM_BYTES: usize = 16 * 1024;
const MAX_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_CONTEXT_EVIDENCE_ITEMS: usize = 64;
const MAX_CONTEXT_EVENTS: usize = 512;
const MAX_CONTEXT_PROJECT_TOKENS: usize = 4096;

#[derive(Clone, Debug)]
struct ContextItem {
    id: String,
    key: String,
    kind: String,
    content: JsonValue,
    status: String,
    evidence: Vec<JsonValue>,
    revision: u64,
    supersedes: Option<String>,
}

impl ContextItem {
    fn json(&self) -> JsonValue {
        json!({
            "id": self.id,
            "key": self.key,
            "kind": self.kind,
            "content": self.content,
            "status": self.status,
            "evidence": self.evidence,
            "revision": self.revision,
            "supersedes": self.supersedes,
            "superseded_by": null,
        })
    }
}

#[derive(Clone, Default)]
pub(super) struct ContextState {
    revision: u64,
    next_id: u64,
    items: BTreeMap<String, ContextItem>,
    events: VecDeque<JsonValue>,
    remote_item_count: Option<usize>,
    total_item_bytes: usize,
}

impl ContextState {
    pub(super) fn write(
        &mut self,
        key: String,
        kind: String,
        content: JsonValue,
        status: String,
        evidence: Vec<JsonValue>,
    ) -> Result<JsonValue, String> {
        validate_text("key", &key)?;
        validate_text("kind", &kind)?;
        validate_status(&status)?;
        validate_item_size(&content, &evidence)?;
        if status != "archived" && !self.items.contains_key(&key) {
            self.require_capacity()?;
        }
        let prior = self.items.get(&key);
        let prior_id = prior.map(|item| item.id.clone());
        let prior_bytes = prior.map(encoded_item_bytes).transpose()?.unwrap_or(0);
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| "context revision overflow".to_string())?;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "context item id overflow".to_string())?;
        let id = format!("context-item-{next_id}");
        let item = ContextItem {
            id: id.clone(),
            key: key.clone(),
            kind,
            content,
            status,
            evidence,
            revision,
            supersedes: prior_id,
        };
        let item_bytes = if item.status == "archived" {
            0
        } else {
            encoded_item_bytes(&item)?
        };
        let total_item_bytes = self
            .total_item_bytes
            .checked_sub(prior_bytes)
            .and_then(|bytes| bytes.checked_add(item_bytes))
            .ok_or_else(|| "context byte accounting overflow".to_string())?;
        if total_item_bytes > MAX_CONTEXT_BYTES {
            return Err(format!(
                "context is {total_item_bytes} bytes; maximum is {MAX_CONTEXT_BYTES}"
            ));
        }
        self.revision = revision;
        self.next_id = next_id;
        self.total_item_bytes = total_item_bytes;
        let value = item.json();
        if item.status == "archived" {
            self.items.remove(&key);
        } else {
            self.items.insert(key.clone(), item);
        }
        self.remote_item_count = None;
        self.push_event(json!({
            "op": "write",
            "revision": self.revision,
            "item": id,
            "key": key,
        }));
        Ok(value)
    }

    pub(super) fn read(
        &mut self,
        query: &str,
        keys: &[String],
        kinds: &[String],
        limit: usize,
    ) -> Vec<JsonValue> {
        let query = query.to_lowercase();
        let query_tokens = relevance_tokens(&query);
        let mut matches = self
            .items
            .values()
            .filter(|item| keys.is_empty() || keys.iter().any(|key| key == &item.key))
            .filter(|item| kinds.is_empty() || kinds.iter().any(|kind| kind == &item.kind))
            .filter_map(|item| {
                let relevance = relevance_score(&searchable_text(item), &query_tokens);
                (query_tokens.is_empty() || relevance > 0).then_some((item, relevance))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(item, relevance)| (Reverse(*relevance), Reverse(item.revision)));
        let values = matches
            .into_iter()
            .take(limit)
            .map(|(item, _)| item.json())
            .collect::<Vec<_>>();
        self.push_event(json!({
            "op": "read",
            "revision": self.revision,
            "query": query,
            "keys": keys,
            "kinds": kinds,
            "selected": values.iter().filter_map(|item| item["id"].as_str()).collect::<Vec<_>>(),
        }));
        values
    }

    pub(super) fn project(
        &mut self,
        focus: &str,
        max_tokens: usize,
        required_keys: &[String],
    ) -> Result<JsonValue, String> {
        if max_tokens == 0 {
            return Err("max_tokens must be greater than zero".to_string());
        }
        if max_tokens > MAX_CONTEXT_PROJECT_TOKENS {
            return Err(format!("max_tokens exceeds {MAX_CONTEXT_PROJECT_TOKENS}"));
        }
        if required_keys.len() > MAX_CONTEXT_ITEMS {
            return Err(format!(
                "projection may require at most {MAX_CONTEXT_ITEMS} keys"
            ));
        }
        let mut required_set = BTreeSet::new();
        for key in required_keys {
            validate_text("required key", key)?;
            if !required_set.insert(key.clone()) {
                return Err(format!("required key is duplicated: {key}"));
            }
        }
        let mut selected = required_keys
            .iter()
            .map(|key| {
                self.items
                    .get(key)
                    .map(ContextItem::json)
                    .ok_or_else(|| format!("required key is missing: {key}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let max_bytes = max_tokens.saturating_mul(4);
        let required_text = serde_json::to_string(&json!({"memory": selected}))
            .map_err(|error| error.to_string())?;
        if required_text.len() > max_bytes {
            return Err(format!(
                "required keys need approximately {} tokens; budget is {max_tokens}",
                required_text.len().saturating_add(3) / 4
            ));
        }
        let focus_tokens = relevance_tokens(focus);
        let mut candidates = self
            .items
            .values()
            .filter(|item| !required_set.contains(&item.key))
            .map(|item| {
                let relevance = relevance_score(&searchable_text(item), &focus_tokens);
                (item, relevance)
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(item, relevance)| (Reverse(*relevance), Reverse(item.revision)));

        let mut truncated = false;
        for (item, _) in candidates {
            let candidate = item.json();
            let mut next = selected.clone();
            next.push(candidate.clone());
            let text = serde_json::to_string(&json!({"memory": next}))
                .map_err(|error| error.to_string())?;
            if text.len() <= max_bytes {
                selected.push(candidate);
            } else {
                truncated = true;
            }
        }
        let text = serde_json::to_string(&json!({"memory": selected}))
            .map_err(|error| error.to_string())?;
        let estimated_tokens = text.len().saturating_add(3) / 4;
        let selected_ids = selected
            .iter()
            .filter_map(|item| item["id"].as_str())
            .collect::<Vec<_>>();
        self.push_event(json!({
            "op": "project",
            "revision": self.revision,
            "focus": focus,
            "max_tokens": max_tokens,
            "required_keys": required_keys,
            "selected": selected_ids,
            "estimated_tokens": estimated_tokens,
            "truncated": truncated,
        }));
        Ok(json!({
            "revision": self.revision,
            "focus": focus,
            "required_keys": required_keys,
            "items": selected,
            "text": text,
            "estimated_tokens": estimated_tokens,
            "truncated": truncated,
        }))
    }

    pub(super) fn diagnostics(&self) -> Option<JsonValue> {
        (!self.events.is_empty()).then(|| {
            json!({
                "revision": self.revision,
                "item_count": self.remote_item_count.unwrap_or(self.items.len()),
                "events": self.events,
            })
        })
    }

    fn require_capacity(&self) -> Result<(), String> {
        if self.items.len() >= MAX_CONTEXT_ITEMS {
            return Err(format!(
                "context item limit {MAX_CONTEXT_ITEMS} reached; compact or archive state before writing more"
            ));
        }
        Ok(())
    }

    pub(super) fn observe_gateway_write(
        &mut self,
        item: &JsonValue,
        revision: u64,
        item_count: usize,
    ) {
        self.revision = revision;
        self.switch_to_gateway(item_count);
        self.push_event(json!({
            "op": "write",
            "revision": revision,
            "item": item.get("id").cloned().unwrap_or(JsonValue::Null),
            "key": item.get("key").cloned().unwrap_or(JsonValue::Null),
            "backend": "gateway",
        }));
    }

    pub(super) fn observe_gateway_read(
        &mut self,
        query: &str,
        keys: &[String],
        kinds: &[String],
        items: &[JsonValue],
        revision: u64,
        item_count: usize,
    ) {
        self.revision = revision;
        self.switch_to_gateway(item_count);
        self.push_event(json!({
            "op": "read",
            "revision": revision,
            "query": query,
            "keys": keys,
            "kinds": kinds,
            "selected": items.iter().filter_map(|item| item["id"].as_str()).collect::<Vec<_>>(),
            "backend": "gateway",
        }));
    }

    pub(super) fn observe_gateway_project(
        &mut self,
        focus: &str,
        max_tokens: usize,
        projection: &JsonValue,
        revision: u64,
        item_count: usize,
    ) {
        self.revision = revision;
        self.switch_to_gateway(item_count);
        let selected = projection
            .get("items")
            .and_then(JsonValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item["id"].as_str())
            .collect::<Vec<_>>();
        self.push_event(json!({
            "op": "project",
            "revision": revision,
            "focus": focus,
            "max_tokens": max_tokens,
            "selected": selected,
            "estimated_tokens": projection.get("estimated_tokens").cloned().unwrap_or(JsonValue::Null),
            "truncated": projection.get("truncated").cloned().unwrap_or(JsonValue::Null),
            "backend": "gateway",
        }));
    }

    fn push_event(&mut self, event: JsonValue) {
        if self.events.len() == MAX_CONTEXT_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    fn switch_to_gateway(&mut self, item_count: usize) {
        // Drop any standalone-session payload as soon as the authoritative
        // Gateway backend is observed; only bounded diagnostics remain local.
        self.items.clear();
        self.total_item_bytes = 0;
        self.remote_item_count = Some(item_count);
    }
}

fn encoded_item_bytes(item: &ContextItem) -> Result<usize, String> {
    serde_json::to_vec(&item.json())
        .map(|bytes| bytes.len())
        .map_err(|error| error.to_string())
}

fn searchable_text(item: &ContextItem) -> String {
    format!(
        "{} {} {} {}",
        item.key,
        item.kind,
        item.status,
        serde_json::to_string(&item.content).unwrap_or_default()
    )
}

fn relevance_tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

fn relevance_score(text: &str, tokens: &[String]) -> usize {
    let text = text.to_lowercase();
    tokens
        .iter()
        .filter(|token| text.contains(token.as_str()))
        .count()
}

fn validate_text(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.len() > 256 {
        return Err(format!("{field} exceeds 256 bytes"));
    }
    Ok(())
}

fn validate_status(status: &str) -> Result<(), String> {
    if matches!(
        status,
        "active" | "verified" | "pending" | "contradicted" | "archived"
    ) {
        Ok(())
    } else {
        Err(format!(
            "unsupported status {status:?}; expected active, verified, pending, contradicted, or archived"
        ))
    }
}

fn validate_item_size(content: &JsonValue, evidence: &[JsonValue]) -> Result<(), String> {
    if evidence.len() > MAX_CONTEXT_EVIDENCE_ITEMS {
        return Err(format!(
            "context evidence has {} entries; maximum is {MAX_CONTEXT_EVIDENCE_ITEMS}",
            evidence.len()
        ));
    }
    let bytes = serde_json::to_vec(&json!({"content": content, "evidence": evidence}))
        .map_err(|error| error.to_string())?
        .len();
    if bytes > MAX_CONTEXT_ITEM_BYTES {
        return Err(format!(
            "context item is {bytes} bytes; maximum is {MAX_CONTEXT_ITEM_BYTES}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_write_supersedes_and_projection_prefers_focus() {
        let mut context = ContextState::default();
        let first = context
            .write(
                "requirement.output".to_string(),
                "requirement".to_string(),
                json!({"text": "keep the binary"}),
                "pending".to_string(),
                vec![json!("trace-1")],
            )
            .unwrap();
        let second = context
            .write(
                "requirement.output".to_string(),
                "requirement".to_string(),
                json!({"text": "keep the verified binary"}),
                "verified".to_string(),
                vec![json!("trace-2")],
            )
            .unwrap();
        context
            .write(
                "outcome.probe".to_string(),
                "outcome".to_string(),
                json!({"action": "probe", "ok": false}),
                "active".to_string(),
                vec![],
            )
            .unwrap();

        let read = context.read("binary", &[], &[], 10);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0]["id"], second["id"]);
        assert_eq!(read[0]["supersedes"], first["id"]);

        let projection = context
            .project(
                "verified output binary",
                256,
                &["requirement.output".to_string()],
            )
            .unwrap();
        assert_eq!(projection["items"][0]["id"], second["id"]);
        assert_eq!(projection["required_keys"], json!(["requirement.output"]));
        assert!(projection["text"]
            .as_str()
            .unwrap()
            .contains("verified binary"));

        let missing = context
            .project("", 256, &["requirement.missing".to_string()])
            .unwrap_err();
        assert!(missing.contains("required key is missing"));
        let too_small = context
            .project("", 1, &["requirement.output".to_string()])
            .unwrap_err();
        assert!(too_small.contains("required keys need"));
    }

    #[test]
    fn repeated_revisions_and_diagnostics_remain_bounded() {
        let mut context = ContextState::default();
        for revision in 0..1000 {
            context
                .write(
                    "candidate.best".to_string(),
                    "candidate".to_string(),
                    json!({"revision": revision}),
                    "active".to_string(),
                    vec![],
                )
                .unwrap();
        }

        assert_eq!(context.revision, 1000);
        assert_eq!(context.items.len(), 1);
        assert_eq!(context.events.len(), MAX_CONTEXT_EVENTS);
        assert_eq!(context.items.get("candidate.best").unwrap().revision, 1000);

        context
            .write(
                "candidate.best".to_string(),
                "candidate".to_string(),
                JsonValue::Null,
                "archived".to_string(),
                vec![],
            )
            .unwrap();
        assert!(context.items.is_empty());
        assert_eq!(context.events.len(), MAX_CONTEXT_EVENTS);
    }
}
