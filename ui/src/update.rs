//! The update banner (index.html `#update-banner`) and the update its button starts: admin.js
//! `renderUpdateBanner`, `handleUpdate`, `pollUpdateProgress`, `waitForRestart` and `restartLoop`.

use std::time::Duration;

use gpui::{
    App, Context, ElementId, EventEmitter, FocusHandle, FontWeight, IntoElement, Render,
    SharedString, Task, Window, div, linear_color_stop, linear_gradient, prelude::*, px, rgb,
    white,
};
use gpui_kit::{
    base::TestSupportExt,
    component::{
        Disableable, FocusableExt,
        button::{Button, ButtonCustomVariant, ButtonVariants},
        label::Label,
    },
};

use crate::{
    api::types::{SwapLine, UpdateProgress, UpdateStatus},
    app::{AppHandle, ToastKind, now_ms},
    logic::{admin::update_banner, update_progress_text, update_running},
    theme::{ACCENT, MAX_WIDTH, WHITE},
    widgets::reveal_on_focus,
};

/// admin.js `pollUpdateProgress` reads the update status this often while Node installs.
const PROGRESS_EVERY: Duration = Duration::from_millis(1500);
/// After `ready`, the wait before the first restart try: Node exits 1 s after it reports `ready`.
const READY_WAIT: Duration = Duration::from_secs(3);
/// A progress read that fails means Node went down to restart; the first try follows this long after.
const DOWN_WAIT: Duration = Duration::from_secs(2);
/// admin.js `restartLoop`: one `GET /api/status` this often, for as long as Node stays down.
const RESTART_EVERY: Duration = Duration::from_secs(2);
/// During the restart wait the banner's elapsed time moves on this often, whatever a try costs: a
/// refused connect alone takes about 2 s on Windows.
const REDRAW_EVERY: Duration = Duration::from_secs(1);

/// admin.css `.update-banner`'s gradient runs from the accent to this violet.
const BANNER_END: u32 = 0x8b5cf6;

/// The banner's text each time the restart wait redraws a running update's last step.
pub struct LastStep(pub SharedString);

impl EventEmitter<LastStep> for UpdateFlow {}

/// The update banner's state and the update that Update Now starts. The banner's button and
/// Settings' Update Now both call `apply`. It also keeps admin.js's `lastProgress`, `lastSwapLog`
/// and `lastStreamState`, which the Monitor tab's stream line reads too.
pub struct UpdateFlow {
    app: AppHandle,
    shown: bool,
    text: SharedString,
    /// The label and whether it is enabled; None while the banner shows no button.
    button: Option<(SharedString, bool)>,
    node_notice: Option<SharedString>,
    /// From Update Now until the apply is refused or the update stops on `error`.
    running: bool,
    /// From the last update status that answered a poll or a progress read; a failed read keeps them.
    progress: Option<UpdateProgress>,
    swap_log: Vec<SwapLine>,
    /// The stream state of the last poll that answered, so "Ready to stream" goes once it is live.
    stream_state: Option<String>,
    /// The banner button's, so the banner can draw its own focus ring.
    focus: FocusHandle,
    _run: Option<Task<()>>,
    /// The restart wait, once started: it runs until Node answers, and a second start is ignored.
    _restart: Option<Task<()>>,
    /// The restart wait's redraw of the last step, stopped when Node answers.
    _redraw: Option<Task<()>>,
}

impl UpdateFlow {
    pub fn new(app: AppHandle, cx: &mut Context<Self>) -> Self {
        Self {
            app,
            shown: false,
            text: "A new version is available.".into(),
            button: Some(("Update Now".into(), true)),
            node_notice: None,
            running: false,
            progress: None,
            swap_log: Vec::new(),
            stream_state: None,
            focus: cx.focus_handle(),
            _run: None,
            _restart: None,
            _redraw: None,
        }
    }

    /// The full update's progress from the last update status that answered.
    pub fn progress(&self) -> Option<&UpdateProgress> {
        self.progress.as_ref()
    }

    pub fn swap_log(&self) -> &[SwapLine] {
        &self.swap_log
    }

    /// admin.js `pollOnce`'s part: the poll's stream state, then the banner from its update status
    /// at the poll's time. A failed update read leaves the banner and the last progress as they were.
    pub fn on_poll(
        &mut self,
        stream_state: &str,
        update: Option<&UpdateStatus>,
        now_ms: i64,
        cx: &mut Context<Self>,
    ) {
        self.stream_state = Some(stream_state.to_string());
        if let Some(update) = update {
            self.read_status(update, now_ms, cx);
        }
    }

    /// An update status from a poll or a progress read: its progress and swap log are kept.
    fn read_status(&mut self, status: &UpdateStatus, now_ms: i64, cx: &mut Context<Self>) {
        self.progress = status.progress.clone();
        self.swap_log = status.swap_log.clone();
        self.render_banner(status, now_ms, cx);
    }

