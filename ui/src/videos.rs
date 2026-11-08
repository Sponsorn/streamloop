//! The Monitor tab's playlist dropdown and video list (admin.js "Monitor: playlist and video list").

use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, FontWeight, Hsla, IntoElement, Role,
    SharedString, Subscription, Window, div, prelude::*, px, rgb, rgba, transparent_black,
};
use gpui_kit::{
    base::TestSupportExt,
    component::{
        Selectable, Sizable,
        button::{Button, ButtonCustomVariant, ButtonVariants},
        label::Label,
        select::{Select, SelectEvent, SelectItem, SelectState},
    },
};

use crate::{
    api::{
        ApiError,
        types::{Config, Playing, PlaylistRef, Video, VideoPage},
    },
    app::AppHandle,
    logic::{
        admin::{playlist_option_label, video_duration},
        playing_list_index,
    },
    monitor::MonitorView,
    theme::{ACCENT, BG, BORDER, MUTED, TEXT, YELLOW},
    widgets::{Busy, action_button, run_action},
};

const HINT: &str = "This playlist is not playing. Press ▶ on a video to switch to this playlist.";
const LOADING: &str = "Loading playlist...";
const NO_VIDEOS: &str = "This playlist has no videos.";

/// admin.css `.video-table tr.current`: the accent at 18 %.
const CURRENT_ROW: u32 = 0x6c5ce72e;

type Options = Vec<PlaylistOption>;

/// One dropdown entry: the list's index in `config.playlists` and its label.
#[derive(Clone)]
struct PlaylistOption {
    index: usize,
    label: SharedString,
}

impl SelectItem for PlaylistOption {
    type Value = usize;

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

    fn value(&self) -> &usize {
        &self.index
    }
}

/// admin.js's playlist dropdown and video list, with its state variables as fields.
pub struct VideoList {
    app: AppHandle,
    select: Entity<SelectState<Options>>,
    _chosen: Subscription,
    /// `configuredPlaylists`.
    playlists: Vec<PlaylistRef>,
    /// `browsedPlaylist`: the list in the table, an index into `playlists`.
    browsed: usize,
    /// `playingPlaylist` and `playingPlaylistId`: the list on air.
    playing: usize,
    playing_id: Option<String>,
    rows: Vec<Video>,
    /// The page buttons: how many, and the one shown. A failed load keeps them, as admin.js does.
    pages: u32,
    page: u32,
    /// `#video-list-status`; `None` hides it.
    status_line: Option<SharedString>,
    /// `lastHighlightedIndex`: the row marked as on air.
    highlighted: Option<u32>,
}

impl VideoList {
    pub fn new(app: AppHandle, window: &mut Window, cx: &mut Context<MonitorView>) -> Self {
        let select = cx.new(|cx| SelectState::new(Options::new(), None, window, cx));
        let chosen = cx.subscribe_in(
            &select,
            window,
            |monitor, _, event: &SelectEvent<Options>, _, cx| {
                if let SelectEvent::Confirm(Some(index)) = event {
                    monitor.videos.choose(*index, cx);
                }
            },
        );
        Self {
            app,
            select,
            _chosen: chosen,
            playlists: Vec::new(),
            browsed: 0,
            playing: 0,
            playing_id: None,
            rows: Vec::new(),
            pages: 0,
            page: 0,
            // index.html shows it until the first page answers.
            status_line: Some(LOADING.into()),
            highlighted: None,
        }
    }

    /// admin.js `showDashboard()`: the configured lists, then page 1 of the one on air.
    pub fn on_dashboard_open(&mut self, window: &mut Window, cx: &mut Context<MonitorView>) {
        self.load_playlists(window, cx);
    }

    /// admin.js `handleSettingsSave()`: a save may have changed the lists, so they load again.
    pub fn on_playlists_saved(&mut self, window: &mut Window, cx: &mut Context<MonitorView>) {
        self.load_playlists(window, cx);
    }

    /// Every poll, as admin.js `renderNowPlaying`: follow another list on air, then mark the row on air.
    pub fn on_status(&mut self, p: &Playing, window: &mut Window, cx: &mut Context<MonitorView>) {
        if let Some(id) = p.playlist_id.as_deref().filter(|id| !id.is_empty())
            && self.playing_id.as_deref() != Some(id)
        {
            self.playing_list_changed(id, window, cx);
        }
        self.highlight(p.index, cx);
    }

