//! Nodo Automazioni: lo sfondo delle chat claude delle automazioni. Le chat sono nodi del
//! canvas disposti sopra di lui; qui si disegnano l'intestazione (con il "+") e, sopra
//! ogni chat, la sua fascia di controllo.
use crate::canvas_view::CanvasView;
use crate::canvas_view::TITLE_BAR_REMS;
use crate::canvas_view::AutomationRow;
use crate::kanban_layout::GAP;
use crate::model::DockKind;
use crate::node::NodeContent;
use gpui::{
    AnyView, AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, MouseButton, Point, Render, Subscription, WeakEntity, Window, px,
};
use ui::{Button, IconButton, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*, utils::WithRemSize};

/// Testo e colore dello stato di un'automazione nella fascia.
pub fn run_status(row: &AutomationRow) -> (String, Color) {
    if row.waiting_input {
        return ("Waiting for input".to_string(), Color::Warning);
    }
    if row.running {
        return ("Running".to_string(), Color::Accent);
    }
    let Some(outcome) = &row.last_outcome else {
        return ("Never run".to_string(), Color::Muted);
    };
    let text = match &row.last_run {
        Some(when) => format!("Last: {outcome}, {when}"),
        None => format!("Last: {outcome}"),
    };
    (text, if row.failed { Color::Error } else { Color::Muted })
}

/// Perche' «Esegui ora» e' disabilitato (il tooltip), se lo e'.
pub fn run_now_blocked(row: &AutomationRow) -> Option<&'static str> {
    if row.waiting_input {
        Some("Waiting for input")
    } else if row.running {
        Some("Already running")
    } else {
        None
    }
}

pub struct AutomationsView {
    canvas: WeakEntity<CanvasView>,
    focus: FocusHandle,
    zoom: f32,
    _subscriptions: Vec<Subscription>,
}

impl AutomationsView {
    pub fn new(canvas: WeakEntity<CanvasView>, cx: &mut Context<Self>) -> Self {
        let subscriptions = canvas
            .upgrade()
            .map(|c| vec![cx.observe(&c, |_, _, cx| cx.notify())])
            .unwrap_or_default();
        Self { canvas, focus: cx.focus_handle(), zoom: 1.0, _subscriptions: subscriptions }
    }
}

impl Focusable for AutomationsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AutomationsView {
    /// Fascia di controllo sopra una chat: nome, frequenza, prossima esecuzione, esito,
    /// Esegui ora, Pausa/Riprendi e Modifica. Posizione e dimensioni in px del contenuto del nodo.
    fn band(&self, row: AutomationRow, origin: Point<f32>, title_bar: f32, cx: &App) -> AnyElement {
        let zoom = self.zoom;
        let node = row.node;
        let colors = cx.theme().colors();
        let (status, status_color) = run_status(&row);
        let (next, next_color) = match &row.next {
            Ok(s) => (s.clone(), Color::Default),
            Err(e) => (e.clone(), Color::Error),
        };
        let run = {
            let canvas = self.canvas.clone();
            Button::new(("superzed-automation-run", node.0 as usize), "Run now")
                .disabled(run_now_blocked(&row).is_some())
                .when_some(run_now_blocked(&row), |b, why| b.tooltip(Tooltip::text(why)))
                .on_click(move |_, _, cx| {
                    canvas.update(cx, |c, cx| c.run_automation_now(node, cx)).ok();
                })
        };
        let pause = {
            let canvas = self.canvas.clone();
            Button::new(("superzed-automation-pause", node.0 as usize), if row.paused { "Resume" } else { "Pause" })
                .on_click(move |_, _, cx| {
                    canvas.update(cx, |c, cx| c.toggle_automation_pause(node, cx)).ok();
                })
        };
        let edit = {
            let canvas = self.canvas.clone();
            Button::new(("superzed-automation-edit", node.0 as usize), "Edit").on_click(move |_, window, cx| {
                canvas.update(cx, |c, cx| c.edit_automation(node, window, cx)).ok();
            })
        };
        v_flex()
            .debug_selector(move || format!("superzed-automation-band-{}", node.0))
            .absolute()
            .left(px((row.band.x - origin.x) * zoom))
            .top(px((row.band.y - origin.y) * zoom - title_bar))
            .w(px(row.width * zoom))
            .h(px(row.height * zoom))
            .overflow_hidden()
            .px_2()
            .py_1()
            .gap_1()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            // i click sui pulsanti non selezionano ne' trascinano la sezione
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .gap_2()
                    .min_w(px(0.))
                    .child(Label::new(row.name).size(LabelSize::Large).weight(FontWeight::BOLD).truncate())
                    .child(Label::new(row.schedule).color(Color::Muted))
                    .child(div().flex_1())
                    .child(div().debug_selector(move || format!("superzed-automation-run-{}", node.0)).child(run))
                    .child(div().debug_selector(move || format!("superzed-automation-pause-{}", node.0)).child(pause))
                    .child(div().debug_selector(move || format!("superzed-automation-edit-{}", node.0)).child(edit)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .min_w(px(0.))
                    .child(Label::new(next).color(next_color).truncate())
                    .child(Label::new(status).color(status_color).truncate()),
            )
            .into_any_element()
    }
}

