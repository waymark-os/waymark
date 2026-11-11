// SPDX-License-Identifier: MIT OR Apache-2.0

//! Observed effects of a command (tier 1): which files under the workspace a
//! `run()` created or changed, reported on its result as `result.effects`.
//!
//! These effects are never declared. In local mode the report comes from one
//! bounded metadata walk after the command: any entry whose modification or
//! status-change time is at or after the moment the command started counts as
//! changed, and a directory whose entry list changed with no new entry in it
//! is reported under `entries_removed_in`. Another process writing at the same
//! time is indistinguishable, so the report says what changed during the run,
//! not what the command alone did. The walk is capped in entries and time; a
//! capped walk says so with `complete: False`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use nu_protocol::{Record, Span, Value};

const MAX_ENTRIES: usize = 100_000;
const MAX_WALK: Duration = Duration::from_millis(150);
const MAX_GROUPS: usize = 8;

/// Tool-owned directories that are large and regenerable. They are not
/// walked; when present they are listed under `not_scanned`, so the report
/// never silently claims they are unchanged.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".cache",
];

#[derive(Clone, Copy)]
pub(crate) struct EffectClock {
    started: SystemTime,
}

/// File timestamps come from the kernel's coarse clock, which can lag the
/// wall clock by a scheduler tick (up to 10 ms), so the start is moved back by
/// a margin. A file written in that margin just before the command may be
/// reported as changed by it.
pub(crate) const CLOCK_MARGIN: Duration = Duration::from_millis(20);

pub(crate) fn effect_clock() -> EffectClock {
    let now = SystemTime::now();
    EffectClock {
        started: now.checked_sub(CLOCK_MARGIN).unwrap_or(now),
    }
}

/// The `effects` record for a finished command that ran in `cwd`.
pub(crate) fn observed_effects(clock: EffectClock, cwd: &Path) -> Value {
    let span = Span::unknown();
    let mut effects = Record::with_capacity(1);
    effects.push("observed", observe(clock, cwd));
    Value::record(effects, span)
}

/// The `effects` record when nothing can be observed locally.
pub(crate) fn unobserved_effects(reason: &str) -> Value {
    let span = Span::unknown();
    let mut observed = Record::with_capacity(2);
    observed.push("complete", Value::bool(false, span));
    observed.push("reason", Value::string(reason, span));
    let mut effects = Record::with_capacity(1);
    effects.push("observed", Value::record(observed, span));
    Value::record(effects, span)
}

fn observe(clock: EffectClock, cwd: &Path) -> Value {
    let span = Span::unknown();
    let Some(root) = scan_root(cwd) else {
        let mut observed = Record::with_capacity(2);
        observed.push("complete", Value::bool(false, span));
        observed.push(
            "reason",
            Value::string(
                "the command ran in / or the home directory, which is not scanned; set cwd to the project directory",
                span,
            ),
        );
        return Value::record(observed, span);
    };
    let mut walk = walk(&root, clock.started);
    if fs::metadata(&root)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| modified >= clock.started)
    {
        walk.dirs_changed.push(String::new());
    }
    let mut changed = walk.changed;
    changed.sort();
    let removed_in = walk
        .dirs_changed
        .into_iter()
        .filter(|dir| !walk.explained_dirs.contains(dir))
        .map(|dir| display_dir(&dir))
        .collect::<Vec<_>>();

    let mut observed = Record::with_capacity(6);
    observed.push("root", Value::string(root.display().to_string(), span));
    observed.push(
        "changed",
        Value::list(
            collapse(&changed)
                .into_iter()
                .map(|entry| Value::string(entry, span))
                .collect(),
            span,
        ),
    );
    observed.push("changed_count", Value::int(changed.len() as i64, span));
    if !removed_in.is_empty() {
        observed.push(
            "entries_removed_in",
            Value::list(
                removed_in
                    .into_iter()
                    .take(MAX_GROUPS)
                    .map(|entry| Value::string(entry, span))
                    .collect(),
                span,
            ),
        );
    }
    if !walk.skipped.is_empty() {
        let mut skipped = walk.skipped;
        skipped.sort();
        skipped.dedup();
        let extra = skipped.len().saturating_sub(MAX_GROUPS);
        let mut listed = skipped
            .into_iter()
            .take(MAX_GROUPS)
            .map(|dir| Value::string(display_dir(&dir), span))
            .collect::<Vec<_>>();
        if extra > 0 {
            listed.push(Value::string(format!("... and {extra} more"), span));
        }
        observed.push("not_scanned", Value::list(listed, span));
    }
    observed.push("complete", Value::bool(walk.complete, span));
    if !walk.complete {
        observed.push(
            "reason",
            Value::string(
                format!(
                    "stopped after {} entries; files beyond that were not checked",
                    walk.visited
                ),
                span,
            ),
        );
    }
    Value::record(observed, span)
}

/// The project root containing `cwd` (the nearest ancestor with `.git`), or
/// `cwd` itself. `/` and `$HOME` are never scanned.
fn scan_root(cwd: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let too_broad = |path: &Path| path == Path::new("/") || home.as_deref() == Some(path);
    let root = cwd
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .filter(|dir| !too_broad(dir))
        .unwrap_or(cwd)
        .to_path_buf();
    (!too_broad(&root)).then_some(root)
}

struct Walk {
    changed: Vec<String>,
    dirs_changed: Vec<String>,
    explained_dirs: std::collections::HashSet<String>,
    skipped: Vec<String>,
    visited: usize,
    complete: bool,
}

