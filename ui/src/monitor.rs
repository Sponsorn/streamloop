//! The Monitor tab (index.html `#panel-monitor`): the stream card, Now Playing, the Timeline and the Logs.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use gpui::{
    AnyElement, AppContext, Context, Div, Entity, FontWeight, IntoElement, Render, SharedString,
    Window, div, prelude::*, px, rgb,
};
use gpui_kit::{
    base::{SelectableText, TestTextExt},
    component::{
        button::{Button, ButtonVariants},
        label::Label,
        scroll::ScrollableElement,
    },
};

use crate::{
    api::{
        client::encode_component,
        types::{Event, Playing, Status, UpdateStatus},
    },
    app::AppHandle,
    dashboard::TabView,
    logic::{
        Level,
        admin::{encoder_line, local_hms, now_playing_index, now_playing_time},
        source_label, stream_line, twitch_line, youtube_note,
    },
    logs::LogViewer,
    theme::{ACCENT, BORDER, MONO_FONT, MUTED, SURFACE, level_color},
    videos::VideoList,
    widgets::{ActionView, Busy, action_button, btn, reveal_on_focus, run_action, section},
};

const STOP_TITLE: &str = "Stop the stream?";
const STOP_DETAIL: &str = "The channel goes offline until you start it again.";
const STOP_OK: &str = "Stop stream";

/// The Monitor tab. It shows the last answered poll; before the first one, index.html's placeholders.
pub struct MonitorView {
    app: AppHandle,
    busy: Busy,
    status: Option<Status>,
    events: Option<Vec<Event>>,
    pub(crate) videos: VideoList,
    pub(crate) logs: Entity<LogViewer>,
}

