use std::cmp::Ordering;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use streamloop_common::api::{EncoderStatus, SupervisorStatus};
use streamloop_common::paths::Layout;
use streamloop_control::clock::TestClock;
use streamloop_control::http::{FakeHttp, Http, HttpError, HttpRequest, HttpResponse, Progress};
use streamloop_control::runner::{FakeRunner, RunError, RunErrorKind, RunOutput};
use streamloop_control::updater::{
    compare_versions, notice_text, parse_node_version, pick_release, select_exe_assets, should_land, BreakPolicy, DownloadCounter,
    GithubAsset, GithubRelease, NoticeState, UpdateStatus, Updater, UpdaterOptions, GITHUB_RELEASES_URL,
};

const CURRENT: &str = "3.1.0";
const NEXT: &str = "3.1.1";
const EXE_BYTES: &[u8] = b"the new streamloop.exe";
const ON_AIR_48: &str = "It installs by itself in about 48 hours, just before Twitch's 48-hour cut. Update Now installs it at once and restarts the stream.";
const OFF_AIR: &str = "It installs within a minute, since the stream is off air.";
const UPDATE_NOW: &str = "Update Now installs it and restarts the stream.";

fn asset(name: &str) -> GithubAsset {
    GithubAsset { name: name.into(), browser_download_url: format!("https://github.com/x/y/releases/download/v1/{name}") }
}

/// What a complete 3.1+ release carries: the full ZIP, the exe and the exe's checksum.
fn complete_assets(tag: &str) -> Vec<GithubAsset> {
    let v = tag.trim_start_matches('v');
    vec![asset(&format!("streamloop-v{v}.zip")), asset(&format!("streamloop-v{v}.exe")), asset(&format!("streamloop-v{v}.exe.sha256"))]
}

fn release_with(tag: &str, prerelease: bool, assets: Vec<GithubAsset>) -> GithubRelease {
    GithubRelease { tag_name: tag.into(), draft: false, prerelease, html_url: String::new(), assets }
}

fn stable(tag: &str) -> GithubRelease {
    release_with(tag, false, complete_assets(tag))
}

fn pre(tag: &str) -> GithubRelease {
    release_with(tag, true, complete_assets(tag))
}

fn picked<'a>(releases: &'a [GithubRelease], current: &str) -> Option<&'a str> {
    pick_release(releases, current).map(|r| r.tag_name.as_str())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn live(uptime_s: u64) -> SupervisorStatus {
    SupervisorStatus { encoder: Some(EncoderStatus { alive: Some(true), uptime_s: Some(Some(uptime_s)), ..EncoderStatus::default() }), ..SupervisorStatus::default() }
}

fn no_encoder() -> SupervisorStatus {
    SupervisorStatus { encoder: Some(EncoderStatus { alive: Some(false), uptime_s: Some(None), ..EncoderStatus::default() }), ..SupervisorStatus::default() }
}

// compare_versions

#[test]
fn compare_versions_finds_equal_versions_equal() {
    assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
}

#[test]
fn compare_versions_puts_a_higher_major_first() {
    assert_eq!(compare_versions("2.0.0", "1.0.0"), Ordering::Greater);
}

#[test]
fn compare_versions_puts_a_lower_major_below() {
    assert_eq!(compare_versions("1.0.0", "2.0.0"), Ordering::Less);
}

#[test]
fn compare_versions_compares_minor_versions() {
    assert_eq!(compare_versions("1.2.0", "1.1.0"), Ordering::Greater);
    assert_eq!(compare_versions("1.1.0", "1.2.0"), Ordering::Less);
}

#[test]
fn compare_versions_compares_patch_versions() {
    assert_eq!(compare_versions("1.0.2", "1.0.1"), Ordering::Greater);
    assert_eq!(compare_versions("1.0.1", "1.0.2"), Ordering::Less);
}

#[test]
fn compare_versions_ignores_a_leading_v() {
    assert_eq!(compare_versions("v1.2.0", "1.2.0"), Ordering::Equal);
    assert_eq!(compare_versions("v2.0.0", "v1.0.0"), Ordering::Greater);
}

#[test]
fn compare_versions_counts_a_missing_segment_as_zero() {
    assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
    assert_eq!(compare_versions("1.0.1", "1.0"), Ordering::Greater);
    assert_eq!(compare_versions("1.0", "1.0.1"), Ordering::Less);
}

#[test]
fn compare_versions_compares_numbers_not_text() {
    assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
    assert_eq!(compare_versions("1.2.10", "1.2.9"), Ordering::Greater);
}

#[test]
fn a_dev_build_sorts_below_its_release() {
    assert_eq!(compare_versions("v3.0.0", "3.0.0-dev"), Ordering::Greater);
    assert_eq!(compare_versions("3.0.0-dev", "3.0.0"), Ordering::Less);
    assert_eq!(compare_versions("3.0.0-dev", "3.0.0-dev"), Ordering::Equal);
    assert_eq!(compare_versions("3.0.1-dev", "3.0.0"), Ordering::Greater);
}

// parse_node_version

#[test]
fn parse_node_version_accepts_a_v_prefix_and_surrounding_whitespace() {
    assert_eq!(parse_node_version("  v22.23.4\r\n").as_deref(), Some("22.23.4"));
    assert_eq!(parse_node_version("22.23.4").as_deref(), Some("22.23.4"));
}

#[test]
fn parse_node_version_returns_none_for_anything_else() {
    assert_eq!(parse_node_version(""), None);
    assert_eq!(parse_node_version("node 22"), None);
    assert_eq!(parse_node_version("22.23"), None);
}

// pick_release

