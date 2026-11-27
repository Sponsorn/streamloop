//! The tray icon: its colour and tooltip from the stream line, its clicks and menu, and the tray hint.

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    time::Duration,
};

use gpui::{App, AppContext, Task};

use crate::{
    api::{ApiError, client::Client, types::Status},
    logic::{Level, StreamLine, status_line},
    start::RETRYING,
};

/// The app's name, first in every tooltip and the tray hint's title.
pub const NAME: &str = "StreamLoop";
pub const TRAY_HINT: &str = "StreamLoop is still running. Quit it from the tray icon.";
/// While no window is open, the tray polls `GET /api/status` this often.
pub const CLOSED_POLL_EVERY: Duration = Duration::from_secs(5);
/// Windows shows at most this many UTF-16 units of a tray tooltip.
const TOOLTIP_UNITS: usize = 127;
const ELLIPSIS: char = '…';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayColour {
    Green,
    Amber,
    Red,
    Grey,
}

/// Ok → Green, Warn → Amber, Err → Red, Muted → Grey; no answer yet → Grey; a server that cannot be reached → Red.
pub fn colour(line: Option<&StreamLine>, reachable: bool) -> TrayColour {
    let Some(line) = line else {
        return TrayColour::Grey;
    };
    if !reachable {
        return TrayColour::Red;
    }
    match line.level {
        Level::Ok => TrayColour::Green,
        Level::Warn => TrayColour::Amber,
        Level::Err => TrayColour::Red,
        Level::Muted => TrayColour::Grey,
    }
}

/// "StreamLoop: WORD", then a line break and the reason when there is one. Windows shows at most 127
/// UTF-16 units, so a longer reason is cut at its last space that leaves room for "…", and "…" is added.
/// Before the first answer "StreamLoop"; when StreamLoop does not answer, Connecting's retry line.
pub fn tooltip(line: Option<&StreamLine>, reachable: bool) -> String {
    if !reachable {
        return format!("{NAME}\n{RETRYING}");
    }
    let Some(line) = line else {
        return NAME.to_string();
    };
    let head = format!("{NAME}: {}", line.word);
    match line.reason.as_deref().filter(|reason| !reason.is_empty()) {
        Some(reason) => {
            let room = TOOLTIP_UNITS.saturating_sub(units(&head) + 1);
            format!("{head}\n{}", fit(reason, room))
        }
        None => head,
    }
}

/// Windows counts a tooltip in UTF-16 units.
fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `text` whole when it takes at most `room` units, else cut at its last space that leaves room for "…",
/// with "…" added. A first word too long for the room is cut where the room ends.
fn fit(text: &str, room: usize) -> String {
    if units(text) <= room {
        return text.to_string();
    }
    let room = room.saturating_sub(ELLIPSIS.len_utf16());
    let mut used = 0;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            used += c.len_utf16();
            used > room
        })
        .map_or(text.len(), |(at, _)| at);
    let kept = &text[..end];
    let at_a_word = if text[end..].starts_with(' ') {
        kept
    } else {
        kept.rfind(' ').map_or(kept, |space| &kept[..space])
    };
    let cut = match at_a_word.trim_end() {
        "" => kept,
        words => words,
    };
    format!("{cut}{ELLIPSIS}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Open,
    Quit,
}

/// What the app asks of its tray icon. `Tray` is the real one; the UI tests record the calls.
pub trait TrayOutput {
    /// Shows `colour` with `tooltip`, which `tooltip()` has kept within Windows' limit.
    fn show(&mut self, colour: TrayColour, tooltip: &str);
    /// The tray hint's balloon.
    fn notify(&self, title: &str, text: &str);
}

/// The tray icon and what it shows: the stream line of the last answer this run and whether StreamLoop
/// answered since, plus the poll that feeds it while no window is open. Clones share it.
#[derive(Clone, Default)]
pub struct TrayFeed(Rc<RefCell<Feed>>);

#[derive(Default)]
struct Feed {
    output: Option<Box<dyn TrayOutput>>,
    line: Option<StreamLine>,
    unreachable: bool,
    closed_poll: Option<Task<()>>,
}

impl Feed {
    fn show(&mut self) {
        let reachable = !self.unreachable;
        let line = self.line.as_ref();
        if let Some(output) = &mut self.output {
            output.show(colour(line, reachable), &tooltip(line, reachable));
        }
    }
}

impl TrayFeed {
    /// Shows what the feed holds on `output` now, and every change after.
    pub fn attach(&self, output: Box<dyn TrayOutput>) {
        let mut feed = self.0.borrow_mut();
        feed.output = Some(output);
        feed.show();
    }

    /// Drops the tray icon, which takes it off the taskbar, and stops the poll.
    pub fn detach(&self) {
        let (output, poll) = {
            let mut feed = self.0.borrow_mut();
            (feed.output.take(), feed.closed_poll.take())
        };
        drop((output, poll));
    }

