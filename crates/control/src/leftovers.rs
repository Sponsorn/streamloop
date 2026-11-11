//! What 3.0 left in an install that 3.1 no longer uses, removed by name at every start.

use streamloop_common::paths::Layout;

/// Master's full update could leave a copy of each of these beside the install.
const UPDATE_PARTS: [&str; 5] = ["node", "supervisor", "ffmpeg", "pot", "yt-dlp"];

/// The Node app, the old supervisor exe and the leftovers of 3.0's update swaps, plus `pot\` and `node\` once
/// `supervisor\streamloop-pot.exe` exists (the TypeScript fallback needs them until then). A path a running process
/// holds is logged and retried at the next start. `app\config.json`, `app\logs`, `data\` and `_update\` stay.
/// Blocking file work.
pub fn remove_3_0_leftovers(layout: &Layout) {
    let root = &layout.root;
    let app = &layout.app_dir;
    let mut paths = vec![
        app.join("src"),
        app.join("node_modules"),
        app.join("package.json"),
        app.join("package-lock.json"),
        app.join("tsconfig.json"),
        app.join("node-version.txt"),
        root.join("supervisor").join("streamloop-supervisor.exe"),
        root.join("_update_tmp"),
        root.join("_update_old"),
        root.join("START.bat.new"),
        root.join("_apply_launcher.bat"),
        root.join("_update_bad_app"),
        layout.data.join("update-swapping"),
        layout.data.join("update-checking"),
        layout.data.join("update-swap.log"),
    ];
    for part in UPDATE_PARTS {
        paths.push(root.join(format!("_update_old_{part}")));
        paths.push(root.join(format!("_update_bad_{part}")));
    }
    for path in paths {
        remove(&path);
    }
    // node\ first: while a fallback server runs, its node.exe is held, and pot\ (its scripts) must stay whole.
    if layout.token_server.is_file() && remove(&root.join("node")) {
        remove(&root.join("pot"));
    }
}

/// Removes a leftover and logs it; true when the path is gone afterwards.
fn remove(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return true };
    let kind = meta.file_type();
    let removed = if kind.is_symlink() {
        // A link (a dev junction for node_modules) goes as a link; its target is left alone.
        std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path))
    } else if kind.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => {
            tracing::info!(path = %path.display(), "Removed a 3.0 leftover");
            true
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "Could not remove a 3.0 leftover");
            false
        }
    }
}
