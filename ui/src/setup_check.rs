//! The system check that the wizard and the Playback tab share: one run at a time, its report and "Use these settings".

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll, Waker},
};

use gpui::{
    AnyElement, App, AppContext, Context, Div, Entity, Font, FontFallbacks, Global, IntoElement,
    SharedString, Window, div, font, prelude::*, px, rgb,
};
use gpui_kit::{
    base::TestSupportExt,
    component::{
        IndexPath, Sizable,
        label::Label,
        select::{Select, SelectItem, SelectState},
        tooltip::Tooltip,
    },
};
use serde_json::{Value, json};

use crate::{
    api::{
        ApiError,
        client::Client,
        types::{SaveResult, SetupReport},
    },
    logic::{DependencyRow, ENCODER_NAMES, Level, dependency_rows, encoder_label, save_patch},
    theme::{BG, ERROR_TEXT, FONT, GREEN, MUTED, level_color},
    widgets::{fit_menu, reveal_on_focus},
};

/// The run button's label once a check has run or failed.
pub const RUN_LABEL: &str = "Run checks again";
/// The run button's label while its check runs.
pub const CHECKING_LABEL: &str = "Checking...";
pub const CHECKING_TEXT: &str = "Checking this PC. This can take up to a minute.";

/// The one `POST /api/setup-check/run` in flight, as admin.js `setupRun`.
#[derive(Default)]
pub struct SetupCheck {
    run: Option<SetupRun>,
}

impl Global for SetupCheck {}

impl SetupCheck {
    /// admin.js `setupRun ??= post(...)`: starts a run, or joins the one in flight, whose
    /// `playlist_id` then stands.
    pub fn run(cx: &mut App, client: &Client, playlist_id: Option<String>) -> SetupRun {
        if let Some(run) = cx
            .try_global::<SetupCheck>()
            .and_then(|check| check.run.clone())
        {
            return run;
        }
        let run = SetupRun::default();
        cx.set_global(SetupCheck {
            run: Some(run.clone()),
        });
        let (client, answered) = (client.clone(), run.clone());
        cx.spawn(async move |cx| {
            let answer = cx
                .background_spawn(async move { client.run_setup_check(playlist_id.as_deref()) })
                .await;
            // As admin.js's `finally`: no run is in flight once a caller sees the answer.
            cx.update(|cx| cx.set_global(SetupCheck::default()));
            answered.finish(answer);
        })
        .detach();
        run
    }

    pub fn running(cx: &App) -> bool {
        cx.try_global::<SetupCheck>()
            .is_some_and(|check| check.run.is_some())
    }
}

/// A run's answer, awaited by every caller that joined it. It stands in for `futures`' `Shared`,
/// which this crate does not depend on.
#[derive(Clone, Default)]
pub struct SetupRun(Rc<RefCell<RunState>>);

#[derive(Default)]
struct RunState {
    answer: Option<Result<SetupReport, ApiError>>,
    wakers: Vec<Waker>,
}

impl SetupRun {
    fn finish(&self, answer: Result<SetupReport, ApiError>) {
        let wakers = {
            let mut state = self.0.borrow_mut();
            state.answer = Some(answer);
            std::mem::take(&mut state.wakers)
        };
        wakers.into_iter().for_each(Waker::wake);
    }
}

