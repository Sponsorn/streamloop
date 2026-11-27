//! Where the window was: `app\window.json`, and where the next window opens.

use std::{fs, io, path::Path};

use gpui::{Bounds, Pixels, Size, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

/// R3's window size, centred on the primary display.
pub const DEFAULT_SIZE: Size<Pixels> = size(px(1280.), px(800.));
pub const MIN_SIZE: Size<Pixels> = size(px(720.), px(560.));
/// The part of the window that must lie on a display for its saved position to be used: about its title bar.
const TOP: Pixels = px(32.);

/// `app\window.json`. A field the file lacks takes its default.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowState {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub maximized: bool,
    pub tray_hint_shown: bool,
}

impl WindowState {
    /// Takes the position and size from `bounds`, the restore bounds for a maximised or full-screen window.
    pub fn set_bounds(&mut self, bounds: WindowBounds) {
        let restore = bounds.get_bounds();
        self.x = Some(restore.origin.x.into());
        self.y = Some(restore.origin.y.into());
        self.width = Some(restore.size.width.into());
        self.height = Some(restore.size.height.into());
        self.maximized = matches!(bounds, WindowBounds::Maximized(_));
    }

    /// The saved bounds, grown to the minimum size; None unless all four numbers are there.
    fn bounds(&self) -> Option<Bounds<Pixels>> {
        let numbers = [self.x?, self.y?, self.width?, self.height?];
        if !numbers.iter().all(|n| n.is_finite()) {
            return None;
        }
        let [x, y, width, height] = numbers.map(px);
        Some(Bounds::new(point(x, y), size(width, height).max(&MIN_SIZE)))
    }
}

