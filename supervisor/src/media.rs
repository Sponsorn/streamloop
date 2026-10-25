//! ffmpeg arguments (spike 1), PES chunker, read-ahead queue, pumps and the encoder writer.
//! The control loop only takes the queue lock briefly; it never moves media bytes.

use crate::rules::{self, is_video_stream, parse_packet, unwrap_pts, FRAME, PKT};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::ChildStdin;
use std::sync::{Condvar, Mutex};
use std::time::Instant;


pub enum Input<'a> {
    File(&'a str),
    Url { video: &'a str, audio: &'a str, headers: &'a str },
}

const VF: &str = "scale=1920:1080:force_original_aspect_ratio=decrease:in_color_matrix=auto:out_color_matrix=bt709:out_range=tv,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,fps=30,format=yuv420p,setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709";

const AF: &str = "aresample=48000,aformat=sample_fmts=s16:channel_layouts=stereo";

/// Debug faults (`freeze-video`, `mute-audio`): the feeder's own output goes still or silent,
/// so the encoder's watchdog sees a real fault. Added before the normalisation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FeederFault {
    Freeze,
    Mute,
}

/// `overlay` is the filter script from `overlay::prepare` (`[norm]` in, `[outv]` out); an
/// unreadable script means no overlay. With it the video chain becomes one `-filter_complex`
/// graph: fault (if any), the normalisation, then the script, so the overlay is drawn in output
/// coordinates and a freeze fault still freezes the picture under it.
pub fn feeder_args(input: &Input, pos: f64, offset: i64, fault: Option<FeederFault>, overlay: Option<&std::path::Path>) -> Vec<String> {
    let mut a: Vec<String> = ["-v", "error", "-nostats", "-progress", "pipe:2"].map(String::from).to_vec();
    let seek = |a: &mut Vec<String>| {
        if pos > 0.0 {
            a.extend(["-ss".into(), format!("{pos:.3}")]);
        }
    };
    match input {
        Input::File(f) => {
            seek(&mut a);
            a.extend(["-i".into(), f.to_string()]);
        }
        Input::Url { video, audio, headers } => {
            for url in [video, audio] {
                // 403 is not reconnected: that URL is dead and the ladder re-resolves it.
                a.extend(
                    ["-reconnect", "1", "-reconnect_on_network_error", "1", "-reconnect_on_http_error", "5xx", "-reconnect_delay_max", "4"]
                        .map(String::from),
                );
                if !headers.is_empty() {
                    a.extend(["-headers".into(), headers.to_string()]);
                }
                seek(&mut a);
                a.extend(["-i".into(), url.to_string()]);
            }
        }
    }
    let script = overlay.and_then(|p| std::fs::read_to_string(p).ok());
    let (vmap, amap) = match (&script, input) {
        (Some(_), Input::File(_)) => ("[outv]", "0:a:0"),
        (Some(_), Input::Url { .. }) => ("[outv]", "1:a:0"),
        (None, Input::File(_)) => ("", ""),
        (None, Input::Url { .. }) => ("0:v:0", "1:a:0"),
    };
    if !vmap.is_empty() {
        a.extend(["-map", vmap, "-map", amap].map(String::from));
    }
    // Overlaid on the input's frames to keep its pace: `loop` alone runs minutes ahead of the audio
    // and the ingest drops the encoder (and `loop=-1` without size=1 loops nothing).
    let (vf, af) = match fault {
        Some(FeederFault::Freeze) => (format!("split[m][s];[s]trim=end_frame=1,loop=loop=-1:size=1[st];[m][st]overlay=shortest=1,{VF}"), AF.to_string()),
        Some(FeederFault::Mute) => (VF.to_string(), format!("volume=0,{AF}")),
        None => (VF.to_string(), AF.to_string()),
    };
    match script {
        // vf here is the fault prefix + normalisation, which becomes the first stage of the graph.
        Some(body) => a.extend(["-filter_complex".into(), format!("[0:v]{vf}[norm];{body}"), "-af".into(), af]),
        None => a.extend(["-vf".into(), vf, "-af".into(), af]),
    }
    a.extend(
        [
            "-c:v", "mpeg2video", "-q:v", "2", "-g", "15", "-bf", "0", "-c:a", "mp2", "-b:a", "384k",
            "-shortest",
            "-muxdelay", "0", "-muxpreload", "0", "-mpegts_flags", "+initial_discontinuity",
            "-output_ts_offset",
        ]
        .map(String::from),
    );
    a.push(format!("{:.6}", rules::secs(offset)));
    a.extend(["-f", "mpegts", "pipe:1"].map(String::from));
    a
}

