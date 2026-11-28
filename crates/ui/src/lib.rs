//! The StreamLoop app: the native GPUI window on gpui-kit, and its tray icon.

// GPUI's macros expand to `gpui::` paths; this resolves them through the Kit.
extern crate gpui_kit as gpui;

const DEFAULT_PORT: u16 = 7654;

pub mod api;
pub mod app;
pub mod dashboard;
pub mod follow;
pub mod instance;
pub mod logic;
pub mod logs;
pub mod monitor;
pub mod playback;
pub mod quit;
pub mod settings;
pub mod setup_check;
pub mod start;
#[cfg(test)]
mod test_support;
pub mod theme;
pub mod tray;
pub mod ui_log;
pub mod update;
pub mod videos;
pub mod webhooks;
pub mod widgets;
pub mod window_state;
pub mod wizard;

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, Global, Pixels, QuitMode, Render,
    Subscription, Task, TitlebarOptions, Window, WindowOptions, point, px,
};
use gpui_kit::component::Root;
use streamloop_common::{config, paths::Layout, version::VERSION};

use crate::{
    api::{Api, ApiError, client::Client, http::HttpApi},
    app::{AppView, now_ms},
    follow::{Detach, Spawn},
    instance::{Claim, Instance},
    start::StartedBat,
    tray::{Tray, TrayEvent, TrayFeed, TrayOutput},
    window_state::{DEFAULT_SIZE, MIN_SIZE, WindowState},
};

/// The window's state file in `ROOT\app\`.
const WINDOW_STATE: &str = "window.json";
/// For this long after an open the window's bounds are still the open's own: a window opened maximised
/// reads as windowed for about 1.5 s.
const OPEN_SETTLES_IN: Duration = Duration::from_secs(2);
/// A move or resize is saved once the window has stayed put this long.
const SAVE_AFTER_MOVE: Duration = Duration::from_secs(1);

/// What `streamloop.exe ui` was asked for. `root` is absolute; `follow` is the version an update restart
/// started this app to follow; `log_dir` holds `ui.log`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiOptions {
    pub root: Option<PathBuf>,
    pub port: Option<u16>,
    pub tray: bool,
    pub follow: Option<String>,
    pub log_dir: Option<PathBuf>,
}

/// What outlives a window. R3's `AppHandle` is the window's own.
#[derive(Clone)]
pub struct Shell {
    pub client: Client,
    pub root: Option<PathBuf>,
    /// `ROOT\app\window.json`; None without a root, and the window state then lives in memory for the run.
    pub state_path: Option<PathBuf>,
    /// The tray icon, from the start of the app to its end.
    pub tray: TrayFeed,
    /// `--port N`, which the app that follows an update is given too.
    pub port: Option<u16>,
    /// `--follow V`: while the control process reports V, this app does not restart again.
    pub followed: Option<String>,
    /// This app's version, the one `--version` prints.
    pub version: &'static str,
    /// Starts the app that follows an update.
    pub spawn: Rc<dyn Spawn>,
}

impl Shell {
    pub fn new(client: Client, opts: &UiOptions) -> Shell {
        let state_path = opts
            .root
            .as_deref()
            .map(|root| Layout::new(root).app_dir.join(WINDOW_STATE));
        Shell {
            client,
            root: opts.root.clone(),
            state_path,
            tray: TrayFeed::default(),
            port: opts.port,
            followed: opts.follow.clone(),
            version: VERSION,
            spawn: Rc::new(Detach),
        }
    }
}

/// How long a start waits for the install's lock: only an app that follows an update waits, for the app it
/// replaces to quit.
fn lock_wait(opts: &UiOptions) -> Duration {
    if opts.follow.is_some() {
        follow::LOCK_WAIT
    } else {
        Duration::ZERO
    }
}

/// The install's lock, or None when another app of the install keeps it. A start without `--tray` then asks that
/// app to show its window, except a follower: the app it waited for is the one it replaces.
fn take_lock(opts: &UiOptions) -> Option<Instance> {
    let root = opts.root.as_deref();
    match instance::claim(root, lock_wait(opts)) {
        Claim::Ours(instance) => Some(instance),
        Claim::Running => {
            if !opts.tray && opts.follow.is_none() {
                instance::signal_show(root);
            }
            None
        }
    }
}

