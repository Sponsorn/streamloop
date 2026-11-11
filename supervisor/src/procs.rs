//! Process handling: the kill-on-close Job Object, suspend for fault injection. The tree kill and
//! the port lookup live in `streamloop_common::procs`.

use std::process::Child;

pub use streamloop_common::procs::{command, kill_tree, pid_on_port};

/// Put the supervisor itself in a kill-on-close job: children and grandchildren inherit it
/// from their first instruction, and a supervisor crash takes them all down.
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
fn each(pid: u32, access: u32, f: impl Fn(windows_sys::Win32::Foundation::HANDLE) -> bool) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::OpenProcess;
    let mut any = false;
    for p in streamloop_common::procs::tree(pid) {
        unsafe {
            let h = OpenProcess(access, 0, p);
            if !h.is_null() {
                any |= f(h);
                CloseHandle(h);
            }
        }
    }
    any
}

/// Freeze a process tree (fault injection: a feeder that stops delivering).
#[cfg(windows)]
pub fn suspend(child: &Child) -> bool {
    use windows_sys::Win32::System::Threading::PROCESS_SUSPEND_RESUME;
    each(child.id(), PROCESS_SUSPEND_RESUME, |h| unsafe { NtSuspendProcess(h) == 0 })
}

#[cfg(not(windows))]
pub fn suspend(_child: &Child) -> bool {
    false // ponytail: fault injection is Windows-only; SIGSTOP when this runs elsewhere
}

#[cfg(windows)]
fn ft(f: &windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64
}

/// Kernel + user time of one process, in 100 ns units.
#[cfg(windows)]
pub fn cpu_times(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut c, mut e, mut k, mut u) = (z, z, z, z);
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        CloseHandle(h);
        (ok != 0).then(|| ft(&k) + ft(&u))
    }
}

#[cfg(not(windows))]
pub fn cpu_times(_pid: u32) -> Option<u64> {
    None
}

/// (idle, total) system time in 100 ns units, summed over all cores. Kernel time includes idle.
#[cfg(windows)]
pub fn system_times() -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    unsafe {
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut i, mut k, mut u) = (z, z, z);
        (GetSystemTimes(&mut i, &mut k, &mut u) != 0).then(|| (ft(&i), ft(&k) + ft(&u)))
    }
}

#[cfg(not(windows))]
pub fn system_times() -> Option<(u64, u64)> {
    None
}
