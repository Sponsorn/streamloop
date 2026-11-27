//! Playlist listings for the dashboard, fetched with yt-dlp and cached.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use streamloop_common::playlist::{parse_playlist, PRINT_TEMPLATE};
use tokio::sync::watch;

use crate::clock::Clock;
use crate::runner::{RunSpec, Runner};
use crate::ytdlp_update::run_checked;

const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// A failed fetch is not retried for 5 min, so the dashboard cannot hammer YouTube outside the supervisor's gate.
const FAILURE_TTL: Duration = Duration::from_secs(5 * 60);
const YTDLP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaylistVideo {
    pub index: usize,
    pub id: String,
    pub title: String,
    /// Seconds; 0 when yt-dlp does not know it.
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistMetadata {
    pub playlist_id: String,
    pub videos: Vec<PlaylistVideo>,
    /// Milliseconds since the Unix epoch.
    pub fetched_at: u64,
}

pub fn build_argv(id: &str, cookies: &str) -> Vec<String> {
    let mut args = Vec::new();
    if !cookies.is_empty() {
        args.push("--cookies-from-browser".to_string());
        args.push(cookies.to_string());
    }
    args.extend(["--flat-playlist", "--print", PRINT_TEMPLATE, "--no-warnings"].map(String::from));
    args.push(format!("https://www.youtube.com/playlist?list={id}"));
    args
}

/// `parse_playlist` is the supervisor's own function and drops private and deleted entries,
/// so the indexes here are the supervisor's.
fn parse_videos(out: &str) -> Vec<PlaylistVideo> {
    parse_playlist(out)
        .into_iter()
        .enumerate()
        .map(|(index, e)| PlaylistVideo { index, id: e.id, title: e.title, duration: e.duration.unwrap_or(0.0) })
        .collect()
}

type Outcome = Option<Result<PlaylistMetadata, String>>;

#[derive(Default)]
struct State {
    cache: HashMap<String, (PlaylistMetadata, SystemTime)>,
    failures: HashMap<String, (String, SystemTime)>,
    in_flight: HashMap<String, watch::Receiver<Outcome>>,
}

/// Removes the in-flight entry however the leading fetch ends, so a dropped call cannot wedge an id.
struct InFlightGuard<'a> {
    state: &'a Mutex<State>,
    id: &'a str,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.state.lock().unwrap().in_flight.remove(self.id);
    }
}