impl MonitorView {
    pub fn new(app: AppHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            videos: VideoList::new(app.clone(), window, cx),
            logs: cx.new(|cx| LogViewer::new(app.clone(), window, cx)),
            app,
            busy: Busy::default(),
            status: None,
            events: None,
        }
    }

    /// The video on air, from the last poll.
    pub fn playing(&self) -> Option<&Playing> {
        self.status.as_ref()?.supervisor.as_ref()?.playing.as_ref()
    }

    /// admin.js `renderStream`.
    fn render_stream_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status.as_ref();
        let line = status.map(|s| {
            let youtube = s.supervisor.as_ref().and_then(|sup| sup.youtube.as_ref());
            stream_line(Some(&s.stream), youtube_note(youtube).as_deref())
        });
        let (word, level, reason) = match line {
            Some(line) => (line.word, line.level, line.reason),
            None => ("Loading", Level::Muted, None),
        };
        // Both buttons show until the first poll, as in index.html.
        let state = status.map(|s| s.stream.state.as_str());
        let show_start = state.is_none_or(|s| s == "stopped" || s == "off-air");
        let show_stop = state != Some("stopped");
        let twitch = status.map(|s| twitch_line(s.twitch.as_ref()));
        let encoder = status.map(|s| encoder_line(s.supervisor.as_ref()));

        card()
            .mb(px(24.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        Label::new(word)
                            .text_size(px(28.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(level_color(level))),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .when(show_start, |d| {
                                d.child(reveal_on_focus(self.start_button(cx)))
                            })
                            .when(show_stop, |d| {
                                d.child(reveal_on_focus(self.stop_button(cx)))
                            }),
                    ),
            )
            .children(reason.map(|r| Label::new(r).mt(px(4.)).text_size(px(15.))))
            .children(twitch.map(|(text, level)| detail(text, level)))
            .children(encoder.map(|text| detail(text, Level::Muted)))
    }

    fn start_button(&self, cx: &mut Context<Self>) -> Button {
        btn(action_button(
            "btn-stream-start",
            "Start stream",
            &self.busy,
        ))
        .on_click(cx.listener(|this, _, window, cx| {
            let app = this.app.clone();
            run_action(
                this,
                "btn-stream-start",
                &app,
                |c| c.stream_start(),
                |_, _, _, _| {},
                window,
                cx,
            );
        }))
    }

    fn stop_button(&self, cx: &mut Context<Self>) -> Button {
        btn(action_button("btn-stream-stop", "Stop stream", &self.busy))
            .on_click(cx.listener(|this, _, window, cx| this.ask_to_stop(window, cx)))
    }

    /// admin.js asks with `confirm()` before it stops; here a native dialog does.
    fn ask_to_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ask = self
            .app
            .confirm(STOP_TITLE, STOP_DETAIL, STOP_OK, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if !ask.await {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                let app = this.app.clone();
                run_action(
                    this,
                    "btn-stream-stop",
                    &app,
                    |c| c.stream_stop(),
                    |_, _, _, _| {},
                    window,
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    /// admin.js `renderNowPlaying`, without the video list's highlight.
    fn render_now_playing(&self) -> impl IntoElement {
        let supervisor = self.status.as_ref().and_then(|s| s.supervisor.as_ref());
        let none = Playing::default();
        let p = self.playing().unwrap_or(&none);
        let count = supervisor.and_then(|s| s.playlist_count);
        let title = p.title.clone().filter(|t| !t.is_empty());
        let video_id = match p.id.as_deref().filter(|id| !id.is_empty()) {
            Some(id) => video_link(id),
            None => value("-"),
        };

        section("Now Playing").child(
            div()
                .grid()
                .grid_cols(2)
                .gap(px(12.))
                .p(px(16.))
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(BORDER))
                .rounded(px(8.))
                .child(
                    np_item("Video Title", value(title.unwrap_or_else(|| "-".into())))
                        .col_span_full(),
                )
                .child(np_item("Video", value(now_playing_index(p, count))))
                .child(np_item("Video ID", video_id))
                .child(np_item("Position", value(now_playing_time(p))))
                .child(np_item("Source", value(source_label(p.source.as_deref())))),
        )
    }

    /// index.html's `.card.section` under Now Playing: the playlist dropdown, the transport buttons and the video list.
    fn render_playlist_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        card()
            .mb(px(24.))
            .child(self.videos.render_dropdown())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .mb(px(12.))
                    .child(reveal_on_focus(
                        self.player_button("btn-prev", "Previous", "prev", cx),
                    ))
                    .child(reveal_on_focus(self.player_button(
                        "btn-restart",
                        "Restart video",
                        "restart",
                        cx,
                    )))
                    .child(reveal_on_focus(
                        self.player_button("btn-next", "Next", "next", cx),
                    )),
            )
            .child(self.videos.render_list(&self.busy, cx))
    }

    fn player_button(
        &self,
        id: &'static str,
        label: &'static str,
        cmd: &'static str,
        cx: &mut Context<Self>,
    ) -> Button {
        btn(action_button(id, label, &self.busy)).on_click(cx.listener(
            move |this, _, window, cx| {
                let app = this.app.clone();
                run_action(
                    this,
                    id,
                    &app,
                    move |c| c.player(cmd),
                    |_, _, _, _| {},
                    window,
                    cx,
                );
            },
        ))
    }

    /// admin.js `renderEvents`: newest first, each line selectable.
    fn render_timeline(&self) -> impl IntoElement {
        let rows: Vec<AnyElement> = match self.events.as_deref() {
            None => vec![empty_log("Loading events...")],
            Some([]) => vec![empty_log("No events yet")],
            Some(events) => events
                .iter()
                .zip(event_keys(events))
                .enumerate()
                .rev()
                .map(|(i, (event, key))| {
                    let line: SharedString =
                        format!("{} {}", local_hms(&event.timestamp), event.message).into();
                    div()
                        .px(px(12.))
                        .py(px(6.))
                        // The oldest line is the last one and has no border under it.
                        .when(i > 0, |d| d.border_b_1().border_color(rgb(BORDER)))
                        .child(SelectableText::new(("event", key), line.clone()).test_text(line))
                        .into_any_element()
                })
                .collect(),
        };
        section("Timeline").child(
            div()
                .id("event-log")
                .max_h(px(300.))
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(BORDER))
                .rounded(px(8.))
                .font_family(MONO_FONT)
                .text_size(px(12.))
                .overflow_y_scrollbar()
                .children(rows),
        )
    }
}