    /// admin.js `renderUpdateBanner`, for Settings' check result.
    pub fn show_status(&mut self, status: &UpdateStatus, cx: &mut Context<Self>) {
        self.render_banner(status, now_ms(cx), cx);
    }

    fn render_banner(&mut self, status: &UpdateStatus, now_ms: i64, cx: &mut Context<Self>) {
        self.node_notice = status
            .node_notice
            .clone()
            .filter(|notice| !notice.is_empty())
            .map(Into::into);
        match update_banner(status, now_ms, self.stream_state.as_deref()) {
            Some(banner) => {
                self.shown = true;
                self.text = banner.text.into();
                self.button = banner
                    .button
                    .map(|(label, enabled)| (label.into(), enabled));
            }
            None => self.shown = false,
        }
        cx.notify();
    }

    /// admin.js `pollOnce`'s catch after two failed polls: while a full update runs, Node is down
    /// for it, so the window waits for it as after Update Now.
    pub fn server_lost(&mut self, cx: &mut Context<Self>) {
        if update_running(self.progress.as_ref()) {
            self.wait_for_restart(cx);
        }
    }

    /// admin.js `handleUpdate`, then `pollUpdateProgress`. Nothing happens while an update runs
    /// already.
    pub fn apply(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.running = true;
        self.button = Some(("Starting...".into(), false));
        cx.notify();
        let client = self.app.client.clone();
        self._run = Some(cx.spawn(async move |this, cx| {
            let api = client.clone();
            if let Err(err) = cx.background_spawn(async move { api.update_apply() }).await {
                this.update(cx, |flow, cx| {
                    flow.app
                        .toast(format!("Update failed: {err}"), ToastKind::Error, cx);
                    flow.offer_retry(cx);
                })
                .ok();
                return;
            }

            let wait = loop {
                let api = client.clone();
                let Ok(status) = cx
                    .background_spawn(async move { api.update_status() })
                    .await
                else {
                    // Node went down to restart.
                    break DOWN_WAIT;
                };
                if this
                    .update(cx, |flow, cx| flow.read_status(&status, now_ms(cx), cx))
                    .is_err()
                {
                    return;
                }
                // A full update runs on past `ready`: Node stops the stream and swaps the parts.
                if update_running(status.progress.as_ref()) {
                    cx.background_executor().timer(PROGRESS_EVERY).await;
                    continue;
                }
                match status.status.as_str() {
                    "ready" => break READY_WAIT,
                    "error" => {
                        // The banner offers Retry with the error.
                        this.update(cx, |flow, _| flow.running = false).ok();
                        return;
                    }
                    _ => cx.background_executor().timer(PROGRESS_EVERY).await,
                }
            };
            cx.background_executor().timer(wait).await;
            this.update(cx, |flow, cx| flow.wait_for_restart(cx)).ok();
        }));
    }

    /// admin.js `waitForRestart` and `restartLoop`: no limit, since a full update can keep Node down
    /// for minutes. When Node answers, the window starts over as a page reload would. Meanwhile the
    /// banner shows the last step, its time redrawn every second.
    fn wait_for_restart(&mut self, cx: &mut Context<Self>) {
        if self._restart.is_some() {
            return;
        }
        self._redraw = Some(cx.spawn(async move |this, cx| {
            while this
                .update(cx, |flow, cx| flow.show_last_step(cx))
                .is_ok()
            {
                cx.background_executor().timer(REDRAW_EVERY).await;
            }
        }));
        let client = self.app.client.clone();
        self._restart = Some(cx.spawn(async move |this, cx| {
            loop {
                let api = client.clone();
                if cx
                    .background_spawn(async move { api.status() })
                    .await
                    .is_ok()
                {
                    this.update(cx, |flow, cx| {
                        flow._redraw = None;
                        flow.app.start_over(cx);
                    })
                    .ok();
                    return;
                }
                cx.background_executor().timer(RESTART_EVERY).await;
            }
        }));
    }

    /// While Node is down, the banner shows the last step seen, and no button.
    fn show_last_step(&mut self, cx: &mut Context<Self>) {
        let Some(progress) = &self.progress else {
            return;
        };
        self.text = update_progress_text(progress, &self.swap_log, now_ms(cx)).into();
        self.shown = true;
        self.button = None;
        if update_running(Some(progress)) {
            cx.emit(LastStep(self.text.clone()));
        }
        cx.notify();
    }

    fn offer_retry(&mut self, cx: &mut Context<Self>) {
        self.running = false;
        self.button = Some(("Retry".into(), true));
        cx.notify();
    }
}

