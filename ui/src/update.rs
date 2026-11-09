//! The update banner (index.html `#update-banner`) and the update its button starts: admin.js
//! `renderUpdateBanner`, `handleUpdate`, `pollUpdateProgress` and `waitForRestart`.

use std::time::Duration;

use gpui::{
    App, Context, ElementId, FocusHandle, FontWeight, IntoElement, Render, SharedString, Task,
    Window, div, linear_color_stop, linear_gradient, prelude::*, px, rgb, white,
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
    api::types::UpdateStatus,
    app::{AppHandle, ToastKind},
    logic::admin::update_banner,
    theme::{ACCENT, MAX_WIDTH, WHITE},
    widgets::reveal_on_focus,
};

/// admin.js `pollUpdateProgress` reads the update status this often while Node installs.
const PROGRESS_EVERY: Duration = Duration::from_millis(1500);
/// After `ready`, the wait before the first restart try: Node exits 1 s after it reports `ready`.
const READY_WAIT: Duration = Duration::from_secs(3);
/// A progress read that fails means Node went down to restart; the first try follows this long after.
const DOWN_WAIT: Duration = Duration::from_secs(2);
/// admin.js `waitForRestart`: one `GET /api/status` this often, at most `RESTART_TRIES` times.
const RESTART_EVERY: Duration = Duration::from_secs(2);
const RESTART_TRIES: u32 = 30;

const UPDATE_NOW: &str = "Update Now";
const GAVE_UP: &str = "The server did not come back after the update. Check the StreamLoop 3 console window for errors.";
/// admin.css `.update-banner`'s gradient runs from the accent to this violet.
const BANNER_END: u32 = 0x8b5cf6;

/// The update banner's state and the update that Update Now starts. The banner's button and
/// Settings' Update Now both call `apply`.
pub struct UpdateFlow {
    app: AppHandle,
    /// The banner shows while the last update status had an update on offer.
    shown: bool,
    text: SharedString,
    button: SharedString,
    enabled: bool,
    node_notice: Option<SharedString>,
    /// From Update Now until the update fails, stops on `error` or gives up on the restart.
    running: bool,
    /// The banner button's, so the banner can draw its own focus ring.
    focus: FocusHandle,
    _run: Option<Task<()>>,
}

impl UpdateFlow {
    pub fn new(app: AppHandle, cx: &mut Context<Self>) -> Self {
        Self {
            app,
            shown: false,
            text: "A new version is available.".into(),
            button: UPDATE_NOW.into(),
            enabled: true,
            node_notice: None,
            running: false,
            focus: cx.focus_handle(),
            _run: None,
        }
    }

    /// admin.js `renderUpdateBanner`.
    pub fn show_status(&mut self, status: &UpdateStatus, cx: &mut Context<Self>) {
        self.node_notice = status
            .node_notice
            .clone()
            .filter(|notice| !notice.is_empty())
            .map(Into::into);
        let now_ms = chrono::Utc::now().timestamp_millis();
        match update_banner(status, now_ms, None) {
            Some(banner) => {
                self.shown = true;
                self.text = banner.text.into();
                if let Some((label, enabled)) = banner.button {
                    self.set_button(label, enabled);
                }
            }
            None => self.shown = false,
        }
        cx.notify();
    }

    /// admin.js `handleUpdate`, then `pollUpdateProgress` and `waitForRestart`. Nothing happens
    /// while an update runs already.
    pub fn apply(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.running = true;
        self.set_button("Starting...", false);
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
                    break DOWN_WAIT;
                };
                this.update(cx, |flow, cx| flow.show_status(&status, cx))
                    .ok();
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

