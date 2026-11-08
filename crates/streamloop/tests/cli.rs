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
