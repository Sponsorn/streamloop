//! Controls the views share: the action button, admin.js `runAction`.

use std::collections::HashSet;

use gpui::{AppContext, Context, ElementId, SharedString, Styled, Window, px};
use gpui_kit::component::{Disableable, Sizable, button::Button};

use crate::{
    api::{ApiError, client::Client},
    app::{AppHandle, ToastKind},
};

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