            for _ in 0..RESTART_TRIES {
                let api = client.clone();
                if cx
                    .background_spawn(async move { api.status() })
                    .await
                    .is_ok()
                {
                    this.update(cx, |flow, cx| flow.app.start_over(cx)).ok();
                    return;
                }
                cx.background_executor().timer(RESTART_EVERY).await;
            }
            this.update(cx, |flow, cx| {
                flow.app.toast(GAVE_UP, ToastKind::Error, cx);
                flow.offer_retry(cx);
            })
            .ok();
        }));
    }

    fn set_button(&mut self, label: impl Into<SharedString>, enabled: bool) {
        self.button = label.into();
        self.enabled = enabled;
    }

    fn offer_retry(&mut self, cx: &mut Context<Self>) {
        self.running = false;
        self.set_button("Retry", true);
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
            let button = update_button("update-btn", self.button.clone(), self.enabled, cx)
                .track_focus(&self.focus)
                .focus_ring(false)
                .on_click(cx.listener(|flow, _, _, cx| flow.apply(cx)));
            // The theme's accent ring fades into the gradient, so the banner draws a white one, as
            // admin.css draws a focus outline: 2 px wide, 2 px out. A button clips its own children.
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
                        .child(reveal_on_focus(control)),
                )
        });
        div().id("update-banner").children(banner)
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use gpui::{AnyWindowHandle, AppContext, Bounds, Entity, Pixels, TestAppContext};
    use gpui_kit::{base::test_support::texts, test::TestWindowExt};
    use serde_json::{Value, json};

    use crate::{
        api::{ApiError, fake::FakeApi},
        app::{AppView, POLL_EVERY, Screen},
        test_support::{
            answer_polls, config_json, count, offered_json, open_app, shows, status_json,
            update_json,
        },
    };

    const NOTICE: &str = "This release bundles Node 22.24.0. Install the full ZIP to get it.";
    const GAVE_UP: &str = "The server did not come back after the update. Check the StreamLoop 3 console window for errors.";
    const LOST: &str = "Can't reach the server. The dashboard may be out of date.";

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
            assert!(shows(cx, window, text), "{text}");
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
        let text = text_bounds(cx, window, "Version 3.0.6 is available (current: 3.0.5)");
        let notice = text_bounds(cx, window, NOTICE);
        assert!(notice.top() >= text.bottom(), "{notice:?} under {text:?}");
        assert!(text.bottom() <= text_bounds(cx, window, "StreamLoop").top());

        // A failed update status read keeps the banner as it was.
        fake.reply("GET", "/api/update/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(POLL_EVERY);
        assert!(shows(cx, window, NOTICE));
        assert_eq!(
            banner_button(cx, window),
            Some(("Update Now".into(), false))
        );

        // No update: the banner goes, and its notice with it.
        fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
        cx.executor().advance_clock(POLL_EVERY);
        assert_eq!(banner_button(cx, window), None);
        assert!(!shows(cx, window, NOTICE));
        assert!(!shows(
            cx,
            window,
            "Version 3.0.6 is available (current: 3.0.5)"
        ));
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
        assert!(shows(cx, window, "Downloading v3.0.6..."));
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
        assert!(shows(cx, window, "Installing v3.0.6..."));
        assert_eq!(
            banner_button(cx, window),
            Some(("Installing...".into(), true))
        );

        fake.reply("GET", "/api/update/status", Ok(offered_json("ready")));
        advance(cx, 1500);
        assert_eq!(progress_reads(&fake), 4);
        assert!(shows(cx, window, "Update installed. Restarting..."));
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

    #[gpui::test]
    fn gives_up_after_thirty_tries(cx: &mut TestAppContext) {
        let (fake, window, _) = open(cx, offered_json("idle"));
        fake.reply("GET", "/api/update/status", Ok(offered_json("ready")));
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));

        click(cx, window, "update-btn");
        cx.run_until_parked();
        let tries = status_tries(&fake);
        advance(cx, 3000);
        assert_eq!(status_tries(&fake), tries + 1);
        advance(cx, 29 * 2000);
        assert_eq!(status_tries(&fake), tries + 30);
        assert_eq!(
            banner_button(cx, window),
            Some(("Restarting...".into(), true))
        );
        // The polls fail too, and the connection-lost banner shows as in the browser.
        assert!(shows(cx, window, LOST));

        advance(cx, 1999);
        assert!(!shows(cx, window, GAVE_UP));
        advance(cx, 1);
        assert!(shows(cx, window, GAVE_UP));
        assert_eq!(banner_button(cx, window), Some(("Retry".into(), false)));
        assert_eq!(status_tries(&fake), tries + 30);
        advance(cx, 10_000);
        assert_eq!(status_tries(&fake), tries + 30);

        click(cx, window, "update-btn");
        cx.run_until_parked();
        assert_eq!(count(&fake, "POST", "/api/update/apply"), 2);
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
