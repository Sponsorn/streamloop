//! Control mode keeps its std handles out of its children before anything else: the supervisor outlives
//! it, and holding a redirected stdout it would stop the launcher from starting control mode again.
#![cfg(windows)]

use std::os::windows::io::{AsRawHandle, RawHandle};
use std::process::Stdio;

use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};

const TEST: &str = "control_mode_keeps_its_std_handles_from_children_even_when_it_refuses_its_arguments";
const IN_CHILD: &str = "STREAMLOOP_STD_HANDLES_TEST_CHILD";

fn inheritable(handle: RawHandle) -> bool {
    let mut flags = 0u32;
    // SAFETY: a std handle of this process, and a flags word to write to.
    let ok = unsafe { GetHandleInformation(handle as _, &mut flags) } != 0;
    ok && flags & HANDLE_FLAG_INHERIT != 0
}

/// Runs in a child whose stdout and stderr are files handed down by the test, so both start inheritable.
fn in_child() -> ! {
    let before = inheritable(std::io::stdout().as_raw_handle()) && inheritable(std::io::stderr().as_raw_handle());
    let code = streamloop_control::app::run(vec!["--not-a-flag".into()]);
    let after = inheritable(std::io::stdout().as_raw_handle()) || inheritable(std::io::stderr().as_raw_handle());
    std::process::exit(match (before, code, after) {
        (true, 64, false) => 0,
        (false, _, _) => 10,
        (true, 64, true) => 11,
        _ => 12,
    });
}

#[test]
fn control_mode_keeps_its_std_handles_from_children_even_when_it_refuses_its_arguments() {
    if std::env::var_os(IN_CHILD).is_some() {
        in_child();
    }
    // Arrange
    let dir = tempfile::tempdir().unwrap();
    let out = std::fs::File::create(dir.path().join("stdout.txt")).unwrap();
    let err = std::fs::File::create(dir.path().join("stderr.txt")).unwrap();
    let mut child = streamloop_common::procs::command(std::env::current_exe().unwrap());
    child.args(["--exact", TEST, "--nocapture", "--test-threads=1"]).env(IN_CHILD, "1").stdout(Stdio::from(out)).stderr(Stdio::from(err));

    // Act
    let status = child.status().unwrap();

    // Assert
    let said = std::fs::read_to_string(dir.path().join("stderr.txt")).unwrap_or_default();
    assert_eq!(
        status.code(),
        Some(0),
        "10: the handles were not inheritable to begin with; 11: still inheritable after control mode refused its arguments; stderr: {said}"
    );
}
