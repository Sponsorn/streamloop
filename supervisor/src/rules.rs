//! Pure rules: no I/O, no clocks. Everything the control loop decides is here and tested.

/// 90 kHz MPEG-TS ticks. Timeline offsets are kept in ticks so continuing offsets are exact.
pub const HZ: i64 = 90_000;
pub const FRAME: i64 = HZ / 30;
pub const BASE: i64 = 10 * HZ;
const WRAP: i64 = 1 << 33;

pub fn secs(ticks: i64) -> f64 {
    ticks as f64 / HZ as f64
}

/// Share of the whole machine a process used over a window; 100 = every core busy.
pub fn cpu_percent(proc_delta: u64, total_delta: u64) -> f64 {
    if total_delta == 0 {
        return 0.0;
    }
    (proc_delta as f64 / total_delta as f64 * 100.0).min(100.0)
}

/// CPU share of one process over a window, 0 when the sample pair is not the same process.
pub fn window_cpu(prev_pid: Option<u32>, pid: Option<u32>, prev_time: u64, time: u64, total_delta: u64) -> f64 {
    match (prev_pid, pid) {
        (Some(a), Some(b)) if a == b => cpu_percent(time.saturating_sub(prev_time), total_delta),
        _ => 0.0,
    }
}

pub fn ticks(secs: f64) -> i64 {
    (secs * HZ as f64).round() as i64
}


pub const PKT: usize = 188;

#[derive(Debug, Default, PartialEq)]
pub struct Packet {
    pub pid: u16,
    pub pusi: bool,
    /// PES stream id when this packet starts a PES.
    pub stream_id: Option<u8>,
    /// Raw 33-bit PTS when this packet starts a PES that carries one.
    pub pts: Option<i64>,
}

pub fn parse_packet(p: &[u8]) -> Option<Packet> {
    if p.len() < PKT || p[0] != 0x47 {
        return None;
    }
    let pusi = p[1] & 0x40 != 0;
    let pid = (((p[1] & 0x1f) as u16) << 8) | p[2] as u16;
    let mut out = Packet { pid, pusi, ..Default::default() };
    if !pusi || p[3] & 0x10 == 0 {
        return Some(out);
    }
    let mut i = 4;
    if p[3] & 0x20 != 0 {
        i += 1 + p[4] as usize;
    }
    if i + 14 > PKT || p[i..i + 3] != [0, 0, 1] {
        return Some(out);
    }
    out.stream_id = Some(p[i + 3]);
    if p[i + 7] & 0x80 != 0 {
        let b = &p[i + 9..i + 14];
        out.pts = Some(
            ((b[0] as i64 >> 1) & 7) << 30
                | (b[1] as i64) << 22
                | ((b[2] as i64) >> 1) << 15
                | (b[3] as i64) << 7
                | (b[4] as i64) >> 1,
        );
    }
    Some(out)
}

pub fn is_video_stream(id: u8) -> bool {
    (0xe0..=0xef).contains(&id)
}

/// The 33-bit PTS nearest to `reference`, so a timeline past 26.5 h keeps counting up.
pub fn unwrap_pts(raw: i64, reference: i64) -> i64 {
    let base = reference - reference.rem_euclid(WRAP) + raw;
    [base - WRAP, base, base + WRAP]
        .into_iter()
        .min_by_key(|v| (v - reference).abs())
        .unwrap()
}


#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    File,
    Url,
    Slate,
}

