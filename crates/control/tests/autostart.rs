//! Autostart as a Run value: the route against a fake store and a temp Startup folder, the migration of 3.1's
//! `StreamLoop.vbs`, and one run on the real registry under a test key.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use streamloop_common::paths::Layout;
use streamloop_control::autostart_store::{disabled, migrate_vbs, run_command, run_target, AutostartStore, FakeAutostart, Registry};
use support::*;

const MINE: &str = r"C:\Users\Emma Ö\StreamLoop 3";
const MINE_COMMAND: &str = "\"C:\\Users\\Emma Ö\\StreamLoop 3\\streamloop.exe\" ui --tray";
const OTHER_COMMAND: &str = "\"D:\\old\\StreamLoop\\streamloop.exe\" ui --tray";
const OTHER_BAT: &str = r"D:\old\StreamLoop\START.bat";

fn layout() -> Layout {
    Layout::new(Path::new(MINE))
}

/// A Startup folder whose path holds a space, an umlaut and parentheses.
fn startup(rig: &Rig) -> PathBuf {
    rig.dir.path().join("sl4 Ö (x)")
}

fn app(rig: &Rig, store: &Arc<FakeAutostart>) -> axum::Router {
    let (store, startup) = (store.clone(), startup(rig));
    rig.app_with(|d| {
        d.layout = layout();
        d.autostart = store;
        d.startup_dir = startup;
    })
}

/// The text StreamLoop writes: UTF-16LE with a byte order mark.
fn write_vbs(dir: &Path, bat: &str) {
    let text = format!("Set WshShell = CreateObject(\"WScript.Shell\")\r\nWshShell.Run \"\"\"{bat}\"\"\", 1, False");
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    write(&dir.join("StreamLoop.vbs"), &bytes);
}

#[tokio::test]
async fn on_writes_the_quoted_run_command() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::new());
    let app = app(&rig, &store);
    // Act
    let r = post(&app, "/autostart", json!({ "enabled": true })).await;
    let g = get(&app, "/autostart").await.json();
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({ "ok": true, "enabled": true }));
    assert_eq!(store.run_value().unwrap().as_deref(), Some(MINE_COMMAND));
    assert_eq!(g, json!({ "enabled": true, "target": format!("{MINE}\\streamloop.exe"), "thisInstall": true }));
}

#[tokio::test]
async fn on_clears_a_task_manager_disable() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::with_run(MINE_COMMAND));
    store.set_approved(&[0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let app = app(&rig, &store);
    assert!(!get(&app, "/autostart").await.json()["enabled"].as_bool().unwrap());
    // Act
    post(&app, "/autostart", json!({ "enabled": true })).await;
    // Assert
    assert_eq!(store.approved().unwrap(), None);
    assert!(get(&app, "/autostart").await.json()["enabled"].as_bool().unwrap());
}

#[tokio::test]
async fn on_removes_a_streamloop_vbs_of_any_install() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::new());
    write_vbs(&startup(&rig), OTHER_BAT);
    // Act
    let r = post(&app(&rig, &store), "/autostart", json!({ "enabled": true })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert!(!startup(&rig).join("StreamLoop.vbs").exists());
    assert_eq!(store.run_value().unwrap().as_deref(), Some(MINE_COMMAND));
}

#[tokio::test]
async fn on_leaves_a_vbs_that_streamloop_did_not_write() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::new());
    write(&startup(&rig).join("StreamLoop.vbs"), b"MsgBox \"hi\"");
    // Act
    let r = post(&app(&rig, &store), "/autostart", json!({ "enabled": true })).await;
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert!(startup(&rig).join("StreamLoop.vbs").exists());
}

#[tokio::test]
async fn on_moves_autostart_from_another_install() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::with_run(OTHER_COMMAND));
    let app = app(&rig, &store);
    let before = get(&app, "/autostart").await.json();
    // Act
    post(&app, "/autostart", json!({ "enabled": true })).await;
    let after = get(&app, "/autostart").await.json();
    // Assert
    assert_eq!(before, json!({ "enabled": true, "target": "D:\\old\\StreamLoop\\streamloop.exe", "thisInstall": false }));
    assert_eq!(after["thisInstall"], true);
    assert_eq!(store.run_value().unwrap().as_deref(), Some(MINE_COMMAND));
}