/// Detector durations for the encoder's freezedetect and silencedetect.
#[derive(Clone, Copy)]
pub struct WatchdogCfg {
    pub freeze_s: f64,
    pub silence_s: f64,
}

impl Default for WatchdogCfg {
    fn default() -> Self {
        WatchdogCfg { freeze_s: 20.0, silence_s: 30.0 }
    }
}

/// Spike 2's per-vendor video sets: CBR 4500k, fixed 60-frame GOP, no B-frames, High profile.
fn video_set(codec: &str) -> Option<&'static [&'static str]> {
    Some(match codec {
        "h264_nvenc" => &["-preset", "p4", "-rc", "cbr", "-bf", "0", "-profile:v", "high"],
        "h264_amf" => &["-usage", "transcoding", "-quality", "balanced", "-rc", "cbr", "-bf", "0", "-profile:v", "high"],
        // QSV has no -rc: -b:v equal to -maxrate selects CBR.
        "h264_qsv" => &["-preset", "medium", "-look_ahead", "0", "-bf", "0", "-profile:v", "high"],
        "libx264" => &["-preset", "veryfast", "-sc_threshold", "0", "-keyint_min", "60"],
        _ => return None,
    })
}

/// The watchdog's detectors sit in the encoder's own chains (no second output: a critical output
/// is never teed); `-v info` lets their lines reach stderr. Every stderr line is logged redacted.
pub fn encoder_args(target: &str, codec: &str, watchdog: &WatchdogCfg) -> Result<Vec<String>, String> {
    let set = video_set(codec).ok_or_else(|| format!("unknown encoder \"{codec}\" (h264_nvenc, h264_amf, h264_qsv, libx264)"))?;
    // -50dB, not -60dB: the feeder's mpeg2 (q 2, GOP 15) puts a still picture of real content
    // 52-58 dB apart at every I-frame, so -60dB never sees it frozen.
    let vf = format!("freezedetect=n=-50dB:d={}", watchdog.freeze_s);
    let af = format!("aresample=async=1:min_hard_comp=0.01,silencedetect=n=-50dB:d={}", watchdog.silence_s);
    let mut a: Vec<&str> = vec!["-v", "info", "-nostats", "-stats_period", "0.1", "-progress", "pipe:2", "-re", "-f", "mpegts", "-i", "pipe:0", "-c:v", codec];
    a.extend(set);
    a.extend([
        "-b:v", "4500k", "-maxrate", "4500k", "-bufsize", "9000k", "-g", "60", "-pix_fmt", "yuv420p",
        "-vf", &vf, "-af", &af, "-c:a", "aac", "-b:a", "160k", "-ar", "48000",
        "-f", "flv", target,
    ]);
    Ok(a.into_iter().map(String::from).collect())
}


/// One queue entry: a video PES and the packets interleaved with it, up to the next video PES.
pub struct Chunk {
    pub feeder: u64,
    pub data: Vec<u8>,
    /// Unwrapped PTS of the video PES this chunk carries.
    pub pts: i64,
}

/// Holds back the video PES in progress until the next one starts, so the queue only ever
/// holds complete PES units. Trimming a killed feeder = dropping `pending`.
pub struct Chunker {
    carry: Vec<u8>,
    pending: Vec<u8>,
    pending_pts: Option<i64>,
    video_pid: Option<u16>,
    reference: i64,
    pub resyncs: u32,
}

