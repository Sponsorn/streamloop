//! The Monitor tab's Logs section (admin.js "Monitor: logs"): the file dropdown, Refresh and the viewer.

use std::{ops::Range, time::Duration};

use gpui::{
    AnyElement, App, AppContext, AsyncWindowContext, Context, Entity, IntoElement, Render,
    SharedString, Subscription, WeakEntity, Window, div, point, prelude::*, px, relative, rgb,
};
use gpui_kit::component::{
    Sizable,
    button::Button,
    input::{Textarea, TextareaState},
    label::Label,
    select::{Select, SelectEvent, SelectItem, SelectState},
};

use crate::{
    api::{ApiError, types::LogFile},
    app::AppHandle,
    logic::admin::{format_bytes, local_date_time},
    theme::{BG, MONO_FONT, SURFACE},
    widgets::{btn, fit_menu, reveal_on_focus},
};

const SELECT_A_FILE: &str = "Select a log file to view.";
const LOADING: &str = "Loading...";
const EMPTY: &str = "(empty)";
const NO_FILES_OPTION: &str = "(no log files yet)";
const NO_FILES: &str = "No log files yet.";
const LIST_FAILED_OPTION: &str = "(could not load)";

/// Further down than any log reaches; the viewer clamps it to its last line.
const END: f32 = -1.0e9;

/// Bytes put in the viewer at a time. Wrapping 1.1 MB in one go held the window 0.4 s on the dev
/// machine, 1.3 s with emoji and CJK on every line; a piece holds it for tens of ms.
const PIECE: usize = 32 * 1024;
/// The pause between pieces, in which the window draws and takes input.
const PIECE_GAP: Duration = Duration::from_millis(1);

/// A file as admin.js names it in the option's value: its source and name.
#[derive(Clone, Debug, PartialEq)]
struct LogKey {
    source: String,
    name: String,
}

impl LogKey {
    fn of(file: &LogFile) -> Self {
        LogKey {
            source: file.source.clone(),
            name: file.name.clone(),
        }
    }
}

/// One dropdown entry. A placeholder such as "(no log files yet)" has no file, as admin.js's empty value.
#[derive(Clone)]
struct LogOption {
    key: Option<LogKey>,
    label: SharedString,
}

impl LogOption {
    fn placeholder(label: &'static str) -> Self {
        LogOption {
            key: None,
            label: label.into(),
        }
    }

    /// admin.js's option text, with the time as `YYYY-MM-DD HH:MM` local.
    fn file(file: &LogFile) -> Self {
        LogOption {
            key: Some(LogKey::of(file)),
            label: format!(
                "{}: {} ({}, {})",
                file.source,
                file.name,
                format_bytes(file.size),
                local_date_time(&file.mtime)
            )
            .into(),
        }
    }
}

impl SelectItem for LogOption {
    type Value = Option<LogKey>;

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

    fn value(&self) -> &Option<LogKey> {
        &self.key
    }
}

type Options = Vec<LogOption>;

/// admin.js's log picker and `#log-view`. The viewer is a read-only text area, so its text can be
/// selected and copied.
pub struct LogViewer {
    app: AppHandle,
    select: Entity<SelectState<Options>>,
    /// The dropdown's entries, which size its open list.
    labels: Vec<SharedString>,
    _chosen: Subscription,
    view: Entity<TextareaState>,
    /// The option's value: the file in the dropdown, `None` for a placeholder or before the list.
    chosen: Option<LogKey>,
    /// Counts the texts put in the viewer; a file's answer is shown only if nothing came after it.
    shown: u64,
}

impl LogViewer {
    pub fn new(app: AppHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let select = cx.new(|cx| SelectState::new(Options::new(), None, window, cx));
        let chosen = cx.subscribe_in(
            &select,
            window,
            |logs, _, event: &SelectEvent<Options>, window, cx| {
                if let SelectEvent::Confirm(Some(key)) = event {
                    logs.choose(key.clone(), window, cx);
                }
            },
        );
        let view = cx.new(|cx| TextareaState::new(window, cx).default_value(SELECT_A_FILE));
        Self {
            app,
            select,
            labels: Vec::new(),
            _chosen: chosen,
            view,
            chosen: None,
            shown: 0,
        }
    }

