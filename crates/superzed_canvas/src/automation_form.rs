//! Form di creazione e modifica di un'automazione: nome, frequenza (con l'anteprima
//! della prossima esecuzione), account e prompt. Invio nei campi di una riga salva, nel prompt va
//! a capo; cmd-invio salva da ogni campo; tab/shift-tab passano da un campo all'altro;
//! esc chiude (keymap: contesto `SuperzedAutomationForm`).
use crate::automation::{AutomationDraft, validate_draft};
use crate::schedule;
use chrono::Local;
use editor::{Editor, EditorEvent};
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Render, SharedString, Subscription, Window,
};
use ui::{Button, ButtonStyle, ContextMenu, DropdownMenu, DropdownStyle, Label, LabelSize, prelude::*};
use workspace::{ModalView, Workspace};

/// Frequenza proposta in un form nuovo.
const SUGGESTED_SCHEDULE: &str = "4h";

/// Una voce del menu Account: `id` `None` = lo sceglie lo scheduler.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountChoice {
    pub id: Option<String>,
    pub label: SharedString,
}

impl AccountChoice {
    pub fn automatic() -> Self {
        Self { id: None, label: "Automatic".into() }
    }
}

pub struct AutomationFormModal {
    title: SharedString,
    name: Entity<Editor>,
    schedule: Entity<Editor>,
    prompt: Entity<Editor>,
    accounts: Vec<AccountChoice>,
    account: Option<String>,
    error: Option<String>,
    /// I campi com'erano quando l'errore e' stato mostrato: modificandoli l'errore sparisce.
    error_fields: [String; 3],
    /// Anteprima della prossima esecuzione, o l'errore del parser della frequenza.
    preview: String,
    preview_is_error: bool,
    on_submit: Option<Box<dyn FnOnce(AutomationDraft, &mut Window, &mut App)>>,
    _subs: Vec<Subscription>,
}

impl AutomationFormModal {
    fn new(
        title: SharedString,
        initial: Option<AutomationDraft>,
        mut accounts: Vec<AccountChoice>,
        on_submit: impl FnOnce(AutomationDraft, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = initial.unwrap_or_else(|| AutomationDraft {
            name: String::new(),
            schedule: SUGGESTED_SCHEDULE.to_string(),
            prompt: String::new(),
            account: None,
        });
        if !accounts.iter().any(|a| a.id.is_none()) {
            accounts.insert(0, AccountChoice::automatic());
        }
        // tolto dal registro: resta visibile (e selezionato) invece di diventare "Automatic"
        if let Some(id) = initial.account.as_ref().filter(|id| !accounts.iter().any(|a| a.id.as_ref() == Some(*id))) {
            accounts.push(AccountChoice { id: Some(id.clone()), label: format!("{id} (not registered)").into() });
        }
        let name = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text("Production logs", window, cx);
            e.set_text(initial.name.as_str(), window, cx);
            e
        });
        let schedule = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text("15m, 4h, 1d, 09:00, mon-fri 09:00", window, cx);
            e.set_text(initial.schedule.as_str(), window, cx);
            e
        });
        let prompt = cx.new(|cx| {
            let mut e = Editor::auto_height(6, 16, window, cx);
            e.set_placeholder_text("What claude should do on each run", window, cx);
            e.set_text(initial.prompt.as_str(), window, cx);
            e
        });
        let subs = [&name, &schedule, &prompt]
            .into_iter()
            .map(|editor| {
                cx.subscribe(editor, |this: &mut Self, _, event: &EditorEvent, cx| {
                    if matches!(event, EditorEvent::BufferEdited) {
                        this.refresh_preview(cx);
                        if this.error.is_some() && this.fields(cx) != this.error_fields {
                            this.error = None;
                        }
                        cx.notify();
                    }
                })
            })
            .collect();
        let mut this = Self {
            title,
            name,
            schedule,
            prompt,
            accounts,
            account: initial.account,
            error: None,
            error_fields: Default::default(),
            preview: String::new(),
            preview_is_error: false,
            on_submit: Some(Box::new(on_submit)),
            _subs: subs,
        };
        this.refresh_preview(cx);
        this
    }

    fn fields(&self, cx: &App) -> [String; 3] {
        [&self.name, &self.schedule, &self.prompt].map(|e| e.read(cx).text(cx))
    }

    fn editors(&self) -> [&Entity<Editor>; 3] {
        [&self.name, &self.schedule, &self.prompt]
    }

    /// Tab / shift-tab: il campo dopo (o prima), in giro.
    fn move_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let editors = self.editors();
        let current = editors.iter().position(|e| e.focus_handle(cx).contains_focused(window, cx)).unwrap_or(0);
        let next = if forward { (current + 1) % 3 } else { (current + 2) % 3 };
        let handle = editors[next].focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn refresh_preview(&mut self, cx: &App) {
        let text = self.schedule.read(cx).text(cx);
        match schedule::preview(&text, Local::now()) {
            Ok(p) => {
                self.preview = p;
                self.preview_is_error = false;
            }
            Err(e) => {
                self.preview = e;
                self.preview_is_error = true;
            }
        }
    }

    /// Imposta i tre campi (e ricalcola l'anteprima).
    pub fn set_fields(&mut self, name: &str, schedule: &str, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |e, cx| e.set_text(name, window, cx));
        self.schedule.update(cx, |e, cx| e.set_text(schedule, window, cx));
        self.prompt.update(cx, |e, cx| e.set_text(prompt, window, cx));
        self.refresh_preview(cx);
        cx.notify();
    }

    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    pub fn set_account(&mut self, account: Option<String>, cx: &mut Context<Self>) {
        self.account = account;
        cx.notify();
    }

    fn account_label(&self) -> SharedString {
        self.accounts
            .iter()
            .find(|a| a.id == self.account)
            .map(|a| a.label.clone())
            .unwrap_or_else(|| AccountChoice::automatic().label)
    }

    /// Valida i campi: con un errore lo mostra e resta aperto, altrimenti chiude e consegna.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fields = self.fields(cx);
        let [name, schedule, prompt] = &fields;
        match validate_draft(name, schedule, prompt, self.account.clone()) {
            Err(e) => {
                self.error = Some(e);
                self.error_fields = fields;
                cx.notify();
            }
            Ok(draft) => {
                self.error = None;
                let on_submit = self.on_submit.take();
                cx.emit(DismissEvent);
                if let Some(f) = on_submit {
                    f(draft, window, cx);
                }
            }
        }
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn preview(&self) -> &str {
        &self.preview
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    fn secondary_confirm(&mut self, _: &menu::SecondaryConfirm, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn select_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(true, window, cx);
    }

    fn select_previous(&mut self, _: &menu::SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(false, window, cx);
    }
}

