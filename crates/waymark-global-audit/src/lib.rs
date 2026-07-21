// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fail-closed source audit for VM-global Rust state.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use syn::parse::{Parse, ParseStream};
use syn::visit::{self, Visit};
use syn::{ItemStatic, Macro, StaticMutability, Type};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GlobalForm {
    Static,
    ThreadLocal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GlobalDecl {
    file: String,
    name: String,
    form: GlobalForm,
    type_name: String,
    mutable: bool,
}

impl GlobalDecl {
    fn key(&self) -> String {
        format!("{} {}", self.file, self.name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PolicyEntry {
    file: String,
    name: String,
    class: String,
}

impl PolicyEntry {
    fn key(&self) -> String {
        format!("{} {}", self.file, self.name)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditError {
    messages: Vec<String>,
}

impl AuditError {
    pub fn messages(&self) -> &[String] {
        &self.messages
    }
}

impl fmt::Display for AuditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "VM-global state audit failed:")?;
        for message in &self.messages {
            writeln!(formatter, "  - {message}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AuditError {}

/// Audit every Rust source below `source_root` against the explicit policy.
///
/// Policy lines have the form `relative/file.rs SYMBOL CLASS`. Blank lines and
/// lines beginning with `#` are ignored. Both new globals and stale policy
/// entries fail the audit so the policy remains a reviewed inventory.
pub fn audit_source_tree(source_root: &Path, policy_path: &Path) -> Result<(), AuditError> {
    let mut messages = Vec::new();
    let policy = match parse_policy(policy_path) {
        Ok(policy) => policy,
        Err(message) => {
            return Err(AuditError {
                messages: vec![message],
            });
        }
    };
    let files = match rust_files(source_root) {
        Ok(files) => files,
        Err(message) => {
            return Err(AuditError {
                messages: vec![message],
            });
        }
    };

    let mut declarations = BTreeMap::new();
    for file in files {
        let relative = match file.strip_prefix(source_root) {
            Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
            Err(_) => {
                messages.push(format!("source escaped audit root: {}", file.display()));
                continue;
            }
        };
        let source = match fs::read_to_string(&file) {
            Ok(source) => source,
            Err(error) => {
                messages.push(format!("failed to read {}: {error}", file.display()));
                continue;
            }
        };
        let syntax = match syn::parse_file(&source) {
            Ok(syntax) => syntax,
            Err(error) => {
                messages.push(format!("failed to parse {}: {error}", file.display()));
                continue;
            }
        };
        let mut collector = GlobalCollector {
            file: relative,
            declarations: Vec::new(),
            messages: Vec::new(),
        };
        collector.visit_file(&syntax);
        messages.extend(collector.messages);
        for declaration in collector.declarations {
            let key = declaration.key();
            if declarations.insert(key.clone(), declaration).is_some() {
                messages.push(format!("duplicate global identity `{key}`"));
            }
        }
    }

    for (key, declaration) in &declarations {
        let Some(entry) = policy.get(key) else {
            messages.push(format!(
                "unapproved global `{key}` ({:?}, type `{}`); classify it or remove it",
                declaration.form, declaration.type_name
            ));
            continue;
        };
        if declaration.mutable {
            messages.push(format!("`{key}` uses forbidden `static mut`"));
        }
        if let Some(message) = validate_class(declaration, entry) {
            messages.push(message);
        }
    }

    for key in policy.keys() {
        if !declarations.contains_key(key) {
            messages.push(format!("stale global policy entry `{key}`"));
        }
    }

    if messages.is_empty() {
        Ok(())
    } else {
        messages.sort();
        messages.dedup();
        Err(AuditError { messages })
    }
}

fn validate_class(declaration: &GlobalDecl, entry: &PolicyEntry) -> Option<String> {
    let valid = match entry.class.as_str() {
        "vm_atomic" => {
            declaration.form == GlobalForm::Static && declaration.type_name == "VmAtomicU64"
        }
        "vm_frozen" => {
            declaration.form == GlobalForm::Static && declaration.type_name == "VmFrozen"
        }
        "vm_once" => declaration.form == GlobalForm::Static && declaration.type_name == "VmOnce",
        "process_tls" => {
            declaration.form == GlobalForm::ThreadLocal && declaration.type_name == "ProcessTls"
        }
        "dependency_tls" => declaration.form == GlobalForm::ThreadLocal,
        _ => false,
    };
    (!valid).then(|| {
        format!(
            "global `{}` is classified `{}` but has {:?} type `{}`",
            declaration.key(),
            entry.class,
            declaration.form,
            declaration.type_name
        )
    })
}

fn parse_policy(path: &Path) -> Result<BTreeMap<String, PolicyEntry>, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read global policy {}: {error}", path.display()))?;
    let mut policy = BTreeMap::new();
    for (index, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(format!(
                "{}:{} requires `FILE SYMBOL CLASS`",
                path.display(),
                index + 1
            ));
        }
        let entry = PolicyEntry {
            file: fields[0].to_owned(),
            name: fields[1].to_owned(),
            class: fields[2].to_owned(),
        };
        let key = entry.key();
        if policy.insert(key.clone(), entry).is_some() {
            return Err(format!(
                "{}:{} duplicates `{key}`",
                path.display(),
                index + 1
            ));
        }
    }
    Ok(policy)
}

fn rust_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        let entries = fs::read_dir(&path).map_err(|error| {
            format!(
                "failed to read source directory {}: {error}",
                path.display()
            )
        })?;
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("failed to read entry in {}: {error}", path.display()))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("failed to stat {}: {error}", entry.path().display()))?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("rs")
            {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

struct GlobalCollector {
    file: String,
    declarations: Vec<GlobalDecl>,
    messages: Vec<String>,
}

impl GlobalCollector {
    fn record_static(&mut self, item: &ItemStatic, form: GlobalForm) {
        self.declarations.push(GlobalDecl {
            file: self.file.clone(),
            name: item.ident.to_string(),
            form,
            type_name: type_name(&item.ty),
            mutable: matches!(item.mutability, StaticMutability::Mut(_)),
        });
    }

    fn inspect_macro(&mut self, item: &Macro) {
        let name = item
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        match name.as_deref() {
            Some("thread_local") => match syn::parse2::<ThreadLocalBody>(item.tokens.clone()) {
                Ok(body) => {
                    for declaration in body.declarations {
                        self.record_static(&declaration, GlobalForm::ThreadLocal);
                    }
                }
                Err(error) => self.messages.push(format!(
                    "failed to inspect thread_local! in {}: {error}",
                    self.file
                )),
            },
            Some("lazy_static") => self.messages.push(format!(
                "{} contains forbidden lazy_static!; use an audited VM wrapper",
                self.file
            )),
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for GlobalCollector {
    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        self.record_static(item, GlobalForm::Static);
        visit::visit_item_static(self, item);
    }

    fn visit_macro(&mut self, item: &'ast Macro) {
        self.inspect_macro(item);
        visit::visit_macro(self, item);
    }
}

struct ThreadLocalBody {
    declarations: Vec<ItemStatic>,
}

impl Parse for ThreadLocalBody {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut declarations = Vec::new();
        while !input.is_empty() {
            declarations.push(input.parse()?);
        }
        Ok(Self { declarations })
    }
}

fn type_name(ty: &Type) -> String {
    let Type::Path(path) = ty else {
        return "<non-path>".to_owned();
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_else(|| "<empty-path>".to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::audit_source_tree;

    #[test]
    fn accepts_only_classified_wrapper_globals() {
        let fixture = Fixture::new("accept");
        fixture.write_source(
            "runtime.rs",
            r#"
static NEXT: VmAtomicU64 = VmAtomicU64::new(0);
thread_local! {
    static STATE: ProcessTls<Vec<u8>> = const { ProcessTls::new(Vec::new()) };
}
"#,
        );
        fixture.write_policy("runtime.rs NEXT vm_atomic\nruntime.rs STATE process_tls\n");
        audit_source_tree(&fixture.source, &fixture.policy).expect("valid policy");
    }

    #[test]
    fn rejects_unapproved_and_misclassified_globals() {
        let fixture = Fixture::new("reject");
        fixture.write_source(
            "runtime.rs",
            "fn initialize() { static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new(); }\n",
        );
        fixture.write_policy("runtime.rs CACHE vm_once\n");
        let error = audit_source_tree(&fixture.source, &fixture.policy)
            .expect_err("raw OnceLock must fail");
        assert!(error.to_string().contains("type `OnceLock`"));

        fixture.write_policy("");
        let error = audit_source_tree(&fixture.source, &fixture.policy)
            .expect_err("unapproved global must fail");
        assert!(error.to_string().contains("unapproved global"));
    }

    #[test]
    fn rejects_stale_policy_entries_and_lazy_static_macros() {
        let fixture = Fixture::new("stale");
        fixture.write_source(
            "runtime.rs",
            "lazy_static! { static ref BAD: String = String::new(); }",
        );
        fixture.write_policy("runtime.rs GONE vm_atomic\n");
        let error = audit_source_tree(&fixture.source, &fixture.policy)
            .expect_err("stale and lazy globals must fail");
        let message = error.to_string();
        assert!(message.contains("forbidden lazy_static"));
        assert!(message.contains("stale global policy entry"));
    }

    struct Fixture {
        root: std::path::PathBuf,
        source: std::path::PathBuf,
        policy: std::path::PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("waymark-global-audit-{name}-{nonce}"));
            let source = root.join("src");
            let policy = root.join("globals.allow");
            fs::create_dir_all(&source).expect("fixture source");
            Self {
                root,
                source,
                policy,
            }
        }

        fn write_source(&self, name: &str, source: &str) {
            fs::write(self.source.join(name), source).expect("fixture Rust source");
        }

        fn write_policy(&self, source: &str) {
            fs::write(&self.policy, source).expect("fixture policy");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