#[test]
fn picks_the_newest_stable_release_with_the_current_major() {
    // Arrange
    let releases = [stable("v3.0.0"), stable("v2.3.8"), stable("v2.3.7")];
    // Act
    let got = picked(&releases, "2.3.6");
    // Assert
    assert_eq!(got, Some("v2.3.8"));
}

#[test]
fn finds_a_2_x_hotfix_listed_after_a_newer_3_x_release() {
    let releases = [stable("v3.1.0"), stable("v3.0.0"), stable("v2.3.9")];
    assert_eq!(picked(&releases, "2.3.6"), Some("v2.3.9"));
}

#[test]
fn ignores_drafts_and_prereleases_on_2_x() {
    let mut draft = stable("v2.3.9");
    draft.draft = true;
    let releases = [pre("v2.4.0"), draft, stable("v2.3.7")];
    assert_eq!(picked(&releases, "2.3.6"), Some("v2.3.7"));
}

#[test]
fn ignores_tags_that_are_not_x_y_z() {
    let releases = [stable("spike-overlay"), stable("v3"), stable("2.4.0-beta"), stable("v2.3.7")];
    assert_eq!(picked(&releases, "2.3.6"), Some("v2.3.7"));
}

#[test]
fn picks_nothing_when_nothing_matches() {
    assert_eq!(picked(&[stable("v3.0.0")], "2.3.6"), None);
    assert_eq!(picked(&[], "2.3.6"), None);
}

#[test]
fn accepts_tags_with_and_without_a_leading_v() {
    let releases = [stable("2.3.8"), stable("v2.3.7")];
    assert_eq!(picked(&releases, "2.3.6"), Some("2.3.8"));
}

#[test]
fn offers_a_3_0_0_install_the_v3_0_1_prerelease() {
    let releases = [pre("v3.0.1"), pre("v3.0.0")];
    assert_eq!(picked(&releases, "3.0.0"), Some("v3.0.1"));
}

#[test]
fn offers_the_dev_build_the_v3_0_0_prerelease() {
    assert_eq!(picked(&[pre("v3.0.0")], "3.0.0-dev"), Some("v3.0.0"));
}

#[test]
fn never_offers_a_3_0_0_install_a_2_x_or_4_x_release() {
    let releases = [stable("v4.0.0"), pre("v4.0.0"), stable("v2.3.8")];
    assert_eq!(picked(&releases, "3.0.0"), None);
}

#[test]
fn never_offers_a_2_3_8_install_v3_0_1_prerelease_or_not() {
    assert_eq!(picked(&[pre("v3.0.1")], "2.3.8"), None);
    assert_eq!(picked(&[stable("v3.0.1")], "2.3.8"), None);
}

#[test]
fn skips_a_newer_release_whose_exe_or_full_zip_is_not_uploaded_yet() {
    // Arrange
    let releases = [
        release_with("v3.1.3", true, vec![asset("streamloop-v3.1.3.zip")]),
        release_with("v3.1.2", true, vec![asset("streamloop-v3.1.2.exe"), asset("streamloop-v3.1.2.exe.sha256")]),
        pre("v3.1.1"),
    ];
    // Act
    let got = picked(&releases, "3.1.0");
    // Assert
    assert_eq!(got, Some("v3.1.1"));
}

#[test]
fn skips_3_0_releases_without_an_exe() {
    // Arrange: how 3.0.x releases were uploaded
    let three_oh = |v: &str| {
        release_with(
            &format!("v{v}"),
            true,
            vec![
                asset(&format!("streamloop-v{v}.zip")),
                asset(&format!("streamloop-v{v}.sha256")),
                asset(&format!("streamloop-v{v}-update.zip")),
                asset(&format!("streamloop-v{v}-update.sha256")),
                asset("node-version.txt"),
            ],
        )
    };
    let releases = [three_oh("3.0.6"), three_oh("3.0.5")];
    // Act
    let got = picked(&releases, "3.0.5-dev");
    // Assert
    assert_eq!(got, None);
}

#[test]
fn skips_a_release_whose_exe_has_no_checksum() {
    // Arrange: v3.1.2 has the ZIP's checksum but none for its exe
    let releases = [
        release_with("v3.1.2", true, vec![asset("streamloop-v3.1.2.zip"), asset("streamloop-v3.1.2.zip.sha256"), asset("streamloop-v3.1.2.exe")]),
        pre("v3.1.1"),
    ];
    // Act
    let got = picked(&releases, "3.1.0");
    // Assert
    assert_eq!(got, Some("v3.1.1"));
}

#[test]
fn select_exe_assets_takes_the_exe_checksum_before_the_base_name() {
    // Arrange: the base name is also the full ZIP's checksum, so it only stands in for a missing .exe.sha256
    let both = release_with(
        "v3.1.1",
        true,
        vec![asset("streamloop-v3.1.1.sha256"), asset("streamloop-v3.1.1.exe"), asset("streamloop-v3.1.1.exe.sha256"), asset("streamloop-v3.1.1.zip")],
    );
    let base_only = release_with("v3.1.1", true, vec![asset("streamloop-v3.1.1.sha256"), asset("streamloop-v3.1.1.exe")]);
    // Act
    let from_both = select_exe_assets(&both).unwrap();
    let from_base = select_exe_assets(&base_only).unwrap();
    // Assert
    assert!(from_both.exe_url.ends_with("/streamloop-v3.1.1.exe"));
    assert!(from_both.checksum_url.ends_with("/streamloop-v3.1.1.exe.sha256"));
    assert!(from_base.checksum_url.ends_with("/streamloop-v3.1.1.sha256"));
}