impl ActionView for MonitorView {
    fn busy(&mut self) -> &mut Busy {
        &mut self.busy
    }
}

impl TabView for MonitorView {
    fn on_dashboard_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.videos.on_dashboard_open(window, cx);
        self.logs.update(cx, |logs, cx| logs.load_list(window, cx));
    }

    fn on_poll(
        &mut self,
        status: &Status,
        events: &[Event],
        _update: Option<&UpdateStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.status = Some(status.clone());
        self.events = Some(events.to_vec());
        // admin.js `renderNowPlaying` passes an empty video when nothing plays.
        let playing = self.playing().cloned().unwrap_or_default();
        self.videos.on_status(&playing, window, cx);
        cx.notify();
    }

    fn on_playlists_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.videos.on_playlists_saved(window, cx);
    }
}

impl Render for MonitorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(self.render_stream_card(cx))
            .child(self.render_now_playing())
            .child(self.render_playlist_card(cx))
            .child(self.render_timeline())
            .child(section("Logs").child(self.logs.clone()))
    }
}

/// One key per event from its time and text, so a selected line keeps its event when a new event
/// arrives or the server drops its oldest. A repeat of the same time and text is counted in.
fn event_keys(events: &[Event]) -> Vec<u64> {
    let mut seen: HashMap<(&str, &str), u64> = HashMap::new();
    events
        .iter()
        .map(|event| {
            let repeat = seen.entry((&event.timestamp, &event.message)).or_default();
            let mut hasher = DefaultHasher::new();
            (&event.timestamp, &event.message, *repeat).hash(&mut hasher);
            *repeat += 1;
            hasher.finish()
        })
        .collect()
}

/// The Video ID link, with the id escaped as `encodeURIComponent` does.
pub fn youtube_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={}", encode_component(id))
}

/// admin.css `.card`.
fn card() -> Div {
    div()
        .p(px(16.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(8.))
}

/// admin.css `.stream-detail` in the colour of `level`.
fn detail(text: String, level: Level) -> Label {
    Label::new(text)
        .mt(px(4.))
        .text_size(px(13.))
        .text_color(rgb(level_color(level)))
}

/// admin.css `.np-item`: the label in capitals over the value.
fn np_item(label: &str, value: AnyElement) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .child(
            Label::new(label.to_uppercase())
                .text_size(px(11.))
                .text_color(rgb(MUTED)),
        )
        .child(value)
}

/// admin.css `.np-value`.
fn value(text: impl Into<SharedString>) -> AnyElement {
    Label::new(text).text_size(px(15.)).into_any_element()
}

/// The Video ID as a link to the video. It is a button, so Tab reaches it and Enter opens it.
fn video_link(id: &str) -> AnyElement {
    let url = youtube_url(id);
    let link = Button::new("np-videoid")
        .link()
        .text_decoration_none()
        .accessibility_label(id.to_string())
        // Label sets its own colour, so the link's accent goes on it, as admin.css `.np-value a`.
        .child(
            Label::new(id.to_string())
                .text_size(px(15.))
                .text_color(rgb(ACCENT)),
        )
        .on_click(move |_, _, cx| cx.open_url(&url));
    // The row keeps the button as wide as its text, as an inline `<a>`.
    div().flex().child(reveal_on_focus(link)).into_any_element()
}

