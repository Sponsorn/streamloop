//! Pure rules: no I/O, no clocks. Everything the control loop decides is here and tested.

/// 90 kHz MPEG-TS ticks. Timeline offsets are kept in ticks so continuing offsets are exact.
pub const HZ: i64 = 90_000;
pub const FRAME: i64 = HZ / 30;
pub const BASE: i64 = 10 * HZ;
const WRAP: i64 = 1 << 33;

pub fn secs(ticks: i64) -> f64 {
    ticks as f64 / HZ as f64
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

/// Where a feeder seeks its input. The slate always plays from its start: its seam keeps the
/// video position it covers for (so P_view stands still), but seeking the slate file there
/// makes ffmpeg exit with nothing.
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


pub const MAX_RETRIES: u32 = 2;

#[derive(Debug, PartialEq)]
pub enum Ladder {
    Retry,
    Skip,
}

pub fn after_failure(retries_so_far: u32) -> Ladder {
    if retries_so_far < MAX_RETRIES { Ladder::Retry } else { Ladder::Skip }
}

/// 2.x shouldRetryUrl's premature EOF: a clean exit well short of the duration.
pub fn ended_early(code_ok: bool, fed: f64, duration: Option<f64>) -> bool {
    !code_ok || matches!(duration, Some(d) if d > 0.0 && fed < d - 5.0)
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

#[cfg(test)]
mod tests {
    use super::*;

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