/// The fixture's releases carry `-update.zip`; under the exe rule a release carries the exe and its checksum in its place.
fn with_exe_rule(releases: &Value) -> Vec<GithubRelease> {
    let mut releases = releases.clone();
    for r in releases.as_array_mut().unwrap() {
        let mut assets = Vec::new();
        for a in r["assets"].as_array().unwrap() {
            let name = a["name"].as_str().unwrap();
            let url = a["browser_download_url"].as_str().unwrap();
            match name.strip_suffix("-update.zip") {
                Some(base) => {
                    let url_base = url.strip_suffix("-update.zip").unwrap();
                    assets.push(json!({ "name": format!("{base}.exe"), "browser_download_url": format!("{url_base}.exe") }));
                    assets.push(json!({ "name": format!("{base}.exe.sha256"), "browser_download_url": format!("{url_base}.exe.sha256") }));
                }
                None => assets.push(a.clone()),
            }
        }
        r["assets"] = Value::Array(assets);
    }
    serde_json::from_value(releases).unwrap()
}

#[test]
fn matches_every_release_fixture() {
    // Arrange
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/releases.json")).unwrap();
    let mut checked = 0;
    let mut dropped = Vec::new();
    // Act and assert
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let input = &case["input"];
        let want = &case["output"];
        let got = match input["fn"].as_str().unwrap() {
            "compareVersions" => json!(compare_versions(input["a"].as_str().unwrap(), input["b"].as_str().unwrap()) as i8),
            "parseNodeVersion" => json!(parse_node_version(input["text"].as_str().unwrap())),
            "pickRelease" => {
                let releases = with_exe_rule(&input["releases"]);
                json!(pick_release(&releases, input["currentVersion"].as_str().unwrap()).map(|r| r.tag_name.clone()))
            }
            // The update-ZIP choice: R1 never downloads a ZIP.
            "selectReleaseAssets" => {
                dropped.push(name);
                continue;
            }
            other => panic!("{name}: unknown fn {other}"),
        };
        assert_eq!(&got, want, "{name}");
        checked += 1;
    }
    assert_eq!((checked, dropped.len()), (57, 13));
}

// DownloadCounter

#[test]
fn download_counter_reports_at_most_once_a_second_and_once_more_at_the_end() {
    // Arrange: ten chunks of 10 bytes, 300 ms apart
    let mut counter = DownloadCounter::new();
    let mut seen = Vec::new();
    // Act
    for i in 0..10u64 {
        let at = UNIX_EPOCH + Duration::from_millis(300 * i);
        seen.extend(counter.on_progress(10 * (i + 1), Some(100), at));
    }
    seen.extend(counter.finish());
    // Assert
    assert_eq!(seen, vec![10, 50, 90, 100]);
}

#[test]
fn download_counter_reports_nothing_without_a_content_length() {
    // Arrange
    let mut counter = DownloadCounter::new();
    // Act
    let first = counter.on_progress(10, None, UNIX_EPOCH);
    let second = counter.on_progress(20, None, UNIX_EPOCH + Duration::from_secs(2));
    let last = counter.finish();
    // Assert
    assert_eq!((first, second, last), (None, None, None));
}

// should_land and BreakPolicy

#[test]
fn lands_at_171900_s_of_encoder_uptime_and_not_before() {
    // Arrange
    let policy = BreakPolicy::default();
    // Act
    let before = should_land(&policy, true, Some(NEXT), None, Some(&live(171_899)), false, Duration::ZERO);
    let at = should_land(&policy, true, Some(NEXT), None, Some(&live(171_900)), false, Duration::ZERO);
    // Assert
    assert!(!before);
    assert!(at);
}

#[test]
fn lands_when_stopped_from_the_dashboard() {
    // Arrange
    let policy = BreakPolicy::default();
    // Act
    let stopped = should_land(&policy, true, Some(NEXT), None, None, true, Duration::ZERO);
    // Assert
    assert!(stopped);
}

#[test]
fn lands_after_300_s_with_no_live_encoder() {
    // Arrange
    let policy = BreakPolicy::default();
    // Act
    let down_299 = should_land(&policy, true, Some(NEXT), None, None, false, Duration::from_secs(299));
    let down_300 = should_land(&policy, true, Some(NEXT), None, None, false, Duration::from_secs(300));
    let no_key_300 = should_land(&policy, true, Some(NEXT), None, Some(&no_encoder()), false, Duration::from_secs(300));
    // Assert
    assert!(!down_299);
    assert!(down_300);
    assert!(no_key_300);
}

#[test]
fn never_lands_by_itself_with_auto_check_off() {
    // Arrange
    let policy = BreakPolicy::default();
    // Act
    let at_break = should_land(&policy, false, Some(NEXT), None, Some(&live(171_900)), false, Duration::ZERO);
    let stopped = should_land(&policy, false, Some(NEXT), None, None, true, Duration::from_secs(3600));
    // Assert
    assert!(!at_break);
    assert!(!stopped);
}

#[test]
fn does_not_land_a_rolled_back_version_by_itself() {
    // Arrange
    let policy = BreakPolicy::default();
    // Act
    let same = should_land(&policy, true, Some(NEXT), Some(NEXT), None, true, Duration::from_secs(3600));
    let newer = should_land(&policy, true, Some("3.1.2"), Some(NEXT), None, true, Duration::ZERO);
    // Assert
    assert!(!same);
    assert!(newer);
}

#[test]
fn nothing_staged_never_lands() {
    let policy = BreakPolicy::default();
    assert!(!should_land(&policy, true, None, None, None, true, Duration::from_secs(3600)));
}

