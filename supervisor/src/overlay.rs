//! The now-playing overlay, drawn by each video feeder (port of spike 3's default look).
//! Titles reach ffmpeg only through UTF-8 text files, never inside the filter string.

use std::path::{Path, PathBuf};

const FONT: &str = "C:/Windows/Fonts/malgun.ttf";
const FRAME_W: u32 = 1920;
const FRAME_H: u32 = 1080;
const MARGIN: u32 = 24;
const TITLE_SIZE: u32 = 28;
const PROGRESS_SIZE: u32 = 22;
const BAR_H: u32 = 6;
const LABEL: &str = "NOT LIVE";
const LABEL_SIZE: u32 = 22;
/// Average Malgun Gothic glyph width as a fraction of font size (tuned in spike 3).
const CHAR_WIDTH_RATIO: f64 = 0.55;
const LINE_HEIGHT_RATIO: f64 = 1.3;
const STACK_GAP: u32 = 10;

fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x2199 | 0x21A9..=0x21AA | 0x231A..=0x23FF | 0x24C2
        | 0x25AA..=0x25FE | 0x2600..=0x27BF | 0x2934..=0x2935 | 0x2B05..=0x2B55 | 0x3030 | 0x303D
        | 0x3297 | 0x3299 | 0xFE0E | 0xFE0F | 0x200D | 0x1F000..=0x1FAFF)
}

pub fn strip_emoji(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !is_emoji(*c)).collect();
    kept.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

pub fn max_chars_for(font_size: u32, max_width: u32) -> usize {
    ((max_width as f64 / (font_size as f64 * CHAR_WIDTH_RATIO)).floor() as usize).max(1)
}

pub fn truncate_text(text: &str, font_size: u32, max_width: u32) -> String {
    let max = max_chars_for(font_size, max_width);
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('\u{2026}');
    s
}

fn hms(total: f64) -> String {
    let s = total.max(0.0).round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// Drawtext's own expansion (this fixed string only, never a title): the clock, the total and a
/// percentage. The feeder's filters see `t` from 0 at its first frame (`-ss` before `-i`, and
/// `-output_ts_offset` only applies at the muxer), so the clock and the percentage add `start_pos`.
/// `pts:gmtime`'s offset is in whole seconds (checked by render: 600 shows 00:10:00).
pub fn progress_text(duration: f64, start_pos: f64) -> String {
    format!(
        "%{{pts:gmtime:{}:%H\\:%M\\:%S}} / {}  %{{eif:min(100,100*({}+t)/{}):d}}\\%",
        start_pos.round() as i64, hms(duration), start_pos, duration
    )
}

/// ':' separates filter options, so a Windows path takes forward slashes and an escaped drive colon.
fn escape_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace(':', "\\:")
}

fn text_clause(file: &Path, size: u32, y: u32, expand: bool) -> String {
    format!(
        "drawtext=fontfile='{}':textfile='{}'{}:fontsize={size}:fontcolor=white:x={MARGIN}:y={y}:box=1:boxcolor=black@0.55:boxborderw=10",
        escape_path(Path::new(FONT)), escape_path(file), if expand { "" } else { ":expansion=none" }
    )
}

