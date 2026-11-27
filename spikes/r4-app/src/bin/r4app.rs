//! R4 spike, checks 3 to 7: a GPUI app that outlives its window, with a tray icon, one app per root and
//! window bounds kept across a close.
//!
//! r4app [--root DIR] [--log FILE] [--hookup handler|poll] [--tray] [--open-at X,Y,W,H[,max]]
//!       [--balloon] [--end-menu-ms N] [--plain] [--native-close]
//!
//! `--native-close` lets Windows destroy the window on its close button, which leaks the window's
//! renderer in gpui-pre 0.3.11; by default the close is refused and GPUI removes the window itself.

#![windows_subsystem = "windows"]

extern crate gpui_kit as gpui;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Global, IntoElement, ParentElement, QuitMode,
    Render, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions, div, point, px, rgb, size,
};
use sha2::{Digest, Sha256};
use tray_icon::menu::{ContextMenu, Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GR_GDIOBJECTS, GR_USEROBJECTS, GetCurrentProcess, GetGuiResources,
    INFINITE, SetEvent, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{NIF_INFO, NIIF_INFO, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateIconFromResourceEx, EndMenu, GetMenuItemCount,
    GetMenuItemID, GetSystemMetrics, HMENU, LR_DEFAULTCOLOR, SM_CXSMICON,
};

const ICONS: [(&str, &[u8]); 4] = [
    ("Live", include_bytes!("../../icons/green.ico")),
    ("Starting", include_bytes!("../../icons/amber.ico")),
    ("Off air", include_bytes!("../../icons/red.ico")),
    ("Stopped", include_bytes!("../../icons/grey.ico")),
];
const SWAP_EVERY: Duration = Duration::from_secs(2);
const POLL_EVERY: Duration = Duration::from_millis(100);
const MEMORY_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hookup {
    Handler,
    Poll,
}

struct Args {
    root: PathBuf,
    log: PathBuf,
    hookup: Hookup,
    tray_only: bool,
    open_at: Option<WindowBounds>,
    balloon: bool,
    end_menu_ms: Option<u64>,
    plain: bool,
    close_via_remove: bool,
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let mut root = std::env::current_dir().unwrap_or_default();
    let mut log = None;
    let mut hookup = Hookup::Handler;
    let mut tray_only = false;
    let mut open_at = None;
    let mut balloon = false;
    let mut end_menu_ms = None;
    let mut plain = false;
    let mut close_via_remove = true;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => root = it.next().map(PathBuf::from).unwrap_or(root),
            "--log" => log = it.next().map(PathBuf::from),
            "--hookup" => hookup = if it.next().as_deref() == Some("poll") { Hookup::Poll } else { Hookup::Handler },
            "--tray" => tray_only = true,
            "--open-at" => open_at = it.next().and_then(|v| parse_bounds(&v)),
            "--balloon" => balloon = true,
            "--end-menu-ms" => end_menu_ms = it.next().and_then(|v| v.parse().ok()),
            "--plain" => plain = true,
            "--close-via-remove" => close_via_remove = true,
            "--native-close" => close_via_remove = false,
            _ => {}
        }
    }
    let root = std::path::absolute(&root).unwrap_or(root);
    let log = log.unwrap_or_else(|| root.join("r4app.log"));
    Args { root, log, hookup, tray_only, open_at, balloon, end_menu_ms, plain, close_via_remove }
}

fn parse_bounds(v: &str) -> Option<WindowBounds> {
    let parts: Vec<&str> = v.split(',').collect();
    let n: Vec<f32> = parts.iter().take(4).filter_map(|p| p.parse().ok()).collect();
    if n.len() != 4 {
        return None;
    }
    let b = Bounds::new(point(px(n[0]), px(n[1])), size(px(n[2]), px(n[3])));
    Some(if parts.get(4) == Some(&"max") { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) })
}

static LOG: OnceLock<(PathBuf, Instant)> = OnceLock::new();

fn log(line: &str) {
    let Some((path, start)) = LOG.get() else { return };
    let ms = start.elapsed().as_millis();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{ms:>8}] pid {} {line}", std::process::id());
    }
}

fn fmt_bounds(b: &Bounds<gpui::Pixels>) -> String {
    format!(
        "{},{} {}x{}",
        b.origin.x.as_f32(),
        b.origin.y.as_f32(),
        b.size.width.as_f32(),
        b.size.height.as_f32()
    )
}

