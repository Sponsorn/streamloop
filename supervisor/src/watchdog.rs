//! Output watchdog: ffmpeg's freezedetect and silencedetect lines from the encoder, judged
//! against the seam table. Pure: no I/O, no clocks.

use crate::rules::{self, Seam, Source};

/// Times are encoder media seconds, as ffmpeg prints them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Detect {
    FreezeStart(f64),
    FreezeEnd(f64),
    SilenceStart(f64),
    SilenceEnd(f64),
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Ignore,
    Freeze { index: usize, pos: f64 },
    Silence { index: usize, pos: f64 },
}

/// Seconds after a seam start that never count as a freeze or a silence.
pub const SETTLE_S: f64 = 5.0;

pub fn parse_line(line: &str) -> Option<Detect> {
    let keys: [(&str, &str, fn(f64) -> Detect); 4] = [
        ("[freezedetect @", "freeze_start:", Detect::FreezeStart),
        ("[freezedetect @", "freeze_end:", Detect::FreezeEnd),
        ("[silencedetect @", "silence_start:", Detect::SilenceStart),
        ("[silencedetect @", "silence_end:", Detect::SilenceEnd),
    ];
    keys.iter().find_map(|(tag, key, make)| {
        let rest = &line[line.find(key).filter(|_| line.starts_with(tag))? + key.len()..];
        rest.split_whitespace().next()?.parse().ok().map(make)
    })
}

/// Timeline ticks of encoder media seconds: the same mapping as `P_view`.
fn timeline(media: f64) -> i64 {
    rules::BASE + rules::ticks(media)
}

/// A start line, against the seam on air at `now_media`. Ignored on the slate, in the first
/// `SETTLE_S` of a seam, and when a cut came between the start and now.
pub fn judge(d: &Detect, seams: &[Seam], now_media: f64) -> Verdict {
    let (t, freeze) = match *d {
        Detect::FreezeStart(t) => (t, true),
        Detect::SilenceStart(t) => (t, false),
        _ => return Verdict::Ignore,
    };
    let (Some((began, _)), Some((on_air, pos))) = (rules::view_at(seams, timeline(t)), rules::view_at(seams, timeline(now_media))) else {
        return Verdict::Ignore;
    };
    let settled = timeline(t) >= began.offset + rules::ticks(SETTLE_S);
    if began.source == Source::Slate || on_air.source == Source::Slate || began.offset != on_air.offset || !settled {
        return Verdict::Ignore;
    }
    let index = on_air.index;
    if freeze { Verdict::Freeze { index, pos } } else { Verdict::Silence { index, pos } }
}

