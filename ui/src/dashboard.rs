//! The dashboard (index.html `#view-dashboard`): the banners, the header and the four tabs.

use std::{cell::RefCell, rc::Rc};

use gpui::{
    AnyElement, AnyView, App, AppContext, Bounds, Context, Element, ElementId, Entity, FocusHandle,
    FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Point,
    Render, ScrollHandle, WeakFocusHandle, Window, div, point, prelude::*, px, rgb,
    transparent_black,
};
use gpui_kit::component::{
    Selectable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    label::Label,
};

use crate::{
    api::types::{Event, Status, UpdateStatus},
    app::AppHandle,
    logic::admin::format_duration,
    monitor::MonitorView,
    playback::PlaybackView,
    settings::SettingsView,
    theme::{BORDER, MAX_WIDTH, MUTED, RED, SURFACE, SURFACE_HOVER, TEXT, WHITE},
    update::UpdateFlow,
    webhooks::WebhooksView,
    widgets::reveal_on_focus,
};

const LOST_TEXT: &str = "Can't reach the server. The dashboard may be out of date.";

/// Room left between a control brought into view and the edge of the column.
const REVEAL_MARGIN: Pixels = px(8.);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Monitor,
    Playback,
    Webhooks,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Monitor, Tab::Playback, Tab::Webhooks, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Monitor => "Monitor",
            Tab::Playback => "Playback",
            Tab::Webhooks => "Webhooks",
            Tab::Settings => "Settings",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Tab::Monitor => "tab-monitor",
            Tab::Playback => "tab-playback",
            Tab::Webhooks => "tab-webhooks",
            Tab::Settings => "tab-settings",
        }
    }
}

