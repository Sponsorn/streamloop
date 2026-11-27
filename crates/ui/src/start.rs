//! The app's start: whether it starts `START.bat`, and what Connecting says until StreamLoop answers.

use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use streamloop_common::{paths::Layout, procs};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub const STARTING: &str = "Starting StreamLoop...";
pub const RETRYING: &str = "Can't reach StreamLoop. Retrying...";
/// How long after the app started `START.bat` with no answer Connecting adds the hint.
pub const HINT_AFTER: Duration = Duration::from_secs(30);

pub fn should_start_bat(root: Option<&Path>, answered: bool, bat_running: bool, bat_exists: bool) -> bool {
    root.is_some() && !answered && !bat_running && bat_exists
}

/// Starts `START.bat` hidden when nothing answered and no `START.bat` of this install runs, one waiting
/// out its 10 s after a crash included; true when it started.
pub fn start_if_needed(root: Option<&Path>, answered: bool) -> bool {
    let Some(dir) = root.filter(|_| !answered) else {
        return false;
    };
    let exists = dir.join("START.bat").is_file();
    should_start_bat(root, answered, start_bat_running(dir), exists) && start_start_bat(dir)
}

/// `ROOT\streamloop.exe detach start-bat --root ROOT`, its output on NUL.
pub fn start_bat_command(root: &Path) -> Command {
    let mut c = procs::command(Layout::new(root).exe);
    c.args(["detach", "start-bat", "--root"])
        .arg(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// Runs `detach start-bat`, which exits once `START.bat` runs; a thread of its own reaps it.
fn start_start_bat(root: &Path) -> bool {
    match start_bat_command(root).spawn() {
        Ok(mut detach) => {
            std::thread::spawn(move || detach.wait());
            true
        }
        Err(_) => false,
    }
}

/// A cmd.exe whose command line names ROOT\START.bat, compared case-insensitively.
pub fn start_bat_running(root: &Path) -> bool {
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new());
    let cmds: Vec<_> = system
        .processes()
        .iter()
        .filter(|(_, p)| p.name().eq_ignore_ascii_case("cmd.exe"))
        .map(|(pid, _)| *pid)
        .collect();
    if cmds.is_empty() {
        return false;
    }
    let with_command_line = ProcessRefreshKind::new().with_cmd(UpdateKind::Always);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&cmds), with_command_line);
    cmds.iter()
        .filter_map(|pid| system.process(*pid))
        .any(|p| names_start_bat(p.cmd(), root))
}

