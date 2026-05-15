// SPDX-License-Identifier: MIT OR Apache-2.0

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::json::{nu_to_json_value, parse_json_bytes, pipeline_to_json_text};
use nu_engine::command_prelude::*;
use nu_protocol::shell_error::{generic::GenericError, io::IoError};
use nu_protocol::{ast::PathMember, engine::StateWorkingSet, Record};

const MAX_FIND_ENTRIES: usize = 4096;
const MAX_SEARCH_FILES: usize = 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_MATCHES: usize = 1000;

pub fn register_commands(working_set: &mut StateWorkingSet<'_>) {
    for decl in [
        Box::new(Cd) as Box<dyn Command>,
        Box::new(Edit),
        Box::new(Echo),
        Box::new(Emit),
        Box::new(Fail),
        Box::new(Find),
        Box::new(First),
        Box::new(FromJson),
        Box::new(Get),
        Box::new(Help),
        Box::new(Last),
        Box::new(Ls),
        Box::new(Mkdir),
        Box::new(Open),
        Box::new(Pwd),
        Box::new(Rm),
        Box::new(RunExternal),
        Box::new(Save),
        Box::new(Search),
        Box::new(Sort),
        Box::new(ToJson),
        Box::new(ToJsonl),
        Box::new(Log),
        Box::new(Where),
    ] {
        working_set.add_decl(decl);
    }
}

#[derive(Clone)]
struct Cd;

#[derive(Clone)]
struct Edit;

#[derive(Clone)]
struct Echo;

#[derive(Clone)]
struct Emit;

#[derive(Clone)]
struct Fail;

#[derive(Clone)]
struct Find;

#[derive(Clone)]
struct First;

#[derive(Clone)]
struct FromJson;

#[derive(Clone)]
struct Get;

#[derive(Clone)]
struct Help;

#[derive(Clone)]
struct Last;

#[derive(Clone)]
struct Ls;

#[derive(Clone)]
struct Mkdir;

#[derive(Clone)]
struct Open;

#[derive(Clone)]
struct Pwd;

#[derive(Clone)]
struct Rm;

#[derive(Clone)]
struct RunExternal;

#[derive(Clone)]
struct Save;

#[derive(Clone)]
struct Search;

#[derive(Clone)]
struct Sort;

#[derive(Clone)]
struct ToJson;

#[derive(Clone)]
struct ToJsonl;

#[derive(Clone)]
struct Log;

#[derive(Clone)]
struct Where;

impl Command for Cd {
    fn name(&self) -> &str {
        "cd"
    }

    fn description(&self) -> &str {
        "Change the current working directory."
    }

    fn signature(&self) -> Signature {
        Signature::build("cd")
            .input_output_types(vec![(Type::Nothing, Type::Nothing)])
            .optional("path", SyntaxShape::Directory, "Directory to enter.")
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let path_arg = call.opt::<Spanned<String>>(engine_state, stack, 0)?;
        let path_span = path_arg.as_ref().map_or(call.head, |path| path.span);
        let current = engine_state.cwd(Some(stack))?.into_std_path_buf();
        let target = match path_arg.as_ref() {
            Some(path) if path.item == "-" => stack
                .get_env_var(engine_state, "OLDPWD")
                .ok_or_else(|| ShellError::MissingParameter {
                    param_name: "OLDPWD".into(),
                    span: path.span,
                })?
                .to_path()?,
            Some(path) => resolve_path(engine_state, stack, &path.item)?,
            None => home_dir(engine_state, stack, call.head)?,
        };

        let target = fs::canonicalize(&target).map_err(|err| io_error(err, path_span, &target))?;

        if let Some(oldpwd) = stack.get_env_var(engine_state, "PWD") {
            stack.add_env_var("OLDPWD".into(), oldpwd.clone());
        } else {
            stack.add_env_var(
                "OLDPWD".into(),
                Value::string(current.to_string_lossy(), Span::unknown()),
            );
        }

        stack.set_cwd(target)?;
        Ok(PipelineData::empty())
    }
}

impl Command for Edit {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Apply an exact text replacement to a UTF-8 file."
    }

    fn signature(&self) -> Signature {
        Signature::build("edit")
            .input_output_types(vec![(Type::Nothing, Type::record())])
            .required("path", SyntaxShape::Filepath, "File to edit.")
            .required("old", SyntaxShape::String, "Text to replace.")
            .required("new", SyntaxShape::String, "Replacement text.")
            .switch("all", "Replace all matches.", Some('a'))
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let path = call.req::<Spanned<String>>(engine_state, stack, 0)?;
        let old = call.req::<Spanned<String>>(engine_state, stack, 1)?;
        let new = call.req::<Spanned<String>>(engine_state, stack, 2)?;
        let replace_all = call.has_flag(engine_state, stack, "all")?;

        if old.item.is_empty() {
            return Err(ShellError::Generic(
                GenericError::new("Invalid edit", "old text must not be empty", old.span)
                    .with_code("edit_empty_match"),
            ));
        }

        let target = resolve_path(engine_state, stack, &path.item)?;
        let content =
            fs::read_to_string(&target).map_err(|err| io_error(err, path.span, &target))?;
        let matches = content.match_indices(&old.item).count();
        if matches == 0 {
            return Err(ShellError::Generic(
                GenericError::new("Edit failed", "old text was not found", old.span)
                    .with_code("edit_match_not_found"),
            ));
        }
        if matches > 1 && !replace_all {
            return Err(ShellError::Generic(
                GenericError::new(
                    "Edit failed",
                    "old text matched more than once; pass --all to replace all matches",
                    old.span,
                )
                .with_code("edit_multiple_matches"),
            ));
        }

        let edited = if replace_all {
            content.replace(&old.item, &new.item)
        } else {
            content.replacen(&old.item, &new.item, 1)
        };
        fs::write(&target, edited.as_bytes()).map_err(|err| io_error(err, path.span, &target))?;

        let replacements = if replace_all { matches } else { 1 };
        let mut record = Record::with_capacity(3);
        record.push(
            "path",
            Value::string(target.display().to_string(), call.head),
        );
        record.push(
            "replacements",
            Value::int(i64::try_from(replacements).unwrap_or(i64::MAX), call.head),
        );
        record.push(
            "bytes",
            Value::int(i64::try_from(edited.len()).unwrap_or(i64::MAX), call.head),
        );
        Ok(Value::record(record, call.head).into_pipeline_data())
    }
}

impl Command for Echo {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Return the given values."
    }

    fn signature(&self) -> Signature {
        Signature::build("echo")
            .input_output_types(vec![(Type::Nothing, Type::Any)])
            .rest("values", SyntaxShape::Any, "Values to return.")
            .category(Category::Core)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let mut args = call.rest::<Value>(engine_state, stack, 0)?;
        let value = match args.len() {
            0 => Value::string("", call.head),
            1 => args.pop().expect("single echo argument"),
            _ => Value::list(args, call.head),
        };

        Ok(value.into_pipeline_data())
    }
}

impl Command for Emit {
    fn name(&self) -> &str {
        "emit"
    }

    fn description(&self) -> &str {
        "Return an explicit structured task result value."
    }

    fn signature(&self) -> Signature {
        Signature::build("emit")
            .input_output_types(vec![(Type::Any, Type::Any)])
            .optional("value", SyntaxShape::Any, "Value to emit.")
            .category(Category::Core)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        match call.opt::<Value>(engine_state, stack, 0)? {
            Some(value) => Ok(value.into_pipeline_data()),
            None => Ok(input),
        }
    }
}

impl Command for Fail {
    fn name(&self) -> &str {
        "fail"
    }

    fn description(&self) -> &str {
        "Fail the current task intentionally."
    }

    fn signature(&self) -> Signature {
        Signature::build("fail")
            .input_output_types(vec![(Type::Any, Type::Nothing)])
            .required("message", SyntaxShape::String, "Failure message.")
            .named(
                "code",
                SyntaxShape::String,
                "Task-specific failure code.",
                None,
            )
            .named(
                "detail",
                SyntaxShape::Any,
                "Structured failure detail.",
                None,
            )
            .category(Category::Core)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let message = call.req::<Spanned<String>>(engine_state, stack, 0)?;
        let code = call.get_flag::<Spanned<String>>(engine_state, stack, "code")?;
        let detail = call.get_flag::<Value>(engine_state, stack, "detail")?;

        let mut error =
            GenericError::new("Task failure", message.item, message.span).with_code("task_failure");
        if let Some(code) = code {
            error = error.with_help(format!("code={}", code.item));
        }
        if let Some(detail) = detail {
            error = error.with_inner(vec![ShellError::Generic(
                GenericError::new_internal(
                    "Task failure detail",
                    nu_to_json_value(&detail).to_string(),
                )
                .with_code("task_failure_detail"),
            )]);
        }

        Err(ShellError::Generic(error))
    }
}

