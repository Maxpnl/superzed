//! Nodo kanban: lo sfondo delle chat claude agganciate, una colonna per fase.
//! Le chat sono nodi del canvas disposti sopra di lui (`kanban_layout`); qui si
//! disegnano solo le colonne con le intestazioni.
use crate::canvas_view::CanvasView;
use crate::canvas_view::TITLE_BAR_REMS;
use crate::kanban_layout::{self, GAP};
use crate::node::NodeContent;
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, MouseButton,
    MouseDownEvent, Render, Subscription, WeakEntity, Window, px,
};
use superzed_sessions::Phase;
use ui::{IconButton, IconName, IconSize, Label, LabelSize, prelude::*, utils::WithRemSize};

/// Zoom minimo del testo delle intestazioni: sotto, nome e conteggio restano a
/// `rem * KANBAN_HEADER_MIN_ZOOM` (leggibili anche con il kanban intero in vista).
pub const KANBAN_HEADER_MIN_ZOOM: f32 = 0.6;
/// Altezza del testo dell'intestazione, in rem del suo testo (riga di un'etichetta Large,
/// ~1,62 rem, con un po' di margine).
pub const HEADER_TEXT_REMS: f32 = 1.75;

/// Fascia del layout fra la barra del titolo del nodo e la prima chat, in px di schermo:
/// `(header + GAP) * zoom` meno la barra del titolo (`header` in coordinate del canvas,
/// da `kanban_layout::Metrics`).
pub(crate) fn layout_band(header: f32, rem: f32, zoom: f32) -> f32 {
    ((header + GAP) * zoom - rem * TITLE_BAR_REMS * zoom).max(0.)
}

/// Fascia dell'intestazione disegnata: `(altezza, crescita verso l'alto)` in px di schermo.
/// `band` e' la fascia del layout (`layout_band`). Con l'intestazione in rem il testo ci
/// sta da `KANBAN_HEADER_MIN_ZOOM` in su; piu' sotto il testo resta leggibile e la fascia
/// cresce sopra (anche sulla barra del titolo), senza spostare nulla sotto.
pub(crate) fn header_band(band: f32, rem: f32, zoom: f32) -> (f32, f32) {
    let text = rem * zoom.max(KANBAN_HEADER_MIN_ZOOM) * HEADER_TEXT_REMS;
    let height = band.max(text);
    (height, height - band)
}

/// Quanto l'intestazione disegnata sale sopra il bordo superiore del nodo kanban, in px di
/// schermo: la crescita (`header_band`) meno la barra del titolo del nodo. Quando e'
/// positiva il nodo non disegna il bordo superiore, che taglierebbe il testo (il bordo si
/// dipinge sopra i figli), e l'intestazione ne disegna uno suo.
pub(crate) fn header_overhang(header: f32, rem: f32, zoom: f32) -> f32 {
    let (_, lift) = header_band(layout_band(header, rem, zoom), rem, zoom);
    (lift - rem * TITLE_BAR_REMS * zoom).max(0.)
}

pub struct KanbanView {
    canvas: WeakEntity<CanvasView>,
    focus: FocusHandle,
    zoom: f32,
    _subscriptions: Vec<Subscription>,
}

impl KanbanView {
    pub fn new(canvas: WeakEntity<CanvasView>, cx: &mut Context<Self>) -> Self {
        // il canvas notifica anche quando cambiano le sessioni (osserva lo store)
        let subscriptions = canvas
            .upgrade()
            .map(|c| vec![cx.observe(&c, |_, _, cx| cx.notify())])
            .unwrap_or_default();
        Self { canvas, focus: cx.focus_handle(), zoom: 1.0, _subscriptions: subscriptions }
    }

    /// Quante chat agganciate ci sono in ogni fase.
    fn counts(&self, cx: &App) -> [usize; kanban_layout::COLUMNS] {
        let mut counts = [0; kanban_layout::COLUMNS];
        if let Some(canvas) = self.canvas.upgrade() {
            for (_, phase) in canvas.read(cx).docked_chats(cx) {
                if let Some(ix) = Phase::ALL.iter().position(|p| *p == phase) {
                    counts[ix] += 1;
                }
            }
        }
        counts
    }
}

