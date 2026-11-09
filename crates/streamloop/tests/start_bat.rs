//! Guards for `scripts/START.bat`: the batch rules cmd.exe punishes silently, and the launch and rollback flow.

use std::path::Path;

fn bytes() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("scripts").join("START.bat")).unwrap()
}

fn lines() -> Vec<String> {
    String::from_utf8(bytes()).unwrap().split("\r\n").map(str::to_string).collect()
}

fn index_of(lines: &[String], text: &str) -> usize {
    lines.iter().position(|l| l.trim() == text).unwrap_or_else(|| panic!("no line `{text}`"))
}

/// The lines of the parenthesised block that opens at `open` (a line ending in `(`), without the closing `)`.
fn block(lines: &[String], open: usize) -> Vec<String> {
    assert!(lines[open].trim_end().ends_with('('), "line {open} opens no block");
    let mut depth = 0;
    let mut out = vec![];
    for line in &lines[open..] {
        let t = line.trim();
        if t.ends_with('(') {
            depth += 1;
        }
        if t == ")" {
            depth -= 1;
            if depth == 0 {
                return out;
            }
        }
        out.push(line.clone());
    }
    panic!("block at {open} never closes");
}

fn lines_inside_blocks(lines: &[String]) -> Vec<String> {
    let mut depth = 0;
    let mut out = vec![];
    for line in lines {
        let t = line.trim();
        if t == ")" {
            depth -= 1;
        }
        if depth > 0 {
            out.push(line.clone());
        }
        if t.ends_with('(') {
            depth += 1;
        }
    }
    out
}

fn section_from(lines: &[String], label: &str) -> Vec<String> {
    lines[index_of(lines, label)..].to_vec()
}

const ROLLBACK_CALL: &str = "if exist \"%ROOT%_update\\landed\" if exist \"%ROOT%streamloop.exe.old\" call :rollback";

#[test]
fn no_double_colon_comment_inside_a_block() {
    // Arrange
    let inside = lines_inside_blocks(&lines());

    // Act
    let offenders: Vec<&String> = inside.iter().filter(|l| l.trim_start().starts_with("::")).collect();

    // Assert
    assert!(!inside.is_empty());
    assert!(offenders.is_empty(), "`::` inside a block aborts cmd.exe: {offenders:?}");
}

#[test]
fn no_parenthesis_in_a_rem_inside_a_block() {
    // Arrange
    let inside = lines_inside_blocks(&lines());

    // Act
    let offenders: Vec<&String> = inside
        .iter()
        .filter(|l| l.trim_start().to_lowercase().starts_with("rem") && (l.contains('(') || l.contains(')')))
        .collect();

    // Assert
    assert!(offenders.is_empty(), "a parenthesis in a rem closes the block: {offenders:?}");
}

#[test]
fn title_is_streamloop_3() {
    assert!(lines().iter().any(|l| l == "title StreamLoop 3"));
}

#[test]
fn crlf_only() {
    // Arrange
    let raw = bytes();

    // Act
    let bare_lf = raw.iter().enumerate().filter(|(i, b)| **b == b'\n' && (*i == 0 || raw[i - 1] != b'\r')).count();
    let bare_cr = raw.iter().enumerate().filter(|(i, b)| **b == b'\r' && raw.get(i + 1) != Some(&b'\n')).count();

    // Assert
    assert!(raw.windows(2).any(|w| w == b"\r\n"));
    assert_eq!((bare_lf, bare_cr), (0, 0));
}

#[test]
fn waits_with_ping_never_timeout() {
    // Arrange
    let l = lines();

    // Act
    let timeouts: Vec<&String> = l.iter().filter(|x| x.trim_start().to_lowercase().starts_with("timeout")).collect();

    // Assert
    assert!(l.iter().any(|x| x.trim() == "ping -n 11 127.0.0.1 >nul"));
    assert!(timeouts.is_empty(), "timeout fails without a console: {timeouts:?}");
}