impl Chunker {
    pub fn new(offset: i64) -> Self {
        Chunker { carry: vec![], pending: vec![], pending_pts: None, video_pid: None, reference: offset, resyncs: 0 }
    }

    /// Feed raw bytes; returns completed (data, pts) units.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<(Vec<u8>, i64)> {
        self.carry.extend_from_slice(bytes);
        let mut out = vec![];
        let mut i = 0;
        while i + PKT <= self.carry.len() {
            if self.carry[i] != 0x47 {
                self.resyncs += 1;
                i += 1;
                continue;
            }
            let pkt = &self.carry[i..i + PKT];
            let p = parse_packet(pkt).unwrap();
            let starts_video = p.pusi
                && (Some(p.pid) == self.video_pid || p.stream_id.is_some_and(is_video_stream));
            if starts_video {
                self.video_pid = Some(p.pid);
                if let Some(pts) = self.pending_pts.take() {
                    out.push((std::mem::take(&mut self.pending), pts));
                }
                let pts = p.pts.map(|raw| unwrap_pts(raw, self.reference)).unwrap_or(self.reference);
                self.reference = pts;
                self.pending_pts = Some(pts);
            }
            self.pending.extend_from_slice(pkt);
            i += PKT;
        }
        self.carry.drain(..i);
        out
    }

    /// The last unit, for a feeder that ended cleanly.
    pub fn finish(&mut self) -> Option<(Vec<u8>, i64)> {
        let pts = self.pending_pts.take()?;
        Some((std::mem::take(&mut self.pending), pts))
    }
}


pub struct Live {
    pub seq: u64,
    pub last_pts: Option<i64>,
    pub frames: u64,
    pub last_push: Instant,
    pub blocked: bool,
}

pub struct Queue {
    pub chunks: VecDeque<Chunk>,
    pub bytes: usize,
    pub cap: usize,
    /// PTS of the last video PES handed to the encoder.
    pub written_pts: Option<i64>,
    pub live: Option<Live>,
    /// Encoder stdin for the writer; replaced on an encoder restart.
    pub stdin: Option<(u64, ChildStdin)>,
    pub generation: u64,
    pub shutdown: bool,
    /// Timeline the viewer is at, and how far past it a feeder may run (the read-ahead cap).
    pub view: i64,
    pub ahead: i64,
}

impl Queue {
    pub fn new(cap: usize) -> Self {
        Queue { chunks: VecDeque::new(), bytes: 0, cap, written_pts: None, live: None, stdin: None, generation: 0, shutdown: false, view: rules::BASE, ahead: i64::MAX }
    }

    /// Timeline offset where the next feeder continues: last video PTS in the pipe + one frame.
    pub fn end(&self) -> Option<i64> {
        self.chunks.back().map(|c| c.pts).or(self.written_pts).map(|p| p + FRAME)
    }

    /// Drop every queued chunk of feeder `from` and later ones (all of them with 0).
    /// Chunks start at a video PES, so the cut lands on the first video PES after the writer.
    pub fn flush_from(&mut self, from: u64) -> usize {
        let mut dropped = 0;
        while self.chunks.back().is_some_and(|c| c.feeder >= from) {
            dropped += self.chunks.pop_back().unwrap().data.len();
        }
        self.bytes -= dropped;
        dropped
    }

    /// Keep `margin` of media past the writer's last PTS (a URL feeder needs a head start before
    /// the encoder starves), then drop feeder `from`'s chunks from the next video PES onward.
    pub fn flush_keeping(&mut self, from: u64, margin: i64) -> usize {
        let Some(limit) = self.written_pts.or(self.chunks.front().map(|c| c.pts)).map(|w| w + margin) else { return 0 };
        let mut dropped = 0;
        while self.chunks.back().is_some_and(|c| c.feeder >= from && c.pts >= limit) {
            dropped += self.chunks.pop_back().unwrap().data.len();
        }
        self.bytes -= dropped;
        dropped
    }