impl Future for SetupRun {
    type Output = Result<SetupReport, ApiError>;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let mut state = self.0.borrow_mut();
        match &state.answer {
            Some(answer) => Poll::Ready(answer.clone()),
            None => {
                state.wakers.push(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// What a report area shows (index.html `.setup-report`).
#[derive(Clone, Default)]
pub enum ReportArea {
    #[default]
    Empty,
    Checking,
    Failed(SharedString),
    Report(SetupReport),
}

impl ReportArea {
    /// admin.js `runSetupCheck`'s result: the report, or why the check could not run.
    pub fn after_run(answer: &Result<SetupReport, ApiError>) -> ReportArea {
        match answer {
            Ok(report) => ReportArea::Report(report.clone()),
            Err(err) => ReportArea::Failed(format!("The check could not run: {err}").into()),
        }
    }

    /// admin.css `.setup-report`, muted while checking and in the error colour after a failure.
    pub fn render(&self) -> impl IntoElement {
        let text = |text: SharedString, color: u32| {
            Label::new(text)
                .text_size(px(13.))
                .text_color(rgb(color))
                .into_any_element()
        };
        let content = match self {
            ReportArea::Empty => None,
            ReportArea::Checking => Some(text(CHECKING_TEXT.into(), MUTED)),
            ReportArea::Failed(why) => Some(text(why.clone(), ERROR_TEXT)),
            ReportArea::Report(report) => Some(report_view(report).into_any_element()),
        };
        div().mt(px(8.)).mb(px(12.)).children(content)
    }
}

/// admin.js `renderSetupReport`: the encoder line, the cache line and its warning, then the
/// dependencies with the missing ones first. A found file's path is its tooltip.
pub fn report_view(report: &SetupReport) -> impl IntoElement {
    let (encoder, level) = encoder_label(report);
    let cache = &report.cache;
    let warning = cache.warning.clone().filter(|w| !w.is_empty());
    let cache_level = if warning.is_some() {
        Level::Warn
    } else {
        Level::Ok
    };
    div()
        .child(report_line(format!("Encoder: {encoder}"), level))
        .child(report_line(
            format!(
                "Download cache: {} MB recommended ({} MB free on the drive)",
                cache.recommended_mb, cache.free_mb
            ),
            cache_level,
        ))
        .children(warning.map(|w| report_line(w, Level::Warn)))
        .child(
            div().mt(px(8.)).font(marks_font()).children(
                dependency_rows(report)
                    .into_iter()
                    .enumerate()
                    .map(|(index, row)| dependency(index, row)),
            ),
        )
}

/// Segoe UI with Segoe UI Symbol behind it, so ✔ and ✘ draw as text in the row's colour, as the
/// browser draws them. Windows' own fallback would draw ✔ from the colour emoji font.
pub fn marks_font() -> Font {
    Font {
        fallbacks: Some(FontFallbacks::from_fonts(vec!["Segoe UI Symbol".into()])),
        ..font(FONT)
    }
}

/// admin.css `.setup-line`.
fn report_line(text: String, level: Level) -> Label {
    Label::new(text)
        .mb(px(4.))
        .text_size(px(13.))
        .text_color(rgb(level_color(level)))
}

/// One `.setup-deps` row, `("setup-dep", index)`.
fn dependency(index: usize, row: DependencyRow) -> AnyElement {
    let mark = if row.ok { "✔" } else { "✘" };
    let item = div().id(("setup-dep", index)).mb(px(2.)).child(
        Label::new(format!("{mark} {}", row.text))
            .text_size(px(13.))
            .text_color(rgb(if row.ok { GREEN } else { ERROR_TEXT })),
    );
    if !row.ok {
        return item.test_support().into_any_element();
    }
    let path = SharedString::from(row.path);
    item.tooltip(move |window, cx| {
        let path = path.clone();
        Tooltip::element(move |_, _| Label::new(path.clone())).build(window, cx)
    })
    .test_support()
    .into_any_element()
}

/// One encoder dropdown entry: its config value and the name `ENCODER_NAMES` shows.
#[derive(Clone)]
pub struct EncoderOption {
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

/// The encoder dropdown's state: the `ENCODER_NAMES` options in order.
pub type EncoderSelect = SelectState<Vec<EncoderOption>>;

/// A new encoder dropdown that shows the first option, as a `<select>` does before a script sets it.
pub fn encoder_select_state<V: 'static>(
    window: &mut Window,
    cx: &mut Context<V>,
) -> Entity<EncoderSelect> {
    let options: Vec<EncoderOption> = ENCODER_NAMES
        .iter()
        .map(|(value, name)| EncoderOption {
            value: (*value).into(),
            name: (*name).into(),
        })
        .collect();
    cx.new(|cx| SelectState::new(options, Some(IndexPath::new(0)), window, cx))
}

/// admin.css `.form-group select`: the encoder dropdown `id`, at most 280 px wide, its menu `{id}-menu`.
pub fn encoder_select(state: &Entity<EncoderSelect>, id: &'static str) -> Div {
    div()
        .w_full()
        .max_w(px(280.))
        .child(reveal_on_focus(fit_menu(
            format!("{id}-menu"),
            Select::new(state)
                .id(id)
                .large()
                .placeholder("")
                .accessibility_label("Encoder")
                .h(px(40.))
                .px(px(12.))
                .bg(rgb(BG)),
            ENCODER_NAMES
                .iter()
                .map(|(_, name)| SharedString::from(*name)),
        )))
}

/// admin.js `applySetup`: saves the picked encoder and cache size where they differ from the
/// saved config. `None` when nothing differs, so nothing was sent.
pub fn apply_setup(client: &Client, picked: Value) -> Result<Option<SaveResult>, ApiError> {
    let config = client.config()?;
    let saved = json!({ "supervisor": config.get("supervisor").cloned().unwrap_or(Value::Null) });
    let patch = save_patch(&saved, &json!({ "supervisor": picked }));
    if patch.get("supervisor").is_none() {
        return Ok(None);
    }
    client.save_config(patch).map(Some)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{AppContext, Context, Render, TestAppContext, Window, WindowOptions};
    use gpui_kit::test::TestWindowExt;
    use serde_json::json;

    use super::*;
    use crate::{
        api::fake::FakeApi,
        test_support::{config_json, painted, setup_report_json},
    };

    #[gpui::test]
    async fn a_second_run_joins_the_first(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        fake.reply("POST", "/api/setup-check/run", Ok(setup_report_json()));
        let client = Client(fake.clone());

        // Neither run's request has gone out when the second caller asks.
        let (first, second) = cx.update(|cx| {
            let first = SetupCheck::run(cx, &client, Some("PLmain".into()));
            let second = SetupCheck::run(cx, &client, None);
            (first, second)
        });
        assert!(cx.update(|cx| SetupCheck::running(cx)));

        let (first, second) = (first.await.unwrap(), second.await.unwrap());

        assert_eq!(fake.posts().len(), 1);
        assert_eq!(
            fake.posts()[0].body,
            Some(json!({ "playlistId": "PLmain" }))
        );
        assert_eq!(first.cache.recommended_mb, 4096);
        assert_eq!(second.cache.recommended_mb, 4096);
        assert!(!cx.update(|cx| SetupCheck::running(cx)));

        let third = cx.update(|cx| SetupCheck::run(cx, &client, None));
        third.await.unwrap();
        assert_eq!(fake.posts().len(), 2);
        assert_eq!(fake.posts()[1].body, Some(json!({})));
    }

    #[gpui::test]
    async fn a_failed_run_ends_for_every_caller(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        fake.reply(
            "POST",
            "/api/setup-check/run",
            Err(ApiError::Server("ffmpeg is missing".into())),
        );
        let client = Client(fake.clone());

        let (first, second) = cx.update(|cx| {
            (
                SetupCheck::run(cx, &client, None),
                SetupCheck::run(cx, &client, None),
            )
        });

        assert_eq!(first.await.unwrap_err().to_string(), "ffmpeg is missing");
        assert_eq!(second.await.unwrap_err().to_string(), "ffmpeg is missing");
        assert_eq!(fake.posts().len(), 1);
        assert!(!cx.update(|cx| SetupCheck::running(cx)));
    }

    #[test]
    fn apply_setup_posts_only_what_differs() {
        let fake = FakeApi::new();
        let mut config = config_json();
        config["supervisor"]["encoder"] = json!("libx264");
        fake.reply("GET", "/api/config", Ok(config));
        fake.reply(
            "POST",
            "/api/config",
            Ok(json!({ "ok": true, "restartRequired": true })),
        );
        let client = Client(fake.clone());

        let saved = apply_setup(
            &client,
            json!({ "cacheBudgetMb": 4096, "encoder": "h264_nvenc" }),
        )
        .unwrap();

        assert!(saved.expect("a save").restart_required);
        assert_eq!(
            fake.posts()[0].body,
            Some(json!({ "supervisor": {
                "cacheBudgetMb": 4096, "encoder": "h264_nvenc", "preferredEncoder": "h264_nvenc"
            }}))
        );

        let same = apply_setup(
            &client,
            json!({ "cacheBudgetMb": 300, "encoder": "libx264" }),
        );

        assert!(same.unwrap().is_none());
        assert_eq!(fake.posts().len(), 1);
    }

    #[test]
    fn apply_setup_passes_on_a_failed_config_read() {
        let fake = FakeApi::new();
        let client = Client(fake.clone());

        let answer = apply_setup(&client, json!({ "cacheBudgetMb": 4096 }));

        assert_eq!(answer.unwrap_err(), ApiError::Unreachable);
        assert!(fake.posts().is_empty());
    }

    struct Probe(SetupReport);

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(report_view(&self.0))
        }
    }

    fn report(value: Value) -> SetupReport {
        serde_json::from_value(value).unwrap()
    }

    #[gpui::test]
    fn report_view_lists_failures_first_with_paths_as_tooltips(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(crate::theme::init);
        let mut value = setup_report_json();
        value["cache"]["warning"] = json!("Less than 2 GB free on this drive.");
        let (window, _) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                    cx.new(|_| Probe(report(value)))
                })
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            painted(cx, window),
            [
                "Encoder: NVIDIA (NVENC)",
                "Download cache: 4096 MB recommended (120000 MB free on the drive)",
                "Less than 2 GB free on this drive.",
                r"✘ Deno is missing: C:\StreamLoop3\streamloop\yt-dlp\deno.exe",
                "✔ ffmpeg",
            ]
        );

        let ffmpeg = r"C:\StreamLoop3\streamloop\ffmpeg\ffmpeg.exe";
        cx.update_window(window, |_, window, cx| {
            window.hover(("setup-dep", 1usize), cx)
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_secs(1));
        assert!(painted(cx, window).iter().any(|t| t == ffmpeg));

        // A missing file names its path in the line itself and has no tooltip.
        cx.update_window(window, |_, window, cx| {
            window.hover(("setup-dep", 0usize), cx)
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_secs(1));
        let texts = painted(cx, window);
        assert!(!texts.iter().any(|t| t == ffmpeg));
        assert!(
            !texts
                .iter()
                .any(|t| t == r"C:\StreamLoop3\streamloop\yt-dlp\deno.exe")
        );
    }

    #[gpui::test]
    fn report_view_says_when_no_encoder_works(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(crate::theme::init);
        let mut value = setup_report_json();
        value["encoder"]["works"] = json!(false);
        let (window, _) = cx
            .update(|cx| {
                gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                    cx.new(|_| Probe(report(value)))
                })
            })
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            painted(cx, window)[0],
            "Encoder: No encoder works on this PC. Check the graphics driver."
        );
    }
}
