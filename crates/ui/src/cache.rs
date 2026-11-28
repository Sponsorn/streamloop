//! The Cache tab: the cache against its budget, the next download, the look-ahead and the last deletions.

use gpui::{
    AnyElement, Context, Div, FontWeight, IntoElement, Render, Role, Window, div, prelude::*, px,
    rgb,
};
use gpui_kit::component::label::Label;

use crate::{
    api::{
        ApiError,
        types::{
            CacheDeletion, CacheDownload, CacheFile, CacheNext, CacheReply, CacheReport,
            CacheVideo, Config, Event, PlaylistRef, Status, UpdateStatus,
        },
    },
    app::AppHandle,
    dashboard::TabView,
    logic::admin::{
        format_quality, format_rate, format_size, local_date_time, local_hm, local_hms,
    },
    theme::{BORDER, MUTED, TEXT, YELLOW},
    widgets::section,
};

const LOADING: &str = "Loading...";
const NOT_RUNNING: &str = "The supervisor is not running, so there is nothing to show.";
const NOTHING_NEXT: &str = "Every video the cache can hold is downloaded.";
const NOTHING_DELETED: &str = "Nothing deleted since the supervisor started.";

const HD_HEIGHT: u32 = 720;

enum Shown {
    Loading,
    NotRunning,
    Report(CacheReport),
}

/// Asks for `/api/cache` only while the tab shows: when it is picked, then at every poll.
pub struct CacheView {
    app: AppHandle,
    showing: bool,
    /// A request is out, so a poll asks nothing more.
    asking: bool,
    shown: Shown,
    /// The configured lists, whose labels prefix the titles when there is more than one.
    playlists: Vec<PlaylistRef>,
}

