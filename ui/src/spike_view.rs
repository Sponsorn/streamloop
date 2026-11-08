//! The GPUI spike's monitor view, moved onto gpui-kit. It polls the same API as the web dashboard
//! every 2 s, so its memory can be measured against the spike's and the Tauri shell's.

use std::{ops::Range, time::Duration};

use gpui::{
    AsyncApp, Context, Div, ElementId, FontWeight, PromptLevel, Rgba, SharedString, Stateful, Task,
    WeakEntity, Window, div, prelude::*, px, rgb, uniform_list,
};
use serde_json::json;

use crate::spike_api as api;

const POLL_EVERY: Duration = Duration::from_secs(2);

// The web dashboard's colour tokens (src/admin/admin.css).
const BG: u32 = 0x0f1117;
const SURFACE: u32 = 0x1a1d27;
const SURFACE_HOVER: u32 = 0x222633;
const BORDER: u32 = 0x2a2e3d;
const TEXT: u32 = 0xe1e4eb;
const MUTED: u32 = 0x8b8fa3;
const ACCENT: u32 = 0x6c5ce7;
const GREEN: u32 = 0x00b894;
const RED: u32 = 0xe74c3c;
const YELLOW: u32 = 0xf39c12;

#[derive(Clone, Copy)]
enum Level {
    Ok,
    Warn,
    Err,
    Muted,
}

impl Level {
    fn color(self) -> Rgba {
        rgb(match self {
            Level::Ok => GREEN,
            Level::Warn => YELLOW,
            Level::Err => RED,
            Level::Muted => MUTED,
        })
    }
}

struct EventRow {
    time: SharedString,
    message: SharedString,
}

struct VideoRow {
    index: u32,
    id: String,
    number: SharedString,
    title: SharedString,
    duration: SharedString,
}

/// One poll's worth of data, formatted off the UI thread.
struct Snapshot {
    status: api::Status,
    events: Vec<EventRow>,
    update: Option<api::UpdateStatus>,
}

pub(crate) struct Dashboard {
    token: String,
    playlists: Option<Vec<api::PlaylistRef>>,
    status: Option<api::Status>,
    events: Vec<EventRow>,
    update: Option<api::UpdateStatus>,
    videos: Vec<VideoRow>,
    playing_list: usize,
    loaded_list: Option<usize>,
    loading_videos: bool,
    poll_failures: u32,
    busy: bool,
    toast: Option<SharedString>,
    _poll: Task<()>,
}