/// The overlay stage: reads the normalised picture as `[norm]`, writes `[outv]`. The track is a
/// drawbox; the fill is a `color=` source slid in from the left by an `overlay` x expression
/// (drawbox's width is fixed at init). Only that stage needs `shortest=1`: `color=` never ends.
fn script(title_file: &Path, label_file: &Path, progress_file: Option<&Path>, duration: Option<f64>, start_pos: f64) -> String {
    let h = |size: u32| (size as f64 * LINE_HEIGHT_RATIO).round() as u32;
    // Top-left: the bottom of the picture is left to the stream's own overlays.
    let title_y = MARGIN;
    let mut text = vec![text_clause(title_file, TITLE_SIZE, title_y, false)];
    // Top-right, white on red@0.8, always shown (2.x's overlay and spike 3 show the same label).
    text.push(format!(
        "drawtext=fontfile='{}':textfile='{}':expansion=none:fontsize={LABEL_SIZE}:fontcolor=white:x=main_w-text_w-{MARGIN}:y={MARGIN}:box=1:boxcolor=red@0.8:boxborderw=10",
        escape_path(Path::new(FONT)), escape_path(label_file)
    ));
    if let Some(p) = progress_file {
        text.push(text_clause(p, PROGRESS_SIZE, title_y + h(TITLE_SIZE) + STACK_GAP, true));
    }
    let Some(d) = duration else { return format!("[norm]{}[outv]", text.join(",")) };
    let (w, y) = (FRAME_W, FRAME_H - BAR_H);
    format!(
        "[norm]{},drawbox=x=0:y={y}:w={w}:h={BAR_H}:color=white@0.25:t=fill[base];color=c=red:s={w}x{BAR_H}[bar];[base][bar]overlay=x='max(-{w},min(0,{w}*(({start_pos}+t)/{d}-1)))':y={y}:shortest=1[outv]",
        text.join(",")
    )
}