/// A tab's view. `DashboardView` calls the hooks; each does nothing unless the tab overrides it.
pub trait TabView: Render {
    /// admin.js `showDashboard()`: the dashboard opened, with every tab built fresh.
    fn on_dashboard_open(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// admin.js `switchTab()`: the user picked this tab, also when it was showing already.
    fn on_tab_shown(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    /// Every poll that answered, whether this tab shows or not.
    fn on_poll(
        &mut self,
        _status: &Status,
        _events: &[Event],
        _update: Option<&UpdateStatus>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    /// admin.js `handleSettingsSave()` reloading `loadPlaylistSelector()`: a save may have changed the playlists.
    fn on_playlists_saved(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

/// A tab's entity with its type erased, so the dashboard keeps all four in one list.
trait AnyTab {
    fn view(&self) -> AnyView;
    fn dashboard_open(&self, window: &mut Window, cx: &mut App);
    fn shown(&self, window: &mut Window, cx: &mut App);
    fn poll(
        &self,
        status: &Status,
        events: &[Event],
        update: Option<&UpdateStatus>,
        window: &mut Window,
        cx: &mut App,
    );
    fn playlists_saved(&self, window: &mut Window, cx: &mut App);
}

impl<T: TabView> AnyTab for Entity<T> {
    fn view(&self) -> AnyView {
        self.clone().into()
    }

    fn dashboard_open(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |tab, cx| tab.on_dashboard_open(window, cx));
    }

    fn shown(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |tab, cx| tab.on_tab_shown(window, cx));
    }

    fn poll(
        &self,
        status: &Status,
        events: &[Event],
        update: Option<&UpdateStatus>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.update(cx, |tab, cx| {
            tab.on_poll(status, events, update, window, cx)
        });
    }

    fn playlists_saved(&self, window: &mut Window, cx: &mut App) {
        self.update(cx, |tab, cx| tab.on_playlists_saved(window, cx));
    }
}

fn build_tab(
    tab: Tab,
    app: &AppHandle,
    flow: &Entity<UpdateFlow>,
    window: &mut Window,
    cx: &mut Context<DashboardView>,
) -> Box<dyn AnyTab> {
    match tab {
        Tab::Monitor => Box::new(cx.new(|cx| MonitorView::new(app.clone(), window, cx))),
        Tab::Playback => Box::new(cx.new(|cx| PlaybackView::new(app.clone(), window, cx))),
        Tab::Webhooks => Box::new(cx.new(|cx| WebhooksView::new(app.clone(), window, cx))),
        Tab::Settings => {
            Box::new(cx.new(|cx| SettingsView::new(app.clone(), flow.clone(), window, cx)))
        }
    }
}

pub struct DashboardView {
    tabs: Vec<(Tab, Box<dyn AnyTab>)>,
    /// The update banner, which Settings' Update Now shares.
    flow: Entity<UpdateFlow>,
    selected: Tab,
    uptime_ms: Option<u64>,
    connection_lost: bool,
    scroll: ScrollHandle,
    /// The focus the column last followed.
    followed: Rc<RefCell<Option<WeakFocusHandle>>>,
}

impl DashboardView {
    pub fn new(app: &AppHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_tabs(app, build_tab, window, cx)
    }

    fn with_tabs(
        app: &AppHandle,
        build: impl Fn(
            Tab,
            &AppHandle,
            &Entity<UpdateFlow>,
            &mut Window,
            &mut Context<Self>,
        ) -> Box<dyn AnyTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let flow = cx.new(|cx| UpdateFlow::new(app.clone(), cx));
        let tabs: Vec<_> = Tab::ALL
            .into_iter()
            .map(|tab| (tab, build(tab, app, &flow, window, cx)))
            .collect();
        for (_, tab) in &tabs {
            tab.dashboard_open(window, cx);
        }
        Self {
            tabs,
            flow,
            selected: Tab::Monitor,
            uptime_ms: None,
            connection_lost: false,
            scroll: ScrollHandle::new(),
            followed: Rc::default(),
        }
    }

    /// How far the column is scrolled down.
    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> Pixels {
        -self.scroll.offset().y
    }

    pub fn selected_tab(&self) -> Tab {
        self.selected
    }

    /// The view of `tab`, so a test can read that tab's own state.
    #[cfg(test)]
    pub(crate) fn tab_view(&self, tab: Tab) -> AnyView {
        self.tabs
            .iter()
            .find(|(t, _)| *t == tab)
            .map(|(_, view)| view.view())
            .expect("every tab is built")
    }

    pub fn select_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = tab;
        if let Some((_, view)) = self.tabs.iter().find(|(t, _)| *t == tab) {
            view.shown(window, cx);
        }
        cx.notify();
    }

    pub fn on_poll(
        &mut self,
        status: &Status,
        events: &[Event],
        update: Option<&UpdateStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.uptime_ms = Some(status.uptime_ms);
        // admin.js keeps the last banner when the update status did not answer.
        if let Some(update) = update {
            self.flow
                .update(cx, |flow, cx| flow.show_status(update, cx));
        }
        for (_, tab) in &self.tabs {
            tab.poll(status, events, update, window, cx);
        }
        cx.notify();
    }

    pub fn on_playlists_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (_, tab) in &self.tabs {
            tab.playlists_saved(window, cx);
        }
    }

    pub fn set_connection_lost(&mut self, lost: bool, cx: &mut Context<Self>) {
        if self.connection_lost != lost {
            self.connection_lost = lost;
            cx.notify();
        }
    }

    fn render_header(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .mb(px(32.))
            .child(
                Label::new("StreamLoop")
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD),
            )
            .children(self.uptime_ms.map(|ms| {
                Label::new(format!("Uptime: {}", format_duration(ms)))
                    .text_size(px(12.))
                    .text_color(rgb(MUTED))
            }))
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .gap_1()
            .mb(px(20.))
            .children(Tab::ALL.map(|tab| {
                let active = tab == self.selected;
                reveal_on_focus(
                    Button::new(tab.id())
                        .label(tab.label())
                        .custom(tab_colors(active, cx))
                        .selected(active)
                        .px_4()
                        .border_1()
                        .border_color(if active {
                            rgb(BORDER).into()
                        } else {
                            transparent_black()
                        })
                        .on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.select_tab(tab, window, cx)
                            }),
                        ),
                )
            }))
    }
}

/// admin.css `.tab` and `.tab.active`.
fn tab_colors(active: bool, cx: &App) -> ButtonCustomVariant {
    let hsla = |color: u32| -> Hsla { rgb(color).into() };
    if active {
        ButtonCustomVariant::new(cx)
            .color(hsla(SURFACE))
            .foreground(hsla(TEXT))
            .hover(hsla(SURFACE))
            .active(hsla(SURFACE))
    } else {
        ButtonCustomVariant::new(cx)
            .color(transparent_black())
            .foreground(hsla(MUTED))
            .hover(hsla(SURFACE_HOVER))
            .active(hsla(SURFACE_HOVER))
    }
}

