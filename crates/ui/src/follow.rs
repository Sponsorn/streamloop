//! The window follows the exe: after an update or a rollback the control process runs another version than
//! the app, and the app restarts into the install's exe, where its window was.

use std::{io, path::Path, process::Stdio, time::Duration};

use gpui::{App, Global};
use streamloop_common::{paths::Layout, procs};

use crate::{Shell, main_shell, quit::end_app, save_window_state};

/// How long an app started with `--follow` waits for the app it replaces to let go of the install's lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(60);

/// Restart into the control process's version when it differs from ours, unless we were started with
/// --follow V, are not V ourselves, and the control process still reports V.
pub fn should_follow(own: &str, control: &str, followed: Option<&str>) -> bool {
    own != control && followed != Some(control)
}

/// `detach ui --follow V`, then `--tray` when no window was open and `--port N` when the app was given its port.
pub fn follow_args(version: &str, window_open: bool, port: Option<u16>) -> Vec<String> {
    let mut args = vec![
        "detach".to_string(),
        "ui".to_string(),
        "--follow".to_string(),
        version.to_string(),
    ];
    if !window_open {
        args.push("--tray".to_string());
    }
    if let Some(port) = port {
        args.extend(["--port".to_string(), port.to_string()]);
    }
    args
}

/// Starts the app that follows. `Detach` is the app's own; the UI tests record what they are asked.
pub trait Spawn {
    fn spawn(&self, exe: &Path, args: &[String]) -> io::Result<()>;
}

/// Runs the program through `procs::command` with its output on NUL, and only reaps it, on a thread of its own.
pub struct Detach;

impl Spawn for Detach {
    fn spawn(&self, exe: &Path, args: &[String]) -> io::Result<()> {
        let mut c = procs::command(exe);
        c.args(args).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = c.spawn()?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }
}

/// Set once the app that follows has started, so a poll before this app ends starts no second one.
#[derive(Default)]
struct Following(bool);

impl Global for Following {}

/// Follows the control process's version when `should_follow` says so.
pub fn check(shell: &Shell, control: Option<&str>, window_open: bool, cx: &mut App) {
    if let Some(control) = control
        && should_follow(shell.version, control, shell.followed.as_deref())
    {
        follow(shell, control, window_open, cx);
    }
}

/// `check` for the window open now: its polls and its connect.
pub(crate) fn check_from_window(control: Option<&str>, cx: &mut App) {
    if let Some(shell) = main_shell(cx) {
        check(&shell, control, true, cx);
    }
}

