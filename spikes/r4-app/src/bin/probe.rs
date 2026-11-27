//! R4 spike, checks 1 and 2: a GUI-subsystem exe that attaches to its parent's console by Ruling R-console,
//! plus the helpers the check drivers use. With `PROBE_LOG` set, every start records its standard handles there.

#![windows_subsystem = "windows"]

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, HWND, INVALID_HANDLE_VALUE,
    LPARAM, RECT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileType, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, CONSOLE_SCREEN_BUFFER_INFO, COORD, CTRL_C_EVENT,
    FreeConsole, GenerateConsoleCtrlEvent, GetConsoleMode, GetConsoleProcessList,
    GetConsoleScreenBufferInfo, GetConsoleWindow, GetStdHandle, ReadConsoleOutputCharacterW,
    STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
    SetStdHandle,
};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
    GetCurrentProcess,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetClassNameW, GetWindow, GetWindowPlacement, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, IsZoomed, PostMessageW, SWP_NOZORDER,
    SetWindowPos, ShowWindow, WINDOWPLACEMENT,
};

const VERSION: &str = "probe 0.1.0";
const ERROR_ACCESS_DENIED: u32 = 5;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");
    // The helpers read and post to other processes' windows in physical pixels.
    if HELPERS.contains(&mode) {
        // SAFETY: called once, before any window exists in this process.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    }
    let before = [std_info(STD_INPUT_HANDLE), std_info(STD_OUTPUT_HANDLE), std_info(STD_ERROR_HANDLE)];
    let window_before = unsafe { GetConsoleWindow() } as usize;
    // The starter, as `detach` will, never attaches: what it hands cmd must not depend on its caller's console.
    let attach = if mode == "detach-bat" { Attach::Skipped } else { attach_parent() };
    let after = [std_info(STD_INPUT_HANDLE), std_info(STD_OUTPUT_HANDLE), std_info(STD_ERROR_HANDLE)];
    let window_after = unsafe { GetConsoleWindow() } as usize;
    record(&format!(
        r#""event":"start","mode":"{}","args":{},"before":{{"stdin":{},"stdout":{},"stderr":{}}},"consoleWindowBefore":{},"attach":{},"after":{{"stdin":{},"stdout":{},"stderr":{}}},"consoleWindowAfter":{}"#,
        mode,
        json_list(&args),
        before[0].json(),
        before[1].json(),
        before[2].json(),
        window_before,
        attach.json(),
        after[0].json(),
        after[1].json(),
        after[2].json(),
        window_after,
    ));

    let code = match (mode, &args[1..]) {
        ("exit", [code, rest @ ..]) => mode_exit(code, rest.first()),
        ("wait", rest) => mode_wait(rest.first().and_then(|s| s.parse().ok()).unwrap_or(120)),
        ("version", []) => {
            say(VERSION);
            0
        }
        ("spawn-null", child) if !child.is_empty() => mode_spawn_null(child),
        ("launchlike", []) => mode_launchlike(),
        ("hold", rest) => mode_hold(rest.first().and_then(|s| s.parse().ok()).unwrap_or(60)),
        ("detach-bat", [bat]) => mode_detach_bat(bat, false),
        ("detach-bat", [bat, how]) if how == "stdin-only" => mode_detach_bat(bat, true),
        ("windows", []) => helper_windows(),
        ("post", [hwnd, msg, wparam, lparam]) => helper_post(hwnd, msg, wparam, lparam),
        ("ctrlc", [pid]) => helper_ctrlc(pid),
        ("readcon", [pid]) => helper_readcon(pid),
        ("type", [pid, text]) => helper_type(pid, text),
        ("setpos", [hwnd, x, y, w, h]) => helper_setpos(hwnd, x, y, w, h),
        ("showcmd", [hwnd, cmd]) => helper_showcmd(hwnd, cmd),
        ("rect", [hwnd]) => helper_rect(hwnd),
        ("jobinfo", []) => helper_jobinfo(),
        ("icons", [dir]) => helper_icons(dir),
        _ => {
            say("usage: probe exit N [DELAY_MS] | wait [SECS] | version | spawn-null ARGS... | launchlike | hold [SECS] | detach-bat BAT | windows | post HWND MSG WPARAM LPARAM | ctrlc PID | readcon PID | setpos HWND X Y W H | showcmd HWND N | rect HWND | jobinfo | icons DIR");
            64
        }
    };
    std::process::exit(code);
}