/// Writes the text files and the filter script into `dir` and returns the script's path.
/// Without a duration only the title shows.
pub fn prepare(title: &str, duration: Option<f64>, start_pos: f64, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let title_file = dir.join("title.txt");
    std::fs::write(&title_file, truncate_text(&strip_emoji(title), TITLE_SIZE, FRAME_W - 2 * MARGIN))?;
    let label_file = dir.join("label.txt");
    std::fs::write(&label_file, LABEL)?;
    let duration = duration.filter(|d| *d > 0.0);
    let progress_file = match duration {
        Some(d) => {
            let f = dir.join("progress.txt");
            std::fs::write(&f, progress_text(d, start_pos))?;
            Some(f)
        }
        None => None,
    };
    let path = dir.join("overlay.fc");
    std::fs::write(&path, script(&title_file, &label_file, progress_file.as_deref(), duration, start_pos))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_are_stripped_and_spaces_collapsed() {
        assert_eq!(strip_emoji("rocket \u{1F680} launch"), "rocket launch");
        assert_eq!(strip_emoji("flag \u{1F1F8}\u{1F1EA} here"), "flag here");
        assert_eq!(strip_emoji("thumbs \u{1F44D}\u{1F3FD} up"), "thumbs up");
        assert_eq!(strip_emoji("family \u{1F468}\u{200D}\u{1F469} time"), "family time");
        assert_eq!(strip_emoji("\u{1F525} title \u{1F525}"), "title");
        assert_eq!(strip_emoji("\u{1F3B5}\u{1F525}"), "");
    }

    #[test]
    fn cjk_and_punctuation_survive_stripping() {
        assert_eq!(strip_emoji("日本語のタイトル: 100%!"), "日本語のタイトル: 100%!");
    }

    #[test]
    fn short_text_is_untouched_and_long_text_ends_in_an_ellipsis_within_the_budget() {
        assert_eq!(truncate_text("short title", 28, 900), "short title");
        let max = max_chars_for(28, 300);
        assert_eq!(max, (300.0_f64 / (28.0 * 0.55)).floor() as usize);
        let cut = truncate_text(&"x".repeat(max + 20), 28, 300);
        assert_eq!(cut, format!("{}\u{2026}", "x".repeat(max - 1)));
        assert_eq!(cut.chars().count(), max);
    }

    #[test]
    fn truncation_counts_code_points_not_bytes() {
        assert_eq!(truncate_text("ab\u{1F3B5}cd", 1000, 10), "\u{2026}");
        assert_eq!(truncate_text("日本語", 28, 100), "日本語");
    }

    #[test]
    fn progress_text_is_hms_total_and_percent_with_drawtexts_own_escaping() {
        assert_eq!(progress_text(45.0, 0.0), "%{pts:gmtime:0:%H\\:%M\\:%S} / 00:00:45  %{eif:min(100,100*(0+t)/45):d}\\%");
        assert!(progress_text(12300.0, 0.0).contains("/ 03:25:00"));
    }

    /// Evaluates `100*(S+t)/D` at the feeder's first frame (t = 0) from the expression in the text.
    #[test]
    fn a_feeder_started_mid_video_shows_50_percent_at_its_first_frame() {
        let text = progress_text(1200.0, 600.0);
        let expr = text.split("eif:min(100,").nth(1).unwrap().split(":d}").next().unwrap().trim_end_matches(')');
        let nums: Vec<f64> = expr.split(|c: char| !(c.is_ascii_digit() || c == '.')).filter_map(|s| s.parse().ok()).collect();
        // 100, start_pos, duration (t is 0 at the first frame)
        assert_eq!(nums, [100.0, 600.0, 1200.0]);
        assert_eq!(nums[0] * nums[1] / nums[2], 50.0);
        assert!(text.starts_with("%{pts:gmtime:600:"), "clock starts at 00:10:00");
        assert!(text.contains("/ 00:20:00"));
    }

    #[test]
    fn prepare_writes_files_under_dir_and_keeps_titles_out_of_the_script() {
        let dir = std::env::temp_dir().join(format!("sl-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let title = "It's: a \"title\" \u{1F680} 日本語; [x]";
        let path = prepare(title, Some(1200.0), 600.0, &dir).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(path.starts_with(&dir));
        assert_eq!(std::fs::read_to_string(dir.join("title.txt")).unwrap(), "It's: a \"title\" 日本語; [x]");
        assert!(!s.contains("日本語") && !s.contains("title\""));
        assert!(s.starts_with("[norm]") && s.ends_with("[outv]"));
        assert!(s.contains("fontfile='C\\:/Windows/Fonts/malgun.ttf'"));
        assert_eq!(s.matches("shortest=1").count(), 1, "only the color= stage");
        assert!(s.contains("expansion=none"));
        // the NOT LIVE label: top-right, white on red@0.8, from its own text file
        assert_eq!(std::fs::read_to_string(dir.join("label.txt")).unwrap(), "NOT LIVE");
        let label = s.split(',').find(|c| c.contains("label.txt")).unwrap();
        assert!(label.contains("x=main_w-text_w-24:y=24") && label.contains("fontsize=22") && label.contains("boxcolor=red@0.8"), "{label}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Top-left, so the bottom of the picture stays free for the stream's own overlays; the
    /// Twitch read-back's clock region (CLOCK_1080 in twitch-readback.ts) is 8 px above the clock line.
    #[test]
    fn the_title_and_clock_sit_top_left_and_the_bar_stays_at_the_bottom() {
        let dir = std::env::temp_dir().join(format!("sl-overlay-pos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = std::fs::read_to_string(prepare("t", Some(1200.0), 0.0, &dir).unwrap()).unwrap();
        let clause = |file: &str| s.split(',').find(|c| c.contains(file)).unwrap().to_string();
        assert!(clause("title.txt").contains("x=24:y=24"), "{}", clause("title.txt"));
        assert!(clause("progress.txt").contains("x=24:y=70"), "{}", clause("progress.txt"));
        assert!(s.contains("drawbox=x=0:y=1074"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unknown_duration_shows_the_title_alone() {
        let dir = std::env::temp_dir().join(format!("sl-overlay-nodur-{}", std::process::id()));
        let path = prepare("t", None, 0.0, &dir).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(!s.contains("progress.txt") && !s.contains("overlay=") && !s.contains("c=red:s="));
        assert!(s.starts_with("[norm]drawtext=") && s.ends_with("[outv]"));
        assert!(s.contains("label.txt"), "the label shows without a duration too");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
