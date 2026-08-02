// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use nu_protocol::{shell_error::generic::GenericError, Record, ShellError, Span, Value};
use regex::bytes::Regex;

use crate::gateway_runtime;
use crate::global_state::{FreezeSafe, VmFrozen};
use crate::json::{json_to_nu_value, nu_to_json_value};

pub(crate) const STONE_MAX_FIND_ENTRIES: usize = 4096;
pub(crate) const STONE_MAX_SEARCH_FILES: usize = 1024;
pub(crate) const STONE_MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
pub(crate) const STONE_MAX_SEARCH_MATCHES: usize = 1000;

#[derive(Default)]
pub(crate) struct StoneFindOptions {
    pub(crate) name_contains: Option<String>,
    pub(crate) name_glob: Option<String>,
    pub(crate) path_glob: Option<String>,
    pub(crate) kind_filter: Option<String>,
    pub(crate) max_depth: Option<usize>,
    pub(crate) min_size: Option<u64>,
    pub(crate) max_size: Option<u64>,
    pub(crate) modified_after_ms: Option<i64>,
    pub(crate) modified_before_ms: Option<i64>,
}

pub(crate) enum RuntimeFile {
    Read { text: String, closed: bool },
    Write { path: PathBuf, file: Option<File> },
}

pub(crate) fn open_runtime_file(path: &Path, mode: &str) -> Result<RuntimeFile, ShellError> {
    if gateway_runtime::enabled() {
        return match mode {
            "r" | "rt" => Ok(RuntimeFile::Read {
                text: gateway_runtime::read_text(path, usize::MAX)?,
                closed: false,
            }),
            "w" | "wt" | "a" | "at" => Err(stone_error(
                "open",
                "Gateway runtime does not support streaming open() writes yet; use write_text/write_json/write_jsonl",
            )),
            other => Err(stone_error(
                "open",
                format!("unsupported mode `{other}`; expected r, w, or a"),
            )),
        };
    }
    match mode {
        "r" | "rt" => {
            let mut file =
                File::open(path).map_err(|err| io_read_stone_error("open", err, path))?;
            let mut text = String::new();
            use std::io::Read;
            file.read_to_string(&mut text)
                .map_err(|err| io_stone_error("open", err, path))?;
            Ok(RuntimeFile::Read {
                text,
                closed: false,
            })
        }
        "w" | "wt" => {
            ensure_parent_dir_for_write("open", path)?;
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .map_err(|err| io_stone_error("open", err, path))?;
            Ok(RuntimeFile::Write {
                path: path.to_path_buf(),
                file: Some(file),
            })
        }
        "a" | "at" => {
            ensure_parent_dir_for_write("open", path)?;
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|err| io_stone_error("open", err, path))?;
            Ok(RuntimeFile::Write {
                path: path.to_path_buf(),
                file: Some(file),
            })
        }
        other => Err(stone_error(
            "open",
            format!("unsupported mode `{other}`; expected r, w, or a"),
        )),
    }
}

pub(crate) fn cat_text(path: &Path) -> Result<String, ShellError> {
    read_text(path, usize::MAX)
}

pub(crate) fn read_text(path: &Path, max_bytes: usize) -> Result<String, ShellError> {
    stone_file_adapter().read_text(path, max_bytes)
}

pub(crate) fn read_text_range(
    path: &Path,
    offset: u64,
    max_bytes: usize,
) -> Result<String, ShellError> {
    stone_file_adapter().read_text_range(path, offset, max_bytes)
}

pub(crate) fn read_text_lines(
    path: &Path,
    start_line: usize,
    end_line: Option<usize>,
    max_bytes: usize,
) -> Result<String, ShellError> {
    stone_file_adapter().read_text_lines(path, start_line, end_line, max_bytes)
}

pub(crate) fn write_text(path: &Path, text: &str, append: bool) -> Result<Value, ShellError> {
    let written = stone_file_adapter().write_text(path, text, append)?;
    Ok(file_write_record(written, Span::unknown()))
}

pub(crate) fn stat_record(path: &Path, follow_symlinks: bool) -> Result<Value, ShellError> {
    let stat = stone_file_adapter().stat(path, follow_symlinks)?;
    Ok(file_stat_record(stat, Span::unknown()))
}

pub(crate) fn file_nonempty_probe(path: &Path) -> Result<Option<u64>, ShellError> {
    Ok(file_regular_probe(path)?.filter(|size| *size > 0))
}

pub(crate) fn file_regular_probe(path: &Path) -> Result<Option<u64>, ShellError> {
    let Some(stat) = stone_file_adapter().stat_optional(path, false)? else {
        return Ok(None);
    };
    Ok(stat.is_file.then_some(stat.size))
}

