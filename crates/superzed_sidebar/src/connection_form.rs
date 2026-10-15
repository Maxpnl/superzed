//! Form modale di una connessione (vista Fonti): un campo per chiave di `form_fields`;
//! i segreti sono mascherati e, lasciati vuoti in modifica, restano quelli del Portachiavi.
//! Invio o cmd-invio salvano, tab/shift-tab passano da un campo all'altro, esc chiude
//! (keymap: contesto `SuperzedConnectionForm`).
use editor::{Editor, EditorEvent};
use gpui::{App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Subscription, Window};
use std::collections::BTreeMap;
use superzed_sources::{Connection, ConnectionKind, ConnectionStore, FormField, connection_from_fields, fields_of, form_fields, insecure_http_warning};
use ui::{Button, ButtonStyle, Label, LabelSize, prelude::*};
use workspace::{ModalView, Workspace};

pub struct ConnectionFormModal {
    kind: ConnectionKind,
    /// Id della connessione in modifica (`None` = nuova).
    existing: Option<String>,
    title: String,
    fields: Vec<(FormField, Entity<Editor>)>,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl ConnectionFormModal {
    fn new(kind: ConnectionKind, existing: Option<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let values = existing.as_ref().map(fields_of).unwrap_or_default();
        let fields: Vec<(FormField, Entity<Editor>)> = form_fields(kind)
            .into_iter()
            .map(|f| {
                let text = values.get(f.key).cloned().unwrap_or_default();
                let editor = cx.new(|cx| {
                    let mut e = Editor::single_line(window, cx);
                    e.set_placeholder_text(f.placeholder, window, cx);
                    e.set_text(text, window, cx);
                    if f.secret {
                        e.set_masked(true, cx);
                    }
                    e
                });
                (f, editor)
            })
            .collect();
        let subs = fields
            .iter()
            .map(|(_, e)| {
                cx.subscribe(e, |this: &mut Self, _, event: &EditorEvent, cx| {
                    if matches!(event, EditorEvent::BufferEdited) {
                        // anche l'avviso su http:// dipende dai campi
                        this.error = None;
                        cx.notify();
                    }
                })
            })
            .collect();
        let title = match &existing {
            Some(c) => format!("Edit “{}” ({})", c.name, kind.label()),
            None => format!("New {} connection", kind.label()),
        };
        Self { kind, existing: existing.map(|c| c.id), title, fields, error: None, _subs: subs }
    }

    pub fn kind(&self) -> ConnectionKind {
        self.kind
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_field(&mut self, key: &str, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, editor)) = self.fields.iter().find(|(f, _)| f.key == key) {
            editor.update(cx, |e, cx| e.set_text(text, window, cx));
        }
    }

    fn values(&self, cx: &App) -> BTreeMap<String, String> {
        self.fields.iter().map(|(f, e)| (f.key.to_string(), e.read(cx).text(cx))).collect()
    }

    /// Avviso non bloccante: http:// verso un host remoto con password o token.
    pub fn warning(&self, cx: &App) -> Option<String> {
        let stored = self.existing.as_deref().and_then(|id| ConnectionStore::global(cx)?.read(cx).get(id)?.secret_ref().map(|_| ())).is_some();
        insecure_http_warning(&self.values(cx), stored)
    }

    /// Valida e salva; con un errore resta aperto e lo mostra.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let values = self.values(cx);
        let Some(store) = ConnectionStore::global(cx) else {
            self.error = Some("Sources not available".into());
            cx.notify();
            return;
        };
        let (kind, existing_id) = (self.kind, self.existing.clone());
        let result = store.update(cx, |s, cx| {
            let existing = existing_id.as_deref().and_then(|id| s.get(id).cloned());
            let name = values.get("name").map(|n| n.trim().to_string()).unwrap_or_default();
            let id = existing.as_ref().map(|c| c.id.clone()).unwrap_or_else(|| s.new_id(&name));
            let (conn, secrets) = connection_from_fields(kind, &id, &values, existing.as_ref())?;
            s.upsert(conn, secrets, cx)
        });
        match result {
            Ok(()) => cx.emit(DismissEvent),
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    fn move_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.fields.len();
        let current = self.fields.iter().position(|(_, e)| e.focus_handle(cx).contains_focused(window, cx)).unwrap_or(0);
        let next = if forward { (current + 1) % n } else { (current + n - 1) % n };
        let handle = self.fields[next].1.focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        self.save(cx);
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

pub fn open_connection_form(
    workspace: &mut Workspace,
    kind: ConnectionKind,
    existing: Option<Connection>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace.toggle_modal(window, cx, move |window, cx| ConnectionFormModal::new(kind, existing, window, cx));
}

impl EventEmitter<DismissEvent> for ConnectionFormModal {}
impl ModalView for ConnectionFormModal {}

impl Focusable for ConnectionFormModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.fields[0].1.focus_handle(cx)
    }
}

impl Render for ConnectionFormModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let fields = self.fields.iter().map(|(f, editor)| {
            v_flex()
                .gap_0p5()
                .child(Label::new(f.label).color(Color::Muted))
                .child(
                    div()
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.editor_background)
                        .child(editor.clone()),
                )
        });
        v_flex()
            .key_context("SuperzedConnectionForm")
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .elevation_3(cx)
            .w(rems(34.))
            .p_4()
            .gap_2()
            .child(Label::new(self.title.clone()).size(LabelSize::Large))
            .children(fields)
            .when_some(self.warning(cx), |d, w| d.child(Label::new(w).size(LabelSize::Small).color(Color::Warning)))
            .when_some(self.error.clone(), |d, e| d.child(Label::new(e).color(Color::Error)))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .pt_2()
                    .child(Button::new("superzed-connection-cancel", "Cancel").on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))))
                    .child(
                        Button::new("superzed-connection-save", "Save")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}