impl Command for Find {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Find files and directories under a root path by name."
    }

    fn signature(&self) -> Signature {
        Signature::build("find")
            .input_output_types(vec![(Type::Nothing, Type::table())])
            .required("root", SyntaxShape::Directory, "Root path to walk.")
            .named(
                "name_contains",
                SyntaxShape::String,
                "Only include entries whose name contains this substring.",
                None,
            )
            .named(
                "name_glob",
                SyntaxShape::String,
                "Only include entries whose name matches this * and ? pattern.",
                None,
            )
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let root = call.req::<Spanned<String>>(engine_state, stack, 0)?;
        let name_contains =
            call.get_flag::<Spanned<String>>(engine_state, stack, "name_contains")?;
        let name_glob = call.get_flag::<Spanned<String>>(engine_state, stack, "name_glob")?;
        let root_path = resolve_path(engine_state, stack, &root.item)?;
        let mut entries = Vec::new();
        let mut queue = VecDeque::from([root_path.clone()]);

        while let Some(path) = queue.pop_front() {
            if entries.len() >= MAX_FIND_ENTRIES {
                break;
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|err| io_error(err, root.span, &path))?;
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let include = find_name_matches(
                &name,
                name_contains.as_ref().map(|needle| needle.item.as_str()),
                name_glob.as_ref().map(|pattern| pattern.item.as_str()),
            );
            if include {
                entries.push(find_entry_value(
                    name,
                    path.clone(),
                    metadata.clone(),
                    call.head,
                ));
            }
            if metadata.is_dir() {
                let mut children = fs::read_dir(&path)
                    .map_err(|err| io_error(err, root.span, &path))?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| io_error(err, root.span, &path))?;
                children.sort();
                queue.extend(children);
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
        Ok(Value::list(entries, call.head).into_pipeline_data())
    }
}

impl Command for First {
    fn name(&self) -> &str {
        "first"
    }

    fn description(&self) -> &str {
        "Return the first item or items from a list."
    }

    fn signature(&self) -> Signature {
        Signature::build("first")
            .input_output_types(vec![(Type::List(Box::new(Type::Any)), Type::Any)])
            .optional("rows", SyntaxShape::Int, "Number of items to return.")
            .category(Category::Filters)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        mut input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let rows = parse_row_count(call.opt::<Spanned<i64>>(engine_state, stack, 0)?)?;
        let head = call.head;
        let metadata = input.take_metadata();

        match input {
            PipelineData::ListStream(stream, ..) => {
                let span = stream.span();
                let mut iter = stream.into_iter();
                if rows.count == 1 && !rows.explicit {
                    Ok(iter
                        .next()
                        .unwrap_or_else(|| Value::nothing(head))
                        .into_pipeline_data_with_metadata(metadata))
                } else {
                    Ok(iter.take(rows.count).into_pipeline_data_with_metadata(
                        span,
                        engine_state.signals().clone(),
                        metadata,
                    ))
                }
            }
            other => {
                let value = other.into_value(head)?;
                let span = value.span();
                match value {
                    Value::List { mut vals, .. } => {
                        if rows.count == 1 && !rows.explicit {
                            Ok(vals
                                .drain(..)
                                .next()
                                .unwrap_or_else(|| Value::nothing(head))
                                .into_pipeline_data_with_metadata(metadata))
                        } else {
                            vals.truncate(rows.count);
                            Ok(Value::list(vals, span).into_pipeline_data_with_metadata(metadata))
                        }
                    }
                    other => Err(type_mismatch("list", other.get_type().to_string(), head)),
                }
            }
        }
    }
}

impl Command for FromJson {
    fn name(&self) -> &str {
        "from_json"
    }

    fn description(&self) -> &str {
        "Parse JSON text into structured values."
    }

    fn signature(&self) -> Signature {
        Signature::build("from_json")
            .input_output_types(vec![(Type::Any, Type::Any)])
            .category(Category::Formats)
    }

    fn run(
        &self,
        _engine_state: &EngineState,
        _stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let value = parse_json_input(input, call.head)?;
        Ok(value.into_pipeline_data())
    }
}

impl Command for Get {
    fn name(&self) -> &str {
        "get"
    }

    fn description(&self) -> &str {
        "Extract structured data using a cell path."
    }

    fn signature(&self) -> Signature {
        Signature::build("get")
            .input_output_types(vec![(Type::Any, Type::Any)])
            .required("cell_path", SyntaxShape::CellPath, "Cell path to extract.")
            .category(Category::Filters)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let path = call.req::<CellPath>(engine_state, stack, 0)?;
        let head = call.head;
        let has_int_member = path
            .members
            .iter()
            .any(|member| matches!(member, PathMember::Int { .. }));

        if has_int_member {
            let value = input.into_value(head)?;
            Ok(value
                .follow_cell_path(&path.members)?
                .into_owned()
                .into_pipeline_data())
        } else {
            let members = path.members;
            input.map(
                move |value| match value.follow_cell_path(&members) {
                    Ok(found) => found.into_owned(),
                    Err(err) => Value::error(err, head),
                },
                engine_state.signals(),
            )
        }
    }
}

impl Command for Help {
    fn name(&self) -> &str {
        "help"
    }

    fn description(&self) -> &str {
        "Show the Stone syntax and primitive contract."
    }

    fn signature(&self) -> Signature {
        Signature::build("help")
            .input_output_types(vec![(Type::Nothing, Type::record())])
            .optional(
                "name",
                SyntaxShape::String,
                "Optional Stone builtin or syntax topic to describe.",
            )
            .category(Category::Core)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let name = call.opt::<String>(engine_state, stack, 0)?;
        let value = match name {
            Some(name) => stone_help_topic(&name, call.head),
            None => stone_help_overview(call.head),
        };
        Ok(value.into_pipeline_data())
    }
}

struct StoneHelpEntry {
    name: &'static str,
    signature: &'static str,
    use_when: &'static str,
    examples: &'static [&'static str],
    avoid: &'static [&'static str],
    aliases: &'static [&'static str],
}

struct StoneHelpTopic {
    name: &'static str,
    summary: &'static str,
    bullets: &'static [&'static str],
}

