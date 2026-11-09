//! The window's root view: the Connecting screen, the wizard or the dashboard, and what every view
//! shares: the dashboard's poll, toasts and confirm dialogs.

use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext, AsyncApp, Context, Entity, FocusHandle, FontWeight,
    IntoElement, PromptButton, PromptLevel, Render, SharedString, Subscription, Task, WeakEntity,
    Window, div, prelude::*, px, rgb,
};
use gpui_kit::component::label::Label;

use crate::{
    api::{
        ApiError,
        client::Client,
        types::{Event, Status, UpdateStatus},
    },
    dashboard::DashboardView,
    theme::{
        BG, ERROR_TEXT, FONT, GREEN, MUTED, TEXT, TOAST_ERROR_BG, TOAST_ERROR_BORDER,
        TOAST_SUCCESS_BG, TOAST_SUCCESS_BORDER,
    },
    wizard::WizardView,
};

/// admin.js polls every 2 s; the Connecting screen tries as often.
pub const POLL_EVERY: Duration = Duration::from_secs(2);
/// admin.js `showToast`.
pub const TOAST_FOR: Duration = Duration::from_secs(4);
/// Failed polls in a row that show the connection-lost banner and, while a full update runs, start
/// the restart wait.
const LOST_AFTER: u32 = 2;

const CONNECTING_TITLE: &str = "StreamLoop backend is not running";
const CONNECTING_HINT: &str =
    "Start it with START.bat. This window switches to the dashboard when the backend is up.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Connecting,
    Wizard,
    Dashboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Error,
    Success,
}

/// What the wizard and the tabs need from the app: the API client and the app's shared services.
/// Each call runs once the current update is done, so a view may call it from inside a hook.
#[derive(Clone)]
pub struct AppHandle {
    pub client: Client,
    view: WeakEntity<AppView>,
    window: AnyWindowHandle,
}

impl AppHandle {
    pub fn toast(&self, text: impl Into<SharedString>, kind: ToastKind, cx: &mut App) {
        let text = text.into();
        self.later(cx, move |app, _, cx| app.toast(text, kind, cx));
    }

    pub fn poll_now(&self, cx: &mut App) {
        self.later(cx, |app, _, cx| app.poll_now(cx));
    }

    pub fn start_over(&self, cx: &mut App) {
        self.later(cx, |app, _, cx| app.start_over(cx));
    }

    /// The wizard's "Go to Dashboard".
    pub fn show_dashboard(&self, cx: &mut App) {
        self.later(cx, |app, window, cx| app.show_dashboard(window, cx));
    }

    /// A Settings save went through: every tab's `on_playlists_saved` runs.
    pub fn playlists_saved(&self, cx: &mut App) {
        self.later(cx, |app, window, cx| {
            if let Body::Dashboard(dashboard) = &app.body {
                dashboard.update(cx, |dashboard, cx| dashboard.on_playlists_saved(window, cx));
            }
        });
    }

    pub fn confirm(
        &self,
        title: &str,
        detail: &str,
        ok: &str,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<bool> {
        confirm(title, detail, ok, window, cx)
    }

    fn later(
        &self,
        cx: &mut App,
        f: impl FnOnce(&mut AppView, &mut Window, &mut Context<AppView>) + 'static,
    ) {
        let (view, window) = (self.view.clone(), self.window);
        cx.defer(move |cx| {
            cx.update_window(window, |_, window, cx| {
                view.update(cx, |app, cx| f(app, window, cx))
            })
            .ok();
        });
    }
}

enum Body {
    Connecting,
    Wizard(Entity<WizardView>),
    Dashboard(Entity<DashboardView>),
}

struct Toast {
    id: u64,
    text: SharedString,
    kind: ToastKind,
}

type PollAnswer = (
    Result<Status, ApiError>,
    Result<Vec<Event>, ApiError>,
    Option<UpdateStatus>,
);

pub struct AppView {
    handle: AppHandle,
    body: Body,
    /// Holds the keyboard focus whenever no control does, so Tab always reaches the first control.
    focus: FocusHandle,
    failures: u32,
    toasts: Vec<Toast>,
    next_toast: u64,
    _focus_lost: Subscription,
    _connecting: Option<Task<()>>,
    _polling: Option<Task<()>>,
    _poll_once: Option<Task<()>>,
}

impl AppView {
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let handle = AppHandle {
            client,
            view: cx.weak_entity(),
            window: window.window_handle(),
        };
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let focus_lost = cx.on_focus_lost(window, |app, window, cx| app.focus.focus(window, cx));
        let mut view = Self {
            handle,
            body: Body::Connecting,
            focus,
            failures: 0,
            toasts: Vec::new(),
            next_toast: 0,
            _focus_lost: focus_lost,
            _connecting: None,
            _polling: None,
            _poll_once: None,
        };
        view.connect(cx);
        view
    }

