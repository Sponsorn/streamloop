//! The install layout, relative to the install root. Nothing is looked up on PATH.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Layout {
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

impl Layout {
    pub fn new(root: &Path) -> Layout {
        let at = |rel: &str| rel.split('/').fold(root.to_path_buf(), |p, part| p.join(part));
        Layout {
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

    fn scratch(name: &str, with_app: bool) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sl-common-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(if with_app { d.join("app") } else { d.clone() }).unwrap();
        d
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
        let exe_dir = scratch("exe", true);
        // Act: the exe's folder beats STREAMLOOP_ROOT
        let r = install_root(None, &exe_dir, Some(Path::new("R:/env"))).unwrap();
        // Assert
        assert_eq!(r, Root { dir: exe_dir.clone(), from_exe: true });
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn install_root_takes_the_flag_before_the_exe_folder() {
        // Arrange
        let exe_dir = scratch("flag", true);
        // Act
        let r = install_root(Some(Path::new("R:/flag")), &exe_dir, Some(Path::new("R:/env"))).unwrap();
        // Assert
        assert_eq!(r, Root { dir: PathBuf::from("R:/flag"), from_exe: false });
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn install_root_falls_back_to_the_environment() {
        // Arrange: an exe folder without app\
        let exe_dir = scratch("env", false);
        // Act
        let r = install_root(None, &exe_dir, Some(Path::new("R:/env"))).unwrap();
        // Assert
        assert_eq!(r, Root { dir: PathBuf::from("R:/env"), from_exe: false });
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn install_root_without_any_source_is_an_error() {
        // Arrange
        let exe_dir = scratch("none", false);
        // Act
        let e = install_root(None, &exe_dir, None).unwrap_err();
        // Assert
        assert_eq!(e, "No install root: run streamloop.exe from the install folder, or pass --root.");
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }
}
