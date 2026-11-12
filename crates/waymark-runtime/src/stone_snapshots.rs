//! Snapshots of a directory tree, kept in a Git object store Waymark owns.
//!
//! The store is a bare Git repository outside the workspace
//! (`WAYMARK_SNAPSHOT_STORE`, default `~/.local/state/waymark/snapshots.git`
//! where the command runs). It is used only as a content-addressed object
//! database, through Git's plumbing commands: a snapshot is the id of a tree
//! object, built with `ls-files`/`update-index`/`write-tree` from a per-root
//! index inside the store; a diff is `diff-tree`; `apply_patch` builds the
//! tree of a snapshot plus a patch in the store (`read-tree` plus
//! `apply --cached`). No step ever writes a file in the directory itself.
//!
//! The workspace's own `.git` (index, refs, HEAD, config, hooks) is never read
//! or written; when the root has one, the store borrows its objects
//! (`objects/info/alternates`) so nothing is copied. Any directory works, Git
//! repository or not. Files the root's `.gitignore` files ignore (build
//! outputs, caches) are not part of a snapshot.
//!
//! The steps run as one `sh` script where commands run (locally or in the
//! Gateway's container), so snapshots live next to the files they describe.

use nu_protocol::{Record, ShellError, Span, Value};

use crate::stone_eval::stone_typed_error;

/// Environment variable naming the store directory (read by `SCRIPT`).
pub(crate) const STORE_ENV: &str = "WAYMARK_SNAPSHOT_STORE";

/// `sh -c SCRIPT waymark-snapshot OP ROOT [BASE [SINCE TO]]` with OP `save`,
/// `diff`, or `apply`; patch text, if any, on stdin. Exit 2: setup problem;
/// 3: a snapshot is not in the store; 4: the patch does not apply. Output
/// starts with the root and a tree id.
pub(crate) const SCRIPT: &str = r#"set -e
op=$1; root=$2; base=$3; since=$4; to=$5
command -v git >/dev/null 2>&1 || { echo "snapshots need the git program, which is not installed where commands run" >&2; exit 2; }
[ -d "$root" ] || { echo "$root is not a directory" >&2; exit 2; }
root=$(cd "$root" && pwd -P)
store=${WAYMARK_SNAPSHOT_STORE:-${XDG_STATE_HOME:-${HOME:-/tmp}/.local/state}/waymark/snapshots.git}
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0
unset GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE GIT_CEILING_DIRECTORIES
if [ ! -f "$store/HEAD" ]; then mkdir -p "$store" && git init -q --bare "$store" || { echo "could not create the snapshot store $store" >&2; exit 2; }; fi
export GIT_DIR="$store" GIT_WORK_TREE="$root"
key=$(printf %s "$root" | git hash-object --stdin)
export GIT_INDEX_FILE="$store/index-$key"
alt="$store/objects/info/alternates"
if [ -d "$root/.git/objects" ] && ! grep -qxF "$root/.git/objects" "$alt" 2>/dev/null; then echo "$root/.git/objects" >> "$alt"; fi
need() { git cat-file -e "$1^{tree}" 2>/dev/null || { echo "snapshot $1 is not in the snapshot store $store" >&2; exit 3; }; }
save() {
  git update-index -q --refresh >/dev/null 2>&1 || true
  git ls-files -z --deleted | git update-index -z --remove --stdin
  git ls-files -z --modified | git update-index -z --add --remove --stdin
  git ls-files -z --others --exclude-standard | git update-index -z --add --stdin
  tree=$(git write-tree)
  git update-ref "refs/snapshots/$tree" "$tree"
}
case "$op" in
save)
  save; printf '%s\n%s\n' "$root" "$tree" ;;
diff)
  need "$base"
  if [ -n "$since" ]; then need "$since"; tree=$since; else save; fi
  printf '%s\n%s\n' "$root" "$tree"
  git diff-tree -r -z --no-renames --name-status "$base" "$tree"; printf '\n'
  git diff-tree -r -p --binary --no-renames "$base" "$tree" ;;
