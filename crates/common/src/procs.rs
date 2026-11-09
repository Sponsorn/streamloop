//! The one place a child process is built. Nothing else calls `Command::new` (`repo_guards.rs`).
//! Also the process-tree and port lookups the supervisor and the control process share.

use std::process::{Command, Stdio};

/// Children start outside the install folder (2.x spawn-cwd rule).
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.current_dir(std::env::temp_dir()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// (pid, parent pid) of every process, from one snapshot; empty when the snapshot fails.
#[cfg(windows)]
fn processes() -> Vec<(u32, u32)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut pairs = Vec::new();
    // SAFETY: the snapshot handle is checked and closed, and the entry is zeroed with its size set as the API requires.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return pairs;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut entry) != 0;
        while more {
            pairs.push((entry.th32ProcessID, entry.th32ParentProcessID));
            more = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
    }
    pairs
}

/// `root` and its descendants, by parent pid. A parent pid can be reused, which is fine for a live
/// process's own children; an `ffmpeg` on PATH can be a shim whose child is the real one.
#[cfg(windows)]
pub fn tree(root: u32) -> Vec<u32> {
    let pairs = processes();
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i];
        let children: Vec<u32> = pairs.iter().filter(|(c, p)| *p == parent && *c != parent && !out.contains(c)).map(|(c, _)| *c).collect();
        out.extend(children);
        i += 1;
    }
    out
}

/// Ends a process and every process under it.
#[cfg(windows)]
pub fn kill_tree(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    for p in tree(pid) {
        // SAFETY: the handle is checked before use and closed after it.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, p);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn kill_tree(pid: u32) {
    let _ = command("kill").args(["-9", &pid.to_string()]).status();
}

/// The pid listening on 127.0.0.1:`port` or on every address, from the TCP table.
#[cfg(windows)]
pub fn pid_on_port(port: u16) -> Option<u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER};
    const AF_INET: u32 = 2;
    const NO_ERROR: u32 = 0;
    const ROW_WORDS: usize = std::mem::size_of::<MIB_TCPROW_OWNER_PID>() / 4;
    let loopback = u32::from_ne_bytes([127, 0, 0, 1]);
    let mut size = 0u32;
    // The first call reports the size; the table can grow before the next, which then reports it again.
    for _ in 0..4 {
        let mut buf = vec![0u32; (size as usize).div_ceil(4).max(1)];
        // SAFETY: `buf` holds at least `size` bytes, aligned for the table's u32 fields; the call writes
        // no more than `size` bytes and sets `size` to what it needs when that is too small.
        let rc = unsafe { GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0) };
        if rc != NO_ERROR {
            continue;
        }
        let count = (buf[0] as usize).min((buf.len() - 1) / ROW_WORDS);
        // SAFETY: on success the buffer holds the entry count followed by that many rows; `count` is
        // capped to the rows that fit in `buf`.
        let rows = unsafe { std::slice::from_raw_parts(buf.as_ptr().add(1).cast::<MIB_TCPROW_OWNER_PID>(), count) };
        return rows
            .iter()
            .find(|r| u16::from_be((r.dwLocalPort & 0xffff) as u16) == port && (r.dwLocalAddr == loopback || r.dwLocalAddr == 0))
            .map(|r| r.dwOwningPid);
    }
    None
}

#[cfg(not(windows))]
pub fn pid_on_port(_port: u16) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_starts_in_the_temp_folder() {
        // Arrange
        let c = command("whatever");
        // Act
        let cwd = c.get_current_dir().map(|p| p.to_path_buf());
        // Assert
        assert_eq!(cwd, Some(std::env::temp_dir()));
    }

    #[cfg(windows)]
    #[test]
    fn finds_the_pid_listening_on_a_local_port() {
        // Arrange
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Act
        let pid = pid_on_port(port);
        // Assert
        assert_eq!(pid, Some(std::process::id()));
    }

    #[cfg(windows)]
    #[test]
    fn finds_no_pid_on_a_port_nothing_listens_on() {
        // Arrange
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        // Act
        let pid = pid_on_port(port);
        // Assert
        assert_eq!(pid, None);
    }

    #[cfg(windows)]
    #[test]
    fn kill_tree_ends_a_process_and_its_children() {
        use std::time::Duration;
        // Arrange: cmd.exe running ping, which is cmd's child
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let ping = system.join("PING.EXE").to_string_lossy().into_owned();
        let mut cmd = command(system.join("cmd.exe"));
        cmd.args(["/c", ping.as_str(), "-n", "30", "127.0.0.1"]).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let mut started = Vec::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            let now = tree(child.id());
            if now.len() > 1 && now == started {
                break;
            }
            started = now;
        }
        assert!(started.len() > 1, "cmd started no child");
        // Act
        kill_tree(child.id());
        // Assert
        assert!(!child.wait().unwrap().success());
        let gone = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            let all = processes();
            started.iter().all(|pid| !all.iter().any(|(p, _)| p == pid))
        });
        assert!(gone, "{started:?} still run");
    }
}