/// `ROOT\streamloop.exe detach ui --follow V [--tray] [--port N]`, through procs::command; the app then quits.
/// The window state is saved first, so the new app opens where this window was. Without a root the app never
/// follows, and a start that fails leaves the app running as it was.
pub fn follow(shell: &Shell, version: &str, window_open: bool, cx: &mut App) {
    let Some(root) = shell.root.as_deref() else {
        return;
    };
    if cx.try_global::<Following>().is_some_and(|following| following.0) {
        return;
    }
    save_window_state(cx);
    let exe = Layout::new(root).exe;
    let args = follow_args(version, window_open, shell.port);
    if shell.spawn.spawn(&exe, &args).is_err() {
        return;
    }
    cx.set_global(Following(true));
    end_app(shell, cx);
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

    use gpui::TestAppContext;
    use serde_json::json;

    use super::*;
    use crate::{
        api::{ApiError, fake::FakeApi},
        app::POLL_EVERY,
        test_support::{
            StateDir, TrayLog, answer_polls, close, count, open_shell, record_tray, saving_to,
            status_json, update_json,
        },
        tray::CLOSED_POLL_EVERY,
    };

    #[test]
    fn follows_a_different_version() {
        // Act
        let follows = should_follow("3.2.0", "3.2.1", None);

        // Assert
        assert!(follows);
    }

    #[test]
    fn stays_when_the_versions_match() {
        // Act
        let (plain, follower) = (
            should_follow("3.2.0", "3.2.0", None),
            should_follow("3.2.0", "3.2.0", Some("3.2.0")),
        );

        // Assert
        assert!(!plain);
        assert!(!follower);
    }

    #[test]
    fn a_follower_that_is_not_v_does_not_loop_while_control_reports_v() {
        // Act: a dev root whose exe is a -dev build of the version its control process runs
        let follows = should_follow("3.2.0-dev", "3.2.0", Some("3.2.0"));

        // Assert
        assert!(!follows);
    }

    #[test]
    fn a_follower_follows_a_later_different_version() {
        // Act: the app followed 3.2.1, which then rolled back to 3.2.0
        let follows = should_follow("3.2.1", "3.2.0", Some("3.2.1"));

        // Assert
        assert!(follows);
    }

    #[test]
    fn follow_args_add_tray_and_port_only_when_asked() {
        // Act
        let args = [
            follow_args("3.2.1", true, None),
            follow_args("3.2.1", false, None),
            follow_args("3.2.1", true, Some(8123)),
        ];

        // Assert
        assert_eq!(
            args,
            [
                vec!["detach", "ui", "--follow", "3.2.1"],
                vec!["detach", "ui", "--follow", "3.2.1", "--tray"],
                vec!["detach", "ui", "--follow", "3.2.1", "--port", "8123"],
            ]
        );
    }

    /// This app's version in the UI tests.
    const OWN: &str = "3.2.0";

    /// What a follow asked to start, and what had happened by then.
    #[derive(Debug)]
    struct Spawned {
        exe: PathBuf,
        args: Vec<String>,
        state_saved: bool,
        tray_dropped: bool,
    }

    struct RecordingSpawn {
        spawned: Rc<RefCell<Vec<Spawned>>>,
        state: PathBuf,
        tray: Rc<RefCell<TrayLog>>,
        refuse: bool,
    }

    impl Spawn for RecordingSpawn {
        fn spawn(&self, exe: &Path, args: &[String]) -> io::Result<()> {
            self.spawned.borrow_mut().push(Spawned {
                exe: exe.to_path_buf(),
                args: args.to_vec(),
                state_saved: self.state.exists(),
                tray_dropped: self.tray.borrow().dropped,
            });
            if self.refuse {
                Err(io::Error::from(io::ErrorKind::NotFound))
            } else {
                Ok(())
            }
        }
    }

    struct Follower {
        shell: Shell,
        spawned: Rc<RefCell<Vec<Spawned>>>,
        tray: Rc<RefCell<TrayLog>>,
    }

    /// An app of the install in `dir` that runs OWN, keeps its window state there and records its tray icon and
    /// what it starts; with `refuse` every start fails.
    fn follower(fake: Arc<FakeApi>, dir: &StateDir, refuse: bool) -> Follower {
        let mut shell = saving_to(fake, &dir.file());
        shell.root = Some(dir.path().to_path_buf());
        shell.version = OWN;
        let tray = record_tray(&shell);
        let spawned = Rc::new(RefCell::new(Vec::new()));
        shell.spawn = Rc::new(RecordingSpawn {
            spawned: spawned.clone(),
            state: dir.file(),
            tray: tray.clone(),
            refuse,
        });
        Follower {
            shell,
            spawned,
            tray,
        }
    }

    /// `GET /api/update/status` from a control process that runs `version`.
    fn control_runs(fake: &FakeApi, version: &str) {
        let mut update = update_json("idle");
        update["currentVersion"] = json!(version);
        fake.reply("GET", "/api/update/status", Ok(update));
    }

    #[gpui::test]
    fn a_version_difference_starts_detach_ui_and_quits(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        control_runs(&fake, "3.2.1");
        let dir = StateDir::new("follow-open");
        let f = follower(fake, &dir, false);

        // Act: the window's first poll finds the other version, and more polls come before the app ends
        open_shell(cx, &f.shell, None);
        cx.executor().advance_clock(POLL_EVERY * 3);

        // Assert
        let spawned = f.spawned.borrow();
        assert_eq!(spawned.len(), 1, "one start, however many polls follow it");
        assert_eq!(spawned[0].exe, dir.path().join("streamloop.exe"));
        assert_eq!(spawned[0].args, ["detach", "ui", "--follow", "3.2.1"]);
        assert!(spawned[0].state_saved, "the window state was saved before the start");
        assert!(!spawned[0].tray_dropped, "the app quit only after the start");
        assert!(f.tray.borrow().dropped, "the app quits as Quit ends it");
    }

    #[gpui::test]
    fn a_closed_window_follows_with_tray(cx: &mut TestAppContext) {
        // Arrange: the window closed while the control process ran this app's version
        let fake = FakeApi::new();
        answer_polls(&fake);
        control_runs(&fake, OWN);
        let dir = StateDir::new("follow-closed");
        let f = follower(fake.clone(), &dir, false);
        let (_, window) = open_shell(cx, &f.shell, None);
        close(cx, window);
        let at_close = count(&fake, "GET", "/api/update/status");
        cx.executor().advance_clock(CLOSED_POLL_EVERY * 2);
        let while_answering = count(&fake, "GET", "/api/update/status") - at_close;

        // Act: the control process is away for the update, then answers on 3.2.1
        fake.reply("GET", "/api/status", Err(ApiError::Unreachable));
        cx.executor().advance_clock(CLOSED_POLL_EVERY);
        control_runs(&fake, "3.2.1");
        fake.reply("GET", "/api/status", Ok(status_json("live", false)));
        cx.executor().advance_clock(CLOSED_POLL_EVERY);

        // Assert
        assert_eq!(while_answering, 0, "no update status while the polls answer");
        assert_eq!(
            count(&fake, "GET", "/api/update/status") - at_close,
            1,
            "one read, after the reconnect"
        );
        let spawned = f.spawned.borrow();
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].args, ["detach", "ui", "--follow", "3.2.1", "--tray"]);
        assert!(f.tray.borrow().dropped);
    }

    #[gpui::test]
    fn a_follow_passes_on_the_port_the_app_was_given(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        control_runs(&fake, "3.2.1");
        let dir = StateDir::new("follow-port");
        let mut f = follower(fake, &dir, false);
        f.shell.port = Some(8123);

        // Act
        open_shell(cx, &f.shell, None);

        // Assert
        let spawned = f.spawned.borrow();
        assert_eq!(
            spawned[0].args,
            ["detach", "ui", "--follow", "3.2.1", "--port", "8123"]
        );
    }

    #[gpui::test]
    fn the_wizard_follows_a_different_version_when_it_connects(cx: &mut TestAppContext) {
        // Arrange: a first run, whose wizard does not poll
        let fake = FakeApi::new();
        fake.reply("GET", "/api/status", Ok(status_json("off-air", true)));
        control_runs(&fake, "3.2.1");
        let dir = StateDir::new("follow-wizard");
        let f = follower(fake, &dir, false);

        // Act
        open_shell(cx, &f.shell, None);

        // Assert
        let spawned = f.spawned.borrow();
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].args, ["detach", "ui", "--follow", "3.2.1"]);
    }

    #[gpui::test]
    fn without_a_root_the_app_never_follows(cx: &mut TestAppContext) {
        // Arrange
        let fake = FakeApi::new();
        answer_polls(&fake);
        control_runs(&fake, "3.2.1");
        let dir = StateDir::new("follow-no-root");
        let mut f = follower(fake, &dir, false);
        f.shell.root = None;

        // Act
        open_shell(cx, &f.shell, None);
        cx.executor().advance_clock(POLL_EVERY * 3);

        // Assert
        assert!(f.spawned.borrow().is_empty());
        assert!(!f.tray.borrow().dropped);
    }

    #[gpui::test]
    fn a_follower_stays_while_the_control_process_reports_the_version_it_followed(
        cx: &mut TestAppContext,
    ) {
        // Arrange: started with --follow 3.2.1, and this exe is not 3.2.1
        let fake = FakeApi::new();
        answer_polls(&fake);
        control_runs(&fake, "3.2.1");
        let dir = StateDir::new("follow-no-loop");
        let mut f = follower(fake, &dir, false);
        f.shell.followed = Some("3.2.1".into());

        // Act
        open_shell(cx, &f.shell, None);
        cx.executor().advance_clock(POLL_EVERY * 3);

        // Assert
        assert!(f.spawned.borrow().is_empty());
        assert!(!f.tray.borrow().dropped);
    }

    #[gpui::test]
    fn a_start_that_fails_keeps_the_app_running(cx: &mut TestAppContext) {
        // Arrange: the install's exe cannot be started
        let fake = FakeApi::new();
        answer_polls(&fake);
        control_runs(&fake, "3.2.1");
        let dir = StateDir::new("follow-refused");
        let f = follower(fake, &dir, true);

        // Act
        let (_, window) = open_shell(cx, &f.shell, None);

        // Assert
        assert_eq!(f.spawned.borrow().len(), 1);
        assert!(!f.tray.borrow().dropped, "the app keeps running");
        assert_eq!(cx.windows(), vec![window]);
    }
}
