//! Anteprima "Update GitHub": le modifiche proposte dal report di una chat in Fatto,
//! ognuna con la sua spunta; titolo e corpo delle issue nuove e commento delle issue
//! toccate modificabili, Area e Status a scelta. Scrive solo il pulsante «Scrivi su
//! GitHub»: il modale non gestisce `menu::Confirm` (Invio nei campi non fa nulla, nel corpo
//! va a capo) ed Esc chiude. Aprirlo di nuovo per la stessa sessione (un altro «Rivedi») lo
//! lascia aperto.
use editor::{Editor, EditorEvent};
use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, Render, SharedString, Subscription, Window,
};
use superzed_done::{InvalidField, Preview, TouchAction, actions, touch_label, validate_fields};
use ui::{
    Button, ButtonLink, ButtonSize, ButtonStyle, Checkbox, Icon, IconName, IconSize, Label, LabelSize, TintColor,
    ToggleState, prelude::*,
};
use workspace::{ModalView, Workspace};

type OnWrite = Box<dyn FnOnce(Preview, &mut Window, &mut App)>;

pub struct DoneReviewModal {
    chat_title: SharedString,
    preview: Preview,
    /// Titolo e corpo di ogni issue nuova, nello stesso ordine di `preview.new_issues`.
    titles: Vec<Entity<Editor>>,
    bodies: Vec<Entity<Editor>>,
    /// Commento di ogni issue toccata, nello stesso ordine di `preview.touched`.
    notes: Vec<Entity<Editor>>,
    summary: Option<Entity<Editor>>,
    /// Avviso fisso in cima (es. il file del report cambiato dopo la prima scrittura).
    warning: Option<String>,
    error: Option<String>,
    error_field: Option<InvalidField>,
    focus: FocusHandle,
    on_write: Option<OnWrite>,
    _subscriptions: Vec<Subscription>,
}

fn text_editor(text: &str, placeholder: &str, single_line: bool, window: &mut Window, cx: &mut App) -> Entity<Editor> {
    cx.new(|cx| {
        let mut e = if single_line { Editor::single_line(window, cx) } else { Editor::auto_height(2, 8, window, cx) };
        e.set_text(text, window, cx);
        e.set_placeholder_text(placeholder, window, cx);
        e
    })
}

/// Testo del pulsante finale, con le chiusure contate a parte.
pub fn write_label(pending: usize, closes: usize) -> String {
    match closes {
        0 => format!("Write to GitHub ({pending})"),
        1 => format!("Write to GitHub ({pending}), including 1 close"),
        n => format!("Write to GitHub ({pending}), including {n} closes"),
    }
}

