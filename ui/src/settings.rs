//! The Settings tab (index.html `#panel-settings`): playlists, the stream, the Twitch check,
//! startup, updates and the dashboard server.

use std::time::Duration;

use gpui::{
    AnyElement, AppContext, ClipboardItem, Context, Div, Entity, FontStyle, FontWeight,
    HighlightStyle, IntoElement, Render, SharedString, StyledText, Subscription, Window, div,
    prelude::*, px, relative, rgb,
};
use gpui_kit::{
    base::TestTextExt,
    component::{
        Disableable, Icon, IconName, Sizable,
        button::{Button, ButtonVariants},
        input::InputState,
        label::Label,
        switch::Switch,
    },
};
use serde_json::{Value, json};

use crate::{
    api::types::Config,
    app::{AppHandle, ToastKind},
    dashboard::TabView,
    logic::{
        admin::{check_update_result, stream_key_state, without_blanks},
        autostart_label, changed_fields,
    },
    theme::{ACCENT, BG, BORDER, MONO_FONT, MUTED, SURFACE, TEXT},
    update::{UpdateFlow, update_button},
    widgets::{
        ActionView, Busy, PlaylistRowIds, PlaylistRows, RestartNote, SAVE_RESET, btn, form_group,
        put_text, reload_warning, reveal_on_focus, section, settings_panel, text_input, text_of,
    },
};

pub const NOTE: RestartNote = RestartNote {
    id: "set-restart-note",
    button: "set-restart-stream",
    text: "The new ingest URL applies after the stream restarts. Viewers see a short drop.",
};

const ROW_IDS: PlaylistRowIds = PlaylistRowIds {
    id: "set-pl-id",
    name: "set-pl-name",
    remove: "set-pl-remove",
    add: "set-add-playlist",
};

const SAVE_LABEL: &str = "Save Settings";
const PLAYLISTS_HINT: &str = "Playlists play in this order, then start over. To switch now, press ▶ on a video on the Monitor tab.";
const INGEST_HINT: &str =
    "The default is rtmp://live.twitch.tv/app. A blank field keeps the saved URL.";
const TWITCH_LABEL: &str = "Check that Twitch is receiving the stream";
const TWITCH_DESC: &str = "Restart the stream when the encoder runs but the channel is offline, or when the picture on Twitch stops moving (for example after Twitch's 48-hour disconnect)";
const HOWTO: &str = "How do I get Twitch API credentials?";
const CONSOLE_URL: &str = "https://dev.twitch.tv/console/apps";
const REDIRECT_URL: &str = "https://localhost";
/// admin.js `copyRedirectUrl` shows "Copied" this long.
const COPIED_FOR: Duration = Duration::from_secs(2);
const AUTOSTART_LABEL: &str = "Run on Windows startup";
const AUTO_OPEN_LABEL: &str = "Open the web dashboard when StreamLoop starts";
const AUTO_OPEN_DESC: &str = "Opens the web dashboard in your browser each time the server starts";
const CHECK_LABEL: &str = "Check for Updates";
const YTDLP_LABEL: &str = "Update yt-dlp";
const YTDLP_HINT: &str = "yt-dlp updates itself on startup. Update it by hand if playlists show fewer videos than expected.";
const SHUTDOWN_LABEL: &str = "Shut down the dashboard";
const SHUTDOWN_TITLE: &str = "Shut down the dashboard server?";
const SHUTDOWN_DETAIL: &str =
    "The stream keeps running, but this window stops working until StreamLoop starts again.";

/// The Settings tab, with admin.js's settings variables as fields.
pub struct SettingsView {
    app: AppHandle,
    /// The dashboard's update banner, which Update Now runs and a check renders.
    flow: Entity<UpdateFlow>,
    busy: Busy,
    playlists: Entity<PlaylistRows>,
    ingest: Entity<InputState>,
    /// Masked. The key stays here until the one `POST /api/stream-key` that saves it.
    stream_key: Entity<InputState>,
    client_id: Entity<InputState>,
    /// Masked. It shows what the server sent, `********` for a saved secret.
    client_secret: Entity<InputState>,
    channel: Entity<InputState>,
    /// `supervisor.streamKeySet`: the state line, Set or Replace, and the key's placeholder.
    key_set: bool,
    key_saving: bool,
    twitch_enabled: bool,
    howto_open: bool,
    /// Copy reads "Copied" while set. Each copy counts up, so only the last one's timer clears it.
    copied: bool,
    copies: usize,
    /// `settingsConfig`: the config the form was filled from, which a save is measured against.
    loaded: Value,
    /// `settingsLoaded`.
    form_loaded: bool,
    /// Counts form loads; an answer to an older one is dropped.
    form_loads: usize,
    form_loading: bool,
    /// The last form load's error, until a load works.
    load_error: Option<SharedString>,
    save_label: &'static str,
    save_disabled: bool,
    autostart: bool,
    /// `#autostart-target`.
    autostart_target: SharedString,
    auto_open: bool,
    checking: bool,
    check_result: SharedString,
    /// `#settings-update-btn` shows after a check that found an update, unless in dev mode.
    update_now_shown: bool,
    ytdlp_updating: bool,
    ytdlp_result: SharedString,
    shutting_down: bool,
    _notes: Subscription,
}

