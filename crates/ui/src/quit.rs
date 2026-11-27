//! Quit StreamLoop, from the tray menu and from Settings: the dialog in the window, `POST /api/quit`, then the
//! wait for the control process to let go of its port before the app ends.

use std::time::Duration;

use gpui::{App, AppContext, Global, Task};

use crate::{
    Shell,
    api::ApiError,
    app::{ToastKind, confirm, now_ms},
    main_view, open_handle, save_window_state, show_main_window,
};

pub const QUIT_TITLE: &str = "Quit StreamLoop?";
pub const QUIT_DETAIL: &str =
    "The stream stops and StreamLoop closes. Start StreamLoop again to go back on air.";
pub const QUIT_UNREACHABLE: &str = "StreamLoop is not answering, so the stream may still be on air. Close this window anyway?";
/// After a quit the app asks `GET /api/status` this often until it stops answering.
pub const PORT_POLL_EVERY: Duration = Duration::from_secs(1);
/// The app ends this long after a quit even when the port still answers.
pub const PORT_WAIT_LIMIT: Duration = Duration::from_secs(60);
const QUIT: &str = "Quit";
const CLOSE: &str = "Close";

/// What the answer to `POST /api/quit` means for the app.
#[derive(Clone, Debug, PartialEq)]
pub enum QuitOutcome {
    /// The control process stops the stream and exits.
    Quit,
    /// It refused, with this text (409 during an update).
    Busy(String),
    Unreachable,
}

impl QuitOutcome {
    pub fn of(answer: Result<(), ApiError>) -> QuitOutcome {
        match answer {
            Ok(()) => QuitOutcome::Quit,
            Err(ApiError::Unreachable) => QuitOutcome::Unreachable,
            Err(err) => QuitOutcome::Busy(err.to_string()),
        }
    }
}

/// Set from a dialog until the app ends or the quit is given up, so a second Quit meanwhile only brings the
/// window forward.
#[derive(Default)]
struct Quitting(bool);

impl Global for Quitting {}

fn quitting(cx: &App) -> bool {
    cx.try_global::<Quitting>().is_some_and(|q| q.0)
}

fn set_quitting(cx: &mut App, on: bool) {
    cx.set_global(Quitting(on));
}

/// Opens the window first when it is closed, then asks in it. When the last poll found StreamLoop not
/// answering, the dialog offers to close the app only.
pub fn ask_to_quit(shell: &Shell, cx: &mut App) {
    show_main_window(cx, shell);
    if quitting(cx) {
        return;
    }
    if shell.tray.reachable() {
        confirm_quit(shell, cx);
    } else {
        offer_to_close(shell, cx);
    }
}

/// The Quit dialog; Quit sends `POST /api/quit`.
fn confirm_quit(shell: &Shell, cx: &mut App) {
    let Some(answer) = ask(QUIT_TITLE, Some(QUIT_DETAIL), QUIT, cx) else {
        return;
    };
    set_quitting(cx, true);
    let shell = shell.clone();
    cx.spawn(async move |cx| {
        if !answer.await {
            cx.update(|cx| set_quitting(cx, false));
            return;
        }
        let client = shell.client.clone();
        let outcome = QuitOutcome::of(cx.background_spawn(async move { client.quit() }).await);
        cx.update(|cx| match outcome {
            QuitOutcome::Quit => wait_for_the_port_to_close(&shell, cx),
            QuitOutcome::Busy(text) => {
                set_quitting(cx, false);
                if let Some(view) = main_view(cx) {
                    view.update(cx, |app, cx| app.toast(text, ToastKind::Error, cx));
                }
            }
            QuitOutcome::Unreachable => offer_to_close(&shell, cx),
        });
    })
    .detach();
}

/// StreamLoop does not answer, so nothing can stop the stream: Close ends the app alone. No "Quit StreamLoop?"
/// title, since Close leaves the stream on air.
fn offer_to_close(shell: &Shell, cx: &mut App) {
    let Some(answer) = ask(QUIT_UNREACHABLE, None, CLOSE, cx) else {
        set_quitting(cx, false);
        return;
    };
    set_quitting(cx, true);
    let shell = shell.clone();
    cx.spawn(async move |cx| {
        let close = answer.await;
        cx.update(|cx| {
            if close {
                quit_app(&shell, cx);
            } else {
                set_quitting(cx, false);
            }
        });
    })
    .detach();
}

