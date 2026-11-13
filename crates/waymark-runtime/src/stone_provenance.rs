// SPDX-License-Identifier: MIT OR Apache-2.0

//! Provenance labels: where a value's data came from (design 5.9, D-3).
//!
//! A label names a set of sources, such as "the answer of model call T".
//! Labels are carried by the interpreter, as in TaintDroid: strings carry
//! one in [`LabeledStr`], and lists, dicts, and records carry one in their
//! container slot (`stone_containers::Label`). An operation that builds a
//! new string from labeled operands gives the result the union of their
//! labels. Numbers and booleans carry no label, so the numeric fast path is
//! untouched, and decisions made on labeled data (implicit flows) are not
//! tracked.
//!
//! The safe failure is losing a label: an operation that does not know
//! labels sees a plain string, and its result is unlabeled. Labels are
//! read at sinks: file writes, command arguments, and `files=`.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde_json::Value as JsonValue;

use super::stone_containers::Label;
use super::stone_runtime_value::RuntimeValue;

/// A string that carries a provenance label. Unlabeled strings stay plain
/// `RuntimeValue::Nu(Value::String)`, so code that never touches labeled
/// data runs exactly as before.
#[derive(Clone, Debug)]
pub(super) struct LabeledStr {
    pub(super) text: String,
    pub(super) label: Label,
}

/// The sources and label sets of one session. `Label(0)` is "no label";
/// `Label(n)` is the n-th interned set of source ids.
#[derive(Default)]
pub(super) struct Provenance {
    /// One JSON description per source, such as
    /// `{"kind": "model", "model": ..., "transition_id": ...}`.
    sources: Vec<JsonValue>,
    /// Label n-1: sorted, deduplicated source indexes.
    sets: Vec<Vec<u32>>,
    interned: HashMap<Vec<u32>, u32>,
    /// Files this program wrote with labeled text, by path as written
    /// (lexically normalized). A later read of the file gets the label back.
    files: HashMap<PathBuf, Label>,
}

impl Provenance {
    /// A new source, and the label that names only it.
    pub(super) fn source(&mut self, description: JsonValue) -> Label {
        let id = u32::try_from(self.sources.len()).unwrap_or(u32::MAX);
        self.sources.push(description);
        self.intern(vec![id])
    }

    fn intern(&mut self, set: Vec<u32>) -> Label {
        if set.is_empty() {
            return Label::default();
        }
        if let Some(label) = self.interned.get(&set) {
            return Label(*label);
        }
        self.sets.push(set.clone());
        let label = u32::try_from(self.sets.len()).unwrap_or(u32::MAX);
        self.interned.insert(set, label);
        Label(label)
    }

    /// The label naming the sources of both `left` and `right`.
    pub(super) fn join(&mut self, left: Label, right: Label) -> Label {
        if left == right || right.0 == 0 {
            return left;
        }
        if left.0 == 0 {
            return right;
        }
        let mut set = self.set(left).to_vec();
        set.extend_from_slice(self.set(right));
        set.sort_unstable();
        set.dedup();
        self.intern(set)
    }

    pub(super) fn join_all(&mut self, labels: impl IntoIterator<Item = Label>) -> Label {
        labels
            .into_iter()
            .fold(Label::default(), |acc, label| self.join(acc, label))
    }

    fn set(&self, label: Label) -> &[u32] {
        match label.0 {
            0 => &[],
            n => self.sets.get(n as usize - 1).map(Vec::as_slice).unwrap_or(&[]),
        }
    }

    /// The descriptions of a label's sources, in the order they were made.
    pub(super) fn sources_of(&self, label: Label) -> Vec<JsonValue> {
        self.set(label)
            .iter()
            .filter_map(|id| self.sources.get(*id as usize).cloned())
            .collect()
    }

    /// Record that the program wrote `label`ed text to `path`. A full write
    /// replaces the file's label; an append or an edit (`keep_old`) adds to it.
    pub(super) fn note_file(&mut self, path: &Path, label: Label, keep_old: bool) {
        let path = normalize_path(path);
        let old = if keep_old {
            self.files.get(&path).copied().unwrap_or_default()
        } else {
            Label::default()
        };
        let label = self.join(old, label);
        if label.0 == 0 {
            self.files.remove(&path);
        } else {
            self.files.insert(path, label);
        }
    }

    /// The label of text the program wrote to `path`, if any.
    pub(super) fn file_label(&self, path: &Path) -> Label {
        self.files
            .get(&normalize_path(path))
            .copied()
            .unwrap_or_default()
    }

    /// Forget `path` and every tracked file under it (`rm`).
    pub(super) fn forget_files(&mut self, path: &Path) {
        let path = normalize_path(path);
        self.files.retain(|tracked, _| !tracked.starts_with(&path));
    }