#[test]
fn preflight_checks_each_tool_with_the_reinstall_message() {
    // Arrange
    let l = lines();
    let tools = [
        ("if not exist \"%EXE%\" goto :missing_exe", ":missing_exe"),
        (r#"if not exist "%ROOT%ffmpeg\ffmpeg.exe" goto :missing_ffmpeg"#, ":missing_ffmpeg"),
        (r#"if not exist "%ROOT%node\node.exe" goto :missing_node"#, ":missing_node"),
        (r#"if not exist "%ROOT%pot\build\main.js" goto :missing_pot"#, ":missing_pot"),
    ];

    for (check, label) in tools {
        // Act
        let at = index_of(&l, check);
        let body: Vec<&str> = l[index_of(&l, label) + 1..].iter().map(|x| x.trim()).take(3).collect();

        // Assert
        assert!(at < index_of(&l, ":loop"), "{check}");
        assert!(body[0].starts_with("echo [ERROR]") && body[0].ends_with("Reinstall StreamLoop from the full ZIP."), "{label}");
        assert_eq!(&body[1..], ["pause", "exit /b 1"], "{label}");
        assert!(index_of(&l, label) > l.iter().rposition(|x| x.trim() == "goto loop").unwrap(), "{label} would run inside the loop");
    }
}

#[test]
fn no_expanded_path_inside_a_block() {
    // Arrange
    let inside = lines_inside_blocks(&lines());

    // Act
    let offenders: Vec<&String> = inside.iter().filter(|l| l.contains("%ROOT%") || l.contains("%EXE%")).collect();

    // Assert
    assert!(!inside.is_empty());
    assert!(offenders.is_empty(), "a ) in the install path would close the block early: {offenders:?}");
}

#[test]
fn restores_a_half_swapped_exe_before_the_preflight() {
    // Arrange
    let l = lines();

    // Act
    let heal = index_of(
        &l,
        "if not exist \"%EXE%\" if exist \"%ROOT%streamloop.exe.old\" rename \"%ROOT%streamloop.exe.old\" streamloop.exe",
    );

    // Assert
    assert!(heal < index_of(&l, "if not exist \"%EXE%\" goto :missing_exe"));
}

#[test]
fn exit_0_stays_down_and_any_other_code_restarts_after_10_s() {
    // Arrange
    let l = lines();

    // Act
    let zero = block(&l, index_of(&l, "if \"%CODE%\"==\"0\" ("));
    let restart = section_from(&l, ":restart");

    // Assert
    assert!(zero.iter().any(|x| x.trim() == "exit /b 0"));
    assert!(!zero.iter().any(|x| x.contains("goto")));
    assert!(restart.iter().any(|x| x.trim() == "ping -n 11 127.0.0.1 >nul"));
    assert!(restart.iter().any(|x| x.trim() == "goto loop"));
}

#[test]
fn a_crash_before_confirmation_puts_the_old_exe_back() {
    // Arrange
    let l = lines();

    // Act
    let call = index_of(&l, ROLLBACK_CALL);
    let swap = section_from(&l, ":rollback").join("\n");

    // Assert
    assert!(call < index_of(&l, ":restart"));
    assert!(swap.contains("rename \"%EXE%\" streamloop.exe.bad"));
    assert!(swap.contains("rename \"%ROOT%streamloop.exe.old\" streamloop.exe\n"));
    assert!(swap.contains("move /y \"%ROOT%_update\\landed\" \"%ROOT%data\\update-rolled-back\""));
    assert!(swap.contains("goto :eof"));
}

#[test]
fn exit_75_skips_the_rollback() {
    // Arrange
    let l = lines();

    // Act
    let skip = index_of(&l, "if \"%CODE%\"==\"75\" goto restart");

    // Assert
    assert!(skip < index_of(&l, ROLLBACK_CALL));
}

#[test]
fn rollback_label_comes_after_the_last_goto_loop() {
    // Arrange
    let l = lines();

    // Act
    let last_goto = l.iter().rposition(|x| x.trim() == "goto loop").unwrap();

    // Assert
    assert!(index_of(&l, ":rollback") > last_goto, "the loop would fall into :rollback");
}

#[test]
fn has_no_update_swap_or_trampoline() {
    // Arrange
    let text = String::from_utf8(bytes()).unwrap();

    // Act
    let found: Vec<&str> = ["_update_tmp", "_update_old", "START.bat.new", "TRAMP", "node_modules", "index.ts"]
        .into_iter()
        .filter(|w| text.contains(w))
        .collect();

    // Assert
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn sets_streamloop_start_for_each_run() {
    // Arrange
    let l = lines();

    // Act
    let first = index_of(&l, "set \"STREAMLOOP_START=first\"");
    let restart = index_of(&l, "set \"STREAMLOOP_START=restart\"");
    let rollback = index_of(&l, "set \"STREAMLOOP_START=rollback\"");
    let values: Vec<String> = l
        .iter()
        .filter_map(|x| x.trim().strip_prefix("set \"STREAMLOOP_START=").map(|v| v.trim_end_matches('"').to_string()))
        .collect();

    // Assert
    assert!(first < index_of(&l, ":loop"), "first is set once, before the loop");
    assert!(index_of(&l, "\"%EXE%\"") < restart);
    assert!(restart < index_of(&l, ROLLBACK_CALL), "the rollback may override the restart default");
    assert!(restart < index_of(&l, ":restart"));
    assert!(rollback > index_of(&l, ":rollback"));
    assert_eq!(values, ["first", "restart", "rollback"]);
}

#[test]
fn exit_2_stays_down_without_a_rollback() {
    // Arrange
    let l = lines();
    let at = index_of(&l, "if \"%CODE%\"==\"2\" (");

    // Act
    let body = block(&l, at);

    // Assert
    assert!(body
        .iter()
        .any(|x| x.trim() == "echo Another StreamLoop is already running. Close it and start this one again."));
    assert!(body.iter().any(|x| x.trim() == "pause"));
    assert!(body.iter().any(|x| x.trim() == "exit /b 2"));
    assert!(!body.iter().any(|x| x.contains("goto")));
    assert!(at < index_of(&l, ROLLBACK_CALL));
}

#[test]
fn rollback_of_a_half_swap_only_puts_the_old_exe_back() {
    // Arrange
    let l = lines();
    let section = section_from(&l, ":rollback");
    let cut_off = section_from(&l, ":cut_off");
    let end = cut_off.iter().position(|x| x.trim() == "goto :eof").unwrap();

    // Act
    let half = &cut_off[..end];
    let rest = section[..section.iter().position(|x| x.trim() == "goto :eof").unwrap()].join("\n");

    // Assert
    assert_eq!(section[1].trim(), "if not exist \"%EXE%\" goto :cut_off", "the half-swap check comes first");
    assert!(half.iter().any(|x| x.trim() == "rename \"%ROOT%streamloop.exe.old\" streamloop.exe"));
    assert!(!half.iter().any(|x| x.contains("landed") || x.contains("update-rolled-back") || x.contains("STREAMLOOP_START")));
    assert!(rest.contains("update-rolled-back") && rest.contains("STREAMLOOP_START=rollback"));
}
