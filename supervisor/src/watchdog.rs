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
    /// ffmpeg rebuilt the video (`true`) or audio filter graph, for example on new colour tags.
    /// The detector in it starts over, so a stretch it had open never gets an end line.
    Rebuilt { video: bool },
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
    if line.contains("] Reconfiguring filter graph") {
        let video = line.starts_with("[vf#") || line.contains("because video parameters");
        let audio = line.starts_with("[af#") || line.contains("because audio parameters");
        return (video || audio).then_some(Detect::Rebuilt { video });
    }
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

/// A feeder started for this reason replaces the picture (control, skip, retry, fault, ...):
/// a stretch from before its seam must never come due. Playout and URL handovers continue it.
pub fn moves_cut(reason: &str) -> bool {
    !matches!(reason, "normal" | "handover" | "refresh")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Heal {
    /// Restart the video in place at the cut.
    Retry,
    Skip,
    /// The silence is in the video ("brb" stretches): report it, play on, never skip.
    Content,
}

/// A freeze is a fault: the stall ladder (`retries`), then a skip. A silence gets one retry per
/// stretch; `retried` = (video, when that retry went on air), and a silence on that video that
/// began within `SETTLE_S` of it re-began with the restart: content.
pub fn heal(v: &Verdict, since: f64, retried: Option<(usize, f64)>, retries: u32) -> Option<Heal> {
    match v {
        Verdict::Ignore => None,
        Verdict::Freeze { .. } => Some(match rules::after_failure(retries) {
            rules::Ladder::Retry => Heal::Retry,
            rules::Ladder::Skip => Heal::Skip,
        }),
        Verdict::Silence { index, .. } => {
            let same_stretch = retried.is_some_and(|(i, aired)| i == *index && since < aired + SETTLE_S);
            Some(if same_stretch { Heal::Content } else { Heal::Retry })
        }
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
            (
                "[vf#0:0 @ 00000208bbfbe440] Reconfiguring filter graph because video parameters changed to yuv420p(tv, bt709), 1920x1080",
                Some(Detect::Rebuilt { video: true }),
            ),
            // not seen in a run: built from ffmpeg's audio message
            ("[af#0:1 @ 00000208bbfbe880] Reconfiguring filter graph because audio parameters changed to 48000 Hz, stereo, fltp", Some(Detect::Rebuilt { video: false })),
            // older ffmpeg and -filter_complex name the graph differently; the message still says which
            ("[fc#0 @ 000001] Reconfiguring filter graph because video parameters changed to yuv420p(tv, bt709), 1920x1080", Some(Detect::Rebuilt { video: true })),
            ("[Parsed_x @ 000001] Reconfiguring filter graph because audio parameters changed to 48000 Hz, stereo, fltp", Some(Detect::Rebuilt { video: false })),
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

    /// Video 3 has been frozen since 15 s; the operator pressed "next" and video 4's seam is queued
    /// at 40 s, not yet on air. The stretch must not come due in that window and undo the control.
    #[test]
    fn a_freeze_never_comes_due_across_a_pending_cut() {
        let mut s = vec![seam(10.0, 3, Source::File, 100.0)];
        let mut cut_at = 0.0;
        let pending = seam(40.0, 4, Source::File, 0.0);
        for reason in ["control", "skip", "retry", "fault", "watchdog", "encoder-restart"] {
            if moves_cut(reason) {
                cut_at = rules::secs(pending.offset - rules::BASE);
            }
            assert!(cut_at == 40.0, "{reason} does not move the cut");
        }
        crate::rules::push_seam(&mut s, pending);

        for now in [36.0, 38.0, 39.9] {
            assert_eq!(due(15.0_f64.max(cut_at), &s, now, 20.0), None, "{now}");
        }
        // plain playout and URL handovers continue the picture: they are not cuts
        assert!(!moves_cut("normal") && !moves_cut("handover") && !moves_cut("refresh"));
    }

    const NEED: f64 = 20.0;

    /// What the detector sees after a retry of a still stretch.
    #[derive(Clone, Copy)]
    enum After {
        /// The stretch runs on across the retry's seam (no end line).
        Continues,
        /// The restart's first frames read as a change: an end, then a new start at once.
        BeginsAgainAtTheSeam,
        /// The retry cleared it: an end line, then normal picture and sound.
        Cleared,
    }

    /// Ten minutes of a video still from 0 s, judged and healed as `watchdog_work` does; a new
    /// stretch opens at `reopen_at` if nothing is open then.
    fn ten_still_minutes(freeze: bool, after: After, reopen_at: Option<f64>) -> Vec<Heal> {
        let mut s = vec![seam(0.0, 2, Source::File, 0.0)];
        let (mut open, mut reopen, mut retried, mut retries, mut heals, mut now) = (Some(0.0), reopen_at, None, 0, vec![], 0.0);
        while now < 600.0 {
            now += 0.1;
            if open.is_none() && reopen.is_some_and(|t| now >= t) {
                open = reopen.take();
            }
            let Some(since) = open else { continue };
            let Some(start) = due(since, &s, now, NEED) else { continue };
            let d = if freeze { Detect::FreezeStart(start) } else { Detect::SilenceStart(start) };
            let h = heal(&judge(&d, &s, now), since, retried, retries).unwrap();
            heals.push(h);
            if h != Heal::Retry {
                open = None;
                continue;
            }
            // the retry restarts the same video one second on: a new seam
            (retries, retried) = (retries + 1, Some((2, now + 1.0)));
            crate::rules::push_seam(&mut s, seam(now + 1.0, 2, Source::File, now + 1.0));
            open = match after {
                After::Continues => open,
                After::BeginsAgainAtTheSeam => Some(now + 1.5),
                After::Cleared => None,
            };
        }
        heals
    }

    #[test]
    fn a_freeze_that_returns_after_its_retry_escalates_to_a_skip() {
        let v = Verdict::Freeze { index: 3, pos: 125.0 };
        assert_eq!(heal(&v, 15.0, None, 0), Some(Heal::Retry));
        assert_eq!(heal(&v, 15.0, Some((3, 40.0)), 1), Some(Heal::Retry), "a freeze is never content");
        assert_eq!(heal(&v, 15.0, Some((3, 40.0)), 2), Some(Heal::Skip));

        assert_eq!(ten_still_minutes(true, After::Continues, None), [Heal::Retry, Heal::Retry, Heal::Skip]);
        assert_eq!(ten_still_minutes(true, After::BeginsAgainAtTheSeam, None), [Heal::Retry, Heal::Retry, Heal::Skip]);
    }

    /// Ten minutes of "brb" silence that belongs to the video: one retry, then content, never a skip.
    #[test]
    fn a_silence_that_returns_after_its_retry_is_content() {
        let v = Verdict::Silence { index: 3, pos: 125.0 };
        assert_eq!(heal(&v, 15.0, None, 0), Some(Heal::Retry));
        assert_eq!(heal(&v, 15.0, Some((3, 40.0)), 5), Some(Heal::Content));
        assert_eq!(heal(&v, 41.0, Some((3, 40.0)), 5), Some(Heal::Content), "re-began at the seam");

        assert_eq!(ten_still_minutes(false, After::Continues, None), [Heal::Retry, Heal::Content]);
        assert_eq!(ten_still_minutes(false, After::BeginsAgainAtTheSeam, None), [Heal::Retry, Heal::Content]);
    }

    #[test]
    fn a_silence_starting_10s_after_the_retry_gets_a_retry() {
        assert_eq!(heal(&Verdict::Silence { index: 3, pos: 80.0 }, 50.0, Some((3, 40.0)), 0), Some(Heal::Retry));
    }

    #[test]
    fn a_later_separate_stretch_gets_one_retry_again() {
        // retried at 40 s; the sound came back, and a new silence began at 200 s
        assert_eq!(heal(&Verdict::Silence { index: 3, pos: 300.0 }, 200.0, Some((3, 40.0)), 0), Some(Heal::Retry));
        // another video's retry never makes this one content
        assert_eq!(heal(&Verdict::Silence { index: 4, pos: 5.0 }, 41.0, Some((3, 40.0)), 0), Some(Heal::Retry));
        assert_eq!(heal(&Verdict::Ignore, 41.0, Some((3, 40.0)), 0), None);
    }

    #[test]
    fn a_freeze_cleared_by_its_retry_stays_healed() {
        // cleared, then a new stretch on the same video long after: a retry again, not content or a skip
        assert_eq!(ten_still_minutes(true, After::Cleared, Some(200.0)), [Heal::Retry, Heal::Retry]);
        assert_eq!(ten_still_minutes(false, After::Cleared, Some(200.0)), [Heal::Retry, Heal::Retry]);
    }

    #[test]
    fn judge_ignores_end_lines() {
        assert_eq!(judge(&Detect::FreezeEnd(30.0), &seams(), 35.0), Verdict::Ignore);
        assert_eq!(judge(&Detect::SilenceEnd(30.0), &seams(), 35.0), Verdict::Ignore);
    }
}