    /// StreamLoop answered, and its stream shows as `line`.
    pub fn answered(&self, line: StreamLine) {
        let mut feed = self.0.borrow_mut();
        feed.line = Some(line);
        feed.unreachable = false;
        feed.show();
    }

    /// StreamLoop did not answer. Before the first answer of the run the icon stays grey.
    pub fn unreachable(&self) {
        let mut feed = self.0.borrow_mut();
        feed.unreachable = true;
        feed.show();
    }

    pub fn notify(&self, title: &str, text: &str) {
        if let Some(output) = &self.0.borrow().output {
            output.notify(title, text);
        }
    }

    /// Polls `GET /api/status` at once and then every 5 s, until `stop_polling`. Without a tray icon there is
    /// nothing to feed, so nothing polls.
    pub fn poll_while_closed(&self, client: Client, cx: &mut App) {
        if self.0.borrow().output.is_none() {
            return;
        }
        // Weak: the feed owns this task, which must not keep the feed alive.
        let feed: Weak<RefCell<Feed>> = Rc::downgrade(&self.0);
        let poll = cx.spawn(async move |cx| {
            loop {
                let api = client.clone();
                let status = cx.background_spawn(async move { api.status() }).await;
                let Some(fed) = feed.upgrade() else {
                    return;
                };
                TrayFeed(fed).take(status);
                cx.background_executor().timer(CLOSED_POLL_EVERY).await;
            }
        });
        self.0.borrow_mut().closed_poll = Some(poll);
    }

    pub fn stop_polling(&self) {
        self.0.borrow_mut().closed_poll = None;
    }

    /// One closed-window poll: any failure counts as StreamLoop not answering.
    fn take(&self, status: Result<Status, ApiError>) {
        match status {
            Ok(status) => self.answered(status_line(&status)),
            Err(_) => self.unreachable(),
        }
    }
}

#[cfg(windows)]
pub use real::Tray;

/// Without Windows there is no tray icon.
#[cfg(not(windows))]
pub struct Tray;

#[cfg(not(windows))]
impl Tray {
    pub fn new(
        _cx: &mut App,
        _on_event: impl Fn(TrayEvent, &mut App) + 'static,
    ) -> anyhow::Result<Tray> {
        Err(anyhow::anyhow!("the tray icon is Windows only"))
    }
}

#[cfg(not(windows))]
impl TrayOutput for Tray {
    fn show(&mut self, _colour: TrayColour, _tooltip: &str) {}
    fn notify(&self, _title: &str, _text: &str) {}
}

