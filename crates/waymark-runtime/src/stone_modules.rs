// SPDX-License-Identifier: MIT OR Apache-2.0

//! Stone modules: `from lib import name`.
//!
//! Imports are linked before the program runs, so a missing module, an
//! unknown name, or a module that would run code at import is reported
//! before any statement executes.
//!
//! - `lib` is found as `lib.stone` (and `pkg.util` as `pkg/util.stone`) in
//!   the command's working directory, then `.stone/lib` under it, then
//!   `~/.stone/lib`, then `/usr/share/waymark/stone/lib`.
//!   `WAYMARK_STONE_LIB_DIRS` replaces this search path.
//! - A module's top level only defines functions, error classes, and
//!   constants, and imports other modules (checked in `stone_scope`).
//! - A module's names are qualified (`helper` becomes `lib.helper`), so they
//!   never collide with the importer's names. Each module is linked once and
//!   placed ahead of the program as a `Stmt::Module`; running it defines the
//!   functions and freezes the constants. `Stmt::Import` then binds each
//!   imported function like a top-level `def` (same call path as a local
//!   function) and each constant as a variable.
//! - Error classes keep their own names, because error types are global and
//!   known by name. Two modules may not define the same error class.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use nu_protocol::{shell_error::generic::GenericError, ShellError};
use sha2::{Digest, Sha256};

use crate::stone_ast::{
    import_suggestions_for_module, lower_module_source, AssignTarget, ImportKind, ModuleInit,
    Program, Stmt,
};
use crate::stone_errors::is_builtin_error_type;
use crate::stone_eval::stone_builtin_names;
use crate::stone_scope::resolve_module;

/// Names the evaluator accepts besides the builtin table: dict helpers
/// called as functions, `isinstance`, and JSON spellings of literals.
const MODULE_EXTRA_NAMES: &[&str] = &[
    "get",
    "items",
    "keys",
    "values",
    "isinstance",
    "true",
    "false",
    "null",
];

/// Link the program's imports, placing each imported module's definitions
/// ahead of the program. A program without imports is left untouched.
pub(crate) fn link_program(program: &mut Program, cwd: &Path) -> Result<(), ShellError> {
    check_imports_at_top_level(&program.statements, true)?;
    if !program
        .statements
        .iter()
        .any(|statement| matches!(statement, Stmt::Import { .. }))
    {
        return Ok(());
    }
    let mut linker = Linker {
        cwd: cwd.to_path_buf(),
        dirs: stone_module_dirs(cwd),
        exports: HashMap::new(),
        loading: Vec::new(),
        error_classes: HashMap::new(),
        linked: Vec::new(),
    };
    linker.link_imports(&mut program.statements, None)?;
    let mut statements = std::mem::take(&mut linker.linked);
    statements.append(&mut program.statements);
    program.statements = statements;
    Ok(())
}

pub(crate) fn stone_module_dirs(cwd: &Path) -> Vec<PathBuf> {
    if let Some(raw) = env::var_os("WAYMARK_STONE_LIB_DIRS") {
        return env::split_paths(&raw).collect();
    }
    let mut dirs = vec![cwd.to_path_buf(), cwd.join(".stone").join("lib")];
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".stone").join("lib"));
    }
    dirs.push(PathBuf::from("/usr/share/waymark/stone/lib"));
    dirs
}

struct Linker {
    cwd: PathBuf,
    dirs: Vec<PathBuf>,
    /// For each linked module: exported name -> kind.
    exports: HashMap<String, HashMap<String, ImportKind>>,
    /// Modules being linked, outermost first, to report import cycles.
    loading: Vec<String>,
    /// Error class name -> the module that defines it.
    error_classes: HashMap<String, String>,
    /// Linked modules in dependency order.
    linked: Vec<Stmt>,
}

