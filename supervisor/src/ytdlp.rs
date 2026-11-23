//! yt-dlp (playlist, resolve, download) and the PO-token server (spike 4's arguments).

use crate::procs::{command, kill_tree};
use crate::Logs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use streamloop_common::paths::{Layout as Paths, TokenServer};
use streamloop_common::playlist::{parse_playlist, PRINT_TEMPLATE};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const FORMAT: &str = "bv*[height<=1080][vcodec^=avc1]+ba[ext=m4a]/bv*[height<=1080][dynamic_range=?SDR]+ba/b[height<=1080][dynamic_range=?SDR]/bv*[height<=1080]+ba";

pub struct Ytdlp {
    exe: PathBuf,
    deno: PathBuf,
    plugins: PathBuf,
    ffmpeg: PathBuf,
    /// Changed live by POST /reload.
    pub player_client: Mutex<String>,
    /// `--cookies-from-browser` value, empty for none; changed live by POST /reload.
    pub cookies: Mutex<String>,
    /// Per finished call while cookies are set: Some(yt-dlp's cookie error) when it went on without them.
    cookie_notes: Mutex<Vec<Option<String>>>,
    limit_rate: Option<String>,
    /// The token server the plugin talks to: the one the supervisor watches.
    token_port: u16,
}

pub use streamloop_common::playlist::PlaylistEntry as Entry;

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

/// The cookie error of a call that went on without its cookies; None when the cookies were read,
/// or when the call failed on them (that failure is handled as a cookie problem on its own).
pub fn cookies_skipped(stderr: &str, ok: bool) -> Option<String> {
    if !ok && crate::rules::is_cookie_problem(&describe_error(stderr, None)) {
        return None;
    }
    stderr.lines().find(|l| l.contains("ERROR") && crate::rules::is_cookie_problem(l)).map(|l| l.trim().trim_start_matches("ERROR:").trim().to_string())
}

pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub cancelled: bool,
}

impl Ytdlp {
    pub fn new(paths: &Paths, player_client: &str, limit_rate: Option<String>, token_port: u16) -> Ytdlp {
        Ytdlp { exe: paths.ytdlp.clone(), deno: paths.deno.clone(), plugins: paths.plugins.clone(), ffmpeg: paths.ffmpeg.clone(), player_client: Mutex::new(player_client.into()), cookies: Mutex::default(), cookie_notes: Mutex::default(), limit_rate, token_port }
    }

    pub fn note_cookies(&self, note: Option<String>) {
        self.cookie_notes.lock().unwrap().push(note);
    }

    pub fn take_cookie_notes(&self) -> Vec<Option<String>> {
        std::mem::take(&mut *self.cookie_notes.lock().unwrap())
    }

    /// deno must be named by absolute path: yt-dlp runs in the temp folder, and a relative one fails.
    /// `client` replaces the configured player client list for this call.
    fn base(&self, client: Option<&str>) -> Vec<String> {
        let mut a = vec![
            "--no-update".into(),
            // only the install's plugin, never one from the user's yt-dlp folders
            "--no-plugin-dirs".into(),
            "--plugin-dirs".into(),
            self.plugins.to_string_lossy().into_owned(),
            "--js-runtimes".into(),
            format!("deno:{}", self.deno.to_string_lossy()),
            // the merge of video and audio needs ffmpeg; without this yt-dlp looks on PATH
            "--ffmpeg-location".into(),
            self.ffmpeg.to_string_lossy().into_owned(),
            "--extractor-args".into(),
            format!("youtube:player_client={}", client.map_or_else(|| self.player_client.lock().unwrap().clone(), String::from)),
            "--extractor-args".into(),
            format!("youtubepot-bgutilhttp:base_url=http://127.0.0.1:{}", self.token_port),
        ];
        // No shell between here and yt-dlp, so a profile path with spaces stays one argument.
        let cookies = self.cookies.lock().unwrap();
        if !cookies.is_empty() {
            a.extend(["--cookies-from-browser".into(), cookies.clone()]);
        }
        a
    }