apply)
  need "$base"
  patch="$store/patch-$$"; build="$store/build-$$"
  trap 'rm -f "$patch" "$build"' EXIT
  if [ -n "$since" ]; then
    need "$since"; need "$to"
    git diff-tree -r -p --binary --no-renames "$since" "$to" > "$patch"
  else
    cat > "$patch"
  fi
  tree=$base
  if [ -s "$patch" ]; then
    GIT_INDEX_FILE="$build" git read-tree "$base"
    GIT_INDEX_FILE="$build" git apply --cached --whitespace=nowarn "$patch" || exit 4
    tree=$(GIT_INDEX_FILE="$build" git write-tree)
    git update-ref "refs/snapshots/$tree" "$tree"
  fi
  printf '%s\n%s\n' "$root" "$tree" ;;
*)
  echo "unknown snapshot step $op" >&2; exit 2 ;;
esac
"#;

/// A snapshot as a program passes it back: the root it was taken of and its
/// tree id.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SnapshotRef {
    pub root: String,
    pub id: String,
}

/// The patch `apply_patch` applies.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PatchSpec {
    None,
    /// Patch text, applied with `git apply`.
    Text(String),
    /// A `diff(since=...)` result: the change from one snapshot to another,
    /// replayed from the store (no text round trip).
    Trees { since: String, to: String },
}

const PATCH_HINT: &str = "apply_patch(snapshot, patch) takes a patch that is a diff(since=...) result, patch text, or None";

fn string_field(record: &Record, name: &str) -> Option<String> {
    match record.get(name) {
        Some(Value::String { val, .. }) if !val.is_empty() => Some(val.clone()),
        _ => None,
    }
}

fn is_tree_id(id: &str) -> bool {
    (id.len() == 40 || id.len() == 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A `Snapshot` record (or a dict with its `root` and `id`).
pub(crate) fn snapshot_ref(value: &Value, context: &str, what: &str) -> Result<SnapshotRef, ShellError> {
    let wrong = || {
        stone_typed_error(
            "TypeError",
            context,
            format!("{context}() {what} takes a snapshot from snapshot() (a record with root and id)"),
        )
    };
    let Value::Record { val, .. } = value else {
        return Err(wrong());
    };
    let (Some(root), Some(id)) = (string_field(val, "root"), string_field(val, "id")) else {
        return Err(wrong());
    };
    if !is_tree_id(&id) {
        return Err(stone_typed_error(
            "ValueError",
            context,
            format!("{context}() {what}: {id:?} is not a snapshot id"),
        ));
    }
    Ok(SnapshotRef { root, id })
}

pub(crate) fn patch_spec(value: &Value, context: &str) -> Result<PatchSpec, ShellError> {
    match value {
        Value::Nothing { .. } => Ok(PatchSpec::None),
        Value::String { val, .. } => Ok(if val.trim().is_empty() {
            PatchSpec::None
        } else {
            PatchSpec::Text(val.clone())
        }),
        Value::Record { val, .. } => match (string_field(val, "since"), string_field(val, "to")) {
            (Some(since), Some(to)) if is_tree_id(&since) && is_tree_id(&to) => {
                Ok(PatchSpec::Trees { since, to })
            }
            _ => Err(stone_typed_error("TypeError", context, PATCH_HINT.to_string())),
        },
        _ => Err(stone_typed_error("TypeError", context, PATCH_HINT.to_string())),
    }
}

/// The error for a failed snapshot step.
pub(crate) fn step_error(context: &str, exit: Option<i64>, stderr: &str) -> ShellError {
    let detail = stderr.trim();
    match exit {
        Some(3) => stone_typed_error("FileNotFoundError", context, format!("{context}(): {detail}")),
        Some(4) => stone_typed_error(
            "ValueError",
            context,
            format!("{context}(): the patch does not apply to the snapshot: {detail}"),
        ),
        _ => stone_typed_error("OSError", context, format!("{context}(): snapshot step failed: {detail}")),
    }
}

fn lines(stdout: &str, count: usize, context: &str) -> Result<(Vec<String>, String), ShellError> {
    let mut parts = stdout.splitn(count + 1, '\n');
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        match parts.next() {
            Some(line) if !line.is_empty() => out.push(line.to_string()),
            _ => {
                return Err(stone_typed_error(
                    "OSError",
                    context,
                    format!("{context}(): unexpected output from the snapshot step: {stdout:?}"),
                ))
            }
        }
    }
    Ok((out, parts.next().unwrap_or("").to_string()))
}

/// `save` output: (root, tree id).
pub(crate) fn parse_save(stdout: &str, context: &str) -> Result<SnapshotRef, ShellError> {
    let (lines, _) = lines(stdout, 2, context)?;
    Ok(SnapshotRef { root: lines[0].clone(), id: lines[1].clone() })
}

pub(crate) fn snapshot_record(snapshot: &SnapshotRef) -> Record {
    let span = Span::unknown();
    let mut record = Record::new();
    record.push("root", Value::string(snapshot.root.clone(), span));
    record.push("id", Value::string(snapshot.id.clone(), span));
    record.push("seq", Value::int(next_seq(), span));
    record
}

/// A number that increases with every runtime result that carries one
/// (`RunResult`, `TestRun`, `Snapshot`), so results can be put in the order
/// they were made. A scalar counter (`vm_atomic` in global_state.allow).
#[cfg(not(target_os = "hermit"))]
pub(crate) fn next_seq() -> i64 {
    static RESULT_SEQ: crate::global_state::VmAtomicU64 = crate::global_state::VmAtomicU64::new(1);
    RESULT_SEQ.fetch_add_relaxed(1) as i64
}

#[cfg(target_os = "hermit")]
pub(crate) fn next_seq() -> i64 {
    0
}

fn change_name(status: &str) -> &'static str {
    match status.chars().next() {
        Some('A') => "added",
        Some('D') => "deleted",
        Some('T') => "type_changed",
        _ => "modified",
    }
}