/// The dialog in the window open now; `None` without one.
fn ask(message: &str, detail: Option<&str>, ok: &str, cx: &mut App) -> Option<Task<bool>> {
    open_handle(cx)?
        .update(cx, |_, window, cx| confirm(message, detail, ok, window, cx))
        .ok()
}

/// After a 200: polls `GET /api/status` every 1 s until it stops answering, at most 60 s, then quits the app.
pub fn wait_for_the_port_to_close(shell: &Shell, cx: &mut App) {
    let until = now_ms(cx) + PORT_WAIT_LIMIT.as_millis() as i64;
    let shell = shell.clone();
    cx.spawn(async move |cx| {
        loop {
            let client = shell.client.clone();
            let answer = cx.background_spawn(async move { client.status() }).await;
            if matches!(answer, Err(ApiError::Unreachable)) || cx.update(|cx| now_ms(cx)) >= until {
                break;
            }
            cx.background_executor().timer(PORT_POLL_EVERY).await;
        }
        cx.update(|cx| quit_app(&shell, cx));
    })
    .detach();
}

/// Ends the app. The window state is saved and the tray icon dropped before GPUI quits.
fn quit_app(shell: &Shell, cx: &mut App) {
    save_window_state(cx);
    shell.tray.detach();
    cx.quit();
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::TestAppContext;
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        api::fake::FakeApi,
        app::POLL_EVERY,
        begin, on_tray_event,
        test_support::{
            self, StateDir, UPDATE_RUNNING, answer_polls, count, open_shell, record_tray,
            saving_to, shows,
        },
        tray::TrayEvent,
        window_state,
    };

    fn quit_answers(fake: &FakeApi, answer: Result<Value, ApiError>) {
        fake.reply("POST", "/api/quit", answer);
    }

    /// The tray menu's Quit StreamLoop.
    fn quit_from_the_tray(cx: &mut TestAppContext, shell: &Shell) {
        cx.update(|cx| on_tray_event(TrayEvent::Quit, cx, shell));
        cx.run_until_parked();
    }

    fn answer(cx: &mut TestAppContext, button: &str) {
        cx.simulate_prompt_answer(button);
        cx.run_until_parked();
    }

    fn asks(title: &str, detail: &str) -> Option<(String, String)> {
        Some((title.to_string(), detail.to_string()))
    }

    #[gpui::test]
    fn cancel_sends_nothing(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        quit_answers(&fake, Ok(json!({ "ok": true })));
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);
        let (_, window) = open_shell(cx, &shell, None);

        // Act
        quit_from_the_tray(cx, &shell);
        let asked = cx.pending_prompt();
        answer(cx, "Cancel");
        cx.executor().advance_clock(Duration::from_secs(10));

        // Assert
        assert_eq!(asked, asks(QUIT_TITLE, QUIT_DETAIL));
        assert!(fake.posts().is_empty(), "Cancel sends nothing");
        assert!(!tray.borrow().dropped);
        assert_eq!(cx.windows(), vec![window]);
    }

    #[gpui::test]
    fn quit_from_the_tray_opens_the_window_for_the_dialog(cx: &mut TestAppContext) {
        // Arrange: a tray start, so no window is open
        let fake = FakeApi::new();
        answer_polls(&fake);
        let shell = test_support::shell(fake.clone());
        cx.update(gpui_kit::init);
        cx.update(crate::theme::init);
        cx.update(|cx| begin(cx, &shell, true));
        cx.run_until_parked();
        let windows_before = cx.windows().len();

        // Act
        quit_from_the_tray(cx, &shell);
        let asked = cx.pending_prompt();
        answer(cx, "Cancel");

        // Assert
        assert_eq!(windows_before, 0);
        assert_eq!(cx.windows().len(), 1, "the window opened for the dialog");
        assert_eq!(asked, asks(QUIT_TITLE, QUIT_DETAIL));
        assert!(fake.posts().is_empty());
    }

    #[gpui::test]
    fn quit_during_an_update_shows_the_409_and_keeps_the_app(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        quit_answers(&fake, Err(ApiError::Server(UPDATE_RUNNING.into())));
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);
        let (_, window) = open_shell(cx, &shell, None);

        // Act
        quit_from_the_tray(cx, &shell);
        answer(cx, "Quit");
        let shown = shows(cx, window, UPDATE_RUNNING);
        cx.executor().advance_clock(PORT_WAIT_LIMIT + Duration::from_secs(10));
        quit_from_the_tray(cx, &shell);
        let asked_again = cx.pending_prompt();
        answer(cx, "Cancel");

        // Assert
        assert!(shown, "the 409's text shows as a refused action's does");
        assert_eq!(count(&fake, "POST", "/api/quit"), 1);
        assert!(!tray.borrow().dropped, "the app keeps running");
        assert_eq!(cx.windows(), vec![window]);
        assert_eq!(asked_again, asks(QUIT_TITLE, QUIT_DETAIL), "a later Quit asks again");
    }

    #[gpui::test]
    fn an_unreachable_server_asks_to_close_the_app_only(cx: &mut TestAppContext) {
        // Arrange: the window's last polls found nothing answering
        let fake = FakeApi::new();
        answer_polls(&fake);
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);
        open_shell(cx, &shell, None);
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(POLL_EVERY * 2);

        // Act
        quit_from_the_tray(cx, &shell);
        let asked = cx.pending_prompt();
        answer(cx, "Close");

        // Assert
        assert_eq!(asked, asks(QUIT_UNREACHABLE, ""), "its one text, with no title above it");
        assert!(fake.posts().is_empty(), "no POST /api/quit");
        assert!(tray.borrow().dropped, "the app quits");
    }

    #[gpui::test]
    fn a_quit_that_cannot_reach_streamloop_asks_to_close_the_app_only(cx: &mut TestAppContext) {
        // Arrange: StreamLoop answered the polls, and POST /api/quit finds nothing on the port
        let fake = FakeApi::new();
        answer_polls(&fake);
        quit_answers(&fake, Err(ApiError::Unreachable));
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);
        open_shell(cx, &shell, None);

        // Act
        quit_from_the_tray(cx, &shell);
        answer(cx, "Quit");
        let asked = cx.pending_prompt();
        answer(cx, "Close");

        // Assert
        assert_eq!(asked, asks(QUIT_UNREACHABLE, ""), "its one text, with no title above it");
        assert_eq!(count(&fake, "POST", "/api/quit"), 1);
        assert!(tray.borrow().dropped, "the app quits");
    }

    #[gpui::test]
    fn after_quit_the_app_exits_once_the_port_stops_answering(cx: &mut TestAppContext) {
        // Arrange: the control process still answers for 3 s after the quit, while it stops the supervisor
        let fake = FakeApi::new();
        answer_polls(&fake);
        quit_answers(&fake, Ok(json!({ "ok": true })));
        let dir = StateDir::new("quit");
        let shell = saving_to(fake.clone(), &dir.file());
        let tray = record_tray(&shell);
        open_shell(cx, &shell, None);
        quit_from_the_tray(cx, &shell);

        // Act
        answer(cx, "Quit");
        cx.executor().advance_clock(Duration::from_secs(3));
        let while_answering = tray.borrow().dropped;
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(PORT_POLL_EVERY);

        // Assert
        assert_eq!(count(&fake, "POST", "/api/quit"), 1);
        assert!(!while_answering, "the app waits while StreamLoop still answers");
        assert!(tray.borrow().dropped, "the app ends once the port stops answering");
        assert!(
            window_state::load(&dir.file()).width.is_some(),
            "the window state was saved"
        );
    }

    #[gpui::test]
    fn the_wait_for_the_port_ends_after_60_s(cx: &mut TestAppContext) {
        // Arrange: a port that keeps answering
        let fake = FakeApi::new();
        answer_polls(&fake);
        quit_answers(&fake, Ok(json!({ "ok": true })));
        let shell = test_support::shell(fake.clone());
        let tray = record_tray(&shell);
        open_shell(cx, &shell, None);
        quit_from_the_tray(cx, &shell);

        // Act
        answer(cx, "Quit");
        cx.executor()
            .advance_clock(PORT_WAIT_LIMIT - Duration::from_secs(1));
        let before_the_limit = tray.borrow().dropped;
        cx.executor().advance_clock(Duration::from_secs(1));

        // Assert
        assert!(!before_the_limit);
        assert!(tray.borrow().dropped, "the app ends after 60 s");
    }

    #[test]
    fn the_answer_to_quit_picks_the_outcome() {
        // Arrange
        let answers = [
            Ok(()),
            Err(ApiError::Server(UPDATE_RUNNING.into())),
            Err(ApiError::Unreachable),
        ];

        // Act
        let outcomes = answers.map(QuitOutcome::of);

        // Assert
        assert_eq!(
            outcomes,
            [
                QuitOutcome::Quit,
                QuitOutcome::Busy(UPDATE_RUNNING.into()),
                QuitOutcome::Unreachable
            ]
        );
    }
}