/// Apre il form come modale del workspace; `on_submit` riceve i campi validati.
pub fn open_automation_form(
    workspace: &mut Workspace,
    title: &str,
    initial: Option<AutomationDraft>,
    accounts: Vec<AccountChoice>,
    on_submit: impl FnOnce(AutomationDraft, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let title: SharedString = title.to_string().into();
    workspace.toggle_modal(window, cx, move |window, cx| {
        AutomationFormModal::new(title, initial, accounts, on_submit, window, cx)
    });
}

impl EventEmitter<DismissEvent> for AutomationFormModal {}
impl ModalView for AutomationFormModal {}

impl Focusable for AutomationFormModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

impl Render for AutomationFormModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity().downgrade();
        let choices = self.accounts.clone();
        let account_menu = ContextMenu::build(window, cx, move |mut menu, _, _| {
            for choice in choices {
                let form = form.clone();
                menu = menu.entry(choice.label.clone(), None, move |_, cx| {
                    form.update(cx, |f, cx| f.set_account(choice.id.clone(), cx)).ok();
                });
            }
            menu
        });
        let colors = cx.theme().colors();
        let field = |editor: &Entity<Editor>| {
            div()
                .w_full()
                .px_2()
                .py_1()
                .rounded_sm()
                .border_1()
                .border_color(colors.border)
                .bg(colors.editor_background)
                .child(editor.clone())
        };
        let label = |text: &'static str| Label::new(text).size(LabelSize::Default).color(Color::Muted);
        let preview_color = if self.preview_is_error { Color::Error } else { Color::Muted };
        v_flex()
            .key_context("SuperzedAutomationForm")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::secondary_confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .elevation_3(cx)
            .w(rems(36.))
            .p_4()
            .gap_2()
            .child(Label::new(self.title.clone()).size(LabelSize::Large))
            .child(label("Name"))
            .child(field(&self.name))
            .child(label("Schedule"))
            .child(field(&self.schedule))
            .child(Label::new(self.preview.clone()).size(LabelSize::Small).color(preview_color))
            .child(label("Account"))
            .child(
                DropdownMenu::new("superzed-automation-account", self.account_label(), account_menu)
                    .style(DropdownStyle::Outlined)
                    .full_width(true),
            )
            .child(label("Prompt"))
            .child(field(&self.prompt))
            .when_some(self.error.clone(), |d, e| d.child(Label::new(e).color(Color::Error)))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(
                        Button::new("superzed-automation-cancel", "Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new("superzed-automation-save", "Save")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