#[test]
fn the_test_variable_moves_the_break() {
    // Arrange
    let policy = BreakPolicy::from_vars(|name| (name == "STREAMLOOP_TEST_BREAK_AT_S").then(|| "120".to_string()));
    // Act
    let before = should_land(&policy, true, Some(NEXT), None, Some(&live(119)), false, Duration::ZERO);
    let at = should_land(&policy, true, Some(NEXT), None, Some(&live(120)), false, Duration::ZERO);
    // Assert
    assert_eq!(policy.break_at_s, 120);
    assert!(!before);
    assert!(at);
}

#[test]
fn the_break_stays_at_171900_s_without_the_test_variable() {
    // Arrange
    let unset = BreakPolicy::from_vars(|_| None);
    let unreadable = BreakPolicy::from_vars(|_| Some("soon".to_string()));
    // Assert
    assert_eq!((unset.break_at_s, unset.off_air_after), (171_900, Duration::from_secs(300)));
    assert_eq!(unreadable, unset);
}

// notice_text

#[test]
fn notice_names_the_hours_left_before_twitchs_cut_when_staged_on_air() {
    assert_eq!(notice_text(NoticeState::OnAir, Some(48), None).as_deref(), Some(ON_AIR_48));
}

#[test]
fn notice_says_it_installs_within_a_minute_when_off_air() {
    assert_eq!(notice_text(NoticeState::OffAir, None, None).as_deref(), Some(OFF_AIR));
}

#[test]
fn notice_says_only_update_now_installs_it_with_auto_check_off() {
    assert_eq!(notice_text(NoticeState::UpdateNowOnly, None, None).as_deref(), Some(UPDATE_NOW));
}

#[test]
fn notice_says_why_the_download_failed() {
    // Act
    let text = notice_text(NoticeState::Failed("the checksum does not match.".into()), None, None);
    // Assert
    assert_eq!(text.as_deref(), Some("The download failed: the checksum does not match. It is tried again at the next check."));
}

#[test]
fn notice_adds_the_node_notice_after_a_space() {
    // Arrange
    let node = "This release bundles Node 24.1.0. Install the full ZIP to get it.".to_string();
    // Act
    let joined = notice_text(NoticeState::OffAir, None, Some(node.clone()));
    let alone = notice_text(NoticeState::Quiet, None, Some(node.clone()));
    // Assert
    assert_eq!(joined, Some(format!("{OFF_AIR} {node}")));
    assert_eq!(alone, Some(node));
}

#[test]
fn notice_without_known_hours_on_air_falls_back_to_update_now() {
    assert_eq!(notice_text(NoticeState::OnAir, None, None).as_deref(), Some(UPDATE_NOW));
}

#[test]
fn notice_is_empty_with_nothing_to_say() {
    assert_eq!(notice_text(NoticeState::Quiet, None, None), None);
}

// The Updater

struct Harness {
    _dir: tempfile::TempDir,
    http: Arc<FakeHttp>,
    runner: Arc<FakeRunner>,
    clock: Arc<TestClock>,
    layout: Layout,
}

fn exe_url(v: &str) -> String {
    format!("https://github.com/Sponsorn/streamloop/releases/download/v{v}/streamloop-v{v}.exe")
}

fn release_json(v: &str, names: &[String], base: &str) -> Value {
    json!({
        "tag_name": format!("v{v}"), "draft": false, "prerelease": true,
        "html_url": format!("https://github.com/Sponsorn/streamloop/releases/tag/v{v}"),
        "assets": names.iter().map(|n| json!({ "name": n, "browser_download_url": format!("{base}/v{v}/{n}") })).collect::<Vec<_>>(),
    })
}

fn complete_names(v: &str) -> Vec<String> {
    vec![format!("streamloop-v{v}.zip"), format!("streamloop-v{v}.exe"), format!("streamloop-v{v}.exe.sha256"), "node-version.txt".to_string()]
}

const GH_DOWNLOADS: &str = "https://github.com/Sponsorn/streamloop/releases/download";

impl Harness {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("sl3 Ö updater").tempdir().unwrap();
        let layout = Layout::new(dir.path());
        Self {
            _dir: dir,
            http: Arc::new(FakeHttp::new()),
            runner: Arc::new(FakeRunner::new()),
            clock: Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_792_929_600))),
            layout,
        }
    }

    /// GitHub lists `v` with a full ZIP, the exe and its checksum; the exe's bytes are `EXE_BYTES`.
    fn offer(&self, v: &str) {
        let list = json!([release_json(v, &complete_names(v), GH_DOWNLOADS)]).to_string();
        self.http.respond(GITHUB_RELEASES_URL, 200, &list);
        self.http.respond(&exe_url(v), 200, std::str::from_utf8(EXE_BYTES).unwrap());
        self.http.respond(&format!("{}.sha256", exe_url(v)), 200, &format!("{}  streamloop-v{v}.exe\n", sha256_hex(EXE_BYTES)));
    }

    fn node_version(&self, v: &str, text: &str) {
        self.http.respond(&format!("{GH_DOWNLOADS}/v{v}/node-version.txt"), 200, text);
    }

    fn options(&self, http: Arc<dyn Http>) -> UpdaterOptions {
        UpdaterOptions {
            current: CURRENT.into(),
            dev_mode: false,
            http,
            runner: self.runner.clone(),
            clock: self.clock.clone(),
            layout: self.layout.clone(),
            releases_url: GITHUB_RELEASES_URL.into(),
            test_releases: None,
            bundled_node: Some("22.23.4".into()),
            policy: BreakPolicy::default(),
        }
    }

    fn updater(&self) -> Updater {
        Updater::new(self.options(self.http.clone()))
    }

    fn github_calls(&self) -> usize {
        self.http.requests().iter().filter(|r| r.url.starts_with("https://api.github.com/")).count()
    }

    fn downloads(&self) -> usize {
        self.http.requests().iter().filter(|r| r.url.ends_with(".exe")).count()
    }

    fn past_cooldown(&self) {
        self.clock.advance(Duration::from_secs(61));
    }

    async fn staged(&self, v: &str) -> Updater {
        self.offer(v);
        self.runner.push(Ok(RunOutput::ok(&format!("{v}\r\n"))));
        let updater = self.updater();
        updater.check().await;
        updater.wait_for_stage().await;
        assert_eq!(updater.staged_version().as_deref(), Some(v), "the exe was staged");
        updater
    }
}

