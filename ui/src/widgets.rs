//! Controls the views share: the action button (admin.js `runAction`), the restart note, the
//! config save's reload warning, the settings forms' layout, inputs and text, and the focus reveal.

use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Div, Element, ElementId, Entity, FocusHandle,
    FontStyle, FontWeight, Global, GlobalElementId, HighlightStyle, InspectorElementId,
    IntoElement, LayoutId, Pixels, Point, Render, ScrollHandle, SharedString, Styled, StyledText,
    Subscription, WeakFocusHandle, Window, div, point, prelude::*, px, rems, rgb, rgba,
};
use gpui_kit::{
    base::{TestSupportExt, TestTextExt},
    component::{
        Disableable, Icon, IconName, Sizable,
        button::{Button, ButtonCustomVariant, ButtonVariants},
        input::{Input, InputState, TextareaState},
        label::Label,
        select::{Select, SelectDelegate, SelectItem},
        switch::Switch,
    },
};

use crate::{
    api::{
        ApiError,
        client::Client,
        types::{PlaylistRef, SaveResult},
    },
    app::{AppHandle, ToastKind},
    logic::{
        admin::{PlaylistEntry, collect_playlists},
        is_benign_reload_error,
    },
    theme::{ACCENT, BG, BORDER, MUTED, RED, SURFACE},
};

/// admin.js puts a save button's label back 1.5 s after a save.
pub const SAVE_RESET: Duration = Duration::from_millis(1500);

/// admin.css's page line height, for a `Label` over 16 px: the Label's own 1.25 rem line is
/// shorter than its glyphs, which then run into the line below.
pub const PAGE_LINE_HEIGHT: f32 = 1.6;

/// The action buttons of one view whose request is running.
#[derive(Default)]
pub struct Busy(HashSet<ElementId>);

impl Busy {
    pub fn contains(&self, id: &ElementId) -> bool {
        self.0.contains(id)
    }
}

/// A view with action buttons keeps their `Busy` set.
pub trait ActionView: Sized + 'static {
    fn busy(&mut self) -> &mut Busy;
}

/// A button that is disabled while its request runs. Wire its click to `run_action` with the same `id`.
pub fn action_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    busy: &Busy,
) -> Button {
    let id = id.into();
    let disabled = busy.contains(&id);
    Button::new(id).label(label).disabled(disabled)
}

/// admin.css `.btn`: 14 px text, 20 px side padding, about 38 px tall.
pub fn btn(button: Button) -> Button {
    button.large().h(px(38.)).px(px(20.))
}

/// admin.js `runAction`: disables button `id` while `request` runs, toasts its error, enables it again and polls once.
/// `on_ok` gets the answer first; a click while the button is busy sends nothing.
pub fn run_action<V: ActionView, T: Send + 'static>(
    view: &mut V,
    id: impl Into<ElementId>,
    app: &AppHandle,
    request: impl FnOnce(&Client) -> Result<T, ApiError> + Send + 'static,
    on_ok: impl FnOnce(&mut V, T, &mut Window, &mut Context<V>) + 'static,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let id = id.into();
    if !view.busy().0.insert(id.clone()) {
        return;
    }
    cx.notify();
    let (app, client) = (app.clone(), app.client.clone());
    cx.spawn_in(window, async move |view, cx| {
        let result = cx.background_spawn(async move { request(&client) }).await;
        let error = result.as_ref().err().map(ToString::to_string);
        view.update_in(cx, |view, window, cx| {
            view.busy().0.remove(&id);
            cx.notify();
            if let Ok(answer) = result {
                on_ok(view, answer, window, cx);
            }
        })
        .ok();
        cx.update(|_, cx| {
            if let Some(error) = error {
                app.toast(error, ToastKind::Error, cx);
            }
            app.poll_now(cx);
        })
        .ok();
    })
    .detach();
}