impl Focusable for KanbanView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for KanbanView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let counts = self.counts(cx);
        // colonne dove cadrebbero le chat trascinate (la stessa scelta del rilascio)
        let drop_columns = self.canvas.upgrade().map(|c| c.read(cx).kanban_drop_columns()).unwrap_or_default();
        // le stesse misure del layout (posizioni delle chat, rilascio)
        let metrics = self
            .canvas
            .upgrade()
            .map(|c| c.read(cx).kanban_metrics(cx))
            .unwrap_or_else(|| kanban_layout::Metrics::for_rem(f32::from(window.rem_size())));
        let colors = cx.theme().colors();
        // il nodo e' disegnato a `size * zoom` e il kanban e' largo una colonna per fase: le
        // colonne `flex_1` affiancate sono larghe `column_width(chat_size) * zoom` ciascuna.
        // L'intestazione occupa la fascia visibile fra la barra del titolo del nodo
        // (sopra il contenuto) e la prima chat, che parte a `header + GAP` dall'origine.
        let rem = window.rem_size();
        let band = layout_band(metrics.header, f32::from(rem), self.zoom);
        // sopra il bordo superiore del nodo (che qui non si disegna): un bordo suo, di 1 px
        // in piu' verso l'alto cosi' il testo non perde spazio
        let overhang = header_overhang(metrics.header, f32::from(rem), self.zoom) > 0.;
        let (header, lift) = header_band(band, f32::from(rem), self.zoom);
        let (header, lift) = if overhang { (header + 1., lift + 1.) } else { (header, lift) };
        // il testo dell'intestazione non scende sotto `KANBAN_HEADER_MIN_ZOOM`
        let header_rem = rem * self.zoom.max(KANBAN_HEADER_MIN_ZOOM);
        let columns = Phase::ALL.into_iter().zip(counts).enumerate().map(|(ix, (phase, n))| {
            // Nuove: pulsante + per una chat nuova gia' agganciata
            let plus = (phase == Phase::New).then(|| {
                let canvas = self.canvas.clone();
                // il mouse-down non arriva all'intestazione (il suo doppio click zooma
                // sulla colonna); un doppio click sul + apre una chat sola
                div()
                    .debug_selector(|| "superzed-kanban-new-chat".into())
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        IconButton::new("superzed-kanban-new-chat", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(ui::Tooltip::text("New chat in the kanban"))
                            .on_click(move |e, window, cx| {
                                if e.click_count() > 1 {
                                    return;
                                }
                                canvas.update(cx, |c, cx| c.new_claude_in_kanban(window, cx)).ok();
                            }),
                    )
            });
            // colonna vuota: un invito a trascinarci una chat (Nuove ha gia' il +)
            let empty = (n == 0 && phase != Phase::New).then(|| {
                div()
                    .debug_selector(move || format!("superzed-kanban-empty-{}", phase.as_str()))
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(
                        WithRemSize::new(header_rem).px_2().text_center().child(
                            Label::new("Drag a chat here").size(LabelSize::Default).color(Color::Muted),
                        ),
                    )
            });
            let drop_target = drop_columns.contains(&phase).then(|| {
                div()
                    .debug_selector(move || format!("superzed-kanban-drop-{}", phase.as_str()))
                    .absolute()
                    .inset_0()
                    .bg(colors.drop_target_background)
                    .border_2()
                    .border_color(colors.drop_target_border)
            });
            v_flex()
                .debug_selector(move || format!("superzed-kanban-col-{}", phase.as_str()))
                .relative()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .when(ix > 0, |d| d.border_l_1().border_color(colors.border))
                .child(
                    h_flex()
                        .debug_selector(move || format!("superzed-kanban-header-{}", phase.as_str()))
                        .flex_none()
                        .h(px(header))
                        // cresce verso l'alto, fuori dal riquadro: le card restano dove sono
                        .mt(px(-lift))
                        .bg(colors.title_bar_background)
                        .border_b_1()
                        .when(overhang, |d| d.border_t_1())
                        .border_color(colors.border_variant)
                        // doppio click: la colonna in vista
                        .on_mouse_down(MouseButton::Left, {
                            let canvas = self.canvas.clone();
                            move |e: &MouseDownEvent, _, cx| {
                                if e.click_count == 2 {
                                    canvas.update(cx, |c, cx| c.zoom_to_kanban_column(phase, cx)).ok();
                                }
                            }
                        })
                        .overflow_hidden()
                        .child(
                            WithRemSize::new(header_rem)
                                .flex()
                                .items_center()
                                .size_full()
                                .min_w_0()
                                .px_2()
                                .gap_1()
                                // nome troncato, conteggio mai tagliato
                                .child(div().min_w_0().debug_selector(move || format!("superzed-kanban-name-{}", phase.as_str())).child(
                                    Label::new(phase.label()).size(LabelSize::Large).weight(FontWeight::BOLD).truncate(),
                                ))
                                .child(
                                    div()
                                        .flex_none()
                                        .debug_selector(move || format!("superzed-kanban-count-{}", phase.as_str()))
                                        .child(Label::new(n.to_string()).size(LabelSize::Large).color(Color::Muted)),
                                )
                                .children(plus),
                        ),
                )
                .children(empty)
                .children(drop_target)
        });
        WithRemSize::new(window.rem_size() * self.zoom).size_full().child(
            h_flex()
                .key_context("Kanban")
                .track_focus(&self.focus)
                .size_full()
                .bg(colors.panel_background)
                .children(columns),
        )
    }
}