/// The scroll offset that brings `target` fully into `viewport` with `margin` to spare, or `None`
/// when it is fully in view already.
fn reveal_offset(
    viewport: Bounds<Pixels>,
    offset: Point<Pixels>,
    max_offset: Point<Pixels>,
    target: Bounds<Pixels>,
    margin: Pixels,
) -> Option<Point<Pixels>> {
    if target.top() >= viewport.top() && target.bottom() <= viewport.bottom() {
        return None;
    }
    let (top, bottom) = (viewport.top() + margin, viewport.bottom() - margin);
    let delta = if target.top() < top || target.size.height > bottom - top {
        top - target.top()
    } else {
        bottom - target.bottom()
    };
    let y = (offset.y + delta).clamp(-max_offset.y, px(0.));
    (y != offset.y).then_some(point(offset.x, y))
}

/// The dashboard's scrolling column. When the keyboard moves the focus, it scrolls to the bounds the
/// focused control asked for with `Window::request_autoscroll`; other frames leave the wheel free.
struct FollowFocus {
    column: AnyElement,
    scroll: ScrollHandle,
    followed: Rc<RefCell<Option<WeakFocusHandle>>>,
}

impl IntoElement for FollowFocus {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for FollowFocus {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.column.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.take_autoscroll();
        self.column.prepaint(window, cx);
        let target = window.take_autoscroll();
        let focused = window.focused(cx);
        let moved = {
            let mut followed = self.followed.borrow_mut();
            let moved = match (followed.as_ref(), focused.as_ref()) {
                (Some(before), Some(now)) => before != now,
                (None, None) => false,
                _ => true,
            };
            *followed = focused.as_ref().map(FocusHandle::downgrade);
            moved
        };
        if !moved || !window.last_input_was_keyboard() {
            return;
        }
        let Some(target) = target else {
            return;
        };
        let scroll = &self.scroll;
        if let Some(offset) = reveal_offset(
            scroll.bounds(),
            scroll.offset(),
            scroll.max_offset(),
            target,
            REVEAL_MARGIN,
        ) {
            scroll.set_offset(offset);
            // This frame is laid out already; the next one draws the column at its new offset.
            window.request_animation_frame();
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.column.paint(window, cx);
    }
}

fn lost_banner() -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .justify_center()
        .px_4()
        .py(px(10.))
        .bg(rgb(RED))
        .child(
            Label::new(LOST_TEXT)
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(WHITE)),
        )
}

