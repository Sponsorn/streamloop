use std::process::Command;
use streamloop_common::version::VERSION;

fn exe() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_streamloop"));
    c.env_remove("STREAMLOOP_START");
    c
}

#[test]
fn version_prints_into_a_pipe() {
    // Act: the updater reads a staged exe's version this way
    let out = exe().arg("--version").output().unwrap();

    // Assert
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{VERSION}\n"));
    assert_eq!(out.status.code(), Some(0));
}

#[cfg(windows)]
#[test]
fn the_exe_is_a_windows_gui_program() {
    // Arrange
    const IMAGE_SUBSYSTEM_WINDOWS_GUI: u16 = 2;
    let image = std::fs::read(env!("CARGO_BIN_EXE_streamloop")).unwrap();
    let pe = u32::from_le_bytes(image[0x3c..0x40].try_into().unwrap()) as usize;

    // Act: the optional header follows the 4-byte signature and the 20-byte file header; Subsystem is at 68
    let subsystem = u16::from_le_bytes(image[pe + 24 + 68..pe + 24 + 70].try_into().unwrap());

    // Assert
    assert_eq!(&image[pe..pe + 4], b"PE\0\0");
    assert_eq!(subsystem, IMAGE_SUBSYSTEM_WINDOWS_GUI, "a console program opens a console on a double-click");
}

#[test]
fn control_usage_names_the_control_mode() {
    // Act
    let out = exe().args(["control", "--bogus"]).output().unwrap();

    // Assert
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("streamloop.exe control"), "{stderr}");
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn a_plain_exe_under_start_bat_is_the_control_process() {
    // Act: every START.bat since 3.1.0 sets STREAMLOOP_START and runs streamloop.exe with no mode word
    let out = exe().arg("--bogus").env("STREAMLOOP_START", "first").output().unwrap();

    // Assert
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("streamloop.exe control"), "{stderr}");
    assert_eq!(out.status.code(), Some(64));
}

#[test]
fn control_output_reaches_a_file_it_was_redirected_to() {
    // Arrange: a log file, as a launcher that runs `streamloop.exe >> log 2>&1` hands it over
    let dir = std::env::temp_dir().join(format!("sl-cli-redirect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("console.log");
    let file = std::fs::File::create(&log).unwrap();

    // Act
    let status = exe().args(["control", "--bogus"]).stdout(file.try_clone().unwrap()).stderr(file).status().unwrap();

    // Assert
    let text = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(text.contains("streamloop.exe control"), "{text}");
    assert_eq!(status.code(), Some(64));
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
    let mut child = exe().arg("control").arg("--root").arg(&root).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();

    // Act: the reader goes before the first line is written
    drop(child.stdout.take());
    let status = child.wait().unwrap();

    // Assert
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(status.code(), Some(1), "a panic in the log layer exits 101");
}

#[cfg(windows)]
fn cmds_naming(needle: &str) -> usize {
    let filter = format!("(Get-CimInstance Win32_Process -Filter \"Name='cmd.exe'\" | Where-Object {{ $_.CommandLine -like '*{needle}*' }} | Measure-Object).Count");
    let out = Command::new("powershell").args(["-NoProfile", "-Command", &filter]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

#[cfg(windows)]
#[test]
fn detach_start_bat_runs_the_batch_with_from_app_set() {
    use std::time::{Duration, Instant};
    // Arrange: a root with a space, an umlaut and a parenthesis, whose START.bat reports its variables and pauses
    let root = std::env::temp_dir().join(format!("sl4 Ö (x) {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("app")).unwrap();
    let seen = root.join("seen.txt");
    let bat = "@echo off\r\n(echo from-app=%STREAMLOOP_FROM_APP%& echo start=%STREAMLOOP_START%) > \"%~dp0seen.txt\"\r\npause\r\n";
    std::fs::write(root.join("START.bat"), bat).unwrap();
    let needle = root.file_name().unwrap().to_string_lossy().into_owned();

    // Act: the parent has STREAMLOOP_START set too, which detach must drop
    let status = exe().args(["detach", "start-bat", "--root"]).arg(&root).env("STREAMLOOP_START", "first").status().unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !seen.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(500));
    let text = std::fs::read_to_string(&seen).unwrap_or_default();
    let mut left = cmds_naming(&needle);
    while left > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(200));
        left = cmds_naming(&needle);
    }

    // Assert
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(status.code(), Some(0));
    assert_eq!(text.lines().collect::<Vec<_>>(), ["from-app=1", "start="], "{text:?}");
    assert_eq!(left, 0, "a cmd for the batch is still running");
}

#[cfg(windows)]
#[test]
fn detach_start_bat_runs_a_relative_root_from_the_folder_it_was_started_in() {
    use std::time::{Duration, Instant};
    // Arrange: `rel\START.bat` under a folder that is not the temp folder, where cmd itself starts
    let base = std::env::temp_dir().join(format!("sl-cli-detach-rel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("rel")).unwrap();
    let seen = base.join("rel").join("seen.txt");
    std::fs::write(base.join("rel").join("START.bat"), "@echo off\r\n(echo ran) > \"%~dp0seen.txt\"\r\n").unwrap();

    // Act
    let status = exe().current_dir(&base).args(["detach", "start-bat", "--root", "rel"]).status().unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !seen.exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_millis(300));
    let text = std::fs::read_to_string(&seen).unwrap_or_default();

    // Assert
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(status.code(), Some(0));
    assert_eq!(text.trim(), "ran");
}

#[test]
fn detach_start_bat_without_a_batch_file_exits_1() {
    // Arrange
    let root = std::env::temp_dir().join(format!("sl-cli-detach-nobat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    // Act
    let out = exe().args(["detach", "start-bat", "--root"]).arg(&root).output().unwrap();

    // Assert
    let _ = std::fs::remove_dir_all(&root);
    assert!(String::from_utf8_lossy(&out.stderr).contains("START.bat"));
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn detach_with_bad_arguments_says_the_usage() {
    // Act
    let out = exe().args(["detach", "bogus"]).output().unwrap();

    // Assert
    assert!(String::from_utf8_lossy(&out.stderr).contains("streamloop.exe detach"));
    assert_eq!(out.status.code(), Some(64));
}
