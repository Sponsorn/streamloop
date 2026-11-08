//! The Playback tab (index.html `#panel-playback`): encoder and overlay, the system check, buffering and the watchdog.

use std::{collections::HashSet, time::Duration};

use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px, rgb,
};
use gpui_kit::component::{
    Disableable, IndexPath, Sizable,
    button::{Button, ButtonVariants},
    input::InputState,
    label::Label,
    select::{Select, SelectItem, SelectState},
    switch::Switch,
};
use serde_json::{Value, json};

use crate::{
    api::types::{Config, SaveResult, SetupReport},
    app::{AppHandle, ToastKind},
    dashboard::TabView,
    logic::{
        ENCODER_NAMES,
        admin::{PLAYBACK_FIELDS, check_number, local_date_time, without_blanks},
        save_patch,
    },
    setup_check::{CHECKING_LABEL, RUN_LABEL, ReportArea, SetupCheck, apply_setup},
    theme::{BG, MUTED},
    widgets::{
        ActionView, Busy, RestartNote, btn, form_group, reload_warning, section, settings_panel,
        text_input,
    },
};

/// admin.js resets the save button 1.5 s after a save.
pub const SAVE_RESET: Duration = Duration::from_millis(1500);

pub const NOTE: RestartNote = RestartNote {
    id: "pb-restart-note",
    button: "pb-restart-stream",
    text: "Some changes apply only after the stream restarts. Viewers see a short drop.",
};

const SAVE_LABEL: &str = "Save Playback Settings";
const NO_CHECK: &str = "No check has run on this PC yet.";

const ENCODER_HINT: &str =
    "Pick the graphics card in this PC. Software encoding works anywhere but loads the CPU.";
const PLAYER_CLIENT_HINT: &str = "Which YouTube clients yt-dlp asks for video, tried in order. Change it only if videos keep failing to load. The default is mweb,web_embedded; a blank field keeps the saved value.";
const COOKIES_HINT: &str = "Lets the stream's YouTube lookups and downloads, and the video list on the Monitor tab, use your YouTube login. That avoids most \"Sign in to confirm you're not a bot\" errors. Firefox works while it is open. Chrome often doesn't, because it locks its cookies while it runs. Leave blank for none. The stream uses a new value at once; the video list after StreamLoop restarts.";

/// The number fields in `PLAYBACK_FIELDS` order: element id, label and hint.
const NUMBER_INPUTS: [(&str, &str, &str); 4] = [
    (
        "pb-read-ahead",
        "Read-ahead (seconds)",
        "Seconds of video kept ready ahead of what viewers see. Minimum 2.",
    ),
    (
        "pb-cache",
        "Download cache (MB)",
        "Disk space for videos downloaded ahead of time. Minimum 50.",
    ),
    (
        "pb-freeze",
        "Frozen picture (seconds)",
        "How long the picture must stay still before the video is retried. Minimum 5. Raise it if you stream still images.",
    ),
    (
        "pb-silence",
        "Silence (seconds)",
        "How long the audio must stay silent before it is noted on the timeline. Silence never interrupts the stream. Minimum 5.",
    ),
];

/// One encoder dropdown entry: its config value and the name `ENCODER_NAMES` shows.
#[derive(Clone)]
struct EncoderOption {
    value: SharedString,
    name: SharedString,
}

impl SelectItem for EncoderOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    // The closed box and the menu rows draw the name as a Label, like the app's other text.
    fn display_title(&self) -> Option<AnyElement> {
        Some(Label::new(self.name.clone()).into_any_element())
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Label::new(self.name.clone())
    }

    fn value(&self) -> &SharedString {
        &self.value
    }
}

/// The Playback tab, with admin.js's playback variables as fields.
pub struct PlaybackView {
    app: AppHandle,
    busy: Busy,
    encoder: Entity<SelectState<Vec<EncoderOption>>>,
    pub(crate) overlay: bool,
    /// In `PLAYBACK_FIELDS` order.
    numbers: [Entity<InputState>; 4],
    player_client: Entity<InputState>,
    cookies: Entity<InputState>,
    /// The config keys of the number fields marked invalid.
    invalid: HashSet<&'static str>,
    /// `playbackConfig`: the config the form was filled from, which a save's patch is measured against.
    loaded: Value,
    /// `playbackSettingsLoaded`.
    form_loaded: bool,
    save_label: &'static str,
    save_disabled: bool,
    /// `playbackReport`: what "Use these settings" applies; `None` hides that button.
    report: Option<SetupReport>,
    /// `#pb-setup-when`.
    when: SharedString,
    area: ReportArea,
    /// This tab's check is running.
    checking: bool,
    /// "Use these settings" is saving.
    using: bool,
    _notes: Subscription,
}

