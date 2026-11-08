//! Controls the views share: the action button (admin.js `runAction`), the restart note, the
//! config save's reload warning, the settings forms' layout and inputs, and the focus reveal.

use std::{collections::HashSet, time::Duration};

use gpui::{
    AnyElement, App, AppContext, Bounds, Context, Div, Element, ElementId, Entity, FontWeight,
    Global, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, SharedString,
    Styled, Subscription, Window, div, prelude::*, px, rgb, rgba,
};
use gpui_kit::{
    base::TestSupportExt,
    component::{
        Disableable, Sizable,
        button::Button,
        input::{Input, InputState, TextareaState},
        label::Label,
    },
};

use crate::{
    api::{ApiError, client::Client, types::SaveResult},
    app::{AppHandle, ToastKind},
    logic::is_benign_reload_error,
    theme::{BG, BORDER, MUTED, RED, SURFACE},
};

/// admin.js puts a save button's label back 1.5 s after a save.
pub const SAVE_RESET: Duration = Duration::from_millis(1500);

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
