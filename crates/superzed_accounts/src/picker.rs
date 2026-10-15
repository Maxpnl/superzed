//! Picker `cmd-shift-n`: una riga per account con quote rimanenti, carico, stato e
//! punteggio; il migliore preselezionato; Invio apre la chat su quello scelto.
use crate::{AccountStore, quota, scheduler};
use collections::HashMap;
use std::path::Path;
use gpui::{App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Window};
use superzed_sessions::AccountLoad;
use ui::prelude::*;
use workspace::{ModalView, Workspace};

#[derive(Clone, Debug, PartialEq)]
pub struct PickerRow {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    /// Percentuale rimanente.
    pub five_hour: Option<u8>,
    pub weekly: Option<u8>,
    pub load: u32,
    pub status: &'static str,
    /// `None`: escluso dallo scheduler.
    pub score: Option<i32>,
    /// Non si sceglie da solo in questa cartella (`exclude_from`), ma a mano si'.
    pub excluded_here: bool,
}

/// Righe del picker (sistema primo, poi il registro) e indice del migliore per lo scheduler.
pub fn picker_rows(store: &AccountStore, loads: &HashMap<String, AccountLoad>, project: Option<&Path>) -> (Vec<PickerRow>, usize) {
    let now = store.now();
    let candidates = store.candidates(loads, project);
    let scored = scheduler::score_all(store.strategy(), &candidates, now);
    let best = scheduler::pick(store.strategy(), &candidates, now).id;
    let rows: Vec<PickerRow> = store
        .accounts()
        .iter()
        .zip(scored)
        .map(|(a, s)| {
            let q = store.quota(&a.id);
            PickerRow {
                id: a.id.clone(),
                label: a.label.clone(),
                email: a.email.clone(),
                five_hour: quota::remaining_pct(q.and_then(|q| q.session.as_ref()), now),
                weekly: quota::remaining_pct(q.and_then(|q| q.weekly.as_ref()), now),
                load: s.load,
                status: store.status(&a.id).short(),
                score: (!s.excluded).then(|| s.score.round() as i32),
                excluded_here: s.excluded_here,
            }
        })
        .collect();
    let selected = rows.iter().position(|r| r.id == best).unwrap_or(0);
    (rows, selected)
}

/// Riga successiva (`step` 1) o precedente (-1) fra quelle non escluse; nessuna: si resta.
pub fn step_selection(rows: &[PickerRow], from: usize, step: isize) -> usize {
    let mut ix = from as isize + step;
    while ix >= 0 && (ix as usize) < rows.len() {
        if rows[ix as usize].score.is_some() {
            return ix as usize;
        }
        ix += step;
    }
    from
}

pub struct AccountPickerModal {
    rows: Vec<PickerRow>,
    selected: usize,
    focus_handle: FocusHandle,
    on_confirm: Option<Box<dyn FnOnce(String, &mut Window, &mut App)>>,
}

impl AccountPickerModal {
    pub fn selected_id(&self) -> &str {
        &self.rows[self.selected].id
    }

    fn select_next(&mut self, _: &menu::SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = step_selection(&self.rows, self.selected, 1);
        cx.notify();
    }

