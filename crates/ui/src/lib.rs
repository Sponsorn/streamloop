//! StreamLoop's dashboard as a native GPUI window, on gpui-kit.

// GPUI's macros expand to `gpui::` paths; this resolves them through the Kit.
extern crate gpui_kit as gpui;

const DEFAULT_PORT: u16 = 7654;

pub mod api;
pub mod app;
pub mod dashboard;
pub mod instance;
pub mod logic;
pub mod logs;
pub mod monitor;
pub mod playback;
pub mod settings;
pub mod setup_check;
pub mod start;
#[cfg(test)]
mod test_support;
pub mod theme;
pub mod update;
pub mod videos;
pub mod webhooks;
pub mod widgets;
pub mod window_state;
pub mod wizard;

use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, Global, Pixels, QuitMode, Render,
    Subscription, Task, TitlebarOptions, Window, WindowOptions, point, px,
};
use streamloop_common::{config, paths::Layout};

use crate::{
    api::{Api, ApiError, client::Client, http::HttpApi},
    app::{AppView, now_ms},
    instance::Claim,
    start::StartedBat,
    window_state::{DEFAULT_SIZE, MIN_SIZE, WindowState},
};

/// The window's state file in `ROOT\app\`.
const WINDOW_STATE: &str = "window.json";
/// For this long after an open the window's bounds are still the open's own: a window opened maximised
/// reads as windowed for about 1.5 s.
const OPEN_SETTLES_IN: Duration = Duration::from_secs(2);
/// A move or resize is saved once the window has stayed put this long.
const SAVE_AFTER_MOVE: Duration = Duration::from_secs(1);

/// What `streamloop.exe ui` was asked for. `root` is absolute; `follow` is read by a later stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiOptions {
    pub root: Option<PathBuf>,
    pub port: Option<u16>,
    pub tray: bool,
    pub follow: Option<String>,
}

/// What outlives a window. R3's `AppHandle` is the window's own.
#[derive(Clone)]
pub struct Shell {
    pub client: Client,
    pub root: Option<PathBuf>,
    /// `ROOT\app\window.json`; None without a root, and the window state then lives in memory for the run.
    pub state_path: Option<PathBuf>,
}

impl Shell {
    pub fn new(client: Client, root: Option<PathBuf>) -> Shell {
        let state_path = root
            .as_deref()
            .map(|root| Layout::new(root).app_dir.join(WINDOW_STATE));
        Shell {
            client,
            root,
            state_path,
        }
    }
}

/// Takes the install's lock, starts `START.bat` when nothing answers and opens the window unless `--tray`.
/// Closing the window leaves the app running; a second app of the install asks the first to show its window
/// and returns.
pub fn run(opts: UiOptions) {
    let root = opts.root.as_deref();
    let instance = match instance::claim(root, Duration::ZERO) {
        Claim::Ours(instance) => instance,
        Claim::Running => {
            if !opts.tray {
                instance::signal_show(root);
            }
            return;
        }
    };
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
    let shell = Shell::new(Client(api), opts.root.clone());
    let tray = opts.tray;
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            theme::init(cx);
            set_started_bat(cx, started_bat);
            if !tray {
                open_main_window(cx, &shell);
            }
            serve_show_signal(cx, shell, show_rx);
            cx.activate(true);
        });
    drop(instance);
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

/// Opens the StreamLoop window where it last was, with every view built anew.
pub fn open_main_window(cx: &mut App, shell: &Shell) -> AnyWindowHandle {
    let client = shell.client.clone();
    let started_bat = cx.default_global::<Starting>().0.take();
    open_window_of(cx, shell, |window, cx| {
        cx.new(|cx| AppView::new(client, started_bat, window, cx))
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
/// refused: on gpui-pre 0.3.11 that path leaks about 26 MB per close.
pub(crate) fn close_main_window(window: &mut Window, cx: &mut App) -> bool {
    remember(window, cx);
    let handle = window.window_handle();
    if let Some(main) = main_window(cx)
        && main.open.as_ref().is_some_and(|open| open.handle == handle)
    {
        main.open = None;
    }
    window.remove_window();
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
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    use gpui::{
        AnyWindowHandle, Bounds, Pixels, TestAppContext, VisualTestContext, WindowBounds, point,
        px, size,
    };

    use super::*;
    use crate::{
        api::fake::FakeApi,
        app::Screen,
        test_support::{self, answer_polls, app_view, close, config_json, count, open_shell},
        window_state::{self, WindowState},
    };

    /// Past the time in which the window's bounds are still the open's own.
    const SETTLED: Duration = Duration::from_secs(3);

    /// A folder of this test process alone for `window.json`, removed when the test ends.
    struct StateDir(PathBuf);

    impl StateDir {
        fn new(name: &str) -> StateDir {
            let dir =
                std::env::temp_dir().join(format!("sl-ui-lib-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            StateDir(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join("window.json")
        }
    }

    impl Drop for StateDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn saving_to(fake: Arc<FakeApi>, path: &Path) -> Shell {
        Shell {
            state_path: Some(path.to_path_buf()),
            ..test_support::shell(fake)
        }
    }

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
        let polled = count(&fake, "GET", "/api/status");
        cx.executor().advance_clock(Duration::from_secs(10));

        // Assert
        assert!(cx.windows().is_empty(), "the window's close callback removed it");
        assert_eq!(count(&fake, "GET", "/api/status"), polled, "the closed window polls no more");
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