impl SettingsView {
    pub fn new(
        app: AppHandle,
        flow: Entity<UpdateFlow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let playlists = cx.new(|_| PlaylistRows::new(app.clone(), ROW_IDS));
        let (_, _, key_placeholder) = stream_key_state(false);
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        Self {
            app,
            flow,
            busy: Busy::default(),
            playlists,
            ingest: input("rtmp://live.twitch.tv/app", window, cx),
            stream_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(key_placeholder)
            }),
            client_id: input("Your Twitch application Client ID", window, cx),
            client_secret: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("Your Twitch application Client Secret")
            }),
            channel: input("Your Twitch channel name (lowercase)", window, cx),
            key_set: false,
            key_saving: false,
            twitch_enabled: false,
            howto_open: false,
            copied: false,
            copies: 0,
            loaded: Value::Null,
            form_loaded: false,
            form_loads: 0,
            form_loading: false,
            load_error: None,
            save_label: SAVE_LABEL,
            save_disabled: false,
            autostart: false,
            autostart_target: "-".into(),
            auto_open: true,
            checking: false,
            check_result: SharedString::default(),
            update_now_shown: false,
            ytdlp_updating: false,
            ytdlp_result: SharedString::default(),
            shutting_down: false,
            _notes: RestartNote::observe(cx),
        }
    }

    /// admin.js `loadSettings`.
    fn load_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.form_loads += 1;
        let load = self.form_loads;
        self.form_loading = true;
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx.background_spawn(async move { client.config() }).await;
            this.update_in(cx, |this, window, cx| {
                if this.form_loads != load {
                    return;
                }
                this.form_loading = false;
                match answer {
                    Ok(config) => {
                        this.load_error = None;
                        this.fill(config, window, cx);
                    }
                    Err(err) => {
                        let error = SharedString::from(err.to_string());
                        this.load_error = Some(error.clone());
                        this.app.toast(
                            format!("Could not load settings: {error}"),
                            ToastKind::Error,
                            cx,
                        );
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn fill(&mut self, config: Value, window: &mut Window, cx: &mut Context<Self>) {
        let settings = Config::from_value(&config);
        self.playlists
            .update(cx, |rows, cx| rows.set(&settings.playlists, window, cx));
        put_text(&self.ingest, &settings.supervisor.ingest_url, window, cx);
        self.show_key_state(settings.supervisor.stream_key_set, window, cx);
        self.twitch_enabled = settings.twitch_liveness_enabled;
        put_text(&self.client_id, &settings.twitch_client_id, window, cx);
        put_text(
            &self.client_secret,
            &settings.twitch_client_secret,
            window,
            cx,
        );
        put_text(&self.channel, &settings.twitch_channel, window, cx);
        self.auto_open = settings.auto_open_admin;
        self.loaded = config;
        self.form_loaded = true;
        cx.notify();
    }

    /// admin.js `renderStreamKeyState`.
    fn show_key_state(&mut self, is_set: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.key_set = is_set;
        let (_, _, placeholder) = stream_key_state(is_set);
        self.stream_key.update(cx, |state, cx| {
            state.set_placeholder(placeholder, window, cx)
        });
    }

    /// admin.js `handleStreamKeySet`. The key goes out once, untrimmed, and the field is emptied
    /// only when the server took it.
    fn set_stream_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.key_saving {
            return;
        }
        let key = text_of(&self.stream_key, cx);
        if key.trim().is_empty() {
            self.app
                .toast("Paste the stream key first.", ToastKind::Error, cx);
            return;
        }
        self.key_saving = true;
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.set_stream_key(&key) })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.key_saving = false;
                match answer {
                    Ok(()) => {
                        put_text(&this.stream_key, "", window, cx);
                        this.show_key_state(true, window, cx);
                        this.app.toast("Stream key saved.", ToastKind::Success, cx);
                    }
                    Err(err) => this.app.toast(
                        format!("Could not save the stream key: {err}"),
                        ToastKind::Error,
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handleSettingsSave`.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.save_disabled {
            return;
        }
        if !self.form_loaded {
            // Measured against no config, every field would go out and blank the saved settings.
            match &self.load_error {
                Some(error) => self.app.toast(
                    format!("Could not load settings: {error}"),
                    ToastKind::Error,
                    cx,
                ),
                None if !self.form_loading => self.load_form(window, cx),
                None => {}
            }
            return;
        }
        let Ok(playlists) = self.playlists.update(cx, |rows, cx| rows.collect(cx)) else {
            return;
        };
        if playlists.is_empty() {
            self.app
                .toast("Add at least one playlist.", ToastKind::Error, cx);
            return;
        }
        let edited = json!({
            "playlists": playlists,
            "supervisor": without_blanks(json!({ "ingestUrl": text_of(&self.ingest, cx).trim() })),
            "twitchLivenessEnabled": self.twitch_enabled,
            "twitchClientId": text_of(&self.client_id, cx).trim(),
            "twitchClientSecret": text_of(&self.client_secret, cx),
            "twitchChannel": text_of(&self.channel, cx).trim(),
        });
        let body = changed_fields(&self.loaded, &edited);
        self.save_disabled = true;
        if body.as_object().is_none_or(|b| b.is_empty()) {
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
                .background_spawn(async move { client.save_config(body) })
                .await;
            this.update_in(cx, |this, window, cx| {
                match answer {
                    Ok(saved) => {
                        reload_warning(&this.app, &saved, cx);
                        this.save_label = "Saved";
                        if saved.restart_required {
                            NOTE.show(cx);
                        }
                        this.form_loaded = false;
                        this.load_form(window, cx);
                        this.app.playlists_saved(cx);
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

    /// admin.js `copyRedirectUrl`.
    fn copy_redirect_url(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(REDIRECT_URL.to_string()));
        self.copies += 1;
        let copy = self.copies;
        self.copied = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |this, cx| {
                if this.copies == copy {
                    this.copied = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `loadAutostart`. A failed read leaves the switch as it is.
    fn load_autostart(&mut self, cx: &mut Context<Self>) {
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx.background_spawn(async move { client.autostart() }).await;
            this.update(cx, |this, cx| {
                match answer {
                    Ok(status) => {
                        // On only when it starts this install, so a file another install owns offers the takeover.
                        this.autostart = status.this_install;
                        this.autostart_target = autostart_label(&status).into();
                    }
                    Err(_) => this.autostart_target = "-".into(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handleAutostartToggle`: the change, then the state as the server reads it.
    fn change_autostart(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.autostart = enabled;
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.set_autostart(enabled) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(err) = answer {
                    this.app.toast(
                        format!("Could not change autostart: {err}"),
                        ToastKind::Error,
                        cx,
                    );
                }
                this.load_autostart(cx);
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handleAutoOpenToggle`: a failed save puts the switch back.
    fn change_auto_open(&mut self, on: bool, cx: &mut Context<Self>) {
        self.auto_open = on;
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.save_config(json!({ "autoOpenAdmin": on })) })
                .await;
            this.update(cx, |this, cx| match answer {
                Ok(saved) => reload_warning(&this.app, &saved, cx),
                Err(err) => {
                    this.app.toast(
                        format!("Could not change the setting: {err}"),
                        ToastKind::Error,
                        cx,
                    );
                    this.auto_open = !on;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handleCheckUpdate`. An update on offer also goes to the banner.
    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        self.check_result = SharedString::default();
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.update_check() })
                .await;
            this.update(cx, |this, cx| {
                this.checking = false;
                match answer {
                    Ok(status) => {
                        this.check_result = check_update_result(&status).into();
                        this.update_now_shown = status.update_available && !status.is_dev_mode;
                        if status.update_available {
                            this.flow
                                .update(cx, |flow, cx| flow.show_status(&status, cx));
                        }
                    }
                    Err(err) => this.check_result = format!("Check failed: {err}").into(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handleYtdlpUpdate`.
    fn update_ytdlp(&mut self, cx: &mut Context<Self>) {
        if self.ytdlp_updating {
            return;
        }
        self.ytdlp_updating = true;
        self.ytdlp_result = SharedString::default();
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.ytdlp_update() })
                .await;
            this.update(cx, |this, cx| {
                this.ytdlp_updating = false;
                this.ytdlp_result = match answer {
                    Ok(update) => format!("Updated to yt-dlp {}", update.version),
                    Err(err) => format!("Update failed: {err}"),
                }
                .into();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handleShutdown`, with a native dialog for its `confirm()`.
    fn ask_to_shut_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        let ask = self
            .app
            .confirm(SHUTDOWN_TITLE, SHUTDOWN_DETAIL, "Shut down", window, cx);
        cx.spawn(async move |this, cx| {
            if !ask.await {
                return;
            }
            this.update(cx, |this, cx| this.shut_down(cx)).ok();
        })
        .detach();
    }

    /// The button stays "Shutting down..." for good: the server is going away.
    fn shut_down(&mut self, cx: &mut Context<Self>) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        cx.notify();
        let (app, client) = (self.app.clone(), self.app.client.clone());
        cx.spawn(async move |_, cx| {
            let answer = cx.background_spawn(async move { client.shutdown() }).await;
            if let Err(err) = answer {
                cx.update(|cx| app.toast(err.to_string(), ToastKind::Error, cx));
            }
        })
        .detach();
    }

    fn render_playlists(&self) -> Div {
        settings_panel().child(
            div()
                .mb(px(16.))
                .child(self.playlists.clone())
                .child(hint(PLAYLISTS_HINT)),
        )
    }

    fn render_stream(&self, cx: &mut Context<Self>) -> Div {
        let (state, button_label, _) = stream_key_state(self.key_set);
        // admin.css `.btn-secondary.btn-small`.
        let set = Button::new("set-stream-key-btn")
            .label(button_label)
            .small()
            .h(px(28.))
            .px(px(12.))
            .disabled(self.key_saving)
            .on_click(cx.listener(|this, _, window, cx| this.set_stream_key(window, cx)));
        let key = div()
            .child(
                // admin.css `.input-row`.
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(text_input(
                        &self.stream_key,
                        "set-stream-key",
                        "Twitch stream key",
                        false,
                    )))
                    .child(reveal_on_focus(set)),
            )
            .child(hint(state));
        settings_panel()
            .child(form_group(
                "Ingest URL",
                text_input(&self.ingest, "set-ingest", "Ingest URL", false),
                Some(INGEST_HINT),
            ))
            .child(form_group("Stream key", key, None))
    }

    fn render_twitch(&self, cx: &mut Context<Self>) -> Div {
        let toggle = Switch::new("twitch-enabled-toggle")
            .checked(self.twitch_enabled)
            .accessibility_label(TWITCH_LABEL)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                this.twitch_enabled = *checked;
                cx.notify();
            }));
        let channel_hint = rich(
            &[
                Run::Plain("The channel name from your Twitch URL: twitch.tv/"),
                Run::Strong("yourname"),
            ],
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..HighlightStyle::default()
            },
        );
        section("Twitch Liveness Check")
            .child(toggle_row(TWITCH_LABEL, &[TWITCH_DESC.into()], toggle))
            .child(self.render_howto(cx))
            .child(
                // admin.css `.twitch-fields`.
                settings_panel()
                    .mt(px(12.))
                    .child(form_group(
                        "Twitch Client ID",
                        text_input(
                            &self.client_id,
                            "set-twitch-client-id",
                            "Twitch Client ID",
                            false,
                        ),
                        None,
                    ))
                    .child(form_group(
                        "Twitch Client Secret",
                        text_input(
                            &self.client_secret,
                            "set-twitch-client-secret",
                            "Twitch Client Secret",
                            false,
                        ),
                        None,
                    ))
                    .child(
                        form_group(
                            "Twitch Channel Name",
                            text_input(
                                &self.channel,
                                "set-twitch-channel",
                                "Twitch Channel Name",
                                false,
                            ),
                            None,
                        )
                        .child(
                            div()
                                .mt(px(2.))
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child(channel_hint),
                        ),
                    ),
            )
    }

    /// index.html's `details.wiz-collapsible`: the summary opens and closes the steps.
    fn render_howto(&self, cx: &mut Context<Self>) -> Div {
        let marker = if self.howto_open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let summary = Button::new("set-twitch-howto")
            .text()
            .accessibility_label(HOWTO)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(Icon::new(marker).size(px(12.)).text_color(rgb(ACCENT)))
                    .child(Label::new(HOWTO).text_size(px(13.)).text_color(rgb(ACCENT))),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.howto_open = !this.howto_open;
                cx.notify();
            }));
        div()
            .mt(px(8.))
            .child(div().flex().py(px(4.)).child(reveal_on_focus(summary)))
            .when(self.howto_open, |d| d.child(self.render_steps(cx)))
    }

    /// index.html's `ol.wiz-instructions`, with its bold and italic words.
    fn render_steps(&self, cx: &mut Context<Self>) -> Div {
        use Run::{Em, Plain, Strong};
        let strong = HighlightStyle {
            color: Some(rgb(TEXT).into()),
            font_weight: Some(FontWeight::MEDIUM),
            ..HighlightStyle::default()
        };
        let line = |runs: &[Run]| rich(runs, strong);
        let link = Button::new("set-twitch-console-link")
            .link()
            .accessibility_label("dev.twitch.tv/console/apps")
            .child(
                Label::new("dev.twitch.tv/console/apps")
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(TEXT)),
            )
            .on_click(|_, _, cx| cx.open_url(CONSOLE_URL));
        // A space's width between the link and the words around it.
        let first = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(3.5))
            .child(line(&[Plain("Go to")]))
            .child(reveal_on_focus(link))
            .child(line(&[
                Plain("and log in with "),
                Strong("the Twitch account you stream from"),
            ]));
        let form = div().child(line(&[Plain("Fill in the form:")])).child(
            // admin.css `.wiz-sublist`.
            div()
                .my(px(6.))
                .child(bullet(line(&[
                    Strong("Name"),
                    Plain(": anything you like (e.g. "),
                    Em("StreamLoop"),
                    Plain(")"),
                ])))
                .child(bullet(
                    div()
                        .child(line(&[
                            Strong("OAuth Redirect URLs"),
                            Plain(": copy the value below, paste it in, and click "),
                            Strong("Add"),
                            Plain(". Twitch requires it; StreamLoop doesn't use it."),
                        ]))
                        .child(self.render_copy_row(cx)),
                ))
                .child(bullet(line(&[
                    Strong("Category"),
                    Plain(": select "),
                    Strong("Application Integration"),
                ])))
                .child(bullet(line(&[
                    Strong("Client Type"),
                    Plain(": select "),
                    Strong("Confidential"),
                ]))),
        );
        let steps: [AnyElement; 7] = [
            first.into_any_element(),
            line(&[Plain("Click "), Strong("Register Your Application")]).into_any_element(),
            form.into_any_element(),
            line(&[Plain("Click "), Strong("Create")]).into_any_element(),
            line(&[
                Plain("Back in the app list, click "),
                Strong("Manage"),
                Plain(" on your new app."),
            ])
            .into_any_element(),
            line(&[
                Plain("Copy the "),
                Strong("Client ID"),
                Plain(" and paste it below"),
            ])
            .into_any_element(),
            line(&[
                Plain("Click "),
                Strong("New Secret"),
                Plain(", confirm, then copy the "),
                Strong("Client Secret"),
                Plain(" and paste it below. Twitch shows it only once."),
            ])
            .into_any_element(),
        ];
        // admin.css `.wiz-instructions`.
        div()
            .mt(px(8.))
            .text_size(px(13.))
            .text_color(rgb(MUTED))
            .line_height(relative(1.7))
            .children(
                steps
                    .into_iter()
                    .enumerate()
                    .map(|(index, step)| list_item(format!("{}.", index + 1), step)),
            )
    }

    /// admin.css `.wiz-copy-row`: the redirect URL and Copy, which turns green as "Copied".
    fn render_copy_row(&self, cx: &mut Context<Self>) -> Div {
        let copy = Button::new("copy-redirect-btn")
            .label(if self.copied { "Copied" } else { "Copy" })
            .small()
            .h(px(28.))
            .px(px(12.))
            // admin.css `.btn-copied`, through the theme's success button.
            .when(self.copied, |button| button.success())
            .on_click(cx.listener(|this, _, _, cx| this.copy_redirect_url(cx)));
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .mt(px(6.))
            .px(px(12.))
            .py(px(8.))
            .bg(rgb(BG))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded(px(8.))
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(REDIRECT_URL)
                        .font_family(MONO_FONT)
                        .text_size(px(13.))
                        .text_color(rgb(ACCENT)),
                ),
            )
            .child(reveal_on_focus(copy))
    }

    fn render_startup(&self, cx: &mut Context<Self>) -> Div {
        let autostart = Switch::new("autostart-toggle")
            .checked(self.autostart)
            .accessibility_label(AUTOSTART_LABEL)
            .on_click(
                cx.listener(|this, checked: &bool, _, cx| this.change_autostart(*checked, cx)),
            );
        let auto_open = Switch::new("auto-open-toggle")
            .checked(self.auto_open)
            .accessibility_label(AUTO_OPEN_LABEL)
            .on_click(
                cx.listener(|this, checked: &bool, _, cx| this.change_auto_open(*checked, cx)),
            );
        let target: SharedString = format!("Autostart: {}", self.autostart_target).into();
        // admin.css `.section-gap`.
        section("Startup")
            .mt(px(24.))
            .child(toggle_row(
                AUTOSTART_LABEL,
                &["Start StreamLoop when Windows starts".into(), target],
                autostart,
            ))
            .child(
                // admin.css `.toggle-row-stacked`.
                toggle_row(AUTO_OPEN_LABEL, &[AUTO_OPEN_DESC.into()], auto_open).mt(px(8.)),
            )
    }

    fn render_updates(&self, cx: &mut Context<Self>) -> Div {
        let check = btn(Button::new("check-update-btn")
            .label(if self.checking {
                "Checking..."
            } else {
                CHECK_LABEL
            })
            .disabled(self.checking))
        .on_click(cx.listener(|this, _, _, cx| this.check_for_updates(cx)));
        let update_now = self.update_now_shown.then(|| {
            update_button("settings-update-btn", "Update Now", true, cx).on_click(
                cx.listener(|this, _, _, cx| this.flow.update(cx, |flow, cx| flow.apply(cx))),
            )
        });
        let ytdlp = btn(Button::new("update-ytdlp-btn")
            .label(if self.ytdlp_updating {
                "Updating..."
            } else {
                YTDLP_LABEL
            })
            .disabled(self.ytdlp_updating))
        .on_click(cx.listener(|this, _, _, cx| this.update_ytdlp(cx)));
        section("Updates")
            .child(action_row(
                [check].into_iter().chain(update_now),
                self.check_result.clone(),
            ))
            .child(action_row([ytdlp], self.ytdlp_result.clone()))
            .child(hint(YTDLP_HINT))
    }

    fn render_server(&self, cx: &mut Context<Self>) -> Div {
        let shutdown = btn(Button::new("shutdown-btn")
            .danger()
            .label(if self.shutting_down {
                "Shutting down..."
            } else {
                SHUTDOWN_LABEL
            })
            .disabled(self.shutting_down))
        .on_click(cx.listener(|this, _, window, cx| this.ask_to_shut_down(window, cx)));
        section("Dashboard Server").child(action_row(
            [shutdown],
            "Stops the dashboard server only. The stream keeps running.".into(),
        ))
    }
}

