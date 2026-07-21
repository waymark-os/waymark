// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let source = root.join("src");
    let policy = root.join("global_state.allow");
    let nu_utils_source = root.join("../../vendor/nu-utils/src");
    let nu_utils_policy = root.join("nu_utils_global_state.allow");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed={}", policy.display());
    println!("cargo:rerun-if-changed={}", nu_utils_source.display());
    println!("cargo:rerun-if-changed={}", nu_utils_policy.display());
    if let Err(error) = waymark_global_audit::audit_source_tree(&source, &policy) {
        panic!("{error}");
    }
    if let Err(error) = waymark_global_audit::audit_source_tree(&nu_utils_source, &nu_utils_policy)
    {
        panic!("vendored nu-utils {error}");
    }
}