#[tokio::test]
async fn status_keeps_masters_eleven_fields_in_order() {
    // Arrange
    let h = Harness::new();
    let updater = h.updater();
    // Act
    let text = serde_json::to_string(&updater.status()).unwrap();
    // Assert
    assert_eq!(
        text,
        r#"{"currentVersion":"3.1.0","latestVersion":null,"updateAvailable":false,"status":"idle","error":null,"isDevMode":false,"nodeNotice":null,"kind":null,"parts":[],"sameVersion":false,"downloadPercent":null}"#
    );
}

#[tokio::test]
async fn stages_a_verified_exe() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.runner.push(Ok(RunOutput::ok("3.1.1\r\n")));
    let updater = h.updater();
    // Act
    let info = updater.check().await;
    updater.wait_for_stage().await;
    // Assert
    assert_eq!((info.status, info.update_available, info.latest_version.as_deref(), info.kind.as_deref()), (UpdateStatus::Idle, true, Some(NEXT), Some("app")));
    assert_eq!(std::fs::read(&h.layout.staged_exe).unwrap(), EXE_BYTES);
    let calls = h.runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(Path::new(&calls[0].program), h.layout.staged_exe.as_path());
    assert_eq!(calls[0].args, vec!["--version".to_string()]);
    assert_eq!(calls[0].timeout, Duration::from_secs(10));
    let after = updater.status();
    assert_eq!((after.status, after.download_percent, after.error), (UpdateStatus::Idle, None, None));
    assert_eq!(updater.staged_version().as_deref(), Some(NEXT));
}

#[tokio::test]
async fn refuses_a_checksum_mismatch_and_deletes_the_file() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.http.respond(&format!("{}.sha256", exe_url(NEXT)), 200, &format!("{}  streamloop-v{NEXT}.exe\n", "0".repeat(64)));
    let updater = h.updater();
    // Act
    updater.check().await;
    updater.wait_for_stage().await;
    // Assert
    assert!(!h.layout.staged_exe.exists());
    assert!(h.runner.calls().is_empty(), "a file that failed its checksum never runs");
    let info = updater.status();
    assert_eq!(info.status, UpdateStatus::Idle);
    assert_eq!(info.node_notice.as_deref(), Some("The download failed: the checksum does not match. It is tried again at the next check."));
    assert_eq!(updater.staged_version(), None);
}

#[tokio::test]
async fn refuses_an_exe_that_prints_another_version() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.runner.push(Ok(RunOutput::ok("3.1.0\r\n")));
    let updater = h.updater();
    // Act
    updater.check().await;
    updater.wait_for_stage().await;
    // Assert
    assert!(!h.layout.staged_exe.exists());
    assert_eq!(updater.status().status, UpdateStatus::Idle);
    assert_eq!(
        updater.status().node_notice.as_deref(),
        Some("The download failed: the downloaded exe reports version 3.1.0, not 3.1.1. It is tried again at the next check.")
    );
    assert_eq!(updater.staged_version(), None);
}

#[tokio::test]
async fn refuses_an_exe_that_does_not_answer_its_version_check() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.runner.push(Err(RunError { kind: RunErrorKind::Timeout, message: "timed out".into() }));
    let updater = h.updater();
    // Act
    updater.check().await;
    updater.wait_for_stage().await;
    // Assert
    assert!(!h.layout.staged_exe.exists());
    assert_eq!(
        updater.status().node_notice.as_deref(),
        Some("The download failed: the downloaded exe did not answer --version within 10 seconds. It is tried again at the next check.")
    );
}

#[tokio::test]
async fn a_failed_download_keeps_idle_with_the_notice() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.http.respond(&exe_url(NEXT), 404, "Not Found");
    let updater = h.updater();
    // Act
    updater.check().await;
    updater.wait_for_stage().await;
    // Assert
    let info = updater.status();
    assert_eq!((info.status, info.update_available, info.error.as_deref()), (UpdateStatus::Idle, true, None));
    assert_eq!(info.node_notice.as_deref(), Some("The download failed: HTTP 404. It is tried again at the next check."));
    assert!(!h.layout.staged_exe.exists());
}

#[tokio::test]
async fn the_next_check_tries_a_failed_download_again() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.http.respond(&exe_url(NEXT), 404, "Not Found");
    let updater = h.updater();
    updater.check().await;
    updater.wait_for_stage().await;
    h.offer(NEXT);
    h.runner.push(Ok(RunOutput::ok("3.1.1")));
    h.past_cooldown();
    // Act
    updater.check().await;
    updater.wait_for_stage().await;
    // Assert
    assert_eq!(h.downloads(), 2);
    assert_eq!(updater.staged_version().as_deref(), Some(NEXT));
    assert_eq!(updater.status().node_notice, None);
}

