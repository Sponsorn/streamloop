//! Process handling: the kill-on-close Job Object, suspend for fault injection, tree kill.

use std::process::{Child, Command, Stdio};

/// Children start outside the install folder (2.x spawn-cwd rule).
pub fn command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.current_dir(std::env::temp_dir()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// Put the supervisor itself in a kill-on-close job. Job membership is inherited, so every
/// child and grandchild (yt-dlp's PyInstaller worker, node) is in it from its first
/// instruction, and a supervisor crash closes the handle and takes them all down.
#[cfg(windows)]
pub fn enter_job() -> Result<(), String> {
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err("CreateJobObjectW failed".into());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of_val(&info) as u32,
        );
        if ok == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            return Err(format!("job setup failed: {}", std::io::Error::last_os_error()));
        }
        // The handle is deliberately leaked: it must live exactly as long as this process.
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn enter_job() -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
#[link(name = "ntdll")]
extern "system" {
    fn NtSuspendProcess(handle: windows_sys::Win32::Foundation::HANDLE) -> i32;
}

#[cfg(windows)]
pub fn suspend(child: &Child) -> bool {
    use std::os::windows::io::AsRawHandle;
    unsafe { NtSuspendProcess(child.as_raw_handle() as _) == 0 }
}

#[cfg(not(windows))]
pub fn suspend(_child: &Child) -> bool {
    false // ponytail: fault injection is Windows-only; SIGSTOP when this runs elsewhere
}

/// Kill a process and its descendants (yt-dlp's stub alone would orphan its worker).
pub fn kill_tree(pid: u32) {
    let _ = command("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// PID listening on a local TCP port, for killing an adopted token server.
pub fn pid_on_port(port: u16) -> Option<u32> {
    let out = command("netstat").args(["-ano", "-p", "TCP"]).output().ok()?;
    let needle = format!("127.0.0.1:{port}");
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() >= 5 && f[1] == needle && f[3] == "LISTENING").then(|| f[4].parse().ok()).flatten()
    })
}