pub(crate) fn list_dir_records(path: &Path) -> Result<Vec<Value>, ShellError> {
    let mut entries = stone_file_adapter()
        .list_dir(path)?
        .into_iter()
        .map(|entry| file_entry_record(entry, Span::unknown()))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.get_data_by_key("name")
            .and_then(|value| value.coerce_string().ok())
            .cmp(
                &right
                    .get_data_by_key("name")
                    .and_then(|value| value.coerce_string().ok()),
            )
    });
    Ok(entries)
}

pub(crate) fn find_records(
    root: PathBuf,
    options: StoneFindOptions,
) -> Result<Vec<Value>, ShellError> {
    let mut entries = Vec::new();
    let mut queue = VecDeque::from([(root, 0usize)]);

    while let Some((path, depth)) = queue.pop_front() {
        if entries.len() >= STONE_MAX_FIND_ENTRIES {
            break;
        }
        let stat = stone_file_adapter().stat(&path, false)?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        if stone_find_entry_matches(
            &path,
            &name,
            &stat,
            options.name_contains.as_deref(),
            options.name_glob.as_deref(),
            options.path_glob.as_deref(),
            options.kind_filter.as_deref(),
            options.min_size,
            options.max_size,
            options.modified_after_ms,
            options.modified_before_ms,
        ) {
            entries.push(file_entry_record(
                StoneFileEntry {
                    name,
                    stat: stat.clone(),
                },
                Span::unknown(),
            ));
        }
        if stat.is_dir && options.max_depth.is_none_or(|max_depth| depth < max_depth) {
            for entry in stone_file_adapter().list_dir(&path)? {
                queue.push_back((entry.stat.path, depth.saturating_add(1)));
            }
        }
    }

    entries.sort_by(|left, right| {
        left.get_data_by_key("path")
            .and_then(|value| value.coerce_string().ok())
            .cmp(
                &right
                    .get_data_by_key("path")
                    .and_then(|value| value.coerce_string().ok()),
            )
    });
    Ok(entries)
}

pub(crate) fn diff_record_for_paths(
    left_path: &Path,
    right_path: &Path,
) -> Result<Value, ShellError> {
    let left_text = stone_file_adapter().read_text(left_path, 4 * 1024 * 1024)?;
    let right_text = stone_file_adapter().read_text(right_path, 4 * 1024 * 1024)?;
    Ok(stone_diff_record(
        left_path,
        right_path,
        &left_text,
        &right_text,
        Span::unknown(),
    ))
}

pub(crate) fn read_json_file(path: &Path) -> Result<Value, ShellError> {
    let bytes = read_bytes_for_jsonl(path, "read_json")?;
    let json = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|err| stone_error("read_json", format!("{}: {}", path.display(), err)))?;
    Ok(json_to_nu_value(json, Span::unknown()))
}

pub(crate) fn read_csv_file(path: &Path, limit: Option<usize>) -> Result<Value, ShellError> {
    let text = read_text(path, usize::MAX)?;
    let rows = parse_csv_records(&text, limit)?;
    Ok(Value::list(rows, Span::unknown()))
}

pub(crate) fn write_json_file(path: &Path, value: &Value) -> Result<Value, ShellError> {
    let json = nu_to_json_value(value);
    let text = serde_json::to_string_pretty(&json)
        .map_err(|err| stone_error("write_json", err.to_string()))?
        + "\n";
    write_text(path, &text, false)?;
    Ok(Value::int(
        i64::try_from(text.len()).unwrap_or(i64::MAX),
        Span::unknown(),
    ))
}

pub(crate) fn write_jsonl_file(path: &Path, rows: Vec<Value>) -> Result<Value, ShellError> {
    let mut text = String::new();
    for value in rows {
        let json = nu_to_json_value(&value);
        text.push_str(
            &serde_json::to_string(&json)
                .map_err(|err| stone_error("write_jsonl", err.to_string()))?,
        );
        text.push('\n');
    }
    write_text(path, &text, false)?;
    Ok(Value::int(
        i64::try_from(text.len()).unwrap_or(i64::MAX),
        Span::unknown(),
    ))
}

pub(crate) fn read_bytes_for_jsonl(path: &Path, context: &str) -> Result<Vec<u8>, ShellError> {
    read_bytes(path, usize::MAX, context)
}

pub(crate) fn read_bytes(
    path: &Path,
    max_bytes: usize,
    context: &str,
) -> Result<Vec<u8>, ShellError> {
    if gateway_runtime::enabled() {
        return gateway_runtime::read_bytes(path, max_bytes);
    }
    let mut bytes = fs::read(path).map_err(|err| io_read_stone_error(context, err, path))?;
    bytes.truncate(max_bytes);
    Ok(bytes)
}

pub(crate) fn create_dir_all(path: &Path) -> Result<(), ShellError> {
    if gateway_runtime::enabled() {
        return gateway_runtime::mkdir(path);
    }
    fs::create_dir_all(path).map_err(|err| io_stone_error("mkdir", err, path))
}