    /// admin.js `loadPlaylistSelector`. A failed load changes nothing.
    fn load_playlists(&mut self, window: &mut Window, cx: &mut Context<MonitorView>) {
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |monitor, cx| {
            let Ok(config) = cx.background_spawn(async move { client.config() }).await else {
                return;
            };
            let playlists = Config::from_value(&config).playlists;
            monitor
                .update_in(cx, |monitor, window, cx| {
                    let list = &mut monitor.videos;
                    list.playlists = playlists;
                    list.playing = playing_list_index(&list.playlists, list.playing_id.as_deref());
                    list.browsed = list.playing;
                    list.show_options(window, cx);
                    list.load_page(1, cx);
                })
                .ok();
        })
        .detach();
    }

    /// admin.js `onPlayingListChanged`: whoever watched the old list follows the new one, whoever browsed stays.
    fn playing_list_changed(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<MonitorView>,
    ) {
        let following = self.browsed == self.playing;
        self.playing_id = Some(id.to_string());
        self.playing = playing_list_index(&self.playlists, Some(id));
        if following && self.browsed != self.playing {
            self.browsed = self.playing;
            self.load_page(1, cx);
        }
        self.show_options(window, cx);
        cx.notify();
    }

    /// admin.js `updateVideoListHighlight`. The index on air means nothing in a browsed list.
    fn highlight(&mut self, index: Option<u32>, cx: &mut Context<MonitorView>) {
        if self.browsed != self.playing || index == self.highlighted {
            return;
        }
        self.highlighted = index;
        cx.notify();
    }

    /// The dropdown's `change`: browse the chosen list. Nothing changes on air until a ▶.
    fn choose(&mut self, index: usize, cx: &mut Context<MonitorView>) {
        if index == self.browsed {
            return;
        }
        self.browsed = index;
        self.load_page(1, cx);
    }

    /// admin.js `renderPlaylistOptions`: "(playing)" after the list on air, and the browsed list chosen.
    fn show_options(&self, window: &mut Window, cx: &mut Context<MonitorView>) {
        let options: Options = self
            .playlists
            .iter()
            .enumerate()
            .map(|(index, p)| PlaylistOption {
                index,
                label: playlist_option_label(p, index, self.playing).into(),
            })
            .collect();
        let browsed = self.browsed;
        self.select.update(cx, |select, cx| {
            select.set_items(options, window, cx);
            select.set_selected_value(&browsed, window, cx);
        });
    }

    /// admin.js `loadVideoList`. The table stays until the answer; one for a list no longer browsed is dropped.
    fn load_page(&mut self, page: u32, cx: &mut Context<MonitorView>) {
        let which = self.browsed;
        self.status_line = Some(LOADING.into());
        cx.notify();
        let client = self.app.client.clone();
        cx.spawn(async move |monitor, cx| {
            let answer = cx
                .background_spawn(async move { client.videos(which, page) })
                .await;
            monitor
                .update(cx, |monitor, cx| {
                    monitor.videos.page_loaded(which, answer, cx)
                })
                .ok();
        })
        .detach();
    }

    fn page_loaded(
        &mut self,
        which: usize,
        answer: Result<VideoPage, ApiError>,
        cx: &mut Context<MonitorView>,
    ) {
        if which != self.browsed {
            return;
        }
        match answer {
            Ok(page) => self.show_page(page),
            Err(err) => {
                self.rows.clear();
                self.status_line = Some(format!("Could not load the playlist: {err}").into());
            }
        }
        cx.notify();
    }

    /// admin.js `renderVideoList`.
    fn show_page(&mut self, data: VideoPage) {
        self.status_line = data.videos.is_empty().then(|| NO_VIDEOS.into());
        self.highlighted = data.current_index;
        self.pages = match data.per_page {
            0 => 0,
            per_page => data.total.div_ceil(per_page as usize) as u32,
        };
        self.page = data.page;
        self.rows = data.videos;
    }

    /// index.html's `.form-group` with the Playlist label and the dropdown.
    pub fn render_dropdown(&self) -> Div {
        div()
            .mb(px(16.))
            .child(
                Label::new("Playlist")
                    .mb(px(4.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(MUTED)),
            )
            .child(
                div().w_full().max_w(px(280.)).child(
                    Select::new(&self.select)
                        .id("playlist-select")
                        .large()
                        // With no lists the box stays empty, as the browser's <select> does.
                        .placeholder("")
                        .accessibility_label("Playlist")
                        .h(px(40.))
                        .px(px(12.))
                        .bg(rgb(BG)),
                ),
            )
    }

    /// Below the transport buttons: the browsing hint, the table, the page buttons and the status line.
    pub fn render_list(&self, busy: &Busy, cx: &mut Context<MonitorView>) -> Div {
        div()
            .when(self.browsed != self.playing, |d| {
                d.child(Label::new(HINT).my(px(8.)).text_color(rgb(YELLOW)))
            })
            .child(self.render_table(busy, cx))
            .when(self.pages > 1, |d| d.child(self.render_pages(cx)))
            .children(self.status_line.clone().map(|text| {
                div()
                    .p(px(16.))
                    .flex()
                    .justify_center()
                    .text_center()
                    .child(Label::new(text))
            }))
    }

    /// admin.css `.video-table`: #, Title, Duration and ▶, with the row on air highlighted.
    fn render_table(&self, busy: &Busy, cx: &mut Context<MonitorView>) -> impl IntoElement {
        let header = row()
            .font_weight(FontWeight::BOLD)
            .child(number_cell().child(Label::new("#")))
            .child(title_cell().child(Label::new("Title")))
            .child(duration_cell().child(Label::new("Duration")))
            .child(play_cell());
        div()
            .id("video-table")
            .role(Role::Table)
            .w_full()
            .text_size(px(13.))
            .child(header)
            .children(
                self.rows
                    .iter()
                    .map(|video| self.render_row(video, busy, cx)),
            )
    }

    fn render_row(&self, video: &Video, busy: &Busy, cx: &mut Context<MonitorView>) -> AnyElement {
        let index = video.index;
        let current = self.highlighted == Some(index);
        let title = video
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Untitled".into());
        // admin.js sends no id when the video has none.
        let id = video.id.clone().filter(|id| !id.is_empty());
        let play = action_button(("play", index), "▶", busy)
            .small()
            .h(px(26.))
            .px(px(10.))
            .accessibility_label(format!("Play video {}", index + 1))
            .on_click(cx.listener(move |monitor, _, window, cx| {
                play(monitor, index, id.clone(), window, cx)
            }));
        row()
            .id(("video", index))
            .role(Role::Row)
            .aria_selected(current)
            .when(current, |r| r.bg(rgba(CURRENT_ROW)))
            .child(number_cell().child(Label::new((index + 1).to_string())))
            .child(title_cell().child(Label::new(title)))
            .child(duration_cell().child(Label::new(video_duration(video.duration))))
            .child(play_cell().child(play))
            .test_support()
            .into_any_element()
    }

    /// admin.css `.video-pagination`: one button per page, the page shown marked.
    fn render_pages(&self, cx: &mut Context<MonitorView>) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .justify_center()
            .gap(px(4.))
            .mt(px(8.))
            .children((1..=self.pages).map(|page| {
                let current = page == self.page;
                // Button has no aria-current; the name says what a browser announces for `aria-current="page"`.
                let name = match current {
                    true => format!("{page}, current page"),
                    false => page.to_string(),
                };
                Button::new(("page", page))
                    .label(page.to_string())
                    .accessibility_label(name)
                    .small()
                    .custom(page_colors(current, cx))
                    .selected(current)
                    .h(px(21.))
                    .px(px(8.))
                    .border_1()
                    .border_color(rgb(if current { ACCENT } else { BORDER }))
                    .on_click(
                        cx.listener(move |monitor, _, _, cx| monitor.videos.load_page(page, cx)),
                    )
            }))
    }
}