#[cfg(test)]
impl SettingsView {
    fn stream_key_text(&self, cx: &gpui::App) -> String {
        text_of(&self.stream_key, cx)
    }

    /// Ingest URL, Twitch Client ID, Client Secret and Channel Name.
    fn values(&self, cx: &gpui::App) -> Vec<String> {
        [
            &self.ingest,
            &self.client_id,
            &self.client_secret,
            &self.channel,
        ]
        .into_iter()
        .map(|input| text_of(input, cx))
        .collect()
    }
}

impl ActionView for SettingsView {
    fn busy(&mut self) -> &mut Busy {
        &mut self.busy
    }
}

impl TabView for SettingsView {
    /// admin.js `showDashboard()`: `loadSettings()` and `loadAutostart()`.
    fn on_dashboard_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load_form(window, cx);
        self.load_autostart(cx);
    }

    /// admin.js `switchTab('settings')`: the form again only when it is not loaded.
    fn on_tab_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.form_loaded && !self.form_loading {
            self.load_form(window, cx);
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let save = btn(Button::new("set-btn")
            .primary()
            .label(self.save_label)
            .disabled(self.save_disabled))
        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        div()
            .child(section("Playlists").child(self.render_playlists()))
            .child(section("Stream").child(self.render_stream(cx)))
            .child(self.render_twitch(cx))
            // As wide as its label, as the inline button in index.html.
            .child(div().flex().child(reveal_on_focus(save)))
            .children(NOTE.render(&self.app, &self.busy, cx))
            .child(self.render_startup(cx))
            .child(self.render_updates(cx))
            .child(self.render_server(cx))
    }
}

