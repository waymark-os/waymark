// SPDX-License-Identifier: MIT OR Apache-2.0

use nu_protocol::{Record, Span, Value};

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
        name: "repr",
        signature: "repr(value: Any) -> str",
        use_when: "Use as a Python-compatibility alias for str(value) when generated code wants a printable representation.",
        examples: &[r#"debug = repr(["ok", 2])"#],
        avoid: &["Use json_dumps(value) when the output must be valid JSON."],
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
        name: "tuple",
        signature: "tuple(value: list | record) -> list",
        use_when: "Use as a Python-compatibility alias for list(value); Stone represents tuples as list values.",
        examples: &[r#"names = tuple(counts)"#],
        avoid: &["Use list(value) when you do not need Python compatibility for generated code."],
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
        use_when: "Use for headered CSV. Values are strings. Quoted fields may contain commas, quotes, and newlines.",
        examples: &[
            r#"rows = read_csv("/app/input.csv")"#,
            r#"sample = read_csv("/app/input.csv", limit=5)"#,
        ],
        avoid: &["Convert with int()/float() before arithmetic; Stone does not coerce strings."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "write_csv",
        signature: "write_csv(path: str, rows: list[record], columns: list[str]? = None) -> record",
        use_when: "Use to write headered CSV output from record rows with standard CSV quoting.",
        examples: &[r#"write_csv("/app/out.csv", [{"name": "ada", "score": 10}])"#],
        avoid: &["Do not hand-roll CSV quoting with string concatenation unless the format is intentionally nonstandard."],
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
        signature: "json_dumps(value: Any, indent: int? = None, separators: list? = None) -> str",
        use_when: "Use to serialize a value to JSON text. Supports compact output, indent=2, and separators=(\",\", \":\") for Python-shaped agent code.",
        examples: &[
            r#"text = json_dumps({"ok": True})"#,
            r#"pretty = json_dumps({"ok": True}, indent=2)"#,
            r#"compact = json_dumps({"ok": True}, separators=(",", ":"))"#,
        ],
        avoid: &["Use write_json(path, value) for final JSON files."],
        aliases: &["to_json"],
    },
    StoneHelpEntry {
        name: "md5",
        signature: "md5(text: str) -> str",
        use_when: "Use to compute a lowercase hexadecimal MD5 digest of text.",
        examples: &[r#"digest = md5("abcdefghijklmnopqrstuvwxyz")"#],
        avoid: &["Do not import hashlib for MD5 hashing."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sha1",
        signature: "sha1(text: str) -> str",
        use_when: "Use to compute a lowercase hexadecimal SHA-1 digest of text.",
        examples: &[r#"digest = sha1("abcdefghijklmnopqrstuvwxyz")"#],
        avoid: &["Do not import hashlib for SHA-1 hashing."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "sha256",
        signature: "sha256(text: str) -> str",
        use_when: "Use to compute a lowercase hexadecimal SHA-256 digest of text.",
        examples: &[r#"digest = sha256("abcdefghijklmnopqrstuvwxyz")"#],
        avoid: &["Do not import hashlib for SHA-256 hashing."],
        aliases: &[],
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
            r#"rows.sort(key=lambda r: r["name"], reverse=True)"#,
            r#"names = sort(names)"#,
        ],
        avoid: &["Remember list.sort(...) mutates in place and returns None; use top-level sort(...) when you need a sorted copy."],
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
        signature: "set(values: iterable | generator? = None) -> list",
        use_when: "Use for Python-shaped ordered uniqueness. The result is a list with unique values.",
        examples: &[
            r#"seen = set()"#,
            r#"seen.add(user)"#,
            r#"unique_names = set(names)"#,
            r#"unique_names = set(row["name"] for row in rows)"#,
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
        signature: "split(text: str, separator: str? = None, maxsplit: int? = None) -> list[str]",
        use_when: "Use for top-level text splitting; string method syntax also works.",
        examples: &[r#"parts = split(line, ",")"#, r#"key, rest = "name:value:extra".split(":", 1)"#, r#"words = split(line)"#],
        avoid: &["For line splitting, prefer text.splitlines() when operating on a string."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "join",
        signature: "join(items: iterable | generator, separator: str = \"\") -> str",
        use_when: "Use for top-level list-to-text joining; string method syntax also works.",
        examples: &[
            r#"line = join(fields, ",")"#,
            r#"initials = "".join(word[0] for word in names)"#,
        ],
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
        use_when: "Use for small positional text templates, numbered placeholders, and simple fixed decimal specs.",
        examples: &[
            r#"line = format("{}:{}", name, count)"#,
            r#"line = format("{1}:{0}", name, count)"#,
            r#"amount = format("{:.2f}", total)"#,
        ],
        avoid: &["Use f-strings when they are clearer and do not need format specs."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "print",
        signature: "print(...values: Any) -> Any",
        use_when: "Use only for diagnostic stdout during local probes.",
        examples: &[r#"print("debug:", count)"#],
        avoid: &["Use emit(value) for structured results and write_file/write_json for task outputs."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run",
        signature: r#"run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, background: bool = False, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576) -> record"#,
        use_when: "Use only when the task explicitly needs a POSIX program. Nonzero exits return ok=false with stdout, stderr, and an explanation record. For task commands that may run more than a few seconds but should eventually exit, pass background=True and manage the returned run_id with run_status/run_wait/run_terminate.",
        examples: &[
            r#"result = run(["wc", "-l", "/app/input.txt"])"#,
            r#"result = run(["printf", "ok"], timeout_ms=5000)"#,
            r#"result = run(["sh", "-c", "sleep 0.01 && printf done"], cwd="/app", timeout_ms=5000)"#,
            r#"result = run(["sh", "-c", "printf warning >&2"], stdout="suppress", stderr="capture", max_stderr_bytes=12000)"#,
            r#"if not result.ok:
    emit({"exit_code": result.exit_code, "stderr": result.stderr, "explanation": result.explanation})"#,
        ],
        avoid: &[
            "Do not pass shell strings; pass argv lists.",
            "Do not use run for normal file/JSON/CSV work.",
            "Use background=True for long-running task commands that should eventually exit, such as builds, tests, installs, downloads, benchmarks, or data processing.",
            "Do not use shell backgrounding, nohup, or `&`; use background=True for long task commands, or start_daemon() for servers/services that must stay running while tests execute.",
            "For noisy commands, suppress or cap output explicitly instead of flooding stdout/stderr.",
            "Do not ignore result.ok; inspect stderr, exit_code, timed_out, and explanation before retrying.",
            "If result.still_running is true and result.run_id is present, use run_status(result.run_id), run_wait(result.run_id, timeout_ms=...), or run_terminate(result.run_id).",
            "After run_wait returns still_running=false or done=true, do not call run_wait for that run_id again.",
            "If result.timed_out is true without a run_id, inspect partial output first; rerun with a larger timeout_ms only when the command is expected to be slow.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "must_run",
        signature: r#"must_run(argv: list[str], cwd: str? = None, stdin: str? = None, timeout_ms: int? = None, env: record? = None, stdout: str = "capture", stderr: str = "capture", max_stdout_bytes: int = 1048576, max_stderr_bytes: int = 1048576) -> record"#,
        use_when: "Use for set -e style process steps: it returns the same run record on success and raises a Stone error when the external process exits nonzero or times out.",
        examples: &[
            r#"must_run(["mkdir", "-p", "target/out"])"#,
            r#"step = must_run(["printf", "ok"], timeout_ms=5000)"#,
            r#"must_run(["sh", "-c", "printf input"], stdout="suppress", stderr="capture", max_stderr_bytes=12000)"#,
        ],
        avoid: &[
            "Use run() instead when a nonzero exit is expected and should be handled as data.",
            "Do not use must_run for normal file/JSON/CSV work.",
            "Do not pass shell strings; pass argv lists.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run_status",
        signature: "run_status(run_id: str) -> record",
        use_when: "Gateway mode only. Use after Gateway-backed run() returns still_running=true and a run_id when you want an immediate per-run status check without waiting.",
        examples: &[r#"if "still_running" in result and result.still_running:
    status = run_status(result.run_id)"#],
        avoid: &[
            "Do not use for general workspace state; use state() for runtime and transaction state.",
            "Do not call for normal completed run() results without a run_id.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run_wait",
        signature: "run_wait(run_id: str, timeout_ms: int = 30000) -> record",
        use_when: "Gateway mode only. Use after Gateway-backed run() returns timed_out=true, still_running=true, and a run_id when you intentionally want to wait; timeout_ms=0 waits until finish.",
        examples: &[r#"while "still_running" in result and result.still_running:
    status = run_status(result.run_id)
    result = run_wait(result.run_id, timeout_ms=30000)"#],
        avoid: &[
            "Do not call for normal completed run() results without a run_id.",
            "Do not use long waits through MCP when you need interactive progress; use run_status() and short run_wait() calls.",
            "Do not call again after run_wait returns still_running=false or done=true; inspect the result and continue the task.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "run_terminate",
        signature: "run_terminate(run_id: str) -> record",
        use_when: "Use to stop a Gateway-backed run() that returned still_running=true when the command should not continue.",
        examples: &[r#"if "still_running" in result and result.still_running:
    stopped = run_terminate(result.run_id)"#],
        avoid: &["Prefer run_wait() if the command is expected to finish soon."],
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
        name: "ps",
        signature: "ps(interval_ms: int = 0) -> list[record]",
        use_when: "Use to inspect live processes as typed records without scraping /proc or shelling out to ps.",
        examples: &[
            r#"procs = ps()"#,
            r#"python = where(ps(), lambda p: p["command"].find("python") >= 0)"#,
        ],
        avoid: &[
            "Do not parse `ps aux` text when process id, command, status, cwd, CPU, and memory fields are needed.",
            "Pass a nonzero interval_ms only when cpu_percent matters; it samples over that interval.",
        ],
        aliases: &["process_list"],
    },
    StoneHelpEntry {
        name: "sysinfo",
        signature: r#"sysinfo(section: "os" | "cpu" | "cpu_long" | "mem" | "disks" | "net" | "temp" | "users" | "all" = "all") -> record | list"#,
        use_when: "Use to inspect typed host system facts without shelling out to uname, free, df, ip, or sysctl-style commands.",
        examples: &[
            r#"host = sysinfo("os")"#,
            r#"mem = sysinfo("mem")"#,
            r#"emit({"os": sysinfo("os").os, "cpus": len(sysinfo("cpu"))})"#,
        ],
        avoid: &[
            "Do not parse platform command text when a sysinfo section has the needed fields.",
            "Use sysinfo(\"cpu_long\") only when sampled CPU usage is needed; it waits briefly to sample.",
        ],
        aliases: &["sys", "sys_info"],
    },
    StoneHelpEntry {
        name: "state",
        signature: "state() -> record",
        use_when: "Use to retrieve cheap agent-facing runtime state such as cwd, workspace root, git status, common tool availability, and Gateway transaction state when active.",
        examples: &[r#"snapshot = state()"#, r#"emit(state().workspace)"#],
        avoid: &["Do not shell out to git status or which/version probes when this structured snapshot is enough."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_info",
        signature: r#"attempt_info(attempt: str = "") -> record"#,
        use_when: "Use in Gateway mode to inspect the current task attempt metadata, or a specific attempt when an id is passed.",
        examples: &[r#"me = attempt_info()"#, r#"emit(attempt_info().state)"#],
        avoid: &["Do not infer attempt identity from transaction ids; use the structured attempt record."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_state",
        signature: r#"attempt_state(attempt: str = "", sample_limit: int = 100) -> record"#,
        use_when: "Use in Gateway mode to inspect an attempt plus its transaction diff state.",
        examples: &[r#"state = attempt_state()"#, r#"emit(attempt_state(sample_limit=25).clean)"#],
        avoid: &["Do not treat attempt state as a commit; finish the attempt explicitly when work is resolved."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempts",
        signature: r#"attempts(task: str = "", workspace: str = "", state: str = "") -> list[record]"#,
        use_when: "Use in Gateway mode to list task attempts with optional task, workspace, or lifecycle-state filters.",
        examples: &[r#"active = attempts(state="active")"#],
        avoid: &["Do not scan gateway storage directories directly to discover attempts."],
        aliases: &["attempt_list"],
    },
    StoneHelpEntry {
        name: "attempt_spawn",
        signature: r#"attempt_spawn(task: str, workspace: str, controller: str = "", capability_profile: str = "", container: str = "", workspace_mount: str = "", resource_limits: record = {}, metadata: record = {}) -> record"#,
        use_when: "Use in Gateway mode when a controller needs to create a new top-level task attempt with its own transaction.",
        examples: &[r#"child = attempt_spawn("task-debug", "repo", controller="codex", capability_profile="shell-mcp")"#],
        avoid: &["Do not spawn attempts for ordinary file edits inside the current attempt; use workspace builtins directly."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_fork",
        signature: r#"attempt_fork(parent_attempt: str = "", task: str = "", controller: str = "", capability_profile: str = "", container: str = "", workspace_mount: str = "", resource_limits: record = {}, metadata: record = {}) -> record"#,
        use_when: "Use in Gateway mode to create a child attempt from the current or specified parent attempt workspace state.",
        examples: &[r#"branch = attempt_fork(task="try-alt-fix", controller="codex")"#],
        avoid: &["Do not assume a fork mutates the parent attempt; it returns a separate attempt and transaction."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "attempt_finish",
        signature: r#"attempt_finish(action: "commit" | "rollback" | "fail" | "kill", attempt: str = "", message: str = "", reason: str = "", allow_risky: bool = False) -> record"#,
        use_when: "Use in Gateway mode to close the current or specified attempt by committing, rolling back, failing, or killing it.",
        examples: &[r#"attempt_finish("rollback", reason="debug branch done")"#],
        avoid: &["Do not finish a parent attempt from a child controller unless that capability was explicitly delegated."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_state",
        signature: "env_state(sample_limit: int = 100) -> record",
        use_when: "Use in Gateway mode to inspect uncommitted transaction changes, warnings, and a bounded structured diff.",
        examples: &[r#"changes = env_state()"#, r#"emit(env_state(sample_limit=25).clean)"#],
        avoid: &["Do not wait until the final answer to inspect risky file changes after running commands."],
        aliases: &["env_diff"],
    },
    StoneHelpEntry {
        name: "env_tx_info",
        signature: r#"env_tx_info(tx: str = "") -> record"#,
        use_when: "Use in Gateway mode to inspect transaction metadata such as parent checkpoint and retained checkpoint-run purpose.",
        examples: &[r#"info = env_tx_info()"#, r#"info = env_tx_info(debug.branch_tx)"#],
        avoid: &["Do not infer retained branch lifecycle from path names; use structured metadata."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_txs",
        signature: r#"env_txs(workspace: str = "", purpose: str = "") -> list[record]"#,
        use_when: "Use in Gateway mode to discover open transactions, especially retained checkpoint-run branches.",
        examples: &[r#"debug_branches = env_txs(purpose="checkpoint-run")"#],
        avoid: &["Do not assume retained branches are gone until they disappear from this list."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_finish",
        signature: "env_finish() -> record",
        use_when: "Use before the final answer in Gateway mode to verify the transaction is clean or already closed.",
        examples: &[r#"finish = env_finish()"#, r#"if not finish.ok:
    emit(finish.next_actions)"#],
        avoid: &["Do not leave a dirty transaction unresolved; commit intended changes or restore/rollback unwanted changes."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_restore",
        signature: "env_restore(paths: list[str] | str = []) -> record",
        use_when: "Use in Gateway mode to discard unwanted uncommitted changes for specific paths, or all changes when no paths are passed.",
        examples: &[r#"env_restore(["tmp.log", "build/"])"#, r#"env_restore()"#],
        avoid: &["Do not restore intended task outputs; commit them after review instead."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_checkpoint",
        signature: r#"env_checkpoint(reason: str = "") -> record"#,
        use_when: "Use in Gateway mode to save the current transaction state before a risky repair, verifier attempt, or alternate branch.",
        examples: &[r#"cp = env_checkpoint(reason="before verifier")"#],
        avoid: &["Do not use checkpoints as commits; commit intended final changes as a generation."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_fork",
        signature: "env_fork(checkpoint: str) -> record",
        use_when: "Use in Gateway mode to open an independent transaction branch from a checkpoint.",
        examples: &[r#"branch = env_fork(cp.checkpoint)"#],
        avoid: &["Do not assume the current transaction changes after forking; env_fork returns a new tx id."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_restore_checkpoint",
        signature: "env_restore_checkpoint(checkpoint: str) -> record",
        use_when: "Use in Gateway mode to restore the current transaction back to a named checkpoint state.",
        examples: &[r#"env_restore_checkpoint(cp.checkpoint)"#],
        avoid: &["Do not restore to a checkpoint while expecting long-running commands in the same tx to keep running."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_checkpoints",
        signature: r#"env_checkpoints(workspace: str = "", include_discarded: bool = False) -> list[record]"#,
        use_when: "Use in Gateway mode to inspect active checkpoint branches and storage metrics.",
        examples: &[r#"checkpoints = env_checkpoints()"#],
        avoid: &["Do not parse checkpoint directories directly; use the structured list."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_checkpoint_gc",
        signature: "env_checkpoint_gc(apply: bool = False) -> record",
        use_when: "Use in Gateway mode to inspect checkpoint storage reachability, or remove reclaimable orphan payloads only when apply is true.",
        examples: &[r#"gc = env_checkpoint_gc()"#, r#"env_checkpoint_gc(apply=True)"#],
        avoid: &["Do not pass apply=True without reviewing the dry-run entries first."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_discard_checkpoint",
        signature: "env_discard_checkpoint(checkpoint: str, force: bool = False) -> record",
        use_when: "Use in Gateway mode to discard an unneeded checkpoint branch.",
        examples: &[r#"env_discard_checkpoint(cp.checkpoint)"#],
        avoid: &["Do not discard a checkpoint that may still be needed as a parent unless force is intentional."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_run_checkpoint",
        signature: r#"env_run_checkpoint(checkpoint: str, image: str, argv: list[str], workspace_mount: str = "/app", workdir: str = "/app", timeout_ms: int = 300000, env: record? = None, stdin: str = "", user: str = "", keep_tx: bool = False) -> record"#,
        use_when: "Use in Gateway mode to fork a checkpoint into a branch, run a Linux command there, inspect output and diff, and roll the branch back unless keep_tx is true.",
        examples: &[
            r#"result = env_run_checkpoint(cp.checkpoint, "python:3.12-slim", ["python", "-c", "print('ok')"])"#,
            r#"debug = env_run_checkpoint(cp.checkpoint, "python:3.12-slim", ["pytest", "-q"], keep_tx=True)"#,
        ],
        avoid: &[
            "Do not use retained branches as commits; commit intended final changes explicitly.",
            "Do not use hidden benchmark verifier output as stock pass@1 feedback.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_commit",
        signature: r#"env_commit(message: str = "agent commit", allow_risky: bool = False) -> record"#,
        use_when: "Use in Gateway mode to publish intended transaction changes as a new immutable generation. In blind Gateway agent surface mode, Waymark authorizes intended commits because change inspection is hidden.",
        examples: &[r#"env_commit(message="solve task")"#],
        avoid: &["Do not pass allow_risky=True in full surface mode unless you reviewed warnings for deletes, binary changes, or risky paths."],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "env_rollback",
        signature: "env_rollback() -> record",
        use_when: "Use in Gateway mode to discard and close the whole transaction when the attempted work should not be kept.",
        examples: &[r#"env_rollback()"#],
        avoid: &["Do not rollback after producing the intended answer; use env_commit instead."],
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
        signature: "wait_port(port: int, host: str = \"127.0.0.1\", timeout_ms: int = 30000, protocol: str = \"tcp\") -> record",
        use_when: "Use after start_daemon() when service readiness is represented by a TCP port accepting connections or a UDP endpoint accepting datagrams.",
        examples: &[
            r#"ready = wait_port(9, host="127.0.0.1", timeout_ms=1)"#,
            r#"udp_ready = wait_port(9, protocol="udp", timeout_ms=1)"#,
        ],
        avoid: &[
            "If wait_port() times out, call daemon_status() with a log path before retrying blindly.",
            "UDP has no connection handshake; protocol=\"udp\" only verifies that Stone can send a datagram to the endpoint.",
        ],
        aliases: &[],
    },
    StoneHelpEntry {
        name: "wait_for",
        signature: "wait_for(predicate: lambda, timeout_ms: int = 30000, interval_ms: int = 100, ignore_errors: bool = False) -> record",
        use_when: "Use after start_daemon() or asynchronous setup when readiness is represented by an arbitrary Stone predicate, such as log text, file contents, or structured status.",
        examples: &[
            r#"ready = wait_for(lambda: True, timeout_ms=1000)"#,
            r#"ready = wait_for(lambda: read_file("missing.log").find("READY") >= 0, timeout_ms=10, interval_ms=5, ignore_errors=True)"#,
        ],
        avoid: &[
            "Use wait_port() instead when readiness is just a TCP or UDP port probe.",
            "Keep ignore_errors=False unless transient predicate errors are expected, such as a log file that may not exist yet.",
        ],
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
            "Values: lists, tuple literals as list values, records/dicts, slices, indexing, item assignment, True, False, None.",
            "Record fields can be read as row[\"name\"] or row.name when the field name is identifier-shaped.",
            "Operators: +, -, *, /, //, &, |, <<, >>, comparisons, and/or/not, membership, is None.",
            "Conditional expressions use Python's value if condition else fallback shape.",
            "Functions: def name(arg) works; optional type annotations like def name(arg: str) -> str are checked; immutable default values are supported.",
            "try/except catches runtime evaluation errors; supported handlers are except:, except Exception:, and except Exception as e:.",
            "Lambdas: expression-only callbacks work in sort/map/filter, e.g. lambda r: r[\"name\"].",
            "String methods include strip/lstrip/rstrip, isdigit/isalpha/isalnum, count, split/rsplit/splitlines, replace, join, lower/upper, zfill, startswith, and endswith; split and rsplit accept optional maxsplit and default whitespace splitting.",
            "File handles support read(), readlines()/splitlines(), write(text), and close().",
            "List variables support append(value), extend(values), count(value), mutating sort(key=..., reverse=...), and set-style add(value) for unique append.",
            "Use emit(value) when you want structured data returned to the caller.",
        ],
    },
    StoneHelpTopic {
        name: "unsupported",
        summary: "Common Python habits that fail in Stone, with replacements.",
        bullets: &[
            "No imports/modules/os/pathlib/glob/json; use find/read_json/json_loads/json_dumps.",
            "No isinstance(value, type); use type(value) == \"list\"/\"str\"/\"int\"/\"float\"/\"record\" or direct structural checks.",
            "Lambda is expression-only; use explicit loops when callback logic needs statements or mutation.",
            "No classes/decorators/async/nested functions.",
            "No mutable default args, *args, **kwargs, or keyword calls to user functions.",
            "No try/finally, try/else, except*, or exception classes other than Exception.",
            "Method keyword arguments are intentionally narrow: split(maxsplit=...) and sort(key=..., reverse=...) are supported; most other methods take positional arguments only.",
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
        "env_diff" => "env_state",
        "attempt_list" => "attempts",
        "sys" | "sys_info" => "sysinfo",
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