/// Takes the install's lock, starts `START.bat` when nothing answers and opens the window unless `--tray`.
/// Closing the window leaves the app running; a second app of the install asks the first to show its window
/// and returns.
pub fn run(opts: UiOptions) {
    let Some(instance) = take_lock(&opts) else {
        return;
    };
    let root = opts.root.as_deref();
    let port = opts
        .port
        .or_else(|| root.and_then(|r| config::read_port(&Layout::new(r).config)))
        .unwrap_or(DEFAULT_PORT);
    let api = Arc::new(HttpApi::new(port));
    let answered = api.get_json("/api/status").err() != Some(ApiError::Unreachable);
    // GPUI's production clock is `Utc::now`, the time base the view measures the hint against.
    let started_bat = opts
        .root
        .clone()
        .filter(|_| start::start_if_needed(root, answered))
        .map(|root| StartedBat {
            root,
            at_ms: chrono::Utc::now().timestamp_millis(),
        });
    // The show signal arrives on a thread of its own; GPUI's main thread takes it from this channel.
    let (show_tx, show_rx) = async_channel::unbounded::<()>();
    instance.on_show(move || {
        let _ = show_tx.send_blocking(());
    });
    let shell = Shell::new(Client(api), &opts);
    let tray_only = opts.tray;
    let log_dir = opts.log_dir.clone();
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            theme::init(cx);
            set_started_bat(cx, started_bat);
            let events = shell.clone();
            let tray = Tray::new(cx, move |event, cx| on_tray_event(event, cx, &events));
            attach_tray(&shell, tray, log_dir.as_deref());
            begin(cx, &shell, tray_only);
            serve_show_signal(cx, shell, show_rx);
            cx.activate(true);
        });
    drop(instance);
}

/// The tray icon, or a line in `ui.log` in `log_dir` saying why there is none. Without its icon the app still
/// runs: a second start shows the window, and the closed-window poll still follows an update.
pub(crate) fn attach_tray(
    shell: &Shell,
    tray: anyhow::Result<impl TrayOutput + 'static>,
    log_dir: Option<&Path>,
) {
    match tray {
        Ok(tray) => shell.tray.attach(Box::new(tray)),
        Err(err) => {
            if let Some(dir) = log_dir {
                let line = format!("The tray icon could not be made: {err}");
                ui_log::append(dir, &ui_log::stamped(&line));
            }
        }
    }
}

/// Opens the window, or with `--tray` only polls for the tray icon. When this app started `START.bat`, the
/// tray icon counts to Connecting's hint from here, with the window open or not.
pub(crate) fn begin(cx: &mut App, shell: &Shell, tray_only: bool) {
    if let Some(started) = cx.try_global::<Starting>().and_then(|s| s.0.clone()) {
        shell.tray.started_bat(&started, cx);
    }
    if tray_only {
        poll_while_closed(shell, cx);
    } else {
        open_main_window(cx, shell);
    }
}

/// The tray icon's poll while no window is open. After a reconnect the app follows the control process's
/// version when it differs.
fn poll_while_closed(shell: &Shell, cx: &mut App) {
    // The tray feed owns the poll, so the poll keeps the shell without the feed and is handed the feed instead.
    let rest = Shell {
        tray: TrayFeed::default(),
        ..shell.clone()
    };
    let on_reconnect = move |tray: &TrayFeed, control: Option<&str>, cx: &mut App| {
        let shell = Shell {
            tray: tray.clone(),
            ..rest.clone()
        };
        let window_open = open_handle(cx).is_some();
        follow::check(&shell, control, window_open, cx);
    };
    shell
        .tray
        .poll_while_closed(shell.client.clone(), on_reconnect, cx);
}

/// A left click, a double click and Open StreamLoop show the window. Quit StreamLoop asks in the window
/// whether to quit StreamLoop.
pub(crate) fn on_tray_event(event: TrayEvent, cx: &mut App, shell: &Shell) {
    match event {
        TrayEvent::Open => show_main_window(cx, shell),
        TrayEvent::Quit => quit::ask_to_quit(shell, cx),
    }
}

/// The `START.bat` this app started, for the first window it opens.
#[derive(Default)]
struct Starting(Option<StartedBat>);

impl Global for Starting {}

pub(crate) fn set_started_bat(cx: &mut App, started: Option<StartedBat>) {
    cx.set_global(Starting(started));
}

/// What the app keeps between its windows, and the window open now.
struct MainWindow {
    shell: Shell,
    /// Where the window is, or was when it closed.
    state: WindowState,
    open: Option<OpenWindow>,
}

impl Global for MainWindow {}

struct OpenWindow {
    handle: AnyWindowHandle,
    /// Until then the window's bounds are not read.
    settled_at_ms: i64,
    save_after_move: Option<Task<()>>,
    _bounds: Subscription,
}

/// Opens the StreamLoop window where it last was, with every view built anew. The window's own polls feed the
/// tray icon from now on.
pub fn open_main_window(cx: &mut App, shell: &Shell) -> AnyWindowHandle {
    shell.tray.stop_polling();
    let (client, tray) = (shell.client.clone(), shell.tray.clone());
    let started_bat = cx.default_global::<Starting>().0.take();
    open_window_of(cx, shell, |window, cx| {
        cx.new(|cx| AppView::new(client, tray, started_bat, window, cx))
    })
    .0
}

