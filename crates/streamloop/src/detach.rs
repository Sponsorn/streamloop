//! `detach ui ARGS` and `detach start-bat [--root DIR]`: start the app or `START.bat` through a short-lived
//! intermediate and exit, so what it started has no living parent and sits outside the caller's job.
//! `launch` does the same for the supervisor.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use streamloop_common::paths::install_root;
use streamloop_common::procs;

const USAGE: &str = "Usage: streamloop.exe detach ui [ARGS] | streamloop.exe detach start-bat [--root DIR]";
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 64;

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

#[derive(Debug, PartialEq)]
pub enum DetachTarget {
    Ui(Vec<String>),
    StartBat { root: Option<PathBuf> },
}

/// `ui ARGS...` (the arguments are the app's, checked by it) or `start-bat [--root DIR]`.
pub fn parse_detach(args: &[String]) -> Result<DetachTarget, String> {
    match args.first().map(String::as_str) {
        Some("ui") => Ok(DetachTarget::Ui(args[1..].to_vec())),
        Some("start-bat") => match &args[1..] {
            [] => Ok(DetachTarget::StartBat { root: None }),
            [flag, dir] if flag == "--root" => Ok(DetachTarget::StartBat { root: Some(PathBuf::from(dir)) }),
            _ => Err(USAGE.into()),
        },
        _ => Err(USAGE.into()),
    }
}

/// Ui: DETACHED_PROCESS, since the app is a GUI program and needs no console. StartBat: a hidden console
/// that cmd's children share; DETACHED_PROCESS there would make a console child of cmd open its own window.
/// CREATE_NO_WINDOW stays off Ui because Windows ignores it beside DETACHED_PROCESS.
pub fn flags(target: &DetachTarget, breakaway: bool) -> u32 {
    let console = match target {
        DetachTarget::Ui(_) => DETACHED_PROCESS,
        DetachTarget::StartBat { .. } => CREATE_NO_WINDOW,
    };
    console | CREATE_NEW_PROCESS_GROUP | if breakaway { CREATE_BREAKAWAY_FROM_JOB } else { 0 }
}

/// What `detach` starts, with the creation flags for a first try with breakaway. `exe` is this exe, `root`
/// the install root (used by StartBat only).
pub fn detach_command(target: &DetachTarget, exe: &Path, root: &Path) -> Command {
    let mut c = match target {
        DetachTarget::Ui(args) => {
            let mut c = procs::command(exe);
            c.args(std::iter::once("ui").chain(args.iter().map(String::as_str)));
            c.env_remove("STREAMLOOP_START").env_remove("STREAMLOOP_FROM_APP");
            c
        }
        DetachTarget::StartBat { .. } => {
            let mut c = procs::command(system32().join("cmd.exe"));
            // With /s cmd strips the outer quote pair, so a path with spaces or parentheses survives.
            raw_arg(&mut c, &format!("/d /s /c \"\"{}\"\"", root.join("START.bat").display()));
            c.env_remove("STREAMLOOP_START").env("STREAMLOOP_FROM_APP", "1");
            c
        }
    };
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    set_flags(&mut c, flags(target, true));
    c
}

fn system32() -> PathBuf {
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    system.join("System32")
}

#[cfg(windows)]
fn raw_arg(c: &mut Command, line: &str) {
    use std::os::windows::process::CommandExt;
    c.raw_arg(line);
}

#[cfg(not(windows))]
fn raw_arg(c: &mut Command, line: &str) {
    c.arg(line);
}

#[cfg(windows)]
fn set_flags(c: &mut Command, flags: u32) {
    use std::os::windows::process::CommandExt;
    c.creation_flags(flags);
}

#[cfg(not(windows))]
fn set_flags(_c: &mut Command, _flags: u32) {}

/// A job without breakaway-OK refuses CREATE_BREAKAWAY_FROM_JOB with access denied; the retry stays in it.
fn spawn(target: &DetachTarget, exe: &Path, root: &Path) -> std::io::Result<u32> {
    let mut breakaway = true;
    loop {
        let mut c = detach_command(target, exe, root);
        set_flags(&mut c, flags(target, breakaway));
        match c.spawn() {
            Ok(child) => return Ok(child.id()),
            Err(e) if streamloop_supervisor::retry_without_breakaway(&e, breakaway) => breakaway = false,
            Err(e) => return Err(e),
        }
    }
}