fn fmt_window_bounds(wb: &WindowBounds) -> String {
    match wb {
        WindowBounds::Windowed(b) => format!("Windowed({})", fmt_bounds(b)),
        WindowBounds::Maximized(b) => format!("Maximized({})", fmt_bounds(b)),
        WindowBounds::Fullscreen(b) => format!("Fullscreen({})", fmt_bounds(b)),
    }
}

fn memory_line() -> String {
    // SAFETY: queries this process only; the counters are sized as the API requires.
    unsafe {
        let process = GetCurrentProcess();
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        GetProcessMemoryInfo(process, (&raw mut counters).cast(), counters.cb);
        format!(
            "memory private={:.1}MB workingset={:.1}MB gdi={} user={}",
            counters.PrivateUsage as f64 / 1048576.0,
            counters.WorkingSetSize as f64 / 1048576.0,
            GetGuiResources(process, GR_GDIOBJECTS),
            GetGuiResources(process, GR_USEROBJECTS),
        )
    }
}

/// Local\StreamLoop-app-H and Local\StreamLoop-show-H, H the first 16 hex digits of SHA-256 of the
/// root's full path, lowercased.
fn instance_names(root: &Path) -> (String, String) {
    let lowered = root.to_string_lossy().to_lowercase();
    let digest = Sha256::digest(lowered.as_bytes());
    let h: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    (format!("Local\\StreamLoop-app-{h}"), format!("Local\\StreamLoop-show-{h}"))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

enum Claim {
    Ours { _mutex: usize, show: usize },
    Running,
}

fn claim(root: &Path) -> Claim {
    let (mutex_name, show_name) = instance_names(root);
    log(&format!("instance names {mutex_name} {show_name}"));
    let (m, s) = (wide(&mutex_name), wide(&show_name));
    // SAFETY: the names are NUL-terminated and outlive the calls; the handles stay open for the process's life.
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, m.as_ptr());
        let existed = GetLastError() == ERROR_ALREADY_EXISTS;
        // Auto-reset: one signal wakes the waiting thread once.
        let show = CreateEventW(std::ptr::null(), 0, 0, s.as_ptr());
        if existed {
            CloseHandle(mutex);
            Claim::Running
        } else {
            Claim::Ours { _mutex: mutex as usize, show: show as usize }
        }
    }
}

fn signal_show(root: &Path) {
    let (_, show_name) = instance_names(root);
    let s = wide(&show_name);
    // SAFETY: the name is NUL-terminated; the handle is closed after the signal.
    unsafe {
        // The first app may take the foreground with the rights this start was given.
        let allowed = AllowSetForegroundWindow(ASFW_ANY) != 0;
        let show = CreateEventW(std::ptr::null(), 0, 0, s.as_ptr());
        let set = SetEvent(show) != 0;
        CloseHandle(show);
        log(&format!("second instance: signalled show set={set} allow-foreground={allowed}"));
    }
}

#[derive(Debug)]
enum AppEvent {
    Open(&'static str),
    Quit,
    RightClick,
}

fn tray_event(e: &TrayIconEvent) -> Option<AppEvent> {
    match e {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            Some(AppEvent::Open("tray left click"))
        }
        TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => Some(AppEvent::Open("tray double click")),
        TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Up, .. } => {
            Some(AppEvent::RightClick)
        }
        _ => None,
    }
}

fn menu_event(e: &MenuEvent, open: &MenuId, quit: &MenuId) -> Option<AppEvent> {
    if &e.id == open {
        Some(AppEvent::Open("menu Open StreamLoop"))
    } else if &e.id == quit {
        Some(AppEvent::Quit)
    } else {
        None
    }
}

struct Shell {
    window: Option<AnyWindowHandle>,
    saved: Option<WindowBounds>,
    opened: u32,
    tray: Option<TrayIcon>,
    icons: Vec<Icon>,
    end_menu_ms: Option<u64>,
    plain: bool,
    close_via_remove: bool,
}

impl Global for Shell {}

/// `--plain`: a bare view through `cx.open_window`, without gpui-component's Root.
struct PlainView {
    n: u32,
}

impl Render for PlainView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().bg(rgb(0x14171c)).text_color(rgb(0xe6e6e6)).child(format!("Plain window #{}", self.n))
    }
}