/// Brings the window forward, or opens a fresh one when it is closed.
pub fn show_main_window(cx: &mut App, shell: &Shell) {
    if let Some(handle) = open_handle(cx)
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let handle = open_main_window(cx, shell);
    let _ = handle.update(cx, |_, window, _| window.activate_window());
}

/// Shows the window for each show signal from a second start.
pub(crate) fn serve_show_signal(cx: &mut App, shell: Shell, show: async_channel::Receiver<()>) {
    cx.spawn(async move |cx| {
        while show.recv().await.is_ok() {
            cx.update(|cx| show_main_window(cx, &shell));
        }
    })
    .detach();
}

/// Writes where the window is now, or where it was when it closed.
pub fn save_window_state(cx: &mut App) {
    if let Some(handle) = open_handle(cx)
        && handle
            .update(cx, |_, window, cx| remember(window, cx))
            .is_ok()
    {
        return;
    }
    write_state(cx);
}

/// The window's close button, Alt+F4 and the taskbar's Close. GPUI removes the window and Windows' own close is
/// refused: on gpui-pre 0.3.11 that path leaks about 26 MB per close. The app goes on in the tray, which says so
/// the first time in an install; without a tray icon the hint waits for a close that has one.
pub(crate) fn close_main_window(window: &mut Window, cx: &mut App) -> bool {
    let first_close = main_window(cx).is_some_and(|main| {
        let first = main.shell.tray.has_icon() && !main.state.tray_hint_shown;
        if first {
            main.state.tray_hint_shown = true;
        }
        first
    });
    remember(window, cx);
    let handle = window.window_handle();
    let shell = main_window(cx).map(|main| {
        if main.open.as_ref().is_some_and(|open| open.handle == handle) {
            main.open = None;
        }
        main.shell.clone()
    });
    window.remove_window();
    if let Some(shell) = shell {
        if first_close {
            shell.tray.notify(tray::NAME, tray::TRAY_HINT);
        }
        poll_while_closed(&shell, cx);
    }
    false
}

