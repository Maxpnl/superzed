// superzed: conferma in un piccolo modale: messaggio, «Annulla» e il bottone dell'azione;
// anche da tastiera, y/invio = si', n/esc = no. Con un'azione secondaria y/invio vanno a
// quella: l'azione principale di quei modali e' distruttiva e si raggiunge solo col click.
use crate::{ModalView, Workspace};
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    Render, SharedString, Window,
};
use ui::{Button, ButtonStyle, KeyBinding, TintColor, Tooltip, prelude::*};

pub fn confirm_key(key: &str) -> Option<bool> {
    match key {
        "y" | "enter" => Some(true),
        "n" | "escape" => Some(false),
        _ => None,
    }
}

pub struct SecondaryAction {
    pub label: SharedString,
    /// `Some(motivo)`: bottone disabilitato col motivo nel tooltip.
    pub disabled_reason: Option<SharedString>,
    pub on_click: Box<dyn FnOnce(&mut Window, &mut App)>,
}

pub struct ConfirmModal {
    message: SharedString,
    /// Etichetta del bottone dell'azione (es. «Close», «Delete»).
    action_label: SharedString,
    /// Azione distruttiva: bottone in rosso.
    destructive: bool,
    on_confirm: Option<Box<dyn FnOnce(&mut Window, &mut App)>>,
    secondary: Option<SecondaryAction>,
    focus: FocusHandle,
}

impl ConfirmModal {
    pub fn new(
        message: SharedString,
        action_label: SharedString,
        destructive: bool,
        on_confirm: impl FnOnce(&mut Window, &mut App) + 'static,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message,
            action_label,
            destructive,
            on_confirm: Some(Box::new(on_confirm)),
            secondary: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn with_secondary(mut self, secondary: SecondaryAction) -> Self {
        self.secondary = Some(secondary);
        self
    }

    fn answer_secondary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // un secondario disabilitato non deve ne' scattare ne' chiudere il modale
        if self
            .secondary
            .as_ref()
            .is_none_or(|secondary| secondary.disabled_reason.is_some())
        {
            return;
        }
        let Some(secondary) = self.secondary.take() else {
            return;
        };
        self.on_confirm = None;
        cx.emit(DismissEvent);
        (secondary.on_click)(window, cx);
    }

    /// Risposta da tastiera (y/n, invio/esc).
    fn answer_key(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        if yes && self.secondary.is_some() {
            self.answer_secondary(window, cx);
        } else {
            self.answer(yes, window, cx);
        }
    }

    fn answer(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let on_confirm = self.on_confirm.take();
        cx.emit(DismissEvent);
        if yes && let Some(f) = on_confirm {
            f(window, cx);
        }
    }
}