#[tokio::test]
async fn a_later_check_for_the_same_version_keeps_the_staged_exe() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    h.past_cooldown();
    // Act
    updater.check().await;
    updater.wait_for_stage().await;
    // Assert
    assert_eq!(h.downloads(), 1);
    assert_eq!(h.github_calls(), 2);
    assert_eq!(updater.staged_version().as_deref(), Some(NEXT));
}

#[tokio::test]
async fn dev_mode_checks_but_never_stages() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    let mut opts = h.options(h.http.clone());
    opts.dev_mode = true;
    let updater = Updater::new(opts);
    // Act
    let info = updater.check().await;
    updater.wait_for_stage().await;
    let stage = updater.stage().await;
    // Assert
    assert_eq!((info.update_available, info.is_dev_mode, info.node_notice), (true, true, None));
    assert_eq!(stage, Err("Cannot apply updates in dev mode".to_string()));
    assert_eq!(h.downloads(), 0);
    assert!(!h.layout.staged_exe.exists());
}

#[tokio::test]
async fn a_release_whose_exe_has_no_checksum_is_never_downloaded() {
    // Arrange
    let h = Harness::new();
    let names = vec![format!("streamloop-v{NEXT}.zip"), format!("streamloop-v{NEXT}.zip.sha256"), format!("streamloop-v{NEXT}.exe")];
    h.http.respond(GITHUB_RELEASES_URL, 200, &json!([release_json(NEXT, &names, GH_DOWNLOADS)]).to_string());
    let updater = h.updater();
    // Act
    let info = updater.check().await;
    updater.wait_for_stage().await;
    let stage = updater.stage().await;
    // Assert
    assert_eq!((info.update_available, info.latest_version.as_deref()), (false, Some(CURRENT)));
    assert_eq!(stage, Err("No update available to apply".to_string()));
    assert_eq!(h.http.requests().len(), 1);
}

// Node notice

#[tokio::test]
async fn node_notice_compares_with_the_bundled_node() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.node_version(NEXT, "v24.1.0\n");
    let updater = h.updater();
    // Act
    let info = updater.check().await;
    // Assert
    assert!(info.update_available);
    assert_eq!(info.node_notice.as_deref(), Some("This release bundles Node 24.1.0. Install the full ZIP to get it."));
}

#[tokio::test]
async fn leaves_the_node_notice_empty_when_the_node_versions_match() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.node_version(NEXT, "v22.23.4\r\n");
    // Act
    let info = h.updater().check().await;
    // Assert
    assert_eq!(info.node_notice, None);
}

#[tokio::test]
async fn leaves_the_node_notice_empty_when_node_version_txt_is_missing_or_malformed() {
    // Arrange
    let missing = Harness::new();
    missing.offer(NEXT);
    let malformed = Harness::new();
    malformed.offer(NEXT);
    malformed.node_version(NEXT, "latest");
    // Act
    let a = missing.updater().check().await;
    let b = malformed.updater().check().await;
    // Assert
    assert_eq!((a.node_notice, b.node_notice), (None, None));
}

#[tokio::test]
async fn leaves_the_node_notice_empty_in_dev_mode() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.node_version(NEXT, "v24.1.0");
    let mut opts = h.options(h.http.clone());
    opts.dev_mode = true;
    // Act
    let info = Updater::new(opts).check().await;
    // Assert
    assert_eq!(info.node_notice, None);
}

// The notice through status()

#[tokio::test]
async fn status_names_the_hours_before_twitchs_cut_for_a_staged_exe_on_air() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    // Act
    let due = updater.observe(Some(&live(0)), false, true);
    // Assert
    assert!(!due);
    assert_eq!(updater.status().node_notice.as_deref(), Some(ON_AIR_48));
}

#[tokio::test]
async fn status_counts_down_the_hours_with_the_encoder_uptime() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    // Act
    updater.observe(Some(&live(171_900 - 5 * 3600)), false, true);
    // Assert
    assert_eq!(
        updater.status().node_notice.as_deref(),
        Some("It installs by itself in about 5 hours, just before Twitch's 48-hour cut. Update Now installs it at once and restarts the stream.")
    );
}

#[tokio::test]
async fn status_says_within_a_minute_when_stopped() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    // Act
    let due = updater.observe(None, true, true);
    // Assert
    assert!(due);
    assert_eq!(updater.status().node_notice.as_deref(), Some(OFF_AIR));
}

#[tokio::test]
async fn status_says_only_update_now_installs_it_with_auto_check_off() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    // Act
    let due = updater.observe(Some(&live(171_900)), false, false);
    // Assert
    assert!(!due);
    assert_eq!(updater.status().node_notice.as_deref(), Some(UPDATE_NOW));
}

#[tokio::test]
async fn status_says_only_update_now_installs_a_version_that_rolled_back() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    std::fs::create_dir_all(&h.layout.data).unwrap();
    std::fs::write(&h.layout.rolled_back, json!({ "from": CURRENT, "to": NEXT, "on_air": true }).to_string()).unwrap();
    // Act
    let due = updater.observe(None, true, true);
    // Assert
    assert!(!due);
    assert_eq!(updater.status().node_notice.as_deref(), Some(UPDATE_NOW));
}

#[tokio::test]
async fn observe_lands_once_the_encoder_has_been_down_for_300_s() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    updater.observe(Some(&live(100)), false, true);
    // Act
    let at_start = updater.observe(None, false, true);
    h.clock.advance(Duration::from_secs(299));
    let at_299 = updater.observe(Some(&no_encoder()), false, true);
    h.clock.advance(Duration::from_secs(1));
    let at_300 = updater.observe(None, false, true);
    // Assert
    assert_eq!((at_start, at_299, at_300), (false, false, true));
}

