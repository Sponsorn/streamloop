//! The first-run wizard (index.html `#view-wizard`): six steps under progress dots. The last saves
//! the stream key, the settings and autostart, then waits for the stream to go live.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, AppContext, Context, Div, Entity, FontWeight,
    HighlightStyle, IntoElement, Render, ScrollHandle, SharedString, Task, Window, div, prelude::*,
    pulsating_between, px, relative, rgb,
};
use gpui_kit::component::{
    Disableable,
    button::{Button, ButtonVariants},
    input::InputState,
    label::Label,
    switch::Switch,
};
use serde_json::{Value, json};

use crate::{
    api::types::{Config, PlaylistRef, SetupReport, Stream},
    app::{AppHandle, ToastKind},
    logic::{
        Verdict,
        admin::{PLAYBACK_FIELDS, PlaylistEntry, check_number},
        stream_line, wizard_verdict,
    },
    setup_check::{
        CHECKING_LABEL, EncoderSelect, RUN_LABEL, ReportArea, SetupCheck, apply_setup,
        encoder_select, encoder_select_state, marks_font,
    },
    theme::{
        ACCENT, BG, BORDER, ERROR_TEXT, GREEN, MONO_FONT, MUTED, SURFACE, TEXT, WHITE, YELLOW,
    },
    widgets::{
        FollowFocus, Followed, PlaylistRowIds, PlaylistRows, Run, btn, form_group, hint, list_item,
        put_text, reload_warning, reveal_on_focus, rich, summary_button, text_input, text_of,
        toggle_row,
    },
};

const ROW_IDS: PlaylistRowIds = PlaylistRowIds {
    id: "wiz-pl-id",
    name: "wiz-pl-name",
    remove: "wiz-pl-remove",
    add: "wiz-add-playlist",
};

/// admin.js `WIZ_TOTAL_STEPS`, `WIZ_CHECK_STEP` and the progress labels.
const STEPS: usize = 6;
const CHECK_STEP: usize = 5;
const STEP_LABELS: [&str; STEPS] = [
    "Welcome",
    "Playlists",
    "Stream",
    "Extras",
    "Check",
    "Finish",
];

/// admin.js `runVerification` reads the status this often while it waits for the stream.
const LIVE_CHECK_EVERY: Duration = Duration::from_secs(2);

/// The Finish checklist in page order: index.html's `wiz-chk-config`, `-key` and `-live`.
const CONFIG: usize = 0;
const KEY: usize = 1;
const LIVE: usize = 2;
const CHECKLIST: [(&str, &str); 3] = [
    ("wiz-chk-config", "Save settings"),
    ("wiz-chk-key", "Save the stream key"),
    ("wiz-chk-live", "Stream is live"),
];

const NO_PLAYLIST: &str = "Add at least one playlist ID.";
const NO_KEY: &str = "Enter your stream key. Twitch needs it to accept the stream.";
const DISCORD_HOWTO: &str = "How do I create a Discord webhook?";
const AUTOSTART_LABEL: &str = "Run on Windows startup";

/// One Finish checklist icon, admin.js `setVerifyIcon`'s states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Pending,
    Working,
    Passed,
    Failed,
}

/// A progress dot (index.html `.wiz-progress-dot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    Pending,
    Current,
    Done,
}

/// admin.js `wizState`: what the steps collected when they were left. The stream key is not here:
/// it stays in its field until Finish sends it.
#[derive(Default)]
struct Collected {
    playlists: Vec<PlaylistEntry>,
    ingest_url: String,
    discord_webhook_url: String,
    autostart: bool,
}

/// The first-run wizard, with admin.js's `wiz*` variables as fields.
pub struct WizardView {
    app: AppHandle,
    /// `wizStep`, 1 to 6.
    step: usize,
    collected: Collected,
    playlists: Entity<PlaylistRows>,
    /// Masked. The key stays here until a Finish whose settings save works.
    stream_key: Entity<InputState>,
    ingest: Entity<InputState>,
    discord: Entity<InputState>,
    /// `#wiz-autostart`.
    autostart: bool,
    howto_open: bool,
    /// `wizSetupReport`: the last check's report, `None` before one or after a failed run.
    report: Option<SetupReport>,
    area: ReportArea,
    /// This wizard's check is running.
    checking: bool,
    /// `#wiz-setup-apply`, "Use these settings" and their hint show.
    apply_shown: bool,
    encoder: Entity<EncoderSelect>,
    cache: Entity<InputState>,
    cache_invalid: bool,
    /// "Use these settings" is saving.
    using: bool,
    /// In `CHECKLIST` order.
    marks: [Mark; 3],
    /// `#wiz-verify-status`.
    status: SharedString,
    next_label: SharedString,
    next_disabled: bool,
    /// `#wiz-back.invisible`.
    back_hidden: bool,
    /// `wizFinished`: the next button goes to the dashboard.
    finished: bool,
    scroll: ScrollHandle,
    followed: Followed,
    /// Finish's saves and live wait; dropping the wizard stops them.
    _finish: Option<Task<()>>,
}