    fn select_previous(&mut self, _: &menu::SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = step_selection(&self.rows, self.selected, -1);
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

/// Apre il picker (nessuna riga: niente). `on_confirm` riceve l'id scelto.
pub fn open_account_picker(
    workspace: &mut Workspace,
    rows: Vec<PickerRow>,
    selected: usize,
    on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if rows.is_empty() {
        return;
    }
    workspace.toggle_modal(window, cx, move |_, cx| AccountPickerModal {
        selected: selected.min(rows.len() - 1),
        rows,
        focus_handle: cx.focus_handle(),
        on_confirm: Some(Box::new(on_confirm)),
    });
}

impl EventEmitter<DismissEvent> for AccountPickerModal {}
impl ModalView for AccountPickerModal {}

impl Focusable for AccountPickerModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AccountPickerModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pct = |p: Option<u8>| p.map(|p| format!("{p}%")).unwrap_or_else(|| "–".into());
        // colonne a larghezza fissa: i numeri restano allineati tra le righe
        let cell = |width: f32, label: Label| div().w(rems(width)).flex_none().child(label);
        let rows = self.rows.clone().into_iter().enumerate().map(|(i, row)| {
            let selector = format!("superzed-account-row-{}", row.id);
            let excluded = row.score.is_none();
            let score = match row.score {
                Some(s) if row.excluded_here => format!("not here · {s}"),
                Some(s) => format!("score {s}"),
                None => "excluded".into(),
            };
            // escluso dallo scheduler: tutta la riga attenuata; altrimenti le quote con
            // le soglie comuni (pct_color)
            let quota_color = |p: Option<u8>| match p {
                _ if excluded => Color::Disabled,
                Some(p) => superzed_sessions::ui::pct_color(p),
                None => Color::Muted,
            };
            let muted = if excluded { Color::Disabled } else { Color::Muted };
            h_flex()
                .id(("superzed-account-row", i))
                .debug_selector(move || selector)
                .px_3()
                .py_2()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .when(i == self.selected, |d| d.bg(cx.theme().colors().element_selected))
                .hover(|d| d.bg(cx.theme().colors().element_hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.selected = i;
                    this.confirm(&menu::Confirm, window, cx);
                }))
                .child(
                    v_flex()
                        .w(rems(16.))
                        .flex_none()
                        .overflow_hidden()
                        .child(
                            Label::new(row.label.clone())
                                .size(LabelSize::Large)
                                .color(if excluded { Color::Disabled } else { Color::Default })
                                .truncate(),
                        )
                        .children(row.email.clone().map(|e| Label::new(e).size(LabelSize::Small).color(muted).truncate())),
                )
                .child(cell(6., Label::new(format!("5h {}", pct(row.five_hour))).color(quota_color(row.five_hour))))
                .child(cell(7., Label::new(format!("week {}", pct(row.weekly))).color(quota_color(row.weekly))))
                .child(cell(6., Label::new(format!("load {}", row.load)).color(muted)))
                .child(cell(8., Label::new(row.status).color(muted)))
                .child(cell(8., Label::new(score).color(if excluded { Color::Disabled } else { Color::Default })))
        });
        v_flex()
            .key_context("SuperzedAccountPicker")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .elevation_3(cx)
            .max_w(rems(60.))
            .p_3()
            .gap_1()
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .child(Label::new("New chat: choose an account").size(LabelSize::Large)),
            )
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn row(id: &str, score: Option<i32>) -> PickerRow {
        PickerRow { id: id.into(), label: id.into(), email: None, five_hour: None, weekly: None, load: 0, status: "ok", score, excluded_here: false }
    }

    #[test]
    fn up_and_down_skip_excluded_rows() {
        let rows = [row("a", Some(1)), row("b", None), row("c", Some(2)), row("d", None)];
        assert_eq!(step_selection(&rows, 0, 1), 2, "b escluso: saltato");
        assert_eq!(step_selection(&rows, 2, 1), 2, "d escluso e ultimo: si resta");
        assert_eq!(step_selection(&rows, 2, -1), 0);
        assert_eq!(step_selection(&rows, 0, -1), 0);
    }

    #[gpui::test]
    fn rows_show_remaining_load_status_and_the_best_is_selected(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _t, _f) = cx.update(|cx| crate::test_support::fixture_store(cx, dir.path(), &[(90., 20.), (10., 30.)]));
        cx.run_until_parked();
        let mut loads = collections::HashMap::default();
        loads.insert("a2".to_string(), superzed_sessions::AccountLoad { working: 0, recent: 2 });
        let (rows, selected) = store.read_with(cx, |s, _| picker_rows(s, &loads, None));
        assert_eq!(rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["system", "a1", "a2"]);
        assert_eq!(rows[0].score, None, "sistema senza token: escluso");
        assert_eq!(rows[0].status, "not logged in");
        assert_eq!(rows[1], PickerRow {
            id: "a1".into(), label: "a1".into(), email: Some("a1@x.it".into()),
            five_hour: Some(10), weekly: Some(80), load: 0, status: "ok", score: Some(10), excluded_here: false,
        });
        // settimana che si resetta nel 2099: nessuna pressione, conta il rimanente 5h
        assert_eq!((rows[2].load, rows[2].score), (2, Some(90 - 20)));
        assert_eq!(selected, 2);
    }

    #[gpui::test]
    fn an_account_excluded_from_the_folder_stays_selectable_but_is_not_preselected(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let acme = dir.path().join("Acme");
        let (store, _t, _f) = cx.update(|cx| {
            crate::test_support::fixture_store_with(cx, dir.path(), &[(10., 20.), (50., 30.)], |registry| {
                registry.accounts[0].exclude_from = vec![acme.clone()];
            })
        });
        cx.run_until_parked();
        let loads = collections::HashMap::default();
        let (rows, selected) = store.read_with(cx, |s, _| picker_rows(s, &loads, Some(acme.as_path())));
        assert_eq!((rows[1].score, rows[1].excluded_here), (Some(90), true));
        assert_eq!((rows[2].score, rows[2].excluded_here), (Some(50), false));
        assert_eq!(selected, 2, "a1 vale di piu' ma qui non si sceglie da solo");
        assert_eq!(step_selection(&rows, 2, -1), 1, "a mano resta scelgibile");
    }
}
