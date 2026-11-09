//! The web dashboard's look (`src/admin/admin.css`), set as gpui-component's theme.

use std::rc::Rc;

use gpui::{App, SharedString};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode};

use crate::logic::Level;

pub const BG: u32 = 0x0f1117;
pub const SURFACE: u32 = 0x1a1d27;
pub const SURFACE_HOVER: u32 = 0x222633;
pub const BORDER: u32 = 0x2a2e3d;
pub const TEXT: u32 = 0xe1e4eb;
pub const MUTED: u32 = 0x8b8fa3;
pub const ACCENT: u32 = 0x6c5ce7;
pub const ACCENT_HOVER: u32 = 0x7c6ef7;
pub const GREEN: u32 = 0x00b894;
/// Borders and bars. Text in red uses `ERROR_TEXT`.
pub const RED: u32 = 0xe74c3c;
/// admin.css `.err`: `RED` is under 4.5:1 on the surface at small sizes.
pub const ERROR_TEXT: u32 = 0xff6b5e;
pub const YELLOW: u32 = 0xf39c12;
pub const WHITE: u32 = 0xffffff;
/// admin.css `.btn-danger`.
pub const DANGER: u32 = 0xc0392b;

/// admin.css `.toast-error` and `.toast-success` (15 % fill, 30 % border) laid over `BG`, so a toast is opaque.
pub const TOAST_ERROR_BG: u32 = 0x2f1a1d;
pub const TOAST_ERROR_BORDER: u32 = 0x502322;
pub const TOAST_SUCCESS_BG: u32 = 0x0d2a2a;
pub const TOAST_SUCCESS_BORDER: u32 = 0x0b433d;

/// admin.css `.container`.
pub const MAX_WIDTH: f32 = 960.;
pub const FONT: &str = "Segoe UI";
pub const MONO_FONT: &str = "Consolas";

/// admin.css `.ok`, `.warn`, `.err` and `.muted`.
pub fn level_color(level: Level) -> u32 {
    match level {
        Level::Ok => GREEN,
        Level::Warn => YELLOW,
        Level::Err => ERROR_TEXT,
        Level::Muted => MUTED,
    }
}

fn hex(color: u32) -> Option<SharedString> {
    Some(format!("#{color:06x}").into())
}

/// Dark only; call once after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    let mut config = ThemeConfig::default();
    config.name = "StreamLoop".into();
    config.mode = ThemeMode::Dark;
    config.font_family = Some(FONT.into());
    config.mono_font_family = Some(MONO_FONT.into());
    config.font_size = Some(14.);
    config.radius = Some(8);
    config.radius_lg = Some(8);
    config.shadow = Some(false);

    let c = &mut config.colors;
    c.background = hex(BG);
    c.foreground = hex(TEXT);
    c.border = hex(BORDER);
    c.input = hex(BORDER);
    c.muted = hex(SURFACE);
    c.muted_foreground = hex(MUTED);
    c.accent = hex(SURFACE_HOVER);
    c.accent_foreground = hex(TEXT);
    c.button = hex(SURFACE);
    c.button_hover = hex(SURFACE_HOVER);
    c.button_active = hex(SURFACE_HOVER);
    c.button_foreground = hex(TEXT);
    c.primary = hex(ACCENT);
    c.primary_hover = hex(ACCENT_HOVER);
    c.primary_active = hex(ACCENT);
    c.primary_foreground = hex(WHITE);
    c.secondary = hex(SURFACE);
    c.secondary_hover = hex(SURFACE_HOVER);
    c.secondary_active = hex(SURFACE_HOVER);
    c.secondary_foreground = hex(TEXT);
    c.danger = hex(DANGER);
    c.danger_hover = hex(RED);
    c.danger_active = hex(DANGER);
    c.danger_foreground = hex(WHITE);
    // admin.css `.btn-danger`: solid, white text, no hover change. The button tokens otherwise
    // fall back to a 20 % tint with red text.
    c.button_danger = hex(DANGER);
    c.button_danger_hover = hex(DANGER);
    c.button_danger_active = hex(DANGER);
    c.button_danger_foreground = hex(WHITE);
    // admin.css `.btn-copied`, with the page colour for text: white on the green is 2.5:1.
    c.button_success = hex(GREEN);
    c.button_success_hover = hex(GREEN);
    c.button_success_active = hex(GREEN);
    c.button_success_foreground = hex(BG);
    c.success = hex(GREEN);
    c.warning = hex(YELLOW);
    c.popover = hex(SURFACE);
    c.popover_foreground = hex(TEXT);
    c.list = hex(SURFACE);
    c.list_hover = hex(SURFACE_HOVER);
    c.list_active = hex(SURFACE_HOVER);
    c.link = hex(ACCENT);
    c.ring = hex(ACCENT);
    c.caret = hex(TEXT);
    c.selection = hex(ACCENT);
    c.scrollbar = hex(BG);
    c.scrollbar_thumb = hex(BORDER);
    c.scrollbar_thumb_hover = hex(MUTED);
    // admin.css outlines an off toggle in muted; this Switch has no outline, so its track takes the
    // colour. BORDER would be 1.25:1 on the surface, under the 3:1 a control needs.
    c.switch = hex(MUTED);

    Theme::update(cx, |theme| theme.apply_config(&Rc::new(config)));
}
