//! `streamloop.exe ui`: the window. Argument parsing, the working folder and the panic log live here; the window is `streamloop_ui`.

use std::path::{Path, PathBuf};

use streamloop_common::paths::{install_root, Layout};
use streamloop_ui::{ui_log, UiOptions};

const USAGE: &str = "Usage: streamloop.exe ui [--root DIR] [--port N] [--tray] [--follow V]";
const EXIT_USAGE: i32 = 64;

#[derive(Debug, PartialEq)]
pub struct UiArgs {
    pub root: Option<PathBuf>,
    pub port: Option<u16>,
    pub tray: bool,
    pub follow: Option<String>,
}

/// `--root DIR`, `--port N` (N above 0), `--tray` and `--follow V`, in any order; anything else is the usage line.
pub fn parse_ui_args(args: &[String]) -> Result<UiArgs, String> {
    let mut parsed = UiArgs { root: None, port: None, tray: false, follow: None };
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().ok_or_else(|| USAGE.to_string());
        match flag.as_str() {
            "--root" => parsed.root = Some(PathBuf::from(value()?)),
            "--port" => parsed.port = Some(value()?.parse().ok().filter(|&p| p != 0).ok_or_else(|| USAGE.to_string())?),
            "--tray" => parsed.tray = true,
            "--follow" => parsed.follow = Some(value()?.clone()),
            _ => return Err(USAGE.into()),
        }
    }
    Ok(parsed)
}

/// Runs the window: 0 when it closes, 64 for bad arguments.
pub fn ui_main(args: Vec<String>) -> i32 {
    let exe_dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf));
    let parsed = parse_ui_args(&args);
    let flag = parsed.as_ref().ok().and_then(|a| a.root.clone());
    let root = exe_dir.as_deref().and_then(|dir| {
        let env_root = std::env::var_os("STREAMLOOP_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from);
        install_root(flag.as_deref(), dir, env_root.as_deref()).ok()
    });
    // Before the working folder moves: the lock names, the START.bat check and `detach` all take this path.
    let root = root.map(|r| full_path(r.dir));
    let log_dir = match (&root, &exe_dir) {
        (Some(root), _) => Some(Layout::new(root).app_logs),
        (None, Some(dir)) => Some(dir.clone()),
        (None, None) => None,
    };
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(usage) => {
            if let Some(dir) = &log_dir {
                ui_log::append(dir, &usage);
            }
            return EXIT_USAGE;
        }
    };
    if let Some(dir) = exe_dir {
        // A browser opened for a link starts here, so it never holds a folder inside the install.
        let _ = std::env::set_current_dir(&dir);
    }
    if let Some(dir) = log_dir.clone() {
        log_panics(dir);
    }
    streamloop_ui::run(UiOptions {
        root,
        port: parsed.port,
        tray: parsed.tray,
        follow: parsed.follow,
        log_dir,
    });
    0
}

/// `dir` from the current folder, with no trailing separator, so `C:\SL\` and `C:\SL` name one install.
fn full_path(dir: PathBuf) -> PathBuf {
    std::path::absolute(&dir).unwrap_or(dir).components().collect()
}

fn log_panics(dir: PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ui_log::append(&dir, &ui_log::stamped(&info.to_string()));
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn takes_no_arguments() {
        // Arrange
        let none = args(&[]);

        // Act
        let parsed = parse_ui_args(&none).unwrap();

        // Assert
        assert_eq!(parsed, UiArgs { root: None, port: None, tray: false, follow: None });
    }

    #[test]
    fn takes_root_port_tray_and_follow_in_any_order() {
        // Arrange
        let one = args(&["--root", "C:\\SL", "--port", "8123", "--tray", "--follow", "3.2.0"]);
        let other = args(&["--follow", "3.2.0", "--tray", "--port", "8123", "--root", "C:\\SL"]);

        // Act
        let (a, b) = (parse_ui_args(&one).unwrap(), parse_ui_args(&other).unwrap());

        // Assert
        let expected = UiArgs {
            root: Some(PathBuf::from("C:\\SL")),
            port: Some(8123),
            tray: true,
            follow: Some("3.2.0".into()),
        };
        assert_eq!(a, expected);
        assert_eq!(b, expected);
    }

    #[test]
    fn refuses_port_zero_and_unknown_flags() {
        for bad in [
            &["--port", "0"][..],
            &["--port", "abc"],
            &["--port", "70000"],
            &["--verbose"],
            &["8123"],
        ] {
            // Act
            let parsed = parse_ui_args(&args(bad));

            // Assert
            assert!(parsed.is_err(), "{bad:?}");
        }
    }

    #[test]
    fn refuses_a_flag_without_its_value() {
        for bad in [&["--root"][..], &["--port"], &["--follow"], &["--tray", "--port"]] {
            // Act
            let parsed = parse_ui_args(&args(bad));

            // Assert
            assert!(parsed.is_err(), "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_root_is_made_full_without_a_trailing_separator() {
        // Arrange
        let cwd = std::env::current_dir().unwrap();

        // Act
        let relative = full_path(PathBuf::from(r"dev\root"));
        let trailing = full_path(PathBuf::from(r"C:\SL\"));

        // Assert
        assert_eq!(relative, cwd.join("dev").join("root"));
        assert_eq!(trailing.as_os_str(), r"C:\SL");
    }
}