pub(crate) fn remove_path(path: &Path) -> Result<(), ShellError> {
    if gateway_runtime::enabled() {
        return gateway_runtime::remove(path);
    }
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|err| io_stone_error("rm", err, path))
    } else {
        fs::remove_file(path).map_err(|err| io_stone_error("rm", err, path))
    }
}

pub(crate) fn edit_text_file(
    path: &Path,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<Value, ShellError> {
    if old.is_empty() {
        return Err(stone_error("edit", "old text must not be empty"));
    }
    let text = read_text(path, usize::MAX)?;
    let matches = text.matches(old).count();
    if matches == 0 {
        return Err(stone_error("edit", "old text was not found"));
    }
    let replaced = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    write_text(path, &replaced, false)?;
    let mut record = Record::with_capacity(4);
    record.push(
        "path",
        Value::string(path.display().to_string(), Span::unknown()),
    );
    record.push(
        "replacements",
        Value::int(
            if replace_all { matches as i64 } else { 1 },
            Span::unknown(),
        ),
    );
    record.push("matched", Value::int(matches as i64, Span::unknown()));
    record.push("all", Value::bool(replace_all, Span::unknown()));
    Ok(Value::record(record, Span::unknown()))
}

pub(crate) fn save_value_file(
    path: &Path,
    value: &Value,
    append: bool,
    force: bool,
) -> Result<Value, ShellError> {
    let exists = if gateway_runtime::enabled() {
        stone_file_adapter().stat(path, false).is_ok()
    } else {
        path.exists()
    };
    if exists && !append && !force {
        return Err(stone_error(
            "save",
            format!(
                "{} already exists; pass force=True to overwrite",
                path.display()
            ),
        ));
    }
    let bytes = value_to_save_bytes(value)?;
    if gateway_runtime::enabled() {
        let text = String::from_utf8(bytes.clone()).map_err(|err| {
            stone_error(
                "save",
                format!(
                    "{}: Gateway save currently supports UTF-8 text, got invalid UTF-8: {err}",
                    path.display()
                ),
            )
        })?;
        write_text(path, &text, append)?;
    } else {
        ensure_parent_dir_for_write("save", path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .map_err(|err| io_stone_error("save", err, path))?;
        file.write_all(&bytes)
            .map_err(|err| io_stone_error("save", err, path))?;
        file.flush()
            .map_err(|err| io_stone_error("save", err, path))?;
    }
    let mut record = Record::with_capacity(3);
    record.push(
        "path",
        Value::string(path.display().to_string(), Span::unknown()),
    );
    record.push(
        "bytes",
        Value::int(
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            Span::unknown(),
        ),
    );
    record.push("append", Value::bool(append, Span::unknown()));
    Ok(Value::record(record, Span::unknown()))
}

fn value_to_save_bytes(value: &Value) -> Result<Vec<u8>, ShellError> {
    match value {
        Value::Binary { val, .. } => Ok(val.clone()),
        Value::String { val, .. } | Value::Glob { val, .. } => Ok(val.as_bytes().to_vec()),
        other => serde_json::to_vec(&nu_to_json_value(other))
            .map_err(|err| stone_error("save", err.to_string())),
    }
}

pub(crate) fn search_records(
    root: PathBuf,
    needle: &str,
    regex: bool,
) -> Result<Vec<Value>, ShellError> {
    let matcher = StoneSearchMatcher::new(needle, regex)?;
    let mut files_visited = 0usize;
    let mut matches = Vec::new();
    let mut queue = VecDeque::from([root]);
    while let Some(path) = queue.pop_front() {
        if files_visited >= STONE_MAX_SEARCH_FILES || matches.len() >= STONE_MAX_SEARCH_MATCHES {
            break;
        }
        let stat = stone_file_adapter().stat(&path, false)?;
        if stat.is_dir {
            for entry in stone_file_adapter().list_dir(&path)? {
                queue.push_back(entry.stat.path);
            }
            continue;
        }
        if !stat.is_file || stat.size > STONE_MAX_SEARCH_FILE_BYTES {
            continue;
        }
        files_visited += 1;
        let Ok(bytes) = read_bytes_for_jsonl(&path, "search") else {
            continue;
        };
        if stone_bytes_look_binary(&bytes) || !matcher.is_match(&bytes) {
            continue;
        }
        push_stone_search_line_matches(&mut matches, &path, &bytes, &matcher);
    }
    Ok(matches)
}

fn stone_diff_record(
    left_path: &Path,
    right_path: &Path,
    left_text: &str,
    right_text: &str,
    span: Span,
) -> Value {
    let left_lines = left_text.lines().collect::<Vec<_>>();
    let right_lines = right_text.lines().collect::<Vec<_>>();
    let ops = stone_diff_ops(&left_lines, &right_lines);
    let mut hunks = Vec::new();
    let mut current = StoneDiffHunk::new(1, 1);
    let mut old_line = 1usize;
    let mut new_line = 1usize;
    let mut changed = false;

    for op in ops {
        match op {
            StoneDiffOp::Equal => {
                if current.has_changes {
                    hunks.push(current.to_value(span));
                    current = StoneDiffHunk::new(old_line, new_line);
                }
                old_line += 1;
                new_line += 1;
                current.old_start = old_line;
                current.new_start = new_line;
            }
            StoneDiffOp::Delete(text) => {
                changed = true;
                current.has_changes = true;
                current.old_lines += 1;
                current
                    .lines
                    .push(stone_diff_line("-", Some(old_line), None, text, span));
                old_line += 1;
            }
            StoneDiffOp::Insert(text) => {
                changed = true;
                current.has_changes = true;
                current.new_lines += 1;
                current
                    .lines
                    .push(stone_diff_line("+", None, Some(new_line), text, span));
                new_line += 1;
            }
        }
    }
    if current.has_changes {
        hunks.push(current.to_value(span));
    }

    let mut record = Record::new();
    record.push("changed", Value::bool(changed, span));
    record.push(
        "path_a",
        Value::string(left_path.display().to_string(), span),
    );
    record.push(
        "path_b",
        Value::string(right_path.display().to_string(), span),
    );
    record.push("hunks", Value::list(hunks, span));
    Value::record(record, span)
}

#[derive(Debug)]
enum StoneDiffOp<'a> {
    Equal,
    Delete(&'a str),
    Insert(&'a str),
}

struct StoneDiffHunk {
    old_start: usize,
    new_start: usize,
    old_lines: usize,
    new_lines: usize,
    has_changes: bool,
    lines: Vec<Value>,
}

impl StoneDiffHunk {
    fn new(old_start: usize, new_start: usize) -> Self {
        Self {
            old_start,
            new_start,
            old_lines: 0,
            new_lines: 0,
            has_changes: false,
            lines: Vec::new(),
        }
    }

    fn to_value(self, span: Span) -> Value {
        let mut record = Record::new();
        record.push(
            "old_start",
            Value::int(i64::try_from(self.old_start).unwrap_or(i64::MAX), span),
        );
        record.push(
            "old_lines",
            Value::int(i64::try_from(self.old_lines).unwrap_or(i64::MAX), span),
        );
        record.push(
            "new_start",
            Value::int(i64::try_from(self.new_start).unwrap_or(i64::MAX), span),
        );
        record.push(
            "new_lines",
            Value::int(i64::try_from(self.new_lines).unwrap_or(i64::MAX), span),
        );
        record.push("lines", Value::list(self.lines, span));
        Value::record(record, span)
    }
}

fn stone_diff_line(
    kind: &str,
    old_line: Option<usize>,
    new_line: Option<usize>,
    text: &str,
    span: Span,
) -> Value {
    let mut record = Record::new();
    record.push("kind", Value::string(kind, span));
    record.push(
        "old_line",
        old_line
            .map(|line| Value::int(i64::try_from(line).unwrap_or(i64::MAX), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push(
        "new_line",
        new_line
            .map(|line| Value::int(i64::try_from(line).unwrap_or(i64::MAX), span))
            .unwrap_or_else(|| Value::nothing(span)),
    );
    record.push("text", Value::string(text.to_owned(), span));
    Value::record(record, span)
}

fn stone_diff_ops<'a>(left: &[&'a str], right: &[&'a str]) -> Vec<StoneDiffOp<'a>> {
    let mut lcs = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            lcs[i][j] = if left[i] == right[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            ops.push(StoneDiffOp::Equal);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(StoneDiffOp::Delete(left[i]));
            i += 1;
        } else {
            ops.push(StoneDiffOp::Insert(right[j]));
            j += 1;
        }
    }
    while i < left.len() {
        ops.push(StoneDiffOp::Delete(left[i]));
        i += 1;
    }
    while j < right.len() {
        ops.push(StoneDiffOp::Insert(right[j]));
        j += 1;
    }
    ops
}

fn stone_name_matches(name: &str, contains: Option<&str>, glob: Option<&str>) -> bool {
    contains.is_none_or(|needle| name.contains(needle))
        && glob.is_none_or(|pattern| stone_wildcard_match(pattern, name))
}

fn stone_find_entry_matches(
    path: &Path,
    name: &str,
    stat: &StoneFileStat,
    contains: Option<&str>,
    name_glob: Option<&str>,
    path_glob: Option<&str>,
    kind_filter: Option<&str>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    modified_after_ms: Option<i64>,
    modified_before_ms: Option<i64>,
) -> bool {
    stone_name_matches(name, contains, name_glob)
        && path_glob.is_none_or(|pattern| stone_path_glob_match(pattern, path))
        && kind_filter.is_none_or(|kind| match kind {
            "file" => stat.is_file,
            "dir" => stat.is_dir,
            "symlink" => stat.is_symlink,
            "any" => true,
            _ => false,
        })
        && min_size.is_none_or(|size| stat.size >= size)
        && max_size.is_none_or(|size| stat.size <= size)
        && modified_after_ms.is_none_or(|after| stat.modified_ms.is_some_and(|ms| ms > after))
        && modified_before_ms.is_none_or(|before| stat.modified_ms.is_some_and(|ms| ms < before))
}

fn stone_path_glob_match(pattern: &str, path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(suffix) = pattern.strip_prefix("**/") {
        return stone_wildcard_match(pattern, &text)
            || stone_wildcard_match(suffix, &text)
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| stone_wildcard_match(suffix, name));
    }
    if stone_wildcard_match(pattern, &text) {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| stone_wildcard_match(pattern, name))
}

fn stone_wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut pattern_index, mut text_index) = (0usize, 0usize);
    let mut star_index = None;
    let mut star_text_index = 0usize;

    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_text_index = text_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_text_index += 1;
            text_index = star_text_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }

    pattern_index == pattern.len()
}

fn parse_csv_records(text: &str, limit: Option<usize>) -> Result<Vec<Value>, ShellError> {
    let records = parse_csv_record_fields(text).map_err(|err| stone_error("read_csv", err))?;
    let Some(headers) = records.first() else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    for (record_index, fields) in records.iter().skip(1).enumerate() {
        if limit.is_some_and(|limit| rows.len() >= limit) {
            break;
        }
        if fields.len() == 1 && fields[0].is_empty() {
            continue;
        }
        if fields.len() != headers.len() {
            return Err(stone_error(
                "read_csv",
                format!(
                    "record {} has {} field(s), expected {}",
                    record_index + 2,
                    fields.len(),
                    headers.len()
                ),
            ));
        }
        let mut record = Record::with_capacity(headers.len());
        for (header, field) in headers.iter().zip(fields) {
            record.push(header.clone(), Value::string(field, Span::unknown()));
        }
        rows.push(Value::record(record, Span::unknown()));
    }
    Ok(rows)
}

fn parse_csv_record_fields(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => {
                in_quotes = !in_quotes;
            }
            ',' if !in_quotes => {
                record.push(std::mem::take(&mut field));
            }
            '\n' if !in_quotes => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            '\r' if !in_quotes => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(ch),
        }
    }
    if in_quotes {
        return Err("unterminated quoted field".to_owned());
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

trait StoneFileAdapter {
    fn read_text(&self, path: &Path, max_bytes: usize) -> Result<String, ShellError>;
    fn read_text_range(
        &self,
        path: &Path,
        offset: u64,
        max_bytes: usize,
    ) -> Result<String, ShellError>;
    fn read_text_lines(
        &self,
        path: &Path,
        start_line: usize,
        end_line: Option<usize>,
        max_bytes: usize,
    ) -> Result<String, ShellError>;
    fn write_text(
        &self,
        path: &Path,
        text: &str,
        append: bool,
    ) -> Result<StoneFileWrite, ShellError>;
    fn stat(&self, path: &Path, follow_symlinks: bool) -> Result<StoneFileStat, ShellError>;
    fn stat_optional(
        &self,
        path: &Path,
        follow_symlinks: bool,
    ) -> Result<Option<StoneFileStat>, ShellError>;
    fn list_dir(&self, path: &Path) -> Result<Vec<StoneFileEntry>, ShellError>;
}

#[derive(Clone, Debug)]
struct StoneFileEntry {
    name: String,
    stat: StoneFileStat,
}

#[derive(Clone, Debug)]
struct StoneFileStat {
    path: PathBuf,
    kind: &'static str,
    is_file: bool,
    is_dir: bool,
    is_symlink: bool,
    readonly: bool,
    size: u64,
    modified_ms: Option<i64>,
    accessed_ms: Option<i64>,
    created_ms: Option<i64>,
}

#[derive(Clone, Debug)]
struct StoneFileWrite {
    path: PathBuf,
    bytes: usize,
    append: bool,
}

struct StdStoneFileAdapter;

// SAFETY: this adapter is a zero-sized immutable dispatcher. It owns no state
// and resolves every operation from the calling Stone process.
unsafe impl FreezeSafe for StdStoneFileAdapter {}

static STD_STONE_FILE_ADAPTER: VmFrozen<StdStoneFileAdapter> = VmFrozen::new(StdStoneFileAdapter);

fn stone_file_adapter() -> &'static dyn StoneFileAdapter {
    &*STD_STONE_FILE_ADAPTER
}