impl CacheView {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            showing: false,
            asking: false,
            shown: Shown::Loading,
            playlists: Vec::new(),
        }
    }

    fn ask(&mut self, cx: &mut Context<Self>) {
        if self.asking {
            return;
        }
        self.asking = true;
        let client = self.app.client.clone();
        cx.spawn(async move |view, cx| {
            let answer = cx.background_spawn(async move { client.cache() }).await;
            view.update(cx, |view, cx| view.answered(answer, cx)).ok();
        })
        .detach();
    }

    fn answered(&mut self, answer: Result<CacheReply, ApiError>, cx: &mut Context<Self>) {
        self.asking = false;
        self.shown = match answer {
            Ok(CacheReply {
                cache: Some(report),
            }) => Shown::Report(report),
            _ => Shown::NotRunning,
        };
        cx.notify();
    }

    fn load_playlists(&mut self, cx: &mut Context<Self>) {
        let client = self.app.client.clone();
        cx.spawn(async move |view, cx| {
            let Ok(config) = cx.background_spawn(async move { client.config() }).await else {
                return;
            };
            let playlists = Config::from_value(&config).playlists;
            view.update(cx, |view, cx| {
                view.playlists = playlists;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The video's title, after its list's label when more than one list is configured.
    fn title(&self, v: &CacheVideo) -> String {
        let title = v
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .or_else(|| v.id.clone())
            .unwrap_or_default();
        let list = v
            .playlist_id
            .as_deref()
            .filter(|_| self.playlists.len() > 1);
        match list {
            Some(id) => {
                let label = self
                    .playlists
                    .iter()
                    .find(|p| p.id == id)
                    .and_then(|p| p.name.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| id.to_string());
                format!("{label}: {title}")
            }
            None => title,
        }
    }

    fn render_report(&self, report: &CacheReport) -> Div {
        div()
            .child(
                section("Cache")
                    .child(Label::new(head_line(report)).mb(px(4.)))
                    .child(
                        Label::new(next_line(report.next.as_ref()))
                            .mb(px(12.))
                            .text_color(rgb(MUTED)),
                    )
                    .child(self.render_videos(&report.videos)),
            )
            .child(section("Recent deletions").child(render_deletions(&report.deletions)))
    }

    fn render_videos(&self, videos: &[CacheVideo]) -> impl IntoElement {
        let header = row()
            .font_weight(FontWeight::BOLD)
            .child(number_cell().child(Label::new("#")))
            .child(title_cell().child(Label::new("Title")))
            .child(state_cell().child(Label::new("State")))
            .child(size_cell().child(Label::new("Size")))
            .child(quality_cell().child(Label::new("Quality")))
            .child(short_cell().child(Label::new("Format")))
            .child(short_cell().child(Label::new("Client")))
            .child(time_cell().child(Label::new("Downloaded")));
        div()
            .id("cache-table")
            .role(Role::Table)
            .w_full()
            .text_size(px(13.))
            .child(header)
            .children(
                videos
                    .iter()
                    .enumerate()
                    .map(|(row_no, v)| self.render_video(row_no, v)),
            )
    }

    fn render_video(&self, row_no: usize, v: &CacheVideo) -> AnyElement {
        let file = v.file.as_ref();
        let text = |value: Option<&String>| value.cloned().unwrap_or_else(|| "-".into());
        row()
            .id(("cache-row", row_no))
            .role(Role::Row)
            .child(number_cell().child(Label::new((v.index + 1).to_string())))
            .child(title_cell().child(Label::new(self.title(v))))
            .child(state_cell().child(Label::new(state_text(v))))
            .child(size_cell().child(Label::new(v.bytes.map_or("-".into(), format_size))))
            .child(quality_cell().child(match file {
                Some(f) => Label::new(format_quality(f)).text_color(rgb(quality_colour(f))),
                None => Label::new("-"),
            }))
            .child(short_cell().child(Label::new(text(file.and_then(|f| f.format.as_ref())))))
            .child(short_cell().child(Label::new(text(file.and_then(|f| f.client.as_ref())))))
            .child(
                time_cell().child(Label::new(
                    file.and_then(|f| f.finished_at.as_deref())
                        .map_or("-".into(), local_date_time),
                )),
            )
            .into_any_element()
    }
}

fn render_deletions(deletions: &[CacheDeletion]) -> AnyElement {
    if deletions.is_empty() {
        return Label::new(NOTHING_DELETED).into_any_element();
    }
    div()
        .id("deletions-table")
        .role(Role::Table)
        .w_full()
        .text_size(px(13.))
        .children(deletions.iter().enumerate().map(|(row_no, d)| {
            let title = d
                .title
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or(d.id.clone());
            row()
                .id(("deletion-row", row_no))
                .role(Role::Row)
                .child(clock_cell().child(Label::new(local_hms(&d.at))))
                .child(title_cell().child(Label::new(title)))
                .child(size_cell().child(Label::new(format_size(d.bytes))))
                .child(reason_cell().child(Label::new(reason_text(&d.reason))))
        }))
        .into_any_element()
}

fn head_line(report: &CacheReport) -> String {
    format!(
        "Cache: {} of {}",
        format_size(report.cache_bytes),
        format_size(report.budget_bytes)
    )
}

/// What the downloader does next. A wait that names no video has nothing left to fetch.
fn next_line(next: Option<&CacheNext>) -> String {
    let Some(next) = next else {
        return NOTHING_NEXT.into();
    };
    let title = next
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| next.id.clone());
    let Some(title) = title else {
        return NOTHING_NEXT.into();
    };
    let at = next.at.as_deref().map(local_hm);
    let at_or = |with: String, without: String| at.as_ref().map_or(without, |_| with);
    let time = at.clone().unwrap_or_default();
    match next.when.as_str() {
        "now" => format!("Next download: {title}, starting now."),
        "downloading" => format!("Downloading {title}."),
        "size-lookup" => format!("Next: asking YouTube how big {title} is."),
        "after-failure" => at_or(
            format!("Next download: {title}, at {time}, after a failed try."),
            format!("Next download: {title}, after a failed try."),
        ),
        "cookies" => format!("Next download: {title}, once the browser's cookies can be read."),
        "youtube-paused" => at_or(
            format!("Next download: {title}, when YouTube lookups resume at {time}."),
            format!("Next download: {title}, when YouTube lookups resume."),
        ),
        "retry" => at_or(
            format!("Next download: {title}, another try at {time}."),
            format!("Next download: {title}, another try later."),
        ),
        "room" => at_or(
            format!("Next download: {title}, when there is room, about {time}."),
            format!("Next download: {title}, when there is room."),
        ),
        _ => NOTHING_NEXT.into(),
    }
}

fn state_text(v: &CacheVideo) -> String {
    match v.state.as_str() {
        "on-air" => match v.source.as_deref() {
            Some("file") => "On air from the file".into(),
            Some("url") => "On air from YouTube".into(),
            _ => "On air (slate)".into(),
        },
        "cached" => "Cached".into(),
        "downloading" => download_text(&v.download.clone().unwrap_or_default()),
        "queued" => "Waiting to download".into(),
        "size-unknown" if v.looked_up => "Size unknown".into(),
        "size-unknown" => "Size not known yet".into(),
        "failed" => match v.retry_at.as_deref() {
            Some(at) => format!("Failed, tries again at {}", local_hm(at)),
            None => "Failed".into(),
        },
        // The ladder skips a video YouTube will not serve and one that failed to play twice.
        "skipped" => "Skipped after it failed to play".into(),
        "waiting-for-room" => match v.room_at.as_deref() {
            Some(at) => format!("Waiting for room, about {}", local_hm(at)),
            None => "Waiting for room".into(),
        },
        other => other.to_string(),
    }
}

/// `Downloading 45%, 2.3 MB/s, 1 min left`, with what is unknown left out.
fn download_text(d: &CacheDownload) -> String {
    let mut text = format!("Downloading {:.0}%", d.percent);
    if let Some(bps) = d.speed_bps {
        text.push_str(&format!(", {}", format_rate(bps)));
    }
    if let Some(eta) = d.eta_s {
        text.push_str(&format!(", {}", time_left(eta)));
    }
    if d.parts == 2 && d.part == 2 {
        text.push_str(" (audio)");
    }
    text
}

fn time_left(eta_s: u64) -> String {
    if eta_s < 60 {
        return "under a minute left".into();
    }
    let minutes = (eta_s + 30) / 60;
    if minutes < 60 {
        format!("{minutes} min left")
    } else {
        format!("{} h {} min left", minutes / 60, minutes % 60)
    }
}

fn reason_text(reason: &str) -> String {
    match reason {
        "played" => "Played".into(),
        "outside-look-ahead" => "Removed before it aired".into(),
        "not-in-playlist" => "No longer in a playlist".into(),
        "unreadable" => "Could not be read".into(),
        other => other.to_string(),
    }
}

/// Yellow for a file below 720p, which the downloader only takes when nothing better is offered.
fn quality_colour(f: &CacheFile) -> u32 {
    match f.height {
        Some(h) if h < HD_HEIGHT => YELLOW,
        _ => TEXT,
    }
}

/// A table row, with the border under it as the playlist table has.
fn row() -> Div {
    div()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(rgb(BORDER))
}

fn cell() -> Div {
    div().px(px(8.)).py(px(6.))
}

fn number_cell() -> Div {
    cell().flex_none().w(px(40.))
}

fn title_cell() -> Div {
    cell().flex_1().min_w_0()
}

fn state_cell() -> Div {
    cell().flex_none().w(px(210.))
}

fn size_cell() -> Div {
    cell().flex_none().w(px(80.))
}

fn quality_cell() -> Div {
    cell().flex_none().w(px(104.))
}

fn short_cell() -> Div {
    cell().flex_none().w(px(72.))
}

fn time_cell() -> Div {
    cell().flex_none().w(px(128.))
}

fn clock_cell() -> Div {
    cell().flex_none().w(px(80.))
}

fn reason_cell() -> Div {
    cell().flex_none().w(px(200.))
}

impl TabView for CacheView {
    /// The lists are read here too, so a hidden tab asks nothing and a save shows at the next look.
    fn on_tab_shown(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.showing = true;
        self.load_playlists(cx);
        self.ask(cx);
    }

    fn on_tab_hidden(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.showing = false;
    }

    fn on_poll(
        &mut self,
        _status: &Status,
        _events: &[Event],
        _update: Option<&UpdateStatus>,
        _now_ms: i64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.showing {
            self.ask(cx);
        }
    }

    fn on_playlists_saved(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.showing {
            self.load_playlists(cx);
        }
    }
}

impl Render for CacheView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match &self.shown {
            Shown::Loading => section("Cache").child(Label::new(LOADING)),
            Shown::NotRunning => section("Cache").child(Label::new(NOT_RUNNING)),
            Shown::Report(report) => self.render_report(report),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AnyWindowHandle, AppContext, ElementId, TestAppContext};
    use gpui_kit::test::TestWindowExt;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi, types::CacheFile},
        app::POLL_EVERY,
        logic::admin::{local_date_time, local_hm, local_hms},
        test_support::{answer_polls, config_json, count, open_app, painted, shows},
        theme::{TEXT, YELLOW},
    };

    const RETRY_AT: &str = "2026-11-28T10:05:00.000Z";
    const ROOM_AT: &str = "2026-11-28T11:30:00.000Z";
    const FINISHED_AT: &str = "2026-11-28T09:14:02.000Z";
    const DELETED_AT: &str = "2026-11-28T09:00:00.000Z";

    fn file_json(height: u32) -> Value {
        json!({
            "width": height * 16 / 9, "height": height, "fps": 30.0, "vcodec": "h264", "acodec": "aac",
            "kbps": 4000, "format": "137+140", "client": "mweb", "finishedAt": FINISHED_AT
        })
    }

    /// 14.2 of 30 GB, a video in each state from the one on air from its file to one waiting for room.
    fn report_json() -> Value {
        let video = |index: u32, id: &str, title: &str, state: &str, extra: Value| {
            let mut v = json!({
                "index": index, "playlistId": "PLmain", "id": id, "title": title, "duration": 600.0,
                "state": state, "source": null, "bytes": 200_000_000u64, "lookedUp": true,
                "download": null, "retryAt": null, "roomAt": null, "file": null
            });
            for (key, value) in extra.as_object().unwrap() {
                v[key] = value.clone();
            }
            v
        };
        json!({
            "cacheBytes": 15_246_000_000u64, "budgetBytes": 32_212_254_720u64,
            "next": { "when": "now", "at": null, "index": 3, "playlistId": "PLmain", "id": "d", "title": "Delta" },
            "videos": [
                video(0, "a", "Alpha", "on-air", json!({ "source": "file", "file": file_json(1080) })),
                video(1, "b", "Bravo", "cached", json!({ "file": file_json(480) })),
                video(2, "c", "Charlie", "downloading", json!({
                    "download": { "percent": 45.2, "speedBps": 2_411_725.0, "etaS": 70, "part": 2, "parts": 2 }
                })),
                video(3, "d", "Delta", "queued", json!({})),
                video(4, "e", "Echo", "failed", json!({ "retryAt": RETRY_AT })),
                video(5, "f", "Foxtrot", "waiting-for-room", json!({ "roomAt": ROOM_AT }))
            ],
            "deletions": [
                { "at": DELETED_AT, "id": "z", "title": "Zulu", "bytes": 314_572_800u64, "reason": "played" },
                { "at": DELETED_AT, "id": "y", "title": "Yankee", "bytes": 5, "reason": "not-in-playlist" }
            ]
        })
    }

    /// A live dashboard with two playlists configured, whose cache answers `cache`.
    fn scripted(cache: Result<Value, ApiError>) -> Arc<FakeApi> {
        let fake = FakeApi::new();
        answer_polls(&fake);
        let mut config = config_json();
        config["playlists"] = json!([
            { "id": "PLmain", "name": "Main VODs" },
            { "id": "PLhigh", "name": "Highlights" }
        ]);
        fake.reply("GET", "/api/config", Ok(config));
        fake.reply("GET", "/api/cache", cache.map(|c| json!({ "cache": c })));
        fake
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
        let id = id.into();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    fn poll(cx: &mut TestAppContext) {
        cx.executor().advance_clock(POLL_EVERY);
        cx.run_until_parked();
    }

    fn open_cache_tab(cx: &mut TestAppContext, fake: &Arc<FakeApi>) -> AnyWindowHandle {
        let (_, window) = open_app(cx, fake.clone());
        click(cx, window, "tab-cache");
        window
    }

    fn asked(fake: &FakeApi) -> usize {
        count(fake, "GET", "/api/cache")
    }

    #[gpui::test]
    fn the_cache_tab_asks_only_while_it_shows(cx: &mut TestAppContext) {
        let fake = scripted(Ok(report_json()));
        let (_, window) = open_app(cx, fake.clone());

        poll(cx);
        poll(cx);
        let while_monitor_shows = asked(&fake);
        let config_reads = count(&fake, "GET", "/api/config");
        click(cx, window, "tab-cache");
        let on_show = asked(&fake);
        assert_eq!(
            count(&fake, "GET", "/api/config"),
            config_reads + 1,
            "the lists are read when the tab shows"
        );
        poll(cx);
        let after_a_poll = asked(&fake);
        click(cx, window, "tab-monitor");
        poll(cx);
        let after_leaving = asked(&fake);

        assert_eq!(
            (while_monitor_shows, on_show, after_a_poll, after_leaving),
            (0, 1, 2, 2)
        );
    }

    #[gpui::test]
    fn the_cache_tab_shows_the_budget_the_next_download_and_each_row(cx: &mut TestAppContext) {
        let fake = scripted(Ok(report_json()));

        let window = open_cache_tab(cx, &fake);

        let texts = painted(cx, window);
        let want = [
            "Cache: 14.2 GB of 30.0 GB".to_string(),
            "Next download: Delta, starting now.".to_string(),
            "On air from the file".to_string(),
            "Cached".to_string(),
            "Downloading 45%, 2.3 MB/s, 1 min left (audio)".to_string(),
            "Waiting to download".to_string(),
            format!("Failed, tries again at {}", local_hm(RETRY_AT)),
            format!("Waiting for room, about {}", local_hm(ROOM_AT)),
            "Main VODs: Alpha".to_string(),
            "1080p30 h264".to_string(),
            "137+140".to_string(),
            "mweb".to_string(),
            local_date_time(FINISHED_AT),
        ];
        let missing: Vec<_> = want.iter().filter(|w| !texts.contains(w)).collect();
        assert!(missing.is_empty(), "missing {missing:?} in {texts:?}");
    }

    #[gpui::test]
    fn a_file_below_720p_shows_its_quality_as_a_warning(cx: &mut TestAppContext) {
        let fake = scripted(Ok(report_json()));
        let file = |height: Option<u32>| CacheFile {
            height,
            ..CacheFile::default()
        };

        let window = open_cache_tab(cx, &fake);

        assert!(shows(cx, window, "480p30 h264"));
        assert_eq!(
            (
                quality_colour(&file(Some(480))),
                quality_colour(&file(Some(720))),
                quality_colour(&file(None))
            ),
            (YELLOW, TEXT, TEXT)
        );
    }

    #[gpui::test]
    fn the_cache_tab_says_the_supervisor_is_not_running_when_the_report_is_null(
        cx: &mut TestAppContext,
    ) {
        let null = scripted(Ok(Value::Null));
        let unreachable = scripted(Err(ApiError::Unreachable));

        let null_window = open_cache_tab(cx, &null);
        let null_shows = shows(cx, null_window, NOT_RUNNING);
        let unreachable_window = open_cache_tab(cx, &unreachable);
        let unreachable_shows = shows(cx, unreachable_window, NOT_RUNNING);

        assert_eq!((null_shows, unreachable_shows), (true, true));
    }

    #[gpui::test]
    fn the_cache_tab_lists_recent_deletions_with_their_reason(cx: &mut TestAppContext) {
        let fake = scripted(Ok(report_json()));
        let mut none_deleted = report_json();
        none_deleted["deletions"] = json!([]);
        let empty = scripted(Ok(none_deleted));

        let window = open_cache_tab(cx, &fake);
        let texts = painted(cx, window);
        let empty_window = open_cache_tab(cx, &empty);

        for want in [
            local_hms(DELETED_AT).as_str(),
            "Zulu",
            "300.0 MB",
            "Played",
            "Yankee",
            "No longer in a playlist",
        ] {
            assert!(texts.iter().any(|t| t == want), "{want} in {texts:?}");
        }
        assert!(shows(cx, empty_window, NOTHING_DELETED));
    }
}
