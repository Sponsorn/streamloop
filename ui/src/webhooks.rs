//! The Webhooks tab (index.html `#panel-webhooks`): the Discord webhook and its identity, the event
//! switches, the message templates, a preview and Save.

use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, FontWeight, Hsla, IntoElement, Render,
    SharedString, Subscription, Window, div, prelude::*, px, relative, rgb,
};
use gpui_kit::component::{
    Disableable, IndexPath, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    input::{InputEvent, InputState, Textarea, TextareaState},
    label::Label,
    select::{Select, SelectEvent, SelectItem, SelectState},
    switch::Switch,
};
use serde_json::{Map, Value, json};

use crate::{
    api::types::{Config, DiscordDefaults},
    app::{AppHandle, ToastKind},
    dashboard::TabView,
    logic::admin::{event_info, preview_colour, preview_title, render_template},
    theme::{
        ACCENT, BG, BORDER, ERROR_TEXT, GREEN, MONO_FONT, MUTED, SURFACE, SURFACE_HOVER, WHITE,
        YELLOW,
    },
    widgets::{
        SAVE_RESET, btn, form_group, put_area_text, put_text, reload_warning, reveal_on_focus,
        section, settings_panel, text_input, text_of,
    },
};

const SAVE_LABEL: &str = "Save Webhook Settings";
/// What Node sends for a saved webhook URL, and what a save sends to keep it.
const MASK: &str = "********";
const URL_PLACEHOLDER: &str = "https://discord.com/api/webhooks/...";
const MASKED_PLACEHOLDER: &str = "Webhook set (leave blank to keep it)";
const SENDING: &str = "Sending a test message...";
const SENT: &str = "Test message sent. Check your Discord channel.";
const TEST_FAILED: &str = "Could not send the test message.";

/// Two rows, growing with the text where admin.css lets the user drag it taller. While it grows,
/// Tab leaves the template as in a browser; a fixed textarea would indent.
const TEMPLATE_ROWS: (usize, usize) = (2, 12);

/// admin.css `.wh-discord-preview`, and its colour bar before an event is chosen.
const PREVIEW_BG: u32 = 0x2f3136;
const PREVIEW_TEXT: u32 = 0xdcddde;
const BAR_BEFORE: u32 = 0x5865f2;

/// How the Test result line reads: admin.css `.result-line`, `.valid` and `.invalid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestResult {
    Sending,
    Sent,
    Failed,
}

/// One event: its switch and its template, in the order of the server's defaults.
struct Row {
    key: String,
    label: String,
    desc: String,
    variables: Vec<String>,
    default: String,
    enabled: bool,
    template: Entity<TextareaState>,
}

/// One preview dropdown entry: the event key and its label.
#[derive(Clone)]
struct EventOption {
    key: SharedString,
    label: SharedString,
}

impl SelectItem for EventOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    // The closed box and the menu rows draw the label as a Label, like the app's other text.
    fn display_title(&self) -> Option<AnyElement> {
        Some(Label::new(self.label.clone()).into_any_element())
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Label::new(self.label.clone())
    }

    fn value(&self) -> &SharedString {
        &self.key
    }
}

/// The Webhooks tab, with admin.js's webhook variables as fields.
pub struct WebhooksView {
    app: AppHandle,
    url: Entity<InputState>,
    bot_name: Entity<InputState>,
    avatar_url: Entity<InputState>,
    role_ping: Entity<InputState>,
    /// `#wh-test-result`; `None` while it is hidden.
    test_result: Option<(SharedString, TestResult)>,
    rows: Vec<Row>,
    /// `webhookSettingsLoaded`.
    loaded: bool,
    loading: bool,
    /// A load has filled the form. A save clears `loaded` but not this.
    filled: bool,
    /// What a failed load puts where the event switches go.
    load_error: Option<SharedString>,
    preview_select: Entity<SelectState<Vec<EventOption>>>,
    save_label: &'static str,
    save_disabled: bool,
    /// The Reset or chip button under the pointer, for admin.css's hover colours.
    hovered: Option<String>,
    _chosen: Subscription,
    /// One per template: typing in it draws the preview again.
    _typed: Vec<Subscription>,
}

impl WebhooksView {
    pub fn new(app: AppHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let preview_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let chosen = cx.subscribe_in(
            &preview_select,
            window,
            |_, _, _: &SelectEvent<Vec<EventOption>>, _, cx| cx.notify(),
        );
        Self {
            app,
            url: input(URL_PLACEHOLDER, window, cx),
            bot_name: input("StreamLoop (default)", window, cx),
            avatar_url: input("https://example.com/avatar.png", window, cx),
            role_ping: input("e.g. 123456789012345678", window, cx),
            test_result: None,
            rows: Vec::new(),
            loaded: false,
            loading: false,
            filled: false,
            load_error: None,
            preview_select,
            save_label: SAVE_LABEL,
            save_disabled: false,
            hovered: None,
            _chosen: chosen,
            _typed: Vec::new(),
        }
    }