const STONE_HELP_ENTRIES: &[StoneHelpEntry] = &[
    StoneHelpEntry {
        name: "help",
        signature: r#"help(name: str? = None) -> record"#,
        use_when: "Use to inspect Stone syntax, constraints, and examples before writing scripts.",
        examples: &[r#"emit(help())"#, r#"emit(help("save"))"#],
        avoid: &["Do not assume Python stdlib or Nu pipe syntax; ask help for the Stone function."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "emit",
        signature: "emit(value: Any? = pipeline_value) -> Any",
        use_when: "Use to return a structured result from an Stone script or MCP call.",
        examples: &[r#"emit({"ok": True, "path": "/app/out.json"})"#],
        avoid: &[
            "Do not print final structured results; emit them.",
            "Do not emit large lists just to inspect them; bind the list and emit len/head/tail summaries.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "fail",
        signature: "fail(message: str, code: str? = None, detail: Any? = None) -> never",
        use_when: "Use to intentionally mark a task as failed with a clear message.",
        examples: &[r#"fail("missing required input", code="missing_input")"#],
        avoid: &["Do not use fail for ordinary recoverable probes; return/emit diagnostics instead."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "echo",
        signature: "echo(value: Any, ...values: Any) -> Any | list",
        use_when: "Use for quick literal values in small probes.",
        examples: &[r#"emit(echo("hello"))"#, r#"emit(echo("name", 3))"#],
        avoid: &["Prefer emit(value) for final task results."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "int",
        signature: "int(value: Any) -> int",
        use_when: "Use to explicitly convert strings or floats before integer arithmetic.",
        examples: &[r#"qty = int(row["qty"])"#],
        avoid: &["Do not rely on automatic string-number coercion."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "float",
        signature: "float(value: Any) -> float",
        use_when: "Use to explicitly convert strings or integers before floating-point arithmetic.",
        examples: &[r#"amount = float(row["amount"])"#],
        avoid: &["Use parse_float(value, default) when malformed input should not fail the script."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "str",
        signature: "str(value: Any) -> str",
        use_when: "Use to explicitly convert values before string concatenation or formatted text output.",
        examples: &[r#"line = row["name"] + "," + str(count)"#],
        avoid: &["Do not concatenate strings and numbers without str()."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "len",
        signature: "len(value: str | list | record) -> int",
        use_when: "Use for counts and compact summaries of large values.",
        examples: &[r#"emit({"rows": len(rows), "sample": head(rows, 5)})"#],
        avoid: &["Do not emit a full large list just to learn its length."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "list",
        signature: "list(value: list | record) -> list",
        use_when: "Use to materialize a list view of an existing list or record keys.",
        examples: &[r#"names = list(counts)"#],
        avoid: &["Use keys(record), values(record), or items(record) when the intended record view is specific."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "min",
        signature: "min(value: Any, ...values: Any) -> Any",
        use_when: "Use for the smallest comparable value.",
        examples: &[r#"lowest = min(a, b, c)"#],
        avoid: &["Do not compare unrelated types such as strings and numbers."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "max",
        signature: "max(value: Any, ...values: Any) -> Any",
        use_when: "Use for the largest comparable value.",
        examples: &[r#"highest = max(a, b, c)"#],
        avoid: &["Use sort(rows, key=...) for top-N records."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "round",
        signature: "round(value: int | float, digits: int = 0) -> int | float",
        use_when: "Use for rounded numeric outputs, especially task-required decimal precision.",
        examples: &[r#"avg = round(total / count, 2)"#],
        avoid: &["Do not pass strings; convert with float() first."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "parse_int",
        signature: "parse_int(value: Any, default: Any) -> int | Any",
        use_when: "Use when bad integer input should fall back instead of failing.",
        examples: &[r#"qty = parse_int(row["qty"], 0)"#],
        avoid: &["Use int(value) when malformed input should be treated as an error."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "parse_float",
        signature: "parse_float(value: Any, default: Any) -> float | Any",
        use_when: "Use when bad floating-point input should fall back instead of failing.",
        examples: &[r#"amount = parse_float(row["amount"], 0.0)"#],
        avoid: &["Use float(value) when malformed input should be treated as an error."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "pwd",
        signature: "pwd() -> str",
        use_when: "Use to inspect the current Stone working directory.",
        examples: &[r#"emit(pwd())"#],
        avoid: &["Use absolute /app paths for task inputs when possible."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "cd",
        signature: "cd(path: str) -> str",
        use_when: "Use to change the current Stone working directory for later session calls.",
        examples: &[r#"cd("/app/subdir")"#],
        avoid: &["For one command only, prefer run(argv, cwd=...) instead of changing session cwd."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "ls",
        signature: "ls(path: str? = cwd) -> list[record]",
        use_when: "Use for shallow directory inspection. Alias for list_dir.",
        examples: &[r#"entries = ls("/app")"#],
        avoid: &["Use find(root, glob) for recursive discovery."],
        aliases: &["list_dir"],
    },
    StoneHelpEntry {
        name: "open",
        signature: r#"open(path: str, mode: "r" | "w" | "a" = "r") -> file"#,
        use_when: "Use for streaming/line-oriented text reads or simple text writes.",
        examples: &[
            r#"text = open("/app/input.txt").read()"#,
            r#"lines = []
for line in open("/app/input.txt"):
    lines.append(line.strip())"#,
            r#"open("/app/out.txt", "w").write("done\n")"#,
        ],
        avoid: &[
            "Do not emit/return file objects; read them first.",
            "For JSON/CSV/JSONL, prefer the structured helpers.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "cat",
        signature: "cat(path: str | file_record) -> str",
        use_when: "Use for quick whole-file UTF-8 text reads.",
        examples: &[r#"text = cat("/app/report.txt")"#],
        avoid: &["Use read_file(path, max_bytes=...) for bounded reads and structured helpers for JSON/CSV/JSONL."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_file",
        signature: "read_file(path: str, max_bytes: int? = None) -> str",
        use_when: "Use for bounded plain-text reads. Alias for read_text.",
        examples: &[r#"text = read_file("/app/report.txt")"#],
        avoid: &["Do not parse JSON/CSV manually if read_json/read_jsonl/read_csv fits."],
        aliases: &["read_text"],
    },
    StoneHelpEntry {
        name: "write_file",
        signature: "write_file(path: str, text: str, append: bool = False) -> record",
        use_when: "Use for writing final text outputs. Alias for write_text.",
        examples: &[r#"write_file("/app/report.txt", "ok\n")"#],
        avoid: &[
            "Do not json_dumps then write_file for JSON outputs; prefer write_json.",
            "Pass append=True only when the task explicitly needs append behavior.",
        ],
        aliases: &["write_text"],
    },
    StoneHelpEntry {
        name: "find",
        signature: "find(root: str, name_glob: str = '*', path_glob: str? = None, type: str? = None) -> list[record]",
        use_when: "Use to discover task input files by name/path glob and optional type, size, or modified-time filters.",
        examples: &[
            r#"files = find("/app", "*.jsonl")"#,
            r#"py = find("/app", path_glob="**/*.py", type="file")"#,
            r#"rows = read_jsonl(files[0])"#,
        ],
        avoid: &["Do not import glob/pathlib/os; use find instead."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "diff",
        signature: "diff(path_a: str, path_b: str) -> record",
        use_when: "Use to compare two text files and inspect structured hunks with line numbers.",
        examples: &[r#"changes = diff("expected.txt", "actual.txt")"#],
        avoid: &["For binary files or very large files, use run([\"diff\", ...]) explicitly."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "search",
        signature: "search(root: str, needle: str) -> list[record]",
        use_when: "Use for bounded literal text search across UTF-8 files.",
        examples: &[r#"matches = search("/app", "ERROR")"#],
        avoid: &["Use read_json/read_csv/read_jsonl for structured data filtering."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "stat",
        signature: "stat(path: str, follow_symlinks: bool = False) -> record",
        use_when: "Use to inspect file type, size, and timestamps.",
        examples: &[r#"info = stat("/app/input.txt")"#],
        avoid: &["Use ls/list_dir when you need multiple directory entries."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_csv",
        signature: "read_csv(path_or_file: str | record, limit: int? = None) -> list[record]",
        use_when: "Use for headered CSV. Values are strings.",
        examples: &[
            r#"rows = read_csv("/app/input.csv")"#,
            r#"sample = read_csv("/app/input.csv", limit=5)"#,
        ],
        avoid: &["Convert with int()/float() before arithmetic; Stone does not coerce strings."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_json",
        signature: "read_json(path_or_file: str | record) -> Any",
        use_when: "Use for JSON files.",
        examples: &[r#"data = read_json("/app/config.json")"#],
        avoid: &["Do not import json; use read_json/json_loads/json_dumps."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "read_jsonl",
        signature: "read_jsonl(path_or_file: str | record, limit: int? = None) -> list[Any]",
        use_when: "Use for JSON Lines data. Prefer this over manual line parsing.",
        examples: &[
            r#"rows = read_jsonl("/app/events.jsonl")"#,
            r#"sample = read_jsonl("/app/events.jsonl", limit=5)"#,
        ],
        avoid: &["Do not emit huge row lists to inspect them; use a limit for samples."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "json_loads",
        signature: "json_loads(text: str) -> Any",
        use_when: "Use to parse JSON text already held in a string.",
        examples: &[r#"value = json_loads(text)"#],
        avoid: &["Use read_json(path) for JSON files."],
        aliases: &["from_json"],
    },
    StoneHelpEntry {
        name: "json_dumps",
        signature: "json_dumps(value: Any) -> str",
        use_when: "Use to serialize a value to compact JSON text.",
        examples: &[r#"text = json_dumps({"ok": True})"#],
        avoid: &["Use write_json(path, value) for final JSON files."],
        aliases: &["to_json"],
    },
    StoneHelpEntry {
        name: "write_json",
        signature: "write_json(path: str, value: Any) -> int",
        use_when: "Use for final JSON outputs from dictionaries/lists.",
        examples: &[r#"write_json("/app/out.json", {"ok": True, "items": rows})"#],
        avoid: &["Do not wrap values in json_dumps before write_json."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "write_jsonl",
        signature: "write_jsonl(path: str, rows: list[Any]) -> int",
        use_when: "Use for JSON Lines output files.",
        examples: &[r#"write_jsonl("/app/out.jsonl", rows)"#],
        avoid: &["Pass a list of row values, not pre-joined JSONL text."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "to_jsonl",
        signature: "to_jsonl(rows: list[Any]) -> str",
        use_when: "Use to serialize row values to JSON Lines text already held in memory.",
        examples: &[r#"text = to_jsonl(rows)"#],
        avoid: &["Use write_jsonl(path, rows) for final JSONL files."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "save",
        signature: "save(value: Any, path: str, append: bool = False, force: bool = False) -> record",
        use_when: "Use to write an explicit value to a file when write_file/write_json do not fit.",
        examples: &[r#"save(to_json(rows), "/app/rows.json", force=True)"#],
        avoid: &["Do not rely on Nu pipeline input; pass the value explicitly as the first argument."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "edit",
        signature: "edit(path: str, old: str, new: str, all: bool = False) -> record",
        use_when: "Use for exact text replacement in a UTF-8 file.",
        examples: &[r#"edit("/app/config.txt", "debug=false", "debug=true")"#],
        avoid: &["Do not pass empty old text; read a sample first if the replacement is risky."],
        aliases: &["edit_file"],
    },
    StoneHelpEntry {
        name: "mkdir",
        signature: "mkdir(path: str, ...paths: str) -> None",
        use_when: "Use to create directories, including parents.",
        examples: &[r#"mkdir("/app/out/logs")"#],
        avoid: &["Use write_file/write_json when only parent creation is needed for a file."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "rm",
        signature: "rm(path: str, ...paths: str) -> None",
        use_when: "Use to remove explicit files or directories.",
        examples: &[r#"rm("/app/tmp.txt")"#],
        avoid: &["Avoid broad cleanup patterns; pass explicit paths."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "where",
        signature: "where(rows: list[record], key: str, expected: Any) | where(rows, key, op, expected) | where(rows, predicate) -> list[record]",
        use_when: "Use for equality, comparison, or lambda predicate filtering without pipeline syntax.",
        examples: &[
            r#"west = where(rows, "region", "west")"#,
            r#"large = where(rows, "size", ">", 1024)"#,
            r#"open_west = where(rows, lambda r: r["status"] == "open" and r["region"] == "west")"#,
        ],
        avoid: &["Use explicit loops when filtering needs side effects or expensive setup."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "keys",
        signature: "keys(record: record) -> list[str]",
        use_when: "Use to inspect or iterate record field names.",
        examples: &[r#"names = keys(row)"#],
        avoid: &["Use row.keys() when method syntax is clearer."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "values",
        signature: "values(record: record) -> list[Any]",
        use_when: "Use to inspect or iterate record values.",
        examples: &[r#"vals = values(row)"#],
        avoid: &["Use items(record) when keys are needed too."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "items",
        signature: "items(record: record) -> list[list[Any]]",
        use_when: "Use to iterate key/value pairs from a record.",
        examples: &[r#"pairs = []
for key, value in items(counts):
    pairs.append(key + ":" + str(value))"#],
        avoid: &["Initialize dictionary counters before incrementing missing keys."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "get",
        signature: "get(record: record, key: str, default: Any = None) -> Any",
        use_when: "Use to read optional record fields with a fallback.",
        examples: &[r#"score = get(row, "score", 0)"#],
        avoid: &["Use row[key] when a missing key should be an error."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sort",
        signature: "sort(values, key: str | lambda? = None, reverse: bool = False) -> list",
        use_when: "Use for sorted copies and top-N record lists.",
        examples: &[
            r#"top = sort(rows, key="amount", reverse=True)[:5]"#,
            r#"top = sort(rows, key=lambda r: (-r["count"], r["name"]))[:5]"#,
            r#"names = sort(names)"#,
        ],
        avoid: &["Do not use list.sort() or method keyword arguments."],
        aliases: &["sorted"],
    },
    StoneHelpEntry {
        name: "map",
        signature: "map(lambda_or_builtin, values: iterable) -> list",
        use_when: "Use for compact per-item transforms when a lambda is clearer than an explicit loop.",
        examples: &[r#"names = map(lambda r: r["name"], rows)"#],
        avoid: &["Use explicit loops when the transform needs statements or mutation."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "filter",
        signature: "filter(lambda, values: iterable) -> list",
        use_when: "Use for compact per-item filtering when a lambda is clearer than an explicit loop.",
        examples: &[r#"errors = filter(lambda r: r["status"] == 404, rows)"#],
        avoid: &["Use where(rows, key, expected) for simple equality on one record field."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "all",
        signature: "all(values: iterable | generator) -> bool",
        use_when: "Use to test whether every value is truthy, with generator short-circuiting.",
        examples: &[r#"ok = all("score" in row for row in rows)"#],
        avoid: &["Use explicit loops when you need to collect diagnostics for failed items."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "any",
        signature: "any(values: iterable | generator) -> bool",
        use_when: "Use to test whether any value is truthy, with generator short-circuiting.",
        examples: &[r#"has_error = any("ERROR" in line for line in lines)"#],
        avoid: &["Use search(root, needle) for file content search."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sum",
        signature: "sum(values: iterable | generator) -> int | float",
        use_when: "Use for numeric totals over lists or generator expressions.",
        examples: &[r#"total = sum(int(row["qty"]) for row in rows)"#],
        avoid: &["Convert strings to numbers before summing."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "set",
        signature: "set(values: iterable? = None) -> list",
        use_when: "Use for Python-shaped ordered uniqueness. The result is a list with unique values.",
        examples: &[
            r#"seen = set()"#,
            r#"seen.add(user)"#,
            r#"unique_names = set(names)"#,
        ],
        avoid: &["Do not rely on hash-set ordering; Stone preserves first-seen order."],
        aliases: &["unique"],
    },
    StoneHelpEntry {
        name: "type",
        signature: "type(value: Any) -> str",
        use_when: "Use for lightweight validation when checking task outputs.",
        examples: &[r#"ok = type(row["name"]) == "str""#],
        avoid: &["Prefer direct conversions like int()/float() when you need numeric values."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "first",
        signature: "first(values: list, count: int? = None) -> Any | list",
        use_when: "Use to inspect or keep the first item(s) of a list.",
        examples: &[r#"sample = first(rows, 5)"#, r#"sample = head(rows, 5)"#],
        avoid: &["Use slicing when it is clearer, such as rows[:5]."],
        aliases: &["head"],
    },
    StoneHelpEntry {
        name: "last",
        signature: "last(values: list, count: int? = None) -> Any | list",
        use_when: "Use to inspect or keep the last item(s) of a list.",
        examples: &[r#"tail_sample = last(rows, 5)"#, r#"tail_sample = tail(rows, 5)"#],
        avoid: &["Use slicing when it is clearer."],
        aliases: &["tail"],
    },
    StoneHelpEntry {
        name: "range",
        signature: "range(stop: int) | range(start: int, stop: int, step: int = 1) -> list[int]",
        use_when: "Use for numeric loops and index generation.",
        examples: &[r#"seen = []
for i in range(3):
    seen.append(i)"#, r#"indexes = range(1, 10, 2)"#],
        avoid: &["Use enumerate(values) when you need indexes and values together."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "enumerate",
        signature: "enumerate(values: iterable, start: int = 0) -> list[list[Any]]",
        use_when: "Use to iterate indexes and values together.",
        examples: &[r#"labels = []
for i, row in enumerate(rows):
    labels.append(str(i) + ":" + row["name"])"#],
        avoid: &["Use range(len(values)) only when you specifically need index-only access."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "split",
        signature: "split(text: str, separator: str? = None) -> list[str]",
        use_when: "Use for top-level text splitting; string method syntax also works.",
        examples: &[r#"parts = split(line, ",")"#, r#"words = split(line)"#],
        avoid: &["For line splitting, prefer text.splitlines() when operating on a string."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "join",
        signature: "join(items: list[str], separator: str = \"\") -> str",
        use_when: "Use for top-level list-to-text joining; string method syntax also works.",
        examples: &[r#"line = join(fields, ",")"#],
        avoid: &["Convert non-string items with map(str, items) or explicit str() first."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "slice",
        signature: "slice(value: str | list, start: int? = None, end: int? = None) -> str | list",
        use_when: "Use for dynamic slicing when bracket syntax is awkward.",
        examples: &[r#"top = slice(rows, 0, 5)"#],
        avoid: &["Use rows[:5] when bounds are simple literals."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "starts_with",
        signature: "starts_with(text: str, prefix: str) -> bool",
        use_when: "Use for prefix tests; startswith is an alias.",
        examples: &[r#"level = "other"
if starts_with(line, "ERROR"):
    level = "error""#],
        avoid: &["Use string method line.startswith(prefix) when method syntax is clearer."],
        aliases: &["startswith"],
    },
    StoneHelpEntry {
        name: "format",
        signature: "format(template: str, ...values: Any) -> str",
        use_when: "Use for small positional text templates.",
        examples: &[r#"line = format("{}:{}", name, count)"#],
        avoid: &["Use f-strings when they are clearer and do not need format specs."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "print",
        signature: "print(value: Any) -> Any",
        use_when: "Use only for diagnostic stdout during local probes.",
        examples: &[r#"print("debug: " + str(count))"#],
        avoid: &["Use emit(value) for structured results and write_file/write_json for task outputs."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run",
        signature: r#"run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576) -> record"#,
        use_when: "Use only when the task explicitly needs a POSIX program that should finish. Nonzero exits return ok=false with stdout, stderr, and an explanation record.",
        examples: &[
            r#"result = run(["wc", "-l", "/app/input.txt"])"#,
            r#"result = run(["printf", "ok"], timeout_ms=5000)"#,
            r#"result = run(["sh", "-c", "printf warning >&2"], stdout="suppress", stderr="capture", max_stderr_bytes=12000)"#,
            r#"if not result.ok:
    emit({"exit_code": result.exit_code, "stderr": result.stderr, "explanation": result.explanation})"#,
        ],
        avoid: &[
            "Do not pass shell strings; pass argv lists.",
            "Do not use run for normal file/JSON/CSV work.",
            "Do not use shell backgrounding, nohup, or `&` for long-lived services; use start_daemon().",
            "For noisy commands, suppress or cap output explicitly instead of flooding stdout/stderr.",
            "Do not ignore result.ok; inspect stderr, exit_code, timed_out, and explanation before retrying.",
            "If result.timed_out is true, inspect partial output first; rerun with a larger timeout_ms only when the command is expected to be slow.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "resolve_command",
        signature: "resolve_command(name: str) -> record",
        use_when: "Use to explain how Stone would resolve an external executable name without starting a process.",
        examples: &[
            r#"info = resolve_command("python3")"#,
            r#"info = resolve_command("definitely-not-a-real-command")
if not info.ok:
    emit(info.explanation)"#,
        ],
        avoid: &[
            "Use run() when you need to execute the command.",
            "Do not use shell-specific command lookup probes when this Stone helper is available.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "state",
        signature: "state() -> record",
        use_when: "Use to retrieve cheap agent-facing runtime state such as cwd, git status, and common tool availability.",
        examples: &[r#"snapshot = state()"#, r#"emit(state().cwd)"#],
        avoid: &["Do not shell out to git status or which/version probes when this structured snapshot is enough."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "last_result",
        signature: "last_result() -> record | None",
        use_when: "Use to recover the previous Waymark command response after the caller's conversation context dropped it.",
        examples: &[r#"previous = last_result()"#, r#"emit(last_result())"#],
        avoid: &["Do not use as long-term storage; it only tracks the immediately previous command response."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "start_daemon",
        signature: "start_daemon(argv: list[str], cwd: str? = None, env: record? = None, stdout: str? = None, stderr: str? = None) -> record",
        use_when: "Use for servers and background services that must still be running when tests execute.",
        examples: &[
            r#"daemon = start_daemon(["sh", "-c", "sleep 0.1"], cwd="/app", stderr="server.err")"#,
            r#"ready = wait_port(9, timeout_ms=1)"#,
            r#"status = daemon_status(daemon, log="server.err")"#,
        ],
        avoid: &[
            "Use run() instead for commands expected to finish.",
            "After starting a daemon, call wait_port() or daemon_status() before assuming it is ready.",
            "Keep stdout/stderr paths when startup logs may explain failures.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "daemon_status",
        signature: "daemon_status(daemon: record | int, port: int? = None, host: str = \"127.0.0.1\", log: str? = None, max_log_bytes: int = 4000) -> record",
        use_when: "Use to check whether a daemon is still alive, whether an expected TCP port is open, and to include recent logs.",
        examples: &[r#"status = daemon_status(daemon)"#],
        avoid: &["Do not treat a spawn result as ready until daemon_status() or wait_port() confirms it."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "stop_daemon",
        signature: "stop_daemon(daemon: record | int, timeout_ms: int = 5000) -> record",
        use_when: "Use to cleanly stop a daemon started by start_daemon().",
        examples: &[r#"stop = stop_daemon(daemon, timeout_ms=2000)"#],
        avoid: &["Do not use for normal foreground commands; run() already waits and cleans up timed-out children."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "wait_port",
        signature: "wait_port(port: int, host: str = \"127.0.0.1\", timeout_ms: int = 30000) -> record",
        use_when: "Use after start_daemon() when service readiness is represented by a TCP port accepting connections.",
        examples: &[r#"ready = wait_port(9, host="127.0.0.1", timeout_ms=1)"#],
        avoid: &["If wait_port() times out, call daemon_status() with a log path before retrying blindly."],
        aliases: &[],
    },
];

const STONE_HELP_TOPICS: &[StoneHelpTopic] = &[
    StoneHelpTopic {
        name: "workflow",
        summary: "Recommended LLM workflow for solving tasks in Stone.",
        bullets: &[
            "Use help() first, then help(\"name\") for a primitive before guessing syntax.",
            "In long-lived task-server and MCP sessions, top-level value and function bindings persist across eval calls; bind intermediate data once and reuse names later.",
            "Stone eval source can be a multi-line script like python -c or bash -c; use assignments, loops, helpers, and emit(value) when a structured return is useful.",
            "For large values, bind them by name and emit compact summaries such as {\"count\": len(rows), \"sample\": head(rows, 5)}; force full output only when necessary.",
            "Use /app paths for task inputs and write exactly the requested output files.",
            "Use stone/data primitives for file, CSV, JSON, JSONL, text, and sorting work.",
            "Use small probes with limit=5 or bounded reads before writing a large final script.",
            "Finish by reading/describing the output file to verify it exists and has the right shape.",
        ],
    },
    StoneHelpTopic {
        name: "session",
        summary: "Long-lived eval session behavior for agents.",
        bullets: &[
            "One-shot CLI evals are fresh, but task-server stream and MCP warm evals behave like a real shell session.",
            "Top-level value and function bindings persist across eval calls; this is live name binding, not a JSON result cache.",
            "Assignment-only evals return null and compact session diagnostics such as bound names instead of echoing large bound values.",
            "Prefer rows = read_csv(...), inspect rows, then reuse rows in later eval calls instead of rereading the file.",
            "Avoid emitting entire large lists; use head()/tail()/first()/last() samples unless the caller explicitly requests full output.",
            "Open file handles do not persist across eval calls; persist paths, text, records, lists, and functions instead.",
        ],
    },
    StoneHelpTopic {
        name: "syntax",
        summary: "Python-like syntax subset that Stone accepts.",
        bullets: &[
            "Assignments: name = value; counters[key] += 1 works after initialization.",
            "Blocks: if/elif/else, for, while, break, continue, pass use indentation.",
            "Values: lists, tuples, records/dicts, slices, indexing, item assignment, True, False, None.",
            "Record fields can be read as row[\"name\"] or row.name when the field name is identifier-shaped.",
            "Operators: +, -, *, /, //, &, |, <<, >>, comparisons, and/or/not, membership, is None.",
            "Conditional expressions use Python's value if condition else fallback shape.",
            "Functions: def name(arg) works; optional type annotations like def name(arg: str) -> str are checked; immutable default values are supported.",
            "try/except catches runtime evaluation errors; supported handlers are except:, except Exception:, and except Exception as e:.",
            "Lambdas: expression-only callbacks work in sort/map/filter, e.g. lambda r: r[\"name\"].",
            "String methods include strip/lstrip/rstrip, isdigit, split/splitlines, replace, join, lower/upper, zfill, startswith, and endswith.",
            "set() returns an ordered unique list; set/list variables support .add(value) for unique append.",
            "Use emit(value) when you want structured data returned to the caller.",
        ],
    },
    StoneHelpTopic {
        name: "unsupported",
        summary: "Common Python habits that fail in Stone, with replacements.",
        bullets: &[
            "No imports/modules/os/pathlib/glob/json; use find/read_json/json_loads/json_dumps.",
            "Lambda is expression-only; use explicit loops when callback logic needs statements or mutation.",
            "No classes/decorators/async/nested functions.",
            "No mutable default args, *args, **kwargs, or keyword calls to user functions.",
            "No try/finally, try/else, except*, or exception classes other than Exception.",
            "No list.sort(); use sort(list) or sorted(list).",
            "No automatic string-number coercion; use int(), float(), and str().",
            "No missing-key arithmetic; initialize dictionary counters before incrementing.",
        ],
    },
    StoneHelpTopic {
        name: "counters",
        summary: "Safe dictionary counter pattern.",
        bullets: &[
            "if key in counts:",
            "    counts[key] += 1",
            "else:",
            "    counts[key] = 1",
        ],
    },
];

#[cfg(test)]
pub(crate) fn stone_help_documented_names_for_tests() -> std::collections::BTreeSet<&'static str> {
    let mut names = std::collections::BTreeSet::new();
    for entry in STONE_HELP_ENTRIES {
        names.insert(entry.name);
        for alias in entry.aliases {
            names.insert(*alias);
        }
    }
    names
}

#[cfg(test)]
pub(crate) fn stone_help_entries_without_examples_for_tests() -> Vec<&'static str> {
    STONE_HELP_ENTRIES
        .iter()
        .filter(|entry| entry.examples.is_empty())
        .map(|entry| entry.name)
        .collect()
}

pub(crate) fn stone_help_overview(span: Span) -> Value {
    let mut record = Record::with_capacity(7);
    record.push("language", Value::string("Stone", span));
    record.push(
        "for_llm",
        Value::string("This help is written for LLM agents generating Stone. Stone eval source can be a multi-line script like python -c or bash -c. In MCP/task-server stream mode, top-level value and function bindings persist across eval calls; reuse named intermediates instead of rereading files. For large values, return len/head/tail summaries unless full output is explicitly required. Prefer these primitives and examples over guessing Python APIs.", span),
    );
    record.push("workflow", topic_bullets("workflow", span));
    record.push(
        "topics",
        Value::list(
            STONE_HELP_TOPICS
                .iter()
                .map(|topic| Value::string(topic.name, span))
                .collect(),
            span,
        ),
    );
    record.push(
        "builtins",
        Value::list(
            STONE_HELP_ENTRIES
                .iter()
                .map(|entry| {
                    let mut item = Record::with_capacity(3);
                    item.push("name", Value::string(entry.name, span));
                    item.push("signature", Value::string(entry.signature, span));
                    item.push("use_when", Value::string(entry.use_when, span));
                    Value::record(item, span)
                })
                .collect(),
            span,
        ),
    );
    record.push("syntax", topic_bullets("syntax", span));
    record.push("unsupported", topic_bullets("unsupported", span));
    record.push(
        "examples",
        string_list(
            &[
                r#"rows = read_csv("/app/input.csv")"#,
                r#"files = find("/app", "*.jsonl")"#,
                r#"write_file("/app/out.txt", "done\n")"#,
                r#"write_json("/app/out.json", {"ok": True})"#,
            ],
            span,
        ),
    );
    Value::record(record, span)
}

pub(crate) fn stone_help_topic(name: &str, span: Span) -> Value {
    let normalized = match name {
        "read_text" => "read_file",
        "write_text" => "write_file",
        "edit_file" => "edit",
        "list_dir" => "ls",
        "head" => "first",
        "tail" => "last",
        "from_json" => "json_loads",
        "to_json" => "json_dumps",
        "sorted" => "sort",
        "gotchas" | "constraints" => "unsupported",
        other => other,
    };
    if let Some(entry) = STONE_HELP_ENTRIES
        .iter()
        .find(|entry| entry.name == normalized)
    {
        let mut record = Record::with_capacity(7);
        record.push("name", Value::string(entry.name, span));
        record.push("signature", Value::string(entry.signature, span));
        record.push("use_when", Value::string(entry.use_when, span));
        record.push("examples", string_list(entry.examples, span));
        record.push("avoid", string_list(entry.avoid, span));
        record.push("aliases", string_list(entry.aliases, span));
        record.push("found", Value::bool(true, span));
        Value::record(record, span)
    } else if let Some(topic) = STONE_HELP_TOPICS
        .iter()
        .find(|topic| topic.name == normalized)
    {
        let mut record = Record::with_capacity(4);
        record.push("name", Value::string(topic.name, span));
        record.push("summary", Value::string(topic.summary, span));
        record.push("bullets", string_list(topic.bullets, span));
        record.push("found", Value::bool(true, span));
        Value::record(record, span)
    } else {
        let mut record = Record::with_capacity(4);
        record.push("name", Value::string(name, span));
        record.push("found", Value::bool(false, span));
        record.push(
            "message",
            Value::string(
                "No detailed Stone help for this topic. Use help() for the available surface.",
                span,
            ),
        );
        record.push(
            "available",
            Value::list(
                STONE_HELP_ENTRIES
                    .iter()
                    .map(|entry| Value::string(entry.name, span))
                    .collect(),
                span,
            ),
        );
        Value::record(record, span)
    }
}

fn topic_bullets(name: &str, span: Span) -> Value {
    STONE_HELP_TOPICS
        .iter()
        .find(|topic| topic.name == name)
        .map(|topic| string_list(topic.bullets, span))
        .unwrap_or_else(|| Value::list(Vec::new(), span))
}

fn string_list(items: &[&str], span: Span) -> Value {
    Value::list(
        items
            .iter()
            .map(|item| Value::string(*item, span))
            .collect(),
        span,
    )
}

impl Command for Last {
    fn name(&self) -> &str {
        "last"
    }

    fn description(&self) -> &str {
        "Return the last item or items from a list."
    }

    fn signature(&self) -> Signature {
        Signature::build("last")
            .input_output_types(vec![(Type::List(Box::new(Type::Any)), Type::Any)])
            .optional("rows", SyntaxShape::Int, "Number of items to return.")
            .category(Category::Filters)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        mut input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let rows = parse_row_count(call.opt::<Spanned<i64>>(engine_state, stack, 0)?)?;
        let head = call.head;
        let metadata = input.take_metadata();

        match input {
            PipelineData::ListStream(stream, ..) => {
                let span = stream.span();
                let mut tail = VecDeque::with_capacity(rows.count.max(1));
                for value in stream {
                    if rows.count == 1 {
                        tail.clear();
                    } else if tail.len() == rows.count {
                        tail.pop_front();
                    }
                    tail.push_back(value);
                }

                if rows.count == 1 && !rows.explicit {
                    Ok(tail
                        .pop_back()
                        .unwrap_or_else(|| Value::nothing(head))
                        .into_pipeline_data_with_metadata(metadata))
                } else {
                    Ok(Value::list(tail.into_iter().collect::<Vec<_>>(), span)
                        .into_pipeline_data_with_metadata(metadata))
                }
            }
            other => {
                let value = other.into_value(head)?;
                let span = value.span();

                match value {
                    Value::List { vals, .. } => {
                        if rows.count == 1 && !rows.explicit {
                            Ok(vals
                                .into_iter()
                                .last()
                                .unwrap_or_else(|| Value::nothing(head))
                                .into_pipeline_data_with_metadata(metadata))
                        } else {
                            let len = vals.len();
                            let start = len.saturating_sub(rows.count);
                            Ok(
                                Value::list(vals.into_iter().skip(start).collect::<Vec<_>>(), span)
                                    .into_pipeline_data_with_metadata(metadata),
                            )
                        }
                    }
                    other => Err(type_mismatch("list", other.get_type().to_string(), head)),
                }
            }
        }
    }
}

impl Command for Ls {
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List a directory or file path."
    }

    fn signature(&self) -> Signature {
        Signature::build("ls")
            .input_output_types(vec![(Type::Nothing, Type::table())])
            .optional(
                "path",
                SyntaxShape::OneOf(vec![SyntaxShape::Directory, SyntaxShape::Filepath]),
                "Path to inspect.",
            )
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let path_arg = call.opt::<Spanned<String>>(engine_state, stack, 0)?;
        let target = match path_arg.as_ref() {
            Some(path) => resolve_path(engine_state, stack, &path.item)?,
            None => engine_state.cwd(Some(stack))?.into_std_path_buf(),
        };
        let span = path_arg.as_ref().map_or(call.head, |path| path.span);

        let mut entries = if target.is_dir() {
            match fs::read_dir(&target) {
                Ok(read_dir) => read_dir
                    .map(|entry| {
                        let entry = entry.map_err(|err| io_error(err, span, &target))?;
                        let path = entry.path();
                        let metadata = fs::symlink_metadata(&path)
                            .map_err(|err| io_error(err, span, &path))?;
                        Ok(list_entry_value(
                            entry.file_name().to_string_lossy().into_owned(),
                            path,
                            metadata,
                            call.head,
                        ))
                    })
                    .collect::<Result<Vec<_>, ShellError>>()?,
                Err(_err) if target == Path::new("/work") => Vec::new(),
                Err(err) => Err(io_error(err, span, &target))?,
            }
        } else if target.exists() {
            let metadata =
                fs::symlink_metadata(&target).map_err(|err| io_error(err, span, &target))?;
            vec![list_entry_value(
                target
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| target.display().to_string()),
                target.clone(),
                metadata,
                call.head,
            )]
        } else {
            Err(io_error(
                std::io::Error::from(std::io::ErrorKind::NotFound),
                span,
                &target,
            ))?
        };

        entries.sort_by(|left, right| {
            left.get_data_by_key("name")
                .and_then(|value| value.coerce_string().ok())
                .cmp(
                    &right
                        .get_data_by_key("name")
                        .and_then(|value| value.coerce_string().ok()),
                )
        });

        Ok(Value::list(entries, call.head).into_pipeline_data())
    }
}

impl Command for Mkdir {
    fn name(&self) -> &str {
        "mkdir"
    }

    fn description(&self) -> &str {
        "Create one or more directories."
    }

    fn signature(&self) -> Signature {
        Signature::build("mkdir")
            .input_output_types(vec![(Type::Nothing, Type::Nothing)])
            .rest("paths", SyntaxShape::Directory, "Directories to create.")
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let paths = call.rest::<Spanned<String>>(engine_state, stack, 0)?;
        if paths.is_empty() {
            return Err(ShellError::MissingParameter {
                param_name: "requires directory paths".into(),
                span: call.head,
            });
        }

        for path in paths {
            let target = resolve_path(engine_state, stack, &path.item)?;
            fs::create_dir_all(&target).map_err(|err| io_error(err, path.span, &target))?;
        }

        Ok(PipelineData::empty())
    }
}

impl Command for Open {
    fn name(&self) -> &str {
        "open"
    }

    fn description(&self) -> &str {
        "Read a file into the pipeline."
    }

    fn signature(&self) -> Signature {
        Signature::build("open")
            .input_output_types(vec![(Type::Nothing, Type::Any)])
            .required("path", SyntaxShape::Filepath, "File to open.")
            .switch("raw", "Read the file as bytes.", Some('r'))
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let raw = call.has_flag(engine_state, stack, "raw")?;
        let path = call.req::<Spanned<String>>(engine_state, stack, 0)?;
        let target = resolve_path(engine_state, stack, &path.item)?;
        let bytes = fs::read(&target).map_err(|err| io_error(err, path.span, &target))?;

        if raw {
            Ok(Value::binary(bytes, call.head).into_pipeline_data())
        } else if is_json_path(&target) {
            Ok(parse_json_bytes(&bytes, call.head)?.into_pipeline_data())
        } else {
            match String::from_utf8(bytes) {
                Ok(text) => Ok(Value::string(text, call.head).into_pipeline_data()),
                Err(err) => Ok(Value::binary(err.into_bytes(), call.head).into_pipeline_data()),
            }
        }
    }
}

impl Command for Pwd {
    fn name(&self) -> &str {
        "pwd"
    }

    fn description(&self) -> &str {
        "Print the current working directory."
    }

    fn signature(&self) -> Signature {
        Signature::build("pwd")
            .input_output_types(vec![(Type::Nothing, Type::String)])
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let cwd = engine_state.cwd_as_string(Some(stack))?;
        Ok(Value::string(cwd, call.head).into_pipeline_data())
    }
}

impl Command for Rm {
    fn name(&self) -> &str {
        "rm"
    }

    fn description(&self) -> &str {
        "Remove files or directories."
    }

    fn signature(&self) -> Signature {
        Signature::build("rm")
            .input_output_types(vec![(Type::Nothing, Type::Nothing)])
            .rest(
                "paths",
                SyntaxShape::OneOf(vec![SyntaxShape::Directory, SyntaxShape::Filepath]),
                "Paths to remove.",
            )
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let paths = call.rest::<Spanned<String>>(engine_state, stack, 0)?;
        if paths.is_empty() {
            return Err(ShellError::MissingParameter {
                param_name: "requires path arguments".into(),
                span: call.head,
            });
        }

        for path in paths {
            let target = resolve_path(engine_state, stack, &path.item)?;
            if target.is_dir() {
                fs::remove_dir_all(&target).map_err(|err| io_error(err, path.span, &target))?;
            } else {
                fs::remove_file(&target).map_err(|err| io_error(err, path.span, &target))?;
            }
        }

        Ok(PipelineData::empty())
    }
}

impl Command for RunExternal {
    fn name(&self) -> &str {
        "run-external"
    }

    fn description(&self) -> &str {
        "Reject external command execution in the guest."
    }

    fn signature(&self) -> Signature {
        Signature::build("run-external")
            .input_output_types(vec![(Type::Any, Type::Any)])
            .rest(
                "command",
                SyntaxShape::Any,
                "External command and arguments.",
            )
            .category(Category::System)
    }

    fn run(
        &self,
        _engine_state: &EngineState,
        _stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        Err(ShellError::ExternalNotSupported { span: call.head })
    }
}

impl Command for Save {
    fn name(&self) -> &str {
        "save"
    }

    fn description(&self) -> &str {
        "Write pipeline input to a file."
    }

    fn signature(&self) -> Signature {
        Signature::build("save")
            .input_output_types(vec![(Type::Any, Type::Nothing)])
            .required("path", SyntaxShape::Filepath, "Destination path.")
            .switch("append", "Append to an existing file.", Some('a'))
            .switch("force", "Overwrite an existing file.", Some('f'))
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let append = call.has_flag(engine_state, stack, "append")?;
        let force = call.has_flag(engine_state, stack, "force")?;
        let path = call.req::<Spanned<String>>(engine_state, stack, 0)?;
        let target = resolve_path(engine_state, stack, &path.item)?;

        if target.exists() && !append && !force {
            return Err(io_error(
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
                path.span,
                &target,
            ));
        }

        let bytes = pipeline_to_bytes(input, engine_state, call.head)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&target)
            .map_err(|err| io_error(err, path.span, &target))?;
        file.write_all(&bytes)
            .map_err(|err| io_error(err, path.span, &target))?;
        file.flush()
            .map_err(|err| io_error(err, path.span, &target))?;

        Ok(PipelineData::empty())
    }
}

impl Command for Search {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Search UTF-8 files under a root path for a literal substring."
    }

    fn signature(&self) -> Signature {
        Signature::build("search")
            .input_output_types(vec![(Type::Nothing, Type::table())])
            .required("root", SyntaxShape::Directory, "Root path to walk.")
            .required("needle", SyntaxShape::String, "Literal text to search for.")
            .category(Category::FileSystem)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        _input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let root = call.req::<Spanned<String>>(engine_state, stack, 0)?;
        let needle = call.req::<Spanned<String>>(engine_state, stack, 1)?;
        if needle.item.is_empty() {
            return Err(ShellError::Generic(
                GenericError::new("Invalid search", "needle must not be empty", needle.span)
                    .with_code("search_empty_needle"),
            ));
        }

        let root_path = resolve_path(engine_state, stack, &root.item)?;
        let mut files_visited = 0usize;
        let mut matches = Vec::new();
        let mut queue = VecDeque::from([root_path.clone()]);

        while let Some(path) = queue.pop_front() {
            if files_visited >= MAX_SEARCH_FILES || matches.len() >= MAX_SEARCH_MATCHES {
                break;
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|err| io_error(err, root.span, &path))?;
            if metadata.is_dir() {
                let mut children = fs::read_dir(&path)
                    .map_err(|err| io_error(err, root.span, &path))?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|err| io_error(err, root.span, &path))?;
                children.sort();
                queue.extend(children);
                continue;
            }
            if !metadata.is_file() || metadata.len() > MAX_SEARCH_FILE_BYTES {
                continue;
            }

            files_visited += 1;
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            for (line_index, line) in content.lines().enumerate() {
                if line.contains(&needle.item) {
                    matches.push(search_match_value(
                        path.clone(),
                        line_index + 1,
                        line.to_string(),
                        call.head,
                    ));
                    if matches.len() >= MAX_SEARCH_MATCHES {
                        break;
                    }
                }
            }
        }

        matches.sort_by(|left, right| {
            left.get_data_by_key("path")
                .and_then(|value| value.coerce_string().ok())
                .cmp(
                    &right
                        .get_data_by_key("path")
                        .and_then(|value| value.coerce_string().ok()),
                )
                .then_with(|| {
                    left.get_data_by_key("line")
                        .and_then(|value| value.as_int().ok())
                        .cmp(
                            &right
                                .get_data_by_key("line")
                                .and_then(|value| value.as_int().ok()),
                        )
                })
        });
        Ok(Value::list(matches, call.head).into_pipeline_data())
    }
}

impl Command for ToJson {
    fn name(&self) -> &str {
        "to_json"
    }

    fn description(&self) -> &str {
        "Serialize structured values as JSON text."
    }

    fn signature(&self) -> Signature {
        Signature::build("to_json")
            .input_output_types(vec![(Type::Any, Type::String)])
            .category(Category::Formats)
    }

    fn run(
        &self,
        _engine_state: &EngineState,
        _stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let text = pipeline_to_json_text(input, call.head)?;
        Ok(Value::string(text, call.head).into_pipeline_data())
    }
}

impl Command for ToJsonl {
    fn name(&self) -> &str {
        "to_jsonl"
    }

    fn description(&self) -> &str {
        "Serialize a value or list of values as JSON Lines text."
    }

    fn signature(&self) -> Signature {
        Signature::build("to_jsonl")
            .input_output_types(vec![(Type::Any, Type::String)])
            .category(Category::Formats)
    }

    fn run(
        &self,
        _engine_state: &EngineState,
        _stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let value = input.into_value(call.head)?;
        let mut lines = Vec::new();
        match value {
            Value::List { vals, .. } => {
                for value in vals {
                    lines.push(serde_json::to_string(&nu_to_json_value(&value)).map_err(
                        |err| {
                            ShellError::Generic(GenericError::new_internal(
                                "Failed to encode JSONL",
                                err.to_string(),
                            ))
                        },
                    )?);
                }
            }
            value => lines.push(serde_json::to_string(&nu_to_json_value(&value)).map_err(
                |err| {
                    ShellError::Generic(GenericError::new_internal(
                        "Failed to encode JSONL",
                        err.to_string(),
                    ))
                },
            )?),
        }

        let mut text = lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        Ok(Value::string(text, call.head).into_pipeline_data())
    }
}

impl Command for Log {
    fn name(&self) -> &str {
        "log"
    }

    fn description(&self) -> &str {
        "Write a structured task log event to stderr."
    }

    fn signature(&self) -> Signature {
        Signature::build("log")
            .input_output_types(vec![(Type::Any, Type::Any)])
            .required("level", SyntaxShape::String, "Log level.")
            .required("message", SyntaxShape::String, "Log message.")
            .optional("fields", SyntaxShape::Any, "Structured log fields.")
            .category(Category::Core)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let level = call.req::<Spanned<String>>(engine_state, stack, 0)?;
        let message = call.req::<Spanned<String>>(engine_state, stack, 1)?;
        let fields = call.opt::<Value>(engine_state, stack, 2)?;
        let event = serde_json::json!({
            "stone_log": true,
            "level": level.item,
            "message": message.item,
            "fields": fields.as_ref().map(nu_to_json_value).unwrap_or(serde_json::Value::Null),
        });
        eprintln!("{event}");
        Ok(input)
    }
}

impl Command for Sort {
    fn name(&self) -> &str {
        "sort"
    }

    fn description(&self) -> &str {
        "Sort a list, optionally by a cell path."
    }

    fn signature(&self) -> Signature {
        Signature::build("sort")
            .input_output_types(vec![(
                Type::List(Box::new(Type::Any)),
                Type::List(Box::new(Type::Any)),
            )])
            .optional(
                "cell_path",
                SyntaxShape::CellPath,
                "Cell path to sort records by.",
            )
            .switch("reverse", "Reverse the sort order.", Some('r'))
            .category(Category::Filters)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let path = call.opt::<CellPath>(engine_state, stack, 0)?;
        let reverse = call.has_flag(engine_state, stack, "reverse")?;
        let value = input.into_value(call.head)?;
        let span = value.span();

        match value {
            Value::List { mut vals, .. } => {
                sort_values(&mut vals, path.as_ref().map(|path| path.members.as_slice()))?;
                if reverse {
                    vals.reverse();
                }
                Ok(Value::list(vals, span).into_pipeline_data())
            }
            other => Err(type_mismatch(
                "list",
                other.get_type().to_string(),
                call.head,
            )),
        }
    }
}

impl Command for Where {
    fn name(&self) -> &str {
        "where"
    }

    fn description(&self) -> &str {
        "Filter a list by equality on a cell path."
    }

    fn signature(&self) -> Signature {
        Signature::build("where")
            .input_output_types(vec![(
                Type::List(Box::new(Type::Any)),
                Type::List(Box::new(Type::Any)),
            )])
            .required("cell_path", SyntaxShape::CellPath, "Cell path to compare.")
            .required("value", SyntaxShape::Any, "Expected value.")
            .category(Category::Filters)
    }

    fn run(
        &self,
        engine_state: &EngineState,
        stack: &mut Stack,
        call: &Call,
        input: PipelineData,
    ) -> Result<PipelineData, ShellError> {
        let path = call.req::<CellPath>(engine_state, stack, 0)?;
        let expected = call.req::<Value>(engine_state, stack, 1)?;
        let head = call.head;
        let members = path.members;
        let metadata = input.metadata_ref().cloned();

        Ok(input
            .into_iter()
            .filter_map(move |value| {
                match value
                    .follow_cell_path(&members)
                    .map(|candidate| candidate.into_owned())
                    .and_then(|candidate| candidate.eq(head, &expected, head))
                {
                    Ok(Value::Bool { val: true, .. }) => Some(value),
                    Ok(Value::Bool { val: false, .. }) => None,
                    Ok(_) => Some(Value::error(
                        type_mismatch("bool", "non-bool comparison result", head),
                        head,
                    )),
                    Err(err) => Some(Value::error(err, head)),
                }
            })
            .into_pipeline_data_with_metadata(head, engine_state.signals().clone(), metadata))
    }
}

fn resolve_path(
    engine_state: &EngineState,
    stack: &Stack,
    raw: &str,
) -> Result<PathBuf, ShellError> {
    let expanded = expand_tilde(engine_state, stack, raw)?;
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(engine_state
            .cwd(Some(stack))?
            .into_std_path_buf()
            .join(expanded))
    }
}

fn expand_tilde(
    engine_state: &EngineState,
    stack: &Stack,
    raw: &str,
) -> Result<PathBuf, ShellError> {
    if raw == "~" {
        home_dir(engine_state, stack, Span::unknown())
    } else if let Some(rest) = raw.strip_prefix("~/") {
        Ok(home_dir(engine_state, stack, Span::unknown())?.join(rest))
    } else {
        Ok(PathBuf::from(raw))
    }
}

fn home_dir(engine_state: &EngineState, stack: &Stack, span: Span) -> Result<PathBuf, ShellError> {
    if let Some(home) = stack.get_env_var(engine_state, "HOME") {
        home.to_path()
    } else {
        Err(ShellError::MissingParameter {
            param_name: "HOME".into(),
            span,
        })
    }
}

fn io_error(
    err: impl Into<nu_protocol::shell_error::io::ErrorKind>,
    span: Span,
    path: impl AsRef<Path>,
) -> ShellError {
    let path = path.as_ref().to_path_buf();
    if span == Span::unknown() {
        ShellError::Io(IoError::new_internal_with_path(
            err,
            format!("I/O error at {}", path.display()),
            path,
        ))
    } else {
        ShellError::Io(IoError::new(err, span, Some(path)))
    }
}

fn pipeline_to_bytes(
    input: PipelineData,
    _engine_state: &EngineState,
    span: Span,
) -> Result<Vec<u8>, ShellError> {
    let value = input.into_value(span)?;
    match value {
        Value::Binary { .. } | Value::String { .. } => value.coerce_into_binary(),
        value => Ok(
            serde_json::to_vec(&nu_to_json_value(&value)).map_err(|err| {
                ShellError::Generic(
                    nu_protocol::shell_error::generic::GenericError::new_internal(
                        "Failed to encode JSON",
                        err.to_string(),
                    ),
                )
            })?,
        ),
    }
}

fn parse_json_input(input: PipelineData, span: Span) -> Result<Value, ShellError> {
    let value = input.into_value(span)?;
    let bytes = match value {
        Value::Binary { .. } | Value::String { .. } => value.coerce_into_binary()?,
        value => {
            return Err(ShellError::TypeMismatch {
                err_message: format!(
                    "expected string or binary JSON input, got {}",
                    value.get_type()
                ),
                span,
            });
        }
    };
    parse_json_bytes(&bytes, span)
}

struct RowCount {
    count: usize,
    explicit: bool,
}

fn parse_row_count(rows: Option<Spanned<i64>>) -> Result<RowCount, ShellError> {
    match rows {
        Some(rows) if rows.item < 0 => Err(ShellError::NeedsPositiveValue { span: rows.span }),
        Some(rows) => Ok(RowCount {
            count: rows.item as usize,
            explicit: true,
        }),
        None => Ok(RowCount {
            count: 1,
            explicit: false,
        }),
    }
}

fn sort_values(values: &mut [Value], path: Option<&[PathMember]>) -> Result<(), ShellError> {
    let mut keyed = Vec::with_capacity(values.len());
    for value in values.iter() {
        let key = match path {
            Some(path) => value.follow_cell_path(path)?.into_owned(),
            None => value.clone(),
        };
        keyed.push(key);
    }

    let mut indices = (0..values.len()).collect::<Vec<_>>();
    indices.sort_by(|left, right| compare_values(&keyed[*left], &keyed[*right]));

    let mut ordered = Vec::with_capacity(values.len());
    let mut slots = values
        .iter_mut()
        .map(|value| std::mem::take(value))
        .collect::<Vec<_>>();
    for index in indices {
        ordered.push(std::mem::take(&mut slots[index]));
    }
    values.swap_with_slice(&mut ordered);
    Ok(())
}

fn compare_values(left: &Value, right: &Value) -> Ordering {
    left.partial_cmp(right)
        .unwrap_or_else(|| {
            left.get_type()
                .to_string()
                .cmp(&right.get_type().to_string())
        })
        .then_with(|| {
            left.coerce_string()
                .unwrap_or_else(|_| format!("{left:?}"))
                .cmp(
                    &right
                        .coerce_string()
                        .unwrap_or_else(|_| format!("{right:?}")),
                )
        })
}

fn type_mismatch(expected: impl Into<String>, actual: impl Into<String>, span: Span) -> ShellError {
    ShellError::TypeMismatch {
        err_message: format!("expected {}, got {}", expected.into(), actual.into()),
        span,
    }
}

fn list_entry_value(name: String, path: PathBuf, metadata: std::fs::Metadata, span: Span) -> Value {
    let file_type = metadata.file_type();
    let kind = if file_type.is_dir() {
        "dir"
    } else if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_file() {
        "file"
    } else {
        "other"
    };

    let mut record = Record::with_capacity(5);
    record.push("name", Value::string(name, span));
    record.push("path", Value::string(path.display().to_string(), span));
    record.push("type", Value::string(kind, span));
    record.push(
        "readonly",
        Value::bool(metadata.permissions().readonly(), span),
    );
    record.push(
        "size",
        Value::int(i64::try_from(metadata.len()).unwrap_or(i64::MAX), span),
    );
    Value::record(record, span)
}

fn find_entry_value(name: String, path: PathBuf, metadata: std::fs::Metadata, span: Span) -> Value {
    let kind = if metadata.is_dir() {
        "dir"
    } else if metadata.is_file() {
        "file"
    } else if metadata.file_type().is_symlink() {
        "symlink"
    } else {
        "other"
    };
    let mut record = Record::with_capacity(4);
    record.push("path", Value::string(path.display().to_string(), span));
    record.push("name", Value::string(name, span));
    record.push("type", Value::string(kind, span));
    record.push(
        "size",
        Value::int(i64::try_from(metadata.len()).unwrap_or(i64::MAX), span),
    );
    Value::record(record, span)
}

fn find_name_matches(name: &str, contains: Option<&str>, glob: Option<&str>) -> bool {
    contains.is_none_or(|needle| name.contains(needle))
        && glob.is_none_or(|pattern| wildcard_match(pattern, name))
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
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

    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}

fn search_match_value(path: PathBuf, line: usize, text: String, span: Span) -> Value {
    let mut record = Record::with_capacity(3);
    record.push("path", Value::string(path.display().to_string(), span));
    record.push(
        "line",
        Value::int(i64::try_from(line).unwrap_or(i64::MAX), span),
    );
    record.push("text", Value::string(text, span));
    Value::record(record, span)
}

fn is_json_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use nu_protocol::{casing::Casing, Record, Span, Spanned, Value};

    use super::{
        compare_values, find_entry_value, find_name_matches, is_json_path, list_entry_value,
        parse_row_count, search_match_value, sort_values, wildcard_match,
    };

    #[test]
    fn parse_row_count_tracks_default_explicit_and_negative_cases() {
        let default = parse_row_count(None).expect("default");
        assert_eq!(default.count, 1);
        assert!(!default.explicit);

        let explicit = parse_row_count(Some(Spanned {
            item: 0,
            span: Span::unknown(),
        }))
        .expect("explicit");
        assert_eq!(explicit.count, 0);
        assert!(explicit.explicit);

        assert!(parse_row_count(Some(Spanned {
            item: -1,
            span: Span::unknown(),
        }))
        .is_err());
    }

    #[test]
    fn wildcard_matching_supports_stars_and_question_marks() {
        assert!(wildcard_match("*.rs", "lib.rs"));
        assert!(wildcard_match("a?c*", "abcdef"));
        assert!(wildcard_match("*", "anything"));
        assert!(!wildcard_match("a?d", "abc"));
        assert!(!wildcard_match("*.rs", "lib.py"));
    }

    #[test]
    fn find_name_matches_combines_contains_and_glob_filters() {
        assert!(find_name_matches(
            "stone_runtime.rs",
            Some("runtime"),
            Some("*.rs")
        ));
        assert!(!find_name_matches(
            "stone_runtime.rs",
            Some("missing"),
            Some("*.rs")
        ));
        assert!(!find_name_matches(
            "stone_runtime.rs",
            Some("runtime"),
            Some("*.txt")
        ));
        assert!(find_name_matches("stone_runtime.rs", None, None));
    }

    #[test]
    fn json_path_detection_is_case_insensitive() {
        assert!(is_json_path(Path::new("data.json")));
        assert!(is_json_path(Path::new("data.JSON")));
        assert!(!is_json_path(Path::new("data.jsonl")));
        assert!(!is_json_path(Path::new("data")));
    }

    #[test]
    fn compare_values_orders_by_value_then_type_fallback() {
        assert_eq!(
            compare_values(
                &Value::int(1, Span::unknown()),
                &Value::int(2, Span::unknown())
            ),
            Ordering::Less
        );
        assert_ne!(
            compare_values(
                &Value::string("1", Span::unknown()),
                &Value::int(1, Span::unknown())
            ),
            Ordering::Equal
        );
    }

    #[test]
    fn sort_values_preserves_records_while_ordering_by_cell_path() {
        let span = Span::unknown();
        let mut values = vec![
            record_value("b", 2, span),
            record_value("a", 1, span),
            record_value("c", 3, span),
        ];
        let path = nu_protocol::ast::CellPath {
            members: vec![nu_protocol::ast::PathMember::String {
                val: "name".to_string(),
                span,
                optional: false,
                casing: Casing::Sensitive,
            }],
        };

        sort_values(&mut values, Some(&path.members)).expect("sort by name");

        assert_eq!(
            values
                .iter()
                .map(|value| value
                    .as_record()
                    .expect("record")
                    .get("name")
                    .expect("name")
                    .as_str()
                    .expect("string"))
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn entry_value_helpers_describe_files_and_matches() {
        let root = temp_dir("commands-entry-values");
        fs::create_dir_all(&root).expect("root");
        let file = root.join("answer.txt");
        fs::write(&file, "hello").expect("file");
        let dir = root.join("nested");
        fs::create_dir(&dir).expect("dir");

        let file_meta = fs::symlink_metadata(&file).expect("file metadata");
        let listed = list_entry_value(
            "answer.txt".to_string(),
            file.clone(),
            file_meta,
            Span::unknown(),
        );
        let listed_record = listed.as_record().expect("record");
        assert_eq!(
            listed_record
                .get("type")
                .expect("type")
                .as_str()
                .expect("string"),
            "file"
        );

        let dir_meta = fs::symlink_metadata(&dir).expect("dir metadata");
        let found = find_entry_value("nested".to_string(), dir.clone(), dir_meta, Span::unknown());
        let found_record = found.as_record().expect("record");
        assert_eq!(
            found_record
                .get("type")
                .expect("type")
                .as_str()
                .expect("string"),
            "dir"
        );

        let matched = search_match_value(file.clone(), 12, "needle".to_string(), Span::unknown());
        let matched_record = matched.as_record().expect("record");
        assert_eq!(
            matched_record
                .get("line")
                .expect("line")
                .as_int()
                .expect("int"),
            12
        );
        assert!(matched_record
            .get("path")
            .expect("path")
            .as_str()
            .expect("string")
            .ends_with("answer.txt"));

        let _ = fs::remove_dir_all(root);
    }

    fn record_value(name: &str, size: i64, span: Span) -> Value {
        let mut record = Record::new();
        record.push("name", Value::string(name, span));
        record.push("size", Value::int(size, span));
        Value::record(record, span)
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        Path::new("/tmp").join(format!("waymark-{label}-{nanos}"))
    }
}