    pub fn push(&mut self, feeder: u64, data: Vec<u8>, pts: i64) -> bool {
        let Some(live) = self.live.as_mut().filter(|l| l.seq == feeder) else { return false };
        live.last_pts = Some(pts);
        live.frames += 1;
        live.last_push = Instant::now();
        self.bytes += data.len();
        self.chunks.push_back(Chunk { feeder, data, pts });
        true
    }
}

pub struct Shared {
    pub q: Mutex<Queue>,
    pub cv: Condvar,
}

/// Feeder stdout -> queue. Returns (frames, last_pts) of what the queue accepted.
pub fn pump(shared: &Shared, seq: u64, offset: i64, mut out: impl Read, clean_exit: impl FnOnce() -> bool) -> (u64, Option<i64>, u32) {
    let mut chunker = Chunker::new(offset);
    let mut buf = vec![0u8; 256 * 1024];
    let (mut frames, mut last) = (0, None);
    let mut accept = |data: Vec<u8>, pts: i64| -> bool {
        let mut q = shared.q.lock().unwrap();
        loop {
            if q.live.as_ref().is_none_or(|l| l.seq != seq) || q.shutdown {
                return false;
            }
            // Back-pressure: the byte cap, and the read-ahead in media seconds, as spike 4's gate.
            if q.bytes < q.cap && pts - q.view <= q.ahead {
                break;
            }
            q.live.as_mut().unwrap().blocked = true;
            q = shared.cv.wait(q).unwrap();
        }
        if let Some(l) = q.live.as_mut() {
            l.blocked = false;
        }
        q.push(seq, data, pts);
        shared.cv.notify_all();
        frames += 1;
        last = Some(pts);
        true
    };
    let mut alive = true;
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if !alive {
            continue; // drain so the process can exit
        }
        for (data, pts) in chunker.feed(&buf[..n]) {
            if !accept(data, pts) {
                alive = false;
                break;
            }
        }
    }
    // A killed or failed feeder's last PES may be cut short: it is dropped (the trim).
    if alive && clean_exit() {
        if let Some((data, pts)) = chunker.finish() {
            accept(data, pts);
        }
    }
    (frames, last, chunker.resyncs)
}