/// `build`'s view in the StreamLoop window, placed from the window state, closed through GPUI and saved after
/// every move or resize.
fn open_window_of<V: Render>(
    cx: &mut App,
    shell: &Shell,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> (AnyWindowHandle, Entity<V>) {
    if let Some(main) = main_window(cx) {
        main.shell = shell.clone();
    } else {
        let state = shell
            .state_path
            .as_deref()
            .map(window_state::load)
            .unwrap_or_default();
        cx.set_global(MainWindow {
            shell: shell.clone(),
            state,
            open: None,
        });
    }
    let displays = cx.displays();
    let areas: Vec<Bounds<Pixels>> = displays.iter().map(|display| display.bounds()).collect();
    let primary = cx.primary_display().map_or(
        Bounds::new(point(px(0.), px(0.)), DEFAULT_SIZE),
        |display| display.visible_bounds(),
    );
    let main = cx.global_mut::<MainWindow>();
    let placed = window_state::placement(&main.state, &areas, primary);
    main.state.set_bounds(placed);
    let options = WindowOptions {
        window_bounds: Some(placed),
        display_id: window_state::display_for(placed.get_bounds(), &areas)
            .map(|i| displays[i].id()),
        window_min_size: Some(MIN_SIZE),
        titlebar: Some(TitlebarOptions {
            title: Some("StreamLoop".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut bounds = None;
    let (handle, view) = gpui_kit::open_window(options, cx, |window, cx| {
        window.on_window_should_close(cx, close_main_window);
        let view = build(window, cx);
        bounds = Some(view.update(cx, |_, cx| {
            cx.observe_window_bounds(window, |_, window, cx| bounds_changed(window, cx))
        }));
        view
    })
    .expect("open the StreamLoop window");
    let settled_at_ms = now_ms(cx) + OPEN_SETTLES_IN.as_millis() as i64;
    cx.global_mut::<MainWindow>().open = Some(OpenWindow {
        handle,
        settled_at_ms,
        save_after_move: None,
        _bounds: bounds.expect("the window built its view"),
    });
    (handle, view)
}

fn main_window(cx: &mut App) -> Option<&mut MainWindow> {
    if cx.has_global::<MainWindow>() {
        Some(cx.global_mut::<MainWindow>())
    } else {
        None
    }
}

fn open_handle(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<MainWindow>()?
        .open
        .as_ref()
        .map(|open| open.handle)
}

/// The shell of the StreamLoop window, for a view inside it.
pub(crate) fn main_shell(cx: &App) -> Option<Shell> {
    Some(cx.try_global::<MainWindow>()?.shell.clone())
}

/// The app view of the window open now.
pub(crate) fn main_view(cx: &App) -> Option<Entity<AppView>> {
    let root = open_handle(cx)?.downcast::<Root>()?;
    root.read(cx).ok()?.view().clone().downcast::<AppView>().ok()
}

/// Saves the window's bounds once a move or resize has ended, from the open's settling on.
fn bounds_changed(window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let now = now_ms(cx);
    let moved = main_window(cx)
        .and_then(|main| main.open.as_ref())
        .is_some_and(|open| open.handle == handle && now >= open.settled_at_ms);
    if !moved {
        return;
    }
    let save = cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_AFTER_MOVE).await;
        cx.update(|cx| {
            let _ = handle.update(cx, |_, window, cx| remember(window, cx));
        });
    });
    if let Some(open) = main_window(cx).and_then(|main| main.open.as_mut()) {
        open.save_after_move = Some(save);
    }
}

/// Takes the window's bounds into the state once the open has settled, and writes the state.
fn remember(window: &Window, cx: &mut App) {
    let handle = window.window_handle();
    let now = now_ms(cx);
    let Some(main) = main_window(cx) else {
        return;
    };
    let settled = main
        .open
        .as_ref()
        .is_some_and(|open| open.handle == handle && now >= open.settled_at_ms);
    if settled {
        main.state.set_bounds(window.window_bounds());
    }
    write_state(cx);
}

/// A state that cannot be written stays in memory for the run.
fn write_state(cx: &App) {
    if let Some(main) = cx.try_global::<MainWindow>()
        && let Some(path) = &main.shell.state_path
    {
        let _ = window_state::save(path, &main.state);
    }
}

#[cfg(test)]
mod harness {
    use gpui::{Context, IntoElement, Render, TestAppContext, Window, div, prelude::*};
    use gpui_kit::component::Root;

    use crate::{api::fake::FakeApi, test_support};

    #[derive(Default)]
    struct Probe {
        renders: usize,
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.renders += 1;
            div().child("StreamLoop")
        }
    }

    #[gpui::test]
    fn harness_renders_a_window(cx: &mut TestAppContext) {
        // Init gets its own update: in the window's update, its effects draw the new window a second time.
        cx.update(gpui_kit::init);
        let shell = test_support::shell(FakeApi::new());

        let (window, probe) = cx.update(|cx| {
            super::open_window_of(cx, &shell, |_, cx| cx.new(|_| Probe::default()))
        });
        cx.run_until_parked();

        assert!(cx.windows().contains(&window));
        assert!(window.downcast::<Root>().is_some());
        assert_eq!(probe.read_with(cx, |probe, _| probe.renders), 1);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{
        AnyWindowHandle, Bounds, Pixels, TestAppContext, VisualTestContext, WindowBounds, point,
        px, size,
    };

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi},
        app::{POLL_EVERY, Screen},
        test_support::{
            self, StateDir, answer_polls, app_view, close, config_json, count, open_shell,
            record_tray, saving_to, status_json,
        },
        tray::{CLOSED_POLL_EVERY, TRAY_HINT, TrayColour},
        window_state::{self, WindowState},
    };

    /// Past the time in which the window's bounds are still the open's own.
    const SETTLED: Duration = Duration::from_secs(3);

    fn window_bounds(cx: &mut TestAppContext, window: AnyWindowHandle) -> WindowBounds {
        cx.update_window(window, |_, window, _| window.window_bounds())
            .expect("the window is open")
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[gpui::test]
    fn closing_the_window_keeps_the_app(cx: &mut TestAppContext) {
        // Arrange: a saved window that has shown the tray hint, resized after it opened
        let fake = FakeApi::new();
        answer_polls(&fake);
        let dir = StateDir::new("close");
        let path = dir.file();
        let before = WindowState {
            x: Some(100.),
            y: Some(50.),
            width: Some(900.),
            height: Some(700.),
            maximized: false,
            tray_hint_shown: true,
        };
        window_state::save(&path, &before).unwrap();
        let shell = saving_to(fake.clone(), &path);
        let (_, window) = open_shell(cx, &shell, None);
        let (show, signals) = async_channel::unbounded();
        cx.update(|cx| serve_show_signal(cx, shell.clone(), signals));
        cx.executor().advance_clock(SETTLED);
        cx.simulate_window_resize(window, size(px(1000.), px(750.)));

        // Act
        close(cx, window);
        let polled = count(&fake, "GET", "/api/events");
        cx.executor().advance_clock(Duration::from_secs(10));

        // Assert
        assert!(cx.windows().is_empty(), "the window's close callback removed it");
        assert_eq!(count(&fake, "GET", "/api/events"), polled, "the closed window's dashboard polls no more");
        assert_eq!(
            window_state::load(&path),
            WindowState {
                width: Some(1000.),
                height: Some(750.),
                ..before
            }
        );
        show.try_send(()).unwrap();
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1, "the app still answers the show signal");
    }

    #[gpui::test]
    fn the_close_callback_refuses_windows_own_close(cx: &mut TestAppContext) {
        // Arrange
        let (_, window) = open_shell(cx, &test_support::shell(FakeApi::new()), None);

        // Act
        let native_close = cx
            .update_window(window, |_, window, cx| close_main_window(window, cx))
            .expect("the window is open");
        cx.run_until_parked();

        // Assert
        assert!(!native_close, "GPUI closes the window, never Windows' own close");
        assert!(cx.windows().is_empty());
    }

    #[gpui::test]
    fn the_show_signal_opens_a_fresh_window_with_its_forms_loaded_again(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", Ok(config_json()));
        let shell = test_support::shell(fake.clone());
        let (_, first) = open_shell(cx, &shell, None);
        let (show, signals) = async_channel::unbounded();
        cx.update(|cx| serve_show_signal(cx, shell.clone(), signals));
        close(cx, first);
        let loaded = count(&fake, "GET", "/api/config");

        // Act
        show.try_send(()).unwrap();
        cx.run_until_parked();

        // Assert
        let windows = cx.windows();
        assert_eq!(windows.len(), 1);
        assert_ne!(windows[0], first);
        let app = app_view(cx, windows[0]);
        assert_eq!(app.read_with(cx, |app, _| app.screen()), Screen::Dashboard);
        assert!(loaded > 0);
        assert!(
            count(&fake, "GET", "/api/config") > loaded,
            "the fresh window's forms read the config again"
        );
    }

    #[gpui::test]
    fn the_show_signal_brings_the_open_window_forward(cx: &mut TestAppContext) {
        // Arrange
        let shell = test_support::shell(FakeApi::new());
        let (_, window) = open_shell(cx, &shell, None);
        let (show, signals) = async_channel::unbounded();
        cx.update(|cx| serve_show_signal(cx, shell.clone(), signals));
        VisualTestContext::from_window(window, cx).deactivate_window();
        assert_eq!(cx.update(|cx| cx.active_window()), None);

        // Act
        show.try_send(()).unwrap();
        cx.run_until_parked();

        // Assert
        assert_eq!(cx.windows(), vec![window]);
        assert_eq!(cx.update(|cx| cx.active_window()), Some(window));
    }

    #[gpui::test]
    fn opens_where_the_window_was_saved(cx: &mut TestAppContext) {
        // Arrange
        let dir = StateDir::new("saved");
        let path = dir.file();
        let saved = WindowState {
            x: Some(100.),
            y: Some(50.),
            width: Some(900.),
            height: Some(700.),
            ..Default::default()
        };
        window_state::save(&path, &saved).unwrap();

        // Act
        let (_, window) = open_shell(cx, &saving_to(FakeApi::new(), &path), None);

        // Assert
        assert_eq!(
            window_bounds(cx, window),
            WindowBounds::Windowed(rect(100., 50., 900., 700.))
        );
    }

    #[gpui::test]
    fn opens_centred_on_the_primary_display_with_nothing_saved(cx: &mut TestAppContext) {
        // Arrange: the test platform's one display is 1920 × 1080
        let dir = StateDir::new("nothing-saved");
        let path = dir.file();

        // Act
        let (_, window) = open_shell(cx, &saving_to(FakeApi::new(), &path), None);

        // Assert
        assert_eq!(
            window_bounds(cx, window),
            WindowBounds::Windowed(rect(320., 140., 1280., 800.))
        );
        assert!(!path.exists(), "opening writes nothing");
    }

    #[gpui::test]
    fn a_resize_is_saved_a_second_after_it_ends(cx: &mut TestAppContext) {
        // Arrange
        let dir = StateDir::new("resize");
        let path = dir.file();
        let (_, window) = open_shell(cx, &saving_to(FakeApi::new(), &path), None);
        cx.executor().advance_clock(SETTLED);

        // Act
        cx.simulate_window_resize(window, size(px(900.), px(700.)));
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.simulate_window_resize(window, size(px(1000.), px(750.)));
        cx.executor().advance_clock(Duration::from_millis(900));
        let early = path.exists();
        cx.executor().advance_clock(Duration::from_millis(200));

        // Assert
        assert!(!early, "the second resize starts the second again");
        let saved = window_state::load(&path);
        assert_eq!((saved.width, saved.height), (Some(1000.), Some(750.)));
    }

    #[gpui::test]
    fn bounds_right_after_the_open_are_not_saved(cx: &mut TestAppContext) {
        // Arrange
        let dir = StateDir::new("right-after-open");
        let path = dir.file();
        let (_, window) = open_shell(cx, &saving_to(FakeApi::new(), &path), None);

        // Act
        cx.simulate_window_resize(window, size(px(900.), px(700.)));
        cx.executor().advance_clock(Duration::from_secs(5));

        // Assert
        assert!(!path.exists());
    }

    /// The window that opened last.
    fn reopen(cx: &mut TestAppContext, show: &async_channel::Sender<()>) -> AnyWindowHandle {
        show.try_send(()).unwrap();
        cx.run_until_parked();
        *cx.windows().last().expect("a window opened")
    }

    #[gpui::test]
    fn the_tray_hint_shows_on_the_first_close_only(cx: &mut TestAppContext) {
        // Arrange: an install whose window has never closed to the tray
        let fake = FakeApi::new();
        answer_polls(&fake);
        let dir = StateDir::new("hint-first");
        let path = dir.file();
        let shell = saving_to(fake, &path);
        let tray = record_tray(&shell);
        let (_, first) = open_shell(cx, &shell, None);
        let (show, signals) = async_channel::unbounded();
        cx.update(|cx| serve_show_signal(cx, shell.clone(), signals));

        // Act
        close(cx, first);
        let second = reopen(cx, &show);
        close(cx, second);

        // Assert
        assert_eq!(
            tray.borrow().notes,
            vec![("StreamLoop".to_string(), TRAY_HINT.to_string())]
        );
        assert!(window_state::load(&path).tray_hint_shown);
    }

    #[gpui::test]
    fn a_tray_hint_shown_before_is_not_shown_again(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        let dir = StateDir::new("hint-shown");
        let path = dir.file();
        let shown = WindowState {
            tray_hint_shown: true,
            ..Default::default()
        };
        window_state::save(&path, &shown).unwrap();
        let shell = saving_to(fake, &path);
        let tray = record_tray(&shell);
        let (_, window) = open_shell(cx, &shell, None);

        // Act
        close(cx, window);

        // Assert
        assert!(tray.borrow().notes.is_empty());
    }

    #[gpui::test]
    fn without_a_root_the_tray_hint_shows_once_per_run(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        let shell = test_support::shell(fake);
        let tray = record_tray(&shell);
        let (_, first) = open_shell(cx, &shell, None);
        let (show, signals) = async_channel::unbounded();
        cx.update(|cx| serve_show_signal(cx, shell.clone(), signals));

        // Act
        close(cx, first);
        let second = reopen(cx, &show);
        close(cx, second);

        // Assert
        assert_eq!(tray.borrow().notes.len(), 1);
    }

    #[gpui::test]
    fn the_tray_follows_the_poll_while_the_window_is_closed(cx: &mut TestAppContext) {
        // Arrange: the window showed a stopped stream and closed
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/status", Ok(status_json("stopped", false)));
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);
        let (_, window) = open_shell(cx, &shell, None);
        close(cx, window);
        let events = count(&fake, "GET", "/api/events");

        // Act
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        cx.executor().advance_clock(CLOSED_POLL_EVERY);
        let live = tray.borrow().now().0;
        let live_tip = tray.borrow().now().1.to_string();
        fake.reply("GET", "/api/status", Ok(status_json("off-air", false)));
        cx.executor().advance_clock(CLOSED_POLL_EVERY);
        let off_air = tray.borrow().now().0;
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(CLOSED_POLL_EVERY);
        let gone = tray.borrow().now().0;

        // Assert
        assert_eq!(live, TrayColour::Green);
        assert_eq!(live_tip, "StreamLoop: Live");
        assert_eq!(off_air, TrayColour::Red);
        assert_eq!(gone, TrayColour::Red);
        assert_eq!(
            tray.borrow().now().1,
            "StreamLoop\nCan't reach StreamLoop. Retrying..."
        );
        assert_eq!(
            count(&fake, "GET", "/api/events"),
            events,
            "only the status is polled while the window is closed"
        );
    }

    #[gpui::test]
    fn the_closed_poll_stops_when_the_window_opens(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        let shell = test_support::shell(fake.clone());
        let _tray = record_tray(&shell);
        let (_, window) = open_shell(cx, &shell, None);
        let (show, signals) = async_channel::unbounded();
        cx.update(|cx| serve_show_signal(cx, shell.clone(), signals));
        close(cx, window);
        let closed_from = count(&fake, "GET", "/api/status");
        cx.executor().advance_clock(CLOSED_POLL_EVERY * 2);
        let closed_polls = count(&fake, "GET", "/api/status") - closed_from;

        // Act
        reopen(cx, &show);
        let (status, events) = (
            count(&fake, "GET", "/api/status"),
            count(&fake, "GET", "/api/events"),
        );
        cx.executor().advance_clock(POLL_EVERY * 5);

        // Assert
        assert_eq!(closed_polls, 2, "one poll every 5 s while closed");
        assert_eq!(
            count(&fake, "GET", "/api/status") - status,
            count(&fake, "GET", "/api/events") - events,
            "only the window polls while it is open"
        );
    }

    #[gpui::test]
    fn a_tray_start_polls_without_a_window(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);

        // Act
        cx.update(|cx| begin(cx, &shell, true));
        cx.run_until_parked();
        let at_start = tray.borrow().now().0;
        cx.executor().advance_clock(CLOSED_POLL_EVERY);

        // Assert
        assert!(cx.windows().is_empty());
        assert_eq!(at_start, TrayColour::Green);
        assert_eq!(count(&fake, "GET", "/api/status"), 2);
    }

    const SL4: &str = r"C:\Temp\sl4 Ö (x)";
    const RETRY_TIP: &str = "StreamLoop\nCan't reach StreamLoop. Retrying...";
    const HINT_TIP: &str =
        "StreamLoop\nStreamLoop did not start. Double-click START.bat in C:\\Temp\\sl4 Ö (x) to see why.";

    /// The app started START.bat in SL4 just now, as `run` records it.
    fn started_bat_now(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let at_ms = crate::app::now_ms(cx);
            set_started_bat(
                cx,
                Some(StartedBat {
                    root: PathBuf::from(SL4),
                    at_ms,
                }),
            );
        });
    }

    #[gpui::test]
    fn a_tray_start_turns_red_with_the_hint_30_s_after_it_started_start_bat(
        cx: &mut TestAppContext,
    ) {
        // Arrange: nothing answers
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        let shell = test_support::shell(fake);
        let tray = record_tray(&shell);
        started_bat_now(cx);

        // Act
        cx.update(|cx| begin(cx, &shell, true));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(29));
        let at_29_s = (tray.borrow().now().0, tray.borrow().now().1.to_string());
        cx.executor().advance_clock(Duration::from_secs(1));

        // Assert
        assert_eq!(at_29_s, (TrayColour::Grey, RETRY_TIP.to_string()));
        assert_eq!(tray.borrow().now(), (TrayColour::Red, HINT_TIP));
    }

    #[gpui::test]
    fn the_tray_turns_red_with_the_hint_while_the_window_is_closed(cx: &mut TestAppContext) {
        // Arrange: the window opened on Connecting and closed
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        let shell = test_support::shell(fake);
        let tray = record_tray(&shell);
        cx.update(gpui_kit::init);
        cx.update(crate::theme::init);
        started_bat_now(cx);
        cx.update(|cx| begin(cx, &shell, false));
        cx.run_until_parked();
        let window = cx.windows()[0];
        close(cx, window);

        // Act
        cx.executor().advance_clock(Duration::from_secs(30));

        // Assert
        assert_eq!(tray.borrow().now(), (TrayColour::Red, HINT_TIP));
    }

    #[gpui::test]
    fn the_first_answer_ends_the_start_hint(cx: &mut TestAppContext) {
        // Arrange: red with the hint
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);
        started_bat_now(cx);
        cx.update(|cx| begin(cx, &shell, true));
        cx.executor().advance_clock(Duration::from_secs(30));

        // Act
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        cx.executor().advance_clock(CLOSED_POLL_EVERY);
        let live = (tray.borrow().now().0, tray.borrow().now().1.to_string());
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(CLOSED_POLL_EVERY);

        // Assert
        assert_eq!(live, (TrayColour::Green, "StreamLoop: Live".to_string()));
        assert_eq!(tray.borrow().now(), (TrayColour::Red, RETRY_TIP));
    }

    #[gpui::test]
    fn an_app_that_did_not_start_start_bat_stays_grey_until_the_first_answer(
        cx: &mut TestAppContext,
    ) {
        // Arrange
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        let shell = test_support::shell(fake);
        let tray = record_tray(&shell);

        // Act
        cx.update(|cx| begin(cx, &shell, true));
        cx.executor().advance_clock(Duration::from_secs(120));

        // Assert
        assert_eq!(tray.borrow().now(), (TrayColour::Grey, RETRY_TIP));
    }

    #[gpui::test]
    fn closing_without_a_tray_icon_leaves_the_tray_hint_for_a_later_close(
        cx: &mut TestAppContext,
    ) {
        // Arrange: no tray icon
        let fake = FakeApi::new();
        answer_polls(&fake);
        let dir = StateDir::new("hint-no-icon");
        let path = dir.file();
        let shell = saving_to(fake, &path);
        let (_, window) = open_shell(cx, &shell, None);

        // Act
        close(cx, window);

        // Assert
        assert!(!window_state::load(&path).tray_hint_shown);
    }

    #[test]
    fn a_tray_icon_that_could_not_be_made_is_written_to_ui_log() {
        // Arrange
        let dir = std::env::temp_dir().join(format!("sl-ui-tray-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let shell = test_support::shell(FakeApi::new());
        let failed: anyhow::Result<crate::tray::Tray> =
            Err(anyhow::anyhow!("the shell refused the icon"));

        // Act
        attach_tray(&shell, failed, Some(&dir));

        // Assert
        let log = std::fs::read_to_string(dir.join("ui.log")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            log.trim_end()
                .ends_with("The tray icon could not be made: the shell refused the icon"),
            "{log:?}"
        );
        assert!(!shell.tray.has_icon());
    }

    #[gpui::test]
    fn open_from_the_tray_opens_the_window_or_brings_it_forward(cx: &mut TestAppContext) {
        // Arrange: a tray start, so no window is open
        let fake = FakeApi::new();
        answer_polls(&fake);
        let shell = test_support::shell(fake);
        cx.update(gpui_kit::init);
        cx.update(crate::theme::init);
        cx.update(|cx| begin(cx, &shell, true));

        // Act
        cx.update(|cx| on_tray_event(TrayEvent::Open, cx, &shell));
        cx.run_until_parked();
        let opened = cx.windows();
        VisualTestContext::from_window(opened[0], cx).deactivate_window();
        cx.update(|cx| on_tray_event(TrayEvent::Open, cx, &shell));
        cx.run_until_parked();

        // Assert
        assert_eq!(opened.len(), 1);
        assert_eq!(cx.windows(), opened, "a second Open opens no second window");
        assert_eq!(cx.update(|cx| cx.active_window()), Some(opened[0]));
    }

    /// A root of this test process alone, so parallel tests never share a lock.
    fn lock_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sl-ui-lock-{name}-{}", std::process::id()))
    }

    #[test]
    fn a_follower_waits_60_s_for_the_lock_and_other_starts_do_not_wait() {
        // Arrange
        let follower = UiOptions {
            follow: Some("3.2.1".into()),
            tray: true,
            ..Default::default()
        };
        let plain = UiOptions::default();

        // Act
        let waits = (lock_wait(&follower), lock_wait(&plain));

        // Assert
        assert_eq!(waits, (Duration::from_secs(60), Duration::ZERO));
    }

    #[test]
    fn a_follower_takes_the_lock_once_the_old_app_lets_go_and_never_signals_it() {
        // Arrange: the old app keeps the lock 300 ms longer and records every show signal
        let root = lock_root("follower");
        let Claim::Ours(old) = instance::claim(Some(&root), Duration::ZERO) else {
            panic!("the old app's claim is ours");
        };
        let (shown, show) = std::sync::mpsc::channel();
        old.on_show(move || {
            let _ = shown.send(());
        });
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(old);
        });
        let follower = UiOptions {
            root: Some(root),
            follow: Some("3.2.1".into()),
            ..Default::default()
        };

        // Act
        let lock = take_lock(&follower);

        // Assert
        letting_go.join().unwrap();
        assert!(lock.is_some(), "the follower took the lock");
        assert!(
            show.recv_timeout(Duration::from_millis(200)).is_err(),
            "the old app was never asked to show its window"
        );
    }

    #[test]
    fn a_second_start_asks_the_app_of_the_install_to_show_its_window() {
        // Arrange
        let root = lock_root("second");
        let Claim::Ours(first) = instance::claim(Some(&root), Duration::ZERO) else {
            panic!("the first app's claim is ours");
        };
        let (shown, show) = std::sync::mpsc::channel();
        first.on_show(move || {
            let _ = shown.send(());
        });
        let second = UiOptions {
            root: Some(root),
            ..Default::default()
        };

        // Act
        let lock = take_lock(&second);

        // Assert
        assert!(lock.is_none(), "the second start exits");
        assert_eq!(show.recv_timeout(Duration::from_secs(5)), Ok(()));
        drop(first);
    }

    #[gpui::test]
    fn saving_on_demand_writes_where_the_window_is_now(cx: &mut TestAppContext) {
        // Arrange
        let dir = StateDir::new("on-demand");
        let path = dir.file();
        let (_, window) = open_shell(cx, &saving_to(FakeApi::new(), &path), None);
        cx.executor().advance_clock(SETTLED);
        cx.simulate_window_resize(window, size(px(900.), px(700.)));

        // Act
        cx.update(save_window_state);

        // Assert
        let saved = window_state::load(&path);
        assert_eq!((saved.width, saved.height), (Some(900.), Some(700.)));
    }
}
