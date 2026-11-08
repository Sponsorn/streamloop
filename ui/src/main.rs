#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const DEFAULT_PORT: u16 = 7654;
const USAGE: &str = "Usage: streamloop-ui.exe [--port N]";
/// Beside the exe, and written only on a panic or a bad argument.
const LOG: &str = "streamloop-ui.log";
const ROLL_PAST: u64 = 1024 * 1024;

fn main() {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let Some(port) = parse_port(std::env::args().skip(1)) else {
        if let Some(dir) = &dir {
            append_log(dir, USAGE);
        }
        std::process::exit(2);
    };
    if let Some(dir) = dir {
        // A browser opened for a link starts here, so it never holds a folder inside the install.
        let _ = std::env::set_current_dir(&dir);
        log_panics(dir);
    }
    streamloop_ui::run(port);
}

/// `--port N` or nothing; anything else is refused.
fn parse_port(args: impl IntoIterator<Item = String>) -> Option<u16> {
    let args: Vec<String> = args.into_iter().collect();
    match args.as_slice() {
        [] => Some(DEFAULT_PORT),
        [flag, port] if flag == "--port" => port.parse().ok().filter(|&p| p != 0),
        _ => None,
    }
}

fn log_panics(dir: PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        append_log(
            &dir,
            &format!(
                "{} {info}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
            ),
        );
        default_hook(info);
    }));
}

/// Appends `line` to the log in `dir`, first moving a log past 1 MB to `.old`.
fn append_log(dir: &Path, line: &str) {
    let path = dir.join(LOG);
    if fs::metadata(&path).is_ok_and(|m| m.len() > ROLL_PAST) {
        let _ = fs::rename(&path, dir.join(format!("{LOG}.old")));
    }
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn takes_7654_without_arguments_and_the_given_port() {
        assert_eq!(parse_port(args(&[])), Some(7654));
        assert_eq!(parse_port(args(&["--port", "8123"])), Some(8123));
    }

    #[test]
    fn refuses_anything_else() {
        for bad in [
            &["--port"][..],
            &["--port", "abc"],
            &["--port", "0"],
            &["--port", "70000"],
            &["--port", "8123", "--port", "8124"],
            &["--verbose"],
            &["8123"],
        ] {
            assert_eq!(parse_port(args(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn rolls_the_log_to_old_past_one_megabyte() {
        let dir =
            std::env::temp_dir().join(format!("streamloop-ui-log-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("streamloop-ui.log");
        let old = dir.join("streamloop-ui.log.old");

        append_log(&dir, "first");
        assert_eq!(fs::read_to_string(&log).unwrap(), "first\n");
        assert!(!old.exists());

        fs::write(&log, vec![b'x'; 1024 * 1024 + 1]).unwrap();
        append_log(&dir, "second");

        assert_eq!(fs::read_to_string(&log).unwrap(), "second\n");
        assert_eq!(fs::metadata(&old).unwrap().len(), 1024 * 1024 + 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