impl Dashboard {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            while refresh(&this, cx).await {
                cx.background_executor().timer(POLL_EVERY).await;
            }
        });
        cx.spawn(async move |this, cx| {
            // The backend may not be up yet; the dashboard needs the token and the list names once.
            loop {
                let got = cx
                    .background_executor()
                    .spawn(async {
                        let token = api::get::<api::Token>("/api/token")?.token;
                        let playlists = api::get::<api::Config>("/api/config")?.playlists;
                        Ok::<_, String>((token, playlists))
                    })
                    .await;
                if let Ok((token, playlists)) = got {
                    this.update(cx, |this, cx| {
                        this.token = token;
                        this.playlists = Some(playlists);
                        this.sync_playlist(cx);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                cx.background_executor().timer(POLL_EVERY).await;
            }
        })
        .detach();

        Self {
            token: String::new(),
            playlists: None,
            status: None,
            events: Vec::new(),
            update: None,
            videos: Vec::new(),
            playing_list: 0,
            loaded_list: None,
            loading_videos: false,
            poll_failures: 0,
            busy: false,
            toast: None,
            _poll: poll,
        }
    }

    fn playing(&self) -> Option<&api::Playing> {
        self.status.as_ref()?.supervisor.as_ref()?.playing.as_ref()
    }

    fn apply(&mut self, result: Result<Snapshot, String>, cx: &mut Context<Self>) {
        match result {
            Ok(snapshot) => {
                self.poll_failures = 0;
                self.status = Some(snapshot.status);
                self.events = snapshot.events;
                if snapshot.update.is_some() {
                    self.update = snapshot.update;
                }
                self.sync_playlist(cx);
            }
            Err(_) => self.poll_failures += 1,
        }
    }

    /// Shows the list on air, loading it when the supervisor moves to another one.
    fn sync_playlist(&mut self, cx: &mut Context<Self>) {
        let Some(playlists) = &self.playlists else { return };
        let playing_id = self.playing().and_then(|p| p.playlist_id.as_deref());
        self.playing_list = playing_id
            .and_then(|id| playlists.iter().position(|p| p.id == id))
            .unwrap_or(0);
        if self.loaded_list != Some(self.playing_list) && !self.loading_videos {
            self.load_videos(self.playing_list, cx);
        }
    }

    fn load_videos(&mut self, list: usize, cx: &mut Context<Self>) {
        self.loading_videos = true;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { api::all_videos(list).map(|vs| vs.into_iter().map(video_row).collect()) })
                .await;
            this.update(cx, |this, cx| {
                this.loading_videos = false;
                match result {
                    Ok(rows) => {
                        this.videos = rows;
                        this.loaded_list = Some(list);
                    }
                    Err(e) => this.toast = Some(format!("Could not load the playlist: {e}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn post_action(&mut self, path: &'static str, body: Option<serde_json::Value>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();
        let token = self.token.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { api::post(path, &token, body) }).await;
            let still_open = this
                .update(cx, |this, cx| {
                    this.busy = false;
                    this.toast = result.err().map(SharedString::from);
                    cx.notify();
                })
                .is_ok();
            if still_open {
                refresh(&this, cx).await;
            }
        })
        .detach();
    }

    fn confirm_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            "Stop the stream?",
            Some("The channel goes offline until you start it again."),
            &["Stop", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |this, cx| this.post_action("/api/stream/stop", None, cx)).ok();
            }
        })
        .detach();
    }

    fn render_header(&self) -> Div {
        let version = self.update.as_ref().and_then(|u| u.current_version.clone()).unwrap_or_default();
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("StreamLoop"))
            .child(div().text_color(rgb(MUTED)).child(if version.is_empty() { String::new() } else { format!("v{version}") }))
    }

    fn render_stream(&self, cx: &mut Context<Self>) -> Div {
        let status = self.status.clone().unwrap_or_default();
        let (word, level) = stream_word(&status.stream.state);
        let youtube = status.supervisor.as_ref().and_then(|s| s.youtube.as_ref());
        let reason = stream_reason(&status, youtube_note(youtube));
        let (twitch, twitch_level) = twitch_line(status.twitch.as_ref());
        let state = status.stream.state.as_str();

        card("Stream")
            .child(div().text_size(px(28.)).font_weight(FontWeight::BOLD).text_color(level.color()).child(word))
            .when_some(reason, |d, r| d.child(div().text_color(rgb(MUTED)).child(r)))
            .child(div().text_color(twitch_level.color()).child(twitch))
            .child(div().child(encoder_line(status.supervisor.as_ref())))
            .child(div().text_color(rgb(MUTED)).child(format!("Uptime: {}", format_uptime(status.uptime_ms))))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(state == "stopped" || state == "off-air", |d| {
                        d.child(
                            button("start", "Start stream", self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.post_action("/api/stream/start", None, cx))),
                        )
                    })
                    .when(state != "stopped", |d| {
                        d.child(
                            button("stop", "Stop stream", self.busy)
                                .on_click(cx.listener(|this, _, window, cx| this.confirm_stop(window, cx))),
                        )
                    }),
            )
    }

    fn render_now_playing(&self, cx: &mut Context<Self>) -> Div {
        let p = self.playing().cloned().unwrap_or_default();
        let count = self.status.as_ref().and_then(|s| s.supervisor.as_ref()).and_then(|s| s.playlist_count);
        let index = match (p.index, count) {
            (Some(i), Some(n)) => format!("{} of {n}", i + 1),
            (Some(i), None) => (i + 1).to_string(),
            _ => "-".into(),
        };
        let time = match p.position {
            Some(pos) => format!("{} / {}", format_time(pos), p.duration.map(format_time).unwrap_or_else(|| "--:--:--".into())),
            None => "-".into(),
        };
        card("Now playing")
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).truncate().child(p.title.unwrap_or_else(|| "-".into())))
            .child(field("Video", index))
            .child(field("Video id", p.id.unwrap_or_else(|| "-".into())))
            .child(field("Time", time))
            .child(field("Source", source_label(p.source.as_deref())))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button("prev", "Previous", self.busy).on_click(cx.listener(|this, _, _, cx| this.post_action("/api/player/prev", None, cx))))
                    .child(button("next", "Next", self.busy).on_click(cx.listener(|this, _, _, cx| this.post_action("/api/player/next", None, cx))))
                    .child(button("restart", "Restart video", self.busy).on_click(cx.listener(|this, _, _, cx| this.post_action("/api/player/restart", None, cx)))),
            )
    }

    fn render_timeline(&self) -> Div {
        card("Timeline").flex_1().min_h_0().child(
            div()
                .id("timeline")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(self.events.iter().map(|e| {
                    div()
                        .flex()
                        .gap_3()
                        .py_1()
                        .border_b_1()
                        .border_color(rgb(BORDER))
                        .child(div().flex_none().text_color(rgb(MUTED)).child(e.time.clone()))
                        .child(div().flex_1().min_w_0().child(e.message.clone()))
                })),
        )
    }

    fn render_playlist(&self, cx: &mut Context<Self>) -> Div {
        let name = self
            .playlists
            .as_ref()
            .and_then(|ps| ps.get(self.playing_list))
            .map(|p| p.name.clone().unwrap_or_else(|| p.id.clone()))
            .unwrap_or_else(|| "Playlist".into());
        let list = uniform_list(
            "videos",
            self.videos.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                let on_air = this.loaded_list == Some(this.playing_list);
                let playing = this.playing().and_then(|p| p.index);
                range
                    .map(|ix| {
                        let v = &this.videos[ix];
                        let current = on_air && playing == Some(v.index);
                        let (index, id) = (v.index, v.id.clone());
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_2()
                            .h(px(34.))
                            .when(current, |d| d.bg(rgb(SURFACE_HOVER)).text_color(rgb(GREEN)))
                            .child(div().w(px(40.)).text_color(rgb(MUTED)).child(v.number.clone()))
                            .child(div().flex_1().min_w_0().truncate().child(v.title.clone()))
                            .child(div().w(px(64.)).text_color(rgb(MUTED)).child(v.duration.clone()))
                            .child(button(("play", ix), "▶", this.busy).on_click(cx.listener(move |this, _, _, cx| {
                                this.post_action("/api/player/jump", Some(json!({ "index": index, "id": id.clone() })), cx)
                            })))
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full();
        card("Playlist")
            .flex_1()
            .min_h_0()
            .child(div().text_color(rgb(MUTED)).child(format!("{name} (playing), {} videos", self.videos.len())))
            .child(div().flex_1().min_h_0().child(list))
    }
}

impl Render for Dashboard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let update = self.update.as_ref().filter(|u| u.update_available).map(|u| {
            format!(
                "Version {} is available (current: {})",
                u.latest_version.as_deref().unwrap_or("?"),
                u.current_version.as_deref().unwrap_or("?")
            )
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(".SystemUIFont")
            .text_sm()
            .child(self.render_header())
            .when(self.poll_failures >= 2, |d| d.child(banner("The connection to StreamLoop was lost. Retrying.", Level::Err)))
            .when_some(update, |d, text| d.child(banner(text, Level::Warn)))
            .child(div().flex().gap_4().child(self.render_stream(cx).flex_1()).child(self.render_now_playing(cx).flex_1().min_w_0()))
            .child(div().flex_1().min_h_0().flex().gap_4().child(self.render_timeline()).child(self.render_playlist(cx)))
            .when_some(self.toast.clone(), |d, text| {
                d.child(
                    div()
                        .id("toast")
                        .p_3()
                        .rounded(px(8.))
                        .bg(rgb(SURFACE_HOVER))
                        .border_1()
                        .border_color(rgb(RED))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toast = None;
                            cx.notify();
                        }))
                        .child(text),
                )
            })
    }
}