fn walk(root: &Path, started: SystemTime) -> Walk {
    let deadline = Instant::now() + MAX_WALK;
    let mut result = Walk {
        changed: Vec::new(),
        dirs_changed: Vec::new(),
        explained_dirs: std::collections::HashSet::new(),
        skipped: Vec::new(),
        visited: 0,
        complete: true,
    };
    #[cfg(unix)]
    let root_device = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(root).map(|meta| meta.dev()).ok()
    };
    // Cargo's build directory at the project root.
    let cargo_target = root.join("Cargo.toml").exists();
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        let Ok(entries) = fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            result.visited += 1;
            if result.visited > MAX_ENTRIES
                || (result.visited.is_multiple_of(512) && Instant::now() > deadline)
            {
                result.complete = false;
                return result;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = relative.join(entry.file_name());
            let key = path.to_string_lossy().into_owned();
            let parent_key = relative.to_string_lossy().into_owned();
            let touched = touched_since(&meta, started);
            if meta.is_dir() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if root_device.is_some_and(|device| device != meta.dev()) {
                        continue;
                    }
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if SKIPPED_DIRS.contains(&name.as_ref())
                    || (cargo_target && relative.as_os_str().is_empty() && name == "target")
                {
                    result.skipped.push(key);
                    continue;
                }
                let created = meta.created().is_ok_and(|created| created >= started);
                if created {
                    result.changed.push(format!("{key}/"));
                    result.explained_dirs.insert(parent_key.clone());
                    result.explained_dirs.insert(key.clone());
                } else if meta.modified().is_ok_and(|modified| modified >= started) {
                    result.dirs_changed.push(key.clone());
                }
                stack.push(path);
            } else if touched {
                // A directory's entry list changes on create, delete, and
                // rename, not on an in-place write. Only a new file explains
                // its parent's change; otherwise something was removed there.
                if meta.created().map_or(true, |created| created >= started) {
                    result.explained_dirs.insert(parent_key);
                }
                result.changed.push(key);
            }
        }
    }
    result
}

fn touched_since(meta: &fs::Metadata, started: SystemTime) -> bool {
    if meta.modified().is_ok_and(|modified| modified >= started) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ctime = SystemTime::UNIX_EPOCH
            + Duration::new(
                meta.ctime().max(0) as u64,
                meta.ctime_nsec().clamp(0, 999_999_999) as u32,
            );
        if ctime >= started {
            return true;
        }
    }
    false
}

fn display_dir(dir: &str) -> String {
    if dir.is_empty() {
        "./".to_string()
    } else {
        format!("{dir}/")
    }
}

/// Collapse a sorted list of changed paths into at most `MAX_GROUPS` entries,
/// choosing the deepest directory level that fits: `src/build/ (14 files)`.
fn collapse(paths: &[String]) -> Vec<String> {
    if paths.len() <= MAX_GROUPS {
        return paths.to_vec();
    }
    let max_depth = paths
        .iter()
        .map(|path| path.trim_end_matches('/').split('/').count())
        .max()
        .unwrap_or(1);
    for depth in (1..max_depth).rev() {
        let mut groups: BTreeMap<String, Vec<&String>> = BTreeMap::new();
        for path in paths {
            let parts = path.trim_end_matches('/').split('/').collect::<Vec<_>>();
            let key = if parts.len() > depth {
                format!("{}/", parts[..depth].join("/"))
            } else {
                path.clone()
            };
            groups.entry(key).or_default().push(path);
        }
        if groups.len() <= MAX_GROUPS || depth == 1 {
            let mut rendered = groups
                .into_iter()
                .map(|(key, members)| {
                    if members.len() == 1 && members[0] == &key {
                        key
                    } else {
                        format!("{key} ({} files)", members.len())
                    }
                })
                .collect::<Vec<_>>();
            if rendered.len() > MAX_GROUPS {
                let rest = rendered.len() - MAX_GROUPS;
                rendered.truncate(MAX_GROUPS);
                rendered.push(format!("... and {rest} more"));
            }
            return rendered;
        }
    }
    paths.iter().take(MAX_GROUPS).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_groups_large_change_sets() {
        let mut paths = (0..14)
            .map(|i| format!("src/build/o{i}.o"))
            .chain((0..6).map(|i| format!("src/.deps/d{i}")))
            .chain(["src/config.h".to_string()])
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            collapse(&paths),
            vec![
                "src/.deps/ (6 files)".to_string(),
                "src/build/ (14 files)".to_string(),
                "src/config.h".to_string(),
            ]
        );
        assert_eq!(collapse(&["a".to_string()]), vec!["a".to_string()]);
    }

    #[test]
    fn observes_created_and_modified_files() {
        let root = std::env::temp_dir().join(format!(
            "stone-effects-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(root.join("keep")).expect("mkdir");
        fs::write(root.join("old.txt"), "x").expect("write");
        fs::write(root.join("keep/gone.txt"), "x").expect("write");
        std::thread::sleep(CLOCK_MARGIN * 3);
        let clock = effect_clock();
        fs::write(root.join("old.txt"), "y").expect("modify");
        fs::write(root.join("new.txt"), "y").expect("create");
        fs::remove_file(root.join("keep/gone.txt")).expect("remove");
        let effects = observed_effects(clock, &root);
        let json = crate::json::nu_to_json_value(&effects);
        let observed = &json["observed"];
        assert_eq!(
            observed["changed"],
            serde_json::json!(["new.txt", "old.txt"])
        );
        assert_eq!(observed["entries_removed_in"], serde_json::json!(["keep/"]));
        assert_eq!(observed["complete"], serde_json::json!(true));
        let _ = fs::remove_dir_all(&root);
    }
}