#[tokio::test]
async fn a_live_encoder_resets_the_off_air_time() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    updater.observe(None, false, true);
    h.clock.advance(Duration::from_secs(200));
    updater.observe(Some(&live(5)), false, true);
    h.clock.advance(Duration::from_secs(1));
    updater.observe(None, false, true);
    // Act
    h.clock.advance(Duration::from_secs(200));
    let due = updater.observe(None, false, true);
    // Assert
    assert!(!due);
}

#[tokio::test]
async fn the_node_notice_follows_the_landing_notice() {
    // Arrange
    let h = Harness::new();
    h.node_version(NEXT, "v24.1.0");
    let updater = h.staged(NEXT).await;
    // Act
    updater.observe(None, true, true);
    // Assert
    assert_eq!(
        updater.status().node_notice,
        Some(format!("{OFF_AIR} This release bundles Node 24.1.0. Install the full ZIP to get it."))
    );
}

// GitHub timeouts

/// Records each request, answers the release list from `inner`, and never answers anything else.
struct StallingHttp {
    inner: FakeHttp,
    stall_list: bool,
    timeouts: Mutex<Vec<(String, Option<Duration>)>>,
}

#[async_trait]
impl Http for StallingHttp {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.timeouts.lock().unwrap().push((req.url.clone(), req.timeout));
        if req.url.starts_with(GITHUB_RELEASES_URL) && !self.stall_list {
            return self.inner.send(req).await;
        }
        std::future::pending().await
    }

    async fn download(&self, _url: &str, _dest: &Path) -> Result<u64, HttpError> {
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn gives_up_on_a_stalled_node_version_txt_and_shows_no_notice() {
    // Arrange
    let h = Harness::new();
    let inner = FakeHttp::new();
    inner.respond(GITHUB_RELEASES_URL, 200, &json!([release_json(NEXT, &complete_names(NEXT), GH_DOWNLOADS)]).to_string());
    let http = Arc::new(StallingHttp { inner, stall_list: false, timeouts: Mutex::new(Vec::new()) });
    let updater = Updater::new(h.options(http.clone()));
    // Act
    let info = updater.check().await;
    // Assert
    assert_eq!((info.status, info.update_available, info.node_notice), (UpdateStatus::Idle, true, None));
    let timeouts = http.timeouts.lock().unwrap().clone();
    assert!(timeouts.iter().any(|(url, t)| url.ends_with("node-version.txt") && *t == Some(Duration::from_secs(10))));
}

#[tokio::test(start_paused = true)]
async fn shows_a_plain_error_when_the_github_api_stalls() {
    // Arrange
    let h = Harness::new();
    let http = Arc::new(StallingHttp { inner: FakeHttp::new(), stall_list: true, timeouts: Mutex::new(Vec::new()) });
    let updater = Updater::new(h.options(http.clone()));
    // Act
    let info = updater.check().await;
    // Assert
    assert_eq!(info.status, UpdateStatus::Error);
    assert_eq!(info.error.as_deref(), Some("GitHub did not answer within 10 seconds."));
    assert_eq!(http.timeouts.lock().unwrap()[0].1, Some(Duration::from_secs(10)));
}

// Checks beside a check or a download

/// Answers from `inner`, but holds a gated release-list request or exe download until `open`.
struct GatedHttp {
    inner: Arc<FakeHttp>,
    gate_list: Mutex<bool>,
    gate_download: Mutex<bool>,
    open: tokio::sync::Notify,
}

impl GatedHttp {
    fn new(inner: Arc<FakeHttp>) -> Self {
        Self { inner, gate_list: Mutex::new(false), gate_download: Mutex::new(false), open: tokio::sync::Notify::new() }
    }

    async fn wait_open(&self) {
        let opened = self.open.notified();
        opened.await;
    }
}

#[async_trait]
impl Http for GatedHttp {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let gated = req.url.starts_with(GITHUB_RELEASES_URL) && std::mem::take(&mut *self.gate_list.lock().unwrap());
        if gated {
            self.wait_open().await;
        }
        self.inner.send(req).await
    }

    async fn download(&self, url: &str, dest: &Path) -> Result<u64, HttpError> {
        self.inner.download(url, dest).await
    }

    async fn download_with_progress(&self, url: &str, dest: &Path, progress: &Progress<'_>) -> Result<u64, HttpError> {
        if std::mem::take(&mut *self.gate_download.lock().unwrap()) {
            progress(EXE_BYTES.len() as u64 / 2, Some(EXE_BYTES.len() as u64));
            self.wait_open().await;
        }
        self.inner.download_with_progress(url, dest, progress).await
    }
}

/// Polls every 5 ms for up to 10 s; the stage's file work runs on tokio's blocking threads, so yielding alone is not enough.
async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the condition never came true");
}

#[tokio::test]
async fn refuses_to_stage_while_a_check_runs_and_the_check_forgets_the_last_offer() {
    // Arrange: a first check stages the update; a second one, past the cooldown, waits on GitHub
    let h = Harness::new();
    h.offer(NEXT);
    h.runner.push(Ok(RunOutput::ok(NEXT)));
    let http = Arc::new(GatedHttp::new(h.http.clone()));
    let updater = Updater::new(h.options(http.clone()));
    updater.check().await;
    updater.wait_for_stage().await;
    *http.gate_list.lock().unwrap() = true;
    h.past_cooldown();
    // Act
    let checking = tokio::spawn({
        let updater = updater.clone();
        async move { updater.check().await }
    });
    until(|| updater.status().status == UpdateStatus::Checking).await;
    let during = updater.status();
    let staged_during = updater.staged_version();
    let stage = updater.stage().await;
    http.open.notify_waiters();
    let after = checking.await.unwrap();
    // Assert
    assert_eq!((during.status, during.update_available), (UpdateStatus::Checking, false));
    assert_eq!(staged_during, None);
    assert_eq!(stage, Err("Update already in progress".to_string()));
    assert_eq!((after.status, after.update_available), (UpdateStatus::Idle, true));
    assert_eq!(updater.staged_version().as_deref(), Some(NEXT));
}

