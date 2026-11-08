//! StreamLoop's dashboard as a native GPUI window, on gpui-kit.

// GPUI's macros expand to `gpui::` paths; this resolves them through the Kit.
extern crate gpui_kit as gpui;

pub mod api;
pub mod app;
pub mod dashboard;
pub mod logic;
pub mod logs;
pub mod monitor;
pub mod playback;
pub mod setup_check;
#[cfg(test)]
mod test_support;
pub mod theme;
pub mod videos;
pub mod widgets;

use std::sync::Arc;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, Render, Result, TitlebarOptions, Window, WindowBounds,
    WindowOptions, px, size,
};

use crate::{
    api::{client::Client, http::HttpApi},
    app::AppView,
};

/// Opens the StreamLoop window against the backend on `port` and runs until it is closed.
pub fn run(port: u16) {
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx: &mut App| {
        gpui_kit::init(cx);
        theme::init(cx);
        let client = Client(Arc::new(HttpApi::new(port)));
        open_main_window(cx, |window, cx| cx.new(|cx| AppView::new(client, window, cx)))
            .expect("open the StreamLoop window");
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
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
        titlebar: Some(TitlebarOptions { title: Some("StreamLoop".into()), ..Default::default() }),
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

        let (window, probe) =
            cx.update(|cx| super::open_main_window(cx, |_, cx| cx.new(|_| Probe::default())).expect("open the window"));
        cx.run_until_parked();

        assert!(cx.windows().contains(&window));
        assert!(window.downcast::<Root>().is_some());
        assert_eq!(probe.read_with(cx, |probe, _| probe.renders), 1);
    }
}
