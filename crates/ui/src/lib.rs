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
pub mod wizard;

use std::{path::PathBuf, sync::Arc, time::Duration};

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, Render, Result, TitlebarOptions, Window,
    WindowBounds, WindowOptions, px, size,
};
use streamloop_common::{config, paths::Layout};

use crate::{
    api::{Api, ApiError, client::Client, http::HttpApi},
    app::AppView,
    instance::Claim,
    start::StartedBat,
};

/// What `streamloop.exe ui` was asked for. `root` is absolute; `follow` is read by a later stage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiOptions {
    pub root: Option<PathBuf>,
    pub port: Option<u16>,
    pub tray: bool,
    pub follow: Option<String>,
}

/// Takes the install's lock, starts `START.bat` when nothing answers and opens the window unless `--tray`,
/// then runs until it closes. A second app of the install asks the first to show its window and returns.
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
    let tray = opts.tray;
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            theme::init(cx);
            let client = Client(api);
            let window = (!tray).then(|| {
                open_main_window(cx, |window, cx| {
                    cx.new(|cx| AppView::new(client, started_bat, window, cx))
                })
                .expect("open the StreamLoop window")
                .0
            });
            cx.spawn(async move |cx| {
                while show_rx.recv().await.is_ok() {
                    if let Some(window) = window {
                        let _ = cx.update_window(window, |_, window, _| window.activate_window());
                    }
                }
            })
            .detach();
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    drop(instance);
}

/// The StreamLoop window, with `build`'s view inside gpui-component's `Root`.
fn open_main_window<V: Render>(
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<(AnyWindowHandle, Entity<V>)> {
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(720.), px(560.))),
        titlebar: Some(TitlebarOptions {
            title: Some("StreamLoop".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, build)
}

#[cfg(test)]
mod harness {
    use gpui::{Context, IntoElement, Render, TestAppContext, Window, div, prelude::*};
    use gpui_kit::component::Root;

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

        let (window, probe) = cx.update(|cx| {
            super::open_main_window(cx, |_, cx| cx.new(|_| Probe::default()))
                .expect("open the window")
        });
        cx.run_until_parked();

        assert!(cx.windows().contains(&window));
        assert!(window.downcast::<Root>().is_some());
        assert_eq!(probe.read_with(cx, |probe, _| probe.renders), 1);
    }
}