/// Spec D6, admin.js `saveConfig`: every config save that went through calls this with the answer.
/// A reload that failed for any reason but a refused connection (no supervisor running) is a toast.
pub fn reload_warning(app: &AppHandle, saved: &SaveResult, cx: &mut App) {
    let (error, code) = (
        saved.reload_error.as_deref(),
        saved.reload_error_code.as_deref(),
    );
    if !is_benign_reload_error(error, code) {
        let text = format!(
            "Saved, but the supervisor could not reload: {}",
            error.unwrap_or_default()
        );
        app.toast(text, ToastKind::Error, cx);
    }
}

/// admin.js `restartStream`: stop, then start once the stop went through.
pub fn restart_stream(client: &Client) -> Result<(), ApiError> {
    client.stream_stop()?;
    client.stream_start()
}

/// Which restart notes show. A tab shows its own; a restart hides them all.
#[derive(Default)]
struct RestartNotes(HashSet<&'static str>);

impl Global for RestartNotes {}

/// index.html `.restart-note`: a tab's note that a saved change applies after a stream restart,
/// with a Restart stream button.
#[derive(Clone, Copy)]
pub struct RestartNote {
    pub id: &'static str,
    pub button: &'static str,
    pub text: &'static str,
}

impl RestartNote {
    pub fn show(self, cx: &mut App) {
        cx.default_global::<RestartNotes>().0.insert(self.id);
    }

    pub fn shown(self, cx: &App) -> bool {
        cx.try_global::<RestartNotes>()
            .is_some_and(|notes| notes.0.contains(self.id))
    }

    /// admin.js hides every `.restart-note` after a restart, whichever button started it.
    pub fn hide_all(cx: &mut App) {
        cx.default_global::<RestartNotes>().0.clear();
    }

    /// Renders the view again whenever a note shows or hides.
    pub fn observe<V: 'static>(cx: &mut Context<V>) -> Subscription {
        cx.observe_global::<RestartNotes>(|_, cx| cx.notify())
    }

    /// The note while it shows. Its button runs `restart_stream` through `run_action`, then hides every note.
    pub fn render<V: ActionView>(
        self,
        app: &AppHandle,
        busy: &Busy,
        cx: &mut Context<V>,
    ) -> Option<AnyElement> {
        if !self.shown(cx) {
            return None;
        }
        let app = app.clone();
        let restart = btn(action_button(self.button, "Restart stream", busy)).on_click(
            cx.listener(move |view, _, window, cx| {
                run_action(
                    view,
                    self.button,
                    &app,
                    restart_stream,
                    |_, _, _, cx| RestartNote::hide_all(cx),
                    window,
                    cx,
                );
            }),
        );
        let note = div()
            .id(self.id)
            .mt(px(12.))
            .px(px(16.))
            .py(px(12.))
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .border_1()
            .border_color(rgba(NOTE_BORDER))
            .bg(rgba(NOTE_BG))
            .rounded(px(8.))
            .child(Label::new(self.text).text_size(px(14.)))
            .child(reveal_on_focus(restart))
            .test_support();
        Some(note.into_any_element())
    }
}

/// admin.css `.restart-note`: the yellow at 40 % for the border and 8 % for the fill.
const NOTE_BORDER: u32 = 0xf39c1266;
const NOTE_BG: u32 = 0xf39c1214;