#[tokio::test]
async fn off_removes_the_value_and_this_installs_vbs() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::with_run(MINE_COMMAND));
    write_vbs(&startup(&rig), &format!("{MINE}\\START.bat"));
    let app = app(&rig, &store);
    // Act
    let r = post(&app, "/autostart", json!({ "enabled": false })).await;
    let g = get(&app, "/autostart").await.json();
    // Assert
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), json!({ "ok": true, "enabled": false }));
    assert_eq!(store.run_value().unwrap(), None);
    assert!(!startup(&rig).join("StreamLoop.vbs").exists());
    assert_eq!(g, json!({ "enabled": false, "target": null, "thisInstall": false }));
}

#[tokio::test]
async fn off_refuses_another_installs_value_with_409() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::with_run(OTHER_COMMAND));
    // Act
    let r = post(&app(&rig, &store), "/autostart", json!({ "enabled": false })).await;
    // Assert
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.error(), "Autostart points to another StreamLoop install: D:\\old\\StreamLoop\\streamloop.exe.");
    assert_eq!(store.run_value().unwrap().as_deref(), Some(OTHER_COMMAND));
}

#[tokio::test]
async fn off_with_only_a_vbs_it_cannot_vouch_for_keeps_r3s_409_texts() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::new());
    write(&startup(&rig).join("StreamLoop.vbs"), b"MsgBox \"hi\"");
    let app = app(&rig, &store);
    // Act
    let foreign = post(&app, "/autostart", json!({ "enabled": false })).await;
    write_vbs(&startup(&rig), OTHER_BAT);
    let other = post(&app, "/autostart", json!({ "enabled": false })).await;
    // Assert
    assert_eq!(foreign.status, StatusCode::CONFLICT);
    assert_eq!(foreign.error(), "StreamLoop.vbs in the Startup folder was not written by StreamLoop. Remove it from the Startup folder by hand.");
    assert_eq!(other.status, StatusCode::CONFLICT);
    assert_eq!(other.error(), format!("Autostart points to another StreamLoop install: {OTHER_BAT}."));
}

#[tokio::test]
async fn a_disabled_value_reads_as_off() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::with_run(MINE_COMMAND));
    store.set_approved(&[0x03, 0, 0, 0]);
    // Act
    let g = get(&app(&rig, &store), "/autostart").await.json();
    // Assert
    assert_eq!(g, json!({ "enabled": false, "target": format!("{MINE}\\streamloop.exe"), "thisInstall": true }));
}

#[tokio::test]
async fn this_install_ignores_the_case_of_the_path() {
    // Arrange
    let rig = Rig::new();
    let store = Arc::new(FakeAutostart::with_run(&MINE_COMMAND.to_lowercase()));
    // Act
    let g = get(&app(&rig, &store), "/autostart").await.json();
    // Assert
    assert_eq!(g["thisInstall"], true);
}

#[test]
fn run_target_reads_quoted_and_unquoted_values() {
    // Arrange
    let quoted = "\"C:\\Program Files\\StreamLoop\\streamloop.exe\" ui --tray";
    let unquoted = "C:\\StreamLoop\\streamloop.exe ui --tray";
    // Act
    let (q, u, none) = (run_target(quoted), run_target(unquoted), run_target("  "));
    // Assert
    assert_eq!(q, Some(PathBuf::from("C:\\Program Files\\StreamLoop\\streamloop.exe")));
    assert_eq!(u, Some(PathBuf::from("C:\\StreamLoop\\streamloop.exe")));
    assert_eq!(none, None);
}

#[test]
fn an_odd_first_byte_of_the_approved_value_means_disabled() {
    // Arrange, act, assert
    assert!(disabled(Some(&[0x03, 0, 0])));
    assert!(!disabled(Some(&[0x02, 0, 0])));
    assert!(!disabled(Some(&[])));
    assert!(!disabled(None));
}

#[test]
fn the_run_command_quotes_the_exe_and_starts_the_app_in_the_tray() {
    // Arrange, act, assert
    assert_eq!(run_command(&layout()), MINE_COMMAND);
}