impl Linker {
    /// Resolve every top-level import in `statements` (the importing file is
    /// `importer`, or the program itself).
    fn link_imports(
        &mut self,
        statements: &mut [Stmt],
        importer: Option<&str>,
    ) -> Result<(), ShellError> {
        for statement in statements {
            let Stmt::Import { module, names } = statement else {
                continue;
            };
            self.load(module, importer)?;
            let exports = &self.exports[module.as_str()];
            for import in names {
                let Some(kind) = exports.get(&import.name) else {
                    let mut available = exports.keys().cloned().collect::<Vec<_>>();
                    available.sort();
                    return Err(module_error(
                        "unknown module name",
                        format!(
                            "module `{module}` has no `{}`; it defines: {}",
                            import.name,
                            if available.is_empty() {
                                "nothing".to_string()
                            } else {
                                available.join(", ")
                            }
                        ),
                    ));
                };
                if *kind == ImportKind::ErrorClass && import.local != import.name {
                    return Err(module_error(
                        "unsupported Stone import",
                        format!(
                            "error class `{}` cannot be renamed on import (`as {}`): error types are known by their own name. Write `from {module} import {}`",
                            import.name, import.local, import.name
                        ),
                    ));
                }
                import.kind = *kind;
            }
        }
        Ok(())
    }

    fn load(&mut self, module: &str, importer: Option<&str>) -> Result<(), ShellError> {
        if self.exports.contains_key(module) {
            return Ok(());
        }
        if let Some(start) = self.loading.iter().position(|name| name == module) {
            let mut cycle = self.loading[start..].to_vec();
            cycle.push(module.to_string());
            return Err(module_error(
                "Stone import cycle",
                format!(
                    "modules import each other in a cycle: {}. Move the shared definitions into a module that both import",
                    cycle.join(" -> ")
                ),
            ));
        }
        let relative = format!("{}.stone", module.replace('.', "/"));
        let Some(path) = self
            .dirs
            .iter()
            .map(|dir| dir.join(&relative))
            .find(|path| path.is_file())
        else {
            let searched = self
                .dirs
                .iter()
                .map(|dir| self.display(dir))
                .collect::<Vec<_>>()
                .join(", ");
            let wanted = match importer {
                Some(importer) => format!(" (imported by module `{importer}`)"),
                None => String::new(),
            };
            return Err(module_error(
                "Stone module not found",
                format!(
                    "no Stone module `{module}`{wanted}: looked for {relative} in {searched}. Python's standard library is not available. {}",
                    import_suggestions_for_module(module)
                ),
            ));
        };
        let shown = self.display(&path);
        let source = fs::read_to_string(&path).map_err(|err| {
            module_error(
                "Stone module not readable",
                format!("cannot read module `{module}` at {shown}: {err}"),
            )
        })?;
        let mut program =
            lower_module_source(&source).map_err(|err| in_module(err, module, &shown))?;

        // Link the module's own imports first: which names are error classes
        // decides which names get qualified.
        self.loading.push(module.to_string());
        let linked = self.link_imports(&mut program.statements, Some(module));
        self.loading.pop();
        linked.map_err(|err| in_module(err, module, &shown))?;
        let reads =
            resolve_module(&mut program, module).map_err(|err| in_module(err, module, &shown))?;
        self.check_free_names(&program.statements, &reads)
            .map_err(|err| in_module(err, module, &shown))?;

        let prefix = format!("{module}.");
        let mut exports = HashMap::new();
        let mut constants = Vec::new();
        for statement in &program.statements {
            match statement {
                Stmt::FunctionDef(function) if function.error_base.is_some() => {
                    if let Some(other) = self
                        .error_classes
                        .insert(function.name.clone(), module.to_string())
                    {
                        return Err(module_error(
                            "duplicate Stone error class",
                            format!(
                                "error class `{}` is defined by both module `{other}` and module `{module}`; error types are known by name, so rename one",
                                function.name
                            ),
                        ));
                    }
                    exports.insert(function.name.clone(), ImportKind::ErrorClass);
                }
                Stmt::FunctionDef(function) => {
                    if let Some(name) = function.name.strip_prefix(&prefix) {
                        exports.insert(name.to_string(), ImportKind::Function);
                    }
                }
                Stmt::Assign { target, .. } => {
                    let names = match target {
                        AssignTarget::Name(name) => vec![name.clone()],
                        AssignTarget::Tuple(names) => names.clone(),
                        AssignTarget::Subscript { .. } => Vec::new(),
                    };
                    for name in names {
                        if let Some(short) = name.strip_prefix(&prefix) {
                            exports.insert(short.to_string(), ImportKind::Value);
                            constants.push(name);
                        }
                    }
                }
                Stmt::Import { names, .. } => {
                    for import in names {
                        if import.kind == ImportKind::ErrorClass {
                            exports.insert(import.local.clone(), ImportKind::ErrorClass);
                        } else if let Some(short) = import.local.strip_prefix(&prefix) {
                            // Re-exported under the module's qualified name.
                            exports.insert(short.to_string(), import.kind);
                        }
                    }
                }
                _ => {}
            }
        }
        constants.sort();
        constants.dedup();
        self.exports.insert(module.to_string(), exports);
        self.linked.push(Stmt::Module(Box::new(ModuleInit {
            name: module.to_string(),
            path: path.display().to_string(),
            sha256: format!("{:x}", Sha256::digest(source.as_bytes())),
            body: program.statements,
            constants,
        })));
        Ok(())
    }

