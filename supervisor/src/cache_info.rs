//! What each cached file is: its streams as ffmpeg reads them, and the format, player client and
//! finish time of the download that wrote it, kept in `data\cache.json` across restarts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// A file's video and audio streams.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
}

/// ffmpeg's `-i` listing: the first video stream line gives the codec, `WxH` and frame rate, the
/// first audio stream line the audio codec. None without a video stream.
pub fn parse_probe(stderr: &str) -> Option<FileInfo> {
    let stream = |kind: &str| stderr.lines().filter(|l| l.trim_start().starts_with("Stream #")).find_map(|l| l.split_once(kind).map(|(_, rest)| rest.to_string()));
    let video = stream(": Video: ")?;
    let codec = |rest: &str| rest.split_whitespace().next().map(|c| c.trim_end_matches(',').to_string());
    let size = video.split(|c: char| c == ' ' || c == ',').find_map(|t| {
        let (w, h) = t.split_once('x')?;
        Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)).filter(|&(w, h)| w > 0 && h > 0)
    });
    let fps = video.split(", ").find_map(|t| t.strip_suffix(" fps")?.trim().parse::<f64>().ok());
    Some(FileInfo { width: size.map(|s| s.0), height: size.map(|s| s.1), fps, vcodec: codec(&video), acodec: stream(": Audio: ").and_then(|a| codec(&a)) })
}

/// ffmpeg -i takes milliseconds on a local file; this only ends one that hangs.
const PROBE_GUARD: Duration = Duration::from_secs(60);

/// Reads `file`'s streams with the install's ffmpeg. ffmpeg exits 1 with no output file named,
/// so only what it printed counts.
pub fn probe(ffmpeg: &Path, file: &Path) -> Option<FileInfo> {
    let mut child = crate::procs::command(ffmpeg).args(["-hide_banner", "-i"]).arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().ok()?;
    let mut pipe = child.stderr.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    while child.try_wait().ok()?.is_none() {
        if started.elapsed() > PROBE_GUARD {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    parse_probe(&reader.join().ok()?)
}

/// One cached file, as `data\cache.json` keeps it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Record {
    pub info: Option<FileInfo>,
    /// yt-dlp's format ids, `137+140` for video and audio merged.
    pub format: Option<String>,
    pub client: Option<String>,
    pub finished_at: Option<String>,
}

/// The records by video id, and the file they live in.
pub struct Records {
    path: PathBuf,
    map: BTreeMap<String, Record>,
}

impl Records {
    /// A missing, empty or broken file reads as no records; the next save writes it whole.
    pub fn load(path: PathBuf) -> Records {
        let map = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Records { path, map }
    }

    pub fn get(&self, id: &str) -> Option<&Record> {
        self.map.get(id)
    }

    pub fn set(&mut self, id: &str, record: Record) {
        self.map.insert(id.to_string(), record);
    }

    pub fn set_info(&mut self, id: &str, info: FileInfo) {
        self.map.entry(id.to_string()).or_default().info = Some(info);
    }

    /// Drops the records `keep` refuses (files that are gone), then writes the rest.
    pub fn save(&mut self, keep: impl Fn(&str) -> bool) {
        self.map.retain(|id, _| keep(id));
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&self.map).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUDIO: &str = "  Stream #0:0[0x1](und): Audio: aac (LC) (mp4a / 0x6134706D), 44100 Hz, stereo, fltp, 127 kb/s (default)";
    const VIDEO: &str = "  Stream #0:1[0x2](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, progressive), 1920x804 [SAR 1:1 DAR 160:67], 1211 kb/s, 24 fps, 24 tbr, 90k tbn, start 0.041000 (default)";

    #[test]
    fn probe_reads_the_video_stream_when_audio_comes_first() {
        let stderr = format!("Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x.mp4':\n  Duration: 00:04:22.76, start: 0.000000, bitrate: 1345 kb/s\n{AUDIO}\n{VIDEO}\nAt least one output file must be specified\n");

        let info = parse_probe(&stderr).unwrap();

        assert_eq!(info, FileInfo { width: Some(1920), height: Some(804), fps: Some(24.0), vcodec: Some("h264".into()), acodec: Some("aac".into()) });
    }

    #[test]
    fn probe_reads_a_fractional_frame_rate() {
        let stderr = "  Stream #0:0: Video: vp9 (Profile 0), yuv420p(tv), 1280x720, SAR 1:1 DAR 16:9, 29.97 fps, 29.97 tbr, 1k tbn\n";

        let info = parse_probe(stderr).unwrap();

        assert_eq!((info.fps, info.height, info.vcodec.as_deref()), (Some(29.97), Some(720), Some("vp9")));
    }

    #[test]
    fn probe_without_audio_leaves_the_audio_codec_empty() {
        let info = parse_probe(VIDEO).unwrap();

        assert_eq!(info.acodec, None);
    }

    #[test]
    fn probe_without_a_video_stream_is_none() {
        assert_eq!(parse_probe(r"C:\x.mp4: Invalid data found when processing input"), None);
        assert_eq!(parse_probe(AUDIO), None);
    }

    fn temp() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("sl-cache-info-").tempdir().unwrap()
    }

    #[test]
    fn records_read_a_missing_or_broken_file_as_empty() {
        let dir = temp();
        let file = dir.path().join("cache.json");

        let missing = Records::load(file.clone());
        std::fs::write(&file, "{not json").unwrap();
        let broken = Records::load(file.clone());

        assert_eq!(missing.get("a"), None);
        assert_eq!(broken.get("a"), None);
    }

    #[test]
    fn records_survive_a_save_and_load_and_drop_files_that_are_gone() {
        let dir = temp();
        let file = dir.path().join("cache.json");
        let a = Record { info: Some(FileInfo { width: Some(1920), height: Some(1080), fps: Some(30.0), vcodec: Some("h264".into()), acodec: Some("aac".into()) }), format: Some("137+140".into()), client: Some("mweb".into()), finished_at: Some("2026-11-28T09:14:02.000Z".into()) };
        let mut records = Records::load(file.clone());
        records.set("a", a.clone());
        records.set("b", Record::default());

        records.save(|id| id == "a");
        let again = Records::load(file.clone());

        assert_eq!(again.get("a"), Some(&a));
        assert_eq!(again.get("b"), None);
        assert!(!dir.path().join("cache.json.tmp").exists(), "the temporary file was left behind");
    }

    #[test]
    fn setting_info_keeps_the_rest_of_the_record() {
        let dir = temp();
        let mut records = Records::load(dir.path().join("cache.json"));
        records.set("a", Record { client: Some("mweb".into()), ..Record::default() });

        records.set_info("a", FileInfo { height: Some(360), ..FileInfo::default() });

        let a = records.get("a").unwrap();
        assert_eq!((a.client.as_deref(), a.info.as_ref().and_then(|i| i.height)), (Some("mweb"), Some(360)));
    }
}