#[cfg(windows)]
mod real {
    use gpui::App;
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem},
    };
    use windows_sys::Win32::UI::{
        Shell::{NIF_INFO, NIIF_INFO, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW},
        WindowsAndMessaging::{
            CreateIconFromResourceEx, GetSystemMetrics, LR_DEFAULTCOLOR, SM_CXSMICON,
        },
    };

    use super::{NAME, TrayColour, TrayEvent, TrayOutput};

    const GREEN: &[u8] = include_bytes!("../assets/tray-green.ico");
    const AMBER: &[u8] = include_bytes!("../assets/tray-amber.ico");
    const RED: &[u8] = include_bytes!("../assets/tray-red.ico");
    const GREY: &[u8] = include_bytes!("../assets/tray-grey.ico");

    /// The tray icon on GPUI's main thread, whose message loop also runs the icon's hidden window.
    pub struct Tray {
        icon: TrayIcon,
        green: Icon,
        amber: Icon,
        red: Icon,
        grey: Icon,
        shown: (TrayColour, String),
    }

    impl Tray {
        /// The icon, grey until the first answer, with Open StreamLoop and Quit StreamLoop on a right click.
        /// Its clicks and menu reach `on_event` on GPUI's main thread. Call it once per process: tray-icon
        /// keeps the first event handlers it is given.
        pub fn new(
            cx: &mut App,
            on_event: impl Fn(TrayEvent, &mut App) + 'static,
        ) -> anyhow::Result<Tray> {
            let image = |ico: &[u8], colour: &str| {
                load_icon(ico).ok_or_else(|| anyhow::anyhow!("the {colour} tray icon did not load"))
            };
            let (green, amber, red, grey) = (
                image(GREEN, "green")?,
                image(AMBER, "amber")?,
                image(RED, "red")?,
                image(GREY, "grey")?,
            );
            let menu = Menu::new();
            let open = MenuItem::new("Open StreamLoop", true, None);
            let quit = MenuItem::new("Quit StreamLoop", true, None);
            menu.append_items(&[&open, &quit])?;
            let icon = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                // Its default opens the menu on a left click as well.
                .with_menu_on_left_click(false)
                .with_tooltip(NAME)
                .with_icon(grey.clone())
                .build()?;

            // The handlers run inside the icon's window procedure; GPUI takes the events from this channel.
            let (events, received) = async_channel::unbounded::<TrayEvent>();
            let clicks = events.clone();
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let Some(event) = click(&event) {
                    let _ = clicks.try_send(event);
                }
            }));
            let (open, quit) = (open.id().clone(), quit.id().clone());
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let event = if event.id == open {
                    TrayEvent::Open
                } else if event.id == quit {
                    TrayEvent::Quit
                } else {
                    return;
                };
                let _ = events.try_send(event);
            }));
            cx.spawn(async move |cx| {
                while let Ok(event) = received.recv().await {
                    cx.update(|cx| on_event(event, cx));
                }
            })
            .detach();

            Ok(Tray {
                icon,
                green,
                amber,
                red,
                grey,
                shown: (TrayColour::Grey, NAME.to_string()),
            })
        }

        fn image(&self, colour: TrayColour) -> &Icon {
            match colour {
                TrayColour::Green => &self.green,
                TrayColour::Amber => &self.amber,
                TrayColour::Red => &self.red,
                TrayColour::Grey => &self.grey,
            }
        }
    }

    impl TrayOutput for Tray {
        fn show(&mut self, colour: TrayColour, tooltip: &str) {
            if colour != self.shown.0 {
                let _ = self.icon.set_icon(Some(self.image(colour).clone()));
            }
            if tooltip != self.shown.1 {
                let _ = self.icon.set_tooltip(Some(tooltip));
            }
            self.shown = (colour, tooltip.to_string());
        }

        fn notify(&self, title: &str, text: &str) {
            // tray-icon keeps the icon's id to itself: 2 for a process's first tray icon. Only this
            // window's icons can match, so the first id the shell takes is ours.
            let _ = (1..=8).any(|id| balloon(&self.icon, id, title, text));
        }
    }

    /// A left click or a double click opens the window; a right click opens the menu by itself.
    pub(super) fn click(event: &TrayIconEvent) -> Option<TrayEvent> {
        match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => Some(TrayEvent::Open),
            _ => None,
        }
    }

    /// The image of `ico` nearest the small-icon size, larger ones first, as an icon handle.
    fn load_icon(ico: &[u8]) -> Option<Icon> {
        // SAFETY: reads a system metric.
        let want = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
        let count = u16::from_le_bytes([*ico.get(4)?, *ico.get(5)?]) as usize;
        let (side, image) = (0..count)
            .filter_map(|i| {
                let entry = ico.get(6 + 16 * i..6 + 16 * (i + 1))?;
                let side = if entry[0] == 0 {
                    256
                } else {
                    u32::from(entry[0])
                };
                let len = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
                let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
                Some((side, ico.get(offset..offset.checked_add(len)?)?))
            })
            .min_by_key(|(side, _)| (*side < want, side.abs_diff(want)))?;
        // SAFETY: `image` is one whole icon image of an .ico, alive for the call; 0x30000 is the format
        // version the call requires.
        let handle = unsafe {
            CreateIconFromResourceEx(
                image.as_ptr(),
                image.len() as u32,
                1,
                0x0003_0000,
                side as i32,
                side as i32,
                LR_DEFAULTCOLOR,
            )
        };
        (!handle.is_null()).then(|| Icon::from_handle(handle as isize))
    }

    fn balloon(icon: &TrayIcon, id: u32, title: &str, text: &str) -> bool {
        // SAFETY: all zeroes is a valid NOTIFYICONDATAW.
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = icon.window_handle();
        data.uID = id;
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        copy_terminated(&mut data.szInfoTitle, title);
        copy_terminated(&mut data.szInfo, text);
        // SAFETY: `data` is sized as the call requires and names an icon of this process's own window.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) != 0 }
    }

    /// `text` into `field`, cut to leave the zero that ends it.
    fn copy_terminated(field: &mut [u16], text: &str) {
        let room = field.len().saturating_sub(1);
        for (slot, unit) in field.iter_mut().zip(text.encode_utf16().take(room)) {
            *slot = unit;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(word: &'static str, level: Level, reason: Option<&str>) -> StreamLine {
        StreamLine {
            word,
            level,
            reason: reason.map(str::to_string),
        }
    }

    const RETRY_TIP: &str = "StreamLoop\nCan't reach StreamLoop. Retrying...";

    #[test]
    fn each_level_has_its_colour() {
        // Arrange
        let lines = [
            line("Live", Level::Ok, None),
            line("Starting", Level::Warn, None),
            line("Off air", Level::Err, None),
            line("Stopped", Level::Muted, None),
        ];

        // Act
        let colours = lines.each_ref().map(|l| colour(Some(l), true));

        // Assert
        assert_eq!(
            colours,
            [
                TrayColour::Green,
                TrayColour::Amber,
                TrayColour::Red,
                TrayColour::Grey
            ]
        );
    }

    #[test]
    fn grey_before_the_first_answer() {
        // Act
        let (waiting, failing) = (colour(None, true), colour(None, false));
        let tip = tooltip(None, true);

        // Assert
        assert_eq!((waiting, failing), (TrayColour::Grey, TrayColour::Grey));
        assert_eq!(tip, "StreamLoop");
    }

    #[test]
    fn red_when_the_server_cannot_be_reached() {
        // Arrange
        let live = line("Live", Level::Ok, None);

        // Act
        let shown = colour(Some(&live), false);
        let tips = [tooltip(Some(&live), false), tooltip(None, false)];

        // Assert
        assert_eq!(shown, TrayColour::Red);
        assert_eq!(tips, [RETRY_TIP, RETRY_TIP]);
    }

    #[test]
    fn the_tooltip_carries_the_word_and_the_reason() {
        // Arrange
        let live = line("Live", Level::Ok, None);
        let off_air = line("Off air", Level::Err, Some("The encoder is not running."));

        // Act
        let tips = [tooltip(Some(&live), true), tooltip(Some(&off_air), true)];

        // Assert
        assert_eq!(
            tips,
            [
                "StreamLoop: Live",
                "StreamLoop: Off air\nThe encoder is not running."
            ]
        );
    }

    /// The tooltip with its "…" taken off is the start of `whole`, and `whole` goes on with a space there.
    fn assert_cut_at_a_word(tip: &str, whole: &str) {
        assert!(
            tip.encode_utf16().count() <= 127,
            "{} units",
            tip.encode_utf16().count()
        );
        assert!(tip.chars().count() <= 127);
        let kept = tip.strip_suffix('…').expect("the tooltip ends in …");
        assert!(
            whole.starts_with(kept),
            "{kept:?} is not the start of the line"
        );
        assert!(
            whole[kept.len()..].starts_with(' '),
            "{kept:?} ends inside a word"
        );
        assert!(!kept.ends_with(' '));
    }

    #[test]
    fn a_long_reason_is_cut_at_a_word_within_127_characters() {
        // Arrange: one reason of plain words, one whose letters take two UTF-16 units each
        let plain = "YouTube is blocking requests. Next try at 14:05. The slate is on air until the \
                     playlist loads again, and the encoder keeps the connection to Twitch open.";
        let wide = "𝐒𝐥𝐚𝐭𝐞 𝐨𝐧 𝐚𝐢𝐫 ".repeat(8);
        let lines = [
            line("Off air", Level::Err, Some(plain)),
            line("Recovering", Level::Warn, Some(wide.trim_end())),
        ];

        // Act
        let tips = lines.each_ref().map(|l| tooltip(Some(l), true));

        // Assert
        assert_cut_at_a_word(&tips[0], &format!("StreamLoop: Off air\n{plain}"));
        assert_cut_at_a_word(
            &tips[1],
            &format!("StreamLoop: Recovering\n{}", wide.trim_end()),
        );
        assert!(
            tips[1].chars().count() < 100,
            "the limit counts UTF-16 units"
        );
    }

    #[test]
    fn a_reason_with_no_space_is_cut_at_the_limit() {
        // Arrange
        let long_word = "x".repeat(300);
        let off_air = line("Off air", Level::Err, Some(&long_word));

        // Act
        let tip = tooltip(Some(&off_air), true);

        // Assert
        assert_eq!(tip.encode_utf16().count(), 127);
        assert!(tip.starts_with("StreamLoop: Off air\nxxx"));
        assert!(tip.ends_with("x…"));
    }

    #[cfg(windows)]
    #[test]
    fn a_left_click_and_a_double_click_open_the_window_and_a_right_click_does_not() {
        use tray_icon::{
            MouseButton, MouseButtonState, Rect, TrayIconEvent, TrayIconId, dpi::PhysicalPosition,
        };

        // Arrange
        let click = |button, button_state| TrayIconEvent::Click {
            id: TrayIconId::new("tray"),
            position: PhysicalPosition::default(),
            rect: Rect::default(),
            button,
            button_state,
        };
        let double = |button| TrayIconEvent::DoubleClick {
            id: TrayIconId::new("tray"),
            position: PhysicalPosition::default(),
            rect: Rect::default(),
            button,
        };
        let events = [
            click(MouseButton::Left, MouseButtonState::Up),
            double(MouseButton::Left),
            click(MouseButton::Left, MouseButtonState::Down),
            click(MouseButton::Right, MouseButtonState::Up),
            double(MouseButton::Right),
        ];

        // Act
        let opened = events.each_ref().map(real::click);

        // Assert
        assert_eq!(
            opened,
            [
                Some(TrayEvent::Open),
                Some(TrayEvent::Open),
                None,
                None,
                None
            ]
        );
    }
}