/// admin.css `.btn-update`: white at 20 % over whatever is behind it, a white border at 30 % and
/// 13 px white text; `.btn:disabled` fades it to 60 %.
pub fn update_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
    cx: &App,
) -> Button {
    let label = label.into();
    let colors = ButtonCustomVariant::new(cx)
        .color(white().opacity(0.3))
        .foreground(white())
        .hover(white().opacity(0.3))
        .active(white().opacity(0.3));
    Button::new(id)
        .accessibility_label(label.clone())
        // A label child, since the button sets its own label's size.
        .child(
            Label::new(label)
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white()),
        )
        .custom(colors)
        .bg(white().opacity(0.2))
        .border_1()
        .border_color(white().opacity(0.3))
        .h(px(35.))
        .px(px(16.))
        .disabled(!enabled)
        .when(!enabled, |button| button.opacity(0.6))
}

impl Render for UpdateFlow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let banner = self.shown.then(|| {
            let control = self.button.clone().map(|(label, enabled)| {
                let button = update_button("update-btn", label, enabled, cx)
                    .track_focus(&self.focus)
                    .focus_ring(false)
                    .on_click(cx.listener(|flow, _, _, cx| flow.apply(cx)));
                // The theme's accent ring fades into the gradient, so the banner draws a white one,
                // as admin.css draws a focus outline: 2 px wide, 2 px out. A button clips its own
                // children.
                let focused = self.focus.is_focused(window);
                let control = div().relative().child(button).when(focused, |control| {
                    control.child(
                        div()
                            .id("update-btn-ring")
                            .absolute()
                            .top(px(-4.))
                            .left(px(-4.))
                            .right(px(-4.))
                            .bottom(px(-4.))
                            .border_2()
                            .border_color(white())
                            .rounded(px(12.))
                            .test_support(),
                    )
                });
                reveal_on_focus(control)
            });
            let gradient = linear_gradient(
                135.,
                linear_color_stop(rgb(ACCENT), 0.),
                linear_color_stop(rgb(BANNER_END), 1.),
            );
            div()
                .w_full()
                .flex()
                .justify_center()
                .px_4()
                .py(px(12.))
                .bg(gradient)
                .child(
                    div()
                        .w_full()
                        .max_w(px(MAX_WIDTH))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    Label::new(self.text.clone())
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(WHITE)),
                                )
                                .children(self.node_notice.clone().map(|notice| {
                                    Label::new(notice).text_size(px(13.)).text_color(rgb(WHITE))
                                })),
                        )
                        .children(control),
                )
        });
        div().id("update-banner").children(banner)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

    use gpui::{
        AnyWindowHandle, AppContext, Bounds, Context, Entity, Pixels, ScrollDelta, TestAppContext,
        point, px,
    };
    use gpui_kit::{base::test_support::texts, test::TestWindowExt};
    use serde_json::{Value, json};

    use super::UpdateFlow;
    use crate::{
        api::{ApiError, fake::FakeApi},
        app::{AppView, POLL_EVERY, Screen, now_ms},
        logic::format_elapsed,
        test_support::{
            UPDATE_RUNNING, answer_polls, banner_text, banner_texts, config_json, count,
            full_offer_json, offered_json, open_app, progress_json, shows, status_json,
            update_json,
        },
    };

    const NOTICE: &str = "This release bundles Node 22.24.0. Install the full ZIP to get it.";
    const LOST: &str = "Can't reach the server. The dashboard may be out of date.";
    const FULL_OFFER: &str =
        "Full update: the stream goes off air while these parts are replaced: Node, ffmpeg.";
    const ROLLED_BACK: &str = "The update to 3.0.6 failed at checking the new version: No encoder works. Still on 3.0.5.";
    /// A step 12.5 minutes old, which prints "12 min" for the next half minute of the test clock.
    const STEP_AGO_S: f64 = 750.;

    fn banner_is(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) {
        assert_eq!(banner_text(cx, window).as_deref(), Some(text));
    }

    /// Opens the dashboard with `update` as the update status. `POST /api/update/apply` answers as
    /// Node does when it starts an update.
    fn open(
        cx: &mut TestAppContext,
        update: Value,
    ) -> (Arc<FakeApi>, AnyWindowHandle, Entity<AppView>) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/update/status", Ok(update));
        fake.reply("GET", "/api/config", Ok(config_json()));
        fake.reply(
            "POST",
            "/api/update/apply",
            Ok(json!({ "ok": true, "message": "Update started" })),
        );
        let (app, window) = open_app(cx, fake.clone());
        (fake, window, app)
    }

    fn advance(cx: &mut TestAppContext, ms: u64) {
        cx.executor().advance_clock(Duration::from_millis(ms));
    }

    /// The banner button's label and whether it is disabled; `None` while the banner is hidden.
    fn banner_button(cx: &mut TestAppContext, window: AnyWindowHandle) -> Option<(String, bool)> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("update-btn").map(|button| {
                (
                    button.label().unwrap_or_default().to_string(),
                    button.disabled() == Some(true),
                )
            })
        })
        .unwrap()
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .unwrap();
    }

    /// Scrolls the dashboard so `id` sits mid-window, clicks it and scrolls back to the banner.
    fn scroll_and_click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let dy = window.viewport_size().height / 2. - window.find(id).bounds().center().y;
            window.scroll("tab-settings", ScrollDelta::Pixels(point(px(0.), dy)), cx);
            window.render_frame(cx);
            window.click(id, cx);
            window.scroll(id, ScrollDelta::Pixels(point(px(0.), -dy)), cx);
        })
        .unwrap();
    }

    fn text_bounds(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) -> Bounds<Pixels> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            texts(window)
                .into_iter()
                .find(|t| t.visible() && t.text() == text)
                .map(|t| t.bounds())
                .unwrap_or_else(|| panic!("{text:?} is painted"))
        })
        .unwrap()
    }

    /// The update flow's own `GET /api/update/status` reads: every poll sends one, and one
    /// `GET /api/events` with it.
    fn progress_reads(fake: &FakeApi) -> usize {
        count(fake, "GET", "/api/update/status") - count(fake, "GET", "/api/events")
    }

    /// `GET /api/status` reads beside the polls': the restart wait's tries, then Connecting's.
    fn status_tries(fake: &FakeApi) -> usize {
        count(fake, "GET", "/api/status") - count(fake, "GET", "/api/events")
    }

    fn screen(cx: &mut TestAppContext, app: &Entity<AppView>) -> Screen {
        app.read_with(cx, |app, _| app.screen())
    }

    #[gpui::test]
    fn banner_follows_the_update_status(cx: &mut TestAppContext) {
        let (fake, window, _) = open(cx, update_json("idle"));
        assert_eq!(banner_button(cx, window), None);

        let mut dev = offered_json("idle");
        dev["isDevMode"] = json!(true);
        let mut counted = offered_json("downloading");
        counted["downloadPercent"] = json!(42);
        let states = [
            (
                offered_json("idle"),
                "Version 3.0.6 is available (current: 3.0.5)",
                "Update Now",
                false,
            ),
            (
                dev,
                "Version 3.0.6 is available (current: 3.0.5)",
                "Dev Mode",
                true,
            ),
            (
                offered_json("downloading"),
                "Downloading v3.0.6...",
                "Downloading...",
                true,
            ),
            (
                counted,
                "Downloading v3.0.6, 42%...",
                "Downloading...",
                true,
            ),
            (
                offered_json("extracting"),
                "Installing v3.0.6...",
                "Installing...",
                true,
            ),
            (
                offered_json("ready"),
                "Update installed. Restarting...",
                "Restarting...",
                true,
            ),
            (
                offered_json("error"),
                "Update failed: Checksum mismatch",
                "Retry",
                false,
            ),
        ];
        for (update, text, button, disabled) in states {
            fake.reply("GET", "/api/update/status", Ok(update));
            cx.executor().advance_clock(POLL_EVERY);
            banner_is(cx, window, text);
            assert_eq!(
                banner_button(cx, window),
                Some((button.to_string(), disabled)),
                "{text}"
            );
        }

        // The Node notice sits under the text, and the banner above the header.
        let mut noted = offered_json("idle");
        noted["nodeNotice"] = json!(NOTICE);
        fake.reply("GET", "/api/update/status", Ok(noted));
        cx.executor().advance_clock(POLL_EVERY);
        let offer = "Version 3.0.6 is available (current: 3.0.5)";
        assert_eq!(banner_texts(cx, window), [offer, NOTICE]);
        let text = text_bounds(cx, window, offer);
        let notice = text_bounds(cx, window, NOTICE);
        assert!(notice.top() >= text.bottom(), "{notice:?} under {text:?}");
        assert!(text.bottom() <= text_bounds(cx, window, "StreamLoop").top());

        // A failed update status read keeps the banner as it was.
        fake.reply("GET", "/api/update/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(POLL_EVERY);
        assert_eq!(banner_texts(cx, window), [offer, NOTICE]);
        assert_eq!(
            banner_button(cx, window),
            Some(("Update Now".into(), false))
        );

        // No update: the banner goes, and its notice with it.
        fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
        cx.executor().advance_clock(POLL_EVERY);
        assert_eq!(banner_button(cx, window), None);
        assert!(banner_texts(cx, window).is_empty());
        assert!(!shows(cx, window, NOTICE));
    }

    #[gpui::test]
    fn apply_polls_progress_then_waits_for_the_restart(cx: &mut TestAppContext) {
        let (fake, window, app) = open(cx, offered_json("idle"));
        let old_dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        let (forgets, configs) = (fake.token_forgets(), count(&fake, "GET", "/api/config"));
        assert_eq!(
            banner_button(cx, window),
            Some(("Update Now".into(), false))
        );
        fake.reply("GET", "/api/update/status", Ok(offered_json("downloading")));

        click(cx, window, "update-btn");
        assert_eq!(
            banner_button(cx, window),
            Some(("Starting...".into(), true))
        );
        // A second press while the update runs sends nothing.
        click(cx, window, "update-btn");
        cx.run_until_parked();

        let applies: Vec<_> = fake
            .posts()
            .into_iter()
            .filter(|r| r.path == "/api/update/apply")
            .collect();
        assert_eq!(applies.len(), 1);
        assert_eq!(applies[0].body, None);
        // The first progress read follows the apply at once, then one every 1.5 s.
        assert_eq!(progress_reads(&fake), 1);
        banner_is(cx, window, "Downloading v3.0.6...");
        assert_eq!(
            banner_button(cx, window),
            Some(("Downloading...".into(), true))
        );
        advance(cx, 1499);
        assert_eq!(progress_reads(&fake), 1);
        advance(cx, 1);
        assert_eq!(progress_reads(&fake), 2);

        fake.reply("GET", "/api/update/status", Ok(offered_json("extracting")));
        advance(cx, 1500);
        assert_eq!(progress_reads(&fake), 3);
        banner_is(cx, window, "Installing v3.0.6...");
        assert_eq!(
            banner_button(cx, window),
            Some(("Installing...".into(), true))
        );

        fake.reply("GET", "/api/update/status", Ok(offered_json("ready")));
        advance(cx, 1500);
        assert_eq!(progress_reads(&fake), 4);
        banner_is(cx, window, "Update installed. Restarting...");
        assert_eq!(
            banner_button(cx, window),
            Some(("Restarting...".into(), true))
        );

        // `ready` ends the progress reads; the restart wait starts 3 s later.
        let tries = status_tries(&fake);
        advance(cx, 2900);
        assert_eq!(progress_reads(&fake), 4);
        assert_eq!(status_tries(&fake), tries);
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        advance(cx, 100);
        assert_eq!(status_tries(&fake), tries + 1);
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        // Without a full update's progress, the failed try leaves the banner as it was.
        banner_is(cx, window, "Update installed. Restarting...");
        assert_eq!(
            banner_button(cx, window),
            Some(("Restarting...".into(), true))
        );
        advance(cx, 1900);
        assert_eq!(status_tries(&fake), tries + 1);
        assert!(!shows(cx, window, LOST));

        // Node is back on the new version. The restart wait's next try starts over, and
        // Connecting's first read fails, so it shows for one round.
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        fake.reply_once("GET", "/api/status", Ok(status_json("live", false)));
        fake.reply_once("GET", "/api/status", Err(ApiError::Unreachable));
        fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
        advance(cx, 100);
        assert_eq!(status_tries(&fake), tries + 3);
        assert_eq!(screen(cx, &app), Screen::Connecting);
        assert_eq!(fake.token_forgets(), forgets + 1);

        advance(cx, 2000);
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        let dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        assert_ne!(dashboard.entity_id(), old_dashboard.entity_id());
        assert!(count(&fake, "GET", "/api/config") > configs);
        assert_eq!(banner_button(cx, window), None);
        assert_eq!(fake.token_forgets(), forgets + 1);
    }

    /// Node is down: every poll and every read fails.
    fn node_down(fake: &FakeApi) {
        for path in ["/api/status", "/api/events", "/api/update/status"] {
            fake.reply("GET", path, Err(ApiError::Unreachable));
        }
    }

    #[gpui::test]
    fn apply_keeps_reading_progress_while_the_update_runs(cx: &mut TestAppContext) {
        let (fake, window, app) = open(cx, full_offer_json("idle", Value::Null));
        let old_dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        let (forgets, configs) = (fake.token_forgets(), count(&fake, "GET", "/api/config"));
        banner_is(cx, window, FULL_OFFER);
        let mut progress = progress_json(cx, "download", "running", STEP_AGO_S);
        progress["downloadPercent"] = json!(42);
        fake.reply(
            "GET",
            "/api/update/status",
            Ok(full_offer_json("downloading", progress)),
        );

        // The dashboard polls at 0 s, 2 s, 4 s...; the click is at 0 s.
        click(cx, window, "update-btn");
        cx.run_until_parked();
        assert_eq!(count(&fake, "POST", "/api/update/apply"), 1);
        assert_eq!(progress_reads(&fake), 1);
        banner_is(cx, window, "Downloading, 42%, 12 min");
        assert_eq!(banner_button(cx, window), None);

        // Node's own status reads `ready` once the files are staged, but the update runs on, so
        // the reads go on every 1.5 s. The swap step is 11 min 52 s old at 0 s.
        let swap_progress = progress_json(cx, "swap", "running", 712.);
        let mut swap = full_offer_json("ready", swap_progress);
        swap["swapLog"] = json!([{ "folder": "node", "status": "retry", "detail": "in use" }]);
        fake.reply("GET", "/api/update/status", Ok(swap));
        advance(cx, 1500);
        assert_eq!(progress_reads(&fake), 2);
        banner_is(cx, window, "Replacing files, 11 min (trying node again)");
        assert_eq!(banner_button(cx, window), None);
        advance(cx, 4500);
        assert_eq!(progress_reads(&fake), 5);

        // Node stops for the swap at 6 s. The read at 7.5 s fails, so the restart wait starts at
        // 9.5 s, before the second failed poll at 10 s asks for it too.
        node_down(&fake);
        advance(cx, 1500);
        assert_eq!(progress_reads(&fake), 6);
        let tries = status_tries(&fake);
        advance(cx, 1999);
        assert_eq!(status_tries(&fake), tries);
        banner_is(cx, window, "Replacing files, 11 min (trying node again)");
        // The first failed try shows the last step at its own time: past 12 minutes.
        advance(cx, 1);
        assert_eq!(status_tries(&fake), tries + 1);
        banner_is(cx, window, "Replacing files, 12 min (trying node again)");
        assert_eq!(banner_button(cx, window), None);

        // One try every 2 s and no limit: 35 more, and the window still waits with the last step.
        advance(cx, 35 * 2000);
        assert_eq!(status_tries(&fake), tries + 36);
        assert_eq!(progress_reads(&fake), 6);
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        banner_is(cx, window, "Replacing files, 13 min (trying node again)");
        assert_eq!(banner_button(cx, window), None);
        assert!(shows(cx, window, LOST));
        assert_eq!(count(&fake, "POST", "/api/update/apply"), 1);

        // The new Node answers during its check: the window starts over with a new token.
        fake.reply("GET", "/api/status", Ok(status_json("off-air", false)));
        fake.reply("GET", "/api/events", Ok(json!([])));
        let mut checking = update_json("idle");
        checking["currentVersion"] = json!("3.0.6");
        checking["progress"] = progress_json(cx, "check", "running", STEP_AGO_S);
        fake.reply("GET", "/api/update/status", Ok(checking));
        advance(cx, 2000);
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        let dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        assert_ne!(dashboard.entity_id(), old_dashboard.entity_id());
        assert!(fake.token_forgets() > forgets);
        assert!(count(&fake, "GET", "/api/config") > configs);
        banner_is(cx, window, "Checking the new version, 12 min");
        assert_eq!(banner_button(cx, window), None);
    }

    #[gpui::test]
    fn two_failed_polls_during_an_update_start_the_restart_wait(cx: &mut TestAppContext) {
        let stopping = full_offer_json("ready", progress_json(cx, "stop", "running", 1.));
        let (fake, window, app) = open(cx, stopping);
        let old_dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        banner_is(cx, window, "Stopping the stream, 1 s");
        assert_eq!(banner_button(cx, window), None);
        let tries = status_tries(&fake);

        // Another window pressed Update Now. This one's Settings check renders the offer it
        // found, with a button, as admin.js `handleCheckUpdate` does.
        fake.reply("POST", "/api/update/check", Ok(offered_json("idle")));
        click(cx, window, "tab-settings");
        cx.run_until_parked();
        scroll_and_click(cx, window, "check-update-btn");
        cx.run_until_parked();
        let offer = "Version 3.0.6 is available (current: 3.0.5)";
        banner_is(cx, window, offer);
        assert_eq!(
            banner_button(cx, window),
            Some(("Update Now".into(), false))
        );

        // Node goes down. A failed poll leaves the banner as it was.
        node_down(&fake);
        advance(cx, 2000);
        assert_eq!(status_tries(&fake), tries);
        assert!(!shows(cx, window, LOST));
        banner_is(cx, window, offer);
        // The second starts the wait, whose first try shows the last step at its own time, and
        // no button.
        advance(cx, 2000);
        assert_eq!(status_tries(&fake), tries + 1);
        assert!(shows(cx, window, LOST));
        banner_is(cx, window, "Stopping the stream, 5 s");
        assert_eq!(banner_button(cx, window), None);
        advance(cx, 20_000);
        assert_eq!(status_tries(&fake), tries + 11);
        banner_is(cx, window, "Stopping the stream, 25 s");
        assert_eq!(banner_button(cx, window), None);
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        assert_eq!(count(&fake, "POST", "/api/update/apply"), 0);

        answer_polls(&fake);
        advance(cx, 2000);
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        let dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        assert_ne!(dashboard.entity_id(), old_dashboard.entity_id());
    }

    /// Every `UpdateFlow` the app makes, kept, so a test can follow one the window has let go.
    fn keep_flows(cx: &mut TestAppContext) -> Rc<RefCell<Vec<Entity<UpdateFlow>>>> {
        let flows: Rc<RefCell<Vec<Entity<UpdateFlow>>>> = Rc::default();
        let kept = flows.clone();
        cx.update(|cx| {
            cx.observe_new(move |_: &mut UpdateFlow, _, cx: &mut Context<UpdateFlow>| {
                kept.borrow_mut().push(cx.entity())
            })
            .detach()
        });
        flows
    }

    /// Each new banner text of `flow`, with the test clock's time it changed.
    fn banner_history(
        cx: &mut TestAppContext,
        flow: &Entity<UpdateFlow>,
    ) -> Rc<RefCell<Vec<(i64, String)>>> {
        let history: Rc<RefCell<Vec<(i64, String)>>> = Rc::default();
        let log = history.clone();
        cx.update(|cx| {
            cx.observe(flow, move |flow, cx| {
                let text = flow.read(cx).text.to_string();
                let mut log = log.borrow_mut();
                if log.last().map(|(_, last)| last) != Some(&text) {
                    log.push((now_ms(cx), text));
                }
            })
            .detach()
        });
        history
    }

    #[gpui::test]
    fn restart_wait_counts_every_second(cx: &mut TestAppContext) {
        let flows = keep_flows(cx);
        let started = cx.update(|cx| now_ms(cx)) - 1000;
        let swap = full_offer_json("ready", progress_json(cx, "swap", "running", 1.));
        let (fake, window, app) = open(cx, swap);
        let old_dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        let replacing = |at: i64| format!("Replacing files, {}", format_elapsed(at - started));
        banner_is(cx, window, "Replacing files, 1 s");
        let flow = flows.borrow()[0].clone();
        let history = banner_history(cx, &flow);

        // Node goes down, and every GET /api/status takes 3 s while the rest of the app runs on, as
        // a refused connect takes 2 s on Windows.
        node_down(&fake);
        let down = cx.update(|cx| now_ms(cx));
        let executor = cx.executor();
        fake.slow("GET", "/api/status", move || {
            executor.advance_clock(Duration::from_secs(3))
        });
        // Step until the wait's first try, which two failed polls start; a slow call in a step adds 3 s.
        let tries = status_tries(&fake);
        while status_tries(&fake) == tries {
            advance(cx, 1000);
        }
        for _ in 0..15 {
            advance(cx, 1000);
            let now = cx.update(|cx| now_ms(cx));
            banner_is(cx, window, &replacing(now));
            assert_eq!(banner_button(cx, window), None);
        }
        assert!(status_tries(&fake) >= tries + 3);

        // Every second of the wait was shown, one at a time.
        let counted: Vec<(i64, String)> = history
            .borrow()
            .iter()
            .filter(|(at, _)| *at > down)
            .cloned()
            .collect();
        assert!(counted.len() >= 15, "{counted:?}");
        for pair in counted.windows(2) {
            assert_eq!(pair[1].0 - pair[0].0, 1000, "{pair:?}");
        }
        for (at, text) in &counted {
            assert_eq!(text, &replacing(*at));
        }

        // Node answers: the window starts over, and the old banner's clock stops.
        fake.fast("GET", "/api/status");
        answer_polls(&fake);
        advance(cx, 5000);
        assert_eq!(screen(cx, &app), Screen::Dashboard);
        let dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        assert_ne!(dashboard.entity_id(), old_dashboard.entity_id());
        let shown = history.borrow().len();
        advance(cx, 5000);
        assert_eq!(history.borrow().len(), shown);
    }

    #[gpui::test]
    fn failed_polls_without_an_update_do_not_start_the_restart_wait(cx: &mut TestAppContext) {
        let (fake, window, app) = open(cx, offered_json("idle"));
        let old_dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        let tries = status_tries(&fake);

        node_down(&fake);
        advance(cx, 10_000);
        assert!(shows(cx, window, LOST));
        assert_eq!(status_tries(&fake), tries);

        // The poll comes back on its own and the window keeps its dashboard.
        answer_polls(&fake);
        advance(cx, 2000);
        let dashboard = app.read_with(cx, |app, _| app.dashboard()).unwrap();
        assert_eq!(dashboard.entity_id(), old_dashboard.entity_id());
        assert!(!shows(cx, window, LOST));
    }

    #[gpui::test]
    fn banner_follows_the_full_update(cx: &mut TestAppContext) {
        let (fake, window, _) = open(cx, full_offer_json("idle", Value::Null));
        banner_is(cx, window, FULL_OFFER);
        assert_eq!(
            banner_button(cx, window),
            Some(("Update Now".into(), false))
        );

        // Running: the progress, and no button.
        let mut progress = progress_json(cx, "download", "running", STEP_AGO_S);
        progress["downloadPercent"] = json!(42);
        fake.reply(
            "GET",
            "/api/update/status",
            Ok(full_offer_json("downloading", progress)),
        );
        advance(cx, 2000);
        banner_is(cx, window, "Downloading, 42%, 12 min");
        assert_eq!(banner_button(cx, window), None);

        // Rolled back with the offer still there: why, and Update Now.
        let mut failed = progress_json(cx, "check", "rolled-back", 60.);
        failed["reason"] = json!("No encoder works.");
        fake.reply(
            "GET",
            "/api/update/status",
            Ok(full_offer_json("idle", failed.clone())),
        );
        advance(cx, 2000);
        banner_is(cx, window, ROLLED_BACK);
        assert_eq!(
            banner_button(cx, window),
            Some(("Update Now".into(), false))
        );
        // Without an offer, no button.
        let mut gone = update_json("idle");
        gone["progress"] = failed;
        fake.reply("GET", "/api/update/status", Ok(gone));
        advance(cx, 2000);
        banner_is(cx, window, ROLLED_BACK);
        assert_eq!(banner_button(cx, window), None);

        // Checked and waiting for Start: "Ready to stream" until the same poll says live.
        let on = |progress: Value| {
            let mut update = update_json("idle");
            update["currentVersion"] = json!("3.0.6");
            update["latestVersion"] = json!("3.0.6");
            update["progress"] = progress;
            update
        };
        let ready = "Updated to 3.0.6. Ready to stream.";
        fake.reply(
            "GET",
            "/api/update/status",
            Ok(on(progress_json(cx, "start", "ok", 60.))),
        );
        fake.reply("GET", "/api/status", Ok(status_json("stopped", false)));
        advance(cx, 2000);
        banner_is(cx, window, ready);
        assert_eq!(banner_button(cx, window), None);
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        advance(cx, 2000);
        assert_eq!(banner_text(cx, window), None);

        // Back on air: for 10 minutes.
        fake.reply(
            "GET",
            "/api/update/status",
            Ok(on(progress_json(cx, "done", "ok", 60.))),
        );
        advance(cx, 2000);
        banner_is(cx, window, "Back on air on 3.0.6.");
        assert_eq!(banner_button(cx, window), None);
        fake.reply(
            "GET",
            "/api/update/status",
            Ok(on(progress_json(cx, "done", "ok", 601.))),
        );
        advance(cx, 2000);
        assert_eq!(banner_text(cx, window), None);

        // An update that neither came from nor went to the running version is not shown.
        let mut older = update_json("idle");
        older["currentVersion"] = json!("3.0.7");
        older["progress"] = progress_json(cx, "check", "failed", 60.);
        fake.reply("GET", "/api/update/status", Ok(older));
        advance(cx, 2000);
        assert_eq!(banner_text(cx, window), None);

        // The same version's parts: the offer is the button too.
        let mut parts = update_json("idle");
        parts["updateAvailable"] = json!(true);
        parts["kind"] = json!("full");
        parts["parts"] = json!(["pot"]);
        parts["sameVersion"] = json!(true);
        fake.reply("GET", "/api/update/status", Ok(parts));
        advance(cx, 2000);
        banner_is(cx, window, "Update parts to 3.0.5");
        assert_eq!(
            banner_button(cx, window),
            Some(("Update parts to 3.0.5".into(), false))
        );
    }

    #[gpui::test]
    fn update_now_during_a_check_shows_the_refusal(cx: &mut TestAppContext) {
        let (fake, window, _) = open(cx, offered_json("idle"));
        fake.reply(
            "POST",
            "/api/update/apply",
            Err(ApiError::Server(UPDATE_RUNNING.into())),
        );

        click(cx, window, "update-btn");
        cx.run_until_parked();

        assert!(shows(cx, window, &format!("Update failed: {UPDATE_RUNNING}")));
        assert_eq!(banner_button(cx, window), Some(("Retry".into(), false)));
        assert_eq!(progress_reads(&fake), 0);
    }

    #[gpui::test]
    fn a_failed_apply_offers_retry(cx: &mut TestAppContext) {
        let (fake, window, _) = open(cx, offered_json("idle"));
        fake.reply(
            "POST",
            "/api/update/apply",
            Err(ApiError::Server("No update available to apply".into())),
        );

        click(cx, window, "update-btn");
        cx.run_until_parked();

        assert!(shows(
            cx,
            window,
            "Update failed: No update available to apply"
        ));
        assert_eq!(banner_button(cx, window), Some(("Retry".into(), false)));
        assert_eq!(progress_reads(&fake), 0);

        click(cx, window, "update-btn");
        cx.run_until_parked();
        assert_eq!(count(&fake, "POST", "/api/update/apply"), 2);
    }

    #[gpui::test]
    fn the_banner_button_comes_before_the_tabs(cx: &mut TestAppContext) {
        let (_, window, _) = open(cx, offered_json("idle"));

        // The banner draws the button's white focus ring itself, only while it has the focus.
        for (id, ring) in [("update-btn", true), ("tab-monitor", false)] {
            cx.update_window(window, |_, window, cx| {
                window.press("tab", cx);
                window.render_frame(cx);
                assert_eq!(window.find(id).focused(), Some(true), "{id}");
                assert_eq!(window.try_find("update-btn-ring").is_some(), ring, "{id}");
            })
            .unwrap();
        }
    }
}