    /// Run yt-dlp; stdout lines go to `on_line`, `[pot` lines and all stderr to ytdlp.log.
    /// `cancel` or `timeout` kill the whole tree.
    pub fn run(&self, client: Option<&str>, args: &[String], logs: &Logs, cancel: &AtomicBool, timeout: Duration, pid: impl FnOnce(u32), mut on_line: impl FnMut(&str)) -> Output {
        let mut child = match command(&self.exe).env("PYTHONIOENCODING", "utf-8").args(self.base(client)).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => return Output { ok: false, stdout: String::new(), stderr: format!("spawn failed: {e}"), cancelled: false },
        };
        pid(child.id());
        // one line per call to YouTube, so a quiet breaker can be checked from the log
        logs.daily("ytdlp", "log", &format!("{} [spawn] {}\n", crate::iso_now(), args.last().map_or("", |a| a.as_str())));
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
                err_logs.daily("ytdlp", "log", &format!("{} {line}\n", crate::iso_now()));
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
                    logs.daily("ytdlp", "log", &format!("{} {line}\n", crate::iso_now()));
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
                logs.daily("ytdlp", "log", &format!("{} {line}\n", crate::iso_now()));
            }
            on_line(&line);
            stdout.push_str(&line);
            stdout.push('\n');
        }
        let stderr = err_thread.join().unwrap_or_default();
        let ok = status.is_some_and(|s| s.success());
        if !cancelled && !self.cookies.lock().unwrap().is_empty() {
            self.note_cookies(cookies_skipped(&stderr, ok));
        }
        Output { ok, stdout, stderr, cancelled }
    }

    pub fn playlist(&self, url: &str, logs: &Logs) -> Result<Vec<Entry>, String> {
        let args: Vec<String> = ["--flat-playlist", "--print", PRINT_TEMPLATE, url].map(String::from).to_vec();
        let out = self.run(None, &args, logs, &AtomicBool::new(false), Duration::from_secs(120), |_| {}, |_| {});
        let entries = parse_playlist(&out.stdout);
        if entries.is_empty() {
            return Err(describe_error(&out.stderr, None));
        }
        Ok(entries)
    }

    pub fn resolve(&self, id: &str, logs: &Logs) -> Result<Resolved, String> {
        let args: Vec<String> = ["-j", "--no-playlist", "-f", FORMAT, &watch_url(id)].map(String::from).to_vec();
        let out = self.run(None, &args, logs, &AtomicBool::new(false), Duration::from_secs(90), |_| {}, |_| {});
        if !out.ok {
            return Err(describe_error(&out.stderr, None));
        }
        let line = out.stdout.lines().find(|l| l.starts_with('{')).ok_or("no JSON from yt-dlp -j")?;
        parse_resolved(line)
    }

    pub fn download(&self, client: &str, id: &str, file: &Path, logs: &Logs, cancel: &AtomicBool, pid: impl FnOnce(u32), mut progress: impl FnMut(f64)) -> Output {
        let mut args: Vec<String> = ["--no-playlist", "--newline", "--no-continue", "-f", FORMAT, "--merge-output-format", "mp4"].map(String::from).to_vec();
        if let Some(r) = &self.limit_rate {
            args.extend(["--limit-rate".into(), r.clone()]);
        }
        args.extend(["-o".into(), file.to_string_lossy().into_owned(), watch_url(id)]);
        self.run(Some(client), &args, logs, cancel, Duration::from_secs(6 * 3600), pid, |line| {
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

/// Drops the token server's cached session data and minters, so its next token comes from a new minter.
pub fn invalidate_token_caches(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)) else { return false };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if s.write_all(b"POST /invalidate_caches HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 12];
    s.read_exact(&mut buf).is_ok() && buf.starts_with(b"HTTP/1.") && buf[9] == b'2'
}

/// The arguments of `streamloop-pot.exe`: loopback only, its snapshot and cache under `data\pot`.
pub fn token_server_args(port: u16, data: &Path) -> Vec<String> {
    ["server", "--host", "127.0.0.1", "--port", &port.to_string(), "--data", &data.join("pot").to_string_lossy()].map(String::from).to_vec()
}

/// The program and arguments of the token server to run, from `Layout::token_server_choice`: `streamloop-pot.exe`
/// when it is installed, else the TypeScript server when `node\` and `pot\` are still there (an install that took the
/// exe through the updater, not the full ZIP).
pub fn token_server_command(paths: &Paths, port: u16) -> Option<(PathBuf, Vec<String>)> {
    match paths.token_server_choice(|p| p.is_file())? {
        TokenServer::Exe(exe) => Some((exe, token_server_args(port, &paths.data))),
        TokenServer::Node { node, script } => {
            let args = [script.to_string_lossy().into_owned(), "-H".into(), "127.0.0.1".into(), "-p".into(), port.to_string()];
            Some((node, args.to_vec()))
        }
    }
}

/// Which server answers `/ping`: its `implementation`, else `bgutil VERSION`, else `unknown`. `None` when nothing answers 200.
pub fn server_identity(port: u16) -> Option<String> {
    let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    s.write_all(b"GET /ping HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    // HTTP/1.0 ends the body at EOF; the cap keeps a server that never stops from filling memory.
    let mut raw = Vec::new();
    let _ = s.take(64 * 1024).read_to_end(&mut raw);
    if !(raw.starts_with(b"HTTP/1.") && raw.len() >= 12 && &raw[9..12] == b"200") {
        return None;
    }
    let text = String::from_utf8_lossy(&raw);
    let body = text.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    Some(match (v["implementation"].as_str(), v["version"].as_str()) {
        (Some(i), _) => i.to_string(),
        (None, Some(ver)) => format!("bgutil {ver}"),
        _ => "unknown".into(),
    })
}

pub fn spawn_token_server(cmd: &(PathBuf, Vec<String>), logs: &Logs) -> std::io::Result<Child> {
    let mut child = command(&cmd.0).args(&cmd.1).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten() {
        let logs = logs.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                logs.daily("tokenserver", "log", &format!("{line}\n"));
            }
        });
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_deleted_videos_never_enter_the_playlist() {
        let out = "a1\tFirst\t60\nb2\t[Private video]\tNA\nc3\t[Deleted video]\tNA\nd4\tLast\t12.5\n";

        let ids: Vec<String> = parse_playlist(out).into_iter().map(|e| e.id).collect();

        assert_eq!(ids, ["a1", "d4"]);
    }

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
    fn clearing_the_token_caches_posts_to_invalidate_caches() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            // Closing with request bytes unread resets the connection on Windows, and the reset can beat the reply.
            let (mut req, mut chunk) = (vec![], [0u8; 64]);
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = c.read(&mut chunk).unwrap();
                assert!(n > 0, "the request ended before its headers did");
                req.extend_from_slice(&chunk[..n]);
            }
            c.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });

        let cleared = invalidate_token_caches(port);

        assert!(cleared);
        assert!(server.join().unwrap().starts_with("POST /invalidate_caches HTTP/1.0"));
        assert!(!invalidate_token_caches(1), "nothing listens on port 1");
    }

    /// Serves one `HTTP/1.0 200 OK` answer with `body` on a free port and returns what `server_identity` makes of it.
    fn identity_for(body: &'static str) -> Option<String> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            // Closing with request bytes unread resets the connection on Windows, and the reset can beat the reply.
            let (mut req, mut chunk) = (vec![], [0u8; 64]);
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = c.read(&mut chunk).unwrap();
                assert!(n > 0, "the request ended before its headers did");
                req.extend_from_slice(&chunk[..n]);
            }
            c.write_all(format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{body}").as_bytes()).unwrap();
        });
        let identity = server_identity(port);
        server.join().unwrap();
        identity
    }

    fn root_with(name: &str, files: &[&str]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("sl-token-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in files {
            let p = f.split('/').fold(root.clone(), |p, part| p.join(part));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn the_token_server_binds_loopback_and_keeps_its_state_in_data() {
        let a = token_server_args(4416, Path::new(r"R:\sl\data"));

        assert_eq!(a[..5], ["server", "--host", "127.0.0.1", "--port", "4416"]);
        assert_eq!(a[5..], ["--data".to_string(), Path::new(r"R:\sl\data").join("pot").to_string_lossy().into_owned()]);
    }

    #[test]
    fn server_identity_prefers_the_implementation_then_the_version() {
        assert_eq!(identity_for(r#"{"server_uptime":1,"version":"2.0.0","implementation":"streamloop-pot 0.1.0"}"#), Some("streamloop-pot 0.1.0".into()));
        assert_eq!(identity_for(r#"{"server_uptime":1,"version":"2.0.0"}"#), Some("bgutil 2.0.0".into()));
        assert_eq!(identity_for("not json"), Some("unknown".into()));
        assert_eq!(server_identity(1), None, "nothing listens on port 1");
    }

    #[test]
    fn prefers_the_rust_token_server() {
        let root = root_with("rust", &["supervisor/streamloop-pot.exe", "node/node.exe", "pot/build/main.js"]);
        let paths = Paths::new(&root);

        let (exe, args) = token_server_command(&paths, 4416).unwrap();

        assert_eq!(exe, root.join("supervisor").join("streamloop-pot.exe"));
        assert_eq!(args[0], "server");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn falls_back_to_the_typescript_server_when_only_it_is_installed() {
        let root = root_with("ts", &["node/node.exe", "pot/build/main.js"]);
        let paths = Paths::new(&root);

        let (exe, args) = token_server_command(&paths, 4416).unwrap();

        assert_eq!(exe, root.join("node").join("node.exe"));
        assert_eq!(args[0], root.join("pot").join("build").join("main.js").to_string_lossy());
        assert_eq!(args[1..], ["-H", "127.0.0.1", "-p", "4416"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn names_no_token_server_when_neither_is_installed() {
        let root = root_with("none", &["node/node.exe"]);
        let paths = Paths::new(&root);

        assert!(token_server_command(&paths, 4416).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_call_gets_the_plugin_dir_and_an_absolute_deno() {
        let root = std::env::temp_dir().join("sl-root");
        let yt = Ytdlp::new(&Paths::new(&root), "mweb", None, 4416);
        let a = yt.base(None);
        let after = |flag: &str| a.iter().position(|x| x == flag).map(|i| a[i + 1].clone()).unwrap_or_default();
        assert_eq!(after("--plugin-dirs"), root.join("yt-dlp").join("plugins").to_string_lossy());
        let deno = after("--js-runtimes");
        let path = deno.strip_prefix("deno:").expect("deno: prefix");
        assert!(Path::new(path).is_absolute() && path.ends_with("deno.exe"), "{deno}");
        assert_eq!(after("--extractor-args"), "youtube:player_client=mweb");
        let b = yt.base(None);
        let at = |f: &str| b.iter().position(|x| x == f).unwrap();
        assert!(at("--no-plugin-dirs") < at("--plugin-dirs"), "default plugin folders are cleared first");
        assert!(b.windows(2).any(|w| w[0] == "--extractor-args" && w[1] == "youtubepot-bgutilhttp:base_url=http://127.0.0.1:4416"));
        let b = yt.base(Some("web_embedded"));
        assert_eq!(b[b.iter().position(|x| x == "--extractor-args").unwrap() + 1], "youtube:player_client=web_embedded");
    }

    #[test]
    fn every_call_keeps_yt_dlp_warnings_for_the_log() {
        // a warning such as "n challenge solving failed" names the cause the final ERROR line leaves out
        let yt = Ytdlp::new(&Paths::new(&std::env::temp_dir().join("sl-root")), "mweb", None, 4416);

        let a = yt.base(None);

        assert!(!a.iter().any(|x| x == "--no-warnings"), "{a:?}");
    }

    #[test]
    fn the_reason_stays_the_error_line_when_warnings_come_first() {
        let stderr = "WARNING: [youtube] x: n challenge solving failed: Some formats may be missing.\nWARNING: Only images are available for download.\nERROR: [youtube] x: Requested format is not available.\n";

        let reason = describe_error(stderr, Some(1));

        assert_eq!(reason, "ERROR: [youtube] x: Requested format is not available.");
    }

    #[test]
    fn the_plugin_talks_to_the_token_port_it_is_given() {
        let yt = Ytdlp::new(&Paths::new(&std::env::temp_dir().join("sl-root")), "mweb", None, 4426);
        assert!(yt.base(None).iter().any(|a| a == "youtubepot-bgutilhttp:base_url=http://127.0.0.1:4426"));
    }

    #[test]
    fn cookies_go_in_as_one_separate_argument_only_when_set() {
        let yt = Ytdlp::new(&Paths::new(&std::env::temp_dir().join("sl-root")), "mweb", None, 4416);
        assert!(!yt.base(None).iter().any(|a| a.contains("cookies")), "{:?}", yt.base(None));

        *yt.cookies.lock().unwrap() = r"firefox:C:\Profiles\x y".into();

        let a = yt.base(None);
        let at = a.iter().position(|x| x == "--cookies-from-browser").expect("--cookies-from-browser");
        assert_eq!(a[at + 1], r"firefox:C:\Profiles\x y");
    }

    #[test]
    fn a_dpapi_failure_before_a_bot_check_is_the_bot_check() {
        let stderr = "ERROR: Failed to decrypt with DPAPI. See  https://github.com/yt-dlp/yt-dlp/issues/10927  for more info\nERROR: [youtube] a: Sign in to confirm you're not a bot\n";

        let reason = describe_error(stderr, Some(1));

        assert!(crate::rules::is_bot_check(&reason) && !crate::rules::is_cookie_problem(&reason), "{reason}");
        assert!(cookies_skipped(stderr, false).is_some_and(|l| l.contains("DPAPI")), "the unused cookies went unnoticed");
    }

    #[test]
    fn cookies_skipped_only_when_the_call_went_on_without_them() {
        let dpapi = "ERROR: Failed to decrypt with DPAPI. See  https://github.com/yt-dlp/yt-dlp/issues/10927  for more info";
        assert_eq!(cookies_skipped(&format!("{dpapi}\n"), true).as_deref(), Some(dpapi.trim_start_matches("ERROR:").trim()));
        // the call failed on the cookies themselves: that is the critical path, not this one
        assert_eq!(cookies_skipped(&format!("{dpapi}\n"), false), None);
        assert_eq!(cookies_skipped("[youtube] a: Downloading webpage\n", true), None);
    }

    #[test]
    fn every_call_names_the_bundled_ffmpeg_for_the_merge() {
        let root = std::env::temp_dir().join("sl-root");
        let yt = Ytdlp::new(&Paths::new(&root), "mweb", None, 4416);
        let a = yt.base(None);
        let at = a.iter().position(|x| x == "--ffmpeg-location").expect("--ffmpeg-location");
        assert_eq!(a[at + 1], root.join("ffmpeg").join("ffmpeg.exe").to_string_lossy());
    }

    #[test]
    fn progress_and_errors() {
        assert_eq!(parse_download_progress("[download]  42.5% of 10MiB"), Some(42.5));
        assert_eq!(parse_download_progress("[download] Destination: x"), None);
        assert_eq!(describe_error("x\nERROR: [youtube] abc: Video unavailable\n", Some(1)), "ERROR: [youtube] abc: Video unavailable");
        assert!(describe_error("ERROR: Sign in to confirm you’re not a bot", None).starts_with("YouTube bot check"));
    }

    #[test]
    fn format_fallbacks_prefer_sdr_video() {
        // yt-dlp puts the "allow unknown" ? after the operator; "=SDR?" is a syntax error it raises
        // at selection time, which would fail every resolve that reaches the fallbacks.
        let branches: Vec<&str> = FORMAT.split('/').collect();
        assert!(branches[0].contains("vcodec^=avc1"));
        let (sdr, last) = branches[1..].split_at(branches.len() - 2);
        for b in sdr {
            assert!(b.contains("[dynamic_range=?SDR]"), "{b}");
        }
        // an upload offered only in HDR still plays rather than failing the resolve
        assert_eq!(last, ["bv*[height<=1080]+ba"]);
        assert!(!FORMAT.contains("SDR?"));
    }
}
