//! Guards over the release build: the layout it ships against the paths the code resolves.

use std::path::{Path, PathBuf};

use streamloop_common::paths::Layout;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// The quoted paths inside `FULL_LAYOUT = [ ... ];` of build/release-layout.js.
fn full_layout() -> Vec<String> {
    let js = read("build/release-layout.js");
    let start = js.find("FULL_LAYOUT = [").expect("FULL_LAYOUT in release-layout.js");
    let body = &js[start..];
    let body = &body[..body.find("];").expect("end of FULL_LAYOUT")];
    body.split('\'').skip(1).step_by(2).map(str::to_string).collect()
}

/// A path is covered when the layout names it, a folder above it, or a file inside it.
fn covered(layout: &[String], rel: &str) -> bool {
    layout.iter().any(|p| p == rel || rel.starts_with(&format!("{p}/")) || p.starts_with(&format!("{rel}/")))
}

#[test]
fn layout_covers_every_path_the_layout_struct_names() {
    // Arrange
    let root = Path::new("ROOT");
    let l = Layout::new(root);
    let named = [
        &l.ffmpeg, &l.ytdlp, &l.deno, &l.plugins, &l.token_server, &l.node, &l.slate, &l.data, &l.exe,
        &l.app_dir, &l.config_example, &l.stopped_marker, &l.rolled_back, &l.setup_report,
        &l.waiting_for_start, &l.update_state,
    ];
    let layout = full_layout();

    // Act
    let missing: Vec<String> = named
        .iter()
        .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
        .filter(|rel| !covered(&layout, rel))
        .collect();

    // Assert
    assert!(layout.len() > 8, "parsed only {layout:?}");
    assert!(missing.is_empty(), "paths in Layout but not in FULL_LAYOUT: {missing:?}");
}

#[test]
fn layout_ships_no_config_json_but_the_example() {
    // Arrange
    let layout = full_layout();

    // Assert
    assert!(!layout.iter().any(|p| p == "app/config.json"), "{layout:?}");
    assert!(layout.iter().any(|p| p == "app/config.example.json"));
    assert!(layout.iter().any(|p| p == "streamloop.exe"));
}

#[test]
fn there_is_no_update_layout() {
    // Arrange
    let layout = read("build/release-layout.js");
    let verify = read("build/verify-release.mjs");
    let prepare = read("build/prepare-release.js");
    let package = read("package.json");

    // Assert
    assert!(!layout.contains("UPDATE_LAYOUT") && !verify.contains("UPDATE_LAYOUT"));
    assert!(!prepare.contains("--slim") && !package.contains("build:release:slim"));
    assert!(!prepare.contains("components.json"));
    assert!(!full_layout().iter().any(|p| p.starts_with("app/src") || p.starts_with("app/node_modules")));
}

#[test]
fn cargo_and_package_json_versions_match() {
    // Arrange
    let package: serde_json::Value = serde_json::from_str(&read("package.json")).unwrap();

    // Assert
    assert_eq!(package["version"].as_str().unwrap(), env!("CARGO_PKG_VERSION"));
}