impl StoneFileAdapter for StdStoneFileAdapter {
    fn read_text(&self, path: &Path, max_bytes: usize) -> Result<String, ShellError> {
        self.read_text_range(path, 0, max_bytes)
    }

    fn read_text_range(
        &self,
        path: &Path,
        offset: u64,
        max_bytes: usize,
    ) -> Result<String, ShellError> {
        if gateway_runtime::enabled() {
            return gateway_runtime::read_text_range(path, offset, max_bytes);
        }
        let mut file =
            File::open(path).map_err(|err| io_read_stone_error("read_text", err, path))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| io_stone_error("read_text seek", err, path))?;
        let mut bytes = Vec::new();
        file.take(u64::try_from(max_bytes).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|err| io_read_stone_error("read_text", err, path))?;
        decode_bounded_text(bytes, path, "read_text")
    }

    fn read_text_lines(
        &self,
        path: &Path,
        start_line: usize,
        end_line: Option<usize>,
        max_bytes: usize,
    ) -> Result<String, ShellError> {
        if gateway_runtime::enabled() {
            return gateway_runtime::read_text_lines(path, start_line, end_line, max_bytes);
        }
        let file = File::open(path).map_err(|err| io_read_stone_error("read_text", err, path))?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut output = Vec::with_capacity(max_bytes.min(64 * 1024));
        let mut line_number = 1usize;
        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .map_err(|err| io_read_stone_error("read_text", err, path))?;
            if read == 0 {
                break;
            }
            if line_number >= start_line {
                if end_line.is_some_and(|end| line_number > end) {
                    break;
                }
                let remaining = max_bytes.saturating_sub(output.len());
                output.extend_from_slice(&line[..line.len().min(remaining)]);
                if output.len() >= max_bytes {
                    break;
                }
            }
            line_number = line_number.saturating_add(1);
        }
        decode_bounded_text(output, path, "read_text")
    }

    fn write_text(
        &self,
        path: &Path,
        text: &str,
        append: bool,
    ) -> Result<StoneFileWrite, ShellError> {
        if gateway_runtime::enabled() {
            let bytes = gateway_runtime::write_text(path, text, append)?;
            return Ok(StoneFileWrite {
                path: path.to_path_buf(),
                bytes,
                append,
            });
        }
        ensure_parent_dir_for_write("write_text", path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .map_err(|err| io_stone_error("write_text", err, path))?;
        file.write_all(text.as_bytes())
            .map_err(|err| io_stone_error("write_text", err, path))?;
        file.flush()
            .map_err(|err| io_stone_error("write_text", err, path))?;
        Ok(StoneFileWrite {
            path: path.to_path_buf(),
            bytes: text.len(),
            append,
        })
    }

    fn stat(&self, path: &Path, follow_symlinks: bool) -> Result<StoneFileStat, ShellError> {
        if gateway_runtime::enabled() {
            return stone_file_stat_from_gateway(path);
        }
        let metadata = if follow_symlinks {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        }
        .map_err(|err| io_read_stone_error("stat", err, path))?;
        Ok(file_stat_from_metadata(path.to_path_buf(), &metadata))
    }

    fn stat_optional(
        &self,
        path: &Path,
        follow_symlinks: bool,
    ) -> Result<Option<StoneFileStat>, ShellError> {
        if gateway_runtime::enabled() {
            return gateway_runtime::stat_optional_record(path, Span::unknown())?
                .map(stone_file_stat_from_value)
                .transpose();
        }
        let metadata = if follow_symlinks {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        };
        match metadata {
            Ok(metadata) => Ok(Some(file_stat_from_metadata(path.to_path_buf(), &metadata))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_stone_error("stat", error, path)),
        }
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<StoneFileEntry>, ShellError> {
        if gateway_runtime::enabled() {
            return gateway_runtime::list_dir_records(path, Span::unknown())?
                .into_iter()
                .map(stone_file_entry_from_value)
                .collect();
        }
        let mut entries = fs::read_dir(path)
            .map_err(|err| io_read_stone_error("list_dir", err, path))?
            .map(|entry| {
                let entry = entry.map_err(|err| io_stone_error("list_dir", err, path))?;
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .map_err(|err| io_stone_error("list_dir", err, &path))?;
                Ok(StoneFileEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    stat: file_stat_from_metadata(path, &metadata),
                })
            })
            .collect::<Result<Vec<_>, ShellError>>()?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }
}

