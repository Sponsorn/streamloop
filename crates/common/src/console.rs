//! The console of a GUI-subsystem `streamloop.exe`, which Windows starts without one.

/// Attaches to the parent process's console unless standard output is a pipe or a disk file (`GetFileType`
/// PIPE or DISK), which then stays the output; NUL and an absent handle both attach. Returns whether it
/// attached. No-op off Windows.
///
/// So the control process under a visible START.bat (cmd passes it no handles) and under a hidden one (cmd
/// passes it NUL) both end up on START.bat's console. A NUL output stays NUL, so under a hidden START.bat its
/// lines go nowhere while Ctrl+C and the console closing still reach it. Only the console modes call this,
/// never `launch` or `supervisor`, so the supervisor started by `launch` can never be on it. A handle still
/// absent after the attach is opened on the console (`CONOUT$`, `CONIN$`).
#[cfg(windows)]
pub fn attach_parent() -> bool {
    use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
    // SAFETY: only queries this process's standard output handle.
    if classify(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }) == Handle::Redirected {
        return false;
    }
    // SAFETY: a plain console call; it fails when the parent has no console.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return false;
    }
    fill_absent_handles();
    true
}

#[cfg(not(windows))]
pub fn attach_parent() -> bool {
    false
}

#[cfg(windows)]
use windows_sys::Win32::Foundation::HANDLE;

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    /// Null, invalid, or a value that names no open handle.
    None,
    /// A pipe or a disk file.
    Redirected,
    /// A console, NUL or another character device.
    Other,
}

#[cfg(windows)]
fn classify(handle: HANDLE) -> Handle {
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, INVALID_HANDLE_VALUE, NO_ERROR};
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_DISK, FILE_TYPE_PIPE, FILE_TYPE_UNKNOWN};
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Handle::None;
    }
    // SAFETY: the handle is only queried. The last error is cleared first, so only GetFileType's own error
    // marks a handle that names nothing.
    unsafe {
        SetLastError(NO_ERROR);
        match GetFileType(handle) {
            FILE_TYPE_PIPE | FILE_TYPE_DISK => Handle::Redirected,
            FILE_TYPE_UNKNOWN if GetLastError() != NO_ERROR => Handle::None,
            _ => Handle::Other,
        }
    }
}

/// Gives each standard handle that still names nothing the console's own, so output reaches the window.
#[cfg(windows)]
fn fill_absent_handles() {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    for (id, device) in [(STD_INPUT_HANDLE, "CONIN$"), (STD_OUTPUT_HANDLE, "CONOUT$"), (STD_ERROR_HANDLE, "CONOUT$")] {
        // SAFETY: only queries this process's standard handle.
        if classify(unsafe { GetStdHandle(id) }) != Handle::None {
            continue;
        }
        let name: Vec<u16> = device.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `name` is NUL-terminated and outlives the call. The handle it opens stays open as this
        // process's standard handle for the rest of its life.
        unsafe {
            let h = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if h != INVALID_HANDLE_VALUE {
                SetStdHandle(id, h);
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

    use super::*;

    #[test]
    fn a_pipe_and_a_disk_file_count_as_redirected() {
        // Arrange
        let (_reader, writer) = std::io::pipe().unwrap();
        let dir = std::env::temp_dir().join(format!("sl-common-console-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::fs::File::create(dir.join("out.log")).unwrap();

        // Act
        let pipe = classify(writer.as_raw_handle());
        let disk = classify(file.as_raw_handle());

        // Assert
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(pipe, Handle::Redirected);
        assert_eq!(disk, Handle::Redirected);
    }

    #[test]
    fn nul_does_not_count_as_redirected() {
        // Arrange: what a hidden START.bat's cmd hands the control process
        let nul = std::fs::OpenOptions::new().write(true).open("NUL").unwrap();

        // Act
        let kind = classify(nul.as_raw_handle());

        // Assert
        assert_eq!(kind, Handle::Other);
    }

    #[test]
    fn a_null_or_invalid_handle_names_nothing() {
        // Act
        let null = classify(std::ptr::null_mut());
        let invalid = classify(INVALID_HANDLE_VALUE);

        // Assert
        assert_eq!(null, Handle::None);
        assert_eq!(invalid, Handle::None);
    }
}