fn on_close(window: &mut Window, cx: &mut App) -> bool {
    let wb = window.window_bounds();
    log(&format!(
        "window closing: window_bounds={} bounds={} is_maximized={}",
        fmt_window_bounds(&wb),
        fmt_bounds(&window.bounds()),
        window.is_maximized()
    ));
    let shell = cx.global_mut::<Shell>();
    shell.saved = Some(wb);
    if shell.close_via_remove {
        // GPUI drops the window itself; the native close is refused.
        window.remove_window();
        return false;
    }
    true
}

struct SpikeView {
    n: u32,
    hookup: Hookup,
}

impl Render for SpikeView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .bg(rgb(0x14171c))
            .text_color(rgb(0xe6e6e6))
            .child(format!("StreamLoop R4 spike, window #{}", self.n))
            .child(format!("Tray hookup: {:?}", self.hookup))
            .child("Close this window: the app stays in the tray. Click the tray icon to open it again.")
            .child(gpui_kit::component::button::Button::new("noop").label("A gpui-component button"))
    }
}

fn open_window(cx: &mut App, hookup: Hookup, why: &str) {
    let n = {
        let shell = cx.global_mut::<Shell>();
        shell.opened += 1;
        shell.opened
    };
    let requested = cx
        .global::<Shell>()
        .saved
        .unwrap_or_else(|| WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(800.)), cx)));
    // GPUI places the window on `display_id` (else the primary) and ignores bounds whose centre is off it.
    let centre = requested.get_bounds().center();
    let display_id = cx.displays().iter().find(|d| d.bounds().contains(&centre)).map(|d| d.id());
    log(&format!("window #{n} on display {display_id:?}"));
    let options = WindowOptions {
        window_bounds: Some(requested),
        display_id,
        window_min_size: Some(size(px(720.), px(560.))),
        titlebar: Some(TitlebarOptions { title: Some(format!("StreamLoop R4 spike #{n}").into()), ..Default::default() }),
        ..Default::default()
    };
    let opened = if cx.global::<Shell>().plain {
        cx.open_window(options, |window, cx| {
            window.on_window_should_close(cx, on_close);
            cx.new(|_| PlainView { n })
        })
        .map(AnyWindowHandle::from)
    } else {
        gpui_kit::open_window(options, cx, |window, cx| {
            window.on_window_should_close(cx, on_close);
            cx.new(|_| SpikeView { n, hookup })
        })
        .map(|(handle, _)| handle)
    };
    match opened {
        Ok(handle) => {
            cx.global_mut::<Shell>().window = Some(handle);
            log(&format!("window #{n} opened ({why}) requested={}", fmt_window_bounds(&requested)));
            let _ = handle.update(cx, |_, window, cx| {
                log(&format!(
                    "window #{n} at open: window_bounds={} bounds={} is_maximized={} scale={} display={:?}",
                    fmt_window_bounds(&window.window_bounds()),
                    fmt_bounds(&window.bounds()),
                    window.is_maximized(),
                    window.scale_factor(),
                    window.display(cx).map(|d| (d.id(), fmt_bounds(&d.bounds())))
                ));
                window.activate_window();
            });
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                cx.update(|cx| {
                    let _ = handle.update(cx, |_, window, _| {
                        log(&format!(
                            "window #{n} after 1.5 s: window_bounds={} is_maximized={}",
                            fmt_window_bounds(&window.window_bounds()),
                            window.is_maximized()
                        ));
                    });
                    log(&memory_line());
                });
            })
            .detach();
        }
        Err(e) => log(&format!("open window failed: {e:#}")),
    }
}

fn show(cx: &mut App, hookup: Hookup, why: &str) {
    if let Some(handle) = cx.global::<Shell>().window {
        if handle.update(cx, |_, window, _| window.activate_window()).is_ok() {
            log(&format!("window activated ({why})"));
            return;
        }
    }
    open_window(cx, hookup, why);
}

fn handle(event: AppEvent, cx: &mut App, hookup: Hookup, sent: Instant) {
    log(&format!("gpui got {event:?} via {hookup:?} after {} us", sent.elapsed().as_micros()));
    match event {
        AppEvent::Open(why) => show(cx, hookup, why),
        AppEvent::Quit => {
            log("quit: removing the tray icon, then cx.quit()");
            cx.global_mut::<Shell>().tray = None;
            cx.quit();
        }
        AppEvent::RightClick => {
            if let Some(ms) = cx.global::<Shell>().end_menu_ms {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_millis(ms)).await;
                    // SAFETY: EndMenu acts on this thread's menu, which TrackPopupMenu opened here.
                    let ended = unsafe { EndMenu() } != 0;
                    cx.update(|_| log(&format!("EndMenu after {ms} ms: {ended}")));
                })
                .detach();
            }
        }
    }
}