fn file_stat_from_metadata(path: PathBuf, metadata: &fs::Metadata) -> StoneFileStat {
    StoneFileStat {
        path,
        kind: file_type_name(metadata),
        is_file: metadata.is_file(),
        is_dir: metadata.is_dir(),
        is_symlink: metadata.file_type().is_symlink(),
        readonly: metadata.permissions().readonly(),
        size: metadata.len(),
        modified_ms: system_time_ms(metadata.modified().ok()),
        accessed_ms: system_time_ms(metadata.accessed().ok()),
        created_ms: system_time_ms(metadata.created().ok()),
    }
}

fn decode_bounded_text(
    mut bytes: Vec<u8>,
    path: &Path,
    operation: &str,
) -> Result<String, ShellError> {
    if let Err(err) = std::str::from_utf8(&bytes) {
        if err.error_len().is_some() {
            return Err(stone_error(
                operation,
                format!("{}: invalid UTF-8: {err}", path.display()),
            ));
        }
        bytes.truncate(err.valid_up_to());
    }
    String::from_utf8(bytes).map_err(|err| {
        stone_error(
            operation,
            format!("{}: invalid UTF-8: {err}", path.display()),
        )
    })
}

fn stone_file_stat_from_gateway(path: &Path) -> Result<StoneFileStat, ShellError> {
    let value = gateway_runtime::stat_record(path, Span::unknown())?;
    stone_file_stat_from_value(value)
}

