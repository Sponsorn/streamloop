//! The temp folder of each yt-dlp run. yt-dlp.exe unpacks about 30 MB into `%TMP%\_MEI…` at every
//! start and removes it only when it exits on its own, so a killed run leaves it behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

/// The folders of this process's runs still going, which no sweep touches.
static LIVE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(0);

fn live() -> MutexGuard<'static, Vec<PathBuf>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// `yt-dlp\temp\`, beside `base`, the shipped exe.
pub fn temp_dir(base: &Path) -> PathBuf {
    base.with_file_name("temp")
}

/// One run's `TMP` and `TEMP`, `yt-dlp\temp\OWNER-PID-N\`, removed with what yt-dlp left in it when this drops.
pub struct RunTemp {
    path: PathBuf,
}

impl RunTemp {
    /// A fresh folder for one run by `owner` (`supervisor` or `control`), made after a sweep of that owner's leftovers.
    /// None without the shipped exe (dev, tests) or on a failure; yt-dlp then unpacks into the user's temp folder.
    pub fn new(base: &Path, owner: &str) -> Option<RunTemp> {
        if !base.is_file() {
            return None;
        }
        let dir = temp_dir(base);
        let mut live = live();
        sweep(&dir, owner, &live);
        let path = dir.join(format!("{owner}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).ok()?;
        live.push(path.clone());
        Some(RunTemp { path })
    }

    pub fn env(&self) -> Vec<(String, String)> {
        let path = self.path.to_string_lossy().into_owned();
        vec![("TMP".into(), path.clone()), ("TEMP".into(), path)]
    }
}

impl Drop for RunTemp {
    fn drop(&mut self) {
        let mut live = live();
        live.retain(|p| p != &self.path);
        // A file a dying yt-dlp still holds keeps the folder for the next run's sweep.
        let _ = std::fs::remove_dir_all(&self.path);
        if let Some(dir) = self.path.parent() {
            // Only goes when empty: the other process's runs share it.
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Removes the folders `owner`'s earlier runs left. Windows refuses to rename a folder while a running yt-dlp
/// holds a file in it, another process's included, so only a folder that renames is removed.
fn sweep(dir: &Path, owner: &str, live: &[PathBuf]) {
    let prefix = format!("{owner}-");
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if !name.starts_with(&prefix) || live.contains(&path) {
            continue;
        }
        if name.ends_with(".gone") {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let gone = dir.join(format!("{name}.gone"));
        if std::fs::rename(&path, &gone).is_ok() {
            let _ = std::fs::remove_dir_all(&gone);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An install's `yt-dlp\` holding the shipped exe; returns that exe's path.
    fn install() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::Builder::new().prefix("sl-common-ytdlp-temp-").tempdir().unwrap();
        let base = dir.path().join("yt-dlp").join("yt-dlp.exe");
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, "").unwrap();
        (dir, base)
    }

    /// What a killed run named `name` left: its folder with an unpacked file in it.
    fn leftover(base: &Path, name: &str) -> PathBuf {
        let dir = temp_dir(base).join(name);
        std::fs::create_dir_all(dir.join("_MEI12345")).unwrap();
        std::fs::write(dir.join("_MEI12345").join("python314.dll"), "x").unwrap();
        dir
    }

    fn folder(run: &RunTemp) -> PathBuf {
        PathBuf::from(&run.env()[0].1)
    }

    #[test]
    fn a_run_gets_its_own_temp_folder_beside_yt_dlp() {
        // Arrange
        let (_dir, base) = install();
        // Act
        let run = RunTemp::new(&base, "supervisor").unwrap();
        // Assert
        let env = run.env();
        assert_eq!(env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["TMP", "TEMP"]);
        assert_eq!(env[0].1, env[1].1);
        let path = folder(&run);
        assert!(path.is_dir());
        assert_eq!(path.parent().unwrap(), base.parent().unwrap().join("temp"));
        assert!(path.file_name().unwrap().to_string_lossy().starts_with(&format!("supervisor-{}-", std::process::id())), "{}", path.display());
    }

    #[test]
    fn the_folder_goes_with_what_yt_dlp_left_in_it_when_the_run_ends() {
        // Arrange
        let (_dir, base) = install();
        let run = RunTemp::new(&base, "supervisor").unwrap();
        let path = folder(&run);
        std::fs::create_dir(path.join("_MEI12345")).unwrap();
        std::fs::write(path.join("_MEI12345").join("python314.dll"), "x").unwrap();
        // Act
        drop(run);
        // Assert
        assert!(!path.exists(), "{} was left behind", path.display());
        assert!(!temp_dir(&base).exists(), "the empty temp folder was left behind");
    }

    #[test]
    fn two_runs_get_different_folders() {
        // Arrange
        let (_dir, base) = install();
        // Act
        let (a, b) = (RunTemp::new(&base, "control").unwrap(), RunTemp::new(&base, "control").unwrap());
        // Assert
        assert_ne!(folder(&a), folder(&b));
        assert!(folder(&a).is_dir() && folder(&b).is_dir());
    }

    #[test]
    fn no_folder_is_made_without_the_shipped_exe() {
        // Arrange
        let dir = tempfile::Builder::new().prefix("sl-common-ytdlp-temp-").tempdir().unwrap();
        let base = dir.path().join("yt-dlp").join("yt-dlp.exe");
        // Act
        let run = RunTemp::new(&base, "control");
        // Assert
        assert!(run.is_none());
        assert!(!dir.path().join("yt-dlp").exists(), "made the yt-dlp folder of an install that has none");
    }

    #[test]
    fn a_new_run_removes_what_a_killed_run_of_its_owner_left() {
        // Arrange
        let (_dir, base) = install();
        let left = leftover(&base, "supervisor-4242-0");
        // Act
        let _run = RunTemp::new(&base, "supervisor").unwrap();
        // Assert
        assert!(!left.exists(), "{} was left behind", left.display());
    }

    #[test]
    fn a_new_run_leaves_another_owners_folders() {
        // Arrange
        let (_dir, base) = install();
        let left = leftover(&base, "control-4242-0");
        // Act
        let _run = RunTemp::new(&base, "supervisor").unwrap();
        // Assert
        assert!(left.exists());
    }

    #[test]
    fn a_new_run_leaves_the_folder_of_a_run_still_going() {
        // Arrange
        let (_dir, base) = install();
        let first = RunTemp::new(&base, "supervisor").unwrap();
        // Act
        let _second = RunTemp::new(&base, "supervisor").unwrap();
        // Assert
        assert!(folder(&first).is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn a_new_run_leaves_a_folder_whose_files_another_process_holds_until_it_is_free() {
        // Arrange
        use std::os::windows::fs::OpenOptionsExt;
        let (_dir, base) = install();
        let left = leftover(&base, "supervisor-4242-0");
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(left.join("_MEI12345").join("python314.dll")).unwrap();
        // Act
        let _run = RunTemp::new(&base, "supervisor").unwrap();
        let kept = left.exists();
        drop(held);
        let _next = RunTemp::new(&base, "supervisor").unwrap();
        // Assert
        assert!(kept, "a folder another process holds was removed");
        assert!(!left.exists(), "the folder stayed once it was free");
    }
}
