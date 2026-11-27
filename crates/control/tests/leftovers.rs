use streamloop_common::paths::Layout;
use streamloop_control::leftovers::remove_stale_old_exe;

/// An install in a temp folder holding `files`, each a path below the root with `/` between its parts.
fn install_with(files: &[&str]) -> (tempfile::TempDir, Layout) {
    let dir = tempfile::Builder::new().prefix("sl3 Ö leftovers").tempdir().unwrap();
    let layout = Layout::new(&dir.path().join("root"));
    for rel in files {
        let path = rel.split('/').fold(layout.root.clone(), |p, part| p.join(part));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
    }
    (dir, layout)
}

#[test]
fn a_stale_old_exe_is_removed_at_start() {
    // Arrange: a confirmed update whose old exe outlived the confirmation's tries
    let (_dir, layout) = install_with(&["streamloop.exe", "streamloop.exe.old"]);

    // Act
    remove_stale_old_exe(&layout);

    // Assert
    assert!(!layout.exe_old.exists());
    assert!(layout.exe.exists());
}

#[test]
fn old_stays_while_an_update_is_landed() {
    // Arrange: the update still waits for its confirmation, or for START.bat's rollback
    let (_dir, layout) = install_with(&["streamloop.exe", "streamloop.exe.old", "_update/landed"]);

    // Act
    remove_stale_old_exe(&layout);

    // Assert
    assert!(layout.exe_old.exists());
    assert!(layout.landed.exists());
}

#[test]
fn old_stays_when_streamloop_exe_is_missing() {
    // Arrange: an update cut off between its two renames, which START.bat puts back
    let (_dir, layout) = install_with(&["streamloop.exe.old"]);

    // Act
    remove_stale_old_exe(&layout);

    // Assert
    assert!(layout.exe_old.exists());
}
