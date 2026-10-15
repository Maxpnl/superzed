//! `superzed::NewSourcePanel` (palette dei comandi): sceglie la connessione e apre il
//! pannello della fonte sul canvas; prima le fonti usate dal progetto.
use gpui::{App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Window, actions};
use superzed_sources::ConnectionStore;
use ui::prelude::*;
use workspace::{ModalView, Toast, Workspace, notifications::NotificationId};

actions!(
    superzed,
    [
        /// Apre sul canvas il pannello di una fonte (GitHub, HyperDX, Zabbix), scegliendo la connessione.
        NewSourcePanel,
    ]
);

#[derive(Clone, Debug, PartialEq)]
pub struct SourceChoice {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub in_project: bool,
}

pub fn source_choices(store: &ConnectionStore, project_sources: &[String]) -> Vec<SourceChoice> {
    let mut out: Vec<SourceChoice> = store
        .connections()
        .iter()
        .filter(|c| c.kind().source_kind().is_some())
        .map(|c| SourceChoice { id: c.id.clone(), name: c.name.clone(), kind: c.kind().label(), in_project: project_sources.contains(&c.id) })
        .collect();
    let rank = |c: &SourceChoice| project_sources.iter().position(|p| *p == c.id).unwrap_or(usize::MAX);
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

pub struct SourcePickerModal {
    choices: Vec<SourceChoice>,
    selected: usize,
    focus_handle: FocusHandle,
    on_confirm: Option<Box<dyn FnOnce(String, &mut Window, &mut App)>>,
}

impl SourcePickerModal {
    pub fn selected_id(&self) -> &str {
        &self.choices[self.selected].id
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = (self.selected + 1).min(self.choices.len() - 1);
        cx.notify();
    }

    fn select_previous(&mut self, _: &menu::SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.selected_id().to_string();
        let on_confirm = self.on_confirm.take();
        cx.emit(DismissEvent);
        if let Some(f) = on_confirm {
            f(id, window, cx);
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

pub fn open_source_picker(
    workspace: &mut Workspace,
    choices: Vec<SourceChoice>,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if choices.is_empty() {
        return;
    }
    workspace.toggle_modal(window, cx, move |_, cx| SourcePickerModal {
        choices,
        selected: 0,
        focus_handle: cx.focus_handle(),
        on_confirm: Some(Box::new(on_confirm)),
    });
}

/// L'azione: picker delle connessioni e pannello sul canvas del workspace.
pub fn new_source_panel_from_picker(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(canvas) = crate::canvas_for(ws) else {
        return;
    };
    let choices = match ConnectionStore::global(cx) {
        Some(store) => source_choices(store.read(cx), canvas.read(cx).project_sources()),
        None => Vec::new(),
    };
    if choices.is_empty() {
        ws.show_toast(
            Toast::new(NotificationId::named("superzed-no-sources".into()), "No sources configured: add one in the Sources view (cmd-6)"),
            cx,
        );
        return;
    }
    open_source_picker(
        ws,
        choices,
        move |id, window, cx| {
            // a fine update: il canvas rilegge il workspace
            window.defer(cx, move |window, cx| {
                canvas.update(cx, |c, cx| {
                    c.new_source_panel(&id, window, cx);
                });
            });
        },
        window,
        cx,
    );
}

impl EventEmitter<DismissEvent> for SourcePickerModal {}
impl ModalView for SourcePickerModal {}

impl Focusable for SourcePickerModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SourcePickerModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.choices.clone().into_iter().enumerate().map(|(i, choice)| {
            let selector = format!("superzed-source-pick-{}", choice.id);
            h_flex()
                .id(("superzed-source-pick", i))
                .debug_selector(move || selector)
                .px_2()
                .py_1()
                .gap_3()
                .rounded_sm()
                .cursor_pointer()
                .when(i == self.selected, |d| d.bg(cx.theme().colors().element_selected))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected = i;
                    this.confirm(&menu::Confirm, window, cx);
                }))
                .child(Label::new(choice.name.clone()))
                .child(Label::new(choice.kind).size(LabelSize::Small).color(Color::Muted))
                .when(choice.in_project, |d| d.child(Label::new("project").size(LabelSize::Small).color(Color::Accent)))
        });
        v_flex()
            .key_context("SuperzedSourcePicker")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .elevation_3(cx)
            .w(rems(32.))
            .p_2()
            .gap_1()
            .child(Label::new("New panel: choose a source").size(LabelSize::Small).color(Color::Muted))
            .children(rows)
    }
}
