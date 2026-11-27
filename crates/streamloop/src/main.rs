//! One exe. The first argument names the mode (`dispatch::pick`): `supervisor`, `launch`, `protect` and
//! `check` run the supervisor crate, `control` the control process, `ui` the app. Without a mode word it is
//! the control process under START.bat and the app otherwise.
//!
//! A Windows GUI program, so a double-click opens no console; the console modes attach to their parent's.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod dispatch;
mod ui_mode;

use dispatch::Mode;
use streamloop_common::version::VERSION;

/// `detach` is reserved for the intermediate that starts the app and START.bat.
const DETACH_USAGE: &str = "Usage: streamloop.exe detach ui [ARGS] | streamloop.exe detach start-bat [--root DIR]";

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (mode, rest) = dispatch::pick(&argv, dispatch::start_set());
    // First, so the control process's Ctrl+C and console-close listeners and the handles it keeps from
    // its children are the attached console's.
    if mode.attaches_to_console() {
        streamloop_common::console::attach_parent();
    }
    let code = match mode {
        Mode::Supervisor if cfg!(feature = "test-supervisor-fails") => broken("supervisor", "test-supervisor-fails"),
        Mode::Supervisor => streamloop_supervisor::run(rest),
        Mode::Launch => streamloop_supervisor::launch_main(rest),
        Mode::Protect => streamloop_supervisor::protect_main(),
        Mode::Check => streamloop_supervisor::check_main(rest),
        Mode::Ui => ui_mode::ui_main(rest),
        Mode::Detach => {
            eprintln!("{DETACH_USAGE}");
            streamloop_control::app::EXIT_USAGE
        }
        Mode::Version => {
            println!("{VERSION}");
            0
        }
        Mode::Control if cfg!(feature = "test-control-fails") => broken("control", "test-control-fails"),
        Mode::Control => streamloop_control::app::run(rest),
    };
    std::process::exit(code);
}

/// A test build's mode that exits at start, so the harness can prove a rollback.
fn broken(mode: &str, feature: &str) -> i32 {
    eprintln!("streamloop.exe {VERSION}: {mode} mode exits at start in this test build ({feature})");
    1
}