impl DoneReviewModal {
    pub fn new(
        chat_title: String,
        preview: Preview,
        warning: Option<String>,
        on_write: impl FnOnce(Preview, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let titles: Vec<_> =
            preview.new_issues.iter().map(|n| text_editor(&n.title, "Issue title", true, window, cx)).collect();
        let bodies: Vec<_> =
            preview.new_issues.iter().map(|n| text_editor(&n.body, "Issue description", false, window, cx)).collect();
        let notes: Vec<_> = preview
            .touched
            .iter()
            .map(|t| text_editor(&t.note, "Comment to post on the issue", false, window, cx))
            .collect();
        let summary = preview.summary.as_ref().map(|s| text_editor(&s.body, "Session summary", false, window, cx));
        // il conteggio del pulsante e le spunte seguono i testi mentre si scrive: i testi
        // che cambiano il conteggio (commenti, riepilogo) si copiano nell'anteprima, cosi'
        // il render non rilegge ogni campo (`collect` serve solo alla scrittura)
        let mut subscriptions: Vec<Subscription> = titles
            .iter()
            .chain(&bodies)
            .map(|e| {
                cx.subscribe(e, |_, _, event: &EditorEvent, cx| {
                    if matches!(event, EditorEvent::BufferEdited) {
                        cx.notify();
                    }
                })
            })
            .collect();
        for (ix, e) in notes.iter().enumerate() {
            subscriptions.push(cx.subscribe(e, move |m: &mut Self, editor, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    let text = editor.read(cx).text(cx);
                    if let Some(t) = m.preview.touched.get_mut(ix) {
                        t.note = text;
                        // un commento svuotato non ha più niente da scrivere
                        if !t.writable() {
                            t.enabled = false;
                        }
                    }
                    cx.notify();
                }
            }));
        }
        if let Some(e) = &summary {
            subscriptions.push(cx.subscribe(e, |m: &mut Self, editor, event: &EditorEvent, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    let text = editor.read(cx).text(cx);
                    if let Some(s) = m.preview.summary.as_mut() {
                        s.body = text;
                    }
                    cx.notify();
                }
            }));
        }
        Self {
            chat_title: chat_title.into(),
            preview,
            titles,
            bodies,
            notes,
            summary,
            warning,
            error: None,
            error_field: None,
            focus: cx.focus_handle(),
            on_write: Some(Box::new(on_write)),
            _subscriptions: subscriptions,
        }
    }

    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn error_field(&self) -> Option<InvalidField> {
        self.error_field
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn session_id(&self) -> &str {
        &self.preview.session_id
    }

    pub fn title_editor(&self, ix: usize) -> Option<&Entity<Editor>> {
        self.titles.get(ix)
    }

    pub fn note_editor(&self, ix: usize) -> Option<&Entity<Editor>> {
        self.notes.get(ix)
    }

    /// L'anteprima con i testi attuali dei campi.
    pub fn collect(&self, cx: &App) -> Preview {
        let mut p = self.preview.clone();
        for (n, (title, body)) in p.new_issues.iter_mut().zip(self.titles.iter().zip(&self.bodies)) {
            n.title = title.read(cx).text(cx);
            n.body = body.read(cx).text(cx);
        }
        for (t, note) in p.touched.iter_mut().zip(&self.notes) {
            if !t.written {
                t.note = note.read(cx).text(cx);
            }
        }
        if let (Some(s), Some(editor)) = (p.summary.as_mut(), &self.summary) {
            s.body = editor.read(cx).text(cx);
        }
        p
    }

    /// Voci che verrebbero scritte adesso, per contarle. Dall'anteprima tenuta in pari
    /// dalle sottoscrizioni: spunte, commenti e riepilogo ci sono gia'; titoli e corpi
    /// delle issue nuove no, ma non cambiano il conteggio.
    fn planned(&self) -> Vec<superzed_done::Action> {
        actions(&self.preview)
    }

    /// Quante voci verrebbero scritte adesso.
    pub fn pending(&self, _: &App) -> usize {
        self.planned().len()
    }

    /// Quante issue verrebbero chiuse adesso.
    pub fn closes(&self, _: &App) -> usize {
        superzed_done::closes(&self.planned())
    }

    fn clear_error(&mut self) {
        self.error = None;
        self.error_field = None;
    }

    pub fn toggle_touched(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(t) = self.preview.touched.get_mut(ix).filter(|t| !t.written && t.writable()) {
            t.enabled = !t.enabled;
            self.clear_error();
            cx.notify();
        }
    }

    pub fn toggle_new(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(n) = self.preview.new_issues.get_mut(ix).filter(|n| !n.written) {
            n.enabled = !n.enabled;
            self.clear_error();
            cx.notify();
        }
    }

    pub fn toggle_summary(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.preview.summary.as_mut().filter(|s| !s.written) {
            s.enabled = !s.enabled;
            self.clear_error();
            cx.notify();
        }
    }

    /// Status di una issue toccata con `update`: il successivo, in giro.
    pub fn cycle_status(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(t) = self.preview.touched.get_mut(ix).filter(|t| t.action == TouchAction::Update && !t.written) {
            t.status = t.status.next();
            cx.notify();
        }
    }

    /// Area di una issue nuova: la successiva fra quelle della config, poi nessuna.
    pub fn cycle_area(&mut self, ix: usize, cx: &mut Context<Self>) {
        let areas = self.preview.areas.clone();
        if let Some(n) = self.preview.new_issues.get_mut(ix).filter(|n| !n.written) {
            n.area = match &n.area {
                None => areas.first().cloned(),
                Some(a) => areas.iter().position(|x| x == a).and_then(|i| areas.get(i + 1)).cloned(),
            };
            self.clear_error();
            cx.notify();
        }
    }

    /// «Scrivi su GitHub»: valida, chiude e consegna l'anteprima. L'unico punto che scrive.
    pub fn write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let preview = self.collect(cx);
        if let Err((field, message)) = validate_fields(&preview) {
            self.error = Some(message);
            self.error_field = Some(field);
            cx.notify();
            return;
        }
        if actions(&preview).is_empty() {
            self.error = Some("Nothing to write".into());
            self.error_field = None;
            cx.notify();
            return;
        }
        let Some(on_write) = self.on_write.take() else {
            return;
        };
        cx.emit(DismissEvent);
        on_write(preview, window, cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

/// Apre l'anteprima come modale del workspace; `on_write` riceve le voci scelte. Gia'
/// aperta per la stessa sessione: resta com'e' (apre, non alterna); per un'altra: la sostituisce.
pub fn open_done_review(
    workspace: &mut Workspace,
    chat_title: String,
    preview: Preview,
    warning: Option<String>,
    on_write: impl FnOnce(Preview, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(open) = workspace.active_modal::<DoneReviewModal>(cx) {
        if open.read(cx).session_id() == preview.session_id {
            return;
        }
        // un toggle con la stessa modale aperta la chiuderebbe soltanto
        if !workspace.hide_modal(window, cx) {
            return;
        }
    }
    workspace.toggle_modal(window, cx, move |window, cx| DoneReviewModal::new(chat_title, preview, warning, on_write, window, cx));
}

impl EventEmitter<DismissEvent> for DoneReviewModal {}
impl ModalView for DoneReviewModal {}

impl Focusable for DoneReviewModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DoneReviewModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (border, editor_bg) = (colors.border, colors.editor_background);
        let error_border = cx.theme().status().error;
        let error_field = self.error_field;
        // il campo in errore ha il bordo rosso
        let field = move |editor: &Entity<Editor>, invalid: bool| {
            div()
                .w_full()
                .px_2()
                .py_1p5()
                .rounded_sm()
                .border_1()
                .border_color(if invalid { error_border } else { border })
                .bg(editor_bg)
                .child(editor.clone())
        };
        let field_label = |text: &'static str| Label::new(text).size(LabelSize::Small).color(Color::Muted);
        let heading = |text: &'static str| {
            div().pt_2().child(Label::new(text).size(LabelSize::Default).weight(FontWeight::BOLD).color(Color::Default))
        };
        let written = |selector: String| {
            div()
                .debug_selector(move || selector)
                .child(Label::new("Already written").color(Color::Success))
                .into_any_element()
        };
        // Area e Status si cambiano con un clic (giro fra le scelte)
        let choice = |id: String, text: String| {
            Button::new(SharedString::from(id), text).style(ButtonStyle::Outlined).size(ButtonSize::Medium)
        };

        let touched: Vec<AnyElement> = self
            .preview
            .touched
            .iter()
            .enumerate()
            .map(|(ix, t)| {
                let check = if t.written {
                    written(format!("superzed-done-written-touched-{ix}"))
                } else {
                    // un commento senza nota non ha niente da scrivere: spunta spenta e bloccata
                    Checkbox::new(SharedString::from(format!("superzed-done-touched-check-{ix}")), ToggleState::from(t.enabled))
                        .disabled(!t.writable())
                        .on_click(cx.listener(move |m, _: &ToggleState, _, cx| m.toggle_touched(ix, cx)))
                        .into_any_element()
                };
                // il numero apre la issue nel browser, se il repo è noto
                let number = match self.preview.issue_url(t.number) {
                    Some(url) => div()
                        .debug_selector(move || format!("superzed-done-link-{ix}"))
                        .child(ButtonLink::new(format!("#{}", t.number), url))
                        .into_any_element(),
                    None => Label::new(format!("#{}", t.number)).into_any_element(),
                };
                let action = if t.action == TouchAction::Close {
                    h_flex()
                        .debug_selector(move || format!("superzed-done-close-{ix}"))
                        .gap_1()
                        .child(Icon::new(IconName::Close).size(IconSize::Small).color(Color::Error))
                        .child(Label::new(touch_label(t.action)).color(Color::Error))
                        .into_any_element()
                } else {
                    Label::new(touch_label(t.action)).into_any_element()
                };
                v_flex()
                    .debug_selector(move || format!("superzed-done-touched-{ix}"))
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(check)
                            .child(number)
                            .child(action)
                            .when(t.action == TouchAction::Update && !t.written, |d| {
                                d.child(
                                    choice(format!("superzed-done-status-{ix}"), format!("Status: {}", t.status.name()))
                                        .on_click(cx.listener(move |m, _, _, cx| m.cycle_status(ix, cx))),
                                )
                            }),
                    )
                    .when(t.written && !t.note.is_empty(), |d| {
                        d.child(div().pl_6().child(Label::new(t.note.clone()).color(Color::Muted)))
                    })
                    .when(!t.written, |d| {
                        d.child(v_flex().pl_6().gap_0p5().child(field_label("Comment")).child(field(&self.notes[ix], false)))
                    })
                    .into_any_element()
            })
            .collect();
        let verify_numbers = self.preview.touched.iter().any(|t| !t.written);

        let has_areas = !self.preview.areas.is_empty();
        let new_issues: Vec<AnyElement> = self
            .preview
            .new_issues
            .iter()
            .enumerate()
            .map(|(ix, n)| {
                let check = if n.written {
                    written(format!("superzed-done-written-new-{ix}"))
                } else {
                    Checkbox::new(SharedString::from(format!("superzed-done-new-check-{ix}")), ToggleState::from(n.enabled))
                        .on_click(cx.listener(move |m, _: &ToggleState, _, cx| m.toggle_new(ix, cx)))
                        .into_any_element()
                };
                let area = n.area.clone().unwrap_or_else(|| "none".into());
                v_flex()
                    .debug_selector(move || format!("superzed-done-new-{ix}"))
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(check)
                            .child(Label::new(format!("{} · {}", n.kind.name(), n.kind.status().name())))
                            .when(has_areas && !n.written, |d| {
                                d.child(
                                    choice(format!("superzed-done-area-{ix}"), format!("Area: {area}"))
                                        .on_click(cx.listener(move |m, _, _, cx| m.cycle_area(ix, cx))),
                                )
                            })
                            .when_some(n.waiting_on.clone(), |d, w| {
                                d.child(Label::new(format!("Waiting on: {w}")).color(Color::Muted))
                            }),
                    )
                    .when(n.written, |d| d.child(div().pl_6().child(Label::new(n.title.clone()).color(Color::Muted))))
                    .when(!n.written, |d| {
                        d.child(field_label("Title"))
                            .child(field(&self.titles[ix], error_field == Some(InvalidField::Title(ix))))
                            .child(field_label("Description"))
                            .child(field(&self.bodies[ix], error_field == Some(InvalidField::Body(ix))))
                    })
                    .into_any_element()
            })
            .collect();

        let summary_target = self.preview.summary_target();
        let summary = self.preview.summary.as_ref().map(|s| {
            let check = if s.written {
                written("superzed-done-written-summary".into())
            } else {
                Checkbox::new("superzed-done-summary-check", ToggleState::from(s.enabled))
                    .on_click(cx.listener(|m, _: &ToggleState, _, cx| m.toggle_summary(cx)))
                    .into_any_element()
            };
            v_flex()
                .debug_selector(|| "superzed-done-summary".into())
                .gap_1()
                .child(heading("Summary"))
                .child(h_flex().gap_2().child(check).child(Label::new(match summary_target {
                    Some(n) => format!("Comment on #{n} (the first checked touched issue)"),
                    None => "No touched issue checked: the summary is not written".to_string(),
                })))
                .children(
                    self.summary.as_ref().filter(|_| !s.written).map(|e| field(e, error_field == Some(InvalidField::Summary))),
                )
        });

        let planned = self.planned();
        let (pending, closes) = (planned.len(), superzed_done::closes(&planned));
        v_flex()
            .key_context("SuperzedDoneReview")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::cancel))
            .elevation_3(cx)
            .w(rems(52.))
            .p_4()
            .gap_2()
            .child(Label::new(format!("Update GitHub: {}", self.chat_title)).size(LabelSize::Large))
            .child(
                Label::new("Check the items to write; nothing is written until you press “Write to GitHub”.")
                    .color(Color::Muted),
            )
            .when_some(self.warning.clone(), |d, w| {
                d.child(div().debug_selector(|| "superzed-done-warning".into()).child(Label::new(w).color(Color::Warning)))
            })
            .child(
                v_flex()
                    .id("superzed-done-review-list")
                    .max_h(rems(40.))
                    .overflow_y_scroll()
                    .gap_3()
                    .when(!touched.is_empty(), |d| {
                        d.child(heading("Touched issues"))
                            // i numeri vengono dal report di claude: si controllano prima di scrivere
                            .when(verify_numbers, |d| {
                                d.child(
                                    div().debug_selector(|| "superzed-done-verify".into()).child(
                                        Label::new("Check the issue numbers before writing")
                                            .color(Color::Warning)
                                            .size(LabelSize::Small),
                                    ),
                                )
                            })
                            .children(touched)
                    })
                    .when(!new_issues.is_empty(), |d| d.child(heading("New issues")).children(new_issues))
                    .children(summary),
            )
            .when_some(self.error.clone(), |d, e| {
                d.child(div().debug_selector(|| "superzed-done-error".into()).child(Label::new(e).color(Color::Error)))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_3()
                    .pt_3()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Button::new("superzed-done-cancel", "Cancel")
                            .size(ButtonSize::Large)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        div().debug_selector(|| "superzed-done-write".into()).child(
                            Button::new("superzed-done-write", write_label(pending, closes))
                                .start_icon(Icon::new(IconName::Github))
                                .style(ButtonStyle::Tinted(TintColor::Accent))
                                .size(ButtonSize::Large)
                                .disabled(pending == 0)
                                .on_click(cx.listener(|m, _, window, cx| m.write(window, cx))),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::workspace_with_repo;
    use gpui::{TestAppContext, VisualTestContext};
    use std::{cell::RefCell, rc::Rc};
    use superzed_done::test_support::{REPORT_JSON, SID};
    use superzed_done::{Action, ItemReport, RunResult, Status, parse_report};

    type Sink = Rc<RefCell<Option<Preview>>>;

    fn preview(previous: &RunResult) -> Preview {
        Preview::from_report(&parse_report(REPORT_JSON, SID).unwrap(), &["frontend".into(), "backend".into()], previous)
    }

    async fn open(cx: &mut TestAppContext, p: Preview) -> (Entity<Workspace>, Entity<DoneReviewModal>, Sink, &'static mut VisualTestContext) {
        let (workspace, _canvas, _fs, cx) = workspace_with_repo(cx, serde_json::json!({})).await;
        let got: Sink = Rc::default();
        let sink = got.clone();
        workspace.update_in(cx, |ws, window, cx| {
            open_done_review(ws, "Import utenti".into(), p, None, move |p, _, _| *sink.borrow_mut() = Some(p), window, cx)
        });
        cx.run_until_parked();
        let modal = workspace.read_with(cx, |ws, cx| ws.active_modal::<DoneReviewModal>(cx)).expect("anteprima aperta");
        (workspace, modal, got, cx)
    }

    fn is_open(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> bool {
        workspace.read_with(cx, |ws, cx| ws.active_modal::<DoneReviewModal>(cx).is_some())
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn every_entry_is_listed_and_only_checked_ones_are_written(cx: &mut TestAppContext) {
        let (workspace, modal, got, cx) = open(cx, preview(&RunResult::default())).await;
        redraw(cx);
        for selector in ["superzed-done-touched-0", "superzed-done-new-0", "superzed-done-write"] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
        assert_eq!(modal.read_with(cx, |m, cx| m.pending(cx)), 6);
        modal.update_in(cx, |m, window, cx| {
            m.toggle_new(1, cx);
            m.title_editor(0).unwrap().update(cx, |e, cx| e.set_text("Test su Safari", window, cx));
            m.cycle_status(1, cx); // In Progress → Blocked
            m.cycle_area(0, cx); // frontend → backend
        });
        assert_eq!(modal.read_with(cx, |m, cx| m.pending(cx)), 5);
        modal.update_in(cx, |m, window, cx| m.write(window, cx));
        cx.run_until_parked();
        let p = got.borrow_mut().take().expect("anteprima consegnata");
        let a = superzed_done::actions(&p);
        assert_eq!(a.iter().map(Action::key).collect::<Vec<_>>(), ["new:0", "new:2", "touched:12", "touched:8", "summary"]);
        match &a[0] {
            Action::Upsert(d) => assert_eq!((d.title.as_str(), d.area.as_deref()), ("Test su Safari", Some("backend"))),
            other => panic!("{other:?}"),
        }
        match &a[3] {
            Action::SetStatus { status, .. } => assert_eq!(*status, Status::Blocked),
            other => panic!("{other:?}"),
        }
        assert!(!is_open(&workspace, cx), "scritto: il modale si chiude");
    }

    #[gpui::test]
    async fn enter_and_escape_never_write(cx: &mut TestAppContext) {
        let (workspace, _modal, got, cx) = open(cx, preview(&RunResult::default())).await;
        cx.dispatch_action(menu::Confirm);
        cx.dispatch_action(menu::SecondaryConfirm);
        cx.run_until_parked();
        assert!(got.borrow().is_none() && is_open(&workspace, cx), "invio e cmd-invio non scrivono e non chiudono");
        cx.dispatch_action(menu::Cancel);
        cx.run_until_parked();
        assert!(got.borrow().is_none() && !is_open(&workspace, cx), "esc chiude senza scrivere");
    }

    #[gpui::test]
    async fn an_empty_title_blocks_the_write(cx: &mut TestAppContext) {
        let (workspace, modal, got, cx) = open(cx, preview(&RunResult::default())).await;
        modal.update_in(cx, |m, window, cx| {
            m.title_editor(0).unwrap().update(cx, |e, cx| e.set_text("  ", window, cx));
            m.write(window, cx);
        });
        assert_eq!(modal.read_with(cx, |m, _| m.error().map(str::to_string)), Some("New issue without a title (the first)".into()));
        assert_eq!(modal.read_with(cx, |m, _| m.error_field()), Some(InvalidField::Title(0)));
        assert!(got.borrow().is_none() && is_open(&workspace, cx));
        redraw(cx);
        assert!(cx.debug_bounds("superzed-done-error").is_some(), "errore sopra il footer");
    }

    #[gpui::test]
    async fn written_entries_are_shown_but_never_rewritten(cx: &mut TestAppContext) {
        let mut previous = RunResult::default();
        previous.record(vec![
            ItemReport { key: "new:0".into(), result: Ok("https://github.com/acme/acme_app/issues/42".into()), issue: None },
            ItemReport { key: "touched:12".into(), result: Ok("#12 chiusa".into()), issue: None },
        ]);
        let (_workspace, modal, _got, cx) = open(cx, preview(&previous)).await;
        modal.update(cx, |m, cx| {
            m.toggle_new(0, cx);
            m.toggle_touched(0, cx);
        });
        modal.read_with(cx, |m, cx| {
            assert!(m.preview().new_issues[0].written && !m.preview().new_issues[0].enabled, "la spunta non la riaccende");
            assert!(!m.preview().touched[0].enabled);
            assert_eq!(m.pending(cx), 4);
        });
        redraw(cx);
        assert!(cx.debug_bounds("superzed-done-written-new-0").is_some(), "«Già scritto» al posto della spunta");
    }

    #[gpui::test]
    async fn nothing_left_to_write_says_so(cx: &mut TestAppContext) {
        let (workspace, modal, got, cx) = open(cx, preview(&RunResult::default())).await;
        modal.update_in(cx, |m, window, cx| {
            (0..3).for_each(|ix| m.toggle_new(ix, cx));
            (0..2).for_each(|ix| m.toggle_touched(ix, cx));
            m.toggle_summary(cx);
            m.write(window, cx);
        });
        assert_eq!(modal.read_with(cx, |m, _| m.error().map(str::to_string)), Some("Nothing to write".into()));
        assert!(got.borrow().is_none() && is_open(&workspace, cx));
    }

    // Revisione finale 6: un commento senza nota non si spunta; un solo avviso di
    // controllare i numeri in cima alla sezione; il riepilogo segue la prima issue spuntata.
    #[gpui::test]
    async fn touched_rows_guard_their_numbers_and_empty_comments(cx: &mut TestAppContext) {
        let mut report = parse_report(REPORT_JSON, SID).unwrap();
        report.issues_touched.push(superzed_done::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let p = Preview::from_report(&report, &["frontend".into()], &RunResult::default());
        let (_workspace, modal, got, cx) = open(cx, p).await;
        modal.update(cx, |m, cx| m.toggle_touched(2, cx));
        assert!(!modal.read_with(cx, |m, _| m.preview().touched[2].enabled), "senza nota: resta spento");
        redraw(cx);
        assert!(cx.debug_bounds("superzed-done-verify").is_some(), "un avviso in cima alla sezione");
        for ix in 0..3 {
            assert!(cx.debug_bounds(leak(format!("superzed-done-verify-{ix}"))).is_none(), "niente avviso sulla riga {ix}");
        }
        modal.update_in(cx, |m, window, cx| {
            m.toggle_touched(0, cx);
            m.write(window, cx);
        });
        cx.run_until_parked();
        let p = got.borrow_mut().take().expect("scritto");
        assert!(
            matches!(superzed_done::actions(&p).last(), Some(Action::Comment { key, number: 8, .. }) if key == superzed_done::SUMMARY_KEY),
            "riepilogo sulla prima spuntata"
        );
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn the_write_button_counts_the_closes() {
        assert_eq!(write_label(6, 1), "Write to GitHub (6), including 1 close");
        assert_eq!(write_label(5, 2), "Write to GitHub (5), including 2 closes");
        assert_eq!(write_label(3, 0), "Write to GitHub (3)");
    }

    #[gpui::test]
    async fn the_edited_comment_is_written(cx: &mut TestAppContext) {
        let mut report = parse_report(REPORT_JSON, SID).unwrap();
        report.issues_touched.push(superzed_done::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let p = Preview::from_report(&report, &["frontend".into()], &RunResult::default());
        let (_workspace, modal, got, cx) = open(cx, p).await;
        assert_eq!(modal.read_with(cx, |m, cx| m.closes(cx)), 1);
        modal.update_in(cx, |m, window, cx| {
            m.note_editor(0).unwrap().update(cx, |e, cx| e.set_text("Import completato e verificato", window, cx));
            m.note_editor(2).unwrap().update(cx, |e, cx| e.set_text("Aggiunto il test", window, cx));
        });
        cx.run_until_parked();
        modal.update(cx, |m, cx| m.toggle_touched(2, cx));
        assert!(modal.read_with(cx, |m, _| m.preview().touched[2].enabled), "con una nota scritta si spunta");
        modal.update_in(cx, |m, window, cx| m.write(window, cx));
        cx.run_until_parked();
        let a = superzed_done::actions(&got.borrow_mut().take().expect("scritto"));
        assert!(a.contains(&Action::Close { key: "touched:12".into(), number: 12, comment: Some("Import completato e verificato".into()) }));
        assert!(a.contains(&Action::Comment { key: "touched:30".into(), number: 30, body: "Aggiunto il test".into() }));
    }

    #[gpui::test]
    async fn issue_numbers_are_links_when_the_repo_is_known(cx: &mut TestAppContext) {
        let mut p = preview(&RunResult::default());
        p.repo = Some("acme/acme_app".into());
        let (_workspace, _modal, _got, cx) = open(cx, p).await;
        redraw(cx);
        assert!(cx.debug_bounds("superzed-done-link-0").is_some() && cx.debug_bounds("superzed-done-link-1").is_some());
        assert!(cx.debug_bounds("superzed-done-close-0").is_some(), "chiusura evidenziata");
        assert!(cx.debug_bounds("superzed-done-close-1").is_none(), "aggiorna: niente evidenza");
    }

    #[gpui::test]
    async fn no_repo_no_links(cx: &mut TestAppContext) {
        let (_workspace, _modal, _got, cx) = open(cx, preview(&RunResult::default())).await;
        redraw(cx);
        assert!(cx.debug_bounds("superzed-done-link-0").is_none());
        assert!(cx.debug_bounds("superzed-done-touched-0").is_some());
    }

    // Revisione finale 6: aprire di nuovo la stessa anteprima la lascia aperta.
    #[gpui::test]
    async fn opening_the_same_review_again_keeps_it(cx: &mut TestAppContext) {
        let (workspace, modal, _got, cx) = open(cx, preview(&RunResult::default())).await;
        workspace.update_in(cx, |ws, window, cx| {
            open_done_review(ws, "Import utenti".into(), preview(&RunResult::default()), None, |_, _, _| {}, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(workspace.read_with(cx, |ws, cx| ws.active_modal::<DoneReviewModal>(cx)), Some(modal));
    }
}