/// A run of `rich` text: plain, `<strong>` or `<em>`.
#[derive(Clone, Copy)]
enum Run {
    Plain(&'static str),
    Strong(&'static str),
    Em(&'static str),
}

/// One wrapping line of text with its `<strong>` runs drawn in `strong` and `<em>` runs in italics.
/// Size and colour come from the surrounding element.
fn rich(runs: &[Run], strong: HighlightStyle) -> impl IntoElement + use<> {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for run in runs {
        let start = text.len();
        let (words, style) = match *run {
            Run::Plain(words) => (words, None),
            Run::Strong(words) => (words, Some(strong)),
            Run::Em(words) => (
                words,
                Some(HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..HighlightStyle::default()
                }),
            ),
        };
        text.push_str(words);
        if let Some(style) = style {
            highlights.push((start..text.len(), style));
        }
    }
    let text = SharedString::from(text);
    div()
        .child(StyledText::new(text.clone()).with_highlights(highlights))
        .test_text(text)
}

/// One `<li>` of the numbered steps: its number in the 20 px gutter of the `<ol>`.
fn list_item(marker: String, content: AnyElement) -> Div {
    div()
        .flex()
        .mb(px(4.))
        .child(
            div()
                .w(px(20.))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .pr(px(4.))
                .child(marker),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// One `<li>` of `.wiz-sublist`: a disc in its 16 px margin.
fn bullet(content: impl IntoElement) -> Div {
    div()
        .flex()
        .child(
            div()
                .w(px(16.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .child("•"),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// admin.css `.hint`.
fn hint(text: impl Into<SharedString>) -> Label {
    Label::new(text)
        .mt(px(2.))
        .text_size(px(11.))
        .text_color(rgb(MUTED))
}

/// admin.css `.toggle-row`: the label and its descriptions, the switch on the right.
fn toggle_row(label: &'static str, descs: &[SharedString], toggle: Switch) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .px(px(16.))
        .py(px(12.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(8.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(label).text_size(px(14.)))
                .children(descs.iter().map(|desc| {
                    Label::new(desc.clone())
                        .text_size(px(12.))
                        .text_color(rgb(MUTED))
                })),
        )
        .child(reveal_on_focus(toggle))
}

/// admin.css `.action-row`: its buttons, then the `.result-line`.
fn action_row(buttons: impl IntoIterator<Item = Button>, result: SharedString) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(12.))
        .mb(px(12.))
        .children(buttons.into_iter().map(reveal_on_focus))
        .when(!result.is_empty(), |row| {
            row.child(Label::new(result).text_size(px(13.)).text_color(rgb(MUTED)))
        })
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use gpui::{AnyWindowHandle, ElementId, ScrollDelta, TestAppContext, point, px};
    use gpui_kit::test::TestWindowExt;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi},
        app::{AppView, TOAST_FOR},
        dashboard::Tab,
        test_support::{answer_polls, config_json, count, open_app, painted, saved_json, shows},
        widgets::SAVE_RESET,
    };

    const VIDEO_URL: &str = "That looks like a single video URL. Paste a playlist URL or a playlist ID (starts with PL).";
    const NOTE_TEXT: &str =
        "The new ingest URL applies after the stream restarts. Viewers see a short drop.";
    const SHUTDOWN_TITLE: &str = "Shut down the dashboard server?";
    const SHUTDOWN_DETAIL: &str =
        "The stream keeps running, but this window stops working until StreamLoop starts again.";
    const OTHER_BAT: &str = r"C:\StreamLoop3-old\streamloop\START.bat";

    /// `GET /api/autostart`.
    fn autostart_json(enabled: bool, target: Option<&str>, this_install: bool) -> Value {
        json!({ "enabled": enabled, "target": target, "thisInstall": this_install })
    }

    fn open_settings(
        cx: &mut TestAppContext,
        config: Value,
    ) -> (Arc<FakeApi>, AnyWindowHandle, Entity<AppView>) {
        open_settings_with(cx, Ok(config), autostart_json(false, None, false))
    }

    /// Opens the dashboard on `config` and `autostart`, then the Settings tab. Every POST the tab
    /// sends answers as Node does when it works.
    fn open_settings_with(
        cx: &mut TestAppContext,
        config: Result<Value, ApiError>,
        autostart: Value,
    ) -> (Arc<FakeApi>, AnyWindowHandle, Entity<AppView>) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", config);
        fake.reply("GET", "/api/autostart", Ok(autostart));
        fake.reply("POST", "/api/config", Ok(saved_json(false)));
        fake.reply(
            "POST",
            "/api/stream-key",
            Ok(json!({ "ok": true, "streamKeySet": true })),
        );
        fake.reply("POST", "/api/autostart", Ok(json!({ "ok": true })));
        fake.reply(
            "POST",
            "/api/yt-dlp/update",
            Ok(json!({ "ok": true, "version": "2026.11.01" })),
        );
        fake.reply(
            "POST",
            "/api/shutdown",
            Ok(json!({ "ok": true, "message": "Shutting down..." })),
        );
        let (app, window) = open_app(cx, fake.clone());
        click(cx, window, "tab-settings");
        cx.run_until_parked();
        (fake, window, app)
    }

    fn settings(cx: &mut TestAppContext, app: &Entity<AppView>) -> Entity<SettingsView> {
        let dashboard = app
            .read_with(cx, |app, _| app.dashboard())
            .expect("the dashboard shows");
        dashboard
            .read_with(cx, |dashboard, _| dashboard.tab_view(Tab::Settings))
            .downcast::<SettingsView>()
            .unwrap_or_else(|_| panic!("the Settings tab is a SettingsView"))
    }

    fn rows(cx: &mut TestAppContext, view: &Entity<SettingsView>) -> Entity<PlaylistRows> {
        view.read_with(cx, |view, _| view.playlists.clone())
    }

    /// The key of the playlist row at `index`, which its element ids carry.
    fn row_key(cx: &mut TestAppContext, view: &Entity<SettingsView>, index: usize) -> usize {
        let rows = rows(cx, view);
        rows.read_with(cx, |rows, _| rows.keys()[index])
    }

    fn row_values(cx: &mut TestAppContext, view: &Entity<SettingsView>) -> Vec<(String, String)> {
        let rows = rows(cx, view);
        rows.read_with(cx, |rows, cx| rows.values(cx))
    }

    fn marks(cx: &mut TestAppContext, view: &Entity<SettingsView>) -> Vec<bool> {
        let rows = rows(cx, view);
        rows.read_with(cx, |rows, _| rows.marks())
    }

    /// Controls the wheel can be sent to: the window draws only part of the tab.
    const LANDMARKS: [&str; 15] = [
        "tab-settings",
        "set-add-playlist",
        "set-ingest",
        "set-stream-key",
        "set-stream-key-btn",
        "twitch-enabled-toggle",
        "set-twitch-howto",
        "set-twitch-client-id",
        "set-twitch-client-secret",
        "set-twitch-channel",
        "set-btn",
        "autostart-toggle",
        "auto-open-toggle",
        "update-ytdlp-btn",
        "shutdown-btn",
    ];

    /// Scrolls the dashboard so `id` sits in the middle of the window.
    fn reveal(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
        let id = id.into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let height = window.viewport_size().height;
            let dy = height / 2. - window.find(id.clone()).bounds().center().y;
            let on_screen = LANDMARKS
                .into_iter()
                .find(|c| {
                    window.try_find(*c).is_some_and(|e| {
                        let y = e.bounds().center().y;
                        y > px(0.) && y < height
                    })
                })
                .expect("a control on screen");
            window.scroll(on_screen, ScrollDelta::Pixels(point(px(0.), dy)), cx);
            window.render_frame(cx);
            let y = window.find(id.clone()).bounds().center().y;
            assert!(
                y > px(0.) && y < height,
                "{id:?} on screen after the scroll"
            );
        })
        .unwrap();
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
        let id = id.into();
        reveal(cx, window, id.clone());
        cx.update_window(window, |_, window, cx| window.click(id, cx))
            .unwrap();
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
        id: impl Into<ElementId>,
    ) -> (Option<String>, bool) {
        let id = id.into();
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

    /// An element's accessible label and placeholder.
    fn named(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        id: impl Into<ElementId>,
    ) -> (Option<String>, Option<String>) {
        let id = id.into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let element = window.find(id);
            (
                element.label().map(str::to_string),
                element.placeholder().map(str::to_string),
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

    fn save(cx: &mut TestAppContext, window: AnyWindowHandle) {
        click(cx, window, "set-btn");
        cx.run_until_parked();
    }

    fn bodies(fake: &FakeApi, path: &str) -> Vec<Value> {
        fake.posts()
            .into_iter()
            .filter(|r| r.path == path)
            .map(|r| r.body.unwrap_or_default())
            .collect()
    }

    fn config_posts(fake: &FakeApi) -> Vec<Value> {
        bodies(fake, "/api/config")
    }

    /// How many times `text` is painted, as several toasts of one text are.
    fn painted_count(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) -> usize {
        painted(cx, window).iter().filter(|t| *t == text).count()
    }

    #[gpui::test]
    fn the_form_shows_the_saved_settings(cx: &mut TestAppContext) {
        let mut config = config_json();
        config["playlists"] = json!([{ "id": "PLmain", "name": "Main" }, { "id": "PLnoname" }]);
        config["supervisor"]["ingestUrl"] = json!("rtmp://ingest.test/app");
        config["twitchLivenessEnabled"] = json!(true);
        config["twitchClientId"] = json!("abc123");
        config["twitchClientSecret"] = json!("********");
        config["twitchChannel"] = json!("loopchannel");
        config["autoOpenAdmin"] = json!(false);
        let autostart = autostart_json(true, Some(r"C:\StreamLoop3\streamloop\START.bat"), true);
        let (_, window, app) = open_settings_with(cx, Ok(config), autostart);
        let view = settings(cx, &app);

        assert_eq!(
            row_values(cx, &view),
            [
                ("PLmain".to_string(), "Main".to_string()),
                ("PLnoname".to_string(), String::new())
            ]
        );
        assert_eq!(
            view.read_with(cx, |view, cx| view.values(cx)),
            [
                "rtmp://ingest.test/app",
                "abc123",
                "********",
                "loopchannel"
            ]
        );
        assert!(shows(cx, window, "A stream key is set."));
        for hint in [
            "Playlists play in this order, then start over. To switch now, press ▶ on a video on the Monitor tab.",
            "The default is rtmp://live.twitch.tv/app. A blank field keeps the saved URL.",
        ] {
            assert!(shows(cx, window, hint), "{hint}");
        }
        assert_eq!(checked(cx, window, "twitch-enabled-toggle"), Some(true));
        assert_eq!(checked(cx, window, "auto-open-toggle"), Some(false));
        assert_eq!(checked(cx, window, "autostart-toggle"), Some(true));
        reveal(cx, window, "set-twitch-channel");
        assert!(shows(
            cx,
            window,
            "The channel name from your Twitch URL: twitch.tv/yourname"
        ));
        reveal(cx, window, "autostart-toggle");
        assert!(shows(cx, window, "Autostart: This install"));
        let key = row_key(cx, &view, 0);
        assert_eq!(
            named(cx, window, ("set-pl-id", key)),
            (
                Some("Playlist ID or URL".into()),
                Some("PLxxxxxxxxxxxxxxxx".into())
            )
        );
        assert_eq!(
            named(cx, window, ("set-pl-name", key)),
            (Some("Playlist name".into()), Some("Name (optional)".into()))
        );
        assert_eq!(
            button(cx, window, ("set-pl-remove", key)),
            (Some("Remove playlist".into()), false)
        );
        // Both secrets are masked, so neither is drawn or read out.
        assert!(view.read_with(cx, |view, cx| {
            view.stream_key.read(cx).presentation().is_masked()
                && view.client_secret.read(cx).presentation().is_masked()
        }));
    }

    #[gpui::test]
    fn loads_once_until_its_own_save(cx: &mut TestAppContext) {
        let (fake, window, app) = open_settings_with(
            cx,
            Err(ApiError::Server("config.json is not valid JSON".into())),
            autostart_json(false, None, false),
        );
        let view = settings(cx, &app);
        assert!(shows(
            cx,
            window,
            "Could not load settings: config.json is not valid JSON"
        ));
        // The Monitor tab's dropdown read it once; the form when the dashboard opened, and again
        // when the tab showed, since that first load failed.
        assert_eq!(count(&fake, "GET", "/api/config"), 3);
        assert_eq!(count(&fake, "GET", "/api/autostart"), 1);

        fake.reply("GET", "/api/config", Ok(config_json()));
        click(cx, window, "tab-settings");
        cx.run_until_parked();

        assert_eq!(count(&fake, "GET", "/api/config"), 4);
        assert_eq!(row_values(cx, &view).len(), 2);

        click(cx, window, "tab-settings");
        cx.run_until_parked();
        assert_eq!(count(&fake, "GET", "/api/config"), 4);

        let key = row_key(cx, &view, 0);
        type_into(cx, window, ("set-pl-name", key), "Renamed");
        save(cx, window);

        // The form and the Monitor tab's dropdown read it again.
        assert_eq!(count(&fake, "GET", "/api/config"), 6);
        click(cx, window, "tab-settings");
        cx.run_until_parked();
        assert_eq!(count(&fake, "GET", "/api/config"), 6);
        assert_eq!(count(&fake, "GET", "/api/autostart"), 1);
    }

    #[gpui::test]
    fn a_save_before_the_form_loads_sends_nothing(cx: &mut TestAppContext) {
        let why = "config.json is not valid JSON";
        let (fake, window, app) = open_settings_with(
            cx,
            Err(ApiError::Server(why.into())),
            autostart_json(false, None, false),
        );
        let view = settings(cx, &app);
        click(cx, window, "set-add-playlist");
        let key = row_key(cx, &view, 0);
        type_into(cx, window, ("set-pl-id", key), "PLnew");
        cx.executor().advance_clock(TOAST_FOR);
        let reads = count(&fake, "GET", "/api/config");

        save(cx, window);

        // A blank form measured against nothing would blank every saved setting.
        assert!(config_posts(&fake).is_empty());
        let toast = format!("Could not load settings: {why}");
        assert_eq!(painted_count(cx, window, &toast), 1);
        assert_eq!(count(&fake, "GET", "/api/config"), reads);
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("Save Settings".into()), false)
        );

        // With no load error to show, Save loads the form again instead.
        fake.reply("GET", "/api/config", Ok(config_json()));
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.load_error = None;
                view.save(window, cx);
            })
        })
        .unwrap();
        cx.run_until_parked();

