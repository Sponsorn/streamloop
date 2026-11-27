//! Autostart as a per-user Run value: `StreamLoop` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`,
//! holding `"ROOT\streamloop.exe" ui --tray`. Task Manager's Startup tab records a disable under
//! `...\Explorer\StartupApproved\Run`, which `GET /api/autostart` honours and a write clears.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use streamloop_common::paths::Layout;

use crate::api::{decode_vbs, vbs_target};

/// The value's name under both keys.
const VALUE: &str = "StreamLoop";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Where the Run value and its Task Manager flag live. Errors are the OS's, as `io::Error`.
pub trait AutostartStore: Send + Sync {
    /// StreamLoop under the Run key; `None` when it is not there.
    fn run_value(&self) -> io::Result<Option<String>>;
    fn set_run_value(&self, command: &str) -> io::Result<()>;
    /// A value that is not there counts as deleted.
    fn delete_run_value(&self) -> io::Result<()>;
    /// StreamLoop under StartupApproved\Run; `None` when it is not there.
    fn approved(&self) -> io::Result<Option<Vec<u8>>>;
    /// A value that is not there counts as deleted.
    fn clear_approved(&self) -> io::Result<()>;
}

/// `"ROOT\streamloop.exe" ui --tray`.
pub fn run_command(layout: &Layout) -> String {
    format!("\"{}\" ui --tray", layout.exe.display())
}

/// The exe path in a Run value: the quoted first token, or the first token when it is unquoted.
pub fn run_target(value: &str) -> Option<PathBuf> {
    let value = value.trim_start();
    let token = match value.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => value.split_whitespace().next().unwrap_or(""),
    };
    (!token.is_empty()).then(|| PathBuf::from(token))
}

/// Task Manager writes a first byte of 3 for a disabled entry and 2 for an enabled one; odd means disabled.
pub fn disabled(approved: Option<&[u8]>) -> bool {
    approved.and_then(|bytes| bytes.first()).is_some_and(|b| b & 1 == 1)
}

/// Whether two paths name one file: canonical forms when both exist, else the absolute paths, either way
/// case-insensitively.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let fold = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    let absolute = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => fold(&a) == fold(&b),
        _ => fold(&absolute(a)) == fold(&absolute(b)),
    }
}

/// At start: a `StreamLoop.vbs` that StreamLoop wrote and that points at this install's `START.bat` becomes the
/// Run value. The value goes first, so a failed write leaves the VBS in place. Returns whether it migrated;
/// a VBS of another install and one StreamLoop did not write stay.
pub fn migrate_vbs(store: &dyn AutostartStore, startup_dir: &Path, layout: &Layout) -> io::Result<bool> {
    let path = startup_dir.join("StreamLoop.vbs");
    if !path.exists() {
        return Ok(false);
    }
    let Some(target) = vbs_target(&decode_vbs(&std::fs::read(&path)?)) else {
        return Ok(false);
    };
    if !same_path(Path::new(&target), &layout.root.join("START.bat")) {
        return Ok(false);
    }
    store.set_run_value(&run_command(layout))?;
    std::fs::remove_file(&path)?;
    Ok(true)
}

/// The registry. `real(None)` is the user's own Run key; a test key keeps a run off it.
pub struct Registry {
    run: String,
    approved: String,
}

impl Registry {
    /// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, or `HKCU\<test_key>\Run` with
    /// `HKCU\<test_key>\StartupApproved\Run` beside it when a test key is given.
    pub fn real(test_key: Option<&str>) -> Registry {
        match test_key {
            Some(key) => {
                let key = key.trim_matches('\\');
                Registry { run: format!(r"{key}\Run"), approved: format!(r"{key}\StartupApproved\Run") }
            }
            None => Registry { run: RUN_KEY.to_string(), approved: APPROVED_KEY.to_string() },
        }
    }

    /// Writes the StartupApproved value, as Task Manager does when it toggles the entry. Tests use it to stand in.
    pub fn set_approved(&self, bytes: &[u8]) -> io::Result<()> {
        win::set(&self.approved, win::BINARY, bytes)
    }
}