/// Conferma distruttiva con il bottone «Confirm».
pub fn superzed_confirm(
    workspace: &mut Workspace,
    message: impl Into<SharedString>,
    on_confirm: impl FnOnce(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    superzed_confirm_action(workspace, message, "Confirm", true, on_confirm, window, cx)
}

/// Conferma con l'etichetta del bottone dell'azione scelta dal chiamante.
pub fn superzed_confirm_action(
    workspace: &mut Workspace,
    message: impl Into<SharedString>,
    action_label: impl Into<SharedString>,
    destructive: bool,
    on_confirm: impl FnOnce(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let (message, action_label) = (message.into(), action_label.into());
    workspace.toggle_modal(window, cx, move |window, cx| {
        ConfirmModal::new(message, action_label, destructive, on_confirm, window, cx)
    });
}

/// Come `superzed_confirm_action`, con un secondo bottone tra «Cancel» e l'azione.
pub fn superzed_confirm_with_secondary(
    workspace: &mut Workspace,
    message: impl Into<SharedString>,
    action_label: impl Into<SharedString>,
    destructive: bool,
    on_confirm: impl FnOnce(&mut Window, &mut App) + 'static,
    secondary: SecondaryAction,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let (message, action_label) = (message.into(), action_label.into());
    workspace.toggle_modal(window, cx, move |window, cx| {
        ConfirmModal::new(message, action_label, destructive, on_confirm, window, cx)
            .with_secondary(secondary)
    });
}

impl EventEmitter<DismissEvent> for ConfirmModal {}
impl ModalView for ConfirmModal {}

impl Focusable for ConfirmModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConfirmModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let key = |action: &dyn gpui::Action, focus: &FocusHandle, cx: &App| {
            KeyBinding::for_action_in(action, focus, cx).map(|kb| kb.size(rems_from_px(12_f32)))
        };
        let cancel = Button::new("superzed-confirm-cancel", "Cancel")
            .style(ButtonStyle::Subtle)
            .key_binding(key(&menu::Cancel, &self.focus, cx))
            .on_click(cx.listener(|this, _, window, cx| this.answer(false, window, cx)));
        let ok = Button::new("superzed-confirm-ok", self.action_label.clone())
            .style(if self.destructive {
                ButtonStyle::Tinted(TintColor::Error)
            } else {
                ButtonStyle::Filled
            })
            .when(self.secondary.is_none(), |button| {
                button.key_binding(key(&menu::Confirm, &self.focus, cx))
            })
            .on_click(cx.listener(|this, _, window, cx| this.answer(true, window, cx)));
        let secondary = self.secondary.as_ref().map(|secondary| {
            let disabled = secondary.disabled_reason.clone();
            let button = Button::new("superzed-confirm-secondary", secondary.label.clone())
                .style(ButtonStyle::Filled)
                .disabled(disabled.is_some())
                .key_binding(key(&menu::Confirm, &self.focus, cx))
                .on_click(cx.listener(|this, _, window, cx| this.answer_secondary(window, cx)));
            div()
                .id("superzed-confirm-secondary-slot")
                .debug_selector(|| "superzed-confirm-secondary".into())
                .when_some(disabled, |slot, reason| slot.tooltip(Tooltip::text(reason)))
                .child(button)
        });
        v_flex()
            .key_context("SuperzedConfirm")
            .track_focus(&self.focus)
            .elevation_3(cx)
            .min_w(rems(20.))
            .max_w(rems(32.))
            .p_3()
            .gap_3()
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                // cmd-y, ctrl-n... non sono risposte (shift si': "Y" maiuscola)
                let m = &e.keystroke.modifiers;
                if m.control || m.alt || m.platform || m.function {
                    return;
                }
                if let Some(yes) = confirm_key(&e.keystroke.key) {
                    cx.stop_propagation();
                    this.answer_key(yes, window, cx);
                }
            }))
            // invio ed esc nel keymap globale sono menu::Confirm / menu::Cancel e
            // vengono risolti prima di on_key_down
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| this.answer_key(true, window, cx)))
            .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| this.answer_key(false, window, cx)))
            .child(Label::new(self.message.clone()).size(LabelSize::Default))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(div().debug_selector(|| "superzed-confirm-cancel".into()).child(cancel))
                    .children(secondary)
                    .child(div().debug_selector(|| "superzed-confirm-ok".into()).child(ok)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MultiWorkspace;
    use crate::tests::init_test;
    use gpui::{TestAppContext, VisualTestContext};
    use project::Project;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn keys() {
        assert_eq!(confirm_key("y"), Some(true));
        assert_eq!(confirm_key("s"), None, "s non e' piu' si'");
        assert_eq!(confirm_key("enter"), Some(true));
        assert_eq!(confirm_key("n"), Some(false));
        assert_eq!(confirm_key("escape"), Some(false));
        assert_eq!(confirm_key("x"), None);
    }

    // superzed: il layer dei modali e i listener di Workspace::register_action sono
    // agganciati dal render di MultiWorkspace, non da quello nudo di Workspace: la
    // radice della finestra di test e' un MultiWorkspace, da cui si prende il workspace.
    async fn open(cx: &mut TestAppContext) -> (Rc<Cell<u32>>, &mut VisualTestContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (mw, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let count = Rc::new(Cell::new(0));
        let c = count.clone();
        workspace.update_in(cx, |ws, window, cx| {
            superzed_confirm(ws, "Close the node?", move |_, _| c.set(c.get() + 1), window, cx)
        });
        cx.run_until_parked();
        (count, cx)
    }

    #[gpui::test]
    async fn y_confirms_once_and_closes(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert_eq!(count.get(), 1);
        // il modale e' chiuso: una seconda y non conferma di nuovo
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert_eq!(count.get(), 1);
    }

    #[gpui::test]
    async fn s_does_not_answer(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        cx.simulate_keystrokes("s");
        cx.run_until_parked();
        assert_eq!(count.get(), 0);
        // il modale e' ancora aperto: y conferma
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert_eq!(count.get(), 1);
    }

    #[gpui::test]
    async fn the_cancel_button_cancels(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        let cancel = cx.debug_bounds("superzed-confirm-cancel").expect("bottone Annulla");
        cx.simulate_click(cancel.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(count.get(), 0);
        assert!(cx.debug_bounds("superzed-confirm-cancel").is_none(), "Annulla chiude il modale");
    }

    #[gpui::test]
    async fn the_action_button_confirms(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        let ok = cx.debug_bounds("superzed-confirm-ok").expect("bottone dell'azione");
        cx.simulate_click(ok.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(count.get(), 1);
    }

    #[gpui::test]
    async fn n_cancels(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        cx.simulate_keystrokes("n");
        cx.run_until_parked();
        assert_eq!(count.get(), 0);
    }

    #[gpui::test]
    async fn enter_confirms(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(count.get(), 1);
    }

    #[gpui::test]
    async fn keys_with_modifiers_do_not_answer(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        cx.simulate_keystrokes("cmd-y ctrl-y alt-y ctrl-n cmd-n");
        cx.run_until_parked();
        assert_eq!(count.get(), 0);
        // il modale e' ancora aperto: y conferma
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert_eq!(count.get(), 1);
    }

    #[gpui::test]
    async fn escape_cancels(cx: &mut TestAppContext) {
        let (count, cx) = open(cx).await;
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(count.get(), 0);
    }

    async fn open_with_secondary(
        cx: &mut TestAppContext,
        disabled: bool,
    ) -> (Rc<Cell<u32>>, Rc<Cell<u32>>, &mut VisualTestContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (mw, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let (primary, secondary) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let (p, s) = (primary.clone(), secondary.clone());
        workspace.update_in(cx, |ws, window, cx| {
            superzed_confirm_with_secondary(
                ws,
                "Close?",
                "Close anyway",
                true,
                move |_, _| p.set(p.get() + 1),
                SecondaryAction {
                    label: "Commit & close".into(),
                    disabled_reason: disabled.then(|| "Wait for the chat to finish".into()),
                    on_click: Box::new(move |_, _| s.set(s.get() + 1)),
                },
                window,
                cx,
            )
        });
        cx.run_until_parked();
        (primary, secondary, cx)
    }

    #[gpui::test]
    async fn the_secondary_button_runs_only_its_action(cx: &mut TestAppContext) {
        let (primary, secondary, cx) = open_with_secondary(cx, false).await;
        let button = cx.debug_bounds("superzed-confirm-secondary").expect("bottone secondario");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!((primary.get(), secondary.get()), (0, 1));
        assert!(cx.debug_bounds("superzed-confirm-secondary").is_none(), "il modale si chiude");
    }

    #[gpui::test]
    async fn a_disabled_secondary_does_nothing_and_enter_does_not_close_anyway(cx: &mut TestAppContext) {
        let (primary, secondary, cx) = open_with_secondary(cx, true).await;
        let button = cx.debug_bounds("superzed-confirm-secondary").expect("bottone secondario");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(secondary.get(), 0);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert_eq!((primary.get(), secondary.get()), (0, 0));
        assert!(cx.debug_bounds("superzed-confirm-secondary").is_some(), "il modale resta aperto");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!((primary.get(), secondary.get()), (0, 0));
        assert!(cx.debug_bounds("superzed-confirm-secondary").is_none(), "esc chiude");
    }

    #[gpui::test]
    async fn enter_runs_the_enabled_secondary_not_the_action(cx: &mut TestAppContext) {
        let (primary, secondary, cx) = open_with_secondary(cx, false).await;
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!((primary.get(), secondary.get()), (0, 1));
        assert!(cx.debug_bounds("superzed-confirm-secondary").is_none(), "il modale si chiude");
    }

    #[gpui::test]
    async fn y_runs_the_enabled_secondary(cx: &mut TestAppContext) {
        let (primary, secondary, cx) = open_with_secondary(cx, false).await;
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert_eq!((primary.get(), secondary.get()), (0, 1));
    }

    #[gpui::test]
    async fn n_cancels_with_a_secondary(cx: &mut TestAppContext) {
        let (primary, secondary, cx) = open_with_secondary(cx, false).await;
        cx.simulate_keystrokes("n");
        cx.run_until_parked();
        assert_eq!((primary.get(), secondary.get()), (0, 0));
        assert!(cx.debug_bounds("superzed-confirm-secondary").is_none(), "n chiude");
    }
}
