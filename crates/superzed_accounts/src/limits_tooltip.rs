//! Tooltip dei limiti della status bar (Task 23): per ogni account, nello stesso ordine
//! della sezione «Limits» (prima il piu' stretto), il nome e un blocco per finestra
//! (5h, Week, limiti per modello) con «NN% left», la barra a tutta larghezza e
//! «Resets in ...». Larghezza fissa, niente a capo; colori solo dal tema.
use crate::AccountStore;
use crate::meter::{self, LimitsRow, WindowMeter, meter};
use gpui::{AnyElement, AnyView, App, Context, FontWeight, IntoElement, Render, Window};
use ui::{Divider, prelude::*, tooltip_container};

/// Larghezza fissa del tooltip.
const WIDTH: f32 = 18.;
/// Altezza della barra: 6px con il font di 16px.
const BAR_HEIGHT: f32 = 0.375;

pub struct LimitsTooltip {
    rows: Vec<LimitsRow>,
}

impl LimitsTooltip {
    pub fn new(rows: Vec<LimitsRow>) -> Self {
        Self { rows }
    }
}

/// Costruttore per `.tooltip(...)`: le righe si rileggono dallo store al momento
/// dell'apertura.
pub fn limits_tooltip(_: &mut Window, cx: &mut App) -> AnyView {
    let rows = AccountStore::global(cx).map(|s| meter::limits_rows(s.read(cx))).unwrap_or_default();
    cx.new(|_| LimitsTooltip::new(rows)).into()
}

fn window_block(id: &str, key: &str, name: &str, window: &WindowMeter, cx: &App) -> AnyElement {
    let sel = |what: &str| format!("superzed-quota-tooltip-{what}-{id}-{key}");
    let (pct_sel, bar_sel, reset_sel) = (sel("pct"), sel("bar"), sel("reset"));
    let (_, color) = meter::pct_label(window.remaining);
    v_flex()
        .w_full()
        .gap_0p5()
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap_2()
                .child(Label::new(name.to_string()).color(Color::Muted).single_line())
                .child(
                    div().flex_none().debug_selector(move || pct_sel).child(
                        Label::new(meter::left_label(window.remaining)).weight(FontWeight::BOLD).color(color).single_line(),
                    ),
                ),
        )
        .child(meter(window.remaining, rems(WIDTH), rems(BAR_HEIGHT), cx).w_full().debug_selector(move || bar_sel))
        .children(window.reset.clone().map(|reset| {
            div()
                .debug_selector(move || reset_sel)
                .child(Label::new(reset).size(LabelSize::Small).color(Color::Muted).single_line())
        }))
        .into_any_element()
}

fn account_block(row: &LimitsRow, cx: &App) -> AnyElement {
    let id = row.id.clone();
    let selector = format!("superzed-quota-tooltip-{id}");
    let email = row.email.clone().filter(|e| *e != row.label);
    let header = h_flex()
        .w_full()
        .gap_1()
        .debug_selector(move || selector)
        .child(Label::new(row.label.clone()).weight(FontWeight::BOLD).single_line())
        .children(email.map(|e| div().min_w_0().child(Label::new(e).color(Color::Muted).single_line().truncate())));
    let body = if row.login_needed {
        let selector = format!("superzed-quota-tooltip-login-needed-{id}");
        div().debug_selector(move || selector).child(Label::new("Login needed").color(Color::Warning)).into_any_element()
    } else if row.has_data() {
        v_flex()
            .w_full()
            .gap_1p5()
            .children(row.windows().into_iter().map(|(key, name, window)| window_block(&id, &key, &name, window, cx)))
            .into_any_element()
    } else {
        let selector = format!("superzed-quota-tooltip-no-data-{id}");
        div().debug_selector(move || selector).child(Label::new("No data").color(Color::Muted)).into_any_element()
    };
    v_flex().w_full().gap_1().child(header).child(body).into_any_element()
}

impl Render for LimitsTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let blocks: Vec<AnyElement> = self.rows.iter().map(|row| account_block(row, cx)).collect();
        tooltip_container(cx, |el, _| {
            let mut children = Vec::new();
            for (i, block) in blocks.into_iter().enumerate() {
                if i > 0 {
                    children.push(Divider::horizontal().into_any_element());
                }
                children.push(block);
            }
            el.debug_selector(|| "superzed-quota-tooltip".into()).w(rems(WIDTH)).py_1p5().gap_2().children(children)
        })
    }
}