impl Source {
    pub fn name(&self) -> &'static str {
        match self {
            Source::File => "file",
            Source::Url => "url",
            Source::Slate => "slate",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Seam {
    pub offset: i64,
    /// For a slate: the video it stands in for.
    pub index: usize,
    pub source: Source,
    pub start_pos: f64,
}

/// A new seam replaces every seam at or after its offset: those were flushed before airing.
pub fn push_seam(seams: &mut Vec<Seam>, seam: Seam) {
    seams.retain(|s| s.offset < seam.offset);
    seams.push(seam);
}

/// What the viewer is watching at timeline `t`: the seam and the position in its video.
/// On a slate the position stands still at the slate's start position.
pub fn view_at(seams: &[Seam], t: i64) -> Option<(&Seam, f64)> {
    let s = seams.iter().rev().find(|s| s.offset <= t).or(seams.first())?;
    let pos = match s.source {
        Source::Slate => s.start_pos,
        _ => s.start_pos + secs((t - s.offset).max(0)),
    };
    Some((s, pos))
}

/// Where a feeder seeks its input. The slate always plays from 0 (seeking the slate file makes
/// ffmpeg exit with nothing); its seam keeps the video position it covers for instead.
pub fn seek_for(source: &Source, pos: f64) -> f64 {
    if *source == Source::Slate { 0.0 } else { pos }
}

/// Where a feeder has fed to: its start position plus what it delivered.
pub fn fed_pos(start_pos: f64, offset: i64, end: i64) -> f64 {
    start_pos + secs(end - offset)
}


pub fn control_target(cmd: &str, index: Option<i64>, view: usize, n: usize) -> Result<usize, String> {
    if n == 0 {
        return Err("playlist not loaded".into());
    }
    match cmd {
        "next" => Ok((view + 1) % n),
        "prev" => Ok((view + n - 1) % n),
        "restart" => Ok(view),
        "jump" => match index {
            Some(i) if i >= 0 && (i as usize) < n => Ok(i as usize),
            Some(i) => Err(format!("index {i} out of range 0..{}", n - 1)),
            None => Err("jump needs an index".into()),
        },
        _ => Err(format!("unknown cmd {cmd:?}")),
    }
}

/// 2.x v2.1.5: a start within 5 s of the end starts the next video at 0.
pub fn resolve_start(index: usize, pos: f64, duration: Option<f64>, n: usize) -> (usize, f64) {
    match duration {
        Some(d) if d > 0.0 && pos > d - 5.0 => ((index + 1) % n, 0.0),
        _ => (index, pos.max(0.0)),
    }
}


/// `expire=` from a googlevideo URL, query or path style.
pub fn parse_expire(url: &str) -> Option<u64> {
    for sep in ["expire=", "/expire/"] {
        if let Some(i) = url.find(sep) {
            let digits: String = url[i + sep.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse() {
                return Some(v);
            }
        }
    }
    None
}

/// Break the signature so googlevideo answers 403, as an expired URL does.
pub fn mangle_sig(url: &str) -> String {
    for key in ["&sig=", "&signature=", "/sig/"] {
        if let Some(i) = url.find(key) {
            let at = i + key.len();
            return format!("{}BROKEN{}", &url[..at], &url[at..]);
        }
    }
    format!("{url}&sig=BROKEN")
}

pub fn refresh_due(now_s: u64, expire_s: u64, margin_s: u64) -> bool {
    now_s + margin_s >= expire_s
}

/// A video on air from its URL gets a spare resolve this long after its URL feeder starts.
pub const SPARE_AFTER_S: f64 = 60.0;

pub fn spare_usable(spare_index: usize, index: usize, expire_s: u64, now_s: u64, margin_s: u64) -> bool {
    spare_index == index && !refresh_due(now_s, expire_s, margin_s)
}

/// Queued media kept past the writer at a cut. A URL feeder reopening mid-video takes longer
/// to deliver its first bytes, and 1 s ran dry (about 0.75 s stutter in the spike).
pub fn cut_margin(source: &Source, pos: f64) -> i64 {
    if *source == Source::Url && pos > 0.0 { 3 * HZ } else { HZ }
}


pub const MAX_RETRIES: u32 = 2;

#[derive(Debug, PartialEq)]
pub enum Ladder {
    Retry,
    Skip,
}

pub fn after_failure(retries_so_far: u32) -> Ladder {
    if retries_so_far < MAX_RETRIES { Ladder::Retry } else { Ladder::Skip }
}

/// A video YouTube will not serve does not come back in seconds: it is skipped at once.
pub fn ladder_for(why: &str, retries_so_far: u32) -> Ladder {
    if why == "unavailable" { Ladder::Skip } else { after_failure(retries_so_far) }
}

/// From this many distinct "unavailable" videos in a row, each one counts toward the breaker:
/// a client that answers "unavailable" for every video would otherwise resolve the whole list.
pub const UNAVAILABLE_COUNTS_FROM: usize = 3;

/// Tracks `streak` (distinct indexes answered "unavailable" since the last successful resolve)
/// and returns the breaker's `counts` for this resolve outcome.
pub fn unavailable_counts(streak: &mut std::collections::HashSet<usize>, ok: bool, per_video: bool, index: usize) -> bool {
    if ok {
        streak.clear();
    } else if per_video {
        streak.insert(index);
        return streak.len() >= UNAVAILABLE_COUNTS_FROM;
    }
    true
}

/// 2.x shouldRetryUrl's premature EOF: a clean exit well short of the duration.
pub fn ended_early(code_ok: bool, fed: f64, duration: Option<f64>) -> bool {
    !code_ok || matches!(duration, Some(d) if d > 0.0 && fed < d - 5.0)
}

/// Global YouTube circuit breaker. A bot check, or this many resolve failures in a row across
/// videos, opens it; while open nothing talks to YouTube except one probe resolve per step.
pub const BREAKER_FAILURES: u32 = 3;
/// Minimum spacing of resolves (and size probes) for the whole supervisor.
pub const RESOLVE_SPACING_S: f64 = 5.0;

pub fn breaker_backoff_s(level: u32) -> f64 {
    [60.0, 120.0, 300.0, 600.0, 1800.0][(level as usize).min(4)]
}

/// yt-dlp's wording for a video that will never play (removed, private, blocked, age-gated, members-only).
/// YouTube's rate limit also says "Video unavailable ... try again later"; that one is not the video's.
pub fn is_video_unavailable(reason: &str) -> bool {
    let r = reason.to_lowercase();
    !r.contains("try again later")
        && !is_bot_check(&r)
        && ["video unavailable", "private video", "has been removed", "has been terminated", "in your country", "confirm your age", "members-only"].iter().any(|w| r.contains(w))
}

pub fn is_bot_check(reason: &str) -> bool {
    let r = reason.to_lowercase();
    r.contains("not a bot") || r.contains("bot check")
}

/// yt-dlp could not read the browser's cookie store (wrong browser name, no such profile, a
/// database the browser holds locked, DPAPI): the PC's problem, not YouTube's answer.
pub fn is_cookie_problem(reason: &str) -> bool {
    let r = reason.to_lowercase();
    if is_bot_check(&r) {
        return false;
    }
    // yt-dlp also prints this one for a single cookie and then carries on to YouTube's own answer.
    if r.contains("failed to decrypt with dpapi") {
        return r.lines().filter(|l| l.contains("error")).count() == 1;
    }
    ["specified for cookies", "cookies database", "cookie database", "failed to load cookies", "invalid cookies from browser", "containers.json"]
        .iter()
        .any(|w| r.contains(w))
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Allow {
    Yes,
    Probe,
    No,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BreakerChange {
    None,
    Opened,
    ProbeFailed,
    Closed,
}

/// Times are seconds on any monotonic clock.
#[derive(Debug, Default)]
pub struct Breaker {
    pub open: bool,
    pub level: u32,
    pub retry_at: f64,
    pub probing: bool,
    pub failures: u32,
}

impl Breaker {
    /// May a resolve run now? `Probe` hands out the one probe of this step.
    pub fn allow(&mut self, now: f64) -> Allow {
        if !self.open {
            Allow::Yes
        } else if !self.probing && now >= self.retry_at {
            self.probing = true;
            Allow::Probe
        } else {
            Allow::No
        }
    }

    /// Only resolves (`counts`) add to the failure run; a bot check from anything opens it.
    /// While open, only the probe's result moves it (a late failure from before it opened doesn't).
    pub fn result(&mut self, ok: bool, bot: bool, counts: bool, probe: bool, now: f64) -> BreakerChange {
        if ok {
            self.failures = 0;
            let was = self.open;
            *self = Breaker::default();
            return if was { BreakerChange::Closed } else { BreakerChange::None };
        }
        if self.open {
            if !probe {
                return BreakerChange::None;
            }
            self.probing = false;
            self.level += 1;
            self.retry_at = now + breaker_backoff_s(self.level);
            return BreakerChange::ProbeFailed;
        }
        if counts {
            self.failures += 1;
        }
        if bot || self.failures >= BREAKER_FAILURES {
            self.open = true;
            self.level = 0;
            self.probing = false;
            self.retry_at = now + breaker_backoff_s(0);
            return BreakerChange::Opened;
        }
        BreakerChange::None
    }
}

/// After every video of the list was skipped: the target is held off (slate on air) for 60 s,
/// doubling to 30 min per new all-skipped, until a video plays healthily again.
#[derive(Debug, Default)]
pub struct SkipHold {
    pub level: u32,
    pub until: Option<f64>,
}

impl SkipHold {
    /// Every video was skipped at `now`: start a hold; returns its length in seconds.
    pub fn trip(&mut self, now: f64) -> f64 {
        let wait = (60.0 * 2f64.powi(self.level.min(5) as i32)).min(1800.0);
        self.level += 1;
        self.until = Some(now + wait);
        wait
    }
    pub fn holding(&self, now: f64) -> bool {
        self.until.is_some_and(|u| now < u)
    }
    /// A video played healthily: the next all-skipped starts at 60 s again.
    pub fn healthy(&mut self) {
        *self = SkipHold::default();
    }
}

/// The `streak`-th feeder spawn failure in a row: (log an event?, seconds before the next try).
pub fn feeder_spawn_failed(streak: u32) -> (bool, u64) {
    (streak == 1, [1, 2, 5, 10][(streak.max(1) as usize - 1).min(3)])
}

pub fn encoder_backoff_s(failures: u32) -> u64 {
    [2, 5, 10, 30, 60][(failures as usize).min(4)]
}

pub fn token_backoff_s(restarts: u32) -> u64 {
    [2, 10, 30, 60][(restarts as usize).min(3)]
}


/// Videos worth having on disk, walking forward from the viewer within the budget. The viewer's
/// own video always counts; the one just behind never does; an unknown size ends the walk.
pub fn window(view: usize, sizes: &[Option<u64>], skipped: &[bool], budget: u64) -> Vec<usize> {
    let n = sizes.len();
    let mut out = vec![];
    let mut total = 0;
    for d in 0..n.saturating_sub(1).max(1) {
        let i = (view + d) % n;
        if d > 0 && skipped[i] {
            continue;
        }
        let Some(size) = sizes[i] else {
            out.push(i);
            break;
        };
        if d > 0 && total + size > budget {
            break;
        }
        total += size;
        out.push(i);
    }
    out
}

/// A file goes once it is outside the window, unless a feeder reads it.
pub fn deletable(i: usize, win: &[usize], view: usize, fed: Option<usize>) -> bool {
    i != view && Some(i) != fed && !win.contains(&i)
}

/// First window entry not on disk and not given up on.
pub fn next_download(win: &[usize], on_disk: &[bool], failed: &[bool]) -> Option<usize> {
    win.iter().copied().find(|&i| !on_disk[i] && !failed[i])
}

/// One download round: the configured list, then each client alone, since yt-dlp takes the first
/// client's formats from a list and does not fall back when they are refused. One client: two tries.
pub fn download_clients(list: &str) -> Vec<String> {
    let each: Vec<String> = list.split(',').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect();
    let mut out = vec![list.to_string()];
    if each.len() > 1 {
        out.extend(each);
    } else {
        out.push(list.to_string());
    }
    out
}

/// Seconds until video `i` goes on air, with the viewer at `pos` into `view`. None when a
/// duration on the way is unknown.
pub fn until_needed_s(view: usize, pos: f64, i: usize, durations: &[Option<f64>]) -> Option<f64> {
    let n = durations.len();
    if i == view || n == 0 {
        return Some(0.0);
    }
    let mut t = durations[view]? - pos;
    let mut j = (view + 1) % n;
    while j != i {
        t += durations[j]?;
        j = (j + 1) % n;
    }
    Some(t.max(0.0))
}

/// Wait after a failed download round: a quarter of the time until the video airs, 2 min to 1 h
/// (10 min when unknown), so a refusal that lifts is caught without asking YouTube every few seconds.
pub fn download_retry_s(until_needed_s: Option<f64>) -> u64 {
    until_needed_s.map_or(600, |t| (t / 4.0).clamp(120.0, 3600.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_round_tries_the_list_then_each_client_alone() {
        assert_eq!(download_clients("mweb,web_embedded"), ["mweb,web_embedded", "mweb", "web_embedded"]);
        assert_eq!(download_clients("mweb"), ["mweb", "mweb"]);
    }

    #[test]
    fn time_until_needed_walks_forward_from_the_viewer() {
        let d = [Some(100.0), Some(200.0), Some(300.0), None];
        assert_eq!(until_needed_s(0, 40.0, 0, &d), Some(0.0));
        assert_eq!(until_needed_s(0, 40.0, 1, &d), Some(60.0));
        assert_eq!(until_needed_s(0, 40.0, 3, &d), Some(560.0));
        assert_eq!(until_needed_s(1, 0.0, 0, &d), None, "video 3's length is unknown");
    }

    #[test]
    fn a_failed_download_waits_longer_the_further_off_it_is() {
        assert_eq!(download_retry_s(Some(8.0 * 3600.0)), 3600);
        assert_eq!(download_retry_s(Some(3600.0)), 900);
        assert_eq!(download_retry_s(Some(60.0)), 120);
        assert_eq!(download_retry_s(None), 600);
    }

    #[test]
    fn cookie_store_failures_are_told_apart_from_youtube() {
        // yt-dlp 2026.10.23's own wording, from its source and a run with a bogus profile
        for r in [
            r"ERROR: could not find firefox cookies database in 'C:\\nonexistent-profile'",
            r#"ERROR: could not find chrome cookies database in "C:\nonexistent-profile""#,
            "ERROR: Could not copy Chrome cookie database. See  https://github.com/yt-dlp/yt-dlp/issues/7271  for more info",
            "ERROR: Failed to decrypt with DPAPI. See  https://github.com/yt-dlp/yt-dlp/issues/10927  for more info",
            "yt-dlp.exe: error: unsupported browser specified for cookies: \"nosuchbrowser\". Supported browsers are: brave, chrome",
            "ERROR: failed to load cookies",
        ] {
            assert!(is_cookie_problem(r), "{r}");
            assert!(!is_bot_check(r) && !is_video_unavailable(r), "{r}");
        }
        assert!(!is_cookie_problem("ERROR: [youtube] a: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies"));
        assert!(!is_cookie_problem("ERROR: [youtube] a: Video unavailable"));
        // A DPAPI line is the cookie problem only when it is the sole ERROR line.
        let dpapi = "ERROR: Failed to decrypt with DPAPI. See  https://github.com/yt-dlp/yt-dlp/issues/10927  for more info";
        assert!(!is_cookie_problem(&format!("{dpapi}\nERROR: [youtube] a: Sign in to confirm you're not a bot")));
        assert!(!is_cookie_problem(&format!("{dpapi}\nERROR: [youtube] a: Requested format is not available")));
        assert!(is_cookie_problem(&format!("[youtube] a: Downloading webpage\n{dpapi}\n")));
    }

    #[test]
    fn window_cpu_is_zero_when_the_process_changed_between_samples() {
        assert_eq!(window_cpu(Some(7), Some(7), 100, 150, 100), 50.0);
        // A restarted encoder is a different process: its time is not comparable.
        assert_eq!(window_cpu(Some(7), Some(9), 900, 150, 100), 0.0);
        assert_eq!(window_cpu(Some(7), Some(9), 100, 900, 100), 0.0);
        assert_eq!(window_cpu(None, Some(9), 0, 150, 100), 0.0);
        assert_eq!(window_cpu(Some(7), None, 100, 0, 100), 0.0);
    }

    #[test]
    fn cpu_percent_scales_to_all_cores_and_survives_a_zero_total() {
        assert_eq!(cpu_percent(50, 100), 50.0);
        assert_eq!(cpu_percent(5, 0), 0.0);
        assert_eq!(cpu_percent(300, 100), 100.0);
    }


    fn pes_packet(pid: u16, stream_id: u8, pts: i64, adaptation: bool) -> Vec<u8> {
        let mut p = vec![0xff; PKT];
        p[0] = 0x47;
        p[1] = 0x40 | (pid >> 8) as u8;
        p[2] = pid as u8;
        let mut i = 4;
        if adaptation {
            p[3] = 0x30;
            p[4] = 7;
            i = 12;
        } else {
            p[3] = 0x10;
        }
        p[i..i + 9].copy_from_slice(&[0, 0, 1, stream_id, 0, 0, 0x80, 0x80, 5]);
        let b = &mut p[i + 9..i + 14];
        b[0] = 0x21 | (((pts >> 30) & 7) << 1) as u8;
        b[1] = (pts >> 22) as u8;
        b[2] = (((pts >> 15) & 0x7f) << 1) as u8 | 1;
        b[3] = (pts >> 7) as u8;
        b[4] = ((pts & 0x7f) << 1) as u8 | 1;
        p
    }

    #[test]
    fn parses_pts_with_and_without_adaptation_field() {
        for adaptation in [false, true] {
            let p = parse_packet(&pes_packet(0x100, 0xe0, 0x1_2345_6789, adaptation)).unwrap();
            assert_eq!(p.pid, 0x100);
            assert!(p.pusi);
            assert_eq!(p.stream_id, Some(0xe0));
            assert_eq!(p.pts, Some(0x1_2345_6789));
        }
    }

    #[test]
    fn continuation_packet_has_no_pts() {
        let mut p = pes_packet(0x101, 0xc0, 1, false);
        p[1] &= !0x40;
        let parsed = parse_packet(&p).unwrap();
        assert!(!parsed.pusi);
        assert_eq!(parsed.pts, None);
        assert!(parse_packet(&[0u8; PKT]).is_none());
    }

    #[test]
    fn unwraps_pts_across_2_pow_33() {
        let near_wrap = WRAP - 3000;
        assert_eq!(unwrap_pts(near_wrap, near_wrap - 3000), near_wrap);
        assert_eq!(unwrap_pts(1000, near_wrap), WRAP + 1000);
        assert_eq!(unwrap_pts(WRAP - 1000, 3 * WRAP + 500), 3 * WRAP - 1000);
        assert_eq!(unwrap_pts(BASE, BASE), BASE);
    }

    fn seam(offset_s: i64, index: usize, source: Source, start_pos: f64) -> Seam {
        Seam { offset: offset_s * HZ, index, source, start_pos }
    }

    #[test]
    fn p_view_maps_through_seams() {
        let mut s = vec![];
        push_seam(&mut s, seam(10, 0, Source::Slate, 0.0));
        push_seam(&mut s, seam(15, 3, Source::Url, 100.0));
        push_seam(&mut s, seam(40, 3, Source::File, 125.0));
        let (v, pos) = view_at(&s, 12 * HZ).unwrap();
        assert_eq!((v.source.clone(), pos), (Source::Slate, 0.0));
        let (v, pos) = view_at(&s, 20 * HZ).unwrap();
        assert_eq!((v.index, pos), (3, 105.0));
        assert_eq!(view_at(&s, 50 * HZ).unwrap().1, 135.0);
    }

    #[test]
    fn a_flushed_seam_is_replaced() {
        let mut s = vec![];
        push_seam(&mut s, seam(10, 0, Source::File, 0.0));
        push_seam(&mut s, seam(30, 1, Source::File, 0.0));
        push_seam(&mut s, seam(20, 5, Source::File, 0.0));
        assert_eq!(s.iter().map(|s| s.index).collect::<Vec<_>>(), [0, 5]);
    }

    #[test]
    fn breaker_opens_on_a_bot_check_and_probes_once_per_step() {
        let mut b = Breaker::default();
        assert_eq!(b.allow(0.0), Allow::Yes);
        assert_eq!(b.result(false, true, true, false, 10.0), BreakerChange::Opened);
        assert_eq!(b.allow(10.0), Allow::No);
        assert_eq!(b.allow(69.9), Allow::No);
        assert_eq!(b.allow(70.0), Allow::Probe);
        assert_eq!(b.allow(70.0), Allow::No, "one probe per step");
        // the probe fails: 2 min, then 5, 10, 30, and 30 forever
        let mut now = 75.0;
        for wait in [120.0, 300.0, 600.0, 1800.0, 1800.0, 1800.0] {
            assert_eq!(b.result(false, true, true, true, now), BreakerChange::ProbeFailed);
            assert_eq!(b.allow(now + wait - 0.1), Allow::No);
            assert_eq!(b.allow(now + wait), Allow::Probe);
            now += wait + 5.0;
        }
        assert_eq!(b.result(true, false, true, true, now), BreakerChange::Closed);
        assert!(!b.open);
        assert_eq!(b.allow(now), Allow::Yes);
        assert_eq!(b.level, 0);
    }

    #[test]
    fn breaker_opens_on_three_resolve_failures_in_a_row_not_on_download_failures() {
        let mut b = Breaker::default();
        assert_eq!(b.result(false, false, true, false, 0.0), BreakerChange::None);
        assert_eq!(b.result(false, false, false, false, 1.0), BreakerChange::None, "a plain download failure does not count");
        assert_eq!(b.result(false, false, true, false, 2.0), BreakerChange::None);
        assert_eq!(b.result(true, false, true, false, 3.0), BreakerChange::None, "a success resets the run");
        assert_eq!(b.result(false, false, true, false, 4.0), BreakerChange::None);
        assert_eq!(b.result(false, false, true, false, 5.0), BreakerChange::None);
        assert_eq!(b.result(false, false, true, false, 6.0), BreakerChange::Opened);
        assert_eq!(b.retry_at, 66.0);
    }

    #[test]
    fn breaker_ignores_late_failures_while_open_but_closes_on_any_success() {
        let mut b = Breaker::default();
        b.result(false, true, false, false, 0.0);
        assert_eq!(b.result(false, true, true, false, 1.0), BreakerChange::None);
        assert_eq!(b.retry_at, 60.0, "a late failure does not push the probe back");
        assert_eq!(b.result(true, false, true, false, 2.0), BreakerChange::Closed);
    }

    #[test]
    fn per_video_reasons_are_told_apart_from_youtube_wide_ones() {
        for r in [
            "ERROR: [youtube] x: Video unavailable. This video has been removed by the uploader",
            "ERROR: [youtube] x: Private video. Sign in if you've been granted access to this video",
            "ERROR: [youtube] x: Video unavailable. The uploader has not made this video available in your country",
            "ERROR: [youtube] x: Video unavailable. This video contains content from SME, who has blocked it in your country on copyright grounds",
            "ERROR: [youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.",
            "ERROR: [youtube] x: Join this channel to get access to members-only content like this video, and other exclusive perks.",
            "ERROR: [youtube] x: Video unavailable. This video is no longer available because the YouTube account associated with this video has been terminated.",
        ] {
            assert!(is_video_unavailable(r), "{r}");
        }
        for r in [
            "ERROR: [youtube] x: Sign in to confirm you're not a bot. Use --cookies-from-browser",
            "ERROR: [youtube] x: Video unavailable. This content isn't available, try again later.",
            "HTTP Error 403: Forbidden",
            "yt-dlp exit None",
        ] {
            assert!(!is_video_unavailable(r), "{r}");
        }
    }

    const GONE: &str = "ERROR: [youtube] x: Video unavailable. This video has been removed by the uploader";

    /// Feeds one resolve outcome through the streak and the breaker, as `on_resolved` does.
    fn resolve(b: &mut Breaker, streak: &mut std::collections::HashSet<usize>, index: usize, ok: bool) -> BreakerChange {
        let counts = unavailable_counts(streak, ok, !ok && is_video_unavailable(GONE), index);
        b.result(ok, false, counts, false, 0.0)
    }

    #[test]
    fn three_distinct_unavailable_videos_in_a_row_count_toward_the_breaker() {
        let (mut b, mut streak) = (Breaker::default(), Default::default());

        let failures: Vec<u32> = [1, 1, 2, 3, 4].iter().map(|&i| {
            resolve(&mut b, &mut streak, i, false);
            b.failures
        }).collect();
        let change = resolve(&mut b, &mut streak, 5, false);

        assert_eq!(failures, [0, 0, 0, 1, 2], "the same video twice is one entry; counting starts at the third");
        assert_eq!(change, BreakerChange::Opened);
    }

    #[test]
    fn a_successful_resolve_resets_the_unavailable_streak() {
        let (mut b, mut streak) = (Breaker::default(), Default::default());
        resolve(&mut b, &mut streak, 1, false);
        resolve(&mut b, &mut streak, 2, false);

        resolve(&mut b, &mut streak, 7, true);
        resolve(&mut b, &mut streak, 3, false);
        resolve(&mut b, &mut streak, 4, false);

        assert_eq!(b.failures, 0);
        resolve(&mut b, &mut streak, 5, false);
        assert_eq!(b.failures, 1);
    }

    #[test]
    fn one_unavailable_video_still_skips_without_touching_the_breaker() {
        let (mut b, mut streak) = (Breaker::default(), Default::default());

        resolve(&mut b, &mut streak, 4, false);

        assert_eq!((b.open, b.failures), (false, 0));
        assert_eq!(ladder_for("unavailable", 0), Ladder::Skip, "a removed video does not come back in 10 s");
        assert_eq!(ladder_for("stall", 0), Ladder::Retry, "other failures keep the ladder");
        assert_eq!(ladder_for("stall", MAX_RETRIES), Ladder::Skip);
    }

    #[test]
    fn all_skipped_holds_the_target_off_with_a_doubling_backoff_reset_by_healthy_play() {
        let mut h = SkipHold::default();
        assert!(!h.holding(0.0));

        let waits: Vec<f64> = (0..7).map(|i| h.trip(i as f64 * 10_000.0)).collect();

        assert_eq!(waits, [60.0, 120.0, 240.0, 480.0, 960.0, 1800.0, 1800.0]);
        assert!(h.holding(60_000.0 + 1799.0) && !h.holding(60_000.0 + 1800.0));
        h.healthy();
        assert_eq!(h.trip(0.0), 60.0, "healthy play resets the backoff");
        assert!(h.holding(59.0) && !h.holding(60.0));
    }

    #[test]
    fn a_feeder_that_will_not_spawn_is_logged_once_and_retried_with_a_backoff() {
        let got: Vec<(bool, u64)> = (1..=6).map(feeder_spawn_failed).collect();

        assert_eq!(got, [(true, 1), (false, 2), (false, 5), (false, 10), (false, 10), (false, 10)]);
    }

    #[test]
    fn bot_check_reasons_are_recognised() {
        assert!(is_bot_check("YouTube bot check: sign in to confirm you're not a bot"));
        assert!(is_bot_check("ERROR: [youtube] x: Sign in to confirm you\u{2019}re not a bot."));
        assert!(!is_bot_check("HTTP Error 403: Forbidden"));
        assert!(!is_bot_check("ERROR: [youtube] x: Sign in to confirm your age."), "an age gate is the video's, not a bot check");
    }

    #[test]
    fn slate_covering_a_mid_video_retry_plays_from_its_start() {
        assert_eq!(seek_for(&Source::Slate, 171.8), 0.0);
        assert_eq!(seek_for(&Source::Url, 171.8), 171.8);
        assert_eq!(seek_for(&Source::File, 12.5), 12.5);
    }

    #[test]
    fn fed_position_continues_the_offset() {
        assert_eq!(fed_pos(100.0, 10 * HZ, 25 * HZ), 115.0);
    }

    #[test]
    fn controls_wrap_and_validate() {
        assert_eq!(control_target("next", None, 4, 5), Ok(0));
        assert_eq!(control_target("prev", None, 0, 5), Ok(4));
        assert_eq!(control_target("restart", None, 2, 5), Ok(2));
        assert_eq!(control_target("jump", Some(3), 0, 5), Ok(3));
        assert!(control_target("jump", Some(5), 0, 5).is_err());
        assert!(control_target("jump", Some(-1), 0, 5).is_err());
        assert!(control_target("jump", None, 0, 5).is_err());
        assert!(control_target("seek", None, 0, 5).is_err());
        assert!(control_target("next", None, 0, 0).is_err());
    }

    #[test]
    fn start_near_the_end_moves_to_the_next_video() {
        assert_eq!(resolve_start(2, 96.0, Some(100.0), 3), (0, 0.0));
        assert_eq!(resolve_start(2, 94.0, Some(100.0), 3), (2, 94.0));
        assert_eq!(resolve_start(1, 50.0, None, 3), (1, 50.0));
    }

    #[test]
    fn expire_parses_both_styles() {
        assert_eq!(parse_expire("https://r1.googlevideo.com/videoplayback?expire=1790000000&ei=x"), Some(1790000000));
        assert_eq!(parse_expire("https://r1.googlevideo.com/videoplayback/expire/1790000001/ei/x"), Some(1790000001));
        assert_eq!(parse_expire("https://example.com/a.mp4"), None);
        assert!(refresh_due(1000, 2800, 1800));
        assert!(!refresh_due(999, 2800, 1800));
    }

    #[test]
    fn a_spare_is_usable_only_for_its_own_video_and_outside_the_refresh_margin() {
        assert!(spare_usable(4, 4, 10_000, 1000, 1800));
        assert!(!spare_usable(3, 4, 10_000, 1000, 1800), "another video's spare");
        assert!(!spare_usable(4, 4, 900, 1000, 1800), "expired");
        assert!(!spare_usable(4, 4, 2800, 1000, 1800), "inside the refresh margin");
        assert!(spare_usable(4, 4, 2801, 1000, 1800));
    }

    #[test]
    fn the_cut_keeps_three_seconds_only_for_a_mid_video_url_start() {
        assert_eq!(cut_margin(&Source::Url, 42.5), 3 * HZ);
        assert_eq!(cut_margin(&Source::Url, 0.0), HZ);
        assert_eq!(cut_margin(&Source::File, 42.5), HZ);
        assert_eq!(cut_margin(&Source::Slate, 42.5), HZ);
    }

    #[test]
    fn mangled_signature_differs() {
        let u = "https://x/videoplayback?expire=1&sig=ABC&n=1";
        assert_eq!(mangle_sig(u), "https://x/videoplayback?expire=1&sig=BROKENABC&n=1");
        assert!(mangle_sig("https://x/v?a=1").ends_with("&sig=BROKEN"));
    }

    #[test]
    fn ladder_retries_twice_then_skips() {
        assert_eq!(after_failure(0), Ladder::Retry);
        assert_eq!(after_failure(1), Ladder::Retry);
        assert_eq!(after_failure(2), Ladder::Skip);
        assert!(ended_early(false, 100.0, Some(100.0)));
        assert!(ended_early(true, 50.0, Some(100.0)));
        assert!(!ended_early(true, 99.7, Some(100.0)));
        assert!(!ended_early(true, 10.0, None));
        assert_eq!([0, 1, 2, 3, 4, 9].map(encoder_backoff_s), [2, 5, 10, 30, 60, 60]);
        assert_eq!([0, 1, 2, 7].map(token_backoff_s), [2, 10, 30, 60]);
    }

    #[test]
    fn window_walks_forward_within_budget() {
        let sizes = [Some(100), Some(100), Some(100), Some(100), Some(100)];
        let none = [false; 5];
        assert_eq!(window(3, &sizes, &none, 250), [3, 4]);
        // the current video always counts, even over budget
        assert_eq!(window(1, &[Some(10), Some(900), Some(10)], &[false; 3], 100), [1]);
        // never the video just behind the viewer, even when everything fits
        assert_eq!(window(2, &sizes, &none, 10_000), [2, 3, 4, 0]);
        // unknown size ends the walk; skipped entries are stepped over
        assert_eq!(window(0, &[Some(10), None, Some(10)], &[false; 3], 100), [0, 1]);
        assert_eq!(window(0, &sizes, &[false, true, false, false, false], 250), [0, 2]);
    }

    #[test]
    fn deletion_follows_playout_and_switches() {
        let sizes = [Some(100); 5];
        let none = [false; 5];
        // viewer moved from 1 to 2 by playout: 1 is behind and goes
        let win = window(2, &sizes, &none, 250);
        assert!(deletable(1, &win, 2, Some(3)));
        assert!(!deletable(2, &win, 2, Some(3)));
        assert!(!deletable(3, &win, 2, Some(3)));
        // jump from 1 to 4: 2 is no longer ahead; restart keeps the current file
        let win = window(4, &sizes, &none, 250);
        assert!(deletable(2, &win, 4, Some(4)));
        assert!(!deletable(1, &window(1, &sizes, &none, 250), 1, Some(1)));
        // a file a live feeder reads is never deleted
        assert!(!deletable(2, &win, 4, Some(2)));
    }

    #[test]
    fn downloads_the_first_missing_window_entry() {
        assert_eq!(next_download(&[3, 4, 0], &[false, false, false, true, false], &[false; 5]), Some(4));
        assert_eq!(next_download(&[3, 4], &[false, false, false, true, false], &[false, false, false, false, true]), None);
    }
}