        assert!(config_posts(&fake).is_empty());
        assert_eq!(count(&fake, "GET", "/api/config"), reads + 1);
        assert_eq!(row_values(cx, &view).len(), 2);
    }

    #[gpui::test]
    fn stream_key_set_clears_the_field(cx: &mut TestAppContext) {
        let mut config = config_json();
        config["supervisor"]["streamKeySet"] = json!(false);
        let (fake, window, app) = open_settings(cx, config);
        let view = settings(cx, &app);
        assert!(shows(
            cx,
            window,
            "No stream key is set. The stream cannot start without one."
        ));
        assert_eq!(
            button(cx, window, "set-stream-key-btn"),
            (Some("Set".into()), false)
        );
        assert_eq!(
            named(cx, window, "set-stream-key"),
            (
                Some("Twitch stream key".into()),
                Some("Paste your Twitch stream key".into())
            )
        );

        click(cx, window, "set-stream-key-btn");
        cx.run_until_parked();
        assert!(shows(cx, window, "Paste the stream key first."));
        type_into(cx, window, "set-stream-key", "   ");
        click(cx, window, "set-stream-key-btn");
        cx.run_until_parked();
        assert_eq!(count(&fake, "POST", "/api/stream-key"), 0);

        type_into(cx, window, "set-stream-key", "live_abc");
        click(cx, window, "set-stream-key-btn");
        assert_eq!(
            button(cx, window, "set-stream-key-btn"),
            (Some("Set".into()), true)
        );
        cx.run_until_parked();

        assert_eq!(
            bodies(&fake, "/api/stream-key"),
            [json!({ "key": "live_abc" })]
        );
        assert_eq!(view.read_with(cx, |view, cx| view.stream_key_text(cx)), "");
        assert!(shows(cx, window, "A stream key is set."));
        assert!(shows(cx, window, "Stream key saved."));
        assert_eq!(
            button(cx, window, "set-stream-key-btn"),
            (Some("Replace".into()), false)
        );
        assert_eq!(
            named(cx, window, "set-stream-key").1.as_deref(),
            Some("Paste a new key to replace it")
        );

        let why = "The supervisor program was not found, so the key cannot be encrypted.";
        fake.reply("POST", "/api/stream-key", Err(ApiError::Server(why.into())));
        type_into(cx, window, "set-stream-key", "live_def");
        click(cx, window, "set-stream-key-btn");
        cx.run_until_parked();

        assert!(shows(
            cx,
            window,
            &format!("Could not save the stream key: {why}")
        ));
        assert_eq!(
            view.read_with(cx, |view, cx| view.stream_key_text(cx)),
            "live_def"
        );
        assert_eq!(
            button(cx, window, "set-stream-key-btn"),
            (Some("Replace".into()), false)
        );
    }

    #[gpui::test]
    fn save_sends_only_changed_fields(cx: &mut TestAppContext) {
        let mut config = config_json();
        config["twitchClientSecret"] = json!("********");
        config["twitchClientId"] = json!("myclient");
        let (fake, window, app) = open_settings(cx, config);
        let view = settings(cx, &app);

        let first = row_key(cx, &view, 0);
        type_into(cx, window, ("set-pl-name", first), "Renamed");
        save(cx, window);

        assert_eq!(
            config_posts(&fake),
            [json!({ "playlists": [
                { "id": "PLmain", "name": "Renamed" }, { "id": "PLsecond", "name": "Second" }
            ]})]
        );
        assert_eq!(button(cx, window, "set-btn"), (Some("Saved".into()), true));
        cx.executor().advance_clock(SAVE_RESET);
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("Save Settings".into()), false)
        );

        // A blank ingest field keeps the saved URL, and the masked secret was not touched.
        type_into(cx, window, "set-ingest", "");
        save(cx, window);
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("No changes".into()), true)
        );
        assert_eq!(config_posts(&fake).len(), 1);
        cx.executor()
            .advance_clock(SAVE_RESET - Duration::from_millis(1));
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("No changes".into()), true)
        );
        cx.executor().advance_clock(Duration::from_millis(1));
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("Save Settings".into()), false)
        );

        type_into(cx, window, "set-ingest", " rtmp://ingest.test/app ");
        type_into(cx, window, "set-twitch-client-id", " myclient ");
        type_into(cx, window, "set-twitch-client-secret", " new secret ");
        type_into(cx, window, "set-twitch-channel", " loopchannel ");
        click(cx, window, "twitch-enabled-toggle");
        save(cx, window);

        assert_eq!(
            config_posts(&fake)[1],
            json!({
                "supervisor": { "ingestUrl": "rtmp://ingest.test/app" },
                "twitchLivenessEnabled": true,
                "twitchClientSecret": " new secret ",
                "twitchChannel": "loopchannel"
            })
        );
    }

    #[gpui::test]
    fn save_needs_a_playlist(cx: &mut TestAppContext) {
        let (fake, window, app) = open_settings(cx, config_json());
        let view = settings(cx, &app);

        for _ in 0..2 {
            let key = row_key(cx, &view, 0);
            click(cx, window, ("set-pl-remove", key));
        }
        assert!(row_values(cx, &view).is_empty());
        save(cx, window);

        assert!(shows(cx, window, "Add at least one playlist."));
        assert!(config_posts(&fake).is_empty());
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("Save Settings".into()), false)
        );

        // A row left blank is no playlist either.
        click(cx, window, "set-add-playlist");
        assert_eq!(row_values(cx, &view), [(String::new(), String::new())]);
        cx.executor().advance_clock(TOAST_FOR);
        save(cx, window);

        assert!(shows(cx, window, "Add at least one playlist."));
        assert!(config_posts(&fake).is_empty());
    }

    #[gpui::test]
    fn add_playlist_appends_a_row_that_is_saved(cx: &mut TestAppContext) {
        let (fake, window, app) = open_settings(cx, config_json());
        let view = settings(cx, &app);
        assert_eq!(
            button(cx, window, "set-add-playlist"),
            (Some("+ Add Playlist".into()), false)
        );

        click(cx, window, "set-add-playlist");
        let added = row_key(cx, &view, 2);
        type_into(
            cx,
            window,
            ("set-pl-id", added),
            "https://www.youtube.com/playlist?list=PLthird",
        );
        let second = row_key(cx, &view, 1);
        click(cx, window, ("set-pl-remove", second));
        save(cx, window);

        assert_eq!(
            config_posts(&fake),
            [json!({ "playlists": [{ "id": "PLmain", "name": "Main" }, { "id": "PLthird" }] })]
        );
    }

    #[gpui::test]
    fn a_video_url_marks_its_row_and_sends_nothing(cx: &mut TestAppContext) {
        let (fake, window, app) = open_settings(cx, config_json());
        let view = settings(cx, &app);
        let (a, b) = (row_key(cx, &view, 0), row_key(cx, &view, 1));

        type_into(
            cx,
            window,
            ("set-pl-id", a),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        );
        type_into(cx, window, ("set-pl-id", b), "https://youtu.be/dQw4w9WgXcQ");
        save(cx, window);

        assert_eq!(painted_count(cx, window, VIDEO_URL), 2);
        assert_eq!(marks(cx, &view), [true, true]);
        assert!(config_posts(&fake).is_empty());
        cx.executor().advance_clock(TOAST_FOR);

        type_into(
            cx,
            window,
            ("set-pl-id", a),
            "https://www.youtube.com/playlist?list=PLfixed",
        );
        save(cx, window);

        assert_eq!(painted_count(cx, window, VIDEO_URL), 1);
        assert_eq!(marks(cx, &view), [false, true]);
        assert!(config_posts(&fake).is_empty());
        cx.executor().advance_clock(TOAST_FOR);

        type_into(cx, window, ("set-pl-id", b), "PLother");
        save(cx, window);

        assert_eq!(marks(cx, &view), [false, false]);
        assert_eq!(
            config_posts(&fake),
            [json!({ "playlists": [
                { "id": "PLfixed", "name": "Main" }, { "id": "PLother", "name": "Second" }
            ]})]
        );
    }

    #[gpui::test]
    fn a_second_save_click_while_saving_sends_nothing(cx: &mut TestAppContext) {
        let (fake, window, app) = open_settings(cx, config_json());
        let view = settings(cx, &app);
        let key = row_key(cx, &view, 0);
        type_into(cx, window, ("set-pl-name", key), "Renamed");

        reveal(cx, window, "set-btn");
        cx.update_window(window, |_, window, cx| {
            window.click("set-btn", cx);
            window.click("set-btn", cx);
            // Past the disabled button too, as a keyboard or a stale frame could reach it.
            view.update(cx, |view, cx| view.save(window, cx));
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(config_posts(&fake).len(), 1);
    }

    #[gpui::test]
    fn save_reloads_the_monitor_dropdown(cx: &mut TestAppContext) {
        let (fake, window, app) = open_settings(cx, config_json());
        let view = settings(cx, &app);
        let reads = count(&fake, "GET", "/api/config");
        let mut renamed = config_json();
        renamed["playlists"][0]["name"] = json!("Main VODs");
        fake.reply("GET", "/api/config", Ok(renamed));

        let key = row_key(cx, &view, 0);
        type_into(cx, window, ("set-pl-name", key), "Main VODs");
        save(cx, window);

        assert_eq!(count(&fake, "GET", "/api/config"), reads + 2);
        assert_eq!(row_values(cx, &view)[0].1, "Main VODs");
        click(cx, window, "tab-monitor");
        cx.run_until_parked();
        let chosen = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.find("playlist-select").value().map(str::to_string)
            })
            .unwrap();
        assert_eq!(chosen.as_deref(), Some("Main VODs (playing)"));
    }

    #[gpui::test]
    fn save_shows_the_restart_note_when_asked(cx: &mut TestAppContext) {
        let (fake, window, _) = open_settings(cx, config_json());

        type_into(cx, window, "set-ingest", "rtmp://ingest.test/app");
        save(cx, window);
        assert!(!present(cx, window, "set-restart-note"));
        cx.executor().advance_clock(SAVE_RESET);

        fake.reply("POST", "/api/config", Ok(saved_json(true)));
        type_into(cx, window, "set-ingest", "rtmp://other.test/app");
        save(cx, window);

        reveal(cx, window, "set-restart-note");
        assert!(shows(cx, window, NOTE_TEXT));
        assert_eq!(
            button(cx, window, "set-restart-stream"),
            (Some("Restart stream".into()), false)
        );
    }

    #[gpui::test]
    fn a_failed_save_says_why(cx: &mut TestAppContext) {
        let (fake, window, app) = open_settings(cx, config_json());
        let view = settings(cx, &app);
        let reads = count(&fake, "GET", "/api/config");
        fake.reply(
            "POST",
            "/api/config",
            Err(ApiError::Server("Invalid playlist ID".into())),
        );

        let key = row_key(cx, &view, 0);
        type_into(cx, window, ("set-pl-name", key), "Renamed");
        save(cx, window);

        assert!(shows(cx, window, "Could not save: Invalid playlist ID"));
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("Saving...".into()), true)
        );
        assert_eq!(count(&fake, "GET", "/api/config"), reads);
        cx.executor().advance_clock(SAVE_RESET);
        assert_eq!(
            button(cx, window, "set-btn"),
            (Some("Save Settings".into()), false)
        );
    }

    #[gpui::test]
    fn the_credentials_link_opens_the_twitch_console(cx: &mut TestAppContext) {
        let (fake, window, _) = open_settings(cx, config_json());
        assert_eq!(
            button(cx, window, "set-twitch-howto").0.as_deref(),
            Some("How do I get Twitch API credentials?")
        );
        assert!(!present(cx, window, "set-twitch-console-link"));
        assert!(!shows(cx, window, "Click Register Your Application"));

        click(cx, window, "set-twitch-howto");

        assert!(shows(cx, window, "Click Register Your Application"));
        assert!(shows(
            cx,
            window,
            "Name: anything you like (e.g. StreamLoop)"
        ));
        click(cx, window, "set-twitch-console-link");
        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://dev.twitch.tv/console/apps")
        );
        assert!(fake.posts().is_empty());

        click(cx, window, "set-twitch-howto");
        assert!(!present(cx, window, "set-twitch-console-link"));
    }

    #[gpui::test]
    fn copy_puts_the_redirect_url_on_the_clipboard(cx: &mut TestAppContext) {
        let (_, window, _) = open_settings(cx, config_json());
        click(cx, window, "set-twitch-howto");
        assert!(shows(cx, window, "https://localhost"));
        assert_eq!(
            button(cx, window, "copy-redirect-btn"),
            (Some("Copy".into()), false)
        );

        click(cx, window, "copy-redirect-btn");

        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("https://localhost")
        );
        assert_eq!(
            button(cx, window, "copy-redirect-btn").0.as_deref(),
            Some("Copied")
        );
        cx.executor().advance_clock(Duration::from_millis(1000));
        // A second copy starts the two seconds again.
        click(cx, window, "copy-redirect-btn");
        cx.executor().advance_clock(Duration::from_millis(1999));
        assert_eq!(
            button(cx, window, "copy-redirect-btn").0.as_deref(),
            Some("Copied")
        );
        cx.executor().advance_clock(Duration::from_millis(1));
        assert_eq!(
            button(cx, window, "copy-redirect-btn").0.as_deref(),
            Some("Copy")
        );
    }

    #[gpui::test]
    fn autostart_toggle_posts_and_reads_back(cx: &mut TestAppContext) {
        let (fake, window, _) = open_settings(cx, config_json());
        reveal(cx, window, "autostart-toggle");
        assert!(shows(cx, window, "Autostart: Off"));
        assert_eq!(checked(cx, window, "autostart-toggle"), Some(false));
        assert_eq!(
            named(cx, window, "autostart-toggle").0.as_deref(),
            Some("Run on Windows startup")
        );
        let this_bat = r"C:\StreamLoop3\streamloop\START.bat";
        fake.reply(
            "GET",
            "/api/autostart",
            Ok(autostart_json(true, Some(this_bat), true)),
        );

        let before = fake.requests().len();
        click(cx, window, "autostart-toggle");
        cx.run_until_parked();

        let sent: Vec<(&str, String, Option<Value>)> = fake.requests()[before..]
            .iter()
            .filter(|r| r.path == "/api/autostart")
            .map(|r| (r.method, r.path.clone(), r.body.clone()))
            .collect();
        assert_eq!(
            sent,
            [
                (
                    "POST",
                    "/api/autostart".to_string(),
                    Some(json!({ "enabled": true }))
                ),
                ("GET", "/api/autostart".to_string(), None)
            ]
        );
        assert!(shows(cx, window, "Autostart: This install"));
        assert_eq!(checked(cx, window, "autostart-toggle"), Some(true));

        // Another install took autostart over in the meantime: Node refuses with 409.
        let refusal = format!("Autostart points to another StreamLoop install: {OTHER_BAT}.");
        fake.reply(
            "POST",
            "/api/autostart",
            Err(ApiError::Server(refusal.clone())),
        );
        fake.reply(
            "GET",
            "/api/autostart",
            Ok(autostart_json(true, Some(OTHER_BAT), false)),
        );
        click(cx, window, "autostart-toggle");
        cx.run_until_parked();

        assert_eq!(
            bodies(&fake, "/api/autostart")[1],
            json!({ "enabled": false })
        );
        assert!(shows(
            cx,
            window,
            &format!("Could not change autostart: {refusal}")
        ));
        assert!(shows(
            cx,
            window,
            &format!("Autostart: Another install: {OTHER_BAT}")
        ));
        assert_eq!(checked(cx, window, "autostart-toggle"), Some(false));

        // An unreadable state reads "-" and leaves the switch where the click put it.
        fake.reply("POST", "/api/autostart", Ok(json!({ "ok": true })));
        fake.reply("GET", "/api/autostart", Err(ApiError::Unreachable));
        click(cx, window, "autostart-toggle");
        cx.run_until_parked();

        assert!(shows(cx, window, "Autostart: -"));
        assert_eq!(checked(cx, window, "autostart-toggle"), Some(true));
    }

    #[gpui::test]
    fn auto_open_flips_back_when_the_save_fails(cx: &mut TestAppContext) {
        let (fake, window, _) = open_settings(cx, config_json());
        reveal(cx, window, "auto-open-toggle");
        assert!(shows(
            cx,
            window,
            "Open the web dashboard when StreamLoop starts"
        ));
        assert!(shows(
            cx,
            window,
            "Opens the web dashboard in your browser each time the server starts"
        ));
        assert_eq!(
            named(cx, window, "auto-open-toggle").0.as_deref(),
            Some("Open the web dashboard when StreamLoop starts")
        );
        assert_eq!(checked(cx, window, "auto-open-toggle"), Some(true));

        click(cx, window, "auto-open-toggle");
        cx.run_until_parked();

        assert_eq!(config_posts(&fake), [json!({ "autoOpenAdmin": false })]);
        assert_eq!(checked(cx, window, "auto-open-toggle"), Some(false));

        fake.reply(
            "POST",
            "/api/config",
            Err(ApiError::Server("EPERM: operation not permitted".into())),
        );
        click(cx, window, "auto-open-toggle");
        assert_eq!(checked(cx, window, "auto-open-toggle"), Some(true));
        cx.run_until_parked();

        assert_eq!(config_posts(&fake)[1], json!({ "autoOpenAdmin": true }));
        assert!(shows(
            cx,
            window,
            "Could not change the setting: EPERM: operation not permitted"
        ));
        assert_eq!(checked(cx, window, "auto-open-toggle"), Some(false));

        fake.reply(
            "POST",
            "/api/config",
            Ok(json!({
                "ok": true, "restartRequired": false,
                "reloadError": "timeout", "reloadErrorCode": "ETIMEDOUT"
            })),
        );
        click(cx, window, "auto-open-toggle");
        cx.run_until_parked();

        assert!(shows(
            cx,
            window,
            "Saved, but the supervisor could not reload: timeout"
        ));
        assert_eq!(checked(cx, window, "auto-open-toggle"), Some(true));
    }

    #[gpui::test]
    fn ytdlp_update_reports_the_version(cx: &mut TestAppContext) {
        let (fake, window, _) = open_settings(cx, config_json());
        reveal(cx, window, "update-ytdlp-btn");
        assert!(shows(
            cx,
            window,
            "yt-dlp updates itself on startup. Update it by hand if playlists show fewer videos than expected."
        ));

        click(cx, window, "update-ytdlp-btn");
        assert_eq!(
            button(cx, window, "update-ytdlp-btn"),
            (Some("Updating...".into()), true)
        );
        cx.run_until_parked();

        assert!(shows(cx, window, "Updated to yt-dlp 2026.11.01"));
        assert_eq!(
            button(cx, window, "update-ytdlp-btn"),
            (Some("Update yt-dlp".into()), false)
        );

        fake.reply(
            "POST",
            "/api/yt-dlp/update",
            Err(ApiError::Server("yt-dlp exited with code 1".into())),
        );
        click(cx, window, "update-ytdlp-btn");
        assert!(!shows(cx, window, "Updated to yt-dlp 2026.11.01"));
        cx.run_until_parked();

        assert!(shows(
            cx,
            window,
            "Update failed: yt-dlp exited with code 1"
        ));
        assert_eq!(
            button(cx, window, "update-ytdlp-btn"),
            (Some("Update yt-dlp".into()), false)
        );
        assert_eq!(count(&fake, "POST", "/api/yt-dlp/update"), 2);
    }

    #[gpui::test]
    fn shutdown_asks_first(cx: &mut TestAppContext) {
        let (fake, window, _) = open_settings(cx, config_json());
        reveal(cx, window, "shutdown-btn");
        assert!(shows(
            cx,
            window,
            "Stops the dashboard server only. The stream keeps running."
        ));

        click(cx, window, "shutdown-btn");
        assert_eq!(
            cx.pending_prompt(),
            Some((SHUTDOWN_TITLE.to_string(), SHUTDOWN_DETAIL.to_string()))
        );
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert_eq!(count(&fake, "POST", "/api/shutdown"), 0);
        assert_eq!(
            button(cx, window, "shutdown-btn"),
            (Some("Shut down the dashboard".into()), false)
        );

        click(cx, window, "shutdown-btn");
        cx.simulate_prompt_answer("Shut down");
        cx.run_until_parked();

        assert_eq!(count(&fake, "POST", "/api/shutdown"), 1);
        assert_eq!(
            button(cx, window, "shutdown-btn"),
            (Some("Shutting down...".into()), true)
        );
    }

    #[gpui::test]
    fn a_failed_shutdown_is_a_toast(cx: &mut TestAppContext) {
        let (fake, window, _) = open_settings(cx, config_json());
        fake.reply(
            "POST",
            "/api/shutdown",
            Err(ApiError::Server("Forbidden: invalid API token".into())),
        );

        click(cx, window, "shutdown-btn");
        cx.simulate_prompt_answer("Shut down");
        cx.run_until_parked();

        assert!(shows(cx, window, "Forbidden: invalid API token"));
        assert_eq!(
            button(cx, window, "shutdown-btn"),
            (Some("Shutting down...".into()), true)
        );
    }

    #[gpui::test]
    fn the_tab_key_reaches_every_control_in_page_order(cx: &mut TestAppContext) {
        let (_, window, app) = open_settings(cx, config_json());
        let view = settings(cx, &app);
        let (a, b) = (row_key(cx, &view, 0), row_key(cx, &view, 1));
        click(cx, window, ("set-pl-id", a));

        let stops: Vec<ElementId> = vec![
            ("set-pl-name", a).into(),
            ("set-pl-remove", a).into(),
            ("set-pl-id", b).into(),
            ("set-pl-name", b).into(),
            ("set-pl-remove", b).into(),
            "set-add-playlist".into(),
            "set-ingest".into(),
            "set-stream-key".into(),
            "set-stream-key-btn".into(),
            "twitch-enabled-toggle".into(),
            "set-twitch-howto".into(),
            "set-twitch-client-id".into(),
            "set-twitch-client-secret".into(),
            "set-twitch-channel".into(),
            "set-btn".into(),
            "autostart-toggle".into(),
            "auto-open-toggle".into(),
            "check-update-btn".into(),
            "update-ytdlp-btn".into(),
            "shutdown-btn".into(),
        ];
        walk(cx, window, &stops, "tab");

        // Back up to the how-to, open it with Enter, and its link and Copy join the order.
        let back: Vec<ElementId> = stops[..stops.len() - 1]
            .iter()
            .rev()
            .take_while(|id| **id != ElementId::from("set-twitch-client-id"))
            .cloned()
            .chain(["set-twitch-client-id".into(), "set-twitch-howto".into()])
            .collect();
        walk(cx, window, &back, "shift-tab");
        cx.update_window(window, |_, window, cx| window.press("enter", cx))
            .unwrap();
        let opened: Vec<ElementId> = vec![
            "set-twitch-console-link".into(),
            "copy-redirect-btn".into(),
            "set-twitch-client-id".into(),
        ];
        walk(cx, window, &opened, "tab");
    }

    /// The Updates section's Check for Updates and Update Now.
    mod updates {
        use super::*;
        use crate::test_support::{offered_json, update_json};

        const NOTICE: &str = "This release bundles Node 22.24.0. Install the full ZIP to get it.";

        fn answer_check(fake: &FakeApi, answer: Result<Value, ApiError>) {
            fake.reply("POST", "/api/update/check", answer);
        }

        /// The banner button's label and whether it is disabled.
        fn banner(cx: &mut TestAppContext, window: AnyWindowHandle) -> Option<(String, bool)> {
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

        #[gpui::test]
        fn check_shows_up_to_date_or_the_new_version(cx: &mut TestAppContext) {
            let (fake, window, _) = open_settings(cx, config_json());
            fake.reply(
                "POST",
                "/api/update/apply",
                Ok(json!({ "ok": true, "message": "Update started" })),
            );
            reveal(cx, window, "check-update-btn");
            assert_eq!(
                button(cx, window, "check-update-btn"),
                (Some("Check for Updates".into()), false)
            );
            assert!(!present(cx, window, "settings-update-btn"));

            // Up to date: no Update Now.
            answer_check(&fake, Ok(update_json("idle")));
            click(cx, window, "check-update-btn");
            assert_eq!(
                button(cx, window, "check-update-btn"),
                (Some("Checking...".into()), true)
            );
            cx.run_until_parked();
            assert!(shows(cx, window, "Up to date (v3.0.5)"));
            assert_eq!(
                button(cx, window, "check-update-btn"),
                (Some("Check for Updates".into()), false)
            );
            assert!(!present(cx, window, "settings-update-btn"));
            assert_eq!(banner(cx, window), None);

            // A new version, with the Node notice: Update Now, and the banner shows it too.
            let mut noted = offered_json("idle");
            noted["nodeNotice"] = json!(NOTICE);
            answer_check(&fake, Ok(noted));
            click(cx, window, "check-update-btn");
            assert!(!shows(cx, window, "Up to date (v3.0.5)"));
            cx.run_until_parked();
            assert!(shows(cx, window, &format!("v3.0.6 is available. {NOTICE}")));
            assert_eq!(
                button(cx, window, "settings-update-btn"),
                (Some("Update Now".into()), false)
            );
            assert_eq!(banner(cx, window), Some(("Update Now".into(), false)));

            // Dev mode: the version, but no Update Now; the banner says Dev Mode.
            let mut dev = offered_json("idle");
            dev["isDevMode"] = json!(true);
            answer_check(&fake, Ok(dev));
            click(cx, window, "check-update-btn");
            cx.run_until_parked();
            assert!(shows(cx, window, "v3.0.6 is available."));
            assert!(!present(cx, window, "settings-update-btn"));
            assert_eq!(banner(cx, window), Some(("Dev Mode".into(), true)));

            // A failed check says why and leaves Update Now as it was.
            answer_check(
                &fake,
                Err(ApiError::Server("GitHub API returned 403".into())),
            );
            click(cx, window, "check-update-btn");
            cx.run_until_parked();
            assert!(shows(cx, window, "Check failed: GitHub API returned 403"));
            assert!(!present(cx, window, "settings-update-btn"));
            assert_eq!(count(&fake, "POST", "/api/update/check"), 4);

            // Settings' Update Now runs the banner's update, and only one at a time.
            answer_check(&fake, Ok(offered_json("idle")));
            click(cx, window, "check-update-btn");
            cx.run_until_parked();
            fake.reply("GET", "/api/update/status", Ok(offered_json("downloading")));
            click(cx, window, "settings-update-btn");
            assert_eq!(banner(cx, window), Some(("Starting...".into(), true)));
            cx.run_until_parked();
            assert_eq!(banner(cx, window), Some(("Downloading...".into(), true)));
            click(cx, window, "settings-update-btn");
            cx.run_until_parked();
            assert_eq!(count(&fake, "POST", "/api/update/apply"), 1);
        }

        #[gpui::test]
        fn the_tab_key_reaches_update_now_after_a_check(cx: &mut TestAppContext) {
            let (fake, window, _) = open_settings(cx, config_json());
            answer_check(&fake, Ok(offered_json("idle")));
            click(cx, window, "check-update-btn");
            cx.run_until_parked();
            // A click focuses an input, never a button.
            click(cx, window, "set-twitch-channel");

            let stops: Vec<ElementId> = vec![
                "set-btn".into(),
                "autostart-toggle".into(),
                "auto-open-toggle".into(),
                "check-update-btn".into(),
                "settings-update-btn".into(),
                "update-ytdlp-btn".into(),
            ];
            walk(cx, window, &stops, "tab");
        }
    }

    /// Presses `key` once per stop and checks that each gets the focus and is scrolled into view.
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
}