/// admin.css `.event-empty`.
fn empty_log(text: &str) -> AnyElement {
    div()
        .p(px(24.))
        .flex()
        .justify_center()
        .child(
            Label::new(text.to_string())
                .text_size(px(12.))
                .text_color(rgb(MUTED)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Local};
    use gpui::{AnyWindowHandle, AppContext, ScrollDelta, TestAppContext, point};
    use gpui_kit::{
        base::{TextSelection, test_support::texts},
        test::TestWindowExt,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi},
        app::POLL_EVERY,
        test_support::{events_json, open_app, painted, shows, status_json, update_json},
    };

    const STOP_TITLE: &str = "Stop the stream?";
    const STOP_DETAIL: &str = "The channel goes offline until you start it again.";
    const REFUSED: &str = "The stream is stopped. Start it first.";

    /// Opens the dashboard on `status` and `events`, with every Monitor POST answering ok.
    fn open_with(
        cx: &mut TestAppContext,
        status: Value,
        events: Value,
    ) -> (Arc<FakeApi>, AnyWindowHandle) {
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status));
        fake.reply("GET", "/api/events", Ok(events));
        fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
        for path in [
            "/api/stream/start",
            "/api/stream/stop",
            "/api/player/prev",
            "/api/player/restart",
            "/api/player/next",
        ] {
            fake.reply("POST", path, Ok(json!({ "ok": true })));
        }
        let (_, window) = open_app(cx, fake.clone());
        (fake, window)
    }

    fn present(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> bool {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).is_some()
        })
        .unwrap()
    }

    fn label(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) -> Option<String> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find(id).label().map(str::to_string)
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

    fn post_paths(fake: &FakeApi) -> Vec<String> {
        fake.posts().into_iter().map(|r| r.path).collect()
    }

    #[gpui::test]
    fn start_shows_when_stopped_or_off_air_and_stop_unless_stopped(cx: &mut TestAppContext) {
        let (fake, window) = open_with(cx, status_json("live", false), events_json(0));

        for (state, start, stop) in [
            ("stopped", true, false),
            ("off-air", true, true),
            ("live", false, true),
        ] {
            fake.reply("GET", "/api/status", Ok(status_json(state, false)));
            cx.executor().advance_clock(POLL_EVERY);

            let shown = (
                present(cx, window, "btn-stream-start"),
                present(cx, window, "btn-stream-stop"),
            );
            assert_eq!(shown, (start, stop), "state {state}");
        }
        fake.reply("GET", "/api/status", Ok(status_json("off-air", false)));
        cx.executor().advance_clock(POLL_EVERY);
        assert_eq!(
            label(cx, window, "btn-stream-start").as_deref(),
            Some("Start stream")
        );
        assert_eq!(
            label(cx, window, "btn-stream-stop").as_deref(),
            Some("Stop stream")
        );
    }

    #[gpui::test]
    fn start_posts_and_polls_at_once(cx: &mut TestAppContext) {
        let (fake, window) = open_with(cx, status_json("stopped", false), events_json(0));
        let before = fake.requests().len();

        click(cx, window, "btn-stream-start");
        cx.run_until_parked();

        let sent: Vec<(&str, String)> = fake.requests()[before..]
            .iter()
            .map(|r| (r.method, r.path.clone()))
            .collect();
        assert_eq!(sent[0], ("POST", "/api/stream/start".to_string()));
        assert!(sent[1..].contains(&("GET", "/api/status".to_string())));
        assert_eq!(post_paths(&fake), ["/api/stream/start"]);
    }

    #[gpui::test]
    fn stop_asks_first_and_sends_nothing_on_cancel(cx: &mut TestAppContext) {
        let (fake, window) = open_with(cx, status_json("live", false), events_json(0));

        click(cx, window, "btn-stream-stop");
        assert_eq!(
            cx.pending_prompt(),
            Some((STOP_TITLE.to_string(), STOP_DETAIL.to_string()))
        );
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(fake.posts().is_empty());

        click(cx, window, "btn-stream-stop");
        cx.simulate_prompt_answer("Stop stream");
        cx.run_until_parked();
        assert_eq!(post_paths(&fake), ["/api/stream/stop"]);
    }

    #[gpui::test]
    fn transport_buttons_post_their_command(cx: &mut TestAppContext) {
        let (fake, window) = open_with(cx, status_json("live", false), events_json(0));
        let labels = ["btn-prev", "btn-restart", "btn-next"].map(|id| label(cx, window, id));
        assert_eq!(
            labels,
            ["Previous", "Restart video", "Next"].map(|l| Some(l.to_string()))
        );

        for id in ["btn-prev", "btn-restart", "btn-next"] {
            click(cx, window, id);
            cx.run_until_parked();
        }
        assert_eq!(
            post_paths(&fake),
            [
                "/api/player/prev",
                "/api/player/restart",
                "/api/player/next"
            ]
        );

        fake.reply(
            "POST",
            "/api/player/next",
            Err(ApiError::Server(REFUSED.into())),
        );
        click(cx, window, "btn-next");
        cx.run_until_parked();
        assert!(shows(cx, window, REFUSED));
    }

    #[gpui::test]
    fn a_second_click_while_busy_sends_nothing(cx: &mut TestAppContext) {
        let (fake, window) = open_with(cx, status_json("live", false), events_json(0));

        // Both clicks land before the request's task runs, so its answer is still held back.
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("btn-next", cx);
            assert_eq!(window.find("btn-next").disabled(), Some(true));
            window.click("btn-next", cx);
        })
        .unwrap();
        cx.run_until_parked();

        assert_eq!(post_paths(&fake), ["/api/player/next"]);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("btn-next").disabled(), None);
        })
        .unwrap();
    }

    #[gpui::test]
    fn video_id_opens_youtube(cx: &mut TestAppContext) {
        let mut status = status_json("live", false);
        status["supervisor"]["playing"]["id"] = json!("dQw4w9WgXcQ");
        let (fake, window) = open_with(cx, status, events_json(0));
        assert_eq!(
            label(cx, window, "np-videoid").as_deref(),
            Some("dQw4w9WgXcQ")
        );

        click(cx, window, "np-videoid");

        assert_eq!(
            cx.opened_url().as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
        assert!(fake.posts().is_empty());
    }

    #[test]
    fn youtube_url_escapes_the_id_like_encode_uri_component() {
        assert_eq!(
            youtube_url("a b&c=d/é"),
            "https://www.youtube.com/watch?v=a%20b%26c%3Dd%2F%C3%A9"
        );
        assert_eq!(
            youtube_url("Ab_9-x.!~*'()"),
            "https://www.youtube.com/watch?v=Ab_9-x.!~*'()"
        );
    }

    #[gpui::test]
    fn stream_card_and_now_playing_show_the_status(cx: &mut TestAppContext) {
        let (fake, window) = open_with(cx, status_json("live", false), events_json(0));
        let texts = painted(cx, window);
        for text in [
            "Live",
            "Twitch check off",
            "Encoder: running, 1.00x speed",
            "Third video",
            "3 of 10",
            "0:01:01 / 1:00:00",
            "Cached file",
        ] {
            assert!(texts.iter().any(|t| t == text), "{text} in {texts:?}");
        }
        assert_eq!(
            label(cx, window, "np-videoid").as_deref(),
            Some("vid-three")
        );

        fake.reply("GET", "/api/status", Ok(status_json("stopped", false)));
        cx.executor().advance_clock(POLL_EVERY);
        let texts = painted(cx, window);
        for text in ["Stopped", "Encoder: not running"] {
            assert!(texts.iter().any(|t| t == text), "{text} in {texts:?}");
        }
        assert!(!texts.iter().any(|t| t == "Third video"));
        assert!(!present(cx, window, "np-videoid"));
    }

    #[gpui::test]
    fn stream_card_shows_the_youtube_note(cx: &mut TestAppContext) {
        let blocked = json!({ "breaker": "open", "retryAt": null, "cookieProblem": null });
        let mut status = status_json("live", false);
        status["supervisor"]["youtube"] = blocked.clone();
        let (fake, window) = open_with(cx, status, events_json(0));
        assert!(shows(cx, window, "Live"));
        assert!(shows(cx, window, "YouTube is blocking requests."));

        let mut status = status_json("off-air", false);
        status["stream"]["reason"] = json!("The encoder cannot reach Twitch.");
        status["supervisor"]["youtube"] = blocked;
        fake.reply("GET", "/api/status", Ok(status));
        cx.executor().advance_clock(POLL_EVERY);
        assert!(shows(cx, window, "Off air"));
        assert!(shows(
            cx,
            window,
            "The encoder cannot reach Twitch. YouTube is blocking requests."
        ));
    }

    fn local(ts: &str) -> String {
        DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Local)
            .format("%H:%M:%S")
            .to_string()
    }

    /// The playlist card pushes the Timeline below the window, so this scrolls the dashboard to its end.
    fn scroll_to_timeline(cx: &mut TestAppContext, window: AnyWindowHandle) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.scroll(
                "btn-next",
                ScrollDelta::Pixels(point(px(0.), px(-2000.))),
                cx,
            );
        })
        .unwrap();
    }

    #[gpui::test]
    fn timeline_is_newest_first_with_local_times(cx: &mut TestAppContext) {
        let (_, window) = open_with(cx, status_json("live", false), events_json(3));
        scroll_to_timeline(cx, window);

        let lines: Vec<String> = painted(cx, window)
            .into_iter()
            .filter(|t| t.contains("Event "))
            .collect();

        let expected: Vec<String> = [3, 2, 1]
            .iter()
            .map(|i| {
                format!(
                    "{} Event {i}",
                    local(&format!("2026-11-08T12:00:{i:02}.000Z"))
                )
            })
            .collect();
        assert_eq!(lines, expected);
    }

    #[gpui::test]
    fn timeline_says_no_events_yet_when_empty(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        // `/api/events` unanswered: the poll fails, so no events have arrived yet.
        let (_, window) = open_app(cx, fake.clone());
        scroll_to_timeline(cx, window);
        assert!(shows(cx, window, "Loading events..."));

        fake.reply("GET", "/api/events", Ok(json!([])));
        cx.executor().advance_clock(POLL_EVERY);

        scroll_to_timeline(cx, window);
        assert!(shows(cx, window, "No events yet"));
        assert!(!shows(cx, window, "Loading events..."));
    }

    fn events(numbers: &[usize]) -> Value {
        let all = events_json(10);
        Value::Array(numbers.iter().map(|n| all[n - 1].clone()).collect())
    }

    #[gpui::test]
    fn a_selected_line_stays_selected_when_the_oldest_event_drops_off(cx: &mut TestAppContext) {
        let (fake, window) = open_with(cx, status_json("live", false), events(&[1, 2, 3]));
        scroll_to_timeline(cx, window);
        let line = format!("{} Event 2", local("2026-11-08T12:00:02.000Z"));
        let selected = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                TextSelection::selected_text(window, cx)
            })
            .unwrap()
        };

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let bounds = texts(window)
                .into_iter()
                .find(|t| t.text() == line)
                .expect("the line is painted")
                .bounds();
            let y = bounds.center().y;
            window.drag(
                point(bounds.left() + px(1.), y),
                point(bounds.right() - px(1.), y),
                cx,
            );
        })
        .unwrap();
        assert_eq!(selected(cx), line);

        // The server keeps 100 events: a new one pushes the oldest out.
        fake.reply("GET", "/api/events", Ok(events(&[2, 3, 4])));
        cx.executor().advance_clock(POLL_EVERY);

        assert_eq!(selected(cx), line);
    }
}