fn stone_file_entry_from_value(value: Value) -> Result<StoneFileEntry, ShellError> {
    let stat = stone_file_stat_from_value(value)?;
    let name = stat
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| stat.path.display().to_string());
    Ok(StoneFileEntry { name, stat })
}

fn stone_file_stat_from_value(value: Value) -> Result<StoneFileStat, ShellError> {
    let Value::Record { val, .. } = value else {
        return Err(stone_error(
            "gateway stat",
            "Gateway stat returned a non-record",
        ));
    };
    let path = record_string(&val, "path")?;
    let kind = match record_string(&val, "type")?.as_str() {
        "file" => "file",
        "dir" => "dir",
        "symlink" => "symlink",
        _ => "other",
    };
    Ok(StoneFileStat {
        path: PathBuf::from(path),
        kind,
        is_file: record_bool(&val, "is_file"),
        is_dir: record_bool(&val, "is_dir"),
        is_symlink: record_bool(&val, "is_symlink"),
        readonly: record_bool(&val, "readonly"),
        size: record_i64(&val, "size").unwrap_or(0).max(0) as u64,
        modified_ms: record_i64(&val, "modified_ms"),
        accessed_ms: record_i64(&val, "accessed_ms"),
        created_ms: record_i64(&val, "created_ms"),
    })
}