/// sysinfo splits command lines as `CommandLineToArgvW` does, cutting cmd's `/c ""PATH""` at its spaces,
/// so both sides lose their quotes and fold whitespace; the batch's name must end an argument.
fn names_start_bat<S: AsRef<OsStr>>(args: &[S], root: &Path) -> bool {
    let fold = |s: &str| {
        s.replace('"', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let line: Vec<String> = args.iter().map(|a| a.as_ref().to_string_lossy().into_owned()).collect();
    let line = fold(&line.join(" "));
    let bat = fold(&root.join("START.bat").to_string_lossy());
    line.match_indices(&bat)
        .any(|(at, _)| line[at + bat.len()..].chars().next().is_none_or(|c| c == ' '))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Connecting {
    /// This app started `START.bat` and nothing has answered yet.
    Starting { hint: Option<String> },
    Retrying,
}

impl Connecting {
    pub fn title(&self) -> &'static str {
        match self {
            Connecting::Starting { .. } => STARTING,
            Connecting::Retrying => RETRYING,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        match self {
            Connecting::Starting { hint } => hint.as_deref(),
            Connecting::Retrying => None,
        }
    }
}

/// `since_start` counts from the app's start of `START.bat`.
pub fn connecting(started_bat: bool, since_start: Duration, root: Option<&Path>) -> Connecting {
    if !started_bat {
        return Connecting::Retrying;
    }
    let hint = root.filter(|_| since_start >= HINT_AFTER).map(|root| {
        format!(
            "StreamLoop did not start. Double-click START.bat in {} to see why.",
            root.display()
        )
    });
    Connecting::Starting { hint }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_start_bat_only_when_nothing_answers_and_none_runs() {
        // Arrange
        let root = Some(Path::new(r"C:\SL"));
        let mut cases = vec![];
        for answered in [false, true] {
            for bat_running in [false, true] {
                for bat_exists in [false, true] {
                    cases.push((answered, bat_running, bat_exists));
                }
            }
        }

        // Act
        let starts: Vec<_> = cases
            .iter()
            .filter(|&&(answered, running, exists)| should_start_bat(root, answered, running, exists))
            .collect();

        // Assert
        assert_eq!(starts, [&(false, false, true)]);
    }

    #[test]
    fn never_starts_start_bat_without_a_root() {
        for answered in [false, true] {
            for bat_running in [false, true] {
                for bat_exists in [false, true] {
                    // Act
                    let start = should_start_bat(None, answered, bat_running, bat_exists);

                    // Assert
                    assert!(!start, "{answered} {bat_running} {bat_exists}");
                }
            }
        }
    }

    #[test]
    fn starts_start_bat_through_the_installs_own_exe() {
        // Arrange
        let root = Path::new(r"C:\Temp\sl4 Ö (x)");

        // Act
        let c = start_bat_command(root);

        // Assert
        assert_eq!(c.get_program(), r"C:\Temp\sl4 Ö (x)\streamloop.exe");
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["detach", "start-bat", "--root", r"C:\Temp\sl4 Ö (x)"]);
        assert_eq!(c.get_current_dir(), Some(std::env::temp_dir().as_path()));
    }

    #[test]
    fn knows_a_start_bat_command_line_however_cmd_splits_it() {
        // Arrange
        let root = Path::new(r"C:\Temp\sl4 Ö (x)");
        let detach = [r"C:\WINDOWS\system32\cmd.exe", "/d", "/s", "/c", r"C:\Temp\sl4", "Ö", r"(x)\START.bat"];
        let explorer = [r"C:\WINDOWS\system32\cmd.exe", "/c", r"C:\Temp\sl4 Ö (x)\START.bat", " "];
        let quoted = ["cmd.exe", "/c", r#""c:\temp\SL4 ö (X)\start.BAT""#];
        let other_install = ["cmd.exe", "/c", r"C:\Temp\sl4 Ö (y)\START.bat"];
        let other_file = ["cmd.exe", "/c", r"C:\Temp\sl4 Ö (x)\START.bat.new"];
        let a_prompt = [r"C:\WINDOWS\system32\cmd.exe"];

        // Act
        let seen = [&detach[..], &explorer, &quoted, &other_install, &other_file, &a_prompt]
            .map(|args| names_start_bat(args, root));

        // Assert
        assert_eq!(seen, [true, true, true, false, false, false]);
    }

    #[cfg(windows)]
    #[test]
    fn sees_a_cmd_running_this_installs_start_bat() {
        use std::os::windows::process::CommandExt;
        // Arrange: cmd started as `detach start-bat` starts it, on a batch that waits
        let root = std::env::temp_dir().join(format!("sl4 Ö (x) ui-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("START.bat"), "@ping -n 30 127.0.0.1 >nul\r\n").unwrap();
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let mut cmd = streamloop_common::procs::command(system.join("cmd.exe"));
        cmd.raw_arg(format!("/d /s /c \"\"{}\"\"", root.join("START.bat").display()))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let before = start_bat_running(&root);
        let mut child = cmd.spawn().unwrap();
        let upper = std::path::PathBuf::from(root.to_string_lossy().to_uppercase());

        // Act
        let running = start_bat_running(&root);
        let running_upper = start_bat_running(&upper);
        let elsewhere = start_bat_running(&root.with_file_name("sl4 elsewhere"));
        streamloop_common::procs::kill_tree(child.id());
        let _ = child.wait();

        // Assert
        let _ = std::fs::remove_dir_all(&root);
        assert!(!before);
        assert!(running);
        assert!(running_upper);
        assert!(!elsewhere);
    }

    #[test]
    fn connecting_adds_the_hint_after_30_s() {
        // Arrange
        let root = Path::new(r"C:\Temp\sl4 Ö (x)");

        // Act
        let at_29 = connecting(true, Duration::from_secs(29), Some(root));
        let at_30 = connecting(true, Duration::from_secs(30), Some(root));

        // Assert
        assert_eq!(at_29, Connecting::Starting { hint: None });
        assert_eq!(
            at_30,
            Connecting::Starting {
                hint: Some(r"StreamLoop did not start. Double-click START.bat in C:\Temp\sl4 Ö (x) to see why.".into())
            }
        );
    }

    #[test]
    fn connecting_retries_when_this_app_did_not_start_start_bat() {
        // Act
        let state = connecting(false, Duration::from_secs(60), Some(Path::new(r"C:\SL")));

        // Assert
        assert_eq!(state, Connecting::Retrying);
    }
}
