use std::process::Command;
use streamloop_common::version::VERSION;

fn exe() -> Command {
    Command::new(env!("CARGO_BIN_EXE_streamloop"))
}

#[test]
fn version_flag_prints_the_workspace_version() {
    let out = exe().arg("--version").output().unwrap();

    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{VERSION}\n"));
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn supervisor_word_runs_the_supervisor() {
    let out = exe().args(["supervisor", "--bogus"]).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "the supervisor's bad-flag code");
}

#[test]
fn check_word_runs_the_install_check_with_the_prefix_node_strips() {
    // Arrange: a root with no config.json and no install files
    let root = std::env::temp_dir().join(format!("sl-cli-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // Act
    let out = exe().arg("check").arg("--root").arg(&root).output().unwrap();

    // Assert
    let _ = std::fs::remove_dir_all(&root);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("streamloop-supervisor check: "), "{stderr}");
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(1));
}