/// Polls once and hands the result to the dashboard; false once the window is gone.
async fn refresh(this: &WeakEntity<Dashboard>, cx: &mut AsyncApp) -> bool {
    let result = cx.background_executor().spawn(async { fetch_snapshot() }).await;
    this.update(cx, |this, cx| {
        this.apply(result, cx);
        cx.notify();
    })
    .is_ok()
}

fn fetch_snapshot() -> Result<Snapshot, String> {
    let status = api::get::<api::Status>("/api/status")?;
    let events = api::get::<Vec<api::Event>>("/api/events")?;
    let update = api::get::<api::UpdateStatus>("/api/update/status").ok();
    Ok(Snapshot {
        status,
        events: events
            .into_iter()
            .rev()
            .map(|e| EventRow { time: local_time(&e.timestamp, "%H:%M:%S").into(), message: e.message.into() })
            .collect(),
        update,
    })
}

fn video_row(v: api::Video) -> VideoRow {
    VideoRow {
        index: v.index,
        id: v.id.unwrap_or_default(),
        number: (v.index + 1).to_string().into(),
        title: v.title.unwrap_or_else(|| "Untitled".into()).into(),
        duration: v.duration.map(|d| format_time(d).trim_start_matches("0:").to_string()).unwrap_or_else(|| "-".into()).into(),
    }
}

fn card(title: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(8.))
        .child(div().text_xs().text_color(rgb(MUTED)).child(title.to_uppercase()))
}

fn field(label: &'static str, value: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .gap_2()
        .child(div().w(px(70.)).text_color(rgb(MUTED)).child(label))
        .child(div().flex_1().min_w_0().truncate().child(value.into()))
}

fn banner(text: impl Into<SharedString>, level: Level) -> Div {
    div().p_3().rounded(px(8.)).bg(rgb(SURFACE)).border_1().border_color(level.color()).child(text.into())
}