impl PlaybackView {
    pub fn new(app: AppHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let options: Vec<EncoderOption> = ENCODER_NAMES
            .iter()
            .map(|(value, name)| EncoderOption {
                value: (*value).into(),
                name: (*name).into(),
            })
            .collect();
        // A <select> shows its first option until the form loads.
        let encoder = cx.new(|cx| SelectState::new(options, Some(IndexPath::new(0)), window, cx));
        Self {
            app,
            busy: Busy::default(),
            encoder,
            overlay: true,
            numbers: std::array::from_fn(|_| cx.new(|cx| InputState::new(window, cx))),
            player_client: cx
                .new(|cx| InputState::new(window, cx).placeholder("mweb,web_embedded")),
            cookies: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("e.g. brave, chrome, firefox, chrome:Profile 2")
            }),
            invalid: HashSet::new(),
            loaded: Value::Null,
            form_loaded: false,
            save_label: SAVE_LABEL,
            save_disabled: false,
            report: None,
            when: SharedString::default(),
            area: ReportArea::Empty,
            checking: false,
            using: false,
            _notes: RestartNote::observe(cx),
        }
    }

    /// admin.js `loadPlaybackSettings`: once, until a save asks for it again.
    fn load_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form_loaded {
            return;
        }
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx.background_spawn(async move { client.config() }).await;
            this.update_in(cx, |this, window, cx| match answer {
                Ok(config) => this.fill(config, window, cx),
                Err(err) => this.app.toast(
                    format!("Could not load playback settings: {err}"),
                    ToastKind::Error,
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }

    fn fill(&mut self, config: Value, window: &mut Window, cx: &mut Context<Self>) {
        let settings = Config::from_value(&config);
        let s = &settings.supervisor;
        let encoder = SharedString::from(s.encoder.clone());
        self.encoder.update(cx, |select, cx| {
            select.set_selected_value(&encoder, window, cx)
        });
        self.overlay = s.overlay;
        let numbers = [
            s.read_ahead_seconds,
            s.cache_budget_mb,
            s.freeze_seconds,
            s.silence_seconds,
        ];
        for (input, n) in self.numbers.iter().zip(numbers) {
            set_text(input, n.to_string(), window, cx);
        }
        set_text(&self.player_client, s.player_client.clone(), window, cx);
        set_text(
            &self.cookies,
            settings.ytdl_cookies_from_browser.clone(),
            window,
            cx,
        );
        self.loaded = config;
        self.form_loaded = true;
        cx.notify();
    }

    /// admin.js `loadPlaybackSetupReport`. A run in flight keeps its busy text, which the saved report would replace.
    fn load_report(&mut self, cx: &mut Context<Self>) {
        if SetupCheck::running(cx) {
            return;
        }
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.setup_check() })
                .await;
            this.update(cx, |this, cx| {
                match answer {
                    Ok(report) => this.show_report(report),
                    Err(err) => {
                        this.report = None;
                        this.when = SharedString::default();
                        this.area = ReportArea::Failed(err.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `showPlaybackReport`.
    fn show_report(&mut self, report: Option<SetupReport>) {
        match report {
            None => {
                self.when = NO_CHECK.into();
                self.area = ReportArea::Empty;
                self.report = None;
            }
            Some(report) => {
                self.when = format!("Last check: {}", local_date_time(&report.at)).into();
                self.area = ReportArea::Report(report.clone());
                self.report = Some(report);
            }
        }
    }

    /// admin.js `handlePlaybackSetupRun`: runs the check, or joins the one in flight.
    fn run_check(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        self.area = ReportArea::Checking;
        cx.notify();
        let run = SetupCheck::run(cx, &self.app.client, None);
        cx.spawn(async move |this, cx| {
            let answer = run.await;
            this.update(cx, |this, cx| {
                this.checking = false;
                this.area = ReportArea::after_run(&answer);
                if let Ok(report) = answer {
                    this.show_report(Some(report));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handlePlaybackSetupUse`: the recommended cache and, when one works, the chosen encoder.
    fn use_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(report) = &self.report else {
            return;
        };
        if self.using {
            return;
        }
        let mut picked = json!({ "cacheBudgetMb": report.cache.recommended_mb });
        if report.encoder.works {
            picked["encoder"] = json!(report.encoder.chosen);
        }
        self.using = true;
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_spawn(async move { apply_setup(&client, picked) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.using = false;
                cx.notify();
                match answer {
                    Ok(None) => {
                        this.app
                            .toast("These settings are already in use.", ToastKind::Success, cx)
                    }
                    Ok(Some(saved)) => {
                        reload_warning(&this.app, &saved, cx);
                        this.app.toast("Settings saved.", ToastKind::Success, cx);
                        this.after_save(&saved, window, cx);
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

    /// admin.js `handlePlaybackSave`.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let encoder = self
            .encoder
            .read(cx)
            .selected_value()
            .cloned()
            .unwrap_or_default();
        let mut supervisor = without_blanks(json!({
            "encoder": encoder.as_ref(),
            "overlay": self.overlay,
            "playerClient": text_of(&self.player_client, cx).trim(),
        }));
        for (field, input) in PLAYBACK_FIELDS.iter().zip(&self.numbers) {
            match check_number(field, &text_of(input, cx)) {
                Ok(n) => {
                    self.invalid.remove(field.key);
                    supervisor[field.key] = json_number(n);
                }
                Err(why) => {
                    self.invalid.insert(field.key);
                    self.app.toast(why, ToastKind::Error, cx);
                    cx.notify();
                    return;
                }
            }
        }
        // Blank cookies is a real value (none), so it is not dropped like the blanks above.
        let cookies = text_of(&self.cookies, cx).trim().to_string();
        let patch = save_patch(
            &self.loaded,
            &json!({ "supervisor": supervisor, "ytdlCookiesFromBrowser": cookies }),
        );
        self.save_disabled = true;
        if patch.as_object().is_none_or(|p| p.is_empty()) {
            self.save_label = "No changes";
            self.reset_save_button(cx);
            cx.notify();
            return;
        }
        self.save_label = "Saving...";
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.save_config(patch) })
                .await;
            this.update_in(cx, |this, window, cx| {
                match answer {
                    Ok(saved) => {
                        reload_warning(&this.app, &saved, cx);
                        this.save_label = "Saved";
                        this.after_save(&saved, window, cx);
                    }
                    Err(err) => {
                        this.app
                            .toast(format!("Could not save: {err}"), ToastKind::Error, cx)
                    }
                }
                this.reset_save_button(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// After a save that went through: the restart note when the server asks for it, and the form again.
    fn after_save(&mut self, saved: &SaveResult, window: &mut Window, cx: &mut Context<Self>) {
        if saved.restart_required {
            NOTE.show(cx);
        }
        self.form_loaded = false;
        self.load_form(window, cx);
    }

    fn reset_save_button(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_RESET).await;
            this.update(cx, |this, cx| {
                this.save_label = SAVE_LABEL;
                this.save_disabled = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether the number field with config key `key` is marked invalid.
    #[cfg(test)]
    pub(crate) fn is_invalid(&self, key: &str) -> bool {
        self.invalid.contains(key)
    }

    /// The form's values in page order: encoder, read-ahead, cache, player client, cookies, freeze, silence.
    #[cfg(test)]
    fn values(&self, cx: &App) -> Vec<String> {
        let encoder = self.encoder.read(cx).selected_value().cloned();
        let [read_ahead, cache, freeze, silence] = &self.numbers;
        let mut values = vec![encoder.map(|e| e.to_string()).unwrap_or_default()];
        for input in [
            read_ahead,
            cache,
            &self.player_client,
            &self.cookies,
            freeze,
            silence,
        ] {
            values.push(text_of(input, cx));
        }
        values
    }

    fn render_encoder(&self) -> Div {
        div().w_full().max_w(px(280.)).child(
            Select::new(&self.encoder)
                .id("pb-encoder")
                .large()
                .placeholder("")
                .accessibility_label("Encoder")
                .h(px(40.))
                .px(px(12.))
                .bg(rgb(BG)),
        )
    }

    /// index.html's `.toggle-row-plain` for the overlay.
    fn render_overlay(&self, cx: &mut Context<Self>) -> Div {
        div()
            .my(px(12.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    .child(Label::new("Now-playing overlay").text_size(px(14.)))
                    .child(
                        Label::new("Draw the video title and a progress line on the stream")
                            .text_size(px(12.))
                            .text_color(rgb(MUTED)),
                    ),
            )
            .child(
                Switch::new("pb-overlay")
                    .checked(self.overlay)
                    .accessibility_label("Now-playing overlay")
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.overlay = *checked;
                        cx.notify();
                    })),
            )
    }

    fn render_check(&self, cx: &mut Context<Self>) -> Div {
        let run = btn(Button::new("pb-setup-run")
            .label(if self.checking {
                CHECKING_LABEL
            } else {
                RUN_LABEL
            })
            .disabled(self.checking))
        .on_click(cx.listener(|this, _, _, cx| this.run_check(cx)));
        let use_settings = self.report.is_some().then(|| {
            btn(Button::new("pb-setup-use")
                .label("Use these settings")
                .disabled(self.using))
            .on_click(cx.listener(|this, _, window, cx| this.use_settings(window, cx)))
        });
        settings_panel()
            .when(!self.when.is_empty(), |d| {
                d.child(Label::new(self.when.clone()).text_size(px(14.)))
            })
            .child(self.area.render())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.))
                    .mb(px(12.))
                    .child(run)
                    .children(use_settings),
            )
    }

    fn render_number(&self, index: usize) -> Div {
        let (id, label, hint) = NUMBER_INPUTS[index];
        let invalid = self.invalid.contains(PLAYBACK_FIELDS[index].key);
        form_group(
            label,
            text_input(&self.numbers[index], id, label, invalid),
            Some(hint),
        )
    }
}

impl ActionView for PlaybackView {
    fn busy(&mut self) -> &mut Busy {
        &mut self.busy
    }
}

impl TabView for PlaybackView {
    /// admin.js `switchTab('playback')`: the form once, the last report every time.
    fn on_tab_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_form(window, cx);
        self.load_report(cx);
    }
}

impl Render for PlaybackView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let save = btn(Button::new("pb-save-btn")
            .primary()
            .label(self.save_label)
            .disabled(self.save_disabled))
        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        div()
            .child(
                section("Stream Output").child(
                    settings_panel()
                        .child(form_group(
                            "Encoder",
                            self.render_encoder(),
                            Some(ENCODER_HINT),
                        ))
                        .child(self.render_overlay(cx)),
                ),
            )
            .child(section("System Check").child(self.render_check(cx)))
            .child(
                section("Buffering").child(
                    settings_panel()
                        .child(self.render_number(0))
                        .child(self.render_number(1))
                        .child(form_group(
                            "yt-dlp player client",
                            text_input(
                                &self.player_client,
                                "pb-player-client",
                                "yt-dlp player client",
                                false,
                            ),
                            Some(PLAYER_CLIENT_HINT),
                        ))
                        .child(form_group(
                            "yt-dlp cookies from browser",
                            text_input(
                                &self.cookies,
                                "pb-ytdl-cookies",
                                "yt-dlp cookies from browser",
                                false,
                            ),
                            Some(COOKIES_HINT),
                        )),
                ),
            )
            .child(
                section("Watchdog").child(
                    settings_panel()
                        .child(self.render_number(2))
                        .child(self.render_number(3)),
                ),
            )
            // As wide as its label, as the inline button in index.html.
            .child(div().flex().child(save))
            .children(NOTE.render(&self.app, &self.busy, cx))
    }
}

fn text_of(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

fn set_text(input: &Entity<InputState>, text: String, window: &mut Window, cx: &mut App) {
    input.update(cx, |state, cx| {
        state.set_value(SharedString::from(text), window, cx)
    });
}

/// A number as `JSON.stringify` writes it: a whole number without a fraction.
fn json_number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AnyWindowHandle, AppContext, ScrollDelta, TestAppContext, point, px};
    use gpui_kit::test::TestWindowExt;
    use serde_json::json;

    use super::*;
    use crate::{
        api::{ApiError, client::Client, fake::FakeApi},
        app::{AppView, TOAST_FOR},
        dashboard::Tab,
        logic::admin::local_date_time,
        setup_check::SetupCheck,
        test_support::{
            answer_polls, config_json, count, open_app, saved_json, setup_report_json, shows,
        },
    };

    const NOTE_TEXT: &str =
        "Some changes apply only after the stream restarts. Viewers see a short drop.";
    const LAST_CHECK_AT: &str = "2026-11-08T12:34:00.000Z";

    /// Opens the dashboard on `config` and the saved `report`, then the Playback tab.
    fn open_playback(
        cx: &mut TestAppContext,
        config: Value,
        report: Value,
    ) -> (Arc<FakeApi>, AnyWindowHandle, Entity<AppView>) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", Ok(config));
        fake.reply("GET", "/api/setup-check", Ok(report));
        fake.reply("POST", "/api/config", Ok(saved_json(false)));
        fake.reply("POST", "/api/stream/stop", Ok(json!({ "ok": true })));
        fake.reply("POST", "/api/stream/start", Ok(json!({ "ok": true })));
        let (app, window) = open_app(cx, fake.clone());
        click(cx, window, "tab-playback");
        cx.run_until_parked();
        (fake, window, app)
    }

    fn playback(cx: &mut TestAppContext, app: &Entity<AppView>) -> Entity<PlaybackView> {
        let dashboard = app
            .read_with(cx, |app, _| app.dashboard())
            .expect("the dashboard shows");
        dashboard
            .read_with(cx, |dashboard, _| dashboard.tab_view(Tab::Playback))
            .downcast::<PlaybackView>()
            .unwrap_or_else(|_| panic!("the Playback tab is a PlaybackView"))
    }

    /// Scrolls the dashboard so `id` sits in the middle of the window. The wheel goes to a
    /// control on screen, since the window draws only part of the tab.
    fn reveal(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let height = window.viewport_size().height;
            let dy = height / 2. - window.find(id).bounds().center().y;
            let on_screen = [
                "tab-playback",
                "pb-overlay",
                "pb-setup-run",
                "pb-save-btn",
                "pb-read-ahead",
                "pb-cache",
                "pb-player-client",
                "pb-ytdl-cookies",
                "pb-freeze",
                "pb-silence",
            ]
            .into_iter()
            .find(|c| {
                let y = window.find(*c).bounds().center().y;
                y > px(0.) && y < height
            })
            .expect("a control on screen");
            window.scroll(on_screen, ScrollDelta::Pixels(point(px(0.), dy)), cx);
            window.render_frame(cx);
            let y = window.find(id).bounds().center().y;
            assert!(y > px(0.) && y < height, "{id} on screen after the scroll");
        })
        .unwrap();
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
        reveal(cx, window, id);
        cx.update_window(window, |_, window, cx| window.click(id, cx))
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

    /// Replaces the text of the input `id` with `text`, as a user selecting all and typing.
    fn type_into(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str, text: &str) {
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

    /// Opens the encoder dropdown and moves `downs` options down from the chosen one.
    fn choose_encoder(cx: &mut TestAppContext, window: AnyWindowHandle, downs: usize) {
        reveal(cx, window, "pb-encoder");
        cx.update_window(window, |_, window, cx| {
            window.within("pb-encoder").click("input", cx);
            for _ in 0..downs {
                window.press("down", cx);
            }
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    fn save(cx: &mut TestAppContext, window: AnyWindowHandle) {
        click(cx, window, "pb-save-btn");
        cx.run_until_parked();
    }

    fn config_posts(fake: &FakeApi) -> Vec<Value> {
        fake.posts()
            .into_iter()
            .filter(|r| r.path == "/api/config")
            .map(|r| r.body.unwrap_or_default())
            .collect()
    }

    #[gpui::test]
    fn playback_loads_the_last_report_each_time_the_tab_opens(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), setup_report_json());

        assert!(shows(
            cx,
            window,
            &format!("Last check: {}", local_date_time(LAST_CHECK_AT))
        ));
        assert!(shows(cx, window, "Encoder: NVIDIA (NVENC)"));
        assert!(present(cx, window, "pb-setup-use"));
        let config_reads = count(&fake, "GET", "/api/config");

        fake.reply("GET", "/api/setup-check", Ok(Value::Null));
        click(cx, window, "tab-playback");
        cx.run_until_parked();

        assert!(shows(cx, window, "No check has run on this PC yet."));
        assert!(!shows(cx, window, "Encoder: NVIDIA (NVENC)"));
        assert!(!present(cx, window, "pb-setup-use"));

        let why = "The install folder is not known, so the setup check cannot run.";
        fake.reply("GET", "/api/setup-check", Err(ApiError::Server(why.into())));
        click(cx, window, "tab-playback");
        cx.run_until_parked();

        assert!(shows(cx, window, why));
        assert!(!shows(cx, window, "No check has run on this PC yet."));
        assert!(!present(cx, window, "pb-setup-use"));
        assert_eq!(count(&fake, "GET", "/api/setup-check"), 3);
        assert_eq!(count(&fake, "GET", "/api/config"), config_reads);
    }

    #[gpui::test]
    fn the_form_shows_the_saved_settings(cx: &mut TestAppContext) {
        let mut config = config_json();
        config["supervisor"]["encoder"] = json!("h264_qsv");
        config["supervisor"]["readAheadSeconds"] = json!(2.5);
        config["supervisor"]["overlay"] = json!(false);
        config["ytdlCookiesFromBrowser"] = json!("firefox");
        let (_, window, app) = open_playback(cx, config, Value::Null);
        let view = playback(cx, &app);

        let values = view.read_with(cx, |view, cx| view.values(cx));

        assert_eq!(
            values,
            [
                "h264_qsv",
                "2.5",
                "300",
                "mweb,web_embedded",
                "firefox",
                "20",
                "30"
            ]
        );
        assert!(!view.read_with(cx, |view, _| view.overlay));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("pb-overlay").checked(), Some(false));
        })
        .unwrap();
    }

    #[gpui::test]
    fn save_sends_only_what_changed(cx: &mut TestAppContext) {
        let mut config = config_json();
        config["ytdlCookiesFromBrowser"] = json!("firefox");
        let (fake, window, _) = open_playback(cx, config, Value::Null);

        type_into(cx, window, "pb-cache", "500");
        save(cx, window);

        assert_eq!(
            config_posts(&fake),
            [json!({ "supervisor": { "cacheBudgetMb": 500 } })]
        );
        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("Saved".into()), true)
        );
        cx.executor().advance_clock(SAVE_RESET);

        choose_encoder(cx, window, 3);
        save(cx, window);

        assert_eq!(
            config_posts(&fake)[1],
            json!({ "supervisor": { "encoder": "libx264", "preferredEncoder": "libx264" } })
        );
        cx.executor().advance_clock(SAVE_RESET);

        type_into(cx, window, "pb-player-client", "");
        type_into(cx, window, "pb-ytdl-cookies", "  ");
        save(cx, window);

        assert_eq!(
            config_posts(&fake)[2],
            json!({ "ytdlCookiesFromBrowser": "" })
        );
    }

    #[gpui::test]
    fn the_overlay_switch_is_saved(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), Value::Null);

        click(cx, window, "pb-overlay");
        save(cx, window);

        assert_eq!(
            config_posts(&fake),
            [json!({ "supervisor": { "overlay": false } })]
        );
    }

    #[gpui::test]
    fn save_with_nothing_changed_sends_nothing(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), Value::Null);
        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("Save Playback Settings".into()), false)
        );

        save(cx, window);

        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("No changes".into()), true)
        );
        cx.executor()
            .advance_clock(SAVE_RESET - Duration::from_millis(1));
        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("No changes".into()), true)
        );
        cx.executor().advance_clock(Duration::from_millis(1));
        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("Save Playback Settings".into()), false)
        );
        assert!(config_posts(&fake).is_empty());
    }

    #[gpui::test]
    fn save_refuses_a_bad_number(cx: &mut TestAppContext) {
        let (fake, window, app) = open_playback(cx, config_json(), Value::Null);
        let view = playback(cx, &app);

        type_into(cx, window, "pb-read-ahead", "1");
        save(cx, window);

        assert!(shows(cx, window, "Read-ahead must be at least 2."));
        assert!(view.read_with(cx, |view, _| view.is_invalid("readAheadSeconds")));
        assert!(config_posts(&fake).is_empty());
        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("Save Playback Settings".into()), false)
        );

        type_into(cx, window, "pb-cache", "50.5");
        type_into(cx, window, "pb-read-ahead", "2");
        save(cx, window);

        assert!(shows(
            cx,
            window,
            "Download cache must be a whole number, at least 50."
        ));
        assert!(!view.read_with(cx, |view, _| view.is_invalid("readAheadSeconds")));
        assert!(view.read_with(cx, |view, _| view.is_invalid("cacheBudgetMb")));
        assert!(config_posts(&fake).is_empty());

        type_into(cx, window, "pb-cache", "50");
        save(cx, window);

        assert!(!view.read_with(cx, |view, _| view.is_invalid("cacheBudgetMb")));
        assert_eq!(
            config_posts(&fake),
            [json!({ "supervisor": { "readAheadSeconds": 2, "cacheBudgetMb": 50 } })]
        );
    }

    #[gpui::test]
    fn a_failed_save_says_why(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), Value::Null);
        fake.reply(
            "POST",
            "/api/config",
            Err(ApiError::Server(
                "ytdlCookiesFromBrowser is too long".into(),
            )),
        );

        type_into(cx, window, "pb-cache", "500");
        save(cx, window);

        assert!(shows(
            cx,
            window,
            "Could not save: ytdlCookiesFromBrowser is too long"
        ));
        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("Saving...".into()), true)
        );
        cx.executor().advance_clock(SAVE_RESET);
        assert_eq!(
            button(cx, window, "pb-save-btn"),
            (Some("Save Playback Settings".into()), false)
        );
    }

    #[gpui::test]
    fn save_shows_the_restart_note_when_asked(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), Value::Null);

        type_into(cx, window, "pb-cache", "500");
        save(cx, window);
        assert!(!present(cx, window, "pb-restart-note"));
        cx.executor().advance_clock(SAVE_RESET);

        fake.reply("POST", "/api/config", Ok(saved_json(true)));
        choose_encoder(cx, window, 1);
        save(cx, window);

        reveal(cx, window, "pb-restart-note");
        assert!(shows(cx, window, NOTE_TEXT));
        assert_eq!(
            button(cx, window, "pb-restart-stream"),
            (Some("Restart stream".into()), false)
        );
    }

    #[gpui::test]
    fn a_save_loads_the_form_again(cx: &mut TestAppContext) {
        let (fake, window, app) = open_playback(cx, config_json(), Value::Null);
        let view = playback(cx, &app);
        let reads = count(&fake, "GET", "/api/config");
        let mut saved = config_json();
        saved["supervisor"]["cacheBudgetMb"] = json!(500);
        fake.reply("GET", "/api/config", Ok(saved));

        type_into(cx, window, "pb-cache", "500");
        save(cx, window);

        assert_eq!(count(&fake, "GET", "/api/config"), reads + 1);
        assert_eq!(view.read_with(cx, |view, cx| view.values(cx))[2], "500");
        cx.executor().advance_clock(SAVE_RESET);
        save(cx, window);
        assert_eq!(config_posts(&fake).len(), 1);
    }

    #[gpui::test]
    fn use_these_settings_saves_the_recommendation(cx: &mut TestAppContext) {
        let mut config = config_json();
        config["supervisor"]["encoder"] = json!("libx264");
        let (fake, window, _) = open_playback(cx, config, setup_report_json());
        fake.reply("POST", "/api/config", Ok(saved_json(true)));

        click(cx, window, "pb-setup-use");
        cx.run_until_parked();

        assert_eq!(
            config_posts(&fake),
            [json!({ "supervisor": {
                "cacheBudgetMb": 4096, "encoder": "h264_nvenc", "preferredEncoder": "h264_nvenc"
            }})]
        );
        assert!(shows(cx, window, "Settings saved."));
        reveal(cx, window, "pb-restart-note");
        assert!(shows(cx, window, NOTE_TEXT));
        cx.executor().advance_clock(TOAST_FOR);

        let mut config = config_json();
        config["supervisor"]["cacheBudgetMb"] = json!(4096);
        fake.reply("GET", "/api/config", Ok(config));
        click(cx, window, "pb-setup-use");
        cx.run_until_parked();

        assert!(shows(cx, window, "These settings are already in use."));
        assert_eq!(config_posts(&fake).len(), 1);
    }

    #[gpui::test]
    fn use_these_settings_leaves_the_encoder_when_none_works(cx: &mut TestAppContext) {
        let mut report = setup_report_json();
        report["encoder"] = json!({ "chosen": "libx264", "software": true, "works": false });
        let (fake, window, _) = open_playback(cx, config_json(), report);

        click(cx, window, "pb-setup-use");
        cx.run_until_parked();

        assert_eq!(
            config_posts(&fake),
            [json!({ "supervisor": { "cacheBudgetMb": 4096 } })]
        );
    }

    #[gpui::test]
    fn use_these_settings_says_why_a_save_failed(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), setup_report_json());
        fake.reply("GET", "/api/config", Err(ApiError::Unreachable));

        click(cx, window, "pb-setup-use");
        cx.run_until_parked();

        assert!(shows(cx, window, "Could not save: Can't reach the server."));
        assert!(config_posts(&fake).is_empty());
        assert_eq!(
            button(cx, window, "pb-setup-use"),
            (Some("Use these settings".into()), false)
        );
    }

    #[gpui::test]
    fn restart_stream_stops_then_starts_and_hides_every_note(cx: &mut TestAppContext) {
        const OTHER: RestartNote = RestartNote {
            id: "other-note",
            button: "other-restart",
            text: "Another tab's note",
        };
        let (fake, window, _) = open_playback(cx, config_json(), Value::Null);
        fake.reply("POST", "/api/config", Ok(saved_json(true)));
        choose_encoder(cx, window, 1);
        save(cx, window);
        cx.update(|cx| OTHER.show(cx));
        assert!(cx.update(|cx| NOTE.shown(cx) && OTHER.shown(cx)));

        fake.reply(
            "POST",
            "/api/stream/stop",
            Err(ApiError::Server("Supervisor not reachable".into())),
        );
        let before = fake.posts().len();
        click(cx, window, "pb-restart-stream");
        cx.run_until_parked();

        let sent: Vec<String> = fake.posts()[before..]
            .iter()
            .map(|r| r.path.clone())
            .collect();
        assert_eq!(sent, ["/api/stream/stop"]);
        assert!(shows(cx, window, "Supervisor not reachable"));
        assert!(cx.update(|cx| NOTE.shown(cx) && OTHER.shown(cx)));

        fake.reply("POST", "/api/stream/stop", Ok(json!({ "ok": true })));
        let before = fake.posts().len();
        click(cx, window, "pb-restart-stream");
        cx.run_until_parked();

        let sent: Vec<String> = fake.posts()[before..]
            .iter()
            .map(|r| r.path.clone())
            .collect();
        assert_eq!(sent, ["/api/stream/stop", "/api/stream/start"]);
        assert!(cx.update(|cx| !NOTE.shown(cx) && !OTHER.shown(cx)));
        assert!(!present(cx, window, "pb-restart-note"));
    }

    #[gpui::test]
    fn shows_the_reload_warning(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), Value::Null);
        let warning = "Saved, but the supervisor could not reload: timeout";
        fake.reply(
            "POST",
            "/api/config",
            Ok(json!({
                "ok": true, "restartRequired": false,
                "reloadError": "timeout", "reloadErrorCode": "ETIMEDOUT"
            })),
        );

        type_into(cx, window, "pb-cache", "500");
        save(cx, window);

        assert!(shows(cx, window, warning));
        cx.executor().advance_clock(TOAST_FOR);
        assert!(!shows(cx, window, warning));

        fake.reply(
            "POST",
            "/api/config",
            Ok(json!({
                "ok": true, "restartRequired": false,
                "reloadError": "connect ECONNREFUSED 127.0.0.1:4417", "reloadErrorCode": "ECONNREFUSED"
            })),
        );
        type_into(cx, window, "pb-cache", "600");
        save(cx, window);

        assert_eq!(config_posts(&fake).len(), 2);
        assert!(!shows(
            cx,
            window,
            "Saved, but the supervisor could not reload: connect ECONNREFUSED 127.0.0.1:4417"
        ));
    }

    #[gpui::test]
    fn run_checks_again_shows_checking_then_the_report(cx: &mut TestAppContext) {
        let (fake, window, _) = open_playback(cx, config_json(), Value::Null);
        fake.reply("POST", "/api/setup-check/run", Ok(setup_report_json()));
        assert_eq!(
            button(cx, window, "pb-setup-run"),
            (Some("Run checks again".into()), false)
        );

        click(cx, window, "pb-setup-run");

        assert_eq!(
            button(cx, window, "pb-setup-run"),
            (Some("Checking...".into()), true)
        );
        assert!(shows(
            cx,
            window,
            "Checking this PC. This can take up to a minute."
        ));
        cx.run_until_parked();

        assert_eq!(fake.posts()[0].body, Some(json!({})));
        assert!(shows(
            cx,
            window,
            &format!("Last check: {}", local_date_time(LAST_CHECK_AT))
        ));
        assert!(shows(cx, window, "✔ ffmpeg"));
        assert!(present(cx, window, "pb-setup-use"));
        assert_eq!(
            button(cx, window, "pb-setup-run"),
            (Some("Run checks again".into()), false)
        );

        fake.reply(
            "POST",
            "/api/setup-check/run",
            Err(ApiError::Server("ffmpeg is missing".into())),
        );
        click(cx, window, "pb-setup-run");
        cx.run_until_parked();

        assert!(shows(
            cx,
            window,
            "The check could not run: ffmpeg is missing"
        ));
        assert!(!shows(cx, window, "✔ ffmpeg"));
        // The last report still stands, so its time and "Use these settings" stay.
        assert!(shows(
            cx,
            window,
            &format!("Last check: {}", local_date_time(LAST_CHECK_AT))
        ));
        assert!(present(cx, window, "pb-setup-use"));
    }

    #[gpui::test]
    async fn run_checks_again_joins_a_run_in_flight(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", Ok(config_json()));
        fake.reply("GET", "/api/setup-check", Ok(Value::Null));
        fake.reply("POST", "/api/setup-check/run", Ok(setup_report_json()));
        let (_, window) = open_app(cx, fake.clone());
        let client = Client(fake.clone());

        // The wizard's run, still waiting for its answer.
        let wizard = cx.update(|cx| SetupCheck::run(cx, &client, Some("PLmain".into())));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("tab-playback", cx);
            window.render_frame(cx);
            window.click("pb-setup-run", cx);
            assert_eq!(window.find("pb-setup-run").label(), Some("Checking..."));
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(count(&fake, "POST", "/api/setup-check/run"), 1);
        assert_eq!(
            fake.posts()[0].body,
            Some(json!({ "playlistId": "PLmain" }))
        );
        assert_eq!(count(&fake, "GET", "/api/setup-check"), 0);
        assert!(shows(cx, window, "✔ ffmpeg"));
        assert_eq!(wizard.await.unwrap().cache.recommended_mb, 4096);
    }

    #[gpui::test]
    fn the_tab_key_reaches_every_control_in_page_order(cx: &mut TestAppContext) {
        let (_, window, _) = open_playback(cx, config_json(), setup_report_json());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("pb-encoder", cx);
            window.press("escape", cx);
        })
        .unwrap();

        let order = [
            "pb-overlay",
            "pb-setup-run",
            "pb-setup-use",
            "pb-read-ahead",
            "pb-cache",
            "pb-player-client",
            "pb-ytdl-cookies",
            "pb-freeze",
            "pb-silence",
            "pb-save-btn",
        ];
        for id in order {
            cx.update_window(window, |_, window, cx| {
                window.press("tab", cx);
                window.render_frame(cx);
                assert_eq!(window.find(id).focused(), Some(true), "{id}");
            })
            .unwrap();
        }
    }
}
