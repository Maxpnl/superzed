//! Pezzi condivisi dai pannelli della sidebar (Sessions, Sources): intestazione con
//! titolo, totale e filtro, intestazione di gruppo con disclosure, pill e pallino di stato.
use editor::Editor;
use gpui::{AnyElement, App, ClickEvent, Div, ElementId, Entity, Focusable, FontWeight, Hsla, IntoElement, Stateful, Window, div};
use std::rc::Rc;
use ui::{Disclosure, Label, LabelSize, prelude::*};

/// La query (spazi ai lati ignorati, senza distinzione di maiuscole) compare in almeno
/// uno dei campi; una query vuota fa passare tutto.
pub fn matches_query<'a>(fields: impl IntoIterator<Item = &'a str>, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || fields.into_iter().any(|field| field.to_lowercase().contains(&query))
}

/// Pill a destra di una riga (account, contesto, stato).
pub fn pill(bg: Hsla) -> Div {
    div().flex_none().px_1().rounded_sm().bg(bg)
}

/// Sfondo di una pill il cui testo ha il colore di stato `color`.
pub fn pill_background(color: Color, cx: &App) -> Hsla {
    let status = cx.theme().status();
    match color {
        Color::Error => status.error_background,
        Color::Warning => status.warning_background,
        Color::Success => status.success_background,
        _ => cx.theme().colors().element_background,
    }
}

/// Lato del pallino di stato (anche lo slot fisso delle righe senza stato).
pub fn status_dot_size() -> Rems {
    rems(0.5)
}

/// Pallino di stato di una riga.
pub fn status_dot(color: Color, cx: &App) -> Div {
    div().flex_none().size(status_dot_size()).rounded_full().bg(color.color(cx))
}

/// Intestazione del pannello: «Title» grande in grassetto col totale attenuato (e a
/// destra `trailing`, se c'e'), sotto il campo filtro. Selettori `{prefix}-header` e
/// `{prefix}-filter`.
pub fn panel_header(
    prefix: &'static str,
    title: &'static str,
    total: usize,
    filter: &Entity<Editor>,
    trailing: Option<AnyElement>,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let colors = cx.theme().colors();
    let border = if filter.focus_handle(cx).is_focused(window) { colors.border_focused } else { colors.border };
    v_flex()
        .flex_none()
        .gap_1p5()
        .px_1()
        .pb_1p5()
        .child(
            h_flex()
                .gap_1p5()
                .child(
                    h_flex()
                        .debug_selector(move || format!("{prefix}-header"))
                        .flex_1()
                        .gap_1p5()
                        .items_baseline()
                        .child(Label::new(title).size(LabelSize::Large).weight(FontWeight::BOLD))
                        .child(Label::new(total.to_string()).color(Color::Muted)),
                )
                .children(trailing),
        )
        .child(
            div()
                .debug_selector(move || format!("{prefix}-filter"))
                .px_1()
                .py_0p5()
                .border_1()
                .border_color(border)
                .rounded_md()
                .child(filter.clone()),
        )
}

/// Intestazione di un gruppo: fondo `element_background`, disclosure a sinistra, click
/// (anche sul disclosure) che chiama `on_toggle`. Il resto lo aggiunge il chiamante.
pub fn group_header(
    id: impl Into<ElementId>,
    disclosure_id: impl Into<ElementId>,
    open: bool,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Stateful<Div> {
    let on_toggle = Rc::new(on_toggle);
    let toggle = on_toggle.clone();
    h_flex()
        .id(id)
        .gap_1p5()
        .px_1()
        .py_1()
        .rounded_md()
        .bg(cx.theme().colors().element_background)
        .cursor_pointer()
        .on_click(move |event, window, cx| toggle(event, window, cx))
        .child(Disclosure::new(disclosure_id, open).on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_toggle(event, window, cx)
        }))
}

/// Un gruppo e' aperto se non e' fra quelli chiusi dall'utente (`collapsed`), oppure se
/// un filtro e' attivo: con una query non vuota tutti i gruppi si mostrano espansi, cosi'
/// le righe che combaciano non restano nascoste sotto un gruppo chiuso. Non tocca lo
/// stato salvato di collassato/aperto: solo il render ignora `collapsed` mentre si filtra.
pub fn group_is_open(collapsed: bool, query: &str) -> bool {
    !collapsed || !query.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_query_trims_and_ignores_case() {
        assert!(matches_query(["Alpha"], ""));
        assert!(matches_query(["Alpha"], "  "));
        assert!(matches_query(["x", "Alpha"], " ALP "));
        assert!(!matches_query(["Alpha"], "beta"));
        assert!(!matches_query(std::iter::empty(), "a"));
    }

    #[test]
    fn group_is_open_ignores_the_collapsed_state_while_filtering() {
        assert!(!group_is_open(true, ""), "chiuso senza filtro: resta chiuso");
        assert!(group_is_open(false, ""), "aperto senza filtro: resta aperto");
        assert!(group_is_open(true, "abc"), "chiuso ma con un filtro attivo: si apre lo stesso");
        assert!(!group_is_open(true, "  "), "solo spazi: filtro vuoto, resta chiuso");
    }
}
