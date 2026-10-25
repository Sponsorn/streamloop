//! `streamloop-supervisor.exe launch ARGS`: start the supervisor through a short-lived
//! intermediate. The supervisor's parent is then a dead process, so a tree kill of the caller
//! (`taskkill /T`, "End process tree") cannot walk to it, and it leaves the caller's job when the
//! job allows breakaway.

use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
const ERROR_ACCESS_DENIED: i32 = 5;
const PORT_TAKEN_EXIT: i32 = 2;
const BIND_WAIT: Duration = Duration::from_secs(3);

/// CREATE_NO_WINDOW is left out: Windows ignores it beside DETACHED_PROCESS, which has no console at all.
pub fn flags(breakaway: bool) -> u32 {
    DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | if breakaway { CREATE_BREAKAWAY_FROM_JOB } else { 0 }
}

/// A job without breakaway-OK refuses CREATE_BREAKAWAY_FROM_JOB with access denied; the retry
/// stays in that job.
pub fn retry_without_breakaway(err: &std::io::Error, breakaway: bool) -> bool {
    breakaway && err.raw_os_error() == Some(ERROR_ACCESS_DENIED)
}

/// The intermediate's exit code: port taken is relayed so Node can attach, anything else is 0.
pub fn relay(code: Option<i32>) -> i32 {
    if code == Some(PORT_TAKEN_EXIT) {
        PORT_TAKEN_EXIT
    } else {
        0
    }
}

fn spawn(args: &[String], flags: u32) -> std::io::Result<Child> {
    let mut c = Command::new(std::env::current_exe()?);
    c.args(args).current_dir(std::env::temp_dir()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(flags);
    }
    #[cfg(not(windows))]
    let _ = flags;
    c.spawn()
}

fn answers(port: u16) -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).is_ok()
}

/// Spawn the supervisor with `args`, report (breakaway, pid), and wait until it binds `port`
/// or exits, for at most about 3 s.
pub fn run(args: &[String], port: u16, report: impl Fn(bool, u32)) -> i32 {
    // A port already held means the child will exit 2; only its exit settles it.
    let held_before = answers(port);
    let mut breakaway = true;
    let mut child = loop {
        match spawn(args, flags(breakaway)) {
            Ok(c) => break c,
            Err(e) if retry_without_breakaway(&e, breakaway) => breakaway = false,
            Err(e) => {
                crate::logs::echo(&mut std::io::stderr().lock(), &format!("streamloop-supervisor launch: {e}"));
                return 1;
            }
        }
    };
    report(breakaway, child.id());
    let until = Instant::now() + BIND_WAIT;
    while Instant::now() < until {
        if let Ok(Some(status)) = child.try_wait() {
            return relay(status.code());
        }
        if !held_before && answers(port) {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakaway_flag_is_added_only_when_asked() {
        assert_eq!(flags(true), 0x0100_0208);
        assert_eq!(flags(false), 0x0000_0208);
    }

    #[test]
    fn only_access_denied_on_a_breakaway_spawn_retries() {
        let denied = std::io::Error::from_raw_os_error(5);
        assert!(retry_without_breakaway(&denied, true));
        assert!(!retry_without_breakaway(&denied, false));
        assert!(!retry_without_breakaway(&std::io::Error::from_raw_os_error(2), true));
    }

    #[test]
    fn port_taken_is_relayed_and_everything_else_is_zero() {
        assert_eq!(relay(Some(2)), 2);
        assert_eq!(relay(Some(1)), 0);
        assert_eq!(relay(Some(0)), 0);
        assert_eq!(relay(None), 0);
    }
}