/// `diff` output as a `TreeDiff` record: the root, the two snapshot ids, each
/// changed path, and the patch text.
pub(crate) fn parse_diff(stdout: &str, since: &str, context: &str) -> Result<Record, ShellError> {
    let span = Span::unknown();
    let (head, rest) = lines(stdout, 2, context)?;
    let (names, patch) = match rest.strip_prefix('\n') {
        Some(patch) => ("", patch),
        None => match rest.find("\0\n") {
            Some(end) => (&rest[..end], &rest[end + 2..]),
            None => (rest.as_str(), ""),
        },
    };
    let fields: Vec<&str> = names.split('\0').filter(|part| !part.is_empty()).collect();
    let files = fields
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .map(|pair| {
            let mut file = Record::new();
            file.push("path", Value::string(pair[1], span));
            file.push("change", Value::string(change_name(pair[0]), span));
            Value::record(file, span)
        })
        .collect();
    let mut record = Record::new();
    record.push("root", Value::string(head[0].clone(), span));
    record.push("since", Value::string(since, span));
    record.push("to", Value::string(head[1].clone(), span));
    record.push("files", Value::list(files, span));
    record.push("patch", Value::string(patch, span));
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_output_reads_into_paths_and_patch() {
        let out = "/r\nbbbb\nM\0a.txt\0D\0b.txt\0A\0dir/c d.txt\0\ndiff --git a/a.txt b/a.txt\n+x\n";
        let record = parse_diff(out, "aaaa", "diff").unwrap();
        let Some(Value::List { vals, .. }) = record.get("files") else { panic!() };
        assert_eq!(vals.len(), 3);
        let Value::Record { val, .. } = &vals[2] else { panic!() };
        assert_eq!(val.get("path").unwrap().as_str().unwrap(), "dir/c d.txt");
        assert_eq!(val.get("change").unwrap().as_str().unwrap(), "added");
        assert_eq!(record.get("patch").unwrap().as_str().unwrap(), "diff --git a/a.txt b/a.txt\n+x\n");
        let empty = parse_diff("/r\nbbbb\n\n", "aaaa", "diff").unwrap();
        assert!(matches!(empty.get("files"), Some(Value::List { vals, .. }) if vals.is_empty()));
        assert_eq!(empty.get("patch").unwrap().as_str().unwrap(), "");
    }
}
