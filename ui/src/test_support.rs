//! The UI tests' window on a `FakeApi`: opening the app, reading what a window painted, and canned answers.

use std::sync::Arc;

use gpui::{AnyWindowHandle, AppContext, Entity, TestAppContext};
use gpui_kit::{base::test_support::texts, test::TestWindowExt};
use serde_json::{Value, json};

use crate::{
    api::{client::Client, fake::FakeApi},
    app::AppView,
};

/// Opens the app window on `fake` and runs until it waits for a timer.
/// Drive it with `cx.update_window` and `TestWindowExt`; move time with `cx.executor().advance_clock`.
pub fn open_app(cx: &mut TestAppContext, fake: Arc<FakeApi>) -> (Entity<AppView>, AnyWindowHandle) {
    // Init gets its own update: in the window's update, its effects draw the new window a second time.
    cx.update(gpui_kit::init);
    cx.update(crate::theme::init);
    let client = Client(fake);
    let (window, app) = cx
        .update(|cx| {
            crate::open_main_window(cx, |window, cx| {
                cx.new(|cx| AppView::new(client, window, cx))
            })
        })
        .expect("open the app window");
    cx.run_until_parked();
    (app, window)
}

/// Draws a frame and returns the text it painted inside the window, in paint order.
pub fn painted(cx: &mut TestAppContext, window: AnyWindowHandle) -> Vec<String> {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        texts(window)
            .into_iter()
            .filter(|t| t.visible())
            .map(|t| t.text().to_string())
            .collect()
    })
    .expect("the window is open")
}

pub fn shows(cx: &mut TestAppContext, window: AnyWindowHandle, text: &str) -> bool {
    painted(cx, window).iter().any(|t| t == text)
}

pub fn count(fake: &FakeApi, method: &str, path: &str) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r.method == method && r.path == path)
        .count()
}

/// Scripts the dashboard's poll: a live stream, three events, no update.
pub fn answer_polls(fake: &FakeApi) {
    fake.reply("GET", "/api/status", Ok(status_json("live", false)));
    fake.reply("GET", "/api/events", Ok(events_json(3)));
    fake.reply("GET", "/api/update/status", Ok(update_json("idle")));
}

/// `GET /api/status` with the stream in `state`; the supervisor is gone when it is `stopped`.
pub fn status_json(state: &str, first_run: bool) -> Value {
    let supervisor = (state != "stopped").then(|| {
        json!({
            "playing": {
                "index": 2, "playlistId": "PLmain", "id": "vid-three", "title": "Third video",
                "source": "file", "position": 61.0, "duration": 3600.0
            },
            "encoder": { "alive": true, "speed": 1.0, "restarts": 0 },
            "youtube": { "breaker": "closed", "retryAt": null, "cookieProblem": null },
            "playlistCount": 10
        })
    });
    json!({
        "stream": { "state": state, "reason": null },
        "supervisor": supervisor,
        "launcher": if state == "stopped" { "stopped" } else { "running" },
        "twitch": { "enabled": false },
        "firstRun": first_run,
        "uptimeMs": 65_000
    })
}

/// `GET /api/config` as Node masks it: secrets as `********`, the key only as `streamKeySet`.
pub fn config_json() -> Value {
    json!({
        "playlists": [{ "id": "PLmain", "name": "Main" }, { "id": "PLsecond", "name": "Second" }],
        "port": 7654,
        "discord": {
            "webhookUrl": "********", "botName": "", "avatarUrl": "", "rolePing": "",
            "events": {
                "error": true, "skip": true, "recovery": true, "critical": true, "resume": true,
                "streamDrop": true, "streamRestart": true, "outputFrozen": true,
                "twitchMismatch": true, "twitchRestart": true
            },
            "templates": {}
        },
        "supervisor": {
            "ingestUrl": "rtmp://live.twitch.tv/app", "encoder": "h264_nvenc", "cacheBudgetMb": 300,
            "readAheadSeconds": 20, "playerClient": "mweb,web_embedded", "overlay": true,
            "freezeSeconds": 20, "silenceSeconds": 30, "streamKeySet": true
        },
        "autoUpdateCheck": true,
        "updateCheckIntervalMs": 21_600_000,
        "autoUpdateYtdlp": true,
        "autoOpenAdmin": true,
        "autoStartStream": true,
        "twitchClientId": "",
        "twitchClientSecret": "",
        "twitchChannel": "",
        "twitchLivenessEnabled": false,
        "twitchPollIntervalMs": 60_000,
        "ytdlCookiesFromBrowser": ""
    })
}

/// A setup check report: NVENC works, 4096 MB recommended, ffmpeg found and Deno missing (listed found first).
pub fn setup_report_json() -> Value {
    json!({
        "at": "2026-11-08T12:34:00.000Z",
        "encoder": { "chosen": "h264_nvenc", "tried": ["h264_nvenc"], "software": false, "works": true },
        "cache": { "freeMb": 120_000, "recommendedMb": 4096, "warning": null },
        "dependencies": [
            { "name": "ffmpeg", "ok": true, "path": r"C:\StreamLoop3\streamloop\ffmpeg\ffmpeg.exe" },
            { "name": "Deno", "ok": false, "path": r"C:\StreamLoop3\streamloop\yt-dlp\deno.exe" }
        ]
    })
}

/// `POST /api/config`'s answer after a reload that worked.
pub fn saved_json(restart_required: bool) -> Value {
    json!({
        "ok": true, "config": config_json(), "restartRequired": restart_required,
        "reloadError": null, "reloadErrorCode": null
    })
}

/// `GET /api/events`, oldest first, one second apart: "Event 1" to "Event n".
pub fn events_json(n: usize) -> Value {
    let events: Vec<Value> = (1..=n)
        .map(|i| json!({ "timestamp": format!("2026-11-08T12:{:02}:{:02}.000Z", i / 60, i % 60), "message": format!("Event {i}") }))
        .collect();
    Value::Array(events)
}

/// `GET /api/update/status`; any `status` but `idle` has 3.0.6 waiting, and `error` carries its message.
pub fn update_json(status: &str) -> Value {
    let available = status != "idle";
    json!({
        "currentVersion": "3.0.5",
        "latestVersion": if available { "3.0.6" } else { "3.0.5" },
        "updateAvailable": available,
        "status": status,
        "error": if status == "error" { Some("Checksum mismatch") } else { None },
        "isDevMode": false,
        "nodeNotice": null
    })
}

/// `GET /api/update/status` with 3.0.6 waiting, also in `idle`, the state after a check found it.
pub fn offered_json(status: &str) -> Value {
    let mut update = update_json(status);
    update["updateAvailable"] = json!(true);
    update["latestVersion"] = json!("3.0.6");
    update
}