impl AutostartStore for Registry {
    fn run_value(&self) -> io::Result<Option<String>> {
        Ok(win::get(&self.run, win::STRING)?.map(|bytes| {
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units).trim_end_matches('\0').to_string()
        }))
    }

    fn set_run_value(&self, command: &str) -> io::Result<()> {
        let bytes: Vec<u8> = command.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
        win::set(&self.run, win::STRING, &bytes)
    }

    fn delete_run_value(&self) -> io::Result<()> {
        win::delete(&self.run)
    }

    fn approved(&self) -> io::Result<Option<Vec<u8>>> {
        win::get(&self.approved, win::BINARY)
    }

    fn clear_approved(&self) -> io::Result<()> {
        win::delete(&self.approved)
    }
}

mod win {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_BINARY, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_BINARY,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };

    use super::VALUE;

    pub const STRING: u32 = REG_SZ;
    pub const BINARY: u32 = REG_BINARY;

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn check(code: u32) -> io::Result<()> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }

    /// The raw bytes of `VALUE` under `HKCU\key`, `None` when the key or the value is missing.
    pub fn get(key: &str, kind: u32) -> io::Result<Option<Vec<u8>>> {
        let (key, name) = (wide(key), wide(VALUE));
        let flags = if kind == STRING { RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND } else { RRF_RT_REG_BINARY };
        let mut buf: Vec<u8> = vec![0; 512];
        loop {
            let mut size = buf.len() as u32;
            // SAFETY: both names are NUL-terminated and outlive the call; `buf` holds `size` bytes.
            let code = unsafe {
                RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), flags, std::ptr::null_mut(), buf.as_mut_ptr().cast::<c_void>(), &mut size)
            };
            match code {
                ERROR_SUCCESS => {
                    buf.truncate(size as usize);
                    return Ok(Some(buf));
                }
                ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => return Ok(None),
                ERROR_MORE_DATA => buf.resize((size as usize).max(buf.len() * 2), 0),
                other => return Err(io::Error::from_raw_os_error(other as i32)),
            }
        }
    }

    /// Writes `VALUE` under `HKCU\key`, creating the key when it is missing.
    pub fn set(key: &str, kind: u32, bytes: &[u8]) -> io::Result<()> {
        let (key, name) = (wide(key), wide(VALUE));
        // SAFETY: both names are NUL-terminated and `bytes` is `len` readable bytes.
        let code = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), kind, bytes.as_ptr().cast::<c_void>(), bytes.len() as u32) };
        check(code)
    }

    /// Deletes `VALUE` under `HKCU\key`; a missing key or value is already deleted.
    pub fn delete(key: &str) -> io::Result<()> {
        let (key, name) = (wide(key), wide(VALUE));
        // SAFETY: both names are NUL-terminated and outlive the call.
        match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(()),
            code => check(code),
        }
    }
}

/// A store that keeps its values in memory, for tests.
#[derive(Default)]
pub struct FakeAutostart {
    state: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    run: Option<String>,
    approved: Option<Vec<u8>>,
    fail_set: bool,
}

impl FakeAutostart {
    pub fn new() -> FakeAutostart {
        FakeAutostart::default()
    }

    pub fn with_run(command: &str) -> FakeAutostart {
        let fake = FakeAutostart::new();
        fake.state().run = Some(command.to_string());
        fake
    }

    /// Task Manager's flag for the entry.
    pub fn set_approved(&self, bytes: &[u8]) {
        self.state().approved = Some(bytes.to_vec());
    }

    /// Makes every `set_run_value` fail, as a locked-down registry would.
    pub fn fail_writes(&self) {
        self.state().fail_set = true;
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl AutostartStore for FakeAutostart {
    fn run_value(&self) -> io::Result<Option<String>> {
        Ok(self.state().run.clone())
    }

    fn set_run_value(&self, command: &str) -> io::Result<()> {
        let mut state = self.state();
        if state.fail_set {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the registry refused the write"));
        }
        state.run = Some(command.to_string());
        Ok(())
    }

    fn delete_run_value(&self) -> io::Result<()> {
        self.state().run = None;
        Ok(())
    }

    fn approved(&self) -> io::Result<Option<Vec<u8>>> {
        Ok(self.state().approved.clone())
    }

    fn clear_approved(&self) -> io::Result<()> {
        self.state().approved = None;
        Ok(())
    }
}
