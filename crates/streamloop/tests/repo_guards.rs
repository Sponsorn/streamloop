//! Source guards over the whole workspace.

use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_command_new_outside_procs() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut files = vec![];
    rust_files(&root.join("supervisor").join("src"), &mut files);
    for krate in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        rust_files(&krate.path().join("src"), &mut files);
    }
    let allowed = Path::new("crates").join("common").join("src").join("procs.rs");

    // Act
    let offenders: Vec<String> = files
        .iter()
        .filter(|f| !f.ends_with(&allowed))
        .filter(|f| std::fs::read_to_string(f).unwrap().contains("Command::new("))
        .map(|f| f.strip_prefix(&root).unwrap_or(f).display().to_string())
        .collect();

    // Assert
    assert!(files.len() > 10, "the scan found only {} files", files.len());
    assert!(offenders.is_empty(), "Command::new outside streamloop_common::procs: {offenders:?}");
}