/// The entry of `ico` closest to the small-icon size, as an HICON.
fn icon_from_ico(ico: &[u8]) -> Option<Icon> {
    // SAFETY: reads a system metric.
    let want = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let entries = (0..count).map(|i| {
        let e = &ico[6 + 16 * i..6 + 16 * (i + 1)];
        let side = if e[0] == 0 { 256 } else { e[0] as u32 };
        let len = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        let offset = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
        (side, offset, len)
    });
    let (side, offset, len) = entries.min_by_key(|(side, ..)| (*side < want, side.abs_diff(want)))?;
    // SAFETY: `offset..offset+len` lies inside `ico`, which holds one BMP or PNG icon image there.
    let hicon = unsafe {
        CreateIconFromResourceEx(ico[offset..offset + len].as_ptr(), len as u32, 1, 0x0003_0000, side as i32, side as i32, LR_DEFAULTCOLOR)
    };
    (!hicon.is_null()).then(|| Icon::from_handle(hicon as isize))
}

fn build_tray() -> Result<(TrayIcon, Vec<Icon>, MenuId, MenuId), String> {
    let icons: Vec<Icon> = ICONS.iter().filter_map(|(_, bytes)| icon_from_ico(bytes)).collect();
    if icons.len() != ICONS.len() {
        return Err("an icon did not load".into());
    }
    let menu = Menu::new();
    let open = MenuItem::new("Open StreamLoop", true, None);
    let quit = MenuItem::new("Quit StreamLoop", true, None);
    menu.append_items(&[&open, &quit]).map_err(|e| e.to_string())?;
    let hmenu = menu.hpopupmenu() as HMENU;
    // SAFETY: `hmenu` is the menu just built, alive while `menu` is.
    let ids: Vec<u32> = unsafe { (0..GetMenuItemCount(hmenu)).map(|i| GetMenuItemID(hmenu, i)).collect() };
    log(&format!("menu command ids {ids:?} (Open, Quit)"));
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(format!("StreamLoop: {}", ICONS[0].0))
        .with_icon(icons[0].clone())
        .build()
        .map_err(|e| e.to_string())?;
    Ok((tray, icons, open.id().clone(), quit.id().clone()))
}

/// The tray hint as a balloon; returns the icon id the shell took, which tray-icon keeps private.
fn balloon(tray: &TrayIcon) -> Option<u32> {
    (1..=8).find(|&id| balloon_with_id(tray, id))
}

fn balloon_with_id(tray: &TrayIcon, id: u32) -> bool {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = tray.window_handle();
    nid.uID = id;
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    for (dst, src) in nid.szInfoTitle.iter_mut().zip("StreamLoop".encode_utf16()) {
        *dst = src;
    }
    for (dst, src) in nid.szInfo.iter_mut().zip("StreamLoop is still running. Quit it from the tray icon.".encode_utf16()) {
        *dst = src;
    }
    // SAFETY: `nid` names this process's own tray icon and is sized as the API requires.
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) != 0 }
}