/// 0 once it is started, 1 when the start failed or there is no root (or no START.bat), 64 for bad arguments.
pub fn detach_main(args: Vec<String>) -> i32 {
    let target = match parse_detach(&args) {
        Ok(t) => t,
        Err(usage) => {
            eprintln!("{usage}");
            return EXIT_USAGE;
        }
    };
    let Ok(exe) = std::env::current_exe() else {
        eprintln!("streamloop detach: cannot tell where this exe is");
        return EXIT_FAILED;
    };
    let root = match &target {
        DetachTarget::Ui(_) => PathBuf::new(),
        DetachTarget::StartBat { root } => {
            let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
            let env_root = std::env::var_os("STREAMLOOP_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from);
            let root = match install_root(root.as_deref(), &exe_dir, env_root.as_deref()) {
                // cmd starts in the temp folder, so a relative root would name a START.bat there.
                Ok(r) => std::path::absolute(&r.dir).unwrap_or(r.dir),
                Err(why) => {
                    eprintln!("{why}");
                    return EXIT_FAILED;
                }
            };
            if !root.join("START.bat").is_file() {
                eprintln!("streamloop detach: {} does not exist", root.join("START.bat").display());
                return EXIT_FAILED;
            }
            root
        }
    };
    match spawn(&target, &exe, &root) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("streamloop detach: {e}");
            EXIT_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    fn env_of(c: &Command, key: &str) -> Option<Option<String>> {
        c.get_envs().find(|(k, _)| *k == key).map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    #[test]
    fn ui_target_drops_the_start_variables() {
        // Arrange
        let target = DetachTarget::Ui(args(&["--tray", "--port", "9"]));

        // Act
        let c = detach_command(&target, Path::new("C:\\x\\streamloop.exe"), Path::new("R"));

        // Assert
        assert_eq!(c.get_program(), "C:\\x\\streamloop.exe");
        assert_eq!(c.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>(), ["ui", "--tray", "--port", "9"]);
        assert_eq!(env_of(&c, "STREAMLOOP_START"), Some(None));
        assert_eq!(env_of(&c, "STREAMLOOP_FROM_APP"), Some(None));
    }

    #[test]
    fn start_bat_target_sets_from_app_and_drops_start() {
        // Arrange
        let target = DetachTarget::StartBat { root: None };

        // Act
        let c = detach_command(&target, Path::new("streamloop.exe"), Path::new("R"));

        // Assert
        assert_eq!(env_of(&c, "STREAMLOOP_FROM_APP"), Some(Some("1".into())));
        assert_eq!(env_of(&c, "STREAMLOOP_START"), Some(None));
        assert!(c.get_program().to_string_lossy().to_lowercase().ends_with("system32\\cmd.exe"), "{:?}", c.get_program());
    }

    #[test]
    fn start_bat_line_quotes_a_root_with_a_space_an_umlaut_and_a_parenthesis() {
        // Arrange
        let root = Path::new("C:\\Temp\\sl4 Ö (x)");

        // Act
        let c = detach_command(&DetachTarget::StartBat { root: None }, Path::new("streamloop.exe"), root);

        // Assert
        let line: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(line, ["/d /s /c \"\"C:\\Temp\\sl4 Ö (x)\\START.bat\"\""]);
    }

    #[test]
    fn start_bat_gets_a_hidden_console_never_a_detached_one() {
        // Arrange
        let target = DetachTarget::StartBat { root: None };

        // Act
        let with = flags(&target, true);
        let without = flags(&target, false);

        // Assert
        assert_eq!(with, CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        assert_eq!(without, CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
        assert_eq!(with & DETACHED_PROCESS, 0);
    }

    #[test]
    fn ui_gets_no_console_and_breakaway_only_when_asked() {
        // Arrange
        let target = DetachTarget::Ui(vec![]);

        // Act and Assert
        assert_eq!(flags(&target, true), DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        assert_eq!(flags(&target, false), DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    #[test]
    fn arguments_pick_the_target_or_say_the_usage() {
        // Act and Assert
        assert_eq!(parse_detach(&args(&["ui", "--tray"])), Ok(DetachTarget::Ui(args(&["--tray"]))));
        assert_eq!(parse_detach(&args(&["start-bat"])), Ok(DetachTarget::StartBat { root: None }));
        assert_eq!(parse_detach(&args(&["start-bat", "--root", "D"])), Ok(DetachTarget::StartBat { root: Some("D".into()) }));
        assert!(parse_detach(&args(&["start-bat", "--root"])).is_err());
        assert!(parse_detach(&args(&["start-bat", "x"])).is_err());
        assert!(parse_detach(&args(&[])).is_err());
        assert!(parse_detach(&args(&["bogus"])).is_err());
    }
}