const HELPERS: [&str; 8] = ["windows", "post", "ctrlc", "readcon", "setpos", "showcmd", "rect", "jobinfo"];

struct StdInfo {
    raw: usize,
    kind: &'static str,
    console_mode: bool,
}

impl StdInfo {
    fn absent(&self) -> bool {
        matches!(self.kind, "null" | "invalid" | "stale")
    }

    fn json(&self) -> String {
        format!(r#"{{"handle":"{:#x}","type":"{}","consoleMode":{}}}"#, self.raw, self.kind, self.console_mode)
    }
}

fn std_info(id: STD_HANDLE) -> StdInfo {
    // SAFETY: the handle is only queried; a null, invalid or stale value makes the calls fail.
    unsafe {
        let h = GetStdHandle(id);
        if h.is_null() {
            return StdInfo { raw: 0, kind: "null", console_mode: false };
        }
        if h == INVALID_HANDLE_VALUE {
            return StdInfo { raw: h as usize, kind: "invalid", console_mode: false };
        }
        let mut mode = 0;
        let console_mode = GetConsoleMode(h, &mut mode) != 0;
        let kind = match GetFileType(h) {
            1 => "disk",
            2 => "char",
            3 => "pipe",
            0x8000 => "remote",
            // FILE_TYPE_UNKNOWN with an error means the value names no open handle.
            _ if GetLastError() != 0 => "stale",
            _ => "unknown",
        };
        StdInfo { raw: h as usize, kind, console_mode }
    }
}

enum Attach {
    Skipped,
    Kept,
    Attached { reopened: Vec<&'static str> },
    NoConsole(u32),
}

impl Attach {
    fn json(&self) -> String {
        match self {
            Attach::Skipped => r#"{"result":"skipped"}"#.to_string(),
            Attach::Kept => r#"{"result":"kept-redirected"}"#.to_string(),
            Attach::Attached { reopened } => {
                format!(r#"{{"result":"attached","reopened":{}}}"#, json_list(reopened))
            }
            Attach::NoConsole(err) => format!(r#"{{"result":"no-parent-console","error":{err}}}"#),
        }
    }
}

/// R-console: a pipe, a file or NUL on stdout is kept; an absent stdout or a console handle attaches.
fn attach_parent() -> Attach {
    let out = std_info(STD_OUTPUT_HANDLE);
    if !out.absent() && !out.console_mode {
        return Attach::Kept;
    }
    // SAFETY: plain console calls; a failure is reported, not acted on.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return Attach::NoConsole(unsafe { GetLastError() });
    }
    // A standard handle that still names nothing gets the console's own, so output reaches the window.
    let mut reopened = Vec::new();
    for (id, name, device) in [
        (STD_INPUT_HANDLE, "stdin", "CONIN$"),
        (STD_OUTPUT_HANDLE, "stdout", "CONOUT$"),
        (STD_ERROR_HANDLE, "stderr", "CONOUT$"),
    ] {
        let info = std_info(id);
        if info.absent() {
            if let Some(h) = open_device(device) {
                // SAFETY: `h` is a fresh console handle owned by this process for its lifetime.
                unsafe { SetStdHandle(id, h) };
                reopened.push(name);
            }
        }
    }
    Attach::Attached { reopened }
}

fn open_device(name: &str) -> Option<HANDLE> {
    let wide = wide(name);
    // SAFETY: `wide` is a NUL-terminated name that outlives the call.
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    (h != INVALID_HANDLE_VALUE).then_some(h)
}

fn mode_exit(code: &str, delay: Option<&String>) -> i32 {
    let code: i32 = code.parse().unwrap_or(1);
    say(&format!("probe exit {code} (pid {})", std::process::id()));
    if let Some(ms) = delay.and_then(|d| d.parse().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
    record(&format!(r#""event":"exit","code":{code}"#));
    code
}

unsafe extern "system" fn on_ctrl(event: u32) -> i32 {
    let name = match event {
        0 => "CTRL_C_EVENT",
        1 => "CTRL_BREAK_EVENT",
        2 => "CTRL_CLOSE_EVENT",
        5 => "CTRL_LOGOFF_EVENT",
        6 => "CTRL_SHUTDOWN_EVENT",
        _ => "OTHER",
    };
    record(&format!(r#""event":"ctrl","ctrl":"{name}""#));
    say(&format!("probe got {name} (pid {})", std::process::id()));
    std::process::exit(0);
}

fn install_ctrl_handler() {
    // SAFETY: `on_ctrl` is a plain function that lives as long as the process.
    let ok = unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) } != 0;
    record(&format!(r#""event":"handler","installed":{ok}"#));
}

fn mode_wait(secs: u64) -> i32 {
    install_ctrl_handler();
    say(&format!("probe waiting for a console event (pid {})", std::process::id()));
    std::thread::sleep(Duration::from_secs(secs));
    record(r#""event":"wait-timeout""#);
    3
}

/// (d): starts its child as the control process starts `launch` (CREATE_NO_WINDOW, NUL on all three), then waits.
fn mode_spawn_null(child: &[String]) -> i32 {
    let exe = std::env::current_exe().expect("own path");
    let spawned = Command::new(exe)
        .args(child)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    match spawned {
        Ok(c) => {
            record(&format!(r#""event":"spawned","child":{}"#, c.id()));
            say(&format!("probe spawn-null started {} as pid {}", child.join(" "), c.id()));
        }
        Err(e) => {
            record(&format!(r#""event":"spawn-failed","error":"{e}""#));
            return 1;
        }
    }
    mode_wait(120)
}

/// (e): starts its child as `launch` starts the supervisor (detached, NUL on all three), lives 3 s and exits.
fn mode_launchlike() -> i32 {
    let (child, breakaway) = match spawn_detached(&["hold".into(), "60".into()], DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP) {
        Ok(v) => v,
        Err(e) => {
            record(&format!(r#""event":"spawn-failed","error":"{e}""#));
            return 1;
        }
    };
    record(&format!(r#""event":"spawned","child":{child},"breakaway":{breakaway}"#));
    std::thread::sleep(Duration::from_secs(3));
    record(r#""event":"launchlike-exit""#);
    0
}

fn spawn_detached(args: &[String], flags: u32) -> std::io::Result<(u32, bool)> {
    let exe = std::env::current_exe()?;
    let mut breakaway = true;
    loop {
        let spawned = Command::new(&exe)
            .args(args)
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags | if breakaway { CREATE_BREAKAWAY_FROM_JOB } else { 0 })
            .spawn();
        match spawned {
            Ok(c) => return Ok((c.id(), breakaway)),
            Err(e) if breakaway && e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => breakaway = false,
            Err(e) => return Err(e),
        }
    }
}

fn mode_hold(secs: u64) -> i32 {
    install_ctrl_handler();
    std::thread::sleep(Duration::from_secs(secs));
    record(r#""event":"hold-done""#);
    0
}

/// Check 2's starter, as `detach start-bat` will be: cmd.exe with a hidden console, out of the caller's job,
/// NUL on all three handles; with `stdin-only`, cmd inherits stdout and stderr, absent when this has none.
fn mode_detach_bat(bat: &str, stdin_only: bool) -> i32 {
    let system = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    let cmd = system.join("System32").join("cmd.exe");
    let mut breakaway = true;
    loop {
        let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | if breakaway { CREATE_BREAKAWAY_FROM_JOB } else { 0 };
        let out = || if stdin_only { Stdio::inherit() } else { Stdio::null() };
        let spawned = Command::new(&cmd)
            .raw_arg(format!("/d /s /c \"\"{bat}\"\""))
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::null())
            .stdout(out())
            .stderr(out())
            .creation_flags(flags)
            .spawn();
        match spawned {
            Ok(c) => {
                record(&format!(r#""event":"detached","cmd":{},"flags":"{flags:#x}","breakaway":{breakaway}"#, c.id()));
                say(&format!("probe detach-bat started cmd.exe as pid {}", c.id()));
                return 0;
            }
            Err(e) if breakaway && e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => breakaway = false,
            Err(e) => {
                record(&format!(r#""event":"spawn-failed","error":"{e}""#));
                return 1;
            }
        }
    }
}

// Helpers for the check drivers, which read their output through a pipe.

fn helper_windows() -> i32 {
    unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> i32 {
        let mut pid = 0;
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let mut class = [0u16; 256];
        let mut title = [0u16; 256];
        // SAFETY: `hwnd` comes from EnumWindows; every buffer is sized as passed.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            GetWindowRect(hwnd, &mut rect);
            let cn = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
            let tn = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
            let owner = GetWindow(hwnd, GW_OWNER) as usize;
            println!(
                "{}\t{}\t{}\t{}\t{}\t{},{},{},{}\t{}",
                hwnd as usize,
                pid,
                IsWindowVisible(hwnd),
                String::from_utf16_lossy(&class[..cn]),
                String::from_utf16_lossy(&title[..tn]).replace(['\t', '\n'], " "),
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                owner,
            );
        }
        1
    }
    // SAFETY: the callback only reads each window.
    unsafe { EnumWindows(Some(each), 0) };
    0
}

fn num(s: &str) -> isize {
    s.strip_prefix("0x").map_or_else(|| s.parse().unwrap_or(0), |h| isize::from_str_radix(h, 16).unwrap_or(0))
}

fn helper_post(hwnd: &str, msg: &str, wparam: &str, lparam: &str) -> i32 {
    // SAFETY: posting to a window handle that may be gone only fails.
    let ok = unsafe { PostMessageW(num(hwnd) as HWND, num(msg) as u32, num(wparam) as usize, num(lparam)) } != 0;
    println!("post {ok}");
    if ok { 0 } else { 1 }
}

/// Runs `f` attached to `pid`'s console, then gives the console back and restores this process's handles.
fn with_console_of<T>(pid: &str, f: impl FnOnce() -> T) -> Result<T, u32> {
    let pid: u32 = pid.parse().unwrap_or(0);
    let saved = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|id| (id, unsafe { GetStdHandle(id) }));
    // SAFETY: plain console calls; the saved handles are put back as they were.
    unsafe {
        FreeConsole();
        if AttachConsole(pid) == 0 {
            return Err(GetLastError());
        }
        let out = f();
        FreeConsole();
        for (id, h) in saved {
            SetStdHandle(id, h);
        }
        Ok(out)
    }
}

fn console_pids() -> Vec<u32> {
    let mut pids = vec![0u32; 64];
    // SAFETY: the buffer holds as many entries as passed.
    let n = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) } as usize;
    pids.truncate(n.min(64));
    pids
}

fn helper_ctrlc(pid: &str) -> i32 {
    let result = with_console_of(pid, || {
        let attached = console_pids();
        // SAFETY: this process ignores the event it sends to everyone else on the console.
        let sent = unsafe {
            SetConsoleCtrlHandler(None, 1);
            GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) != 0
        };
        std::thread::sleep(Duration::from_millis(500));
        (attached, sent)
    });
    match result {
        Ok((attached, sent)) => {
            println!("ctrlc sent={sent} console-processes={attached:?}");
            0
        }
        Err(e) => {
            println!("ctrlc attach failed: {e}");
            1
        }
    }
}

fn helper_readcon(pid: &str) -> i32 {
    let result = with_console_of(pid, || {
        let attached = console_pids();
        let mut lines = Vec::new();
        let window = unsafe { GetConsoleWindow() } as usize;
        if let Some(h) = open_device("CONOUT$") {
            // SAFETY: `h` is this process's console output handle; reads stay within the buffer's width.
            unsafe {
                let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
                if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
                    let width = info.dwSize.X.max(1) as usize;
                    let mut buf = vec![0u16; width];
                    for y in 0..=info.dwCursorPosition.Y {
                        let mut read = 0;
                        ReadConsoleOutputCharacterW(h, buf.as_mut_ptr(), width as u32, COORD { X: 0, Y: y }, &mut read);
                        lines.push(String::from_utf16_lossy(&buf[..read as usize]).trim_end().to_string());
                    }
                }
                CloseHandle(h);
            }
        }
        (attached, window, lines)
    });
    match result {
        Ok((attached, window, lines)) => {
            println!("console-processes={attached:?} console-window={window}");
            for l in lines.iter().filter(|l| !l.is_empty()) {
                println!("| {l}");
            }
            0
        }
        Err(e) => {
            println!("readcon attach failed: {e}");
            1
        }
    }
}

/// Types `text` into `pid`'s console as key presses; `~` stands for Enter.
fn helper_type(pid: &str, text: &str) -> i32 {
    use windows_sys::Win32::System::Console::{
        INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, WriteConsoleInputW,
    };
    let result = with_console_of(pid, || {
        let Some(h) = open_device("CONIN$") else { return 0 };
        let mut records = Vec::new();
        for c in text.encode_utf16() {
            let (ch, vk) = if c == u16::from(b'~') { (13, 0x0d) } else { (c, 0) };
            for down in [1, 0] {
                records.push(INPUT_RECORD {
                    EventType: KEY_EVENT as u16,
                    Event: INPUT_RECORD_0 {
                        KeyEvent: KEY_EVENT_RECORD {
                            bKeyDown: down,
                            wRepeatCount: 1,
                            wVirtualKeyCode: vk,
                            wVirtualScanCode: 0,
                            uChar: KEY_EVENT_RECORD_0 { UnicodeChar: ch },
                            dwControlKeyState: 0,
                        },
                    },
                });
            }
        }
        let mut written = 0;
        // SAFETY: `records` holds as many entries as passed; `h` is this process's console input.
        unsafe {
            WriteConsoleInputW(h, records.as_ptr(), records.len() as u32, &mut written);
            CloseHandle(h);
        }
        written
    });
    println!("type wrote {result:?} records");
    0
}

fn helper_setpos(hwnd: &str, x: &str, y: &str, w: &str, h: &str) -> i32 {
    // SAFETY: a stale handle only makes the call fail.
    let ok = unsafe { SetWindowPos(num(hwnd) as HWND, std::ptr::null_mut(), num(x) as i32, num(y) as i32, num(w) as i32, num(h) as i32, SWP_NOZORDER) } != 0;
    println!("setpos {ok}");
    0
}

fn helper_showcmd(hwnd: &str, cmd: &str) -> i32 {
    // SAFETY: a stale handle only makes the call fail.
    unsafe { ShowWindow(num(hwnd) as HWND, num(cmd) as i32) };
    println!("showcmd done");
    0
}

fn helper_rect(hwnd: &str) -> i32 {
    let hwnd = num(hwnd) as HWND;
    // SAFETY: a stale handle only makes the calls fail; the structures are sized as the API requires.
    unsafe {
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(hwnd, &mut rect);
        let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
        placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        GetWindowPlacement(hwnd, &mut placement);
        let n = placement.rcNormalPosition;
        println!(
            "rect={},{},{},{} zoomed={} showCmd={} normal={},{},{},{}",
            rect.left, rect.top, rect.right, rect.bottom,
            IsZoomed(hwnd),
            placement.showCmd,
            n.left, n.top, n.right, n.bottom
        );
    }
    0
}

fn helper_jobinfo() -> i32 {
    let mut in_job = 0;
    // SAFETY: queries this process only.
    unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) };
    println!("in-job={}", in_job != 0);
    0
}

/// Writes the four tray colours as .ico files: a circle with a play mark, 16 to 48 px, 32-bit BMP entries.
fn helper_icons(dir: &str) -> i32 {
    const COLOURS: [(&str, [u8; 3]); 4] = [
        ("green", [0x2e, 0x9e, 0x44]),
        ("amber", [0xe0, 0x9a, 0x10]),
        ("red", [0xd0, 0x31, 0x31]),
        ("grey", [0x86, 0x86, 0x86]),
    ];
    const SIZES: [u32; 5] = [16, 20, 24, 32, 48];
    for (name, rgb) in COLOURS {
        let images: Vec<Vec<u8>> = SIZES.iter().map(|&s| bmp_entry(s, rgb)).collect();
        let mut ico = Vec::new();
        ico.extend_from_slice(&[0, 0, 1, 0]);
        ico.extend_from_slice(&(SIZES.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * SIZES.len() as u32;
        for (s, img) in SIZES.iter().zip(&images) {
            ico.extend_from_slice(&[*s as u8, *s as u8, 0, 0]);
            ico.extend_from_slice(&1u16.to_le_bytes());
            ico.extend_from_slice(&32u16.to_le_bytes());
            ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
            offset += img.len() as u32;
        }
        for img in &images {
            ico.extend_from_slice(img);
        }
        let path = std::path::Path::new(dir).join(format!("{name}.ico"));
        if let Err(e) = std::fs::write(&path, ico) {
            println!("{}: {e}", path.display());
            return 1;
        }
        println!("wrote {}", path.display());
    }
    0
}

fn bmp_entry(size: u32, rgb: [u8; 3]) -> Vec<u8> {
    let s = size as f32;
    let (cx, cy, r) = (s / 2.0, s / 2.0, s / 2.0 - 0.5);
    // The play mark: a triangle pointing right, centred a little right of the middle.
    let tri = [(s * 0.40, s * 0.28), (s * 0.40, s * 0.72), (s * 0.74, s * 0.50)];
    let inside_tri = |x: f32, y: f32| {
        let sign = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| (x - bx) * (ay - by) - (ax - bx) * (y - by);
        let d1 = sign(tri[0], tri[1]);
        let d2 = sign(tri[1], tri[2]);
        let d3 = sign(tri[2], tri[0]);
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    for v in [40u32, size, size * 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(size * size * 4 + mask_row * size).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    // Rows bottom-up, 4 x 4 samples per pixel.
    for row in (0..size).rev() {
        for col in 0..size {
            let (mut cover, mut white) = (0.0f32, 0.0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = col as f32 + (sx as f32 + 0.5) / 4.0;
                    let y = row as f32 + (sy as f32 + 0.5) / 4.0;
                    if ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() <= r {
                        cover += 1.0 / 16.0;
                        if inside_tri(x, y) {
                            white += 1.0 / 16.0;
                        }
                    }
                }
            }
            let mix = |c: u8| {
                if cover == 0.0 {
                    0
                } else {
                    let w = white / cover;
                    (c as f32 * (1.0 - w) + 255.0 * w).round() as u8
                }
            };
            out.extend_from_slice(&[mix(rgb[2]), mix(rgb[1]), mix(rgb[0]), (cover * 255.0).round() as u8]);
        }
    }
    out.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    out
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn record(fields: &str) {
    let Some(path) = std::env::var_os("PROBE_LOG") else { return };
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let line = format!(r#"{{"ms":{ms},"pid":{},"ppid":{},{fields}}}"#, std::process::id(), parent_pid());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

fn parent_pid() -> u32 {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    let me = std::process::id();
    // SAFETY: the snapshot is checked and closed; the entry is sized as the API requires.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return 0;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut entry) != 0;
        let mut parent = 0;
        while more {
            if entry.th32ProcessID == me {
                parent = entry.th32ParentProcessID;
                break;
            }
            more = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        parent
    }
}

fn json_list<S: AsRef<str>>(items: &[S]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", s.as_ref().replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(","))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