pub struct KanbanNode {
    view: Entity<KanbanView>,
}

impl KanbanNode {
    pub fn new(canvas: WeakEntity<CanvasView>, cx: &mut App) -> Self {
        Self { view: cx.new(|cx| KanbanView::new(canvas, cx)) }
    }
}

impl NodeContent for KanbanNode {
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
    use super::{HEADER_TEXT_REMS, KANBAN_HEADER_MIN_ZOOM, header_band, header_overhang, layout_band};
    use crate::canvas_view::TITLE_BAR_REMS;
    use crate::kanban_layout::Metrics;

    #[test]
    fn header_text_fits_the_layout_band_from_the_minimum_zoom_up() {
        for rem in [12., 16., 20., 24., 32.] {
            for zoom in [KANBAN_HEADER_MIN_ZOOM, 0.8, 1.0, 1.5, 2.0] {
                let band = layout_band(Metrics::for_rem(rem).header, rem, zoom);
                let (height, lift) = header_band(band, rem, zoom);
                assert_eq!(lift, 0., "rem {rem}, zoom {zoom}: fascia {band} senza crescita");
                assert!(height >= rem * zoom * HEADER_TEXT_REMS, "rem {rem}, zoom {zoom}");
            }
        }
        // sotto lo zoom minimo il testo non scende e la fascia cresce verso l'alto
        let rem = 16.;
        let band = layout_band(Metrics::for_rem(rem).header, rem, 0.2);
        assert!(header_band(band, rem, 0.2).1 > 0.);
    }

    #[test]
    fn header_overhangs_the_node_top_only_at_very_low_zoom() {
        let rem = 16.;
        let header = Metrics::for_rem(rem).header;
        for zoom in [KANBAN_HEADER_MIN_ZOOM, 1.0, 2.0] {
            assert_eq!(header_overhang(header, rem, zoom), 0., "zoom {zoom}");
        }
        // zoom 0,15 (screenshot): fascia 6,48, testo 16,8, barra del titolo 5,52
        let zoom = 0.15;
        let band = layout_band(header, rem, zoom);
        let (height, lift) = header_band(band, rem, zoom);
        let expected = lift - rem * TITLE_BAR_REMS * zoom;
        assert!(expected > 0.);
        assert!((header_overhang(header, rem, zoom) - expected).abs() < 1e-4);
        // la parte sopra il nodo piu' quella dentro e' l'intera fascia
        assert!((header_overhang(header, rem, zoom) + rem * TITLE_BAR_REMS * zoom + band - height).abs() < 1e-4);
    }

    #[test]
    fn header_band_grows_upward_when_the_text_does_not_fit() {
        assert_eq!(KANBAN_HEADER_MIN_ZOOM, 0.6);
        // zoom 1: fascia di 40 px, testo 16 * 1 * HEADER_TEXT_REMS: nessuna crescita
        let text1 = 16. * HEADER_TEXT_REMS;
        assert!(text1 <= 40.);
        assert_eq!(header_band(40., 16., 1.0), (40., 0.));
        // zoom 0,1: il testo resta a 16 * 0,6; la fascia cresce verso l'alto
        let text = 16. * 0.6 * HEADER_TEXT_REMS;
        assert_eq!(header_band(4., 16., 0.1), (text, text - 4.));
        // al limite: testo alto come la fascia
        assert_eq!(header_band(text, 16., 0.3), (text, 0.));
        // zoom grande: il testo segue lo zoom
        assert_eq!(header_band(10., 16., 2.0), (16. * 2. * HEADER_TEXT_REMS, 16. * 2. * HEADER_TEXT_REMS - 10.));
    }
}
