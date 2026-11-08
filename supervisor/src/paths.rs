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
        token_server: at("supervisor/streamloop-pot.exe"),
        slate: at("supervisor/slate.mp4"),
        config: at("app/config.json"),
        data: at("data"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_server_is_an_exe_beside_the_supervisor() {
        let root = Path::new(r"R:\sl");

        assert_eq!(layout(root).token_server, root.join("supervisor").join("streamloop-pot.exe"));
    }
}
