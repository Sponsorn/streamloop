//! One exe: `supervisor`, `launch`, `protect` and `check` run the supervisor crate, `ui` runs the window; anything else is control mode.

mod ui_mode;

use streamloop_common::version::VERSION;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let rest = || argv[1..].to_vec();
    let code = match argv.first().map(String::as_str) {
        Some("supervisor") if cfg!(feature = "test-supervisor-fails") => broken("supervisor", "test-supervisor-fails"),
        Some("supervisor") => streamloop_supervisor::run(rest()),
        Some("launch") => streamloop_supervisor::launch_main(rest()),
        Some("protect") => streamloop_supervisor::protect_main(),
        Some("check") => streamloop_supervisor::check_main(rest()),
        Some("ui") => ui_mode::ui_main(rest()),
        Some("--version") => {
            println!("{VERSION}");
            0
        }
        _ if cfg!(feature = "test-control-fails") => broken("control", "test-control-fails"),
        _ => streamloop_control::app::run(argv),
    };
    std::process::exit(code);
}

/// A test build's mode that exits at start, so the harness can prove a rollback.
fn broken(mode: &str, feature: &str) -> i32 {
    eprintln!("streamloop.exe {VERSION}: {mode} mode exits at start in this test build ({feature})");
    1
}