/// admin.js's ▶: a jump within the list on air, or a switch to the browsed list.
fn play(
    monitor: &mut MonitorView,
    index: u32,
    id: Option<String>,
    window: &mut Window,
    cx: &mut Context<MonitorView>,
) {
    let list = &monitor.videos;
    let (browsed, on_air, app) = (list.browsed, list.browsed == list.playing, list.app.clone());
    run_action(
        monitor,
        ("play", index),
        &app,
        move |c| {
            if on_air {
                c.jump(index, id.as_deref())
            } else {
                c.switch_playlist(browsed, index, id.as_deref())
            }
        },
        |_, _, _, _| {},
        window,
        cx,
    );
}

/// A table row, with admin.css's border under it.
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

fn duration_cell() -> Div {
    cell().flex_none().w(px(72.))
}

fn play_cell() -> Div {
    cell().flex_none().w(px(52.))
}

/// admin.css `.page-btn` and `.page-btn.current`; the border is set on the button.
fn page_colors(current: bool, cx: &App) -> ButtonCustomVariant {
    let hsla = |color: u32| -> Hsla { rgb(color).into() };
    ButtonCustomVariant::new(cx)
        .color(transparent_black())
        .foreground(hsla(if current { TEXT } else { MUTED }))
        .hover(transparent_black())
        .active(transparent_black())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AnyWindowHandle, AppContext, ElementId, ScrollDelta, TestAppContext, point, px};
    use gpui_kit::test::TestWindowExt;
    use serde_json::{Value, json};

    use crate::{
        api::{ApiError, fake::FakeApi},
        app::POLL_EVERY,
        test_support::{
            config_json, count, events_json, open_app, painted, shows, status_json, update_json,
        },
    };

    const HINT: &str =
        "This playlist is not playing. Press ▶ on a video to switch to this playlist.";
    const TWO: &[(&str, &str)] = &[("PLmain", "Main VODs"), ("PLhigh", "Highlights")];
    const THREE: &[(&str, &str)] = &[
        ("PLmain", "Main VODs"),
        ("PLhigh", "Highlights"),
        ("PLclips", "Clips"),
    ];

    fn page_path(list: usize, page: u32) -> String {
        format!("/api/playlist/videos?playlist={list}&page={page}&perPage=25")
    }

    /// One page of 25 from list `list`: video `i` is "List {list} video {i + 1}", id `L{list}-{i}`.
    fn page_json(list: usize, page: u32, total: usize, current: Option<u32>) -> Value {
        let first = (page as usize - 1) * 25;
        let videos: Vec<Value> = (first..total.min(first + 25))
            .map(|i| {
                json!({
                    "index": i, "id": format!("L{list}-{i}"),
                    "title": format!("List {list} video {}", i + 1), "duration": 60 * (i + 1)
                })
            })
            .collect();
        json!({
            "videos": videos, "total": total, "page": page, "perPage": 25,
            "currentIndex": current, "playing": current.is_some()
        })
    }

    /// Video 8 with the id "abc" and video 9 with an empty id.
    fn two_videos() -> Value {
        json!({
            "videos": [
                { "index": 7, "id": "abc", "title": "Eighth", "duration": 90 },
                { "index": 8, "id": "", "title": "Ninth", "duration": 90 }
            ],
            "total": 2, "page": 1, "perPage": 25, "currentIndex": null, "playing": false
        })
    }

    /// The live status with video `index` of list `list_id` on air.
    fn status_on(list_id: &str, index: u32) -> Value {
        let mut status = status_json("live", false);
        status["supervisor"]["playing"]["playlistId"] = json!(list_id);
        status["supervisor"]["playing"]["index"] = json!(index);
        status
    }

    /// A live dashboard with `lists` configured and the first on air at video 3. List 0 has 30
    /// videos, the others 3 each; jump and switch answer ok.
    fn scripted(lists: &[(&str, &str)]) -> Arc<FakeApi> {
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        fake.reply("GET", "/api/events", Ok(events_json(0)));
        fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
        let mut config = config_json();
        config["playlists"] = lists
            .iter()
            .map(|(id, name)| json!({ "id": id, "name": name }))
            .collect();
        fake.reply("GET", "/api/config", Ok(config));
        fake.reply("GET", &page_path(0, 1), Ok(page_json(0, 1, 30, Some(2))));
        for list in 1..lists.len() {
            fake.reply("GET", &page_path(list, 1), Ok(page_json(list, 1, 3, None)));
        }
        fake.reply("POST", "/api/player/jump", Ok(json!({ "ok": true })));
        fake.reply("POST", "/api/playlist/switch", Ok(json!({ "ok": true })));
        fake
    }

    fn open(cx: &mut TestAppContext, fake: &Arc<FakeApi>) -> AnyWindowHandle {
        open_app(cx, fake.clone()).1
    }

    /// Opens the dropdown, moves one option with `key` ("down" or "up") and confirms with Enter.
    fn choose(cx: &mut TestAppContext, window: AnyWindowHandle, key: &str) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.within("playlist-select").click("input", cx);
            window.press(key, cx);
            window.press("enter", cx);
        })
        .unwrap();
    }

    fn open_menu(cx: &mut TestAppContext, window: AnyWindowHandle) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.within("playlist-select").click("input", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    fn chosen(cx: &mut TestAppContext, window: AnyWindowHandle) -> Option<String> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find("playlist-select").value().map(str::to_string)
        })
        .unwrap()
    }

    /// `Some(selected)` for an element on screen, `None` when there is none.
    fn selected(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        id: impl Into<ElementId>,
    ) -> Option<bool> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).map(|e| e.selected().unwrap_or(false))
        })
        .unwrap()
    }

    fn label(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        id: impl Into<ElementId>,
    ) -> Option<String> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id)?.label().map(str::to_string)
        })
        .unwrap()
    }

    fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    /// Scrolls the dashboard with the pointer over `over`, which must be on screen.
    fn scroll_dashboard(
        cx: &mut TestAppContext,
        window: AnyWindowHandle,
        over: impl Into<ElementId>,
        dy: f32,
    ) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.scroll(over, ScrollDelta::Pixels(point(px(0.), px(dy))), cx);
        })
        .unwrap();
    }

    /// Scrolls the dashboard to its end first: a page of 25 rows pushes the page buttons out of the window.
    fn click_below(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
        scroll_dashboard(cx, window, "btn-next", -5000.);
        click(cx, window, id);
    }

    fn scroll_to_top(cx: &mut TestAppContext, window: AnyWindowHandle) {
        scroll_dashboard(cx, window, ("page", 1u32), 5000.);
    }

    fn posts(fake: &FakeApi) -> Vec<(String, Option<Value>)> {
        fake.posts().into_iter().map(|r| (r.path, r.body)).collect()
    }

    fn poll_with(cx: &mut TestAppContext, fake: &FakeApi, status: Value) {
        fake.reply("GET", "/api/status", Ok(status));
        cx.executor().advance_clock(POLL_EVERY);
    }

    #[gpui::test]
    fn dropdown_lists_names_and_marks_the_one_playing(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        let window = open(cx, &fake);

        assert_eq!(chosen(cx, window).as_deref(), Some("Main VODs (playing)"));
        open_menu(cx, window);

        let options: Vec<String> = painted(cx, window)
            .into_iter()
            .filter(|t| t.starts_with("Main VODs") || t.starts_with("Highlights"))
            .collect();
        // The closed dropdown's text, then the open menu's two options.
        assert_eq!(
            options,
            ["Main VODs (playing)", "Main VODs (playing)", "Highlights"]
        );
    }

    #[gpui::test]
    fn choosing_a_list_loads_its_first_page_and_shows_the_hint(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        let window = open(cx, &fake);
        assert!(shows(cx, window, "List 0 video 1"));
        assert!(!shows(cx, window, HINT));

        choose(cx, window, "down");
        cx.run_until_parked();

        assert_eq!(count(&fake, "GET", &page_path(1, 1)), 1);
        assert_eq!(chosen(cx, window).as_deref(), Some("Highlights"));
        assert!(shows(cx, window, HINT));
        assert!(shows(cx, window, "List 1 video 1"));
        assert!(!shows(cx, window, "List 0 video 1"));

        choose(cx, window, "up");
        cx.run_until_parked();

        assert_eq!(count(&fake, "GET", &page_path(0, 1)), 2);
        assert!(!shows(cx, window, HINT));
        assert!(shows(cx, window, "List 0 video 1"));
    }

    #[gpui::test]
    fn a_late_answer_for_another_list_is_dropped(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        // A short list 0 keeps the dozen rounds below quick.
        fake.reply("GET", &page_path(0, 1), Ok(page_json(0, 1, 3, Some(2))));
        let window = open(cx, &fake);

        // The two answers come back in either order; the repeats cover list 1's coming last.
        for _ in 0..12 {
            choose(cx, window, "down");
            choose(cx, window, "up");
            cx.run_until_parked();

            let texts = painted(cx, window);
            assert!(texts.iter().any(|t| t == "List 0 video 1"), "{texts:?}");
            assert!(!texts.iter().any(|t| t == "List 1 video 1" || t == HINT));
        }
        assert_eq!(count(&fake, "GET", &page_path(1, 1)), 12);
    }

    #[gpui::test]
    fn play_in_the_list_on_air_jumps(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        fake.reply("GET", &page_path(0, 1), Ok(two_videos()));
        let window = open(cx, &fake);
        assert_eq!(
            label(cx, window, ("play", 7u32)).as_deref(),
            Some("Play video 8")
        );

        click(cx, window, ("play", 7u32));
        click(cx, window, ("play", 8u32));

        assert_eq!(
            posts(&fake),
            [
                (
                    "/api/player/jump".to_string(),
                    Some(json!({ "index": 7, "id": "abc" }))
                ),
                ("/api/player/jump".to_string(), Some(json!({ "index": 8 }))),
            ]
        );
    }

    #[gpui::test]
    fn play_in_another_list_switches(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        fake.reply("GET", &page_path(1, 1), Ok(two_videos()));
        let window = open(cx, &fake);
        choose(cx, window, "down");
        cx.run_until_parked();

        click(cx, window, ("play", 7u32));

        assert_eq!(
            posts(&fake),
            [(
                "/api/playlist/switch".to_string(),
                Some(json!({ "playlistIndex": 1, "videoIndex": 7, "videoId": "abc" }))
            )]
        );
        // The dropdown catches up once the status names the list.
        poll_with(cx, &fake, status_on("PLhigh", 7));
        assert_eq!(chosen(cx, window).as_deref(), Some("Highlights (playing)"));
        assert!(!shows(cx, window, HINT));
    }

    /// The page buttons' names, as a screen reader reads them.
    fn page_names(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<String> {
        (1..=20u32)
            .map_while(|page| label(cx, window, ("page", page)))
            .collect()
    }

    #[gpui::test]
    fn pages_of_25_with_the_current_page_marked(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        fake.reply("GET", &page_path(0, 1), Ok(page_json(0, 1, 120, Some(2))));
        fake.reply("GET", &page_path(0, 3), Ok(page_json(0, 3, 120, Some(2))));
        let window = open(cx, &fake);

        assert_eq!(
            page_names(cx, window),
            ["1, current page", "2", "3", "4", "5"]
        );

        click_below(cx, window, ("page", 3u32));

        assert_eq!(count(&fake, "GET", &page_path(0, 3)), 1);
        assert_eq!(
            page_names(cx, window),
            ["1", "2", "3, current page", "4", "5"]
        );
        scroll_to_top(cx, window);
        assert!(shows(cx, window, "List 0 video 51"));
        assert!(!shows(cx, window, "List 0 video 1"));

        // One page needs no buttons.
        choose(cx, window, "down");
        cx.run_until_parked();
        assert!(page_names(cx, window).is_empty());
    }

    #[gpui::test]
    fn follows_the_list_on_air_unless_browsing(cx: &mut TestAppContext) {
        let fake = scripted(THREE);
        let window = open(cx, &fake);

        // Watching the list on air: the next list on air takes over the table.
        poll_with(cx, &fake, status_on("PLhigh", 1));
        assert_eq!(count(&fake, "GET", &page_path(1, 1)), 1);
        assert_eq!(chosen(cx, window).as_deref(), Some("Highlights (playing)"));
        assert!(shows(cx, window, "List 1 video 1"));
        assert!(!shows(cx, window, HINT));

        poll_with(cx, &fake, status_on("PLmain", 2));
        assert_eq!(chosen(cx, window).as_deref(), Some("Main VODs (playing)"));
        assert!(shows(cx, window, "List 0 video 1"));

        // Browsing list 1 while list 0 plays: list 0 moving on, even to list 2, leaves the table.
        choose(cx, window, "down");
        cx.run_until_parked();
        let loads = fake.requests().len();
        poll_with(cx, &fake, status_on("PLmain", 5));
        poll_with(cx, &fake, status_on("PLclips", 0));

        let lists_asked: Vec<String> = fake.requests()[loads..]
            .iter()
            .filter(|r| r.path.starts_with("/api/playlist/videos"))
            .map(|r| r.path.clone())
            .collect();
        assert!(lists_asked.is_empty(), "{lists_asked:?}");
        assert_eq!(chosen(cx, window).as_deref(), Some("Highlights"));
        assert!(shows(cx, window, "List 1 video 1"));
        assert!(shows(cx, window, HINT));
        open_menu(cx, window);
        let texts = painted(cx, window);
        assert!(texts.iter().any(|t| t == "Clips (playing)"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Main VODs"), "{texts:?}");
    }

    #[gpui::test]
    fn highlight_follows_the_playing_index(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        let window = open(cx, &fake);
        assert_eq!(selected(cx, window, ("video", 2u32)), Some(true));
        assert_eq!(selected(cx, window, ("video", 4u32)), Some(false));

        poll_with(cx, &fake, status_on("PLmain", 4));
        assert_eq!(selected(cx, window, ("video", 4u32)), Some(true));
        assert_eq!(selected(cx, window, ("video", 2u32)), Some(false));

        // Nothing on air: no row is marked.
        poll_with(cx, &fake, status_json("stopped", false));
        assert_eq!(selected(cx, window, ("video", 4u32)), Some(false));

        // In a browsed list the index on air means nothing.
        choose(cx, window, "down");
        cx.run_until_parked();
        poll_with(cx, &fake, status_on("PLmain", 0));
        assert_eq!(selected(cx, window, ("video", 0u32)), Some(false));
    }

    #[gpui::test]
    fn says_the_playlist_has_no_videos(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        let empty =
            json!({ "videos": [], "total": 0, "page": 1, "perPage": 25, "currentIndex": null });
        fake.reply("GET", &page_path(0, 1), Ok(empty));

        let window = open(cx, &fake);

        assert!(shows(cx, window, "This playlist has no videos."));
        assert!(!shows(cx, window, "Loading playlist..."));
    }

    #[gpui::test]
    fn shows_why_the_list_failed(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        fake.reply(
            "GET",
            &page_path(0, 2),
            Err(ApiError::Server("Invalid page".into())),
        );
        let window = open(cx, &fake);
        assert!(shows(cx, window, "List 0 video 1"));

        click_below(cx, window, ("page", 2u32));

        assert!(shows(
            cx,
            window,
            "Could not load the playlist: Invalid page"
        ));
        assert!(!shows(cx, window, "List 0 video 1"));
    }

    #[gpui::test]
    fn says_loading_until_the_playlists_answer_and_loads_again_after_a_save(
        cx: &mut TestAppContext,
    ) {
        let fake = scripted(TWO);
        fake.reply_once("GET", "/api/config", Err(ApiError::Unreachable));
        let (app, window) = open_app(cx, fake.clone());
        assert!(shows(cx, window, "Loading playlist..."));
        assert_eq!(count(&fake, "GET", &page_path(0, 1)), 0);

        // A Settings save that may have changed the playlists loads the dropdown again.
        let handle = app.read_with(cx, |app, _| app.handle());
        cx.update(|cx| handle.playlists_saved(cx));
        cx.run_until_parked();

        assert!(!shows(cx, window, "Loading playlist..."));
        assert!(shows(cx, window, "List 0 video 1"));
        assert_eq!(chosen(cx, window).as_deref(), Some("Main VODs (playing)"));
    }

    #[gpui::test]
    fn rows_show_number_title_and_duration(cx: &mut TestAppContext) {
        let fake = scripted(TWO);
        let page = json!({
            "videos": [
                { "index": 0, "id": "a", "title": "First", "duration": 125 },
                { "index": 1, "id": "b" }
            ],
            "total": 2, "page": 1, "perPage": 25, "currentIndex": 0
        });
        fake.reply("GET", &page_path(0, 1), Ok(page));
        let window = open(cx, &fake);

        let texts = painted(cx, window);
        let start = texts
            .iter()
            .position(|t| t == "#")
            .expect("the table header");
        assert_eq!(
            texts[start..start + 9],
            [
                "#", "Title", "Duration", "1", "First", "02:05", "2", "Untitled", "-"
            ]
        );
    }
}