#[test]
fn migration_replaces_this_installs_vbs_with_the_run_value() {
    // Arrange
    let dir = tempfile::tempdir().unwrap().keep();
    let startup = dir.join("sl4 Ö (x)");
    write_vbs(&startup, &format!("{MINE}\\START.bat"));
    let store = FakeAutostart::new();
    // Act
    let migrated = migrate_vbs(&store, &startup, &layout()).unwrap();
    let again = migrate_vbs(&store, &startup, &layout()).unwrap();
    // Assert
    assert!(migrated);
    assert!(!again);
    assert_eq!(store.run_value().unwrap().as_deref(), Some(MINE_COMMAND));
    assert!(!startup.join("StreamLoop.vbs").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn migration_leaves_another_installs_vbs_and_a_foreign_vbs() {
    // Arrange
    let dir = tempfile::tempdir().unwrap().keep();
    let (other, foreign) = (dir.join("sl4 Ö (x)"), dir.join("foreign"));
    write_vbs(&other, OTHER_BAT);
    write(&foreign.join("StreamLoop.vbs"), b"MsgBox \"hi\"");
    let store = FakeAutostart::new();
    // Act
    let (a, b) = (migrate_vbs(&store, &other, &layout()).unwrap(), migrate_vbs(&store, &foreign, &layout()).unwrap());
    // Assert
    assert!(!a && !b);
    assert_eq!(store.run_value().unwrap(), None);
    assert!(other.join("StreamLoop.vbs").exists());
    assert!(foreign.join("StreamLoop.vbs").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn migration_keeps_the_vbs_when_writing_the_value_fails() {
    // Arrange
    let dir = tempfile::tempdir().unwrap().keep();
    let startup = dir.join("sl4 Ö (x)");
    write_vbs(&startup, &format!("{MINE}\\START.bat"));
    let store = FakeAutostart::new();
    store.fail_writes();
    // Act
    let result = migrate_vbs(&store, &startup, &layout());
    // Assert
    assert!(result.is_err());
    assert!(startup.join("StreamLoop.vbs").exists());
    assert_eq!(store.run_value().unwrap(), None);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Task Manager's disable: 3 in the first byte, then from the fifth on the FILETIME of the disable.
const TASK_MANAGER_OFF: [u8; 12] = [0x03, 0, 0, 0, 0x10, 0x32, 0x54, 0x76, 0x98, 0xba, 0xdc, 0x01];

/// 100 ns intervals since 1601-01-01, as a FILETIME counts.
fn filetime(t: std::time::SystemTime) -> u64 {
    let since_1970 = t.duration_since(std::time::UNIX_EPOCH).unwrap();
    (since_1970.as_nanos() / 100) as u64 + 116_444_736_000_000_000
}

#[tokio::test]
async fn migration_carries_a_task_manager_disable_of_the_vbs_over_to_the_run_value() {
    // Arrange: 3.1's StreamLoop.vbs, disabled in Task Manager
    let rig = Rig::new();
    write_vbs(&startup(&rig), &format!("{MINE}\\START.bat"));
    let store = Arc::new(FakeAutostart::new());
    store.set_vbs_approved(&TASK_MANAGER_OFF);
    // Act
    let migrated = migrate_vbs(store.as_ref(), &startup(&rig), &layout()).unwrap();
    let g = get(&app(&rig, &store), "/autostart").await.json();
    // Assert
    assert!(migrated);
    assert_eq!(store.run_value().unwrap().as_deref(), Some(MINE_COMMAND));
    assert_eq!(store.approved().unwrap().as_deref(), Some(&TASK_MANAGER_OFF[..]));
    assert_eq!(g, json!({ "enabled": false, "target": format!("{MINE}\\streamloop.exe"), "thisInstall": true }));
}

#[test]
fn a_disable_of_another_shape_is_written_as_task_manager_writes_one() {
    // Arrange
    let dir = tempfile::tempdir().unwrap().keep();
    let startup = dir.join("sl4 Ö (x)");
    write_vbs(&startup, &format!("{MINE}\\START.bat"));
    let store = FakeAutostart::new();
    store.set_vbs_approved(&[0x03, 0, 0]);
    let before = filetime(std::time::SystemTime::now());
    // Act
    migrate_vbs(&store, &startup, &layout()).unwrap();
    let after = filetime(std::time::SystemTime::now());
    // Assert
    let written = store.approved().unwrap().expect("a disable is written");
    assert_eq!(written.len(), 12);
    assert_eq!(written[..4], [0x03, 0, 0, 0]);
    let at = u64::from_le_bytes(written[4..].try_into().unwrap());
    assert!((before..=after).contains(&at), "{before} <= {at} <= {after}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn migration_of_a_vbs_that_task_manager_left_on_writes_no_disable() {
    // Arrange
    let dir = tempfile::tempdir().unwrap().keep();
    let startup = dir.join("sl4 Ö (x)");
    write_vbs(&startup, &format!("{MINE}\\START.bat"));
    let store = FakeAutostart::new();
    store.set_vbs_approved(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    // Act
    migrate_vbs(&store, &startup, &layout()).unwrap();
    // Assert
    assert_eq!(store.run_value().unwrap().as_deref(), Some(MINE_COMMAND));
    assert_eq!(store.approved().unwrap(), None);
    std::fs::remove_dir_all(dir).unwrap();
}

/// `HKCU\Software\StreamLoopTest-<random>`, deleted with everything under it when the guard drops.
struct TestKey(String);

impl TestKey {
    fn new() -> TestKey {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        TestKey(format!("Software\\StreamLoopTest-{}-{nanos}", std::process::id()))
    }
}

impl Drop for TestKey {
    fn drop(&mut self) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};
        let name: Vec<u16> = std::ffi::OsStr::new(&self.0).encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: `name` is NUL-terminated and outlives the call.
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr()) };
    }
}

#[test]
fn the_registry_sets_reads_disables_and_clears_the_run_value_under_a_test_key() {
    // Arrange
    let key = TestKey::new();
    let registry = Registry::real(Some(&key.0));
    let command = run_command(&Layout::new(Path::new(r"C:\Users\Emma Ö\Stream Loop (x)")));
    // Act
    let missing = (registry.run_value().unwrap(), registry.approved().unwrap());
    registry.set_run_value(&command).unwrap();
    let set = registry.run_value().unwrap();
    registry.write_approved(&[0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    let off = registry.approved().unwrap();
    registry.clear_approved().unwrap();
    registry.delete_run_value().unwrap();
    let gone = (registry.run_value().unwrap(), registry.approved().unwrap());
    let twice = (registry.delete_run_value().is_ok(), registry.clear_approved().is_ok());
    // Assert
    assert_eq!(missing, (None, None));
    assert_eq!(set.as_deref(), Some(command.as_str()));
    assert!(disabled(off.as_deref()));
    assert_eq!(gone, (None, None));
    assert_eq!(twice, (true, true));
}

/// Writes a REG_BINARY `name` under `HKCU\<key>\<sub>`, as Task Manager writes its flags.
fn write_binary(key: &TestKey, sub: &str, name: &str, bytes: &[u8]) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_BINARY};
    let wide = |s: &str| -> Vec<u16> { std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect() };
    let (path, name) = (wide(&format!("{}\\{sub}", key.0)), wide(name));
    // SAFETY: both names are NUL-terminated and `bytes` is `len` readable bytes.
    let code = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, path.as_ptr(), name.as_ptr(), REG_BINARY, bytes.as_ptr().cast(), bytes.len() as u32) };
    assert_eq!(code, 0, "RegSetKeyValueW");
}

#[test]
fn the_registry_reads_the_vbs_flag_beside_the_test_keys_startup_approved_run() {
    // Arrange: Task Manager's flag for StreamLoop.vbs, where a test key keeps its StartupFolder
    let key = TestKey::new();
    let registry = Registry::real(Some(&key.0));
    let missing = registry.vbs_approved().unwrap();
    write_binary(&key, r"StartupApproved\StartupFolder", "StreamLoop.vbs", &TASK_MANAGER_OFF);
    // Act
    let read = registry.vbs_approved().unwrap();
    // Assert
    assert_eq!(missing, None);
    assert_eq!(read.as_deref(), Some(&TASK_MANAGER_OFF[..]));
    assert_eq!(registry.approved().unwrap(), None, "StartupApproved\\Run holds no StreamLoop.vbs value");
}