pub struct PlaylistCache {
    runner: Arc<dyn Runner>,
    ytdlp: PathBuf,
    cookies: String,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl PlaylistCache {
    pub fn new(runner: Arc<dyn Runner>, ytdlp: PathBuf, cookies: String, clock: Arc<dyn Clock>) -> Self {
        Self { runner, ytdlp, cookies, clock, state: Mutex::new(State::default()) }
    }

    pub async fn fetch(&self, id: &str) -> Result<PlaylistMetadata, String> {
        let now = self.clock.now();
        // The lock ends with this block: a guard held across an await would make the fetch unsendable.
        let in_flight = {
            let mut st = self.state.lock().unwrap();
            if let Some((meta, at)) = st.cache.get(id) {
                if now.duration_since(*at).unwrap_or_default() < CACHE_TTL {
                    return Ok(meta.clone());
                }
            }
            if let Some((err, at)) = st.failures.get(id) {
                if now.duration_since(*at).unwrap_or_default() < FAILURE_TTL {
                    return Err(err.clone());
                }
            }
            match st.in_flight.get(id) {
                Some(rx) => Err(rx.clone()),
                None => {
                    let (tx, rx) = watch::channel(None);
                    st.in_flight.insert(id.to_string(), rx);
                    Ok(tx)
                }
            }
        };
        let sender = match in_flight {
            Ok(tx) => tx,
            Err(mut rx) => {
                return match rx.wait_for(|v| v.is_some()).await {
                    Ok(v) => v.clone().unwrap(),
                    Err(_) => Err("the playlist fetch was cancelled".to_string()),
                };
            }
        };

        let guard = InFlightGuard { state: &self.state, id };
        let result = self.run(id).await;
        {
            let mut st = self.state.lock().unwrap();
            let at = self.clock.now();
            match &result {
                Ok(meta) => {
                    st.cache.insert(id.to_string(), (meta.clone(), at));
                }
                Err(e) => {
                    st.failures.insert(id.to_string(), (e.clone(), at));
                }
            }
        }
        drop(guard);
        let _ = sender.send(Some(result.clone()));
        result
    }

    /// Drops cached listings and failures, so a yt-dlp update retries at once.
    pub fn clear(&self) {
        let mut st = self.state.lock().unwrap();
        st.cache.clear();
        st.failures.clear();
    }

    async fn run(&self, id: &str) -> Result<PlaylistMetadata, String> {
        tracing::info!(playlist_id = id, "Fetching playlist metadata via yt-dlp");
        let spec = RunSpec {
            program: streamloop_common::paths::ytdlp_to_run(&self.ytdlp).to_string_lossy().into_owned(),
            args: build_argv(id, &self.cookies),
            stdin: None,
            timeout: YTDLP_TIMEOUT,
            env: Vec::new(),
        };
        let out = run_checked(self.runner.as_ref(), spec).await?;
        let videos = parse_videos(&out.stdout_text());
        let fetched_at = self.clock.now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        tracing::info!(playlist_id = id, video_count = videos.len(), "Playlist metadata cached");
        Ok(PlaylistMetadata { playlist_id: id.to_string(), videos, fetched_at })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use crate::runner::{FakeRunner, RunError, RunErrorKind, RunOutput};

    fn bot_check() -> Result<RunOutput, RunError> {
        Err(RunError { kind: RunErrorKind::Wait, message: "Sign in to confirm you are not a bot".into() })
    }

    /// Yields before answering, so a second caller arrives while the first run is still in flight.
    struct SlowRunner(Arc<FakeRunner>);

    #[async_trait::async_trait]
    impl Runner for SlowRunner {
        async fn run(&self, spec: RunSpec) -> Result<RunOutput, RunError> {
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.0.run(spec).await
        }
    }

    fn cache_with(runner: &Arc<FakeRunner>, cookies: &str) -> (PlaylistCache, Arc<TestClock>) {
        let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_000_000)));
        let cache = PlaylistCache::new(runner.clone(), PathBuf::from("/tmp/yt-dlp"), cookies.into(), clock.clone());
        (cache, clock)
    }

    #[test]
    fn a_fetch_can_run_on_another_thread() {
        // Arrange
        fn sendable<T: Send>(_: T) {}
        let (cache, _clock) = cache_with(&Arc::new(FakeRunner::new()), "");
        // Act + Assert: compiles only while the fetch holds no lock across an await, as a request handler needs.
        sendable(cache.fetch("PLx"));
    }

    #[test]
    fn parses_lines_into_videos_with_consecutive_indexes() {
        // Act
        let v = parse_videos("abc123\tVideo One\t120\ndef456\tVideo Two\t300\nghi789\tVideo Three\t60\n");
        // Assert
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], PlaylistVideo { index: 0, id: "abc123".into(), title: "Video One".into(), duration: 120.0 });
        assert_eq!(v[2], PlaylistVideo { index: 2, id: "ghi789".into(), title: "Video Three".into(), duration: 60.0 });
    }

    #[test]
    fn missing_or_unknown_fields_become_empty_title_and_zero_duration() {
        // Act
        let v = parse_videos("abc123\ndef456\tNA\tNA\nghi789\tNo Duration\n");
        // Assert
        assert_eq!(v[0], PlaylistVideo { index: 0, id: "abc123".into(), title: String::new(), duration: 0.0 });
        assert_eq!(v[1].duration, 0.0);
        assert_eq!(v[2].duration, 0.0);
    }

    #[test]
    fn skips_blank_lines_and_lines_without_an_id() {
        // Act
        let v = parse_videos("abc123\tValid One\t60\n\n   \n\tNo ID\t90\ndef456\tValid Two\t120\n");
        // Assert
        assert_eq!(v.iter().map(|x| (x.index, x.id.as_str())).collect::<Vec<_>>(), [(0, "abc123"), (1, "def456")]);
    }

    #[test]
    fn drops_private_and_deleted_entries_before_indexing_like_the_supervisor() {
        // Act
        let v = parse_videos("aaa\tOne\t60\nppp\t[Private video]\tNA\nddd\t[Deleted video]\tNA\nbbb\tTwo\t90\n");
        // Assert
        assert_eq!(v.iter().map(|x| (x.index, x.id.as_str())).collect::<Vec<_>>(), [(0, "aaa"), (1, "bbb")]);
    }

    #[test]
    fn empty_or_whitespace_only_output_gives_no_videos() {
        // Assert
        for out in ["", "   ", "\n\n\n", "  \n  \n  "] {
            assert!(parse_videos(out).is_empty(), "{out:?}");
        }
    }

    #[tokio::test]
    async fn a_fetch_runs_the_newest_updated_yt_dlp() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        for file in ["yt-dlp.exe", "yt-dlp-2026.11.24.exe"] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        let runner = Arc::new(FakeRunner::new());
        runner.push(Ok(RunOutput::ok("abc123\tVideo One\t120\n")));
        let clock = Arc::new(TestClock::new(UNIX_EPOCH + Duration::from_secs(1_000_000)));
        let cache = PlaylistCache::new(runner.clone(), dir.path().join("yt-dlp.exe"), String::new(), clock);
        // Act
        cache.fetch("PLx").await.unwrap();
        // Assert
        assert_eq!(runner.calls()[0].program, dir.path().join("yt-dlp-2026.11.24.exe").to_string_lossy());
    }

    #[test]
    fn argv_omits_the_cookies_flag_when_not_configured() {
        // Act
        let argv = build_argv("PL123", "");
        // Assert
        assert_eq!(argv, ["--flat-playlist", "--print", PRINT_TEMPLATE, "--no-warnings", "https://www.youtube.com/playlist?list=PL123"]);
    }

    #[test]
    fn argv_puts_the_cookies_flag_first_when_configured() {
        // Act
        let argv = build_argv("PL123", "brave");
        // Assert
        assert_eq!(&argv[..3], ["--cookies-from-browser", "brave", "--flat-playlist"]);
    }

    #[test]
    fn argv_passes_a_cookies_profile_spec_as_one_argument() {
        // Act
        let argv = build_argv("PL123", "chrome:Profile 2");
        // Assert
        assert_eq!(argv[1], "chrome:Profile 2");
    }

    #[tokio::test]
    async fn returns_the_cached_error_for_5_min_without_running_yt_dlp_again() {
        // Arrange
        let runner = Arc::new(FakeRunner::new());
        runner.push(bot_check());
        let (cache, clock) = cache_with(&runner, "");
        assert!(cache.fetch("PL123").await.unwrap_err().contains("bot"));
        // Act
        clock.advance(Duration::from_secs(4 * 60));
        let again = cache.fetch("PL123").await;
        // Assert
        assert!(again.unwrap_err().contains("bot"));
        assert_eq!(runner.calls().len(), 1);
    }

    #[tokio::test]
    async fn runs_yt_dlp_again_after_5_min_and_per_playlist_id() {
        // Arrange
        let runner = Arc::new(FakeRunner::new());
        for _ in 0..3 {
            runner.push(bot_check());
        }
        let (cache, clock) = cache_with(&runner, "");
        assert!(cache.fetch("PL123").await.is_err());
        // Act
        assert!(cache.fetch("PL999").await.is_err());
        clock.advance(Duration::from_secs(5 * 60));
        assert!(cache.fetch("PL123").await.is_err());
        // Assert
        assert_eq!(runner.calls().len(), 3);
    }

    #[tokio::test]
    async fn clear_drops_cached_failures_so_a_yt_dlp_update_retries_at_once() {
        // Arrange
        let runner = Arc::new(FakeRunner::new());
        runner.push(bot_check());
        runner.push(bot_check());
        let (cache, _clock) = cache_with(&runner, "");
        assert!(cache.fetch("PL123").await.is_err());
        // Act
        cache.clear();
        assert!(cache.fetch("PL123").await.is_err());
        // Assert
        assert_eq!(runner.calls().len(), 2);
    }

    #[tokio::test]
    async fn caches_a_listing_for_an_hour_then_fetches_again() {
        // Arrange
        let runner = Arc::new(FakeRunner::new());
        runner.push(Ok(RunOutput::ok("a\tOne\t10\n")));
        runner.push(Ok(RunOutput::ok("a\tOne\t10\nb\tTwo\t20\n")));
        let (cache, clock) = cache_with(&runner, "brave");
        // Act
        let first = cache.fetch("PL1").await.unwrap();
        clock.advance(Duration::from_secs(59 * 60));
        let cached = cache.fetch("PL1").await.unwrap();
        clock.advance(Duration::from_secs(2 * 60));
        let refreshed = cache.fetch("PL1").await.unwrap();
        // Assert
        assert_eq!((first.videos.len(), cached.videos.len(), refreshed.videos.len()), (1, 1, 2));
        assert_eq!(runner.calls().len(), 2);
        assert_eq!(runner.calls()[0].timeout, Duration::from_secs(120));
        assert_eq!(first.fetched_at, 1_000_000_000);
    }

    #[tokio::test]
    async fn concurrent_fetches_of_one_id_share_a_single_run() {
        // Arrange
        let runner = Arc::new(FakeRunner::new());
        runner.push(Ok(RunOutput::ok("a\tOne\t10\n")));
        let clock = Arc::new(TestClock::new(UNIX_EPOCH));
        let cache = PlaylistCache::new(Arc::new(SlowRunner(runner.clone())), PathBuf::from("/tmp/yt-dlp"), String::new(), clock);
        // Act
        let (a, b) = tokio::join!(cache.fetch("PL1"), cache.fetch("PL1"));
        // Assert
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(runner.calls().len(), 1);
    }
}