#[tokio::test]
async fn offers_nothing_to_stage_after_a_check_that_failed() {
    // Arrange
    let h = Harness::new();
    let updater = h.staged(NEXT).await;
    h.http.respond(GITHUB_RELEASES_URL, 500, "");
    h.past_cooldown();
    // Act
    let failed = updater.check().await;
    let stage = updater.stage().await;
    // Assert
    assert_eq!((failed.status, failed.update_available), (UpdateStatus::Error, false));
    assert_eq!(failed.error.as_deref(), Some("GitHub API returned 500: Internal Server Error"));
    assert_eq!(stage, Err("No update available to apply".to_string()));
    assert_eq!(updater.staged_version(), None);
}

#[tokio::test]
async fn a_check_while_the_exe_downloads_returns_the_current_status() {
    // Arrange
    let h = Harness::new();
    h.offer(NEXT);
    h.runner.push(Ok(RunOutput::ok(NEXT)));
    let http = Arc::new(GatedHttp::new(h.http.clone()));
    *http.gate_download.lock().unwrap() = true;
    let updater = Updater::new(h.options(http.clone()));
    updater.check().await;
    until(|| updater.status().download_percent.is_some()).await;
    h.past_cooldown();
    // Act
    let during = updater.check().await;
    // Assert
    assert_eq!((during.status, during.update_available), (UpdateStatus::Downloading, true));
    assert_eq!(h.github_calls(), 1);
    http.open.notify_waiters();
    updater.wait_for_stage().await;
    assert_eq!(updater.staged_version().as_deref(), Some(NEXT));
}

#[tokio::test]
async fn shows_the_download_percentage_while_the_exe_downloads() {
    // Arrange: the exe arrives in two halves, the second once the test has looked
    let h = Harness::new();
    h.offer(NEXT);
    h.runner.push(Ok(RunOutput::ok(NEXT)));
    let http = Arc::new(GatedHttp::new(h.http.clone()));
    *http.gate_download.lock().unwrap() = true;
    let updater = Updater::new(h.options(http.clone()));
    // Act
    updater.check().await;
    until(|| updater.status().download_percent.is_some()).await;
    let during = updater.status();
    http.open.notify_waiters();
    updater.wait_for_stage().await;
    let after = updater.status();
    // Assert
    assert_eq!((during.status, during.download_percent), (UpdateStatus::Downloading, Some(50)));
    assert_eq!((after.status, after.download_percent), (UpdateStatus::Idle, None));
}

// The test release list

#[tokio::test]
async fn reads_releases_from_the_test_file_and_allows_only_local_asset_urls_while_it_is_set() {
    // Arrange
    let h = Harness::new();
    let file = h.layout.root.join("releases.json");
    let local = |base: &str| json!([release_json(NEXT, &complete_names(NEXT), base)]).to_string();
    h.http.respond(GITHUB_RELEASES_URL, 200, &local("http://127.0.0.1:8791"));
    let check = |test_releases: Option<std::path::PathBuf>| {
        let mut opts = h.options(h.http.clone());
        opts.test_releases = test_releases;
        opts.dev_mode = true;
        async move { Updater::new(opts).check().await }
    };
    // Act
    std::fs::write(&file, local("http://127.0.0.1:8791")).unwrap();
    let from_file = check(Some(file.clone())).await;
    std::fs::write(&file, local(GH_DOWNLOADS)).unwrap();
    let github_in_file = check(Some(file.clone())).await;
    let github_calls = h.github_calls();
    let local_from_github = check(None).await;
    // Assert
    assert_eq!((from_file.status, from_file.update_available, from_file.latest_version.as_deref()), (UpdateStatus::Idle, true, Some(NEXT)));
    assert_eq!(from_file.kind.as_deref(), Some("app"));
    assert_eq!(github_in_file.status, UpdateStatus::Error);
    assert!(github_in_file.error.unwrap().starts_with("Untrusted download URL domain: https://github.com/"));
    assert_eq!(github_calls, 0);
    assert_eq!(local_from_github.status, UpdateStatus::Error);
    assert!(local_from_github.error.unwrap().starts_with("Untrusted download URL domain: http://127.0.0.1:8791/"));
}

#[tokio::test]
async fn takes_downloads_only_from_github_over_https() {
    // Arrange
    let h = Harness::new();
    let list = |base: &str| json!([release_json(NEXT, &complete_names(NEXT), base)]).to_string();
    let check = |body: String| {
        h.http.respond(GITHUB_RELEASES_URL, 200, &body);
        let mut opts = h.options(h.http.clone());
        opts.dev_mode = true;
        async move { Updater::new(opts).check().await }
    };
    // Act
    let github = check(list(GH_DOWNLOADS)).await;
    let objects = check(list("https://objects.githubusercontent.com/x")).await;
    let plain = check(list("http://github.com/Sponsorn/streamloop/releases/download")).await;
    let other = check(list("https://example.com/x")).await;
    // Assert
    assert_eq!((github.status, objects.status), (UpdateStatus::Idle, UpdateStatus::Idle));
    assert_eq!((plain.status, other.status), (UpdateStatus::Error, UpdateStatus::Error));
}
