//! The install layout, relative to the install root. Nothing is looked up on PATH.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
    pub ffmpeg: PathBuf,
    pub ytdlp: PathBuf,
    pub deno: PathBuf,
    pub plugins: PathBuf,
    /// `streamloop-pot.exe`. An install that took only the exe through the updater has no such file and falls back to
    /// the TypeScript server; `token_server_choice` is the one rule for that.
    pub token_server: PathBuf,
    pub slate: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub exe: PathBuf,
    pub exe_old: PathBuf,
    pub exe_bad: PathBuf,
    pub update_dir: PathBuf,
    pub staged_exe: PathBuf,
    pub landed: PathBuf,
    pub app_dir: PathBuf,
    pub app_logs: PathBuf,
    pub config_example: PathBuf,
    pub stopped_marker: PathBuf,
    pub rolled_back: PathBuf,
    pub setup_report: PathBuf,
    /// The launcher's hold marker: while it exists the supervisor waits for Start.
    pub waiting_for_start: PathBuf,
    /// The update's progress for the dashboard, written while a landing runs.
    pub update_state: PathBuf,
}

/// The token server an install runs.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenServer {
    /// `supervisor\streamloop-pot.exe`.
    Exe(PathBuf),
    /// The TypeScript server an exe-only update keeps until the full ZIP: `node\node.exe` running `pot\build\main.js`.
    Node { node: PathBuf, script: PathBuf },
}

impl TokenServer {
    /// The file that identifies the server: the exe, or the script the fallback runs.
    pub fn path(&self) -> &Path {
        match self {
            TokenServer::Exe(exe) => exe,
            TokenServer::Node { script, .. } => script,
        }
    }
}

impl Layout {
    /// The exe when it exists, else the TypeScript fallback when `node\node.exe` and `pot\build\main.js` both do.
    /// The fallback's paths stay out of `Layout`, so the release layout never requires them.
    pub fn token_server_choice(&self, exists: impl Fn(&Path) -> bool) -> Option<TokenServer> {
        if exists(&self.token_server) {
            return Some(TokenServer::Exe(self.token_server.clone()));
        }
        let node = self.root.join("node").join("node.exe");
        let script = self.root.join("pot").join("build").join("main.js");
        (exists(&node) && exists(&script)).then_some(TokenServer::Node { node, script })
    }

    pub fn new(root: &Path) -> Layout {
        let at = |rel: &str| rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part));
        Layout {
            root: root.to_path_buf(),
            ffmpeg: at("ffmpeg/ffmpeg.exe"),
            ytdlp: at("yt-dlp/yt-dlp.exe"),
            deno: at("yt-dlp/deno.exe"),
            plugins: at("yt-dlp/plugins"),
            token_server: at("supervisor/streamloop-pot.exe"),
            slate: at("supervisor/slate.mp4"),
            config: at("app/config.json"),
            data: at("data"),
            exe: at("streamloop.exe"),
            exe_old: at("streamloop.exe.old"),
            exe_bad: at("streamloop.exe.bad"),
            update_dir: at("_update"),
            staged_exe: at("_update/streamloop.exe"),
            landed: at("_update/landed"),
            app_dir: at("app"),
            app_logs: at("app/logs"),
            config_example: at("app/config.example.json"),
            stopped_marker: at("data/stopped-by-user"),
            rolled_back: at("data/update-rolled-back"),
            setup_report: at("data/setup-check.json"),
            waiting_for_start: at("data/waiting-for-start"),
            update_state: at("data/update-state.json"),
        }
    }
}

/// The newest `yt-dlp-<version>.exe` beside `base`, else `base`. yt-dlp reads its modules from its exe as it runs,
/// so its self-update adds a copy instead of replacing an exe in use.
pub fn ytdlp_to_run(base: &Path) -> PathBuf {
    ytdlp_copies(base).into_iter().next().unwrap_or_else(|| base.to_path_buf())
}

/// The updated copies beside `base`, newest version first.
pub fn ytdlp_copies(base: &Path) -> Vec<PathBuf> {
    let Some(dir) = base.parent() else { return Vec::new() };
    let mut found: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = name.strip_prefix(&format!("{}-", ytdlp_stem(base)?))?.strip_suffix(".exe")?;
            Some((version_parts(version)?, entry.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path).collect()
}

/// The name an update that reached `version` takes; none for a version that is not digits and dots.
pub fn ytdlp_copy(base: &Path, version: &str) -> Option<PathBuf> {
    version_parts(version)?;
    Some(base.with_file_name(format!("{}-{version}.exe", ytdlp_stem(base)?)))
}

/// Where the self-update works, run by nothing else.
pub fn ytdlp_update_copy(base: &Path) -> PathBuf {
    base.with_file_name(format!("{}-update.exe", ytdlp_stem(base).unwrap_or("yt-dlp")))
}

fn ytdlp_stem(base: &Path) -> Option<&str> {
    base.file_stem()?.to_str()
}

fn version_parts(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| if part.bytes().all(|b| b.is_ascii_digit()) { part.parse().ok() } else { None }).collect()
}