    /// Tracked files that are `named` or lie under it, as (path, label),
    /// sorted by path. Paths are compared after `.` and `..` are applied.
    pub(super) fn files_named(&self, named: &Path) -> Vec<(PathBuf, Label)> {
        if self.files.is_empty() {
            return Vec::new();
        }
        let named = normalize_path(named);
        let mut found: Vec<_> = self
            .files
            .iter()
            .filter(|(tracked, _)| tracked.starts_with(&named))
            .map(|(tracked, label)| (tracked.clone(), *label))
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    /// The model sources behind a command's inputs, one entry per source:
    /// `{"model", "transition_id", "via": [...]}`, where `via` names the
    /// inputs that carried its text, in the order given.
    pub(super) fn rests_on(&self, parts: &[(String, Label)]) -> Vec<JsonValue> {
        let mut entries: Vec<(u32, Vec<String>)> = Vec::new();
        for (via, label) in parts {
            for id in self.set(*label) {
                let is_model = self
                    .sources
                    .get(*id as usize)
                    .and_then(|source| source.get("kind"))
                    .and_then(JsonValue::as_str)
                    == Some("model");
                if !is_model {
                    continue;
                }
                match entries.iter_mut().find(|(entry, _)| entry == id) {
                    Some((_, vias)) if !vias.contains(via) => vias.push(via.clone()),
                    Some(_) => {}
                    None => entries.push((*id, vec![via.clone()])),
                }
            }
        }
        entries.sort_by_key(|(id, _)| *id);
        entries
            .into_iter()
            .map(|(id, via)| {
                let source = &self.sources[id as usize];
                serde_json::json!({
                    "model": source.get("model").cloned().unwrap_or(JsonValue::Null),
                    "transition_id": source.get("transition_id").cloned().unwrap_or(JsonValue::Null),
                    "via": via,
                })
            })
            .collect()
    }

    pub(super) fn has_files(&self) -> bool {
        !self.files.is_empty()
    }
}

/// `path` with `.` dropped and `..` applied, without touching the disk.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A string value with `label`: labeled if the label is set, plain if not.
pub(super) fn text_value(text: String, label: Label) -> RuntimeValue {
    if label.0 == 0 {
        RuntimeValue::Nu(nu_protocol::Value::string(text, nu_protocol::Span::unknown()))
    } else {
        RuntimeValue::Str(LabeledStr { text, label })
    }
}

/// The text of a string value, labeled or plain.
pub(super) fn as_text(value: &RuntimeValue) -> Option<&str> {
    match value {
        RuntimeValue::Nu(nu_protocol::Value::String { val, .. }) => Some(val),
        RuntimeValue::Str(text) => Some(&text.text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn joins_are_interned_unions() {
        let mut provenance = Provenance::default();
        let a = provenance.source(json!({"kind": "model", "transition_id": "t1"}));
        let b = provenance.source(json!({"kind": "model", "transition_id": "t2"}));
        let ab = provenance.join(a, b);
        assert_ne!(ab, a);
        assert_eq!(provenance.join(b, a), ab);
        assert_eq!(provenance.join(ab, a), ab);
        assert_eq!(provenance.join(Label::default(), a), a);
        assert_eq!(provenance.join_all([a, Label::default(), b]), ab);
        let ids: Vec<_> = provenance
            .sources_of(ab)
            .iter()
            .map(|source| source["transition_id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, ["t1", "t2"]);
        assert!(provenance.sources_of(Label::default()).is_empty());
    }

    #[test]
    fn files_keep_the_label_of_what_was_written() {
        let mut provenance = Provenance::default();
        let a = provenance.source(json!({"kind": "model", "transition_id": "t1"}));
        let b = provenance.source(json!({"kind": "model", "transition_id": "t2"}));
        provenance.note_file(Path::new("/app/tests/./test_x.py"), a, false);
        assert_eq!(provenance.file_label(Path::new("/app/tests/test_x.py")), a);
        provenance.note_file(Path::new("/app/tests/test_x.py"), b, true);
        assert_eq!(provenance.file_label(Path::new("/app/tests/test_x.py")), provenance.join(a, b));
        let named = provenance.files_named(Path::new("/app/tests/"));
        assert_eq!(named.len(), 1);
        assert!(provenance.files_named(Path::new("/app/src")).is_empty());
        // A full write of unlabeled text clears the file's label.
        provenance.note_file(Path::new("/app/tests/test_x.py"), Label::default(), false);
        assert_eq!(provenance.file_label(Path::new("/app/tests/test_x.py")), Label::default());
        provenance.note_file(Path::new("/app/notes/a.txt"), a, false);
        assert_eq!(provenance.files_named(Path::new("/app/x/../notes")).len(), 1);
        provenance.forget_files(Path::new("/app/notes"));
        assert!(!provenance.has_files());
    }
}