impl Render for AutomationsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let canvas = self.canvas.upgrade();
        let (n, rows, origin, header_height) = canvas
            .map(|c| {
                let c = c.read(cx);
                let origin = c
                    .container_node(DockKind::Automations)
                    .and_then(|a| c.model().frame(a))
                    .map(|f| f.origin)
                    .unwrap_or_default();
                (c.dock_members(DockKind::Automations).len(), c.automation_rows(cx), origin, c.automations_metrics(cx).header)
            })
            .unwrap_or_default();
        let colors = cx.theme().colors();
        // come l'intestazione del kanban: la fascia fra la barra del titolo del nodo e la
        // prima fascia, che parte a `header + GAP` dall'origine (in rem, come il layout)
        let title_bar = f32::from(window.rem_size()) * TITLE_BAR_REMS * self.zoom;
        let header = px(((header_height + GAP) * self.zoom - title_bar).max(0.));
        let plus = {
            let canvas = self.canvas.clone();
            // il mouse-down non arriva all'intestazione; un doppio click apre un form solo
            div()
                .debug_selector(|| "superzed-automations-new".into())
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    IconButton::new("superzed-automations-new", IconName::Plus)
                        .icon_size(IconSize::Small)
                        .tooltip(ui::Tooltip::text("New automation"))
                        .on_click(move |e, window, cx| {
                            if e.click_count() > 1 {
                                return;
                            }
                            canvas.update(cx, |c, cx| c.new_automation(window, cx)).ok();
                        }),
                )
        };
        let bands: Vec<AnyElement> = rows.into_iter().map(|row| self.band(row, origin, title_bar, cx)).collect();
        WithRemSize::new(window.rem_size() * self.zoom).size_full().child(
            v_flex()
                .key_context("Automations")
                .track_focus(&self.focus)
                .relative()
                .size_full()
                .bg(colors.panel_background)
                .child(
                    h_flex()
                        .debug_selector(|| "superzed-automations-header".into())
                        .flex_none()
                        .h(header)
                        .overflow_hidden()
                        .px_2()
                        .gap_1()
                        .child(Label::new(format!("Automations ({n})")).size(LabelSize::Large).weight(FontWeight::BOLD))
                        .child(plus),
                )
                .children(bands),
        )
    }
}

pub struct AutomationsNode {
    view: Entity<AutomationsView>,
}

impl AutomationsNode {
    pub fn new(canvas: WeakEntity<CanvasView>, cx: &mut App) -> Self {
        Self { view: cx.new(|cx| AutomationsView::new(canvas, cx)) }
    }
}

impl NodeContent for AutomationsNode {
    fn view(&self) -> AnyView {
        self.view.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }

    fn set_font_zoom(&self, zoom: f32, cx: &mut App) {
        self.view.update(cx, |v, cx| {
            v.zoom = zoom;
            cx.notify();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeId;

    fn row() -> AutomationRow {
        AutomationRow {
            node: NodeId(1),
            name: "Log".into(),
            schedule: "4h".into(),
            next: Ok("next: today 13:00".into()),
            last_outcome: None,
            failed: false,
            last_run: None,
            paused: false,
            running: false,
            waiting_input: false,
            band: gpui::point(0., 0.),
            width: 800.,
            height: 96.,
        }
    }

    #[test]
    fn status_text_and_color() {
        assert_eq!(run_status(&row()), ("Never run".to_string(), Color::Muted));
        let ok = AutomationRow { last_outcome: Some("succeeded".into()), last_run: Some("today 09:00".into()), ..row() };
        assert_eq!(run_status(&ok), ("Last: succeeded, today 09:00".to_string(), Color::Muted));
        let failed = AutomationRow {
            last_outcome: Some("failed (claude not responding)".into()),
            failed: true,
            last_run: Some("yesterday 18:30".into()),
            ..row()
        };
        assert_eq!(run_status(&failed), ("Last: failed (claude not responding), yesterday 18:30".to_string(), Color::Error));
        let no_time = AutomationRow { last_outcome: Some("succeeded".into()), ..row() };
        assert_eq!(run_status(&no_time).0, "Last: succeeded");
        let running = AutomationRow { running: true, ..ok.clone() };
        assert_eq!(run_status(&running), ("Running".to_string(), Color::Accent));
        let waiting = AutomationRow { running: true, waiting_input: true, ..ok };
        assert_eq!(run_status(&waiting), ("Waiting for input".to_string(), Color::Warning));
    }

    #[test]
    fn run_now_is_disabled_while_running_or_waiting() {
        assert_eq!(run_now_blocked(&row()), None);
        assert_eq!(run_now_blocked(&AutomationRow { running: true, ..row() }), Some("Already running"));
        assert_eq!(run_now_blocked(&AutomationRow { waiting_input: true, ..row() }), Some("Waiting for input"));
        assert_eq!(run_now_blocked(&AutomationRow { running: true, waiting_input: true, ..row() }), Some("Waiting for input"));
    }
}