    /// admin.js `loadWebhookSettings`: the config and the defaults together, once until a save.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loaded || self.loading {
            return;
        }
        self.loading = true;
        let (client, defaults_client) = (self.app.client.clone(), self.app.client.clone());
        let config = cx.background_spawn(async move { client.config() });
        let defaults = cx.background_spawn(async move { defaults_client.discord_defaults() });
        cx.spawn_in(window, async move |this, cx| {
            let (config, defaults) = (config.await, defaults.await);
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match config.and_then(|config| Ok((config, defaults?))) {
                    Ok((config, defaults)) => this.fill(&config, defaults, window, cx),
                    Err(err) => {
                        this.load_error =
                            Some(format!("Could not load webhook settings: {err}").into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fill(
        &mut self,
        config: &Value,
        defaults: DiscordDefaults,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let saved = Config::from_value(config).discord;
        let masked = saved.webhook_url == MASK;
        let url = if masked { "" } else { &saved.webhook_url };
        put_text(&self.url, url, window, cx);
        self.url.update(cx, |state, cx| {
            let placeholder = if masked {
                MASKED_PLACEHOLDER
            } else {
                URL_PLACEHOLDER
            };
            state.set_placeholder(placeholder, window, cx)
        });
        put_text(&self.bot_name, &saved.bot_name, window, cx);
        put_text(&self.avatar_url, &saved.avatar_url, window, cx);
        put_text(&self.role_ping, &saved.role_ping, window, cx);

        // Raw, since admin.js turns a switch off only for a saved `false`.
        let events = config.pointer("/discord/events");
        self._typed.clear();
        let mut rows = Vec::new();
        for (key, default) in &defaults.templates {
            let (label, desc, _) = event_info(key);
            // An empty saved template falls back to the default, as admin.js's `||`.
            let text = saved
                .templates
                .get(key)
                .filter(|text| !text.is_empty())
                .unwrap_or(default);
            let (min_rows, max_rows) = TEMPLATE_ROWS;
            let template =
                cx.new(|cx| TextareaState::new(window, cx).auto_grow(min_rows, max_rows));
            put_area_text(&template, text, window, cx);
            self._typed
                .push(cx.subscribe(&template, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                }));
            rows.push(Row {
                key: key.clone(),
                label,
                desc,
                variables: defaults.variables.get(key).cloned().unwrap_or_default(),
                default: default.clone(),
                enabled: events.and_then(|events| events.get(key)) != Some(&Value::Bool(false)),
                template,
            });
        }
        let options: Vec<EventOption> = rows
            .iter()
            .map(|row| EventOption {
                key: row.key.clone().into(),
                label: row.label.clone().into(),
            })
            .collect();
        let any = !options.is_empty();
        self.preview_select.update(cx, |select, cx| {
            // The chosen event stays when the list still has it; else the first, as a <select>.
            select.set_items(options, window, cx);
            if select.selected_value().is_none() && any {
                select.set_selected_index(Some(IndexPath::new(0)), window, cx);
            }
        });
        self.rows = rows;
        self.load_error = None;
        self.loaded = true;
        self.filled = true;
    }

    /// The preview's colour bar, title and text (admin.js `updatePreview`), or `None` before an
    /// event is chosen.
    pub fn preview(&self, cx: &App) -> Option<(u32, String, String)> {
        let key = self.preview_select.read(cx).selected_value()?;
        let (_, _, level) = event_info(key);
        let template = self
            .rows
            .iter()
            .find(|row| row.key == key.as_ref())
            .map(|row| row.template.read(cx).value().to_string())
            .unwrap_or_default();
        Some((
            preview_colour(level),
            preview_title(level),
            render_template(&template),
        ))
    }

    /// admin.js `insertVariable`: `{name}` over the template's selection, the cursor after it.
    fn insert_variable(
        &mut self,
        key: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.iter().find(|row| row.key == key) else {
            return;
        };
        let text = format!("{{{name}}}");
        row.template.update(cx, |state, cx| {
            state.replace(text, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    /// admin.js `resetTemplate`.
    fn reset_template(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.key == key) else {
            return;
        };
        put_area_text(&row.template, &row.default, window, cx);
        cx.notify();
    }

    /// admin.js `testDiscordWebhook`.
    fn test_webhook(&mut self, cx: &mut Context<Self>) {
        self.test_result = Some((SENDING.into(), TestResult::Sending));
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn(async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.discord_test() })
                .await;
            this.update(cx, |this, cx| {
                this.test_result = Some(match answer {
                    Ok(()) => (SENT.into(), TestResult::Sent),
                    Err(err) => {
                        let text = err.to_string();
                        let text = if text.is_empty() {
                            TEST_FAILED.to_string()
                        } else {
                            text
                        };
                        (text.into(), TestResult::Failed)
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `handleWebhookSave`: every field, with the mask for a blank URL so Node keeps the
    /// saved one.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.save_disabled {
            return;
        }
        if !self.filled {
            // An unfilled form would blank the name, avatar, role ping, events and templates.
            match &self.load_error {
                Some(error) => self.app.toast(error.clone(), ToastKind::Error, cx),
                None => self.load(window, cx),
            }
            return;
        }
        let url = text_of(&self.url, cx).trim().to_string();
        let mut events = Map::new();
        let mut templates = Map::new();
        for row in &self.rows {
            events.insert(row.key.clone(), json!(row.enabled));
            templates.insert(
                row.key.clone(),
                json!(row.template.read(cx).value().as_ref()),
            );
        }
        let discord = json!({
            "webhookUrl": if url.is_empty() { MASK } else { url.as_str() },
            "botName": text_of(&self.bot_name, cx).trim(),
            "avatarUrl": text_of(&self.avatar_url, cx).trim(),
            "rolePing": text_of(&self.role_ping, cx).trim(),
            "events": events,
            "templates": templates,
        });
        self.save_label = "Saving...";
        self.save_disabled = true;
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let answer = cx
                .background_spawn(async move { client.save_config(json!({ "discord": discord })) })
                .await;
            this.update_in(cx, |this, _, cx| {
                match answer {
                    Ok(saved) => {
                        reload_warning(&this.app, &saved, cx);
                        this.save_label = "Saved";
                        this.loaded = false;
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

    fn render_identity(&self, cx: &mut Context<Self>) -> Div {
        // admin.css `.btn-secondary.btn-small`.
        let test = Button::new("wh-test-btn")
            .label("Test")
            .small()
            .h(px(28.))
            .px(px(12.))
            .on_click(cx.listener(|this, _, _, cx| this.test_webhook(cx)));
        let result = self.test_result.as_ref().map(|(text, kind)| {
            let color = match kind {
                TestResult::Sending => MUTED,
                TestResult::Sent => GREEN,
                TestResult::Failed => ERROR_TEXT,
            };
            Label::new(text.clone())
                .mt(px(4.))
                .text_size(px(13.))
                .text_color(rgb(color))
        });
        let url = div()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(text_input(
                        &self.url,
                        "wh-url",
                        "Discord Webhook URL",
                        false,
                    )))
                    .child(reveal_on_focus(test)),
            )
            .children(result);
        settings_panel()
            .child(form_group("Discord Webhook URL", url, None))
            .child(form_group(
                "Bot Name",
                text_input(&self.bot_name, "wh-bot-name", "Bot Name", false),
                Some("Name shown on webhook messages"),
            ))
            .child(form_group(
                "Avatar URL",
                text_input(&self.avatar_url, "wh-avatar-url", "Avatar URL", false),
                Some("Image shown on webhook messages"),
            ))
            .child(form_group(
                "Role Ping ID",
                text_input(&self.role_ping, "wh-role-ping", "Role Ping ID", false),
                Some("Discord role ID to @mention on critical alerts only"),
            ))
    }

    /// `#wh-events-container`: "Loading events..." until the first load, a failed load's message,
    /// or the switches.
    fn render_events(&self, cx: &mut Context<Self>) -> Div {
        let panel = settings_panel();
        if let Some(error) = &self.load_error {
            return panel.child(Label::new(error.clone()).text_size(px(16.)));
        }
        if !self.loaded && self.rows.is_empty() {
            return panel.child(Label::new("Loading events...").text_size(px(16.)));
        }
        panel.children(self.rows.iter().enumerate().map(|(index, row)| {
            let key = row.key.clone();
            // admin.css `.wh-toggle`, with a line between rows.
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .py(px(10.))
                .when(index > 0, |d| d.border_t_1().border_color(rgb(BORDER)))
                .child(
                    div()
                        .child(Label::new(row.label.clone()).text_size(px(14.)))
                        .when(!row.desc.is_empty(), |d| {
                            d.child(
                                Label::new(row.desc.clone())
                                    .text_size(px(12.))
                                    .text_color(rgb(MUTED)),
                            )
                        }),
                )
                .child(reveal_on_focus(
                    Switch::new(format!("wh-evt-{key}"))
                        .checked(row.enabled)
                        .accessibility_label(row.label.clone())
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            if let Some(row) = this.rows.iter_mut().find(|row| row.key == key) {
                                row.enabled = *checked;
                            }
                            cx.notify();
                        })),
                ))
        }))
    }

    /// `#wh-templates-container`: one card per event with its label, Reset, the chips and the
    /// template.
    fn render_templates(&self, cx: &mut Context<Self>) -> Vec<Div> {
        self.rows
            .iter()
            .map(|row| {
                let key = row.key.clone();
                let chips = (!row.variables.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .mb(px(8.))
                        .children(row.variables.iter().map(|name| self.chip(&key, name, cx)))
                });
                div()
                    .p(px(16.))
                    .mb(px(12.))
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .rounded(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .mb(px(8.))
                            .child(
                                Label::new(row.label.clone())
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::MEDIUM),
                            )
                            .child(self.reset_button(&key, cx)),
                    )
                    .children(chips)
                    .child(
                        div().id(format!("wh-tpl-{key}")).child(reveal_on_focus(
                            // admin.css `.wh-template-input`.
                            Textarea::new(&row.template)
                                .large()
                                .aria_label(format!("{} template", row.label))
                                .bg(rgb(BG))
                                .font_family(MONO_FONT)
                                .text_size(px(13.))
                                .line_height(relative(1.6)),
                        )),
                    )
            })
            .collect()
    }

    /// admin.css `.btn-reset`: no fill, a border and muted text; yellow under the pointer.
    fn reset_button(&self, key: &str, cx: &mut Context<Self>) -> Div {
        let id = format!("wh-reset-{key}");
        let color = if self.hovered.as_deref() == Some(id.as_str()) {
            YELLOW
        } else {
            MUTED
        };
        let key = key.to_string();
        let button = Button::new(id.clone())
            .label("Reset")
            .small()
            .custom(ButtonCustomVariant::new(cx).foreground(hsla(color)))
            .h(px(22.))
            .px(px(10.))
            .on_hover(self.hover_listener(id, cx))
            .on_click(
                cx.listener(move |this, _, window, cx| this.reset_template(&key, window, cx)),
            );
        let border = if color == YELLOW { YELLOW } else { BORDER };
        outlined(button, border, 8.)
    }

    /// admin.css `.wh-chip`: accent monospace on the page colour, pill-shaped; under the pointer
    /// the hover fill and an accent border.
    fn chip(&self, key: &str, name: &str, cx: &mut Context<Self>) -> Div {
        let id = format!("wh-chip-{key}-{name}");
        let hovered = self.hovered.as_deref() == Some(id.as_str());
        let colors = ButtonCustomVariant::new(cx)
            .color(hsla(BG))
            .foreground(hsla(ACCENT))
            .hover(hsla(SURFACE_HOVER))
            .active(hsla(SURFACE_HOVER));
        let (key, name) = (key.to_string(), name.to_string());
        let button = Button::new(id.clone())
            .label(format!("{{{name}}}"))
            .small()
            .custom(colors)
            .rounded(px(12.))
            .h(px(20.))
            .px(px(8.))
            .font_family(MONO_FONT)
            .on_hover(self.hover_listener(id, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.insert_variable(&key, &name, window, cx)
            }));
        outlined(button, if hovered { ACCENT } else { BORDER }, 12.)
    }

    fn hover_listener(
        &self,
        id: String,
        cx: &mut Context<Self>,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered {
                this.hovered = Some(id.clone());
            } else if this.hovered.as_deref() == Some(id.as_str()) {
                this.hovered = None;
            }
            cx.notify();
        })
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> Div {
        let (bar, title, text) =
            self.preview(cx)
                .unwrap_or((BAR_BEFORE, String::new(), String::new()));
        let select = div().w_full().max_w(px(280.)).child(reveal_on_focus(
            Select::new(&self.preview_select)
                .id("wh-preview-select")
                .large()
                .placeholder("")
                .accessibility_label("Event Type")
                .h(px(40.))
                .px(px(12.))
                .bg(rgb(BG)),
        ));
        settings_panel()
            .child(form_group("Event Type", select, None))
            .child(
                // admin.css `.wh-discord-preview`: Discord's embed, with the level's colour bar.
                div()
                    .flex()
                    .min_h(px(80.))
                    .bg(rgb(PREVIEW_BG))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .child(div().w(px(4.)).flex_shrink_0().bg(rgb(bar)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(px(16.))
                            .py(px(12.))
                            .child(
                                Label::new(title)
                                    .mb(px(6.))
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(WHITE)),
                            )
                            .child(
                                Label::new(text)
                                    .mb(px(8.))
                                    .text_size(px(14.))
                                    .text_color(rgb(PREVIEW_TEXT)),
                            ),
                    ),
            )
    }
}

impl TabView for WebhooksView {
    /// admin.js `switchTab('webhooks')`.
    fn on_tab_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.load(window, cx);
    }
}

impl Render for WebhooksView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let save = btn(Button::new("wh-save-btn")
            .primary()
            .label(self.save_label)
            .disabled(self.save_disabled))
        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        div()
            .child(section("Webhook & Identity").child(self.render_identity(cx)))
            .child(section("Event Notifications").child(self.render_events(cx)))
            .child(section("Message Templates").children(self.render_templates(cx)))
            .child(section("Preview").child(self.render_preview(cx)))
            // As wide as its label, as the inline button in index.html.
            .child(div().flex().child(reveal_on_focus(save)))
    }
}

fn hsla(color: u32) -> Hsla {
    rgb(color).into()
}

/// A 1 px border around `button`. A custom button's own border takes its fill colour under the
/// pointer, which would hide it.
fn outlined(button: Button, border: u32, radius: f32) -> Div {
    div()
        .flex()
        .rounded(px(radius))
        .border_1()
        .border_color(rgb(border))
        .child(reveal_on_focus(button))
}

#[cfg(test)]
impl WebhooksView {
    fn keys(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.key.clone()).collect()
    }

    fn template(&self, key: &str) -> Entity<TextareaState> {
        self.rows
            .iter()
            .find(|row| row.key == key)
            .map(|row| row.template.clone())
            .unwrap_or_else(|| panic!("a template for {key}"))
    }

    /// The URL field's text and placeholder.
    fn url_field(&self, cx: &App) -> (String, String) {
        let url = self.url.read(cx);
        (
            url.value().to_string(),
            url.presentation().placeholder().to_string(),
        )
    }

    fn test_result(&self) -> Option<(String, TestResult)> {
        self.test_result
            .as_ref()
            .map(|(text, kind)| (text.to_string(), *kind))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{
        AnyWindowHandle, Bounds, ElementId, Focusable, Pixels, ScrollDelta, TestAppContext, point,
        px,
    };
    use gpui_kit::test::TestWindowExt;
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi},
        app::{AppView, TOAST_FOR},
        dashboard::Tab,
        logic::admin::render_template,
        test_support::{answer_polls, config_json, count, open_app, painted, saved_json, shows},
        widgets::SAVE_RESET,
    };

    /// The server's event keys, in `DEFAULT_DISCORD_TEMPLATES` order.
    const KEYS: [&str; 10] = [
        "error",
        "skip",
        "recovery",
        "critical",
        "resume",
        "streamDrop",
        "streamRestart",
        "outputFrozen",
        "twitchMismatch",
        "twitchRestart",
    ];
    const MASKED: &str = "Webhook set (leave blank to keep it)";
    const URL_HINT: &str = "https://discord.com/api/webhooks/...";
    const SENDING: &str = "Sending a test message...";
    const SENT: &str = "Test message sent. Check your Discord channel.";
    const SAVE_LABEL: &str = "Save Webhook Settings";

    /// `src/server/config.ts` `DEFAULT_DISCORD_TEMPLATES`.
    fn templates() -> Value {
        json!({
            "error": "Playback error **{errorCode}** on video #{videoIndex} (`{videoId}`)\nRetry attempt: {attempt}",
            "skip": "Skipping video #{videoIndex} (`{videoId}`)\nReason: {reason}",
            "recovery": "Recovery action: **{step}**",
            "critical": "**CRITICAL:** {message}",
            "resume": "Playback resumed at video #{videoIndex} (`{videoId}`)",
            "streamDrop": "Stream stopped unexpectedly — attempting restart (attempt {attempt}/{maxAttempts})",
            "streamRestart": "Stream restarted successfully after {attempts} attempt(s)",
            "outputFrozen": "Output frozen ({kind}) on video #{videoIndex}",
            "twitchMismatch": "Twitch liveness mismatch — the encoder reports streaming but **{channel}** is offline on Twitch. Restarting stream.",
            "twitchRestart": "Stream restarted after Twitch liveness mismatch on **{channel}**"
        })
    }

    /// `src/server/config.ts` `DISCORD_TEMPLATE_VARIABLES`.
    fn variables() -> Value {
        json!({
            "error": ["videoIndex", "videoId", "errorCode", "attempt"],
            "skip": ["videoIndex", "videoId", "reason"],
            "recovery": ["step"],
            "critical": ["message"],
            "resume": ["videoIndex", "videoId"],
            "streamDrop": ["attempt", "maxAttempts"],
            "streamRestart": ["attempts"],
            "outputFrozen": ["kind", "videoIndex"],
            "twitchMismatch": ["channel"],
            "twitchRestart": ["channel"]
        })
    }

    fn defaults_json() -> Value {
        json!({ "templates": templates(), "variables": variables() })
    }

    fn default_template(key: &str) -> String {
        templates()[key].as_str().unwrap().to_string()
    }

    fn open_webhooks(
        cx: &mut TestAppContext,
        config: Value,
    ) -> (Arc<FakeApi>, AnyWindowHandle, Entity<AppView>) {
        open_webhooks_with(cx, config, defaults_json())
    }

    /// Opens the dashboard on `config` and the Discord `defaults`, then the Webhooks tab.
    fn open_webhooks_with(
        cx: &mut TestAppContext,
        config: Value,
        defaults: Value,
    ) -> (Arc<FakeApi>, AnyWindowHandle, Entity<AppView>) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", Ok(config));
        fake.reply("GET", "/api/discord/defaults", Ok(defaults));
        fake.reply("POST", "/api/config", Ok(saved_json(false)));
        fake.reply("POST", "/api/discord/test", Ok(json!({ "ok": true })));
        let (app, window) = open_app(cx, fake.clone());
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();
        (fake, window, app)
    }

    fn webhooks(cx: &mut TestAppContext, app: &Entity<AppView>) -> Entity<WebhooksView> {
        let dashboard = app
            .read_with(cx, |app, _| app.dashboard())
            .expect("the dashboard shows");
        dashboard
            .read_with(cx, |dashboard, _| dashboard.tab_view(Tab::Webhooks))
            .downcast::<WebhooksView>()
            .unwrap_or_else(|_| panic!("the Webhooks tab is a WebhooksView"))
    }

    /// Controls the wheel can be sent to: the window draws only part of the tab.
    fn landmarks() -> Vec<ElementId> {
        let mut ids: Vec<String> = [
            "tab-webhooks",
            "wh-url",
            "wh-test-btn",
            "wh-bot-name",
            "wh-avatar-url",
            "wh-role-ping",
        ]
        .map(String::from)
        .to_vec();
        for key in KEYS {
            ids.push(format!("wh-evt-{key}"));
            ids.push(format!("wh-reset-{key}"));
        }
        ids.push("wh-preview-select".into());
        ids.push("wh-save-btn".into());
        ids.into_iter().map(ElementId::from).collect()
    }

    /// Scrolls the dashboard so `id` sits in the middle of the window.
    fn reveal(cx: &mut TestAppContext, window: AnyWindowHandle, id: &str) {
        let id = ElementId::from(id.to_string());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let height = window.viewport_size().height;
            let dy = height / 2. - window.find(id.clone()).bounds().center().y;
            let on_screen = landmarks()
                .into_iter()
                .find(|c| {
                    window.try_find(c.clone()).is_some_and(|e| {
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

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &str) {
        reveal(cx, window, id);
        cx.update_window(window, |_, window, cx| window.click(id.to_string(), cx))
            .unwrap();
    }

    /// Replaces the text of the one-line input `id` with `text`.
    fn type_into(cx: &mut TestAppContext, window: AnyWindowHandle, id: &str, text: &str) {
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

    /// Clicks into the template of `key`, then replaces its text with `text`.
    fn type_into_template(cx: &mut TestAppContext, window: AnyWindowHandle, key: &str, text: &str) {
        reveal(cx, window, &format!("wh-reset-{key}"));
        cx.update_window(window, |_, window, cx| {
            window
                .within(format!("wh-tpl-{key}"))
                .click("input-state", cx);
            window.press("ctrl-a", cx);
            window.input(text, cx);
        })
        .unwrap();
    }

    /// Opens the preview's event dropdown and moves `downs` options down from the chosen one.
    fn choose_preview(cx: &mut TestAppContext, window: AnyWindowHandle, downs: usize) {
        reveal(cx, window, "wh-preview-select");
        cx.update_window(window, |_, window, cx| {
            window.within("wh-preview-select").click("input", cx);
            for _ in 0..downs {
                window.press("down", cx);
            }
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    /// A button's label and whether it is disabled.
    fn button(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        id: &str,
    ) -> (Option<String>, bool) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let button = window.find(id.to_string());
            (
                button.label().map(str::to_string),
                button.disabled() == Some(true),
            )
        })
        .unwrap()
    }

    fn checked(cx: &mut TestAppContext, window: AnyWindowHandle, key: &str) -> Option<bool> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find(format!("wh-evt-{key}")).checked()
        })
        .unwrap()
    }

    fn template_text(cx: &mut TestAppContext, template: &Entity<TextareaState>) -> String {
        template.read_with(cx, |state, _| state.value().to_string())
    }

    fn config_posts(fake: &FakeApi) -> Vec<Value> {
        fake.posts()
            .into_iter()
            .filter(|r| r.path == "/api/config")
            .map(|r| r.body.unwrap_or_default())
            .collect()
    }

    /// Whether the preview, scrolled into view, draws `text`.
    fn preview_shows(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) -> bool {
        reveal(cx, window, "wh-preview-select");
        shows(cx, window, text)
    }

    fn preview(cx: &mut TestAppContext, view: &Entity<WebhooksView>) -> (u32, String, String) {
        view.read_with(cx, |view, cx| view.preview(cx))
            .expect("the preview shows an event")
    }

    #[gpui::test]
    fn a_saved_webhook_shows_as_a_placeholder(cx: &mut TestAppContext) {
        let (fake, window, app) = open_webhooks(cx, config_json());
        let view = webhooks(cx, &app);

        assert_eq!(
            view.read_with(cx, |view, cx| view.url_field(cx)),
            (String::new(), MASKED.to_string())
        );

        // A value Node did not mask shows as it is, with the plain placeholder.
        let mut config = config_json();
        config["discord"]["webhookUrl"] = json!("https://discord.com/api/webhooks/1/abc");
        fake.reply("GET", "/api/config", Ok(config));
        click(cx, window, "wh-save-btn");
        cx.run_until_parked();
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |view, cx| view.url_field(cx)),
            (
                "https://discord.com/api/webhooks/1/abc".to_string(),
                URL_HINT.to_string()
            )
        );
    }

    #[gpui::test]
    fn toggles_and_templates_follow_the_servers_order(cx: &mut TestAppContext) {
        // The defaults in reverse, so the order can only come from them.
        let reversed: Vec<&str> = KEYS.iter().rev().copied().collect();
        let (mut templates_rev, mut variables_rev) = (Map::new(), Map::new());
        for key in &reversed {
            templates_rev.insert(key.to_string(), templates()[*key].clone());
            variables_rev.insert(key.to_string(), variables()[*key].clone());
        }
        let defaults = json!({ "templates": templates_rev, "variables": variables_rev });
        let (_, window, app) = open_webhooks_with(cx, config_json(), defaults);
        let view = webhooks(cx, &app);

        assert_eq!(view.read_with(cx, |view, _| view.keys()), reversed);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let y = |id: String| window.find(id).bounds().origin.y;
            for pair in reversed.windows(2) {
                assert!(y(format!("wh-evt-{}", pair[0])) < y(format!("wh-evt-{}", pair[1])));
                assert!(y(format!("wh-reset-{}", pair[0])) < y(format!("wh-reset-{}", pair[1])));
            }
            assert_eq!(
                window.find("wh-evt-twitchRestart").label(),
                Some("Twitch Restarted")
            );
            assert_eq!(
                window.find("wh-chip-error-errorCode").label(),
                Some("{errorCode}")
            );
            assert!(window.try_find("wh-chip-recovery-videoId").is_none());
        })
        .unwrap();
        for key in KEYS {
            let template = view.read_with(cx, |view, _| view.template(key));
            assert_eq!(template_text(cx, &template), default_template(key), "{key}");
        }
        // The dropdown lists the events in the same order and starts on the first.
        assert_eq!(preview(cx, &view).1, "ℹ️ StreamLoop");
        assert_eq!(
            preview(cx, &view).2,
            "Stream restarted after Twitch liveness mismatch on **mychannel**"
        );
    }

    #[gpui::test]
    fn the_form_shows_the_saved_settings(cx: &mut TestAppContext) {
        let mut config = config_json();
        config["discord"]["botName"] = json!("Loop Bot");
        config["discord"]["avatarUrl"] = json!("https://example.com/a.png");
        config["discord"]["rolePing"] = json!("123");
        config["discord"]["events"] = json!({ "error": false, "skip": "false", "critical": true });
        config["discord"]["templates"] = json!({ "error": "Saved {videoId}", "skip": "" });
        let (_, window, app) = open_webhooks(cx, config);
        let view = webhooks(cx, &app);

        assert_eq!(checked(cx, window, "error"), Some(false));
        for key in ["skip", "recovery", "critical", "twitchRestart"] {
            assert_eq!(checked(cx, window, key), Some(true), "{key}");
        }
        let error = view.read_with(cx, |view, _| view.template("error"));
        let skip = view.read_with(cx, |view, _| view.template("skip"));
        assert_eq!(template_text(cx, &error), "Saved {videoId}");
        assert_eq!(template_text(cx, &skip), default_template("skip"));
        let fields = cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            ["wh-bot-name", "wh-avatar-url", "wh-role-ping"]
                .map(|id| window.find(id).value().map(str::to_string))
        });
        assert_eq!(
            fields.unwrap(),
            [
                Some("Loop Bot".to_string()),
                Some("https://example.com/a.png".to_string()),
                Some("123".to_string())
            ]
        );
    }

    #[gpui::test]
    fn loads_once_until_its_own_save(cx: &mut TestAppContext) {
        let (fake, window, _) = open_webhooks(cx, config_json());
        let configs = count(&fake, "GET", "/api/config");
        assert_eq!(count(&fake, "GET", "/api/discord/defaults"), 1);

        click(cx, window, "tab-monitor");
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();

        assert_eq!(count(&fake, "GET", "/api/config"), configs);
        assert_eq!(count(&fake, "GET", "/api/discord/defaults"), 1);

        click(cx, window, "wh-save-btn");
        cx.run_until_parked();
        assert_eq!(count(&fake, "GET", "/api/config"), configs);
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();

        assert_eq!(count(&fake, "GET", "/api/config"), configs + 1);
        assert_eq!(count(&fake, "GET", "/api/discord/defaults"), 2);
    }

    #[gpui::test]
    fn a_failed_load_says_why_and_the_next_show_tries_again(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", Ok(config_json()));
        let (_, window) = open_app(cx, fake.clone());
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();

        assert!(shows(
            cx,
            window,
            "Could not load webhook settings: Can't reach the server."
        ));
        assert!(!shows(cx, window, "Loading events..."));

        fake.reply("GET", "/api/discord/defaults", Ok(defaults_json()));
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();

        assert!(!shows(
            cx,
            window,
            "Could not load webhook settings: Can't reach the server."
        ));
        assert_eq!(checked(cx, window, "error"), Some(true));
    }

    #[gpui::test]
    fn a_save_before_the_settings_load_sends_nothing(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", Ok(config_json()));
        fake.reply("POST", "/api/config", Ok(saved_json(false)));
        let (app, window) = open_app(cx, fake.clone());
        let view = webhooks(cx, &app);
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();
        let why = "Could not load webhook settings: Can't reach the server.";
        let painted_why =
            |cx: &mut TestAppContext| painted(cx, window).iter().filter(|t| *t == why).count();
        // Where the event switches go.
        assert_eq!(painted_why(cx), 1);

        type_into(cx, window, "wh-bot-name", "Loop Bot");
        click(cx, window, "wh-save-btn");
        cx.run_until_parked();

        // A save now would blank the name, avatar, role ping, events and templates.
        assert!(config_posts(&fake).is_empty());
        assert_eq!(painted_why(cx), 2);
        assert_eq!(
            button(cx, window, "wh-save-btn"),
            (Some(SAVE_LABEL.into()), false)
        );

        // With no load error to show, Save loads the settings again instead.
        fake.reply("GET", "/api/discord/defaults", Ok(defaults_json()));
        let reads = count(&fake, "GET", "/api/discord/defaults");
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.load_error = None;
                view.save(window, cx);
            })
        })
        .unwrap();
        cx.run_until_parked();

        assert!(config_posts(&fake).is_empty());
        assert_eq!(count(&fake, "GET", "/api/discord/defaults"), reads + 1);
        assert_eq!(view.read_with(cx, |view, _| view.keys()).len(), 10);
    }

    #[gpui::test]
    fn test_reports_both_ways(cx: &mut TestAppContext) {
        let (fake, window, app) = open_webhooks(cx, config_json());
        let view = webhooks(cx, &app);
        assert_eq!(view.read_with(cx, |view, _| view.test_result()), None);

        click(cx, window, "wh-test-btn");

        assert!(shows(cx, window, SENDING));
        assert_eq!(
            view.read_with(cx, |view, _| view.test_result()),
            Some((SENDING.to_string(), TestResult::Sending))
        );
        cx.run_until_parked();
        assert!(shows(cx, window, SENT));
        assert_eq!(
            view.read_with(cx, |view, _| view.test_result()),
            Some((SENT.to_string(), TestResult::Sent))
        );

        let why = "No Discord webhook URL configured";
        fake.reply(
            "POST",
            "/api/discord/test",
            Err(ApiError::Server(why.into())),
        );
        click(cx, window, "wh-test-btn");
        cx.run_until_parked();

        assert!(shows(cx, window, why));
        assert!(!shows(cx, window, SENT));
        assert_eq!(
            view.read_with(cx, |view, _| view.test_result()),
            Some((why.to_string(), TestResult::Failed))
        );
        let tests: Vec<_> = fake
            .posts()
            .into_iter()
            .filter(|r| r.path == "/api/discord/test")
            .collect();
        assert_eq!(tests.len(), 2);
        assert!(tests.iter().all(|r| r.body.is_none()));
    }

    #[gpui::test]
    fn a_chip_inserts_at_the_cursor(cx: &mut TestAppContext) {
        let (_, window, app) = open_webhooks(cx, config_json());
        let view = webhooks(cx, &app);
        let error = view.read_with(cx, |view, _| view.template("error"));

        type_into_template(cx, window, "error", "abcd");
        cx.update_window(window, |_, window, cx| {
            window.press("left", cx);
            window.press("left", cx);
        })
        .unwrap();
        click(cx, window, "wh-chip-error-videoId");
        cx.run_until_parked();

        assert_eq!(template_text(cx, &error), "ab{videoId}cd");
        let inserted_end = "ab{videoId}".len();
        assert_eq!(
            error.read_with(cx, |state, _| state.selected_range()),
            inserted_end..inserted_end
        );
        let focused = cx
            .update_window(window, |_, window, cx| {
                error.read(cx).focus_handle(cx).is_focused(window)
            })
            .unwrap();
        assert!(focused, "the template keeps the focus");
        assert_eq!(preview(cx, &view).2, "abdQw4w9WgXcQcd");
        assert!(preview_shows(cx, window, "abdQw4w9WgXcQcd"));

        // Typing goes on after the inserted text.
        cx.update_window(window, |_, window, cx| window.input("!", cx))
            .unwrap();
        assert_eq!(template_text(cx, &error), "ab{videoId}!cd");

        // A selection is replaced.
        cx.update_window(window, |_, window, cx| window.press("ctrl-a", cx))
            .unwrap();
        click(cx, window, "wh-chip-error-attempt");
        cx.run_until_parked();
        assert_eq!(template_text(cx, &error), "{attempt}");
        assert_eq!(preview(cx, &view).2, "2");

        // A template nobody clicked into has its cursor at the end, as a textarea after its value is set.
        let recovery = view.read_with(cx, |view, _| view.template("recovery"));
        click(cx, window, "wh-chip-recovery-step");
        cx.run_until_parked();
        assert_eq!(
            template_text(cx, &recovery),
            "Recovery action: **{step}**{step}"
        );
    }

    #[gpui::test]
    fn reset_restores_the_default(cx: &mut TestAppContext) {
        let (_, window, app) = open_webhooks(cx, config_json());
        let view = webhooks(cx, &app);
        let error = view.read_with(cx, |view, _| view.template("error"));
        let skip = view.read_with(cx, |view, _| view.template("skip"));
        type_into_template(cx, window, "error", "Something else");
        type_into_template(cx, window, "skip", "Skip text");
        assert_eq!(preview(cx, &view).2, "Something else");
        assert!(preview_shows(cx, window, "Something else"));

        click(cx, window, "wh-reset-error");
        cx.run_until_parked();

        assert_eq!(template_text(cx, &error), default_template("error"));
        assert_eq!(template_text(cx, &skip), "Skip text");
        assert_eq!(
            preview(cx, &view).2,
            render_template(&default_template("error"))
        );
        assert!(!preview_shows(cx, window, "Something else"));
    }

    #[gpui::test]
    fn preview_follows_the_select_and_typing(cx: &mut TestAppContext) {
        let (_, window, app) = open_webhooks(cx, config_json());
        let view = webhooks(cx, &app);
        assert_eq!(
            preview(cx, &view),
            (
                0xf1c40f,
                "⚠️ StreamLoop".to_string(),
                "Playback error **early-eof** on video #3 (`dQw4w9WgXcQ`)\nRetry attempt: 2"
                    .to_string()
            )
        );

        choose_preview(cx, window, 3);

        let critical = "**CRITICAL:** The encoder has stopped 3 times in a row.";
        assert_eq!(
            preview(cx, &view),
            (0xe74c3c, "🚨 StreamLoop".to_string(), critical.to_string())
        );
        assert!(shows(cx, window, "🚨 StreamLoop"));
        assert!(shows(cx, window, critical));

        choose_preview(cx, window, 2);

        let drop = "Stream stopped unexpectedly — attempting restart (attempt 2/3)";
        assert_eq!(
            preview(cx, &view),
            (0xf1c40f, "⚠️ StreamLoop".to_string(), drop.to_string())
        );
        assert!(shows(cx, window, "⚠️ StreamLoop"));
        assert!(shows(cx, window, drop));

        type_into_template(
            cx,
            window,
            "streamDrop",
            "Drop on {videoId}, try {attempt} of {maxAttempts} {unknown}",
        );
        let typed = "Drop on dQw4w9WgXcQ, try 2 of 3 {unknown}";
        assert_eq!(preview(cx, &view).2, typed);
        assert!(preview_shows(cx, window, typed));

        type_into_template(cx, window, "skip", "Not the chosen event");
        assert_eq!(preview(cx, &view).2, typed);
    }

    #[gpui::test]
    fn save_sends_every_field_and_keeps_a_blank_url_as_the_mask(cx: &mut TestAppContext) {
        let (fake, window, _) = open_webhooks(cx, config_json());
        type_into(cx, window, "wh-bot-name", "  Loop Bot  ");
        type_into(cx, window, "wh-avatar-url", "https://example.com/a.png ");
        type_into(cx, window, "wh-role-ping", " 123 ");
        click(cx, window, "wh-evt-skip");
        type_into_template(cx, window, "error", "Custom {videoId} ");

        click(cx, window, "wh-save-btn");

        assert_eq!(
            button(cx, window, "wh-save-btn"),
            (Some("Saving...".into()), true)
        );
        cx.run_until_parked();
        let mut events = json!({});
        for key in KEYS {
            events[key] = json!(key != "skip");
        }
        let mut sent_templates = templates();
        sent_templates["error"] = json!("Custom {videoId} ");
        assert_eq!(
            config_posts(&fake),
            [json!({ "discord": {
                "webhookUrl": "********",
                "botName": "Loop Bot",
                "avatarUrl": "https://example.com/a.png",
                "rolePing": "123",
                "events": events,
                "templates": sent_templates
            }})]
        );
        assert_eq!(
            button(cx, window, "wh-save-btn"),
            (Some("Saved".into()), true)
        );
        cx.executor()
            .advance_clock(SAVE_RESET - std::time::Duration::from_millis(1));
        assert_eq!(
            button(cx, window, "wh-save-btn"),
            (Some("Saved".into()), true)
        );
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1));
        assert_eq!(
            button(cx, window, "wh-save-btn"),
            (Some(SAVE_LABEL.into()), false)
        );

        // A typed URL goes as typed, trimmed; a reload that failed is a toast (spec D6).
        fake.reply(
            "POST",
            "/api/config",
            Ok(json!({
                "ok": true, "restartRequired": false,
                "reloadError": "timeout", "reloadErrorCode": "ETIMEDOUT"
            })),
        );
        type_into(
            cx,
            window,
            "wh-url",
            " https://discord.com/api/webhooks/1/abc ",
        );
        click(cx, window, "wh-save-btn");
        cx.run_until_parked();

        assert_eq!(
            config_posts(&fake)[1]["discord"]["webhookUrl"],
            json!("https://discord.com/api/webhooks/1/abc")
        );
        assert!(shows(
            cx,
            window,
            "Saved, but the supervisor could not reload: timeout"
        ));
    }

    #[gpui::test]
    fn a_failed_save_says_why(cx: &mut TestAppContext) {
        let (fake, window, _) = open_webhooks(cx, config_json());
        fake.reply(
            "POST",
            "/api/config",
            Err(ApiError::Server("Invalid config".into())),
        );

        click(cx, window, "wh-save-btn");
        cx.run_until_parked();

        assert!(shows(cx, window, "Could not save: Invalid config"));
        assert_eq!(
            button(cx, window, "wh-save-btn"),
            (Some("Saving...".into()), true)
        );
        cx.executor().advance_clock(SAVE_RESET);
        assert_eq!(
            button(cx, window, "wh-save-btn"),
            (Some(SAVE_LABEL.into()), false)
        );
        cx.executor().advance_clock(TOAST_FOR);

        // A failed save leaves the form loaded.
        let configs = count(&fake, "GET", "/api/config");
        click(cx, window, "tab-webhooks");
        cx.run_until_parked();
        assert_eq!(count(&fake, "GET", "/api/config"), configs);
    }

    /// A Tab stop: a control by its element id, or the template of an event.
    #[derive(Clone, Debug)]
    enum Stop {
        Id(String),
        Template(&'static str),
    }

    /// Every Tab stop after the URL field, in page order.
    fn tab_stops() -> Vec<Stop> {
        let mut stops: Vec<Stop> = [
            "wh-test-btn",
            "wh-bot-name",
            "wh-avatar-url",
            "wh-role-ping",
        ]
        .map(|id| Stop::Id(id.to_string()))
        .to_vec();
        for key in KEYS {
            stops.push(Stop::Id(format!("wh-evt-{key}")));
        }
        for key in KEYS {
            stops.push(Stop::Id(format!("wh-reset-{key}")));
            for name in variables()[key].as_array().unwrap() {
                stops.push(Stop::Id(format!(
                    "wh-chip-{key}-{}",
                    name.as_str().unwrap()
                )));
            }
            stops.push(Stop::Template(key));
        }
        stops.push(Stop::Id("wh-preview-select".into()));
        stops.push(Stop::Id("wh-save-btn".into()));
        stops
    }

    /// Whether `stop` has the focus, and where it was drawn in the last frame.
    fn stop_state(
        window: &mut Window,
        cx: &mut App,
        view: &Entity<WebhooksView>,
        stop: &Stop,
    ) -> (bool, Bounds<Pixels>) {
        match stop {
            Stop::Id(id) => {
                let element = window.find(id.clone());
                (element.focused() == Some(true), element.bounds())
            }
            Stop::Template(key) => {
                let template = view.read(cx).template(key);
                let focused = template.read(cx).focus_handle(cx).is_focused(window);
                let bounds = window
                    .within(format!("wh-tpl-{key}"))
                    .find("input-state")
                    .bounds();
                (focused, bounds)
            }
        }
    }

    #[gpui::test]
    fn the_tab_key_reaches_every_control_in_page_order(cx: &mut TestAppContext) {
        let (_, window, app) = open_webhooks(cx, config_json());
        let view = webhooks(cx, &app);
        click(cx, window, "wh-url");

        for stop in tab_stops() {
            cx.update_window(window, |_, window, cx| {
                window.press("tab", cx);
                window.render_frame(cx);
                assert!(stop_state(window, cx, &view, &stop).0, "{stop:?}");
            })
            .unwrap();
        }
    }

    #[gpui::test]
    fn tab_scrolls_each_control_into_view_and_leaves_visible_ones_alone(cx: &mut TestAppContext) {
        let (_, window, app) = open_webhooks(cx, config_json());
        let view = webhooks(cx, &app);
        let dashboard = app
            .read_with(cx, |app, _| app.dashboard())
            .expect("the dashboard shows");
        click(cx, window, "wh-url");
        assert_eq!(dashboard.read_with(cx, |d, _| d.scroll_offset()), px(0.));

        let offset = |cx: &mut TestAppContext| dashboard.read_with(cx, |d, _| d.scroll_offset());
        // Down to Save, then Shift-Tab back over three stops.
        let stops = tab_stops();
        let mut walk: Vec<(&str, Stop)> = stops.iter().map(|s| ("tab", s.clone())).collect();
        for stop in stops.iter().rev().skip(1).take(3) {
            walk.push(("shift-tab", stop.clone()));
        }
        let mut went_up = false;

        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        for (key, stop) in walk {
            let before = offset(cx);
            cx.update_window(window, |_, window, cx| {
                // Read from the frame the last step ended on.
                let height = window.viewport_size().height;
                let (_, was) = stop_state(window, cx, &view, &stop);
                let was_in_view = was.top() >= px(0.) && was.bottom() <= height;

                window.press(key, cx);
                // The frame that sees the new focus scrolls; the next one draws it there.
                window.render_frame(cx);
                window.render_frame(cx);

                let (focused, now) = stop_state(window, cx, &view, &stop);
                assert!(focused, "{stop:?}");
                assert!(
                    now.top() >= px(0.) && now.bottom() <= height,
                    "{stop:?} in view: {now:?}"
                );
                if was_in_view {
                    let after = dashboard.read(cx).scroll_offset();
                    assert_eq!(after, before, "{stop:?} was in view, so nothing scrolls");
                }
            })
            .unwrap();
            went_up |= offset(cx) < before;
            if matches!(stop, Stop::Template("recovery")) {
                // The wheel takes the page past the focus, and the focus does not pull it back.
                // The next stop is then above the view.
                let at = offset(cx);
                cx.update_window(window, |_, window, cx| {
                    let delta = ScrollDelta::Pixels(point(px(0.), px(-1500.)));
                    window.scroll("wh-reset-recovery", delta, cx);
                    // A key that keeps the focus: the input is the keyboard's again.
                    window.press("right", cx);
                    window.render_frame(cx);
                    window.render_frame(cx);
                })
                .unwrap();
                assert_eq!(offset(cx), at + px(1500.));
            }
        }
        assert!(went_up, "a stop above the view brought the page back up");
        assert!(offset(cx) > px(1000.), "Save is far down the page");
    }
}