    pub fn handle(&self) -> AppHandle {
        self.handle.clone()
    }

    pub fn screen(&self) -> Screen {
        match self.body {
            Body::Connecting => Screen::Connecting,
            Body::Wizard(_) => Screen::Wizard,
            Body::Dashboard(_) => Screen::Dashboard,
        }
    }

    pub fn dashboard(&self) -> Option<Entity<DashboardView>> {
        match &self.body {
            Body::Dashboard(dashboard) => Some(dashboard.clone()),
            _ => None,
        }
    }

    pub fn wizard(&self) -> Option<Entity<WizardView>> {
        match &self.body {
            Body::Wizard(wizard) => Some(wizard.clone()),
            _ => None,
        }
    }

    pub fn toast(
        &mut self,
        text: impl Into<SharedString>,
        kind: ToastKind,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast;
        self.next_toast += 1;
        self.toasts.push(Toast {
            id,
            text: text.into(),
            kind,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            this.update(cx, |app, cx| {
                app.toasts.retain(|t| t.id != id);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn confirm(
        &mut self,
        title: &str,
        detail: &str,
        ok: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        confirm(title, detail, ok, window, cx)
    }

    /// admin.js `pollOnce` outside the 2 s rhythm, as after an action. Only the dashboard polls.
    pub fn poll_now(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.body, Body::Dashboard(_)) {
            return;
        }
        let (client, window) = (self.handle.client.clone(), self.handle.window);
        self._poll_once = Some(cx.spawn(async move |this, cx| {
            poll(&this, &client, window, cx).await;
        }));
    }

    /// A page reload: the token is read again, then Connecting, then the wizard or the dashboard with every form loaded fresh.
    pub fn start_over(&mut self, cx: &mut Context<Self>) {
        self.handle.client.forget_token();
        self.connect(cx);
    }

    pub fn show_dashboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.handle.clone();
        let dashboard = cx.new(|cx| DashboardView::new(&app, window, cx));
        self.open_dashboard(dashboard, cx);
    }

    /// Shows `dashboard` and starts the poll.
    pub(crate) fn open_dashboard(
        &mut self,
        dashboard: Entity<DashboardView>,
        cx: &mut Context<Self>,
    ) {
        self.set_body(Body::Dashboard(dashboard), cx);
        let (client, window) = (self.handle.client.clone(), self.handle.window);
        self._polling = Some(cx.spawn(async move |this, cx| {
            while poll(&this, &client, window, cx).await {
                cx.background_executor().timer(POLL_EVERY).await;
            }
        }));
    }

    fn show_wizard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.handle.clone();
        let wizard = cx.new(|cx| WizardView::new(app, window, cx));
        self.set_body(Body::Wizard(wizard), cx);
    }

    /// Shows Connecting and tries `GET /api/status` every 2 s until it answers.
    fn connect(&mut self, cx: &mut Context<Self>) {
        self.set_body(Body::Connecting, cx);
        let (client, window) = (self.handle.client.clone(), self.handle.window);
        self._connecting = Some(cx.spawn(async move |this, cx| {
            loop {
                let api = client.clone();
                if let Ok(status) = cx.background_spawn(async move { api.status() }).await {
                    cx.update_window(window, |_, window, cx| {
                        this.update(cx, |app, cx| app.open_screen_for(&status, window, cx))
                    })
                    .ok();
                    return;
                }
                cx.background_executor().timer(POLL_EVERY).await;
            }
        }));
    }

    /// admin.js `init()`.
    fn open_screen_for(&mut self, status: &Status, window: &mut Window, cx: &mut Context<Self>) {
        if status.first_run {
            self.show_wizard(window, cx);
        } else {
            self.show_dashboard(window, cx);
        }
    }

    /// Replaces the screen and stops the old one's polling.
    fn set_body(&mut self, body: Body, cx: &mut Context<Self>) {
        self.body = body;
        self._polling = None;
        self._poll_once = None;
        self.failures = 0;
        cx.notify();
    }

    fn apply_poll(
        &mut self,
        (status, events, update): PollAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Body::Dashboard(dashboard) = &self.body else {
            return;
        };
        let dashboard = dashboard.clone();
        let (Ok(status), Ok(events)) = (status, events) else {
            self.failures += 1;
            if self.failures >= LOST_AFTER {
                dashboard.update(cx, |dashboard, cx| {
                    dashboard.set_connection_lost(true, cx);
                    dashboard.on_server_lost(cx);
                });
            }
            return;
        };
        if self.failures >= LOST_AFTER {
            // Node makes a new token at every start, and it may have restarted while it was away.
            self.handle.client.forget_token();
        }
        self.failures = 0;
        dashboard.update(cx, |dashboard, cx| {
            dashboard.set_connection_lost(false, cx);
            dashboard.on_poll(&status, &events, update.as_ref(), window, cx);
        });
    }

    fn render_toasts(&self) -> impl IntoElement {
        div()
            .absolute()
            .bottom(px(16.))
            .right(px(16.))
            .max_w(px(380.))
            .flex()
            .flex_col()
            .gap_2()
            .children(self.toasts.iter().map(|toast| {
                let (bg, border, text) = match toast.kind {
                    ToastKind::Error => (TOAST_ERROR_BG, TOAST_ERROR_BORDER, ERROR_TEXT),
                    ToastKind::Success => (TOAST_SUCCESS_BG, TOAST_SUCCESS_BORDER, GREEN),
                };
                div()
                    .px_4()
                    .py_3()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(border))
                    .bg(rgb(bg))
                    .child(
                        Label::new(toast.text.clone())
                            .text_size(px(14.))
                            .text_color(rgb(text)),
                    )
            }))
    }
}

/// One admin.js `pollOnce`: status and events must answer, the update status may fail.
/// False once the window is closed.
async fn poll(
    this: &WeakEntity<AppView>,
    client: &Client,
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
) -> bool {
    if cx.update_window(window, |_, _, _| ()).is_err() {
        return false;
    }
    let (a, b, c) = (client.clone(), client.clone(), client.clone());
    let status = cx.background_spawn(async move { a.status() });
    let events = cx.background_spawn(async move { b.events() });
    let update = cx.background_spawn(async move { c.update_status().ok() });
    let answer = (status.await, events.await, update.await);
    cx.update_window(window, |_, window, cx| {
        this.update(cx, |app, cx| app.apply_poll(answer, window, cx))
            .is_ok()
    })
    .unwrap_or(false)
}

/// A native dialog with `title`, `detail` and the buttons `ok` and Cancel; true only for `ok`. Escape answers Cancel.
fn confirm(title: &str, detail: &str, ok: &str, window: &mut Window, cx: &mut App) -> Task<bool> {
    let answer = window.prompt(
        PromptLevel::Warning,
        title,
        Some(detail),
        &[
            PromptButton::ok(ok.to_string()),
            PromptButton::cancel("Cancel"),
        ],
        cx,
    );
    cx.background_spawn(async move { matches!(answer.await, Ok(0)) })
}

fn connecting() -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .p_4()
        .text_center()
        .child(
            Label::new(CONNECTING_TITLE)
                .text_size(px(28.))
                .font_weight(FontWeight::SEMIBOLD),
        )
        .child(Label::new(CONNECTING_HINT).text_color(rgb(MUTED)))
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.body {
            Body::Connecting => connecting().into_any_element(),
            Body::Wizard(view) => view.clone().into_any_element(),
            Body::Dashboard(view) => view.clone().into_any_element(),
        };
        div()
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(FONT)
            .child(body)
            .child(self.render_toasts())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{TestAppContext, WindowOptions};
    use gpui_kit::test::TestWindowExt;

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi},
        dashboard::Tab,
        test_support::{answer_polls, count, events_json, open_app, shows, status_json},
        widgets::{ActionView, Busy, action_button, run_action},
    };

    const BANNER: &str = "Can't reach the server. The dashboard may be out of date.";

    fn screen(cx: &mut TestAppContext, app: &Entity<AppView>) -> Screen {
        app.read_with(cx, |app, _| app.screen())
    }

    #[gpui::test]
    fn shows_connecting_until_the_server_answers(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        let (app, window) = open_app(cx, fake.clone());

        assert_eq!(screen(cx, &app), Screen::Connecting);
        assert!(shows(cx, window, "StreamLoop backend is not running"));
        assert!(shows(
            cx,
            window,
            "Start it with START.bat. This window switches to the dashboard when the backend is up."
        ));

        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        cx.executor().advance_clock(POLL_EVERY);

        assert_eq!(screen(cx, &app), Screen::Dashboard);
        assert!(!shows(cx, window, "StreamLoop backend is not running"));
    }

    #[gpui::test]
    fn opens_the_wizard_on_a_first_run(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("off-air", true)));
        let (app, _) = open_app(cx, fake.clone());

        cx.executor().advance_clock(Duration::from_secs(10));

        assert_eq!(screen(cx, &app), Screen::Wizard);
        assert_eq!(count(&fake, "GET", "/api/events"), 0);
    }

    #[gpui::test]
    fn polls_every_two_seconds(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        let (app, _) = open_app(cx, fake.clone());
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        let paths = ["/api/status", "/api/events", "/api/update/status"];
        let before = paths.map(|p| count(&fake, "GET", p));

        cx.executor().advance_clock(Duration::from_secs(6));

        let after = paths.map(|p| count(&fake, "GET", p));
        assert_eq!(after, before.map(|n| n + 3));
    }

    #[gpui::test]
    fn shows_the_banner_after_two_failed_polls(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        fake.reply("GET", "/api/events", Ok(events_json(3)));
        // `/api/update/status` stays unanswered throughout: admin.js ignores it, so it never fails a poll.
        let (_, window) = open_app(cx, fake.clone());
        assert!(!shows(cx, window, BANNER));

        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(POLL_EVERY);
        assert!(!shows(cx, window, BANNER));
        cx.executor().advance_clock(POLL_EVERY);
        assert!(shows(cx, window, BANNER));
        assert_eq!(fake.token_forgets(), 0);

        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        cx.executor().advance_clock(POLL_EVERY);
        assert!(!shows(cx, window, BANNER));
        assert_eq!(fake.token_forgets(), 1);
    }

    #[gpui::test]
    fn hides_a_toast_after_four_seconds(cx: &mut TestAppContext) {
        let (app, window) = open_app(cx, FakeApi::new());

        app.update(cx, |app, cx| app.toast("Saved.", ToastKind::Success, cx));
        assert!(shows(cx, window, "Saved."));

        cx.executor().advance_clock(Duration::from_millis(3900));
        assert!(shows(cx, window, "Saved."));
        cx.executor().advance_clock(Duration::from_millis(200));
        assert!(!shows(cx, window, "Saved."));
    }

    #[gpui::test]
    fn closing_the_window_with_a_poll_pending_does_not_panic(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        let (app, window) = open_app(cx, fake.clone());
        let polled = count(&fake, "GET", "/api/status");

        // One step runs the poll up to its requests; their replies are still held back.
        app.update(cx, |app, cx| app.poll_now(cx));
        cx.executor().tick();
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert_eq!(count(&fake, "GET", "/api/status"), polled + 1);

        cx.executor().advance_clock(Duration::from_secs(10));
        assert_eq!(count(&fake, "GET", "/api/status"), polled + 1);
    }

    struct Probe {
        app: AppHandle,
        busy: Busy,
    }

    impl ActionView for Probe {
        fn busy(&mut self) -> &mut Busy {
            &mut self.busy
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().child(
                action_button("probe", "Next", &self.busy).on_click(cx.listener(
                    |this, _, window, cx| {
                        let app = this.app.clone();
                        run_action(
                            this,
                            "probe",
                            &app,
                            |c| c.player("next"),
                            |_, _, _, _| {},
                            window,
                            cx,
                        );
                    },
                )),
            )
        }
    }

    #[gpui::test]
    fn a_failed_post_brings_the_button_back(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        let (app, window) = open_app(cx, fake.clone());
        let handle = app.read_with(cx, |app, _| app.handle());
        let (probe, _) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                    cx.new(|_| Probe {
                        app: handle,
                        busy: Busy::default(),
                    })
                })
            })
            .unwrap();
        cx.run_until_parked();
        let polled = count(&fake, "GET", "/api/status");

        cx.update_window(probe, |_, window, cx| {
            window.click("probe", cx);
            assert_eq!(window.find("probe").disabled(), Some(true));
            window.click("probe", cx);
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(fake.posts().len(), 1);
        assert!(shows(cx, window, "Can't reach the server."));
        cx.update_window(probe, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("probe").disabled(), None);
        })
        .unwrap();
        assert_eq!(count(&fake, "GET", "/api/status"), polled + 1);
    }

    #[gpui::test]
    async fn confirm_is_true_only_for_the_first_button(cx: &mut TestAppContext) {
        let (app, window) = open_app(cx, FakeApi::new());
        let ask = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                app.update(cx, |app, cx| {
                    app.confirm(
                        "Stop the stream?",
                        "The channel goes offline until you start it again.",
                        "Stop stream",
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        };

        let stop = ask(cx);
        assert_eq!(
            cx.pending_prompt(),
            Some((
                "Stop the stream?".into(),
                "The channel goes offline until you start it again.".into()
            ))
        );
        cx.simulate_prompt_answer("Stop stream");
        assert!(stop.await);

        let cancel = ask(cx);
        cx.simulate_prompt_answer("Cancel");
        assert!(!cancel.await);
    }

    #[gpui::test]
    fn shows_the_header_and_the_four_tabs(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        let (_, window) = open_app(cx, fake);

        assert!(shows(cx, window, "StreamLoop"));
        assert!(shows(cx, window, "Uptime: 1m 5s"));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let tabs = [
                "tab-monitor",
                "tab-playback",
                "tab-webhooks",
                "tab-settings",
            ]
            .map(|id| window.find(id).label().map(str::to_string));
            assert_eq!(
                tabs,
                ["Monitor", "Playback", "Webhooks", "Settings"].map(|l| Some(l.to_string()))
            );
        })
        .unwrap();
    }

    #[gpui::test]
    fn the_tab_key_walks_the_tabs_in_page_order(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        let (app, window) = open_app(cx, fake);

        let dashboard = app
            .read_with(cx, |app, _| app.dashboard())
            .expect("the dashboard shows");
        cx.update_window(window, |_, window, cx| {
            window.press("tab", cx);
            window.press("tab", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(
            dashboard.read_with(cx, |dashboard, _| dashboard.selected_tab()),
            Tab::Playback
        );
    }
}
