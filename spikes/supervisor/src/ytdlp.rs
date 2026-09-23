//! yt-dlp (playlist, resolve, download) and the PO-token server (spike 4's arguments).

use crate::procs::{command, kill_tree};
use crate::Logs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const PLAYER_CLIENT: &str = "mweb,web_embedded";
pub const FORMAT: &str = "bv*[height<=1080][vcodec^=avc1]+ba[ext=m4a]/bv*[height<=1080]+ba/b[height<=1080]";

pub struct Ytdlp {
    pub exe: String,
    /// deno next to yt-dlp.exe (where the release puts it); otherwise yt-dlp looks on PATH.
    pub deno: Option<String>,
    pub limit_rate: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub duration: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub video: String,
    pub audio: String,
    pub headers: String,
    pub duration: Option<f64>,
    pub bytes: Option<u64>,
    pub expire: Option<u64>,
    /// googlevideo answers 403 until this unix time (yt-dlp's "sleeping as required by the site").
    pub available_at: Option<u64>,
}

fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

/// 2.x describeYtdlError: the last ERROR line, with the bot check spelled out.
pub fn describe_error(stderr: &str, code: Option<i32>) -> String {
    if stderr.to_lowercase().contains("confirm you") && stderr.to_lowercase().contains("not a bot") {
        return "YouTube bot check: sign in to confirm you're not a bot".into();
    }
    stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .or(stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .map(|l| l.trim().chars().take(300).collect())
        .unwrap_or_else(|| format!("yt-dlp exit {code:?}"))
}

pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub cancelled: bool,
}

impl Ytdlp {
    fn base(&self) -> Vec<String> {
        let mut a = vec!["--no-warnings".to_string(), "--no-update".into()];
        if let Some(d) = &self.deno {
            a.extend(["--js-runtimes".into(), format!("deno:{d}")]);
        }
        a.extend(["--extractor-args".into(), format!("youtube:player_client={PLAYER_CLIENT}")]);
        a
    }

    /// Run yt-dlp; stdout lines go to `on_line`, `[pot` lines and all stderr to ytdlp.log.
    /// `cancel` or `timeout` kill the whole tree.
    pub fn run(&self, args: &[String], logs: &Logs, cancel: &AtomicBool, timeout: Duration, pid: impl FnOnce(u32), mut on_line: impl FnMut(&str)) -> Output {
        let mut child = match command(&self.exe).env("PYTHONIOENCODING", "utf-8").args(self.base()).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => return Output { ok: false, stdout: String::new(), stderr: format!("spawn failed: {e}"), cancelled: false },
        };
        pid(child.id());
        // one line per call to YouTube, so a quiet breaker can be checked from the log
        logs.append("ytdlp.log", &format!("{} [spawn] {}\n", crate::iso_now(), args.last().map_or("", |a| a.as_str())));
        let err_pipe = child.stderr.take().unwrap();
        let err_logs = logs.clone();
        // Line by line and timestamped, so a log line can be placed against events.jsonl.
        let err_thread = std::thread::spawn(move || {
            let mut all = String::new();
            let mut r = BufReader::new(err_pipe);
            let mut buf = vec![];
            while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
                let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                buf.clear();
                err_logs.append("ytdlp.log", &format!("{} {line}\n", crate::iso_now()));
                all.push_str(&line);
                all.push('\n');
            }
            all
        });
        let out_pipe = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        // Lossy on purpose: a reader that stops at the first non-UTF-8 byte closes the pipe,
        // and yt-dlp then dies with "[Errno 22] Invalid argument" on its next print.
        std::thread::spawn(move || {
            let mut r = BufReader::new(out_pipe);
            let mut buf = vec![];
            while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
                let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                buf.clear();
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let started = Instant::now();
        let mut stdout = String::new();
        let mut cancelled = false;
        let status = loop {
            while let Ok(line) = rx.try_recv() {
                if line.contains("[pot") {
                    logs.append("ytdlp.log", &format!("{} {line}\n", crate::iso_now()));
                }
                on_line(&line);
                stdout.push_str(&line);
                stdout.push('\n');
            }
            if let Ok(Some(s)) = child.try_wait() {
                break Some(s);
            }
            if cancel.load(Ordering::Relaxed) || started.elapsed() > timeout {
                cancelled = true;
                kill_tree(child.id());
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        // Drain whatever arrived between the last poll and exit.
        std::thread::sleep(Duration::from_millis(50));
        while let Ok(line) = rx.try_recv() {
            if line.contains("[pot") {
                logs.append("ytdlp.log", &format!("{} {line}\n", crate::iso_now()));
            }
            on_line(&line);
            stdout.push_str(&line);
            stdout.push('\n');
        }
        let stderr = err_thread.join().unwrap_or_default();
        Output { ok: status.is_some_and(|s| s.success()), stdout, stderr, cancelled }
    }

    pub fn playlist(&self, url: &str, logs: &Logs) -> Result<Vec<Entry>, String> {
        let args: Vec<String> = ["--flat-playlist", "--print", "%(id)s\t%(title)s\t%(duration)s", url].map(String::from).to_vec();
        let out = self.run(&args, logs, &AtomicBool::new(false), Duration::from_secs(120), |_| {}, |_| {});
        let entries: Vec<Entry> = out
            .stdout
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                let id = f.next()?.trim().to_string();
                (!id.is_empty()).then(|| Entry { id, title: f.next().unwrap_or("").to_string(), duration: f.next().and_then(|d| d.trim().parse().ok()) })
            })
            .collect();
        if entries.is_empty() {
            return Err(describe_error(&out.stderr, None));
        }
        Ok(entries)
    }

    pub fn resolve(&self, id: &str, logs: &Logs) -> Result<Resolved, String> {
        let args: Vec<String> = ["-j", "--no-playlist", "-f", FORMAT, &watch_url(id)].map(String::from).to_vec();
        let out = self.run(&args, logs, &AtomicBool::new(false), Duration::from_secs(90), |_| {}, |_| {});
        if !out.ok {
            return Err(describe_error(&out.stderr, None));
        }
        let line = out.stdout.lines().find(|l| l.starts_with('{')).ok_or("no JSON from yt-dlp -j")?;
        parse_resolved(line)
    }

    pub fn download(&self, id: &str, file: &Path, logs: &Logs, cancel: &AtomicBool, pid: impl FnOnce(u32), mut progress: impl FnMut(f64)) -> Output {
        let mut args: Vec<String> = ["--no-playlist", "--newline", "--no-continue", "-f", FORMAT, "--merge-output-format", "mp4"].map(String::from).to_vec();
        if let Some(r) = &self.limit_rate {
            args.extend(["--limit-rate".into(), r.clone()]);
        }
        args.extend(["-o".into(), file.to_string_lossy().into_owned(), watch_url(id)]);
        self.run(&args, logs, cancel, Duration::from_secs(6 * 3600), pid, |line| {
            if let Some(p) = parse_download_progress(line) {
                progress(p);
            }
        })
    }
}