fn record_string(record: &Record, key: &str) -> Result<String, ShellError> {
    record
        .get(key)
        .and_then(|value| value.coerce_string().ok())
        .ok_or_else(|| stone_error("gateway stat", format!("missing string field `{key}`")))
}

fn record_bool(record: &Record, key: &str) -> bool {
    matches!(record.get(key), Some(Value::Bool { val: true, .. }))
}

fn record_i64(record: &Record, key: &str) -> Option<i64> {
    match record.get(key) {
        Some(Value::Int { val, .. }) => Some(*val),
        _ => None,
    }
}

fn file_entry_record(entry: StoneFileEntry, span: Span) -> Value {
    let mut record = Record::with_capacity(7);
    record.push("name", Value::string(entry.name, span));
    record.push(
        "path",
        Value::string(entry.stat.path.display().to_string(), span),
    );
    record.push("type", Value::string(entry.stat.kind, span));
    record.push("is_file", Value::bool(entry.stat.is_file, span));
    record.push("is_dir", Value::bool(entry.stat.is_dir, span));
    record.push("is_symlink", Value::bool(entry.stat.is_symlink, span));
    record.push(
        "size",
        Value::int(i64::try_from(entry.stat.size).unwrap_or(i64::MAX), span),
    );
    Value::record(record, span)
}

fn search_match_record(path: &Path, line: usize, text: &str) -> Value {
    let span = Span::unknown();
    let mut record = Record::with_capacity(3);
    record.push("path", Value::string(path.display().to_string(), span));
    record.push(
        "line",
        Value::int(i64::try_from(line).unwrap_or(i64::MAX), span),
    );
    record.push("text", Value::string(text.to_string(), span));
    Value::record(record, span)
}

enum StoneSearchMatcher {
    Literal(Vec<u8>),
    Regex(Regex),
}

impl StoneSearchMatcher {
    fn new(needle: &str, regex: bool) -> Result<Self, ShellError> {
        if regex {
            Regex::new(needle)
                .map(Self::Regex)
                .map_err(|err| stone_error("search", format!("invalid regex: {err}")))
        } else {
            Ok(Self::Literal(needle.as_bytes().to_vec()))
        }
    }

    fn is_match(&self, bytes: &[u8]) -> bool {
        match self {
            Self::Literal(needle) => memchr::memmem::Finder::new(needle).find(bytes).is_some(),
            Self::Regex(regex) => regex.is_match(bytes),
        }
    }
}

fn push_stone_search_line_matches(
    matches: &mut Vec<Value>,
    path: &Path,
    content: &[u8],
    matcher: &StoneSearchMatcher,
) {
    let mut line_number = 1usize;
    let mut start = 0usize;
    for end in memchr::memchr_iter(b'\n', content).chain(std::iter::once(content.len())) {
        let line = trim_byte_line_end(&content[start..end]);
        if matcher.is_match(line) {
            matches.push(search_match_record(
                path,
                line_number,
                &String::from_utf8_lossy(line),
            ));
            if matches.len() >= STONE_MAX_SEARCH_MATCHES {
                break;
            }
        }
        if end == content.len() {
            break;
        }
        start = end + 1;
        line_number += 1;
    }
}

