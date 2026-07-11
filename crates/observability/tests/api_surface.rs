//! Guards `observability`'s public API against `opentelemetry*` leaks.
//!
//! Crate wires otel providers/exporters internally but never exposes them: guard
//! fields stay private, builders stay `pub(crate)`. Consumers see only
//! `observability`-owned types. This lets otel version bump internally without
//! breaking downstream signatures.

use std::{
    fs,
    path::{Path, PathBuf},
};

// NOTE: line-scan guard, not full type resolution. Catches `opentelemetry` substring
// on any `pub` signature/re-export line. Won't catch it hiding inside a type alias body
// on a later line, or an aliased import under a different name.
const PUB_PREFIXES: &[&str] = &[
    "pub fn ",
    "pub struct ",
    "pub enum ",
    "pub type ",
    "pub const ",
    "pub static ",
    "pub use ",
];

const BANNED_SUBSTRING: &str = "opentelemetry";

/// Recursively collect every `.rs` file path under `dir`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_opentelemetry_in_public_api() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(
        !files.is_empty(),
        "no .rs files found under {src_dir:?}, scan can't run"
    );

    // guard scan can't pass vacuously: crate root must exist and hold real code.
    let lib_rs = src_dir.join("lib.rs");
    let lib_contents =
        fs::read_to_string(&lib_rs).unwrap_or_else(|e| panic!("failed to read {lib_rs:?}: {e}"));
    assert!(
        !lib_contents.trim().is_empty(),
        "{lib_rs:?} is empty, scan target missing"
    );

    let mut violations = Vec::new();
    for file in &files {
        let contents =
            fs::read_to_string(file).unwrap_or_else(|e| panic!("failed to read {file:?}: {e}"));
        for (lineno, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            let is_pub_signature = PUB_PREFIXES.iter().any(|p| trimmed.starts_with(p));
            if is_pub_signature && trimmed.contains(BANNED_SUBSTRING) {
                violations.push(format!("{}:{}: {}", file.display(), lineno + 1, trimmed));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "opentelemetry leaked into public API surface:\n{}",
        violations.join("\n")
    );
}