    /// A module's code sees only its own names, its imports, builtins, and
    /// error types. Anything else would silently read the importer's
    /// variables, so it is an error here.
    fn check_free_names(
        &self,
        statements: &[Stmt],
        reads: &BTreeSet<String>,
    ) -> Result<(), ShellError> {
        let mut error_classes = BTreeSet::new();
        for statement in statements {
            match statement {
                Stmt::FunctionDef(function) if function.error_base.is_some() => {
                    error_classes.insert(function.name.as_str());
                }
                Stmt::Import { names, .. } => error_classes.extend(
                    names
                        .iter()
                        .filter(|import| import.kind == ImportKind::ErrorClass)
                        .map(|import| import.local.as_str()),
                ),
                _ => {}
            }
        }
        let builtins = stone_builtin_names();
        let unknown = reads
            .iter()
            .filter(|name| {
                !builtins.contains(&name.as_str())
                    && !MODULE_EXTRA_NAMES.contains(&name.as_str())
                    && !is_builtin_error_type(name)
                    && !error_classes.contains(name.as_str())
            })
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>();
        if unknown.is_empty() {
            return Ok(());
        }
        Err(module_error(
            "unknown name in Stone module",
            format!(
                "the module uses {}, which it neither defines nor imports. A module's functions see only the module's own names, its imports, and builtins, never the importing program's variables: define it in the module, import it, or pass it in as an argument",
                unknown.join(", ")
            ),
        ))
    }

    fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.cwd) {
            Ok(relative) if relative.as_os_str().is_empty() => ".".to_string(),
            Ok(relative) => relative.display().to_string(),
            Err(_) => path.display().to_string(),
        }
    }
}

/// Imports belong at the top level of a file, where they are linked.
fn check_imports_at_top_level(statements: &[Stmt], top: bool) -> Result<(), ShellError> {
    for statement in statements {
        let nested: Vec<&[Stmt]> = match statement {
            Stmt::Import { module, names } if !top => {
                let names = names
                    .iter()
                    .map(|import| import.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(module_error(
                    "unsupported Stone import",
                    format!(
                        "`from {module} import {names}` must be at the top level of the file, not inside a function or block: modules are linked before the program runs"
                    ),
                ));
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } | Stmt::With { body, .. } => {
                vec![body]
            }
            Stmt::If {
                then_branch,
                else_branch,
                ..
            } => vec![then_branch, else_branch],
            Stmt::Try { body, handlers } => {
                let mut blocks: Vec<&[Stmt]> = vec![body];
                blocks.extend(handlers.iter().map(|handler| handler.body.as_slice()));
                blocks
            }
            Stmt::FunctionDef(function) => vec![&function.body],
            _ => Vec::new(),
        };
        for block in nested {
            check_imports_at_top_level(block, false)?;
        }
    }
    Ok(())
}

fn module_error(title: &'static str, message: String) -> ShellError {
    ShellError::Generic(GenericError::new_internal(title, message).with_code("stone_module_error"))
}

/// Name the module file in an error raised while linking it.
fn in_module(error: ShellError, module: &str, path: &str) -> ShellError {
    match error {
        ShellError::Generic(mut generic) => {
            let prefix = format!("in module `{module}` ({path}): ");
            if !generic.msg.starts_with("in module `") {
                generic.msg = format!("{prefix}{}", generic.msg).into();
            }
            ShellError::Generic(generic)
        }
        other => other,
    }
}