pub fn load(path: &Path) -> WindowState {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes `window.json.tmp` beside `path`, then renames it over `path`, so a failed write leaves the old file.
pub fn save(path: &Path, state: &WindowState) -> io::Result<()> {
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
    fs::rename(&tmp, path)
}

/// The saved bounds when the top 32 px of the saved window lie on one of `displays` and its centre on one too;
/// else 1280 × 800 centred on `primary`; never smaller than 720 × 560; maximised when saved so.
pub fn placement(state: &WindowState, displays: &[Bounds<Pixels>], primary: Bounds<Pixels>) -> WindowBounds {
    let bounds = state
        .bounds()
        .filter(|bounds| top_on_a_display(bounds, displays) && display_for(*bounds, displays).is_some())
        .unwrap_or_else(|| {
            Bounds::centered_at(primary.center(), DEFAULT_SIZE.min(&primary.size).max(&MIN_SIZE))
        });
    if state.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

/// The display that holds the centre of `bounds`. GPUI must open the window on it: on any other display it
/// drops the bounds for its own default.
pub fn display_for(bounds: Bounds<Pixels>, displays: &[Bounds<Pixels>]) -> Option<usize> {
    let centre = bounds.center();
    displays.iter().position(|display| display.contains(&centre))
}

fn top_on_a_display(bounds: &Bounds<Pixels>, displays: &[Bounds<Pixels>]) -> bool {
    let top = Bounds::new(bounds.origin, size(bounds.size.width, TOP));
    displays.iter().any(|display| {
        display.left() <= top.left()
            && top.right() <= display.right()
            && display.top() <= top.top()
            && top.bottom() <= display.bottom()
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gpui::point;

    use super::*;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    fn saved(x: f32, y: f32, width: f32, height: f32) -> WindowState {
        WindowState {
            x: Some(x),
            y: Some(y),
            width: Some(width),
            height: Some(height),
            ..Default::default()
        }
    }

    /// Two 2560 × 1440 displays side by side, the primary on the left.
    fn two_displays() -> [Bounds<Pixels>; 2] {
        [rect(0., 0., 2560., 1440.), rect(2560., 0., 2560., 1440.)]
    }

    /// The primary display without its 48 px taskbar.
    fn primary() -> Bounds<Pixels> {
        rect(0., 0., 2560., 1392.)
    }

    /// 1280 × 800 in the middle of `primary()`.
    fn centred() -> Bounds<Pixels> {
        rect(640., 296., 1280., 800.)
    }

    /// A folder of this test process alone.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sl-ui-window-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn uses_saved_bounds_on_a_display() {
        // Arrange
        let state = saved(2868., 219., 1084., 711.);

        // Act
        let placed = placement(&state, &two_displays(), primary());
        let display = display_for(placed.get_bounds(), &two_displays());

        // Assert
        assert_eq!(placed, WindowBounds::Windowed(rect(2868., 219., 1084., 711.)));
        assert_eq!(display, Some(1));
    }

    #[test]
    fn centres_when_the_saved_display_is_gone() {
        // Arrange: one window was on the right-hand display, now gone; another's title bar hangs off the
        // left one's right edge, as after a resolution change.
        let left_only = [rect(0., 0., 2560., 1440.)];
        let on_the_gone_display = saved(2868., 219., 1084., 711.);
        let hanging_off = saved(2000., 100., 1084., 711.);

        // Act
        let placed = [&on_the_gone_display, &hanging_off, &WindowState::default()]
            .map(|state| placement(state, &left_only, primary()));
        let display = display_for(placed[0].get_bounds(), &left_only);

        // Assert
        assert_eq!(placed, [WindowBounds::Windowed(centred()); 3]);
        assert_eq!(display, Some(0));
    }

    #[test]
    fn centres_when_the_saved_centre_is_off_every_display() {
        // Arrange: the title strip is on the display's last 40 px, the rest of the window below it
        let one = [rect(0., 0., 2560., 1440.)];
        let hanging_below = saved(100., 1400., 1084., 800.);

        // Act
        let placed = placement(&hanging_below, &one, primary());

        // Assert
        assert_eq!(placed, WindowBounds::Windowed(centred()));
    }

    #[test]
    fn keeps_maximized() {
        // Arrange
        let on_a_display = WindowState {
            maximized: true,
            ..saved(408., 319., 984., 661.)
        };
        let display_gone = WindowState {
            maximized: true,
            ..saved(2868., 219., 1084., 711.)
        };
        let left_only = [rect(0., 0., 2560., 1440.)];

        // Act
        let kept = placement(&on_a_display, &left_only, primary());
        let centred_instead = placement(&display_gone, &left_only, primary());

        // Assert
        assert_eq!(kept, WindowBounds::Maximized(rect(408., 319., 984., 661.)));
        assert_eq!(centred_instead, WindowBounds::Maximized(centred()));
    }

    #[test]
    fn never_opens_below_the_minimum() {
        // Arrange: a saved window smaller than the minimum, and a primary display smaller than the default
        let small = saved(100., 100., 300., 200.);
        let tiny = rect(0., 0., 640., 480.);

        // Act
        let grown = placement(&small, &two_displays(), primary());
        let default_on_tiny = placement(&WindowState::default(), &[tiny], tiny);

        // Assert
        assert_eq!(grown, WindowBounds::Windowed(rect(100., 100., 720., 560.)));
        assert_eq!(default_on_tiny.get_bounds().size, MIN_SIZE);
    }

    #[test]
    fn an_unreadable_file_means_defaults() {
        // Arrange
        let dir = folder("unreadable");
        let missing = dir.join("missing.json");
        let garbage = dir.join("garbage.json");
        let wrong_type = dir.join("wrong-type.json");
        let partial = dir.join("partial.json");
        fs::write(&garbage, "{ not json").unwrap();
        fs::write(&wrong_type, r#"{"width": "wide"}"#).unwrap();
        fs::write(&partial, r#"{"x": 10, "y": 20, "trayHintShown": true}"#).unwrap();

        // Act
        let unreadable = [load(&missing), load(&garbage), load(&wrong_type)];
        let partial = load(&partial);

        // Assert
        assert_eq!(unreadable, [WindowState::default(), WindowState::default(), WindowState::default()]);
        assert_eq!(
            partial,
            WindowState {
                x: Some(10.),
                y: Some(20.),
                tray_hint_shown: true,
                ..Default::default()
            }
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_goes_through_a_temp_file() {
        // Arrange: a saved state, and a folder where the temp file goes, so writing it fails
        let dir = folder("save");
        let path = dir.join("window.json");
        let tmp = dir.join("window.json.tmp");
        let old = saved(1., 2., 800., 600.);
        save(&path, &old).unwrap();
        fs::create_dir(&tmp).unwrap();
        let new = WindowState {
            maximized: true,
            tray_hint_shown: true,
            ..saved(10., 20., 900., 700.)
        };

        // Act
        let blocked = save(&path, &new);
        let after_blocked = load(&path);
        fs::remove_dir(&tmp).unwrap();
        let unblocked = save(&path, &new);

        // Assert
        assert!(blocked.is_err());
        assert_eq!(after_blocked, old, "a failed save leaves the old file whole");
        assert!(unblocked.is_ok());
        assert_eq!(load(&path), new);
        assert!(!tmp.exists());
        assert!(fs::read_to_string(&path).unwrap().contains("\"trayHintShown\": true"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn taking_the_bounds_keeps_the_restore_bounds_and_the_tray_hint() {
        // Arrange
        let mut state = WindowState {
            tray_hint_shown: true,
            ..Default::default()
        };

        // Act
        state.set_bounds(WindowBounds::Maximized(rect(408., 319., 984., 661.)));
        let maximized = state.clone();
        state.set_bounds(WindowBounds::Windowed(rect(2868., 219., 1084., 711.)));

        // Assert
        assert_eq!(
            maximized,
            WindowState {
                maximized: true,
                tray_hint_shown: true,
                ..saved(408., 319., 984., 661.)
            }
        );
        assert_eq!(
            state,
            WindowState {
                tray_hint_shown: true,
                ..saved(2868., 219., 1084., 711.)
            }
        );
    }
}