pub fn parse_download_progress(line: &str) -> Option<f64> {
    let rest = line.strip_prefix("[download]")?.trim_start();
    let num = rest.split('%').next()?;
    if num.len() == rest.len() {
        return None;
    }
    num.trim().parse().ok()
}

pub fn parse_resolved(json: &str) -> Result<Resolved, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("bad yt-dlp JSON: {e}"))?;
    let headers_of = |h: &serde_json::Value| -> String {
        h.as_object()
            .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| format!("{k}: {v}\r\n"))).collect())
            .unwrap_or_default()
    };
    let size_of = |f: &serde_json::Value| f["filesize"].as_u64().or(f["filesize_approx"].as_u64());
    let available_at = v["requested_formats"].as_array().into_iter().flatten().chain([&v]).filter_map(|f| f["available_at"].as_u64()).max();
    let (video, audio, headers, bytes) = match v["requested_formats"].as_array() {
        Some(f) if f.len() >= 2 => (
            f[0]["url"].as_str(),
            f[1]["url"].as_str(),
            headers_of(&f[0]["http_headers"]),
            size_of(&f[0]).zip(size_of(&f[1])).map(|(a, b)| a + b),
        ),
        _ => (v["url"].as_str(), v["url"].as_str(), headers_of(&v["http_headers"]), size_of(&v)),
    };
    let (Some(video), Some(audio)) = (video, audio) else { return Err("yt-dlp -j gave no stream URLs".into()) };
    Ok(Resolved {
        expire: crate::rules::parse_expire(video),
        video: video.into(),
        audio: audio.into(),
        headers,
        duration: v["duration"].as_f64(),
        bytes: bytes.or(v["filesize_approx"].as_u64()),
        available_at,
    })
}

pub fn ping(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if s.write_all(b"GET /ping HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 12];
    s.read_exact(&mut buf).is_ok() && buf.starts_with(b"HTTP/1.") && &buf[9..12] == b"200"
}

pub fn spawn_token_server(script: &str, port: u16, logs: &Logs) -> std::io::Result<Child> {
    let mut child = command("node").args([script, "-H", "127.0.0.1", "-p", &port.to_string()]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten() {
        let logs = logs.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                logs.append("tokenserver.log", &format!("{line}\n"));
            }
        });
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_takes_both_dash_urls_and_headers() {
        let json = r#"{"duration":596.5,"requested_formats":[
            {"url":"https://v/videoplayback?expire=1790000000&sig=A","filesize":100,"http_headers":{"User-Agent":"UA"},"available_at":1790000006},
            {"url":"https://a/videoplayback?expire=1790000000&sig=B","filesize_approx":20}]}"#;
        let r = parse_resolved(json).unwrap();
        assert!(r.video.starts_with("https://v/"));
        assert!(r.audio.starts_with("https://a/"));
        assert_eq!(r.headers, "User-Agent: UA\r\n");
        assert_eq!((r.bytes, r.expire, r.duration, r.available_at), (Some(120), Some(1790000000), Some(596.5), Some(1790000006)));
        assert!(parse_resolved(r#"{"title":"x"}"#).is_err());
    }

    #[test]
    fn progress_and_errors() {
        assert_eq!(parse_download_progress("[download]  42.5% of 10MiB"), Some(42.5));
        assert_eq!(parse_download_progress("[download] Destination: x"), None);
        assert_eq!(describe_error("x\nERROR: [youtube] abc: Video unavailable\n", Some(1)), "ERROR: [youtube] abc: Video unavailable");
        assert!(describe_error("ERROR: Sign in to confirm you’re not a bot", None).starts_with("YouTube bot check"));
    }
}
