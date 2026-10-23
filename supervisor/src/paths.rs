//! The install layout, relative to the install root. Nothing is looked up on PATH.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub ffmpeg: PathBuf,
    pub ytdlp: PathBuf,
    pub deno: PathBuf,
    pub plugins: PathBuf,
    pub token_server: PathBuf,
    pub node: PathBuf,
    pub slate: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
}

pub fn layout(root: &Path) -> Paths {
    let at = |rel: &str| rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part));
    Paths {
        root: root.to_path_buf(),
        ffmpeg: at("ffmpeg/ffmpeg.exe"),
        ytdlp: at("yt-dlp/yt-dlp.exe"),
        deno: at("yt-dlp/deno.exe"),
        plugins: at("yt-dlp/plugins"),
        token_server: at("pot/build/main.js"),
        node: at("node/node.exe"),
        slate: at("supervisor/slate.mp4"),
        config: at("app/config.json"),
        data: at("data"),
    }
}