    /// admin.js `loadLogList`: on dashboard open and on Refresh.
    pub fn load_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |logs, cx| {
            let answer = cx.background_spawn(async move { client.logs() }).await;
            logs.update_in(cx, |logs, window, cx| logs.list_loaded(answer, window, cx))
                .ok();
        })
        .detach();
    }

    /// The chosen file stays when it is still listed, else the newest, which the server lists first.
    fn list_loaded(
        &mut self,
        answer: Result<Vec<LogFile>, ApiError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match answer {
            Ok(files) if files.is_empty() => {
                let options = vec![LogOption::placeholder(NO_FILES_OPTION)];
                self.show_options(options, None, window, cx);
                self.show(NO_FILES, window, cx);
            }
            Ok(files) => {
                let listed = |key: &LogKey| files.iter().any(|f| LogKey::of(f) == *key);
                let key = match self.chosen.take() {
                    Some(key) if listed(&key) => key,
                    _ => LogKey::of(&files[0]),
                };
                let options = files.iter().map(LogOption::file).collect();
                self.show_options(options, Some(key.clone()), window, cx);
                self.open(key, window, cx);
            }
            Err(err) => {
                let options = vec![LogOption::placeholder(LIST_FAILED_OPTION)];
                self.show_options(options, None, window, cx);
                self.show(format!("Could not list the log files: {err}"), window, cx);
            }
        }
    }

    /// The dropdown's `change`: only a different file opens.
    fn choose(&mut self, key: Option<LogKey>, window: &mut Window, cx: &mut Context<Self>) {
        if key == self.chosen {
            return;
        }
        self.chosen = key.clone();
        match key {
            Some(key) => self.open(key, window, cx),
            None => {
                self.show(SELECT_A_FILE, window, cx);
            }
        }
    }

    fn show_options(
        &mut self,
        options: Options,
        chosen: Option<LogKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chosen = chosen.clone();
        self.labels = options.iter().map(|option| option.label.clone()).collect();
        self.select.update(cx, |select, cx| {
            select.set_items(options, window, cx);
            select.set_selected_value(&chosen, window, cx);
        });
    }

    /// admin.js `viewLog`.
    fn open(&mut self, key: LogKey, window: &mut Window, cx: &mut Context<Self>) {
        let asked = self.show(LOADING, window, cx);
        let client = self.app.client.clone();
        cx.spawn_in(window, async move |logs, cx| {
            let answer = cx
                .background_spawn(async move { client.log(&key.source, &key.name) })
                .await;
            let text = logs
                .update_in(cx, |logs, window, cx| {
                    if logs.shown != asked {
                        return None;
                    }
                    match answer {
                        Ok(text) if text.is_empty() => {
                            logs.show(EMPTY, window, cx);
                            None
                        }
                        Ok(text) => Some(text),
                        Err(err) => {
                            logs.show(format!("Could not load the log: {err}"), window, cx);
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            if let Some(text) = text {
                fill(logs, asked, text, cx).await;
            }
        })
        .detach();
    }

    /// Puts `text` in the viewer and returns its count.
    fn show(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.shown += 1;
        let text = text.into();
        self.view
            .update(cx, |view, cx| view.set_value(text, window, cx));
        self.shown
    }
}

/// Puts a file in the viewer a piece at a time, end first, so the window keeps drawing while a large
/// log wraps. View and caret stay at the end; any text shown after `asked` stops the filling.
async fn fill(logs: WeakEntity<LogViewer>, asked: u64, text: String, cx: &mut AsyncWindowContext) {
    let mut pieces = pieces(&text, PIECE);
    let mut first = true;
    while let Some(range) = pieces.pop() {
        if !first {
            cx.background_executor().timer(PIECE_GAP).await;
        }
        let piece = &text[range];
        let last = pieces.is_empty();
        let went_in = logs.update_in(cx, |logs, window, cx| {
            if logs.shown != asked {
                return false;
            }
            logs.view.update(cx, |view, cx| {
                if first {
                    view.set_value(piece, window, cx);
                } else {
                    view.set_selected_range(0..0, cx);
                    view.insert(piece, window, cx);
                }
                if last {
                    let end = view.text().len();
                    view.set_selected_range(end..end, cx);
                }
                view.set_scroll_offset(point(px(0.), px(END)), cx);
            });
            true
        });
        if !went_in.unwrap_or(false) {
            return;
        }
        first = false;
    }
}

/// `text` cut after a newline into pieces of at least `size` bytes, the last one ending where the
/// text ends. A cut after `\n` never falls inside a character.
fn pieces(text: &str, size: usize) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let end = bytes
            .get(start + size..)
            .and_then(|rest| rest.iter().position(|&b| b == b'\n'))
            .map_or(bytes.len(), |at| start + size + at + 1);
        pieces.push(start..end);
        start = end;
    }
    pieces
}

impl Render for LogViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .child(
                // admin.css `.log-picker`; the button is as tall as the box, as in the browser's flex row.
                div()
                    .flex()
                    .gap(px(8.))
                    .mb(px(8.))
                    .child(
                        div().flex_1().min_w_0().child(reveal_on_focus(fit_menu(
                            "log-menu",
                            Select::new(&self.select)
                                .id("log-select")
                                .large()
                                // Before the list answers the box stays empty, as index.html's <select>.
                                .placeholder("")
                                .accessibility_label("Log file")
                                .h(px(40.))
                                .px(px(12.))
                                .bg(rgb(BG)),
                            self.labels.iter().cloned(),
                        ))),
                    )
                    .child(reveal_on_focus(
                        btn(Button::new("log-refresh").label("Refresh"))
                            .h(px(40.))
                            .on_click(
                                cx.listener(|logs, _, window, cx| logs.load_list(window, cx)),
                            ),
                    )),
            )
            .child(
                // admin.css `.log-view`: 320 px of Consolas 11 px on the surface, lines wrapped.
                div().id("log-view").child(reveal_on_focus(
                    Textarea::new(&self.view)
                        .readonly(true)
                        .aria_label("Logs")
                        .h(px(320.))
                        .bg(rgb(SURFACE))
                        .font_family(MONO_FONT)
                        .text_size(px(11.))
                        // admin.css's body line height, which the <pre> inherits.
                        .line_height(relative(1.6)),
                )),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use chrono::{Local, SecondsFormat, TimeZone, Utc};
    use gpui::{AnyWindowHandle, AppContext, Entity, ScrollDelta, TestAppContext, point, px};
    use gpui_kit::{component::input::TextareaState, test::TestWindowExt};
    use serde_json::{Value, json};

    use crate::{
        api::{ApiError, fake::FakeApi},
        app::AppView,
        dashboard::Tab,
        monitor::MonitorView,
        test_support::{count, events_json, open_app, painted, status_json, update_json},
    };

    const NEWEST: &str = "streamloop-2026-11-08.log";
    const NEWEST_LABEL: &str = "node: streamloop-2026-11-08.log (2.0 KB, 2026-11-08 14:30)";
    const OLDER: &str = "ytdlp-2026-11-07.log";
    const OLDER_LABEL: &str = "supervisor: ytdlp-2026-11-07.log (3.3 MB, 2026-11-07 09:05)";
    const TRUNCATED: &str = "... (truncated, showing last 1 MB) ...\n";

    /// `mtime` as Node sends it, for a local time, so the label reads that time on any machine.
    fn mtime(day: u32, hour: u32, minute: u32) -> String {
        Local
            .with_ymd_and_hms(2026, 11, day, hour, minute, 0)
            .unwrap()
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    fn newest() -> Value {
        json!({ "name": NEWEST, "size": 2048, "mtime": mtime(8, 14, 30), "source": "node" })
    }

    fn older() -> Value {
        json!({ "name": OLDER, "size": 3_500_000, "mtime": mtime(7, 9, 5), "source": "supervisor" })
    }

    fn path(source: &str, name: &str) -> String {
        format!("/api/logs/{source}/{name}")
    }

    fn lines(n: usize, word: &str) -> String {
        (1..=n).map(|i| format!("{word} line {i}\n")).collect()
    }

    /// A live dashboard whose `/api/logs` answers `files`; the newest file holds 3 lines, the older 3 others.
    fn scripted(files: Value) -> Arc<FakeApi> {
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        fake.reply("GET", "/api/events", Ok(events_json(0)));
        fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
        fake.reply("GET", "/api/logs", Ok(files));
        fake.text(&path("node", NEWEST), Ok(lines(3, "newest")));
        fake.text(&path("supervisor", OLDER), Ok(lines(3, "older")));
        fake
    }

    /// Opens the app and scrolls the dashboard to its end, where the Logs section is.
    fn open(cx: &mut TestAppContext, fake: &Arc<FakeApi>) -> (Entity<AppView>, AnyWindowHandle) {
        let (app, window) = open_app(cx, fake.clone());
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.scroll(
                "btn-next",
                ScrollDelta::Pixels(point(px(0.), px(-5000.))),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        (app, window)
    }

    /// The viewer's state, read through the dashboard's Monitor tab.
    fn viewer(cx: &mut TestAppContext, app: &Entity<AppView>) -> Entity<TextareaState> {
        cx.read(|cx| {
            let dashboard = app.read(cx).dashboard().expect("the dashboard shows");
            let monitor = dashboard
                .read(cx)
                .tab_view(Tab::Monitor)
                .downcast::<MonitorView>()
                .unwrap_or_else(|_| panic!("the Monitor tab is a MonitorView"));
            monitor.read(cx).logs.read(cx).view.clone()
        })
    }

    fn shown_text(cx: &mut TestAppContext, app: &Entity<AppView>) -> String {
        let view = viewer(cx, app);
        cx.read(|cx| view.read(cx).value().to_string())
    }

    /// The viewer's visible lines after a fresh frame.
    fn visible_lines(
        cx: &mut TestAppContext,
        app: &Entity<AppView>,
        window: AnyWindowHandle,
    ) -> std::ops::Range<usize> {
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
        let view = viewer(cx, app);
        cx.read(|cx| view.read(cx).visible_row_range())
            .expect("the viewer was laid out")
    }

    fn chosen(cx: &mut TestAppContext, window: AnyWindowHandle) -> Option<String> {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.find("log-select").value().map(str::to_string)
        })
        .unwrap()
    }

    /// Opens the dropdown, moves one option down and confirms with Enter.
    fn choose_next(cx: &mut TestAppContext, window: AnyWindowHandle) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.within("log-select").click("input", cx);
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
    }

    fn refresh(cx: &mut TestAppContext, window: AnyWindowHandle) {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("log-refresh", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    #[gpui::test]
    fn lists_the_files_newest_first_with_size_and_time(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest(), older()]));
        let (_, window) = open(cx, &fake);

        assert_eq!(chosen(cx, window).as_deref(), Some(NEWEST_LABEL));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.within("log-select").click("input", cx);
        })
        .unwrap();
        cx.run_until_parked();

        let options: Vec<String> = painted(cx, window)
            .into_iter()
            .filter(|t| t.starts_with("node: ") || t.starts_with("supervisor: "))
            .collect();
        // The closed dropdown's text, then the open menu's two options.
        assert_eq!(options, [NEWEST_LABEL, NEWEST_LABEL, OLDER_LABEL]);
    }

    #[gpui::test]
    fn opens_the_newest_file_scrolled_to_the_end(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest(), older()]));
        let text = lines(200, "newest");
        fake.text(&path("node", NEWEST), Ok(text.clone()));
        let (app, window) = open(cx, &fake);

        assert_eq!(shown_text(cx, &app), text);
        assert_eq!(count(&fake, "GET", &path("node", NEWEST)), 1);
        assert_eq!(count(&fake, "GET", &path("supervisor", OLDER)), 0);
        // Line 200 is row 199; row 200 is the empty row after the last newline.
        let visible = visible_lines(cx, &app, window);
        assert!(visible.contains(&199), "{visible:?}");
        assert!(!visible.contains(&0), "{visible:?}");
        // The caret is at the end too, so the arrow keys start from what shows.
        let view = viewer(cx, &app);
        assert_eq!(cx.read(|cx| view.read(cx).cursor()), text.len());
    }

    #[gpui::test]
    fn refresh_keeps_the_chosen_file_when_it_is_still_there(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest(), older()]));
        let (app, window) = open(cx, &fake);

        choose_next(cx, window);
        assert_eq!(shown_text(cx, &app), "Loading...");
        cx.run_until_parked();
        assert_eq!(shown_text(cx, &app), lines(3, "older"));

        refresh(cx, window);
        assert_eq!(count(&fake, "GET", "/api/logs"), 2);
        assert_eq!(count(&fake, "GET", &path("supervisor", OLDER)), 2);
        assert_eq!(chosen(cx, window).as_deref(), Some(OLDER_LABEL));
        assert_eq!(shown_text(cx, &app), lines(3, "older"));

        // The chosen file is gone: the newest opens.
        fake.reply("GET", "/api/logs", Ok(json!([newest()])));
        refresh(cx, window);
        assert_eq!(chosen(cx, window).as_deref(), Some(NEWEST_LABEL));
        assert_eq!(shown_text(cx, &app), lines(3, "newest"));
    }

    #[gpui::test]
    fn says_when_there_are_no_log_files(cx: &mut TestAppContext) {
        let fake = scripted(json!([]));
        let (app, window) = open(cx, &fake);

        assert_eq!(chosen(cx, window).as_deref(), Some("(no log files yet)"));
        assert_eq!(shown_text(cx, &app), "No log files yet.");
    }

    #[gpui::test]
    fn says_why_the_list_failed(cx: &mut TestAppContext) {
        let fake = scripted(json!([]));
        fake.reply(
            "GET",
            "/api/logs",
            Err(ApiError::Server("Disk on fire".into())),
        );
        let (app, window) = open(cx, &fake);

        assert_eq!(chosen(cx, window).as_deref(), Some("(could not load)"));
        assert_eq!(
            shown_text(cx, &app),
            "Could not list the log files: Disk on fire"
        );
    }

    #[gpui::test]
    fn shows_an_empty_file_as_empty(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest()]));
        fake.text(&path("node", NEWEST), Ok(String::new()));
        let (app, _) = open(cx, &fake);

        assert_eq!(shown_text(cx, &app), "(empty)");
    }

    #[gpui::test]
    fn shows_why_a_log_failed(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest()]));
        fake.text(
            &path("node", NEWEST),
            Err(ApiError::Server("Log file not found".into())),
        );
        let (app, _) = open(cx, &fake);

        assert_eq!(
            shown_text(cx, &app),
            "Could not load the log: Log file not found"
        );
    }

    #[gpui::test]
    fn a_large_log_with_non_ascii_text_loads_whole(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest()]));
        let mut text = TRUNCATED.to_string();
        let mut n = 0;
        while text.len() < 1_100_000 {
            n += 1;
            text.push_str(&format!(
                "2026-11-08 14:30:{:02} å ä ö 🎥 日本 line {n}\n",
                n % 60
            ));
        }
        fake.text(&path("node", NEWEST), Ok(text.clone()));
        let (app, window) = open(cx, &fake);

        // The end shows first, at the bottom of the viewer.
        let end = shown_text(cx, &app);
        assert!(
            end.len() < text.len() && text.ends_with(&end),
            "{}",
            end.len()
        );
        let visible = visible_lines(cx, &app, window);
        assert!(visible.contains(&(end.lines().count() - 1)), "{visible:?}");

        // The rest goes in front of it, a piece at a time.
        cx.executor().advance_clock(Duration::from_secs(1));
        let shown = shown_text(cx, &app);
        assert_eq!(shown.len(), text.len());
        assert!(
            shown == text,
            "the text in the viewer differs from the file"
        );
        // Line n follows the header line, so it is row n.
        let visible = visible_lines(cx, &app, window);
        assert!(visible.contains(&n), "{visible:?} for {n} lines");
    }

    #[gpui::test]
    fn choosing_another_file_while_a_large_one_goes_in_stops_it(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest(), older()]));
        fake.text(&path("node", NEWEST), Ok(lines(20_000, "newest")));
        let (app, window) = open(cx, &fake);
        assert!(shown_text(cx, &app).ends_with("newest line 20000\n"));

        choose_next(cx, window);
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));

        assert_eq!(shown_text(cx, &app), lines(3, "older"));
    }

    #[test]
    fn pieces_end_after_a_newline_and_cover_the_text() {
        let text = "å ä ö\n🎥 日本\nabc\n\nlast line without newline";

        let pieces = super::pieces(text, 4);

        let ends: Vec<&str> = pieces.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(
            ends,
            [
                "å ä ö\n",
                "🎥 日本\n",
                "abc\n\n",
                "last line without newline"
            ]
        );
        assert!(super::pieces("", 4).is_empty());
        let whole = 0..4;
        assert_eq!(super::pieces("one\n", 32), [whole]);
    }

    #[gpui::test]
    fn ctrl_a_and_ctrl_c_copy_the_whole_log(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest()]));
        let (_, window) = open(cx, &fake);

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.within("log-view").click("input-state", cx);
            window.press("ctrl-a", cx);
            window.press("ctrl-c", cx);
        })
        .unwrap();

        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied, Some(lines(3, "newest")));
    }

    #[gpui::test]
    fn a_late_answer_for_another_file_is_dropped(cx: &mut TestAppContext) {
        let fake = scripted(json!([newest(), older()]));
        let (app, window) = open(cx, &fake);

        // The two answers come back in either order; the repeats cover the older file's coming last.
        for _ in 0..12 {
            choose_next(cx, window);
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.within("log-select").click("input", cx);
                window.press("up", cx);
                window.press("enter", cx);
            })
            .unwrap();
            cx.run_until_parked();

            assert_eq!(chosen(cx, window).as_deref(), Some(NEWEST_LABEL));
            assert_eq!(shown_text(cx, &app), lines(3, "newest"));
        }
    }
}