/// An open stretch counts from `since` or `SETTLE_S` into the seam on air, whichever is later; returns
/// that start once it lasted `need`. ffmpeg reports a stretch once, so a retry into it is judged here.
pub fn due(since: f64, seams: &[Seam], now_media: f64, need: f64) -> Option<f64> {
    let (on_air, _) = rules::view_at(seams, timeline(now_media))?;
    let start = since.max(rules::secs(on_air.offset - rules::BASE) + SETTLE_S);
    (now_media - start >= need).then_some(start)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Heal {
    Retry,
    Skip,
    EventOnly,
}

/// A freeze takes the stall ladder (its retry budget); a silence one retry per video, never a skip.
pub fn heal(v: &Verdict, retries: u32, silence_retried: bool) -> Option<Heal> {
    match v {
        Verdict::Ignore => None,
        Verdict::Freeze { .. } => Some(match rules::after_failure(retries) {
            rules::Ladder::Retry => Heal::Retry,
            rules::Ladder::Skip => Heal::Skip,
        }),
        Verdict::Silence { .. } => Some(if silence_retried { Heal::EventOnly } else { Heal::Retry }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{HZ, BASE};

    #[test]
    fn parses_real_ffmpeg_8_detector_lines() {
        // Captured from ffmpeg 8.0 (gyan.dev essentials) on the slate followed by testsrc2.
        let lines = [
            ("[silencedetect @ 000002938f820840] silence_start: 0", Some(Detect::SilenceStart(0.0))),
            ("[freezedetect @ 0000029387615f40] lavfi.freezedetect.freeze_start: 0", Some(Detect::FreezeStart(0.0))),
            ("[freezedetect @ 0000029387615f40] lavfi.freezedetect.freeze_duration: 10.005333", None),
            ("[freezedetect @ 0000029387615f40] lavfi.freezedetect.freeze_end: 10.005333", Some(Detect::FreezeEnd(10.005333))),
            ("[silencedetect @ 000002938f820840] silence_end: 10.005354 | silence_duration: 10.005354", Some(Detect::SilenceEnd(10.005354))),
            ("[freezedetect @ 000001f12e183ec0] lavfi.freezedetect.freeze_start: 0.0100222", Some(Detect::FreezeStart(0.0100222))),
            ("[silencedetect @ 0000019c22640880] silence_start: -0.0213", Some(Detect::SilenceStart(-0.0213))),
            ("Output #0, flv, to 'rtmp://127.0.0.1:1935/live/test':", None),
            ("[flv @ 000001] silence_start: 3", None),
        ];
        for (line, want) in lines {
            assert_eq!(parse_line(line), want, "{line}");
        }
    }

    fn seam(offset_media_s: f64, index: usize, source: Source, start_pos: f64) -> Seam {
        Seam { offset: BASE + (offset_media_s * HZ as f64) as i64, index, source, start_pos }
    }

    /// Slate 0..10 s, video 3 from 10 s at position 100, video 4 from 60 s at 0.
    fn seams() -> Vec<Seam> {
        vec![seam(0.0, 3, Source::Slate, 100.0), seam(10.0, 3, Source::Url, 100.0), seam(60.0, 4, Source::File, 0.0)]
    }

    #[test]
    fn judge_ignores_the_slate() {
        assert_eq!(judge(&Detect::FreezeStart(1.0), &seams(), 9.0), Verdict::Ignore);
        assert_eq!(judge(&Detect::SilenceStart(0.0), &seams(), 9.5), Verdict::Ignore);
    }

    #[test]
    fn judge_ignores_a_freeze_that_began_on_the_slate_and_runs_into_the_video() {
        assert_eq!(judge(&Detect::FreezeStart(2.0), &seams(), 30.0), Verdict::Ignore);
    }

    #[test]
    fn judge_ignores_the_first_five_seconds_after_a_seam() {
        assert_eq!(judge(&Detect::FreezeStart(10.0), &seams(), 31.0), Verdict::Ignore);
        assert_eq!(judge(&Detect::FreezeStart(14.9), &seams(), 35.0), Verdict::Ignore);
        assert_eq!(judge(&Detect::SilenceStart(61.0), &seams(), 95.0), Verdict::Ignore);
    }

    #[test]
    fn judge_ignores_a_freeze_from_before_a_cut() {
        // began in video 3, the viewer is now on video 4
        assert_eq!(judge(&Detect::FreezeStart(40.0), &seams(), 70.0), Verdict::Ignore);
    }

    #[test]
    fn judge_maps_to_the_video_and_the_position_on_air() {
        assert_eq!(judge(&Detect::FreezeStart(15.0), &seams(), 35.0), Verdict::Freeze { index: 3, pos: 125.0 });
        assert_eq!(judge(&Detect::SilenceStart(66.0), &seams(), 96.0), Verdict::Silence { index: 4, pos: 36.0 });
    }

    #[test]
    fn an_open_freeze_counts_from_five_seconds_into_the_seam_on_air() {
        // detected at once, inside one seam
        assert_eq!(due(20.0, &seams(), 39.9, 20.0), None);
        assert_eq!(due(20.0, &seams(), 40.0, 20.0), Some(20.0));
        // began on the slate: counts from 15 s, so not yet at 30 s
        assert_eq!(due(2.0, &seams(), 30.0, 20.0), None);
        assert_eq!(due(2.0, &seams(), 35.0, 20.0), Some(15.0));
        // a retry landed in a still-frozen stretch: the new seam counts on its own
        assert_eq!(due(30.0, &seams(), 70.0, 20.0), None);
        assert_eq!(due(30.0, &seams(), 85.0, 20.0), Some(65.0));
    }

    #[test]
    fn a_frozen_slate_is_never_healed_however_long_it_stays() {
        let long = vec![seam(0.0, 3, Source::Slate, 100.0)];
        for now in [25.0, 40.0, 600.0] {
            let start = due(0.0, &long, now, 20.0).unwrap();
            assert_eq!(judge(&Detect::FreezeStart(start), &long, now), Verdict::Ignore, "{now}");
        }
        // a freeze that began on the slate does not carry into the video
        assert_eq!(due(0.0, &seams(), 34.9, 20.0), None);
    }

    #[test]
    fn a_frozen_video_retries_twice_then_skips() {
        assert_eq!(heal(&Verdict::Freeze { index: 3, pos: 5.0 }, 0, false), Some(Heal::Retry));
        assert_eq!(heal(&Verdict::Freeze { index: 3, pos: 5.0 }, 1, true), Some(Heal::Retry));
        assert_eq!(heal(&Verdict::Freeze { index: 3, pos: 5.0 }, 2, false), Some(Heal::Skip));
        assert_eq!(heal(&Verdict::Ignore, 0, false), None);
    }

    /// Ten minutes of silence that belongs to the video: one retry, then events only, never a skip.
    #[test]
    fn long_content_silence_retries_once_per_video_and_never_skips() {
        let mut s = vec![seam(0.0, 2, Source::File, 0.0)];
        let mut open = Some(0.0);
        let (mut retried, mut heals) = (false, vec![]);
        let mut now = 0.0;
        while now < 600.0 {
            now += 0.1;
            let Some(since) = open else { continue };
            let Some(start) = due(since, &s, now, 30.0) else { continue };
            let v = judge(&Detect::SilenceStart(start), &s, now);
            let h = heal(&v, 2, retried).unwrap();
            heals.push(h);
            if h == Heal::Retry {
                // the retry restarts the same silent video one second on: a new seam, still silent
                retried = true;
                crate::rules::push_seam(&mut s, seam(now + 1.0, 2, Source::File, now + 1.0));
                open = Some(now + 1.0);
            } else {
                open = None;
            }
        }
        assert_eq!(heals, [Heal::Retry, Heal::EventOnly]);
    }

    #[test]
    fn judge_ignores_end_lines() {
        assert_eq!(judge(&Detect::FreezeEnd(30.0), &seams(), 35.0), Verdict::Ignore);
        assert_eq!(judge(&Detect::SilenceEnd(30.0), &seams(), 35.0), Verdict::Ignore);
    }
}
