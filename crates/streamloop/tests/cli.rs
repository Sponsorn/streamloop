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

#[test]
fn a_log_line_to_a_closed_stdout_does_not_crash_the_control_process() {
    // Arrange: a root whose config.json does not load, so the control process logs why and exits 1
    let root = std::env::temp_dir().join(format!("sl-cli-closed-stdout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("app").join("config.json"), "{").unwrap();
    let mut child = exe().arg("--root").arg(&root).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();

    // Act: the reader goes before the first line is written
    drop(child.stdout.take());
    let status = child.wait().unwrap();

    // Assert
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(status.code(), Some(1), "a panic in the log layer exits 101");
}