impl Render for DashboardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = self
            .tabs
            .iter()
            .find(|(tab, _)| *tab == self.selected)
            .map(|(_, view)| view.view());
        let column = div()
            .id("dashboard")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .when(self.connection_lost, |d| d.child(lost_banner()))
            .child(self.flow.clone())
            .child(
                div().w_full().flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(MAX_WIDTH))
                        .px_4()
                        .py_6()
                        .child(self.render_header())
                        .child(self.render_tabs(cx))
                        .children(panel),
                ),
            );
        FollowFocus {
            column: column.into_any_element(),
            scroll: self.scroll.clone(),
            followed: self.followed.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{AppContext, TestAppContext, WindowOptions, point, size};
    use gpui_kit::test::TestWindowExt;

    use super::*;
    use crate::{api::fake::FakeApi, test_support::open_app};

    struct Recorder {
        tab: Tab,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Render for Recorder {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    impl TabView for Recorder {
        fn on_dashboard_open(&mut self, _: &mut Window, _: &mut Context<Self>) {
            self.log.borrow_mut().push(format!("open {:?}", self.tab));
        }

        fn on_tab_shown(&mut self, _: &mut Window, _: &mut Context<Self>) {
            self.log.borrow_mut().push(format!("shown {:?}", self.tab));
        }

        fn on_playlists_saved(&mut self, _: &mut Window, _: &mut Context<Self>) {
            self.log.borrow_mut().push(format!("saved {:?}", self.tab));
        }

        fn on_poll(
            &mut self,
            status: &Status,
            events: &[Event],
            update: Option<&UpdateStatus>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) {
            let line = format!(
                "poll {:?} {} {} {}",
                self.tab,
                status.uptime_ms,
                events.len(),
                update.is_some()
            );
            self.log.borrow_mut().push(line);
        }
    }

    type Log = Rc<RefCell<Vec<String>>>;

    fn recording(
        log: Log,
    ) -> impl Fn(
        Tab,
        &AppHandle,
        &Entity<UpdateFlow>,
        &mut Window,
        &mut Context<DashboardView>,
    ) -> Box<dyn AnyTab> {
        move |tab, _, _, _, cx| {
            Box::new(cx.new(|_| Recorder {
                tab,
                log: log.clone(),
            }))
        }
    }

    #[gpui::test]
    fn calls_the_tab_hooks(cx: &mut TestAppContext) {
        let (app, _) = open_app(cx, FakeApi::new());
        let handle = app.read_with(cx, |app, _| app.handle());
        let log = Log::default();

        let (window, dashboard) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                    cx.new(|cx| {
                        DashboardView::with_tabs(&handle, recording(log.clone()), window, cx)
                    })
                })
            })
            .unwrap();
        assert_eq!(
            *log.borrow(),
            [
                "open Monitor",
                "open Playback",
                "open Webhooks",
                "open Settings"
            ]
        );
        assert_eq!(
            dashboard.read_with(cx, |dashboard, _| dashboard.selected_tab()),
            Tab::Monitor
        );
        log.borrow_mut().clear();

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("tab-settings", cx);
            let status = Status {
                uptime_ms: 5000,
                ..Status::default()
            };
            dashboard.update(cx, |dashboard, cx| {
                dashboard.on_poll(&status, &[Event::default()], None, window, cx)
            });
        })
        .unwrap();

        assert_eq!(
            dashboard.read_with(cx, |dashboard, _| dashboard.selected_tab()),
            Tab::Settings
        );
        assert_eq!(
            *log.borrow(),
            [
                "shown Settings",
                "poll Monitor 5000 1 false",
                "poll Playback 5000 1 false",
                "poll Webhooks 5000 1 false",
                "poll Settings 5000 1 false",
            ]
        );
    }

    #[test]
    fn reveal_offset_scrolls_only_as_far_as_needed() {
        let viewport = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.)));
        let max = point(px(0.), px(3000.));
        let at =
            |y: f32, height: f32| Bounds::new(point(px(10.), px(y)), size(px(50.), px(height)));
        let reveal = |offset: f32, target| {
            reveal_offset(viewport, point(px(0.), px(offset)), max, target, px(8.))
                .map(|offset| offset.y)
        };

        // Fully in view, however close to an edge: nothing moves.
        assert_eq!(reveal(-500., at(0., 30.)), None);
        assert_eq!(reveal(-500., at(770., 30.)), None);
        // Below: its bottom lands 8 px above the viewport's.
        assert_eq!(reveal(-500., at(790., 30.)), Some(px(-500. - 28.)));
        assert_eq!(reveal(-500., at(900., 30.)), Some(px(-500. - 138.)));
        // Above: its top lands 8 px below the viewport's.
        assert_eq!(reveal(-500., at(-100., 30.)), Some(px(-500. + 108.)));
        // Taller than the viewport: its top.
        assert_eq!(reveal(-500., at(900., 900.)), Some(px(-500. - 892.)));
        // Never past either end.
        assert_eq!(reveal(-2990., at(900., 30.)), Some(px(-3000.)));
        assert_eq!(reveal(-20., at(-100., 30.)), Some(px(0.)));
    }

    #[gpui::test]
    fn every_tab_hears_a_playlist_save_from_another_view(cx: &mut TestAppContext) {
        let (app, window) = open_app(cx, FakeApi::new());
        let handle = app.read_with(cx, |app, _| app.handle());
        let log = Log::default();
        cx.update_window(window, |_, window, cx| {
            let dashboard =
                cx.new(|cx| DashboardView::with_tabs(&handle, recording(log.clone()), window, cx));
            app.update(cx, |app, cx| app.open_dashboard(dashboard, cx));
        })
        .unwrap();

        cx.update(|cx| handle.playlists_saved(cx));
        cx.run_until_parked();

        let saved: Vec<String> = log
            .borrow()
            .iter()
            .filter(|line| line.starts_with("saved"))
            .cloned()
            .collect();
        assert_eq!(
            saved,
            [
                "saved Monitor",
                "saved Playback",
                "saved Webhooks",
                "saved Settings"
            ]
        );
    }
}
