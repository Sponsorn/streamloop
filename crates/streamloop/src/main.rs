//! One exe, three jobs: `supervisor`, `launch` and `protect` run the supervisor crate; anything else is control mode.

use streamloop_common::version::VERSION;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let rest = || argv[1..].to_vec();
    let code = match argv.first().map(String::as_str) {
        Some("supervisor") => streamloop_supervisor::run(rest()),
        Some("launch") => streamloop_supervisor::launch_main(rest()),
        Some("protect") => streamloop_supervisor::protect_main(),
        Some("--version") => {
            println!("{VERSION}");
            0
        }
        _ => streamloop_control::app::run(argv),
    };
    std::process::exit(code);
}