/// admin.css `.section` with its `.section-title`, which the CSS shows in capitals.
pub fn section(title: &str) -> Div {
    div().mb(px(24.)).child(
        Label::new(title.to_uppercase())
            .mb(px(12.))
            .text_size(px(14.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(MUTED)),
    )
}

/// admin.css `.settings-panel`.
pub fn settings_panel() -> Div {
    div()
        .p(px(20.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(8.))
}

/// admin.css `.form-group`: the label over the control, then the hint.
pub fn form_group(label: &str, control: impl IntoElement, hint: Option<&str>) -> Div {
    div()
        .mb(px(16.))
        .child(
            Label::new(label.to_string())
                .mb(px(4.))
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(MUTED)),
        )
        .child(control)
        .children(hint.map(|text| {
            Label::new(text.to_string())
                .mt(px(2.))
                .text_size(px(11.))
                .text_color(rgb(MUTED))
        }))
}

/// admin.css `.hint`.
pub fn hint(text: impl Into<SharedString>) -> Label {
    Label::new(text)
        .mt(px(2.))
        .text_size(px(11.))
        .text_color(rgb(MUTED))
}

/// admin.css `.toggle-row`: the label and its descriptions, the switch on the right.
pub fn toggle_row(label: &'static str, descs: &[SharedString], toggle: Switch) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .px(px(16.))
        .py(px(12.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(8.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Label::new(label).text_size(px(14.)))
                .children(descs.iter().map(|desc| {
                    Label::new(desc.clone())
                        .text_size(px(12.))
                        .text_color(rgb(MUTED))
                })),
        )
        .child(reveal_on_focus(toggle))
}

/// The summary of index.html's `details.wiz-collapsible`: accent text after a chevron that points
/// down while `open`. The caller toggles `open` on click.
pub fn summary_button(id: &'static str, text: &'static str, open: bool) -> Button {
    let marker = if open {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    };
    Button::new(id).text().accessibility_label(text).child(
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(Icon::new(marker).size(px(12.)).text_color(rgb(ACCENT)))
            .child(Label::new(text).text_size(px(13.)).text_color(rgb(ACCENT))),
    )
}

/// A run of `rich` text: plain, `<strong>` or `<em>`.
#[derive(Clone, Copy)]
pub enum Run {
    Plain(&'static str),
    Strong(&'static str),
    Em(&'static str),
}

/// One wrapping line of text with its `<strong>` runs drawn in `strong` and `<em>` runs in italics.
/// Size and colour come from the surrounding element.
pub fn rich(runs: &[Run], strong: HighlightStyle) -> impl IntoElement + use<> {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for run in runs {
        let start = text.len();
        let (words, style) = match *run {
            Run::Plain(words) => (words, None),
            Run::Strong(words) => (words, Some(strong)),
            Run::Em(words) => (
                words,
                Some(HighlightStyle {
                    font_style: Some(FontStyle::Italic),
                    ..HighlightStyle::default()
                }),
            ),
        };
        text.push_str(words);
        if let Some(style) = style {
            highlights.push((start..text.len(), style));
        }
    }
    let text = SharedString::from(text);
    div()
        .child(StyledText::new(text.clone()).with_highlights(highlights))
        .test_text(text)
}

/// One `<li>` of a numbered list: its number in the 20 px gutter of the `<ol>`.
pub fn list_item(marker: String, content: AnyElement) -> Div {
    div()
        .flex()
        .mb(px(4.))
        .child(
            div()
                .w(px(20.))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .pr(px(4.))
                .child(marker),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// admin.css `.form-group input`; `.invalid` turns the border red.
pub fn text_input(
    state: &Entity<InputState>,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    invalid: bool,
) -> FocusReveal {
    reveal_on_focus(
        Input::new(state)
            .id(id)
            .large()
            .aria_label(label)
            .h(px(40.))
            .bg(rgb(BG))
            .when(invalid, |input| input.border_color(rgb(RED))),
    )
}

pub fn text_of(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

/// Sets an input's text with the cursor at its end, as a browser does when a script sets a value.
pub fn put_text(input: &Entity<InputState>, text: &str, window: &mut Window, cx: &mut App) {
    input.update(cx, |state, cx| {
        state.set_value(SharedString::from(text.to_string()), window, cx);
        state.set_selected_range(text.len()..text.len(), cx);
    });
}

/// `put_text` for a multi-line input.
pub fn put_area_text(area: &Entity<TextareaState>, text: &str, window: &mut Window, cx: &mut App) {
    area.update(cx, |state, cx| {
        state.set_value(SharedString::from(text.to_string()), window, cx);
        state.set_selected_range(text.len()..text.len(), cx);
    });
}

/// The element ids of one `PlaylistRows`. Each row's ids carry its key: `(id, key)`.
#[derive(Clone, Copy)]
pub struct PlaylistRowIds {
    pub id: &'static str,
    pub name: &'static str,
    pub remove: &'static str,
    pub add: &'static str,
}

struct PlaylistRow {
    key: usize,
    id: Entity<InputState>,
    name: Entity<InputState>,
    invalid: bool,
}

const VIDEO_URL: &str =
    "That looks like a single video URL. Paste a playlist URL or a playlist ID (starts with PL).";

/// admin.js `addPlaylistRow` and `collectPlaylists`: rows of an ID input, a Name input and ×,
/// then "+ Add Playlist". The Settings tab and the wizard each keep one.
pub struct PlaylistRows {
    app: AppHandle,
    ids: PlaylistRowIds,
    rows: Vec<PlaylistRow>,
    /// Keys stay with their row, so removing one leaves the others' ids and focus alone.
    next_key: usize,
    /// The row whose × is under the pointer.
    hovered: Option<usize>,
}

impl PlaylistRows {
    pub fn new(app: AppHandle, ids: PlaylistRowIds) -> Self {
        Self {
            app,
            ids,
            rows: Vec::new(),
            next_key: 0,
            hovered: None,
        }
    }

    /// One row per playlist, in order, replacing every row there was.
    pub fn set(&mut self, playlists: &[PlaylistRef], window: &mut Window, cx: &mut Context<Self>) {
        self.rows.clear();
        for playlist in playlists {
            let name = playlist.name.as_deref().unwrap_or_default();
            self.push(&playlist.id, name, window, cx);
        }
        cx.notify();
    }

    /// "+ Add Playlist".
    pub fn add_empty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.push("", "", window, cx);
        cx.notify();
    }

    fn push(&mut self, id: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id_input = cx.new(|cx| InputState::new(window, cx).placeholder("PLxxxxxxxxxxxxxxxx"));
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name (optional)"));
        put_text(&id_input, id, window, cx);
        put_text(&name_input, name, window, cx);
        self.rows.push(PlaylistRow {
            key: self.next_key,
            id: id_input,
            name: name_input,
            invalid: false,
        });
        self.next_key += 1;
    }

    fn remove(&mut self, key: usize, cx: &mut Context<Self>) {
        self.rows.retain(|row| row.key != key);
        if self.hovered == Some(key) {
            self.hovered = None;
        }
        cx.notify();
    }

    /// The playlists to save, leaving out blank rows. A row whose ID is a video URL is marked red
    /// with a toast of its own, and then nothing comes back; a row that passes loses its mark.
    // The error carries nothing: the toasts have told the user why.
    #[allow(clippy::result_unit_err)]
    pub fn collect(&mut self, cx: &mut Context<Self>) -> Result<Vec<PlaylistEntry>, ()> {
        let texts: Vec<(String, String)> = self
            .rows
            .iter()
            .map(|row| (text_of(&row.id, cx), text_of(&row.name, cx)))
            .collect();
        let answer = collect_playlists(&texts);
        let bad = answer.as_ref().err().cloned().unwrap_or_default();
        for (index, (row, (id, _))) in self.rows.iter_mut().zip(&texts).enumerate() {
            // A blank row is skipped, so it keeps whatever mark it had, as in admin.js.
            if id.trim().is_empty() {
                continue;
            }
            row.invalid = bad.contains(&index);
            if row.invalid {
                self.app.toast(VIDEO_URL, ToastKind::Error, cx);
            }
        }
        cx.notify();
        answer.map_err(|_| ())
    }

    /// admin.css `.btn-remove`: no fill, a border and muted text; red under the pointer.
    fn remove_button(&self, key: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let color = if self.hovered == Some(key) {
            RED
        } else {
            MUTED
        };
        let button = Button::new((self.ids.remove, key))
            .accessibility_label("Remove playlist")
            .tooltip("Remove")
            .custom(ButtonCustomVariant::new(cx).foreground(rgb(color).into()))
            // A label child, since the button sets its own label's size.
            .child(Label::new("×").text_size(px(16.)).text_color(rgb(color)))
            .h(px(38.))
            .px(px(10.))
            .on_click(cx.listener(move |this, _, _, cx| this.remove(key, cx)));
        // A custom button's own border takes its fill colour under the pointer, so a wrapper draws
        // it. The wrapper tracks the pointer too, since the button's tooltip has its hover handler.
        let frame = SharedString::from(format!("{}-frame", self.ids.remove));
        div()
            .id((frame, key))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hovered = Some(key);
                } else if this.hovered == Some(key) {
                    this.hovered = None;
                }
                cx.notify();
            }))
            .flex()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(color))
            .child(reveal_on_focus(button))
    }
}

#[cfg(test)]
impl PlaylistRows {
    /// Each row's key, in page order.
    pub(crate) fn keys(&self) -> Vec<usize> {
        self.rows.iter().map(|row| row.key).collect()
    }

    /// Each row's (ID, Name) text.
    pub(crate) fn values(&self, cx: &App) -> Vec<(String, String)> {
        self.rows
            .iter()
            .map(|row| (text_of(&row.id, cx), text_of(&row.name, cx)))
            .collect()
    }

    /// Whether each row's ID is marked red.
    pub(crate) fn marks(&self) -> Vec<bool> {
        self.rows.iter().map(|row| row.invalid).collect()
    }
}

impl Render for PlaylistRows {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ids = self.ids;
        // admin.css `.playlist-row`: the ID takes two shares of the width and the name one.
        let rows: Vec<Div> = self
            .rows
            .iter()
            .map(|row| {
                div()
                    .flex()
                    .gap(px(8.))
                    .mb(px(8.))
                    .child(grow(div(), 2.).min_w_0().child(text_input(
                        &row.id,
                        (ids.id, row.key),
                        "Playlist ID or URL",
                        row.invalid,
                    )))
                    .child(div().flex_1().min_w_0().child(text_input(
                        &row.name,
                        (ids.name, row.key),
                        "Playlist name",
                        false,
                    )))
                    .child(self.remove_button(row.key, cx))
            })
            .collect();
        let add = btn(Button::new(ids.add).label("+ Add Playlist"))
            .on_click(cx.listener(|this, _, window, cx| this.add_empty(window, cx)));
        div()
            .children(rows)
            // As wide as its label, as the inline button in index.html.
            .child(div().flex().child(reveal_on_focus(add)))
    }
}

/// CSS `flex: <share>`: grows by `share` from no width of its own.
fn grow(mut div: Div, share: f32) -> Div {
    div = div.flex_1();
    div.style().flex_grow = Some(share);
    div
}

/// Wraps a control so the dashboard scrolls it into view when Tab gives it the focus.
pub fn reveal_on_focus(control: impl IntoElement) -> FocusReveal {
    FocusReveal(control.into_any_element())
}

/// A layout-transparent wrapper: when the focus lands inside its control, it asks the containing
/// scroll view to show the control (`Window::request_autoscroll`), as GPUI's inputs do.
pub struct FocusReveal(AnyElement);

impl IntoElement for FocusReveal {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for FocusReveal {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.0.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // `prepaint` returns the focus when it was first assigned inside this control.
        if self.0.prepaint(window, cx).is_some() {
            // An input's own request (its caret line, with a margin) gives way to the whole control.
            window.take_autoscroll();
            window.request_autoscroll(bounds);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.paint(window, cx);
    }
}

/// An open dropdown row's width beside its label in gpui-component 0.9.0 at the 14 px rem: 4 px of
/// list inset and 9 px of padding each side, a 3.5 px gap and the 10.5 px check mark.
const MENU_ROW_CHROME: Pixels = px(40.);

/// The open list's width for its longest label: wide enough for it, or `None` (gpui-component's
/// `Length::Auto`, the box's own width) when the box is wider.
fn menu_width(longest_label: Pixels, box_width: Pixels) -> Option<Pixels> {
    let fit = (longest_label + MENU_ROW_CHROME).ceil();
    (fit > box_width).then_some(fit)
}

/// Wraps `select` so its open list grows to its longest entry and is never narrower than the box,
/// as Chrome's is. `labels` are the entries' titles; the box's width is kept under `id`.
pub fn fit_menu<D>(
    id: impl Into<ElementId>,
    select: Select<D>,
    labels: impl IntoIterator<Item = SharedString>,
) -> FitMenu<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    FitMenu {
        id: id.into(),
        select: Some(select),
        labels: labels.into_iter().collect(),
        child: None,
    }
}

/// A layout-transparent wrapper that sets its dropdown's menu width; see `fit_menu`.
pub struct FitMenu<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    id: ElementId,
    select: Option<Select<D>>,
    labels: Vec<SharedString>,
    child: Option<AnyElement>,
}

