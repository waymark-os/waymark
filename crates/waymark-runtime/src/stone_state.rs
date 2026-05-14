// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;
#[cfg(not(target_os = "hermit"))]
use std::time::Duration;

use nu_protocol::{Record, Span, Value};

#[cfg(not(target_os = "hermit"))]
use crate::stone_run::{bounded_command_output, bounded_command_stdout, resolve_command};

pub(crate) fn runtime_state_record(cwd: &Path) -> Value {
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("cwd", Value::string(cwd.display().to_string(), span));
    record.push("git", git_state_record(cwd, span));
    record.push("tools", tool_state_record(cwd, span));
    Value::record(record, span)
}

#[cfg(target_os = "hermit")]
fn git_state_record(_cwd: &Path, span: Span) -> Value {
    let mut record = Record::new();
    record.push("ok", Value::bool(false, span));
    record.push("kind", Value::string("unavailable", span));
    record.push(
        "message",
        Value::string("git state is unavailable on Hermit", span),
    );
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn git_state_record(cwd: &Path, span: Span) -> Value {
    let cwd_arg = cwd.display().to_string();
    let Some(status) = bounded_command_stdout(
        "git",
        &[
            "-C",
            cwd_arg.as_str(),
            "status",
            "--porcelain=v1",
            "--branch",
        ],
        cwd,
        Duration::from_millis(750),
    ) else {
        let mut record = Record::new();
        record.push("ok", Value::bool(false, span));
        record.push("kind", Value::string("unavailable", span));
        record.push(
            "message",
            Value::string("git status did not complete", span),
        );
        return Value::record(record, span);
    };

    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0_i64;
    let mut behind = 0_i64;
    let mut staged = Vec::new();
    let mut modified = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicted = Vec::new();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            let (head, tracking) = rest.split_once("...").unwrap_or((rest, ""));
            branch = Some(head.to_owned());
            if !tracking.is_empty() {
                let (name, counts) = tracking.split_once(' ').unwrap_or((tracking, ""));
                upstream = Some(name.to_owned());
                ahead = parse_git_count(counts, "ahead").unwrap_or(0);
                behind = parse_git_count(counts, "behind").unwrap_or(0);
            }
            continue;
        }
        if line.len() < 3 {
            continue;
        }
        let status = &line[..2];
        let path = line[3..].to_owned();
        if status == "??" {
            untracked.push(path);
            continue;
        }
        if status.contains('U') || matches!(status, "AA" | "DD") {
            conflicted.push(path.clone());
        }
        let mut chars = status.chars();
        let index = chars.next().unwrap_or(' ');
        let worktree = chars.next().unwrap_or(' ');
        if index != ' ' {
            staged.push(path.clone());
        }
        if worktree != ' ' {
            modified.push(path);
        }
    }

    let mut record = Record::new();
    record.push("ok", Value::bool(true, span));
    record.push(
        "branch",
        branch
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "upstream",
        upstream
            .map(|value| Value::string(value, span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("ahead", Value::int(ahead, span));
    record.push("behind", Value::int(behind, span));
    record.push(
        "dirty",
        Value::bool(
            !staged.is_empty()
                || !modified.is_empty()
                || !untracked.is_empty()
                || !conflicted.is_empty(),
            span,
        ),
    );
    record.push("staged_files", string_list_value(staged, span));
    record.push("modified_files", string_list_value(modified, span));
    record.push("untracked_files", string_list_value(untracked, span));
    record.push("conflicted_files", string_list_value(conflicted, span));
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn parse_git_count(text: &str, key: &str) -> Option<i64> {
    let marker = format!("{key} ");
    let start = text.find(&marker)? + marker.len();
    let digits = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

#[cfg(target_os = "hermit")]
fn tool_state_record(_cwd: &Path, span: Span) -> Value {
    let mut record = Record::new();
    record.push("available", Value::list(Vec::new(), span));
    record.push("unavailable", Value::list(Vec::new(), span));
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn tool_state_record(cwd: &Path, span: Span) -> Value {
    let names = [
        "python3", "python", "pip", "node", "npm", "cargo", "rustc", "go", "java", "javac", "gcc",
        "clang", "make", "git",
    ];
    let mut available = Vec::new();
    let mut unavailable = Vec::new();
    for name in names {
        let resolution = resolve_command(name);
        if let Some(path) = resolution.matches.first() {
            let mut record = Record::new();
            record.push("name", Value::string(name, span));
            record.push("path", Value::string(path.display().to_string(), span));
            if let Some(version) = tool_version(name, cwd) {
                record.push("version", Value::string(version, span));
            }
            available.push(Value::record(record, span));
        } else {
            unavailable.push(Value::string(name, span));
        }
    }
    let mut record = Record::new();
    record.push("available", Value::list(available, span));
    record.push("unavailable", Value::list(unavailable, span));
    Value::record(record, span)
}

#[cfg(not(target_os = "hermit"))]
fn tool_version(name: &str, cwd: &Path) -> Option<String> {
    let args: &[&str] = match name {
        "python3" | "python" => &["--version"],
        "pip" => &["--version"],
        "node" | "npm" | "cargo" | "rustc" | "go" | "java" | "javac" | "gcc" | "clang" | "make"
        | "git" => &["--version"],
        _ => return None,
    };
    bounded_command_output(name, args, cwd, Duration::from_millis(750)).map(|text| {
        text.lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_owned()
    })
}

fn string_list_value(items: Vec<String>, span: Span) -> Value {
    Value::list(
        items
            .into_iter()
            .map(|item| Value::string(item, span))
            .collect(),
        span,
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use nu_protocol::Span;

    use super::{runtime_state_record, string_list_value};

    #[cfg(not(target_os = "hermit"))]
    use super::{parse_git_count, tool_version};

    #[test]
    fn string_list_value_preserves_order() {
        let value = string_list_value(
            vec!["alpha".to_string(), "beta".to_string()],
            Span::unknown(),
        );
        let list = value.as_list().expect("list");

        assert_eq!(list[0].as_str().expect("string"), "alpha");
        assert_eq!(list[1].as_str().expect("string"), "beta");
    }

    #[test]
    fn runtime_state_includes_cwd_git_and_tools_records() {
        let state = runtime_state_record(Path::new("/tmp"));
        let record = state.as_record().expect("record");

        assert_eq!(
            record
                .get("cwd")
                .expect("cwd")
                .as_str()
                .expect("cwd string"),
            "/tmp"
        );
        assert!(record.get("git").expect("git").as_record().is_ok());
        assert!(record.get("tools").expect("tools").as_record().is_ok());
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn parse_git_count_reads_named_counts() {
        assert_eq!(parse_git_count("[ahead 12, behind 3]", "ahead"), Some(12));
        assert_eq!(parse_git_count("[ahead 12, behind 3]", "behind"), Some(3));
        assert_eq!(parse_git_count("[ahead x]", "ahead"), None);
        assert_eq!(parse_git_count("[behind 3]", "ahead"), None);
    }

    #[cfg(not(target_os = "hermit"))]
    #[test]
    fn unknown_tool_has_no_version_probe() {
        assert_eq!(
            tool_version("definitely-not-a-waymark-tool", Path::new("/tmp")),
            None
        );
    }
}