/// Queue -> encoder stdin, forever. Takes the newest stdin after an encoder restart.
pub fn writer(shared: &Shared) {
    let mut stdin: Option<(u64, ChildStdin)> = None;
    loop {
        let chunk = {
            let mut q = shared.q.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(s) = q.stdin.take() {
                    stdin = Some(s);
                }
                if stdin.as_ref().is_some_and(|(g, _)| *g != q.generation) {
                    stdin = None;
                }
                if stdin.is_some() && !q.chunks.is_empty() {
                    break;
                }
                q = shared.cv.wait(q).unwrap();
            }
            let c = q.chunks.pop_front().unwrap();
            q.bytes -= c.data.len();
            q.written_pts = Some(c.pts);
            shared.cv.notify_all();
            c
        };
        if let Some((_, s)) = stdin.as_mut() {
            if s.write_all(&chunk.data).is_err() {
                stdin = None; // encoder gone; the watchdog restarts it
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn encoder_args_carry_each_vendors_exact_set() {
        let sets: [(&str, &[&str]); 4] = [
            ("h264_nvenc", &["-preset", "p4", "-rc", "cbr", "-bf", "0", "-profile:v", "high"]),
            ("h264_amf", &["-usage", "transcoding", "-quality", "balanced", "-rc", "cbr", "-bf", "0", "-profile:v", "high"]),
            ("h264_qsv", &["-preset", "medium", "-look_ahead", "0", "-bf", "0", "-profile:v", "high"]),
            ("libx264", &["-preset", "veryfast", "-sc_threshold", "0", "-keyint_min", "60"]),
        ];
        for (codec, set) in sets {
            // Act
            let a = super::encoder_args("rtmp://x/y", codec, &super::WatchdogCfg::default()).unwrap();
            // Assert: the whole video half, in order
            let mut want: Vec<&str> = vec!["-c:v", codec];
            want.extend(set);
            want.extend(["-b:v", "4500k", "-maxrate", "4500k", "-bufsize", "9000k", "-g", "60", "-pix_fmt", "yuv420p"]);
            let i = a.iter().position(|x| x == "-c:v").unwrap();
            assert_eq!(a[i..i + want.len()], want[..], "{codec}");
            assert_eq!(a.last().unwrap(), "rtmp://x/y");
        }
    }

    #[test]
    fn encoder_runs_the_detectors_inside_its_chains_at_info_level() {
        let w = super::WatchdogCfg { freeze_s: 20.0, silence_s: 30.5 };
        // Act
        let a = super::encoder_args("rtmp://x/y", "h264_nvenc", &w).unwrap();
        // Assert
        let after = |flag: &str| a[a.iter().position(|x| x == flag).unwrap() + 1].clone();
        assert_eq!(after("-v"), "info");
        assert_eq!(after("-vf"), "freezedetect=n=-50dB:d=20");
        assert_eq!(after("-af"), "aresample=async=1:min_hard_comp=0.01,silencedetect=n=-50dB:d=30.5");
        assert_eq!(a.iter().filter(|x| x.starts_with("rtmp://")).count(), 1, "one output only");
    }

    #[test]
    fn feeder_faults_change_only_their_own_chain_before_the_normalisation() {
        let input = super::Input::File("v.mp4");
        let chain = |fault, flag: &str| {
            let a = super::feeder_args(&input, 0.0, crate::rules::BASE, fault, None);
            a[a.iter().position(|x| x == flag).unwrap() + 1].clone()
        };
        let (vf, af) = (chain(None, "-vf"), chain(None, "-af"));
        assert_eq!(chain(Some(super::FeederFault::Freeze), "-vf"), format!("split[m][s];[s]trim=end_frame=1,loop=loop=-1:size=1[st];[m][st]overlay=shortest=1,{vf}"));
        assert_eq!(chain(Some(super::FeederFault::Freeze), "-af"), af);
        assert_eq!(chain(Some(super::FeederFault::Mute), "-af"), format!("volume=0,{af}"));
        assert_eq!(chain(Some(super::FeederFault::Mute), "-vf"), vf);
    }

    #[test]
    fn feeder_filter_fixes_colour_tags() {
        // Arrange: the slate is a File input, so it shares the file feeder's chain
        let tail = "format=yuv420p,setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709";
        let inputs = [super::Input::File("v.mp4"), super::Input::File("slate.mp4"), super::Input::Url { video: "v", audio: "a", headers: "" }];
        for input in &inputs {
            // Act
            let args = super::feeder_args(input, 0.0, crate::rules::BASE, None, None);
            let vf = args[args.iter().position(|x| x == "-vf").unwrap() + 1].clone();
            // Assert
            assert!(vf.ends_with(tail), "{vf}");
            // converted, not just relabelled: the matrix and range follow the source into bt709 tv
            assert!(vf.starts_with("scale=1920:1080:force_original_aspect_ratio=decrease:in_color_matrix=auto:out_color_matrix=bt709:out_range=tv,"), "{vf}");
        }
    }

    #[test]
    fn overlay_script_goes_after_the_normalisation_and_after_a_fault() {
        let dir = std::env::temp_dir().join(format!("sl-media-ov-{}", std::process::id()));
        let script = crate::overlay::prepare("Title", Some(1200.0), 600.0, &dir).unwrap();
        let body = std::fs::read_to_string(&script).unwrap();
        let args = |input: &super::Input, fault| super::feeder_args(input, 600.0, crate::rules::BASE, fault, Some(&script));
        let after = |a: &[String], flag: &str| a[a.iter().position(|x| x == flag).unwrap() + 1].clone();
        let file = args(&super::Input::File("v.mp4"), None);
        // Assert: no -vf, one graph = normalisation, then the script; video and audio mapped
        assert!(!file.contains(&"-vf".to_string()));
        assert_eq!(after(&file, "-filter_complex"), format!("[0:v]{}[norm];{body}", super::VF));
        assert_eq!(file.iter().filter(|x| *x == "-map").count(), 2);
        assert_eq!(after(&file, "-af"), super::AF);
        let url = args(&super::Input::Url { video: "v", audio: "a", headers: "" }, None);
        assert_eq!(url.iter().filter(|x| *x == "-map").count(), 2, "url maps are not doubled");
        assert!(url.windows(2).any(|w| w == ["-map", "[outv]"]) && url.windows(2).any(|w| w == ["-map", "1:a:0"]));
        // Assert: the freeze goes in front of the normalisation, the mute stays on the audio chain
        let freeze = args(&super::Input::File("v.mp4"), Some(super::FeederFault::Freeze));
        assert_eq!(after(&freeze, "-filter_complex"), format!("[0:v]split[m][s];[s]trim=end_frame=1,loop=loop=-1:size=1[st];[m][st]overlay=shortest=1,{}[norm];{body}", super::VF));
        let mute = args(&super::Input::File("v.mp4"), Some(super::FeederFault::Mute));
        assert_eq!(after(&mute, "-filter_complex"), after(&file, "-filter_complex"));
        assert_eq!(after(&mute, "-af"), format!("volume=0,{}", super::AF));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Manual render check: SL_RENDER_CLIP=<a 1080p clip over 620 s> SL_RENDER_OUT=<out.ts> runs a
    /// 20 s feeder at -ss 600 with the overlay; skipped otherwise.
    #[test]
    fn renders_the_overlay_on_a_feeder_started_mid_video() {
        let (Ok(clip), Ok(out)) = (std::env::var("SL_RENDER_CLIP"), std::env::var("SL_RENDER_OUT")) else { return };
        let dir = std::env::temp_dir().join(format!("sl-render-{}", std::process::id()));
        let script = crate::overlay::prepare("Render check: 日本語 title \u{1F680} with: colons & 'quotes'", Some(1200.0), 600.0, &dir).unwrap();
        let mut args = feeder_args(&Input::File(&clip), 600.0, crate::rules::BASE, None, Some(&script));
        args.retain(|a| a != "-progress" && a != "pipe:2");
        args.splice(0..0, ["-t".to_string(), "20".to_string()]);
        let o = std::process::Command::new(r"C:\ProgramData\chocolatey\lib\ffmpeg\tools\ffmpeg\bin\ffmpeg.exe").args(&args).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        std::fs::write(out, o.stdout).unwrap();
    }

    #[test]
    fn encoder_args_reject_an_unknown_codec() {
        assert!(super::encoder_args("rtmp://x/y", "h265_nope", &super::WatchdogCfg::default()).is_err());
    }

    use super::*;
    use crate::rules::{BASE, HZ};

    fn pkt(pid: u16, pusi: bool, stream_id: u8, pts: i64) -> Vec<u8> {
        let mut p = vec![0xffu8; PKT];
        p[0] = 0x47;
        p[1] = (if pusi { 0x40 } else { 0 }) | (pid >> 8) as u8;
        p[2] = pid as u8;
        p[3] = 0x10;
        if pusi {
            p[4..13].copy_from_slice(&[0, 0, 1, stream_id, 0, 0, 0x80, 0x80, 5]);
            let pts = pts & ((1 << 33) - 1);
            p[13] = 0x21 | (((pts >> 30) & 7) << 1) as u8;
            p[14] = (pts >> 22) as u8;
            p[15] = (((pts >> 15) & 0x7f) << 1) as u8 | 1;
            p[16] = (pts >> 7) as u8;
            p[17] = ((pts & 0x7f) << 1) as u8 | 1;
        }
        p
    }

    /// PAT, then per frame: video PES start, a continuation, an audio PES.
    fn stream(offset: i64, frames: i64) -> Vec<u8> {
        let mut s = pkt(0, true, 0, 0);
        s[4] = 0; // not a PES
        for f in 0..frames {
            s.extend(pkt(0x100, true, 0xe0, offset + f * FRAME));
            s.extend(pkt(0x100, false, 0, 0));
            s.extend(pkt(0x101, true, 0xc0, offset + f * FRAME));
        }
        s
    }

    #[test]
    fn chunker_emits_whole_pes_and_holds_back_the_last() {
        let mut c = Chunker::new(BASE);
        let bytes = stream(BASE, 3);
        // odd split sizes: packet alignment survives arbitrary reads
        let mut out = vec![];
        for part in bytes.chunks(100) {
            out.extend(c.feed(part));
        }
        assert_eq!(out.iter().map(|(_, p)| *p).collect::<Vec<_>>(), [BASE, BASE + FRAME]);
        assert_eq!(out[0].0.len(), 4 * PKT); // PAT + video + continuation + audio
        assert_eq!(out[1].0.len(), 3 * PKT);
        assert_eq!(out[1].0[1] & 0x40, 0x40); // every later chunk starts on a video PUSI
        // trim: a killed feeder never calls finish, so frame 2 is dropped
        let (_, last) = c.finish().unwrap();
        assert_eq!(last, BASE + 2 * FRAME);
    }

    #[test]
    fn chunker_unwraps_across_the_33_bit_wrap() {
        let start = (1i64 << 33) - FRAME;
        let mut c = Chunker::new(start);
        let out = c.feed(&stream(start, 3));
        assert_eq!(out[1].1, 1 << 33);
        assert_eq!(c.finish().unwrap().1, (1 << 33) + FRAME);
    }

    fn live(seq: u64) -> Option<Live> {
        Some(Live { seq, last_pts: None, frames: 0, last_push: Instant::now(), blocked: false })
    }

    #[test]
    fn flush_cuts_after_the_writer_and_continues_one_frame_later() {
        let mut q = Queue::new(1 << 30);
        q.live = live(1);
        for f in 0..5 {
            q.push(1, vec![0; PKT], BASE + f * FRAME);
        }
        // the writer took two chunks
        for _ in 0..2 {
            let c = q.chunks.pop_front().unwrap();
            q.bytes -= c.data.len();
            q.written_pts = Some(c.pts);
        }
        assert_eq!(q.end(), Some(BASE + 5 * FRAME));
        assert_eq!(q.flush_from(0), 3 * PKT);
        assert_eq!(q.bytes, 0);
        assert_eq!(q.end(), Some(BASE + 2 * FRAME));
    }

    #[test]
    fn control_flush_keeps_a_second_past_the_writer_and_cuts_at_the_next_pes() {
        let mut q = Queue::new(1 << 30);
        q.live = live(1);
        for f in 0..90 {
            q.push(1, vec![0; PKT], BASE + f * FRAME);
        }
        // the writer took ten chunks: last written PTS is frame 9
        for _ in 0..10 {
            let c = q.chunks.pop_front().unwrap();
            q.bytes -= c.data.len();
            q.written_pts = Some(c.pts);
        }
        // frames 10..=38 are under 1 s past frame 9; frame 39 is the first at 1.0 s
        assert_eq!(q.flush_keeping(0, HZ), (90 - 39) as usize * PKT);
        assert_eq!(q.chunks.back().unwrap().pts, BASE + 38 * FRAME);
        assert_eq!(q.end(), Some(BASE + 39 * FRAME));
    }

    #[test]
    fn control_flush_with_less_than_the_margin_queued_drops_nothing() {
        let mut q = Queue::new(1 << 30);
        q.live = live(1);
        for f in 0..5 {
            q.push(1, vec![0; PKT], BASE + f * FRAME);
        }
        q.written_pts = Some(BASE);
        assert_eq!(q.flush_keeping(0, HZ), 0);
        assert_eq!(q.end(), Some(BASE + 5 * FRAME));
    }

    #[test]
    fn slate_cut_keeps_a_second_and_never_drops_the_video_before_the_slate() {
        let mut q = Queue::new(1 << 30);
        // a failed feeder left 1.5 s of video, then the slate (feeder 2) covered the retry's wait
        q.live = live(1);
        for f in 0..45 {
            q.push(1, vec![0; PKT], BASE + f * FRAME);
        }
        q.live = live(2);
        for f in 45..120 {
            q.push(2, vec![0; PKT], BASE + f * FRAME);
        }
        q.written_pts = Some(BASE);
        // 1 s past the writer lies inside the old video: all of it stays, the slate goes
        assert_eq!(q.flush_keeping(2, HZ), 75 * PKT);
        assert_eq!(q.end(), Some(BASE + 45 * FRAME));
        // later, with the writer 2 s in, only slate past the second is dropped
        q.live = live(3);
        for f in 45..120 {
            q.push(3, vec![0; PKT], BASE + f * FRAME);
        }
        q.written_pts = Some(BASE + 60 * FRAME);
        assert_eq!(q.flush_keeping(3, HZ), (120 - 90) as usize * PKT);
        assert_eq!(q.end(), Some(BASE + 90 * FRAME));
    }

    #[test]
    fn flush_from_a_feeder_keeps_earlier_feeders() {
        let mut q = Queue::new(1 << 30);
        q.live = live(1);
        q.push(1, vec![0; PKT], BASE);
        q.live = live(2);
        q.push(2, vec![0; PKT], BASE + FRAME);
        assert!(!q.push(1, vec![0; PKT], BASE)); // a dead feeder's bytes are refused
        q.flush_from(2);
        assert_eq!(q.chunks.len(), 1);
        assert_eq!(q.end(), Some(BASE + FRAME));
    }

    #[test]
    fn pump_trims_a_failed_feeder_and_keeps_a_clean_one_whole() {
        for (clean, frames) in [(true, 4), (false, 3)] {
            let shared = Shared { q: Mutex::new(Queue::new(1 << 30)), cv: Condvar::new() };
            shared.q.lock().unwrap().live = live(7);
            let (n, last, _) = pump(&shared, 7, BASE, &stream(BASE, 4)[..], || clean);
            assert_eq!(n, frames);
            assert_eq!(last, Some(BASE + (frames as i64 - 1) * FRAME));
        }
    }

    /// Spike 1 advanced the timeline by frames/30; the TS-derived offset must agree.
    #[test]
    fn ts_offset_matches_spike_1_on_a_real_feeder() {
        let slate = std::path::Path::new("../buffer/media/slate.mp4");
        if !slate.exists() {
            eprintln!("skipped: no slate");
            return;
        }
        let offset = BASE + 3 * HZ + 1234; // an offset that is not a whole frame
        let mut args = feeder_args(&Input::File(slate.to_str().unwrap()), 0.0, offset, None, None);
        args.retain(|a| a != "-progress" && a != "pipe:2");
        let Ok(out) = std::process::Command::new("ffmpeg").args(&args).output() else {
            eprintln!("skipped: no ffmpeg");
            return;
        };
        assert!(out.status.success());
        let mut c = Chunker::new(offset);
        let mut units = c.feed(&out.stdout);
        units.extend(c.finish());
        let frames = units.len() as i64;
        assert_eq!(frames, 300);
        let first = units[0].1;
        let last = units.last().unwrap().1;
        assert!((first - offset).abs() <= 1, "first pts {first} vs offset {offset}");
        assert!((last + FRAME - (offset + frames * FRAME)).abs() <= 1);
    }
}