impl<D> IntoElement for FitMenu<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<D> Element for FitMenu<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // The last frame's box, as the dropdown places its open list by that frame's bounds too.
        let box_width = window.with_element_state(id.expect("FitMenu has an id"), |width, _| {
            let width: Pixels = width.unwrap_or_default();
            (width, width)
        });
        let style = window.text_style();
        // The rows' `text_base`.
        let size = rems(1.).to_pixels(window.rem_size());
        let longest = self
            .labels
            .iter()
            .map(|label| {
                let run = style.to_run(label.len());
                window
                    .text_system()
                    .shape_line(label.clone(), size, &[run], None)
                    .width
            })
            .fold(Pixels::ZERO, |widest, width| if width > widest { width } else { widest });
        let mut select = self.select.take().expect("laid out once");
        if let Some(width) = menu_width(longest, box_width) {
            select = select.menu_width(width);
        }
        let mut child = select.into_any_element();
        let layout = child.request_layout(window, cx);
        self.child = Some(child);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_element_state(id.expect("FitMenu has an id"), |_: Option<Pixels>, _| {
            ((), bounds.size.width)
        });
        if let Some(child) = &mut self.child {
            child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

/// Room left between a control brought into view and the edge of the column.
const REVEAL_MARGIN: Pixels = px(8.);

/// The focus a `FollowFocus` last followed, kept by its view between frames.
pub type Followed = Rc<RefCell<Option<WeakFocusHandle>>>;

/// The scroll offset that brings `target` fully into `viewport` with `margin` to spare, or `None`
/// when it is fully in view already.
fn reveal_offset(
    viewport: Bounds<Pixels>,
    offset: Point<Pixels>,
    max_offset: Point<Pixels>,
    target: Bounds<Pixels>,
    margin: Pixels,
) -> Option<Point<Pixels>> {
    if target.top() >= viewport.top() && target.bottom() <= viewport.bottom() {
        return None;
    }
    let (top, bottom) = (viewport.top() + margin, viewport.bottom() - margin);
    let delta = if target.top() < top || target.size.height > bottom - top {
        top - target.top()
    } else {
        bottom - target.bottom()
    };
    let y = (offset.y + delta).clamp(-max_offset.y, px(0.));
    (y != offset.y).then_some(point(offset.x, y))
}

/// A scrolling column. When the keyboard moves the focus, it scrolls to the bounds the focused
/// control asked for with `Window::request_autoscroll`; other frames leave the wheel free.
pub struct FollowFocus {
    column: AnyElement,
    scroll: ScrollHandle,
    followed: Followed,
}

impl FollowFocus {
    /// `column` is the scrolling element, tracked by `scroll`.
    pub fn new(column: impl IntoElement, scroll: &ScrollHandle, followed: &Followed) -> Self {
        Self {
            column: column.into_any_element(),
            scroll: scroll.clone(),
            followed: followed.clone(),
        }
    }
}

impl IntoElement for FollowFocus {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for FollowFocus {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.column.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.take_autoscroll();
        self.column.prepaint(window, cx);
        let target = window.take_autoscroll();
        let focused = window.focused(cx);
        let moved = {
            let mut followed = self.followed.borrow_mut();
            let moved = match (followed.as_ref(), focused.as_ref()) {
                (Some(before), Some(now)) => before != now,
                (None, None) => false,
                _ => true,
            };
            *followed = focused.as_ref().map(FocusHandle::downgrade);
            moved
        };
        if !moved || !window.last_input_was_keyboard() {
            return;
        }
        let Some(target) = target else {
            return;
        };
        let scroll = &self.scroll;
        if let Some(offset) = reveal_offset(
            scroll.bounds(),
            scroll.offset(),
            scroll.max_offset(),
            target,
            REVEAL_MARGIN,
        ) {
            scroll.set_offset(offset);
            // This frame is laid out already; the next one draws the column at its new offset.
            window.request_animation_frame();
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.column.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext, size};
    use gpui_kit::test::TestWindowExt;

    use super::*;
    use crate::{
        api::fake::FakeApi,
        test_support::{answer_polls, config_json, open_app},
    };

    /// admin.css `.form-group select` (at most 280 px) and `.log-picker` (the log box takes the row
    /// beside Refresh and the 8 px gap). `wizard::tests` has the wizard's encoder dropdown.
    #[gpui::test]
    fn dropdowns_take_their_container_width(cx: &mut TestAppContext) {
        let fake = FakeApi::new();
        answer_polls(&fake);
        fake.reply("GET", "/api/config", Ok(config_json()));
        let (_, window) = open_app(cx, fake);
        let bounds = |cx: &mut TestAppContext, tab: &'static str, id: &'static str| {
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click(tab, cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.find(id).bounds()
            })
            .unwrap()
        };

        for (tab, id) in [
            ("tab-monitor", "playlist-select"),
            ("tab-playback", "pb-encoder"),
            ("tab-webhooks", "wh-preview-select"),
        ] {
            assert_eq!(bounds(cx, tab, id).size.width, px(280.), "{id}");
        }

        // The log view below spans the row; its `input-state` sits inside a 1 px border.
        let select = bounds(cx, "tab-monitor", "log-select");
        let (refresh, view) = cx
            .update_window(window, |_, window, _| {
                let view = window.within("log-view").find("input-state").bounds();
                (window.find("log-refresh").bounds(), view)
            })
            .unwrap();
        assert_eq!(select.left(), view.left() - px(1.));
        assert_eq!(refresh.left() - select.right(), px(8.));
        assert_eq!(refresh.right(), view.right() + px(1.));
    }

    #[test]
    fn the_open_list_is_never_narrower_than_the_box() {
        // Entries that fit: the box's own width.
        assert_eq!(menu_width(px(100.), px(280.)), None);
        assert_eq!(menu_width(px(240.), px(280.)), None);
        // A longer one: its label, the row around it and the check mark.
        assert_eq!(menu_width(px(240.5), px(280.)), Some(px(281.)));
        assert_eq!(menu_width(px(360.), px(280.)), Some(px(400.)));
    }

    #[test]
    fn reveal_offset_scrolls_only_as_far_as_needed() {
        let viewport = Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.)));
        let max = point(px(0.), px(3000.));
        let at =
            |y: f32, height: f32| Bounds::new(point(px(10.), px(y)), size(px(50.), px(height)));
        let reveal = |offset: f32, target| {
            reveal_offset(viewport, point(px(0.), px(offset)), max, target, px(8.))
                .map(|offset| offset.y)
        };

        // Fully in view, however close to an edge: nothing moves.
        assert_eq!(reveal(-500., at(0., 30.)), None);
        assert_eq!(reveal(-500., at(770., 30.)), None);
        // Below: its bottom lands 8 px above the viewport's.
        assert_eq!(reveal(-500., at(790., 30.)), Some(px(-500. - 28.)));
        assert_eq!(reveal(-500., at(900., 30.)), Some(px(-500. - 138.)));
        // Above: its top lands 8 px below the viewport's.
        assert_eq!(reveal(-500., at(-100., 30.)), Some(px(-500. + 108.)));
        // Taller than the viewport: its top.
        assert_eq!(reveal(-500., at(900., 900.)), Some(px(-500. - 892.)));
        // Never past either end.
        assert_eq!(reveal(-2990., at(900., 30.)), Some(px(-3000.)));
        assert_eq!(reveal(-20., at(-100., 30.)), Some(px(0.)));
    }
}