/// The install root, and whether it was the exe's own folder (anything else is dev mode).
#[derive(Clone, Debug, PartialEq)]
pub struct Root {
    pub dir: PathBuf,
    pub from_exe: bool,
}

/// `--root`, else the exe's folder when it holds `app\`, else `STREAMLOOP_ROOT`.
pub fn install_root(flag: Option<&Path>, exe_dir: &Path, env_root: Option<&Path>) -> Result<Root, String> {
    if let Some(dir) = flag {
        return Ok(Root { dir: dir.to_path_buf(), from_exe: false });
    }
    if exe_dir.join("app").is_dir() {
        return Ok(Root { dir: exe_dir.to_path_buf(), from_exe: true });
    }
    match env_root {
        Some(dir) => Ok(Root { dir: dir.to_path_buf(), from_exe: false }),
        None => Err("No install root: run streamloop.exe from the install folder, or pass --root.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str, with_app: bool) -> tempfile::TempDir {
        let d = tempfile::Builder::new().prefix(&format!("sl-common-paths-{name}-")).tempdir().unwrap();
        if with_app {
            std::fs::create_dir(d.path().join("app")).unwrap();
        }
        d
    }

    fn ytdlp_folder(name: &str, files: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let root = scratch(&format!("ytdlp-{name}"), false);
        let dir = root.path().join("yt-dlp");
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), file).unwrap();
        }
        (root, dir.join("yt-dlp.exe"))
    }

    #[test]
    fn yt_dlp_runs_from_the_shipped_exe_until_an_update_made_a_copy() {
        // Arrange
        let (_dir, base) = ytdlp_folder("shipped", &["yt-dlp.exe", "deno.exe"]);
        // Act
        let exe = ytdlp_to_run(&base);
        // Assert
        assert_eq!(exe, base);
    }

    #[test]
    fn yt_dlp_runs_from_the_newest_copy_by_version_not_by_name() {
        // Arrange
        let (_dir, base) = ytdlp_folder("newest", &["yt-dlp.exe", "yt-dlp-2026.11.3.exe", "yt-dlp-2026.11.24.232653.exe", "yt-dlp-2026.11.24.exe"]);
        // Act
        let exe = ytdlp_to_run(&base);
        let copies = ytdlp_copies(&base);
        // Assert
        assert_eq!(exe, base.with_file_name("yt-dlp-2026.11.24.232653.exe"));
        let names: Vec<String> = copies.iter().map(|c| c.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["yt-dlp-2026.11.24.232653.exe", "yt-dlp-2026.11.24.exe", "yt-dlp-2026.11.3.exe"]);
    }

    #[test]
    fn the_update_work_copy_and_yt_dlp_leftovers_are_never_run() {
        // Arrange
        let (_dir, base) = ytdlp_folder("leftovers", &["yt-dlp.exe", "yt-dlp-update.exe", "yt-dlp-2026.11.30.exe.old", "yt-dlp-2026.11.30.exe.new", "yt-dlp-.exe"]);
        // Act
        let exe = ytdlp_to_run(&base);
        // Assert
        assert_eq!(exe, base);
        assert_eq!(ytdlp_update_copy(&base), base.with_file_name("yt-dlp-update.exe"));
    }

    #[test]
    fn a_copy_is_named_only_from_a_version_of_digits_and_dots() {
        // Arrange
        let base = Path::new("R:/sl/yt-dlp/yt-dlp.exe");
        // Act
        let named = ["2026.11.24.232653", "2026.11.24", "nightly@2026.11.24", "2026..24", "", "..\\x"].map(|v| ytdlp_copy(base, v));
        // Assert
        assert_eq!(named[0], Some(PathBuf::from("R:/sl/yt-dlp/yt-dlp-2026.11.24.232653.exe")));
        assert_eq!(named[1], Some(PathBuf::from("R:/sl/yt-dlp/yt-dlp-2026.11.24.exe")));
        assert_eq!(named[2..], [None, None, None, None]);
    }

    #[test]
    fn layout_places_every_path_under_the_root() {
        // Arrange
        let root = Path::new("R:/sl");
        // Act
        let l = Layout::new(root);
        // Assert
        assert_eq!(l.ffmpeg, root.join("ffmpeg").join("ffmpeg.exe"));
        assert_eq!(l.config, root.join("app").join("config.json"));
        assert_eq!(l.slate, root.join("supervisor").join("slate.mp4"));
        assert_eq!(l.exe, root.join("streamloop.exe"));
        assert_eq!(l.staged_exe, root.join("_update").join("streamloop.exe"));
        assert_eq!(l.rolled_back, root.join("data").join("update-rolled-back"));
        assert_eq!(l.app_logs, root.join("app").join("logs"));
    }

    #[test]
    fn the_token_server_is_an_exe_in_the_supervisor_folder() {
        // Arrange
        let root = Path::new("R:/sl");
        // Act
        let l = Layout::new(root);
        // Assert
        assert_eq!(l.token_server, root.join("supervisor").join("streamloop-pot.exe"));
    }

    fn has<'a>(files: &'a [&'a str], root: &'a Path) -> impl Fn(&Path) -> bool + 'a {
        move |p| files.iter().any(|f| root.join(f) == p)
    }

    #[test]
    fn the_rust_token_server_wins_when_it_is_installed() {
        // Arrange
        let root = Path::new("R:/sl");
        let l = Layout::new(root);
        let files = ["supervisor/streamloop-pot.exe", "node/node.exe", "pot/build/main.js"];
        // Act
        let pick = l.token_server_choice(has(&files, root));
        // Assert
        assert_eq!(pick, Some(TokenServer::Exe(root.join("supervisor").join("streamloop-pot.exe"))));
    }

    #[test]
    fn the_typescript_server_runs_when_only_node_and_its_script_are_installed() {
        // Arrange
        let root = Path::new("R:/sl");
        let l = Layout::new(root);
        let files = ["node/node.exe", "pot/build/main.js"];
        // Act
        let pick = l.token_server_choice(has(&files, root)).unwrap();
        // Assert
        let script = root.join("pot").join("build").join("main.js");
        assert_eq!(pick, TokenServer::Node { node: root.join("node").join("node.exe"), script: script.clone() });
        assert_eq!(pick.path(), script);
    }

    #[test]
    fn no_token_server_is_found_with_node_but_no_script() {
        // Arrange
        let root = Path::new("R:/sl");
        let l = Layout::new(root);
        let files = ["node/node.exe"];
        // Act
        let pick = l.token_server_choice(has(&files, root));
        // Assert
        assert_eq!(pick, None);
    }

    #[test]
    fn no_token_server_is_found_with_a_script_but_no_node() {
        // Arrange
        let root = Path::new("R:/sl");
        let l = Layout::new(root);
        let files = ["pot/build/main.js"];
        // Act
        let pick = l.token_server_choice(has(&files, root));
        // Assert
        assert_eq!(pick, None);
    }

    #[test]
    fn the_launcher_hold_marker_is_in_the_data_folder() {
        // Arrange
        let root = Path::new("R:/sl");
        // Act
        let l = Layout::new(root);
        // Assert
        assert_eq!(l.waiting_for_start, root.join("data").join("waiting-for-start"));
    }

    #[test]
    fn the_update_progress_file_is_in_the_data_folder() {
        // Arrange
        let root = Path::new("R:/sl");
        // Act
        let l = Layout::new(root);
        // Assert
        assert_eq!(l.update_state, root.join("data").join("update-state.json"));
    }

    #[test]
    fn install_root_is_the_exe_folder_when_it_holds_app() {
        // Arrange
        let tmp = scratch("exe", true);
        // Act: the exe's folder beats STREAMLOOP_ROOT
        let r = install_root(None, tmp.path(), Some(Path::new("R:/env"))).unwrap();
        // Assert
        assert_eq!(r, Root { dir: tmp.path().to_path_buf(), from_exe: true });
    }

    #[test]
    fn install_root_takes_the_flag_before_the_exe_folder() {
        // Arrange
        let tmp = scratch("flag", true);
        // Act
        let r = install_root(Some(Path::new("R:/flag")), tmp.path(), Some(Path::new("R:/env"))).unwrap();
        // Assert
        assert_eq!(r, Root { dir: PathBuf::from("R:/flag"), from_exe: false });
    }

    #[test]
    fn install_root_falls_back_to_the_environment() {
        // Arrange: an exe folder without app\
        let tmp = scratch("env", false);
        // Act
        let r = install_root(None, tmp.path(), Some(Path::new("R:/env"))).unwrap();
        // Assert
        assert_eq!(r, Root { dir: PathBuf::from("R:/env"), from_exe: false });
    }

    #[test]
    fn install_root_without_any_source_is_an_error() {
        // Arrange
        let tmp = scratch("none", false);
        // Act
        let e = install_root(None, tmp.path(), None).unwrap_err();
        // Assert
        assert_eq!(e, "No install root: run streamloop.exe from the install folder, or pass --root.");
    }
}
