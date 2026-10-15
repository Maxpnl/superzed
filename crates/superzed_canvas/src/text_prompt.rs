// prompt di una riga (rinomina di nodi e tab): invio conferma, esc annulla
use editor::Editor;
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, SharedString, Window,
};
use ui::prelude::*;
use workspace::{ModalView, Workspace};

pub struct TextPromptModal {
    title: SharedString,
    editor: Entity<Editor>,
    on_submit: Option<Box<dyn FnOnce(String, &mut Window, &mut App)>>,
}

impl TextPromptModal {
    fn new(
        title: SharedString,
        initial: &str,
        on_submit: impl FnOnce(String, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_text(initial, window, cx);
            editor.select_all(&Default::default(), window, cx);
            editor
        });
        Self {
            title,
            editor,
            on_submit: Some(Box::new(on_submit)),
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text(cx).trim().to_string();
        let on_submit = self.on_submit.take();
        cx.emit(DismissEvent);
        if let Some(f) = on_submit {
            f(text, window, cx);
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

pub fn text_prompt(
    workspace: &mut Workspace,
    title: impl Into<SharedString>,
    initial: &str,
    on_submit: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let title = title.into();
    let initial = initial.to_string();
    workspace.toggle_modal(window, cx, move |window, cx| {
        TextPromptModal::new(title, &initial, on_submit, window, cx)
    });
}

impl EventEmitter<DismissEvent> for TextPromptModal {}
impl ModalView for TextPromptModal {}

impl Focusable for TextPromptModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for TextPromptModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("SuperzedTextPrompt")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .elevation_3(cx)
            .w(rems(24.))
            .p_2()
            .gap_1()
            .child(Label::new(self.title.clone()).size(LabelSize::Small).color(Color::Muted))
            .child(self.editor.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::workspace_with_repo;
    use gpui::{KeyBinding, TestAppContext};
    use serde_json::json;
    use std::{cell::RefCell, rc::Rc};

    #[gpui::test]
    async fn submit_returns_trimmed_text_and_escape_cancels(cx: &mut TestAppContext) {
        let (workspace, _canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        // superzed: test_support::init_test non carica il keymap di default; il prompt
        // si affida solo a menu::Confirm/menu::Cancel (a differenza di ConfirmModal, che
        // ha anche un on_key_down grezzo), quindi qui servono i binding minimi.
        cx.update(|_, cx| {
            menu::init();
            cx.bind_keys([
                KeyBinding::new("enter", menu::Confirm, None),
                KeyBinding::new("escape", menu::Cancel, None),
            ]);
        });
        let got = Rc::new(RefCell::new(None));
        let g = got.clone();
        workspace.update_in(cx, |ws, window, cx| {
            text_prompt(ws, "Rinomina", "vecchio", move |t, _, _| *g.borrow_mut() = Some(t), window, cx)
        });
        cx.run_until_parked();
        // il testo iniziale e' selezionato: si scrive sopra
        cx.simulate_input(" nuovo ");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(got.borrow().as_deref(), Some("nuovo"));

        let g = got.clone();
        workspace.update_in(cx, |ws, window, cx| {
            text_prompt(ws, "Rinomina", "x", move |t, _, _| *g.borrow_mut() = Some(t), window, cx)
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(got.borrow().as_deref(), Some("nuovo"), "esc non chiama on_submit");
    }

    #[gpui::test]
    async fn rename_project_tab_action_sets_tab_name(cx: &mut TestAppContext) {
        let (workspace, _canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| {
            menu::init();
            cx.bind_keys([
                KeyBinding::new("enter", menu::Confirm, None),
                KeyBinding::new("escape", menu::Cancel, None),
            ]);
        });
        workspace.update_in(cx, |ws, window, cx| window.focus(&ws.focus_handle(cx), cx));
        cx.dispatch_action(superzed_actions::RenameProjectTab);
        cx.run_until_parked();
        cx.simulate_input("acme-app");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        workspace.read_with(cx, |ws, _| {
            assert_eq!(ws.superzed_state().tab_name.as_ref().map(|n| n.as_ref()), Some("acme-app"));
        });
    }
}