impl WizardView {
    /// admin.js `wizInitWizard`.
    pub fn new(app: AppHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let mut view = Self {
            playlists: cx.new(|_| PlaylistRows::new(app.clone(), ROW_IDS)),
            app,
            step: 1,
            collected: Collected::default(),
            stream_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("live_...")
            }),
            ingest: input("rtmp://live.twitch.tv/app", window, cx),
            discord: input("https://discord.com/api/webhooks/...", window, cx),
            autostart: false,
            howto_open: false,
            report: None,
            area: ReportArea::Empty,
            checking: false,
            apply_shown: false,
            encoder: encoder_select_state(window, cx),
            cache: cx.new(|cx| InputState::new(window, cx)),
            cache_invalid: false,
            using: false,
            marks: [Mark::Pending; 3],
            status: SharedString::default(),
            next_label: SharedString::default(),
            next_disabled: false,
            back_hidden: true,
            finished: false,
            scroll: ScrollHandle::new(),
            followed: Followed::default(),
            _finish: None,
        };
        view.load_config(cx);
        view.go_to(1, window, cx);
        view
    }

    /// The saved config's webhook URL (unless masked), ingest URL and playlists (unless they are
    /// all the example's placeholder). A failed read leaves everything empty.
    fn load_config(&mut self, cx: &mut Context<Self>) {
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let Ok(config) = cx.background_spawn(async move { client.config() }).await else {
                return;
            };
            this.update(cx, |this, _| this.prefill(&config)).ok();
        })
        .detach();
    }

    fn prefill(&mut self, config: &Value) {
        let config = Config::from_value(config);
        let webhook = config.discord.webhook_url;
        if !webhook.is_empty() && webhook != "********" {
            self.collected.discord_webhook_url = webhook;
        }
        self.collected.ingest_url = config.supervisor.ingest_url;
        if config.playlists.iter().any(|p| !p.id.contains("xxxxx")) {
            self.collected.playlists = config
                .playlists
                .into_iter()
                .map(|p| PlaylistEntry {
                    id: p.id,
                    name: p.name,
                })
                .collect();
        }
    }

    /// admin.js `wizGoTo`: collects the step being left, then shows `step`.
    fn go_to(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.step != step {
            self.collect_step_data(cx);
        }
        self.step = step;
        self.back_hidden = step == 1;
        self.next_label = match step {
            STEPS => "Finish",
            1 => "Get Started",
            _ => "Continue",
        }
        .into();
        self.init_step(step, window, cx);
        cx.notify();
    }

    /// admin.js `wizNext`.
    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.next_disabled {
            return;
        }
        if self.finished {
            self.app.show_dashboard(cx);
            return;
        }
        if self.step == STEPS {
            self.verify(window, cx);
            return;
        }
        if !self.validate_step(cx) {
            return;
        }
        if self.step == CHECK_STEP {
            self.save_setup_and_go_on(window, cx);
            return;
        }
        self.go_to(self.step + 1, window, cx);
    }

    /// admin.js `wizBack`.
    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.step > 1 {
            self.go_to(self.step - 1, window, cx);
        }
    }

    /// admin.js `wizCollectStepData`.
    fn collect_step_data(&mut self, cx: &mut Context<Self>) {
        match self.step {
            2 => {
                if let Ok(playlists) = self.playlists.update(cx, |rows, cx| rows.collect(cx)) {
                    self.collected.playlists = playlists;
                }
            }
            3 => self.collected.ingest_url = text_of(&self.ingest, cx).trim().to_string(),
            4 => {
                self.collected.discord_webhook_url = text_of(&self.discord, cx).trim().to_string();
                self.collected.autostart = self.autostart;
            }
            _ => {}
        }
    }

    /// admin.js `wizInitStep`.
    fn init_step(&mut self, step: usize, window: &mut Window, cx: &mut Context<Self>) {
        match step {
            2 => {
                let playlists: Vec<PlaylistRef> = self
                    .collected
                    .playlists
                    .iter()
                    .map(|p| PlaylistRef {
                        id: p.id.clone(),
                        name: p.name.clone(),
                    })
                    .collect();
                self.playlists.update(cx, |rows, cx| {
                    rows.set(&playlists, window, cx);
                    if playlists.is_empty() {
                        rows.add_empty(window, cx);
                    }
                });
            }
            3 => put_text(&self.ingest, &self.collected.ingest_url, window, cx),
            4 => {
                put_text(
                    &self.discord,
                    &self.collected.discord_webhook_url,
                    window,
                    cx,
                );
                self.autostart = self.collected.autostart;
            }
            CHECK_STEP => {
                if self.report.is_none() && !SetupCheck::running(cx) {
                    self.run_check(window, cx);
                }
            }
            STEPS => {
                self.marks = [Mark::Pending; 3];
                self.status = SharedString::default();
                self.next_disabled = false;
            }
            _ => {}
        }
    }

    /// admin.js `wizValidateStep`.
    fn validate_step(&mut self, cx: &mut Context<Self>) -> bool {
        if self.step == 2 {
            let Ok(playlists) = self.playlists.update(cx, |rows, cx| rows.collect(cx)) else {
                return false;
            };
            let none = playlists.is_empty();
            self.collected.playlists = playlists;
            if none {
                self.app.toast(NO_PLAYLIST, ToastKind::Error, cx);
                return false;
            }
        }
        if self.step == 3 && text_of(&self.stream_key, cx).trim().is_empty() {
            self.app.toast(NO_KEY, ToastKind::Error, cx);
            return false;
        }
        true
    }

    /// admin.js `wizRunSetupCheck`: runs the check for the first playlist, or joins the run in flight.
    fn run_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        self.apply_shown = false;
        self.area = ReportArea::Checking;
        cx.notify();
        // The playlist is not saved yet; the check sizes the cache from its videos.
        let first = self.collected.playlists.first().map(|p| p.id.clone());
        let run = SetupCheck::run(cx, &self.app.client, first);
        cx.spawn_in(window, async move |this, cx| {
            let answer = run.await;
            this.update_in(cx, |this, window, cx| {
                this.checking = false;
                this.area = ReportArea::after_run(&answer);
                this.report = answer.ok();
                if let Some(report) = &this.report {
                    let chosen = SharedString::from(report.encoder.chosen.clone());
                    this.encoder.update(cx, |select, cx| {
                        select.set_selected_value(&chosen, window, cx)
                    });
                    put_text(
                        &this.cache,
                        &report.cache.recommended_mb.to_string(),
                        window,
                        cx,
                    );
                    this.apply_shown = true;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `wizPicked`: the cache size and, when one works, the encoder; `None` after marking
    /// a cache size that is not a whole number of at least 50.
    fn picked(&mut self, cx: &mut Context<Self>) -> Option<Value> {
        let works = self.report.as_ref()?.encoder.works;
        let cache = &PLAYBACK_FIELDS[1];
        let mb = match check_number(cache, &text_of(&self.cache, cx)) {
            Ok(mb) => mb,
            Err(why) => {
                self.cache_invalid = true;
                self.app.toast(why, ToastKind::Error, cx);
                cx.notify();
                return None;
            }
        };
        self.cache_invalid = false;
        cx.notify();
        let mut picked = json!({ "cacheBudgetMb": mb as i64 });
        if works {
            let encoder = self.encoder.read(cx).selected_value().cloned();
            picked["encoder"] = json!(encoder.unwrap_or_default().as_ref());
        }
        Some(picked)
    }

    /// admin.js `wizUseSetup`.
    fn use_settings(&mut self, cx: &mut Context<Self>) {
        if self.using {
            return;
        }
        let Some(picked) = self.picked(cx) else {
            return;
        };
        self.using = true;
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move { apply_setup(&client, picked) })
                .await;
            this.update(cx, |this, cx| {
                this.using = false;
                cx.notify();
                match answer {
                    Ok(saved) => {
                        let text = match saved {
                            Some(saved) => {
                                reload_warning(&this.app, &saved, cx);
                                "Settings saved."
                            }
                            None => "These settings are already in use.",
                        };
                        this.app.toast(text, ToastKind::Success, cx);
                    }
                    Err(err) => {
                        this.app
                            .toast(format!("Could not save: {err}"), ToastKind::Error, cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `wizSaveSetup`, then `wizGoTo(wizStep + 1)` once the save worked. While a check
    /// runs or after one failed, nothing shows, so nothing is saved.
    fn save_setup_and_go_on(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.report.is_none() || SetupCheck::running(cx) {
            self.go_to(self.step + 1, window, cx);
            return;
        }
        let Some(picked) = self.picked(cx) else {
            return;
        };
        self.next_disabled = true;
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_spawn(async move { apply_setup(&client, picked) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.next_disabled = false;
                cx.notify();
                match answer {
                    Ok(saved) => {
                        if let Some(saved) = saved {
                            reload_warning(&this.app, &saved, cx);
                        }
                        this.go_to(this.step + 1, window, cx);
                    }
                    Err(err) => {
                        this.app
                            .toast(format!("Could not save: {err}"), ToastKind::Error, cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `runVerification`: the key first, since the supervisor must never start without one,
    /// then the settings and autostart, then the status every 2 s until `wizard_verdict` decides.
    fn verify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.next_disabled = true;
        self.next_label = "Saving...".into();
        self.back_hidden = true;
        self.collect_step_data(cx);
        self.marks[KEY] = Mark::Working;
        self.status = "Saving the stream key...".into();
        cx.notify();

        let key = text_of(&self.stream_key, cx);
        let collected = &self.collected;
        let mut body = json!({
            "playlists": collected.playlists,
            "discord": { "webhookUrl": collected.discord_webhook_url },
        });
        if !collected.ingest_url.is_empty() {
            body["supervisor"] = json!({ "ingestUrl": collected.ingest_url });
        }
        let autostart = collected.autostart;
        let (app, client) = (self.app.clone(), self.app.client.clone());
        self._finish = Some(cx.spawn_in(window, async move |this, cx| {
            let api = client.clone();
            let key_saved = cx
                .background_spawn(async move { api.set_stream_key(&key) })
                .await;
            let key_ok = key_saved.is_ok();
            let shown = this.update_in(cx, |this, _, cx| {
                match key_saved {
                    Ok(()) => this.marks[KEY] = Mark::Passed,
                    Err(err) => {
                        this.marks[KEY] = Mark::Failed;
                        this.status = format!("Could not save the stream key: {err}").into();
                    }
                }
                this.marks[CONFIG] = Mark::Working;
                if key_ok {
                    this.status = "Saving settings...".into();
                }
                cx.notify();
            });
            if shown.is_err() {
                return;
            }

            let api = client.clone();
            let saved = cx
                .background_spawn(async move { api.save_config(body) })
                .await;
            let saved = this.update_in(cx, |this, window, cx| {
                let ok = match saved {
                    Ok(saved) => {
                        // A refused reload is expected here: the supervisor is not up yet on a first save.
                        reload_warning(&this.app, &saved, cx);
                        // Cleared only now, so a Retry after a failed save sends the key again.
                        if key_ok {
                            put_text(&this.stream_key, "", window, cx);
                        }
                        this.marks[CONFIG] = Mark::Passed;
                        true
                    }
                    Err(err) => {
                        this.marks[CONFIG] = Mark::Failed;
                        this.status = format!("Could not save: {err}").into();
                        this.back_hidden = false;
                        this.next_disabled = false;
                        this.next_label = "Retry".into();
                        false
                    }
                };
                cx.notify();
                ok
            });
            if !matches!(saved, Ok(true)) {
                return;
            }

            if autostart {
                let api = client.clone();
                let answer = cx
                    .background_spawn(async move { api.set_autostart(true) })
                    .await;
                if let Err(err) = answer {
                    cx.update(|_, cx| {
                        app.toast(
                            format!("Could not turn on autostart: {err}"),
                            ToastKind::Error,
                            cx,
                        )
                    })
                    .ok();
                }
            }

            if !key_ok {
                this.update_in(cx, |this, _, cx| {
                    this.marks[LIVE] = Mark::Failed;
                    this.status =
                        format!("{} You can set the key later in Settings.", this.status).into();
                    this.finish(cx);
                })
                .ok();
                return;
            }

            let waiting = this.update_in(cx, |this, _, cx| {
                this.marks[LIVE] = Mark::Working;
                this.next_label = "Checking...".into();
                cx.notify();
            });
            if waiting.is_err() {
                return;
            }
            let started = cx.background_executor().now();
            let mut stream: Option<Stream> = None;
            let verdict = loop {
                cx.background_executor().timer(LIVE_CHECK_EVERY).await;
                // A closed window reads nothing more.
                if cx.update(|_, _| ()).is_err() {
                    return;
                }
                let api = client.clone();
                // The server can drop a read while it takes the new config in; that one is skipped.
                if let Ok(status) = cx.background_spawn(async move { api.status() }).await {
                    let line = stream_line(Some(&status.stream), None).word;
                    let text = match reason(&status.stream) {
                        Some(reason) => format!("{line}: {reason}"),
                        None => line.to_string(),
                    };
                    let shown = this.update_in(cx, |this, _, cx| {
                        this.status = text.into();
                        cx.notify();
                    });
                    if shown.is_err() {
                        return;
                    }
                    stream = Some(status.stream);
                }
                let elapsed = cx.background_executor().now() - started;
                let verdict = wizard_verdict(stream.as_ref(), elapsed.as_millis() as u64);
                if verdict != Verdict::Wait {
                    break verdict;
                }
            };
            this.update_in(cx, |this, _, cx| {
                if verdict == Verdict::Live {
                    this.marks[LIVE] = Mark::Passed;
                    this.status = "The stream is live.".into();
                } else {
                    this.marks[LIVE] = Mark::Failed;
                    let why = stream
                        .as_ref()
                        .and_then(reason)
                        .unwrap_or("The server did not answer.");
                    this.status =
                        format!("{why} You can fix this later from the dashboard.").into();
                }
                this.finish(cx);
            })
            .ok();
        }));
    }

    /// The end of Finish: the next button reads "Go to Dashboard".
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.finished = true;
        self.next_disabled = false;
        self.next_label = "Go to Dashboard".into();
        cx.notify();
    }

    fn dot(&self, step: usize) -> Dot {
        if step == self.step {
            Dot::Current
        } else if step < self.step {
            Dot::Done
        } else {
            Dot::Pending
        }
    }

    /// index.html `.wiz-progress`: numbered dots with their labels, joined by lines.
    fn render_progress(&self) -> Div {
        let mut row = div()
            .flex()
            .items_start()
            .justify_center()
            .mb(px(32.))
            .px(px(8.));
        for (index, label) in STEP_LABELS.into_iter().enumerate() {
            let step = index + 1;
            if step > 1 {
                // `.wiz-progress-line[data-after]`, green once its step is behind.
                let done = step - 1 < self.step;
                row = row.child(
                    div()
                        .flex_1()
                        .min_w(px(20.))
                        .h(px(2.))
                        .mt(px(15.))
                        .bg(rgb(if done { GREEN } else { BORDER })),
                );
            }
            row = row.child(progress_dot(step, label, self.dot(step)));
        }
        row
    }

    fn render_welcome(&self) -> Div {
        let items = [
            "Add your YouTube playlists",
            "Enter your Twitch stream key",
            "Optional: Discord alerts and Windows autostart",
            "Check this PC's encoder, disk space and program files",
            "Check that the stream goes live",
        ];
        div()
            .child(heading(
                "Welcome to StreamLoop",
                "StreamLoop plays YouTube playlists to Twitch around the clock and recovers on its own when a video fails.",
            ))
            .child(
                info_box("Setup takes five steps:").child(
                    // admin.css `.wiz-checklist-preview`.
                    div()
                        .text_size(px(14.))
                        .text_color(rgb(MUTED))
                        .children(items.into_iter().enumerate().map(|(index, item)| {
                            list_item(
                                format!("{}.", index + 1),
                                Label::new(item)
                                    .text_size(px(14.))
                                    .text_color(rgb(MUTED))
                                    .into_any_element(),
                            )
                        })),
                ),
            )
    }

    fn render_playlists(&self) -> Div {
        use Run::{Plain, Strong};
        let accent = HighlightStyle {
            color: Some(rgb(ACCENT).into()),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..HighlightStyle::default()
        };
        div()
            .child(heading(
                "YouTube Playlists",
                "Add the playlists you want to stream. They play in this order, then start over. You can switch later from the dashboard.",
            ))
            .child(
                info_box("Where to find a playlist ID")
                    .child(
                        // admin.css `.wiz-url-diagram`.
                        div()
                            .mb(px(8.))
                            .px(px(14.))
                            .py(px(10.))
                            .bg(rgb(SURFACE))
                            .rounded(px(8.))
                            .font_family(MONO_FONT)
                            .text_size(px(13.))
                            .text_color(rgb(MUTED))
                            .child(rich(
                                &[
                                    Plain("youtube.com/playlist?list="),
                                    Strong("PLxxxxxxxxxxxxxxxx"),
                                ],
                                accent,
                            )),
                    )
                    .child(
                        // admin.css `.wiz-url-hint`.
                        div().text_size(px(12.)).text_color(rgb(MUTED)).child(rich(
                            &[
                                Plain("Paste the whole playlist URL, or just the part after "),
                                Strong("list="),
                                Plain("."),
                            ],
                            strong(),
                        )),
                    ),
            )
            .child(form_group("Playlists", self.playlists.clone(), None))
    }

    fn render_stream(&self) -> Div {
        use Run::{Plain, Strong};
        div()
            .child(heading(
                "Twitch Stream",
                "StreamLoop sends the stream straight to Twitch.",
            ))
            .child(
                info_box("Where to find your stream key").child(instructions(&[
                    &[Plain("Open the "), Strong("Twitch Creator Dashboard")],
                    &[
                        Plain("Go to "),
                        Strong("Settings"),
                        Plain(", then "),
                        Strong("Stream"),
                    ],
                    &[
                        Plain("Click "),
                        Strong("Copy"),
                        Plain(" next to "),
                        Strong("Primary Stream key"),
                        Plain(" and paste it below"),
                    ],
                ])),
            )
            .child(form_group(
                "Stream key",
                text_input(
                    &self.stream_key,
                    "wiz-stream-key",
                    "Twitch stream key",
                    false,
                ),
                Some("Stored encrypted on this PC. The dashboard never shows it again."),
            ))
            .child(form_group(
                "Ingest URL",
                text_input(&self.ingest, "wiz-ingest", "Ingest URL", false),
                Some("Leave the default unless you use a specific Twitch ingest server."),
            ))
    }

    fn render_extras(&self, cx: &mut Context<Self>) -> Div {
        use Run::{Plain, Strong};
        let summary = summary_button("wiz-discord-howto", DISCORD_HOWTO, self.howto_open).on_click(
            cx.listener(|this, _, _, cx| {
                this.howto_open = !this.howto_open;
                cx.notify();
            }),
        );
        let howto = div()
            .mt(px(8.))
            .child(div().flex().py(px(4.)).child(reveal_on_focus(summary)))
            .when(self.howto_open, |d| {
                d.child(div().mt(px(8.)).child(instructions(&[
                    &[
                        Plain("Open your Discord server and go to "),
                        Strong("Server Settings"),
                    ],
                    &[
                        Plain("Click "),
                        Strong("Integrations"),
                        Plain(", then "),
                        Strong("Webhooks"),
                    ],
                    &[Plain("Click "), Strong("New Webhook")],
                    &[Plain(
                        "Choose a channel for notifications and give it a name",
                    )],
                    &[
                        Plain("Click "),
                        Strong("Copy Webhook URL"),
                        Plain(" and paste it above"),
                    ],
                ])))
            });
        let switch = Switch::new("wiz-autostart")
            .checked(self.autostart)
            .accessibility_label(AUTOSTART_LABEL)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.autostart = *checked;
                cx.notify();
            }));
        div()
            .child(heading(
                "Optional Extras",
                "You can skip this step and set these up later in Settings and Webhooks.",
            ))
            .child(
                form_group(
                    "Discord Webhook URL",
                    text_input(&self.discord, "wiz-discord", "Discord Webhook URL", false),
                    Some("Get a Discord message when a video fails or the stream drops"),
                )
                .child(howto),
            )
            .child(
                // admin.css `.wiz-toggle-row`.
                toggle_row(
                    AUTOSTART_LABEL,
                    &["Start StreamLoop when Windows starts".into()],
                    switch,
                )
                .mt(px(16.))
                .bg(rgb(BG)),
            )
    }

    fn render_check(&self, cx: &mut Context<Self>) -> Div {
        let works = self.report.as_ref().is_some_and(|r| r.encoder.works);
        let apply = self.apply_shown.then(|| {
            div()
                // With no working encoder there is nothing to pick.
                .when(works, |d| {
                    d.child(form_group(
                        "Encoder",
                        encoder_select(&self.encoder, "wiz-encoder"),
                        None,
                    ))
                })
                .child(form_group(
                    "Download cache (MB)",
                    text_input(
                        &self.cache,
                        "wiz-cache",
                        "Download cache (MB)",
                        self.cache_invalid,
                    ),
                    Some("Minimum 50. You can change these later on the Playback tab."),
                ))
        });
        let use_settings = self.apply_shown.then(|| {
            btn(Button::new("wiz-setup-use")
                .primary()
                .label("Use these settings")
                .disabled(self.using))
            .on_click(cx.listener(|this, _, _, cx| this.use_settings(cx)))
        });
        let run = btn(Button::new("wiz-setup-run")
            .label(if self.checking {
                CHECKING_LABEL
            } else {
                RUN_LABEL
            })
            .disabled(self.checking))
        .on_click(cx.listener(|this, _, window, cx| this.run_check(window, cx)));
        div()
            .child(heading(
                "System Check",
                "StreamLoop tests the encoders on this PC and checks free disk space and the program files. This can take up to a minute.",
            ))
            .child(self.area.render())
            .children(apply)
            .child(
                // admin.css `.action-row`.
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.))
                    .mb(px(12.))
                    .children(use_settings.map(reveal_on_focus))
                    .child(reveal_on_focus(run)),
            )
            .when(self.apply_shown, |d| {
                d.child(hint("Continue also saves these settings."))
            })
    }

    fn render_finish(&self) -> Div {
        let items =
            CHECKLIST
                .into_iter()
                .zip(self.marks)
                .enumerate()
                .map(|(index, ((id, text), mark))| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .px(px(16.))
                        .py(px(12.))
                        .when(index + 1 < CHECKLIST.len(), |d| {
                            d.border_b_1().border_color(rgb(BORDER))
                        })
                        .child(verify_icon(id, mark))
                        .child(Label::new(text).text_size(px(14.)))
                });
        div()
            .child(heading(
                "Finish",
                "Click Finish to save your settings and start the stream.",
            ))
            .child(
                // admin.css `.wiz-verify-checklist`.
                div()
                    .py(px(4.))
                    .mb(px(16.))
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .rounded(px(8.))
                    .children(items),
            )
            .child(
                // admin.css `.wiz-verify-status`.
                div()
                    .min_h(px(20.))
                    .mb(px(8.))
                    .when(!self.status.is_empty(), |d| {
                        d.child(
                            Label::new(self.status.clone())
                                .text_size(px(13.))
                                .text_color(rgb(MUTED)),
                        )
                    }),
            )
    }

    /// index.html `.wiz-nav`: Back, which keeps its place while hidden, and the next button.
    fn render_nav(&self, cx: &mut Context<Self>) -> Div {
        let back: AnyElement = if self.back_hidden {
            div().into_any_element()
        } else {
            reveal_on_focus(
                btn(Button::new("wiz-back").label("Back"))
                    .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
            )
            .into_any_element()
        };
        let next = btn(Button::new("wiz-next")
            .primary()
            .label(self.next_label.clone())
            .disabled(self.next_disabled))
        .on_click(cx.listener(|this, _, window, cx| this.next(window, cx)));
        div()
            .flex()
            .items_center()
            .justify_between()
            .mt(px(24.))
            .pt(px(20.))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(back)
            .child(reveal_on_focus(next))
    }
}

#[cfg(test)]
impl WizardView {
    fn step(&self) -> usize {
        self.step
    }

    fn rows(&self) -> Entity<PlaylistRows> {
        self.playlists.clone()
    }

    /// The stream key, Ingest URL, Discord webhook URL and Download cache fields.
    fn fields(&self, cx: &gpui::App) -> [String; 4] {
        [&self.stream_key, &self.ingest, &self.discord, &self.cache].map(|input| text_of(input, cx))
    }

    fn playlists(&self) -> Vec<PlaylistEntry> {
        self.collected.playlists.clone()
    }

    /// In `CHECKLIST` order: Save settings, Save the stream key, Stream is live.
    fn marks(&self) -> [Mark; 3] {
        self.marks
    }

    fn dots(&self) -> [Dot; STEPS] {
        std::array::from_fn(|index| self.dot(index + 1))
    }

    fn cache_invalid(&self) -> bool {
        self.cache_invalid
    }

    fn status(&self) -> SharedString {
        self.status.clone()
    }
}

/// `None` for a missing or empty reason, as admin.js's truthiness test reads it.
fn reason(stream: &Stream) -> Option<&str> {
    stream.reason.as_deref().filter(|r| !r.is_empty())
}

/// admin.css `.wiz-instructions strong`.
fn strong() -> HighlightStyle {
    HighlightStyle {
        color: Some(rgb(TEXT).into()),
        font_weight: Some(FontWeight::MEDIUM),
        ..HighlightStyle::default()
    }
}

/// A step's `<h2>` and the `<p>` under it.
fn heading(title: &'static str, text: &'static str) -> Div {
    div()
        .child(
            Label::new(title)
                .mb(px(4.))
                .text_size(px(22.))
                .font_weight(FontWeight::BOLD),
        )
        .child(
            Label::new(text)
                .mb(px(20.))
                .text_size(px(14.))
                .text_color(rgb(MUTED)),
        )
}

/// admin.css `.wiz-info-box` with its `.wiz-info-title`.
fn info_box(title: &'static str) -> Div {
    div()
        .mb(px(20.))
        .p(px(16.))
        .bg(rgb(BG))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(8.))
        .child(
            Label::new(title)
                .mb(px(8.))
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT)),
        )
}

/// admin.css `ol.wiz-instructions`, with its bold words.
fn instructions(steps: &[&[Run]]) -> Div {
    div()
        .text_size(px(13.))
        .text_color(rgb(MUTED))
        .line_height(relative(1.7))
        .children(steps.iter().enumerate().map(|(index, runs)| {
            list_item(
                format!("{}.", index + 1),
                rich(runs, strong()).into_any_element(),
            )
        }))
}

/// One progress dot: its number in a circle over its label.
fn progress_dot(step: usize, label: &'static str, dot: Dot) -> Div {
    // The page colour on the green, not admin.css's white, which is 2.5:1 there.
    let (fill, number, text) = match dot {
        Dot::Pending => (BORDER, MUTED, MUTED),
        Dot::Current => (ACCENT, WHITE, TEXT),
        Dot::Done => (GREEN, BG, GREEN),
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .flex_shrink_0()
        .child(
            div()
                .size(px(32.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(fill))
                .child(
                    Label::new(step.to_string())
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(number)),
                ),
        )
        .child(
            Label::new(label)
                .text_size(px(11.))
                .whitespace_nowrap()
                .text_color(rgb(text)),
        )
}

/// admin.css `.wiz-verify-icon`: ● pending, a yellow pulse while working, ✔ green, ✘ red.
fn verify_icon(id: &'static str, mark: Mark) -> AnyElement {
    let (glyph, color) = match mark {
        Mark::Pending => ("●", MUTED),
        Mark::Working => ("●", YELLOW),
        Mark::Passed => ("✔", GREEN),
        Mark::Failed => ("✘", ERROR_TEXT),
    };
    let icon = div()
        .id(id)
        .w(px(20.))
        .flex()
        .justify_center()
        .font(marks_font())
        .child(
            Label::new(glyph)
                .text_size(px(18.))
                // The page's line height, which makes each `.wiz-verify-item` 54 px tall.
                .line_height(relative(1.6))
                .text_color(rgb(color)),
        );
    if mark != Mark::Working {
        return icon.into_any_element();
    }
    icon.with_animation(
        id,
        Animation::new(Duration::from_secs(1))
            .repeat()
            .with_easing(pulsating_between(0.4, 1.)),
        |icon, opacity| icon.opacity(opacity),
    )
    .into_any_element()
}

impl Render for WizardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let step = match self.step {
            1 => self.render_welcome(),
            2 => self.render_playlists(),
            3 => self.render_stream(),
            4 => self.render_extras(cx),
            CHECK_STEP => self.render_check(cx),
            _ => self.render_finish(),
        };
        // admin.css `.wizard`.
        let card = div()
            .w_full()
            .max_w(px(640.))
            .my(px(40.))
            .p(px(32.))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded(px(12.))
            .child(self.render_progress())
            .child(step)
            .child(self.render_nav(cx));
        let column = div()
            .id("wizard")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(div().w_full().flex().justify_center().child(card));
        FollowFocus::new(column, &self.scroll, &self.followed)
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use gpui::{AnyWindowHandle, ElementId, TestAppContext, px};
    use gpui_kit::test::TestWindowExt;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi},
        app::{AppView, Screen, TOAST_FOR},
        test_support::{
            answer_polls, config_json, count, open_app, painted, setup_report_json, shows,
            status_json,
        },
    };

    const KEY: &str = "live_x";
    const NO_PLAYLIST: &str = "Add at least one playlist ID.";
    const VIDEO_URL: &str = "That looks like a single video URL. Paste a playlist URL or a playlist ID (starts with PL).";
    const NO_KEY: &str = "Enter your stream key. Twitch needs it to accept the stream.";
    const BAD_CACHE: &str = "Download cache must be a whole number, at least 50.";
    const CHECKING: &str = "Checking this PC. This can take up to a minute.";
    const CACHE_HINT: &str = "Minimum 50. You can change these later on the Playback tab.";
    const SAVES_TOO: &str = "Continue also saves these settings.";
    const LATER: &str = " You can fix this later from the dashboard.";
    const NOT_ANSWERING: &str = "The supervisor is not answering.";

    /// config.example.json as Node serves it on a first run: the placeholder playlist, no webhook
    /// and no stream key.
    fn first_run_config() -> Value {
        let mut config = config_json();
        config["playlists"] = json!([{ "id": "PLxxxxxxxxxxxxxxxx" }]);
        config["discord"]["webhookUrl"] = json!("");
        config["supervisor"]["streamKeySet"] = json!(false);
        config
    }

    /// `POST /api/config`'s answer on a first run: no supervisor runs yet, so the reload is refused.
    fn first_save_json() -> Value {
        json!({
            "ok": true, "config": first_run_config(), "restartRequired": false,
            "reloadError": "connect ECONNREFUSED 127.0.0.1:4417", "reloadErrorCode": "ECONNREFUSED"
        })
    }

    /// `GET /api/status` after the first save, with the stream in `state` and `reason`.
    fn stream_json(state: &str, reason: Option<&str>) -> Value {
        let mut status = status_json(state, false);
        status["stream"]["reason"] = json!(reason);
        status
    }

    type Opened = (
        Arc<FakeApi>,
        AnyWindowHandle,
        Entity<AppView>,
        Entity<WizardView>,
    );

    /// Opens the app on a first run whose saved config is `config`. Every POST the wizard sends
    /// answers as Node does when it works.
    fn open_wizard(cx: &mut TestAppContext, config: Value) -> Opened {
        open_wizard_with(cx, Ok(config))
    }

    fn open_wizard_with(cx: &mut TestAppContext, config: Result<Value, ApiError>) -> Opened {
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("off-air", true)));
        fake.reply("GET", "/api/config", config);
        fake.reply("POST", "/api/setup-check/run", Ok(setup_report_json()));
        fake.reply(
            "POST",
            "/api/stream-key",
            Ok(json!({ "ok": true, "streamKeySet": true })),
        );
        fake.reply("POST", "/api/config", Ok(first_save_json()));
        fake.reply(
            "POST",
            "/api/autostart",
            Ok(json!({ "ok": true, "enabled": true })),
        );
        let (app, window) = open_app(cx, fake.clone());
        let view = app
            .read_with(cx, |app, _| app.wizard())
            .expect("the wizard shows");
        (fake, window, app, view)
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
        let id = id.into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .unwrap();
    }

    fn next(cx: &mut TestAppContext, window: AnyWindowHandle) {
        click(cx, window, "wiz-next");
        cx.run_until_parked();
    }

    fn back(cx: &mut TestAppContext, window: AnyWindowHandle) {
        click(cx, window, "wiz-back");
        cx.run_until_parked();
    }

    /// Replaces the text of the one-line input `id` with `text`.
    fn type_into(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        id: impl Into<ElementId>,
        text: &str,
    ) {
        click(cx, window, id);
        cx.update_window(window, |_, window, cx| {
            window.press("ctrl-a", cx);
            if text.is_empty() {
                window.press("backspace", cx);
            } else {
                window.input(text, cx);
            }
        })
        .unwrap();
    }

    fn present(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> bool {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).is_some()
        })
        .unwrap()
    }

    /// A button's label and whether it is disabled.
    fn button(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        id: &'static str,
    ) -> (Option<String>, bool) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let button = window.find(id);
            (
                button.label().map(str::to_string),
                button.disabled() == Some(true),
            )
        })
        .unwrap()
    }

    fn checked(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> Option<bool> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find(id).checked()
        })
        .unwrap()
    }

    fn step(cx: &mut TestAppContext, view: &Entity<WizardView>) -> usize {
        view.read_with(cx, |view, _| view.step())
    }

    fn fields(cx: &mut TestAppContext, view: &Entity<WizardView>) -> [String; 4] {
        view.read_with(cx, |view, cx| view.fields(cx))
    }

    fn rows(cx: &mut TestAppContext, view: &Entity<WizardView>) -> Entity<PlaylistRows> {
        view.read_with(cx, |view, _| view.rows())
    }

    fn row_values(cx: &mut TestAppContext, view: &Entity<WizardView>) -> Vec<(String, String)> {
        let rows = rows(cx, view);
        rows.read_with(cx, |rows, cx| rows.values(cx))
    }

    /// The element id of `part` (`wiz-pl-id`, `wiz-pl-name` or `wiz-pl-remove`) in row `index`.
    fn row_id(
        cx: &mut TestAppContext,
        view: &Entity<WizardView>,
        part: &'static str,
        index: usize,
    ) -> ElementId {
        let rows = rows(cx, view);
        (part, rows.read_with(cx, |rows, _| rows.keys()[index])).into()
    }

    fn marks(cx: &mut TestAppContext, view: &Entity<WizardView>) -> [Mark; 3] {
        view.read_with(cx, |view, _| view.marks())
    }

    fn painted_count(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) -> usize {
        painted(cx, window).iter().filter(|t| *t == text).count()
    }

    fn bodies(fake: &FakeApi, path: &str) -> Vec<Value> {
        fake.posts()
            .into_iter()
            .filter(|r| r.path == path)
            .map(|r| r.body.unwrap_or_default())
            .collect()
    }

    /// Every request from `from` on, as (method, path, body).
    fn requests_from(fake: &FakeApi, from: usize) -> Vec<(&'static str, String, Option<Value>)> {
        fake.requests()[from..]
            .iter()
            .map(|r| (r.method, r.path.clone(), r.body.clone()))
            .collect()
    }

    /// From Welcome to Extras, with `PLfirst` and the key `live_x`.
    fn walk_to_extras(cx: &mut TestAppContext, window: AnyWindowHandle, view: &Entity<WizardView>) {
        next(cx, window);
        let first = row_id(cx, view, "wiz-pl-id", 0);
        type_into(cx, window, first, "PLfirst");
        next(cx, window);
        type_into(cx, window, "wiz-stream-key", KEY);
        next(cx, window);
        assert_eq!(step(cx, view), 4);
    }

    /// On to the Check step, with its check answered.
    fn walk_to_check(cx: &mut TestAppContext, window: AnyWindowHandle, view: &Entity<WizardView>) {
        walk_to_extras(cx, window, view);
        next(cx, window);
        assert_eq!(step(cx, view), 5);
    }

    /// On to Finish, saving the check's settings on the way.
    fn walk_to_finish(cx: &mut TestAppContext, window: AnyWindowHandle, view: &Entity<WizardView>) {
        walk_to_check(cx, window, view);
        next(cx, window);
        assert_eq!(step(cx, view), 6);
    }

    #[gpui::test]
    fn prefills_from_the_saved_config_but_not_the_placeholders(cx: &mut TestAppContext) {
        let mut config = first_run_config();
        config["discord"]["webhookUrl"] = json!("********");
        config["supervisor"]["ingestUrl"] = json!("rtmp://ingest.example/app");
        let (fake, window, _, view) = open_wizard(cx, config);
        assert_eq!(count(&fake, "GET", "/api/config"), 1);

        next(cx, window);
        // The example's placeholder playlist gives one empty row.
        assert_eq!(row_values(cx, &view), [(String::new(), String::new())]);
        let first = row_id(cx, &view, "wiz-pl-id", 0);
        type_into(cx, window, first, "PLfirst");
        next(cx, window);
        assert_eq!(fields(cx, &view)[1], "rtmp://ingest.example/app");
        type_into(cx, window, "wiz-stream-key", KEY);
        next(cx, window);

        // A saved webhook comes back masked, so it is not shown.
        assert_eq!(step(cx, &view), 4);
        assert_eq!(fields(cx, &view)[2], "");
    }

    #[gpui::test]
    fn prefills_real_playlists_and_a_real_webhook(cx: &mut TestAppContext) {
        let mut config = first_run_config();
        config["playlists"] =
            json!([{ "id": "PLmain", "name": "Main" }, { "id": "PLxxxxxxxxxxxxxxxx" }]);
        config["discord"]["webhookUrl"] = json!("https://discord.com/api/webhooks/1/abc");
        let (_, window, _, view) = open_wizard(cx, config);

        next(cx, window);
        assert_eq!(
            row_values(cx, &view),
            [
                ("PLmain".to_string(), "Main".to_string()),
                ("PLxxxxxxxxxxxxxxxx".to_string(), String::new())
            ]
        );
        next(cx, window);
        type_into(cx, window, "wiz-stream-key", KEY);
        next(cx, window);
        assert_eq!(
            fields(cx, &view)[2],
            "https://discord.com/api/webhooks/1/abc"
        );
    }

    #[gpui::test]
    fn a_failed_config_read_leaves_the_fields_empty(cx: &mut TestAppContext) {
        let (_, window, _, view) = open_wizard_with(
            cx,
            Err(ApiError::Server("config.json is not valid JSON".into())),
        );
        assert_eq!(step(cx, &view), 1);

        next(cx, window);
        assert_eq!(row_values(cx, &view), [(String::new(), String::new())]);
        let first = row_id(cx, &view, "wiz-pl-id", 0);
        type_into(cx, window, first, "PLfirst");
        next(cx, window);
        assert_eq!(fields(cx, &view)[1], "");
    }

    #[gpui::test]
    fn buttons_read_get_started_continue_and_finish(cx: &mut TestAppContext) {
        use Dot::{Current, Done, Pending};
        let (_, window, _, view) = open_wizard(cx, first_run_config());

        assert_eq!(
            button(cx, window, "wiz-next").0.as_deref(),
            Some("Get Started")
        );
        assert!(!present(cx, window, "wiz-back"));
        assert_eq!(
            view.read_with(cx, |view, _| view.dots()),
            [Current, Pending, Pending, Pending, Pending, Pending]
        );
        assert!(shows(cx, window, "Welcome to StreamLoop"));
        assert!(shows(cx, window, "Welcome"));
        assert!(shows(cx, window, "Finish"));

        next(cx, window);
        assert_eq!(
            button(cx, window, "wiz-next").0.as_deref(),
            Some("Continue")
        );
        assert_eq!(button(cx, window, "wiz-back").0.as_deref(), Some("Back"));
        assert_eq!(
            view.read_with(cx, |view, _| view.dots()),
            [Done, Current, Pending, Pending, Pending, Pending]
        );
        assert!(shows(cx, window, "YouTube Playlists"));

        let first = row_id(cx, &view, "wiz-pl-id", 0);
        type_into(cx, window, first, "PLfirst");
        next(cx, window);
        assert!(shows(cx, window, "Twitch Stream"));
        type_into(cx, window, "wiz-stream-key", KEY);
        next(cx, window);
        assert!(shows(cx, window, "Optional Extras"));
        assert_eq!(
            button(cx, window, "wiz-next").0.as_deref(),
            Some("Continue")
        );
        next(cx, window);
        assert!(shows(cx, window, "System Check"));
        assert_eq!(
            button(cx, window, "wiz-next").0.as_deref(),
            Some("Continue")
        );
        next(cx, window);

        assert_eq!(step(cx, &view), 6);
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Finish".into()), false)
        );
        assert!(shows(
            cx,
            window,
            "Click Finish to save your settings and start the stream."
        ));
        assert_eq!(
            view.read_with(cx, |view, _| view.dots()),
            [Done, Done, Done, Done, Done, Current]
        );

        back(cx, window);
        assert_eq!(step(cx, &view), 5);
        assert_eq!(
            button(cx, window, "wiz-next").0.as_deref(),
            Some("Continue")
        );
        back(cx, window);
        back(cx, window);
        back(cx, window);
        back(cx, window);
        assert_eq!(step(cx, &view), 1);
        assert!(!present(cx, window, "wiz-back"));
        assert_eq!(
            button(cx, window, "wiz-next").0.as_deref(),
            Some("Get Started")
        );
    }

    #[gpui::test]
    fn playlists_step_refuses_a_video_url_and_an_empty_list(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        next(cx, window);

        next(cx, window);
        assert!(shows(cx, window, NO_PLAYLIST));
        assert_eq!(step(cx, &view), 2);

        let first = row_id(cx, &view, "wiz-pl-id", 0);
        type_into(
            cx,
            window,
            first.clone(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        );
        next(cx, window);
        assert!(shows(cx, window, VIDEO_URL));
        assert_eq!(step(cx, &view), 2);
        let rows = rows(cx, &view);
        assert_eq!(rows.read_with(cx, |rows, _| rows.marks()), [true]);
        assert!(fake.posts().is_empty());

        // W2: "+ Add Playlist" adds a row and × removes one.
        click(cx, window, "wiz-add-playlist");
        assert_eq!(row_values(cx, &view).len(), 2);
        let second = row_id(cx, &view, "wiz-pl-remove", 1);
        click(cx, window, second);
        assert_eq!(row_values(cx, &view).len(), 1);

        type_into(
            cx,
            window,
            first,
            "https://www.youtube.com/playlist?list=PLfirst",
        );
        next(cx, window);
        assert_eq!(step(cx, &view), 3);
        assert_eq!(
            view.read_with(cx, |view, _| view.playlists()),
            [PlaylistEntry {
                id: "PLfirst".into(),
                name: None
            }]
        );

        // Back again, the rows hold what was collected.
        back(cx, window);
        assert_eq!(
            row_values(cx, &view),
            [("PLfirst".to_string(), String::new())]
        );
        assert!(fake.posts().is_empty());
    }

    #[gpui::test]
    fn stream_step_needs_a_key(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        next(cx, window);
        let first = row_id(cx, &view, "wiz-pl-id", 0);
        type_into(cx, window, first, "PLfirst");
        next(cx, window);
        assert_eq!(step(cx, &view), 3);
        assert!(shows(
            cx,
            window,
            "Stored encrypted on this PC. The dashboard never shows it again."
        ));

        next(cx, window);
        assert_eq!(painted_count(cx, window, NO_KEY), 1);
        type_into(cx, window, "wiz-stream-key", "   ");
        next(cx, window);
        assert_eq!(painted_count(cx, window, NO_KEY), 2);
        assert_eq!(step(cx, &view), 3);

        type_into(cx, window, "wiz-stream-key", KEY);
        next(cx, window);
        next(cx, window);
        assert_eq!(step(cx, &view), 5);
        back(cx, window);
        back(cx, window);

        // The key stays in its field and goes into no request before Finish.
        assert_eq!(step(cx, &view), 3);
        assert_eq!(fields(cx, &view)[0], KEY);
        assert!(!fake.requests().is_empty());
        assert!(
            fake.requests()
                .iter()
                .all(|r| !format!("{:?}", r.body).contains(KEY))
        );
        // A masked input draws dots, never the key.
        assert!(!painted(cx, window).iter().any(|t| t.contains(KEY)));
    }

    #[gpui::test]
    fn check_step_runs_once_on_arrival(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_extras(cx, window, &view);

        // The run is held until the executor runs.
        click(cx, window, "wiz-next");
        assert!(shows(cx, window, CHECKING));
        assert_eq!(
            button(cx, window, "wiz-setup-run"),
            (Some("Checking...".into()), true)
        );
        assert!(!present(cx, window, "wiz-setup-use"));
        assert!(!present(cx, window, "wiz-cache"));
        assert!(!shows(cx, window, SAVES_TOO));
        cx.run_until_parked();

        assert_eq!(
            bodies(&fake, "/api/setup-check/run"),
            [json!({ "playlistId": "PLfirst" })]
        );
        assert!(shows(cx, window, "Encoder: NVIDIA (NVENC)"));
        assert!(shows(
            cx,
            window,
            "✘ Deno is missing: C:\\StreamLoop3\\streamloop\\yt-dlp\\deno.exe"
        ));
        assert_eq!(
            button(cx, window, "wiz-setup-run"),
            (Some("Run checks again".into()), false)
        );
        assert_eq!(
            button(cx, window, "wiz-setup-use"),
            (Some("Use these settings".into()), false)
        );
        assert!(present(cx, window, "wiz-encoder"));
        assert!(shows(cx, window, "NVIDIA (NVENC)"));
        assert_eq!(fields(cx, &view)[3], "4096");
        assert!(shows(cx, window, CACHE_HINT));
        assert!(shows(cx, window, SAVES_TOO));

        back(cx, window);
        next(cx, window);
        assert_eq!(count(&fake, "POST", "/api/setup-check/run"), 1);

        // W6: Run checks again.
        click(cx, window, "wiz-setup-run");
        assert!(!present(cx, window, "wiz-cache"));
        cx.run_until_parked();
        assert_eq!(count(&fake, "POST", "/api/setup-check/run"), 2);
        assert!(present(cx, window, "wiz-cache"));
    }

    #[gpui::test]
    fn a_failed_check_runs_again_on_the_next_arrival(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        fake.reply_once(
            "POST",
            "/api/setup-check/run",
            Err(ApiError::Server("The install folder is not known".into())),
        );
        walk_to_check(cx, window, &view);

        assert!(shows(
            cx,
            window,
            "The check could not run: The install folder is not known"
        ));
        assert!(!present(cx, window, "wiz-setup-use"));
        assert!(!present(cx, window, "wiz-cache"));

        // With no report, Continue moves on and saves nothing.
        next(cx, window);
        assert_eq!(step(cx, &view), 6);
        assert!(bodies(&fake, "/api/config").is_empty());

        back(cx, window);
        assert_eq!(count(&fake, "POST", "/api/setup-check/run"), 2);
        assert!(present(cx, window, "wiz-cache"));
    }

    #[gpui::test]
    fn check_hides_the_encoder_when_nothing_works(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        let mut report = setup_report_json();
        report["encoder"]["works"] = json!(false);
        fake.reply("POST", "/api/setup-check/run", Ok(report));
        walk_to_check(cx, window, &view);

        assert!(shows(
            cx,
            window,
            "Encoder: No encoder works on this PC. Check the graphics driver."
        ));
        assert!(!present(cx, window, "wiz-encoder"));
        assert!(present(cx, window, "wiz-cache"));

        next(cx, window);
        assert_eq!(
            bodies(&fake, "/api/config"),
            [json!({ "supervisor": { "cacheBudgetMb": 4096 } })]
        );
    }

    #[gpui::test]
    fn continue_on_check_saves_and_waits_for_the_save(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_check(cx, window, &view);
        let reads = count(&fake, "GET", "/api/config");

        // A cache size that is not a whole number of at least 50 sends nothing.
        type_into(cx, window, "wiz-cache", "50.5");
        next(cx, window);
        assert!(shows(cx, window, BAD_CACHE));
        assert!(view.read_with(cx, |view, _| view.cache_invalid()));
        assert_eq!(count(&fake, "GET", "/api/config"), reads);
        assert_eq!(step(cx, &view), 5);

        // A failed save stays on the step.
        type_into(cx, window, "wiz-cache", "4096");
        fake.reply_once(
            "POST",
            "/api/config",
            Err(ApiError::Server("config.json is locked".into())),
        );
        click(cx, window, "wiz-next");
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Continue".into()), true)
        );
        cx.run_until_parked();
        assert!(shows(cx, window, "Could not save: config.json is locked"));
        assert_eq!(step(cx, &view), 5);
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Continue".into()), false)
        );
        assert!(!view.read_with(cx, |view, _| view.cache_invalid()));

        // A save that worked moves on, and a reload that failed for another reason is a toast (D6).
        let mut saved = first_save_json();
        saved["reloadError"] = json!("timeout");
        saved["reloadErrorCode"] = json!("ETIMEDOUT");
        fake.reply_once("POST", "/api/config", Ok(saved));
        next(cx, window);
        assert_eq!(step(cx, &view), 6);
        assert!(shows(
            cx,
            window,
            "Saved, but the supervisor could not reload: timeout"
        ));
        assert_eq!(
            bodies(&fake, "/api/config"),
            [
                json!({ "supervisor": { "cacheBudgetMb": 4096 } }),
                json!({ "supervisor": { "cacheBudgetMb": 4096 } })
            ]
        );
    }

    #[gpui::test]
    fn use_these_settings_saves_only_what_differs(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_check(cx, window, &view);

        click(cx, window, "wiz-setup-use");
        assert!(button(cx, window, "wiz-setup-use").1);
        cx.run_until_parked();
        assert!(shows(cx, window, "Settings saved."));
        assert!(!button(cx, window, "wiz-setup-use").1);

        let mut config = first_run_config();
        config["supervisor"]["cacheBudgetMb"] = json!(4096);
        fake.reply("GET", "/api/config", Ok(config));
        cx.executor().advance_clock(TOAST_FOR);
        click(cx, window, "wiz-setup-use");
        cx.run_until_parked();
        assert!(shows(cx, window, "These settings are already in use."));

        // AMD (AMF), one down from NVENC.
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.within("wiz-encoder").click("input", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        click(cx, window, "wiz-setup-use");
        cx.run_until_parked();

        assert_eq!(
            bodies(&fake, "/api/config"),
            [
                json!({ "supervisor": { "cacheBudgetMb": 4096 } }),
                json!({ "supervisor": { "encoder": "h264_amf", "preferredEncoder": "h264_amf" } })
            ]
        );
        assert_eq!(step(cx, &view), 5);
    }

    #[gpui::test]
    fn finish_sends_key_config_autostart_in_order(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        fake.reply(
            "GET",
            "/api/status",
            Ok(stream_json("starting", Some("The supervisor is starting."))),
        );
        next(cx, window);
        let first = row_id(cx, &view, "wiz-pl-id", 0);
        type_into(cx, window, first, "PLfirst");
        next(cx, window);
        type_into(cx, window, "wiz-stream-key", KEY);
        type_into(cx, window, "wiz-ingest", "");
        next(cx, window);
        click(cx, window, "wiz-autostart");
        assert_eq!(checked(cx, window, "wiz-autostart"), Some(true));
        next(cx, window);
        next(cx, window);
        assert_eq!(step(cx, &view), 6);
        let from = fake.requests().len();

        click(cx, window, "wiz-next");
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Saving...".into()), true)
        );
        assert!(!present(cx, window, "wiz-back"));
        assert_eq!(
            marks(cx, &view),
            [Mark::Pending, Mark::Working, Mark::Pending]
        );
        assert!(shows(cx, window, "Saving the stream key..."));
        cx.run_until_parked();

        assert_eq!(
            requests_from(&fake, from),
            [
                (
                    "POST",
                    "/api/stream-key".into(),
                    Some(json!({ "key": KEY }))
                ),
                (
                    "POST",
                    "/api/config".into(),
                    Some(
                        json!({ "playlists": [{ "id": "PLfirst" }], "discord": { "webhookUrl": "" } })
                    )
                ),
                (
                    "POST",
                    "/api/autostart".into(),
                    Some(json!({ "enabled": true }))
                ),
            ]
        );
        // The key leaves its field once the settings are saved; a refused reload is no toast.
        assert_eq!(fields(cx, &view)[0], "");
        assert!(
            !painted(cx, window)
                .iter()
                .any(|t| t.starts_with("Saved, but"))
        );
        assert_eq!(
            marks(cx, &view),
            [Mark::Passed, Mark::Passed, Mark::Working]
        );
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Checking...".into()), true)
        );

        cx.executor().advance_clock(Duration::from_secs(2));
        assert_eq!(
            requests_from(&fake, from + 3),
            [("GET", "/api/status".into(), None)]
        );
        assert!(shows(cx, window, "Starting: The supervisor is starting."));
        cx.executor().advance_clock(Duration::from_secs(2));
        assert_eq!(requests_from(&fake, from + 3).len(), 2);
    }

    #[gpui::test]
    fn finish_ends_on_live(cx: &mut TestAppContext) {
        let (fake, window, app, view) = open_wizard(cx, first_run_config());
        walk_to_finish(cx, window, &view);
        fake.reply("GET", "/api/status", Ok(stream_json("live", None)));

        next(cx, window);
        assert_eq!(
            bodies(&fake, "/api/config").last(),
            Some(&json!({
                "playlists": [{ "id": "PLfirst" }],
                "discord": { "webhookUrl": "" },
                "supervisor": { "ingestUrl": "rtmp://live.twitch.tv/app" }
            }))
        );
        // Autostart was not chosen.
        assert_eq!(count(&fake, "POST", "/api/autostart"), 0);
        cx.executor().advance_clock(Duration::from_secs(2));

        assert!(shows(cx, window, "The stream is live."));
        assert_eq!(marks(cx, &view), [Mark::Passed; 3]);
        assert!(shows(cx, window, "✔"));
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Go to Dashboard".into()), false)
        );
        let polls = count(&fake, "GET", "/api/status");
        cx.executor().advance_clock(Duration::from_secs(10));
        assert_eq!(count(&fake, "GET", "/api/status"), polls);

        answer_polls(&fake);
        next(cx, window);
        assert_eq!(app.read_with(cx, |app, _| app.screen()), Screen::Dashboard);
        assert_eq!(count(&fake, "GET", "/api/events"), 1);
    }

    #[gpui::test]
    fn finish_shows_the_reason_after_the_grace(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_finish(cx, window, &view);
        fake.reply(
            "GET",
            "/api/status",
            Ok(stream_json("off-air", Some(NOT_ANSWERING))),
        );
        next(cx, window);

        cx.executor().advance_clock(Duration::from_secs(14));
        assert!(shows(cx, window, &format!("Off air: {NOT_ANSWERING}")));
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Checking...".into()), true)
        );
        assert_eq!(marks(cx, &view)[2], Mark::Working);

        cx.executor().advance_clock(Duration::from_secs(3));
        assert!(shows(cx, window, &format!("{NOT_ANSWERING}{LATER}")));
        assert_eq!(marks(cx, &view)[2], Mark::Failed);
        assert!(shows(cx, window, "✘"));
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Go to Dashboard".into()), false)
        );
    }

    #[gpui::test]
    fn finish_says_the_server_did_not_answer_when_no_read_worked(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_finish(cx, window, &view);
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        next(cx, window);

        cx.executor().advance_clock(Duration::from_secs(14));
        // A failed read is skipped: the line keeps what it had.
        assert_eq!(
            view.read_with(cx, |view, _| view.status()),
            "Saving settings..."
        );
        cx.executor().advance_clock(Duration::from_secs(3));
        assert!(shows(
            cx,
            window,
            &format!("The server did not answer.{LATER}")
        ));
    }

    #[gpui::test]
    fn finish_gives_up_at_ninety_seconds(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_finish(cx, window, &view);
        let starting = "The stream is starting.";
        fake.reply(
            "GET",
            "/api/status",
            Ok(stream_json("starting", Some(starting))),
        );
        next(cx, window);
        let polls = count(&fake, "GET", "/api/status");

        cx.executor().advance_clock(Duration::from_secs(88));
        assert!(shows(cx, window, &format!("Starting: {starting}")));
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Checking...".into()), true)
        );
        assert_eq!(count(&fake, "GET", "/api/status"), polls + 44);

        cx.executor().advance_clock(Duration::from_secs(3));
        assert!(shows(cx, window, &format!("{starting}{LATER}")));
        assert_eq!(marks(cx, &view)[2], Mark::Failed);
        assert_eq!(count(&fake, "GET", "/api/status"), polls + 45);
    }

    #[gpui::test]
    fn a_failed_key_save_still_saves_settings_and_offers_the_dashboard(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        fake.reply(
            "POST",
            "/api/stream-key",
            Err(ApiError::Server("Could not protect the key".into())),
        );
        walk_to_finish(cx, window, &view);
        let polls = count(&fake, "GET", "/api/status");

        next(cx, window);
        assert!(shows(
            cx,
            window,
            "Could not save the stream key: Could not protect the key You can set the key later in Settings."
        ));
        assert_eq!(marks(cx, &view), [Mark::Passed, Mark::Failed, Mark::Failed]);
        assert_eq!(bodies(&fake, "/api/config").len(), 2);
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Go to Dashboard".into()), false)
        );
        assert!(!present(cx, window, "wiz-back"));
        // The key stays: it was never saved.
        assert_eq!(fields(cx, &view)[0], KEY);
        cx.executor().advance_clock(Duration::from_secs(10));
        assert_eq!(count(&fake, "GET", "/api/status"), polls);
    }

    #[gpui::test]
    fn a_failed_config_save_offers_retry_and_resends_the_key(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_finish(cx, window, &view);
        fake.reply("GET", "/api/status", Ok(stream_json("live", None)));
        fake.reply_once(
            "POST",
            "/api/config",
            Err(ApiError::Server("config.json is locked".into())),
        );

        next(cx, window);
        assert!(shows(cx, window, "Could not save: config.json is locked"));
        assert_eq!(
            marks(cx, &view),
            [Mark::Failed, Mark::Passed, Mark::Pending]
        );
        assert_eq!(
            button(cx, window, "wiz-next"),
            (Some("Retry".into()), false)
        );
        assert_eq!(button(cx, window, "wiz-back").0.as_deref(), Some("Back"));
        assert_eq!(fields(cx, &view)[0], KEY);

        next(cx, window);
        assert_eq!(
            bodies(&fake, "/api/stream-key"),
            [json!({ "key": KEY }), json!({ "key": KEY })]
        );
        assert_eq!(fields(cx, &view)[0], "");
        cx.executor().advance_clock(Duration::from_secs(2));
        assert!(shows(cx, window, "The stream is live."));
    }

    #[gpui::test]
    fn a_failed_autostart_is_a_toast_and_the_wait_goes_on(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        fake.reply(
            "POST",
            "/api/autostart",
            Err(ApiError::Server(
                "Autostart points to another StreamLoop install.".into(),
            )),
        );
        fake.reply("GET", "/api/status", Ok(stream_json("live", None)));
        walk_to_extras(cx, window, &view);
        click(cx, window, "wiz-autostart");
        next(cx, window);
        next(cx, window);
        let mut saved = first_save_json();
        saved["reloadError"] = json!("timeout");
        saved["reloadErrorCode"] = json!("ETIMEDOUT");
        fake.reply("POST", "/api/config", Ok(saved));

        next(cx, window);
        assert!(shows(
            cx,
            window,
            "Could not turn on autostart: Autostart points to another StreamLoop install."
        ));
        // Finish's own save warns about a reload that failed for another reason (D6).
        assert!(shows(
            cx,
            window,
            "Saved, but the supervisor could not reload: timeout"
        ));
        assert_eq!(
            marks(cx, &view),
            [Mark::Passed, Mark::Passed, Mark::Working]
        );

        cx.executor().advance_clock(Duration::from_secs(2));
        assert!(shows(cx, window, "The stream is live."));
    }

    #[gpui::test]
    fn closing_the_window_stops_the_live_wait(cx: &mut TestAppContext) {
        let (fake, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_finish(cx, window, &view);
        fake.reply("GET", "/api/status", Ok(stream_json("starting", None)));
        next(cx, window);
        cx.executor().advance_clock(Duration::from_secs(2));
        let polls = count(&fake, "GET", "/api/status");

        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(10));

        assert_eq!(count(&fake, "GET", "/api/status"), polls);
    }

    #[gpui::test]
    fn the_discord_how_to_opens_and_closes(cx: &mut TestAppContext) {
        let (_, window, _, view) = open_wizard(cx, first_run_config());
        walk_to_extras(cx, window, &view);
        let step = "Choose a channel for notifications and give it a name";
        assert!(shows(cx, window, "How do I create a Discord webhook?"));
        assert!(shows(cx, window, "Run on Windows startup"));
        assert!(shows(cx, window, "Start StreamLoop when Windows starts"));
        assert!(!shows(cx, window, step));

        click(cx, window, "wiz-discord-howto");
        assert!(shows(cx, window, step));
        assert!(shows(
            cx,
            window,
            "Click Copy Webhook URL and paste it above"
        ));
        click(cx, window, "wiz-discord-howto");
        assert!(!shows(cx, window, step));
    }

    /// Presses `key` once per stop and checks that each gets the focus and is in view.
    fn walk(cx: &mut TestAppContext, window: AnyWindowHandle, stops: &[ElementId], key: &str) {
        for id in stops {
            cx.update_window(window, |_, window, cx| {
                window.press(key, cx);
                // The frame that sees the new focus scrolls; the next one draws it there.
                window.render_frame(cx);
                window.render_frame(cx);
                let element = window.find(id.clone());
                assert_eq!(element.focused(), Some(true), "{id:?}");
                let (bounds, height) = (element.bounds(), window.viewport_size().height);
                assert!(
                    bounds.top() >= px(0.) && bounds.bottom() <= height,
                    "{id:?} in view: {bounds:?}"
                );
            })
            .unwrap();
        }
    }

    #[gpui::test]
    fn the_tab_key_reaches_every_control_in_page_order(cx: &mut TestAppContext) {
        let (_, window, _, view) = open_wizard(cx, first_run_config());
        next(cx, window);
        let (id, name, remove) = (
            row_id(cx, &view, "wiz-pl-id", 0),
            row_id(cx, &view, "wiz-pl-name", 0),
            row_id(cx, &view, "wiz-pl-remove", 0),
        );
        click(cx, window, id.clone());
        walk(
            cx,
            window,
            &[
                name,
                remove,
                "wiz-add-playlist".into(),
                "wiz-back".into(),
                "wiz-next".into(),
            ],
            "tab",
        );
        type_into(cx, window, id, "PLfirst");
        next(cx, window);

        click(cx, window, "wiz-stream-key");
        walk(
            cx,
            window,
            &["wiz-ingest".into(), "wiz-back".into(), "wiz-next".into()],
            "tab",
        );
        type_into(cx, window, "wiz-stream-key", KEY);
        // Enter on the focused next button moves on.
        cx.update_window(window, |_, window, cx| {
            window.press("tab", cx);
            window.press("tab", cx);
            window.press("tab", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(step(cx, &view), 4);

        click(cx, window, "wiz-discord");
        walk(
            cx,
            window,
            &[
                "wiz-discord-howto".into(),
                "wiz-autostart".into(),
                "wiz-back".into(),
                "wiz-next".into(),
            ],
            "tab",
        );
        next(cx, window);

        click(cx, window, "wiz-cache");
        walk(
            cx,
            window,
            &[
                "wiz-setup-use".into(),
                "wiz-setup-run".into(),
                "wiz-back".into(),
                "wiz-next".into(),
            ],
            "tab",
        );
        walk(
            cx,
            window,
            &[
                "wiz-back".into(),
                "wiz-setup-run".into(),
                "wiz-setup-use".into(),
                "wiz-cache".into(),
                "wiz-encoder".into(),
            ],
            "shift-tab",
        );
    }
}