fn main() {
    let args = parse_args();
    let _ = LOG.set((args.log.clone(), Instant::now()));
    log(&format!(
        "start root={} hookup={:?} tray_only={} args={:?}",
        args.root.display(),
        args.hookup,
        args.tray_only,
        std::env::args().skip(1).collect::<Vec<_>>()
    ));
    let show_event = match claim(&args.root) {
        Claim::Ours { show, .. } => show,
        Claim::Running => {
            if !args.tray_only {
                signal_show(&args.root);
            }
            log("second instance: exit 0");
            std::process::exit(0);
        }
    };

    let (tx, rx) = async_channel::unbounded::<(AppEvent, Instant)>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            // SAFETY: the event handle stays open for the process's life.
            unsafe { WaitForSingleObject(show_event as HANDLE, INFINITE) };
            log("show signal received");
            let _ = tx.send_blocking((AppEvent::Open("second instance"), Instant::now()));
        });
    }

    let hookup = args.hookup;
    let balloon_wanted = args.balloon;
    let open_at = args.open_at;
    let tray_only = args.tray_only;
    let end_menu_ms = args.end_menu_ms;
    let (plain, close_via_remove) = (args.plain, args.close_via_remove);
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            log(&format!(
                "displays {:?} primary {:?}",
                cx.displays().iter().map(|d| (d.id(), fmt_bounds(&d.bounds()), fmt_bounds(&d.visible_bounds()))).collect::<Vec<_>>(),
                cx.primary_display().map(|d| (d.id(), fmt_bounds(&d.bounds())))
            ));
            let (tray, icons, open_id, quit_id) = match build_tray() {
                Ok(t) => t,
                Err(e) => {
                    log(&format!("tray failed: {e}"));
                    cx.quit();
                    return;
                }
            };
            log(&format!("tray built on the GPUI main thread, hwnd={:?}", tray.window_handle()));
            cx.set_global(Shell {
                window: None,
                saved: open_at,
                opened: 0,
                tray: Some(tray),
                icons,
                end_menu_ms,
                plain,
                close_via_remove,
            });

            match hookup {
                Hookup::Handler => {
                    let t = tx.clone();
                    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                        if let Some(ev) = tray_event(&e) {
                            log(&format!("handler got {ev:?}"));
                            let _ = t.try_send((ev, Instant::now()));
                        }
                    }));
                    let t = tx.clone();
                    let (o, q) = (open_id.clone(), quit_id.clone());
                    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                        if let Some(ev) = menu_event(&e, &o, &q) {
                            log(&format!("handler got {ev:?}"));
                            let _ = t.try_send((ev, Instant::now()));
                        }
                    }));
                }
                Hookup::Poll => {
                    cx.spawn(async move |cx| loop {
                        cx.background_executor().timer(POLL_EVERY).await;
                        let mut got = Vec::new();
                        while let Ok(e) = TrayIconEvent::receiver().try_recv() {
                            got.extend(tray_event(&e));
                        }
                        while let Ok(e) = MenuEvent::receiver().try_recv() {
                            got.extend(menu_event(&e, &open_id, &quit_id));
                        }
                        if !got.is_empty() {
                            cx.update(|cx| {
                                for ev in got {
                                    handle(ev, cx, hookup, Instant::now());
                                }
                            });
                        }
                    })
                    .detach();
                }
            }
            // Every event that arrives through the channel: the show signal, and the tray in handler mode.
            cx.spawn(async move |cx| {
                while let Ok((ev, sent)) = rx.recv().await {
                    cx.update(|cx| handle(ev, cx, hookup, sent));
                }
            })
            .detach();

            cx.spawn(async move |cx| {
                let mut i = 0;
                let mut swaps = 0u32;
                loop {
                    cx.background_executor().timer(SWAP_EVERY).await;
                    i = (i + 1) % ICONS.len();
                    let still = cx.update(|cx| {
                        let shell = cx.global::<Shell>();
                        let Some(tray) = shell.tray.as_ref() else { return false };
                        let icon = tray.set_icon(Some(shell.icons[i].clone())).is_ok();
                        let tip = tray.set_tooltip(Some(format!("StreamLoop: {}", ICONS[i].0))).is_ok();
                        swaps += 1;
                        if !(icon && tip) || swaps <= 8 {
                            log(&format!("swap {swaps} to {}: icon={icon} tooltip={tip} rect={:?}", ICONS[i].0, tray.rect()));
                        }
                        true
                    });
                    if !still {
                        break;
                    }
                }
            })
            .detach();

            cx.spawn(async move |cx| loop {
                cx.background_executor().timer(MEMORY_EVERY).await;
                cx.update(|cx| log(&format!("{} windows={}", memory_line(), cx.windows().len())));
            })
            .detach();

            cx.on_window_closed(|cx, id| {
                let shell = cx.global_mut::<Shell>();
                if shell.window.is_some_and(|h| h.window_id() == id) {
                    shell.window = None;
                }
                log(&format!("window closed; windows left={}", cx.windows().len()));
            })
            .detach();

            if balloon_wanted {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    cx.update(|cx| {
                        if let Some(tray) = cx.global::<Shell>().tray.as_ref() {
                            log(&format!("balloon shown with icon id {:?}", balloon(tray)));
                        }
                    });
                })
                .detach();
            }

            log(&memory_line());
            if !tray_only {
                open_window(cx, hookup, "start");
            }
        });
    log("run returned: exit 0");
}