fn button(id: impl Into<ElementId>, label: &'static str, busy: bool) -> Stateful<Div> {
    div()
        .id(id.into())
        .flex_none()
        .px_3()
        .py_1()
        .rounded(px(6.))
        .bg(rgb(SURFACE_HOVER))
        .border_1()
        .border_color(rgb(BORDER))
        .when(busy, |d| d.opacity(0.5))
        .when(!busy, |d| d.cursor_pointer().hover(|s| s.bg(rgb(ACCENT))))
        .child(label)
}

fn stream_word(state: &str) -> (&'static str, Level) {
    match state {
        "live" => ("Live", Level::Ok),
        "recovering" => ("Recovering", Level::Warn),
        "starting" => ("Starting", Level::Warn),
        "stopped" => ("Stopped", Level::Muted),
        _ => ("Off air", Level::Err),
    }
}

/// The reason under the stream word, as the web dashboard's streamLine builds it.
fn stream_reason(status: &api::Status, note: Option<String>) -> Option<String> {
    let state = status.stream.state.as_str();
    let known = matches!(state, "live" | "recovering" | "starting" | "off-air" | "stopped");
    let reason = if state == "live" {
        None
    } else {
        status.stream.reason.clone().or_else(|| (!known).then(|| "The server sent no stream state.".into()))
    };
    match (reason, note) {
        (r, None) => r,
        (r, Some(_)) if state == "stopped" => r,
        (Some(r), Some(n)) => Some(format!("{r} {n}")),
        (None, Some(n)) => Some(n),
    }
}

fn youtube_note(youtube: Option<&api::Youtube>) -> Option<String> {
    let yt = youtube?;
    if let Some(problem) = &yt.cookie_problem {
        return Some(problem.clone());
    }
    if yt.breaker.as_deref() != Some("open") {
        return None;
    }
    Some(match &yt.retry_at {
        Some(at) => format!("YouTube is blocking requests. Next try at {}.", local_time(at, "%H:%M")),
        None => "YouTube is blocking requests.".into(),
    })
}

fn twitch_line(twitch: Option<&api::Twitch>) -> (String, Level) {
    let Some(tw) = twitch.filter(|t| t.enabled) else {
        return ("Twitch check off".into(), Level::Muted);
    };
    let (mut text, level) = match tw.channel_live {
        None => ("Twitch: checking".to_string(), Level::Warn),
        Some(true) => ("Twitch: live".to_string(), Level::Ok),
        Some(false) if tw.consecutive_mismatches > 0 => ("Twitch: offline".to_string(), Level::Err),
        Some(false) => ("Twitch: offline".to_string(), Level::Warn),
    };
    if tw.consecutive_mismatches > 0 {
        text += &format!(" ({}/{})", tw.consecutive_mismatches, tw.mismatch_threshold);
    }
    let rb = tw.readback.clone().unwrap_or_default();
    match rb.state.as_str() {
        "" | "off" => (text + " · read-back off", level),
        "frozen" => (text + " · read-back: picture frozen", Level::Err),
        "unavailable" => (text + " · read-back: no picture from Twitch", if matches!(level, Level::Ok) { Level::Warn } else { level }),
        _ if rb.last_check_at.is_some() => (text + " · read-back: picture moving", level),
        _ => (text + " · read-back: waiting for the first check", level),
    }
}

fn encoder_line(sup: Option<&api::Supervisor>) -> String {
    match sup.and_then(|s| s.encoder.as_ref()).filter(|e| e.alive) {
        None => "Encoder: not running".into(),
        Some(enc) => {
            let mut text = String::from("Encoder: running");
            if let Some(speed) = enc.speed {
                text += &format!(", {speed:.2}x speed");
            }
            if enc.restarts > 0 {
                text += &format!(", {} restart{}", enc.restarts, if enc.restarts == 1 { "" } else { "s" });
            }
            text
        }
    }
}

fn source_label(source: Option<&str>) -> String {
    match source {
        Some("file") => "Cached file".into(),
        Some("url") => "YouTube".into(),
        Some("slate") => "Standby slate".into(),
        Some(other) => other.into(),
        None => "-".into(),
    }
}

fn format_time(seconds: f64) -> String {
    if seconds.is_nan() || seconds <= 0.0 {
        return "0:00:00".into();
    }
    let s = seconds as u64;
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn format_uptime(ms: u64) -> String {
    let s = ms / 1000;
    let (d, h, m) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", s % 60)
    }
}

/// An RFC 3339 timestamp in local time; the raw text when it does not parse.
fn local_time(ts: &str, fmt: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&chrono::Local).format(fmt).to_string())
        .unwrap_or_else(|_| ts.to_string())
}