fn trim_byte_line_end(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn stone_bytes_look_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(1024).any(|byte| *byte == 0)
}

fn file_stat_record(stat: StoneFileStat, span: Span) -> Value {
    let mut record = Record::with_capacity(10);
    record.push("path", Value::string(stat.path.display().to_string(), span));
    record.push("type", Value::string(stat.kind, span));
    record.push("is_file", Value::bool(stat.is_file, span));
    record.push("is_dir", Value::bool(stat.is_dir, span));
    record.push("is_symlink", Value::bool(stat.is_symlink, span));
    record.push("readonly", Value::bool(stat.readonly, span));
    record.push(
        "size",
        Value::int(i64::try_from(stat.size).unwrap_or(i64::MAX), span),
    );
    record.push("modified_ms", optional_i64_value(stat.modified_ms, span));
    record.push("accessed_ms", optional_i64_value(stat.accessed_ms, span));
    record.push("created_ms", optional_i64_value(stat.created_ms, span));
    Value::record(record, span)
}

fn file_write_record(write: StoneFileWrite, span: Span) -> Value {
    let mut record = Record::with_capacity(3);
    record.push(
        "path",
        Value::string(write.path.display().to_string(), span),
    );
    record.push(
        "bytes",
        Value::int(i64::try_from(write.bytes).unwrap_or(i64::MAX), span),
    );
    record.push("append", Value::bool(write.append, span));
    Value::record(record, span)
}

fn file_type_name(metadata: &fs::Metadata) -> &'static str {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        "dir"
    } else if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_file() {
        "file"
    } else {
        "other"
    }
}

fn system_time_ms(time: Option<std::time::SystemTime>) -> Option<i64> {
    let Some(time) = time else {
        return None;
    };
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => Some(i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)),
        Err(_) => None,
    }
}

fn optional_i64_value(value: Option<i64>, span: Span) -> Value {
    match value {
        Some(value) => Value::int(value, span),
        None => Value::nothing(span),
    }
}

pub(crate) fn io_stone_error(kind: &str, err: std::io::Error, path: &Path) -> ShellError {
    let path = path.to_path_buf();
    ShellError::Io(
        nu_protocol::shell_error::io::IoError::new_internal_with_path(
            err,
            format!("Stone {kind} I/O error at {}", path.display()),
            path,
        ),
    )
}

pub(crate) fn io_read_stone_error(kind: &str, err: std::io::Error, path: &Path) -> ShellError {
    if err.kind() != ErrorKind::NotFound {
        return io_stone_error(kind, err, path);
    }
    let suggestions = nearby_read_path_suggestions(path, 5);
    if suggestions.is_empty() {
        return io_stone_error(kind, err, path);
    }
    ShellError::Generic(
        GenericError::new_internal(
            format!("Stone {kind} I/O error"),
            format!(
                "{}: {}. Did you mean {}?",
                path.display(),
                err,
                suggestions.join(" or ")
            ),
        )
        .with_code("io_error"),
    )
}

fn nearby_read_path_suggestions(path: &Path, limit: usize) -> Vec<String> {
    let Some(root) = nearest_existing_search_root(path) else {
        return Vec::new();
    };
    if root == Path::new("/") {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    if let Some(expected_name) = path.file_name().and_then(|name| name.to_str()) {
        collect_read_path_suggestions(&root, limit, &mut candidates, &mut |candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == expected_name)
        });
    }
    if candidates.len() < limit {
        if let Some(expected_suffix) = path.extension().and_then(|suffix| suffix.to_str()) {
            collect_read_path_suggestions(&root, limit, &mut candidates, &mut |candidate| {
                candidate
                    .extension()
                    .and_then(|suffix| suffix.to_str())
                    .is_some_and(|suffix| suffix == expected_suffix)
            });
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates.truncate(limit);
    candidates
}

fn nearest_existing_search_root(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() || !parent.is_dir() {
        return None;
    }
    Some(parent.to_path_buf())
}

fn collect_read_path_suggestions(
    current: &Path,
    limit: usize,
    candidates: &mut Vec<String>,
    matches: &mut dyn FnMut(&Path) -> bool,
) {
    if candidates.len() >= limit {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if candidates.len() >= limit {
            return;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_read_path_suggestions(&path, limit, candidates, matches);
        } else if path.is_file() && matches(&path) {
            candidates.push(path.display().to_string());
        }
    }
}

pub(crate) fn ensure_parent_dir_for_write(kind: &str, path: &Path) -> Result<(), ShellError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() || parent.exists() {
        return Ok(());
    }
    fs::create_dir_all(parent).map_err(|err| io_stone_error(kind, err, parent))
}

fn stone_error(kind: &str, message: impl Into<String>) -> ShellError {
    ShellError::Generic(
        GenericError::new_internal(format!("Stone {kind} error"), message.into())
            .with_code("stone_script_error"),
    )
}
