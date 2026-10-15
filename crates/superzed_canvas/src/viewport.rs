use gpui::{Point, Size, point};

use crate::kanban_layout::MIN_CHAT_SIZE;
use crate::terminal_node::{OVERVIEW_MIN_REM_PX, OVERVIEW_REF_H_REMS, OVERVIEW_REF_W_REMS};

/// Limite inferiore assoluto dello zoom (serve a non dividere per zero). Con nodi chat o
/// terminale lo zoom out si ferma prima, a `min_zoom_for`.
pub const MIN_ZOOM: f32 = 0.02;
/// Zoom minimo di un canvas senza nodi chat/terminale.
pub const MIN_ZOOM_EMPTY: f32 = 0.1;
pub const MAX_ZOOM: f32 = 3.0;
/// Passo di zoom per tastiera e cmd+scroll.
pub const ZOOM_STEP: f32 = 1.1;

/// Trasformazione canvas → schermo locale: screen = canvas * zoom + offset.
/// "Schermo locale" = coordinate con origine nell'angolo in alto a sinistra
/// dell'elemento canvas (non della finestra): convertire gli eventi con
/// [`window_to_local`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub offset: Point<f32>,
    pub zoom: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            offset: point(0., 0.),
            zoom: 1.0,
        }
    }
}

impl Viewport {
    pub fn to_screen(&self, p: Point<f32>) -> Point<f32> {
        point(p.x * self.zoom + self.offset.x, p.y * self.zoom + self.offset.y)
    }

    pub fn to_canvas(&self, p: Point<f32>) -> Point<f32> {
        point(
            (p.x - self.offset.x) / self.zoom,
            (p.y - self.offset.y) / self.zoom,
        )
    }

    pub fn pan(&mut self, delta: Point<f32>) {
        self.offset = point(self.offset.x + delta.x, self.offset.y + delta.y);
    }

    /// Zoom di `factor` tenendo fermo `anchor` (coordinate locali al canvas), mai sotto
    /// `min_zoom` (vedi `clamp_zoom`).
    pub fn zoom_at(&mut self, anchor: Point<f32>, factor: f32, min_zoom: f32) {
        let anchor_canvas = self.to_canvas(anchor);
        self.zoom = clamp_zoom(self.zoom, self.zoom * factor, min_zoom);
        self.offset = point(
            anchor.x - anchor_canvas.x * self.zoom,
            anchor.y - anchor_canvas.y * self.zoom,
        );
    }

    /// Zoom a 1, tenendo fermo `anchor`.
    pub fn reset_zoom_at(&mut self, anchor: Point<f32>) {
        let factor = 1.0 / self.zoom;
        self.zoom_at(anchor, factor, MIN_ZOOM);
        self.zoom = 1.0;
    }
}

/// Zoom minimo dinamico: il piu' piccolo zoom a cui ogni nodo chat/terminale (dimensioni
/// `nodes` in coordinate canvas) ha ancora il testo della vista d'insieme, cioe'
/// `overview_rem` >= `OVERVIEW_MIN_REM`. Decide il nodo piu' esigente. Non dipende dal rem
/// dell'interfaccia finche' e' >= `OVERVIEW_MIN_REM` (sotto, il testo non si legge a
/// nessuno zoom). Sempre entro `MIN_ZOOM..=1`; senza nodi `MIN_ZOOM_EMPTY`.
/// Tetto a `MAX_DYNAMIC_MIN_ZOOM`: un nodo piu' piccolo della chat minima del kanban non
/// blocca lo zoom out di tutto il canvas (a quel minimo puo' mostrare solo l'icona).
pub fn min_zoom_for(nodes: impl IntoIterator<Item = Size<f32>>) -> f32 {
    nodes
        .into_iter()
        .map(node_min_zoom)
        .reduce(f32::max)
        .map_or(MIN_ZOOM_EMPTY, |z| z.min(MAX_DYNAMIC_MIN_ZOOM).clamp(MIN_ZOOM, 1.0))
}

/// Zoom minimo dinamico massimo: quello di una chat grande `MIN_CHAT_SIZE` (~0,31).
pub const MAX_DYNAMIC_MIN_ZOOM: f32 = node_min_zoom(MIN_CHAT_SIZE);

/// Zoom a cui la vista d'insieme del nodo `n` ha esattamente `OVERVIEW_MIN_REM` di rem.
const fn node_min_zoom(n: Size<f32>) -> f32 {
    let by_h = OVERVIEW_MIN_REM_PX * OVERVIEW_REF_H_REMS / n.height;
    let by_w = OVERVIEW_MIN_REM_PX * OVERVIEW_REF_W_REMS / n.width;
    by_h.max(by_w)
}

/// Zoom per un gesto da `current` verso `target`: entro `MAX_ZOOM` e mai sotto `min_zoom`
/// (ridotto a `MIN_ZOOM..=1`). Se `current` e' gia' sotto il minimo (il minimo e' salito)
/// non forza lo zoom in: lo zoom out resta dov'e', lo zoom in procede normalmente.
pub fn clamp_zoom(current: f32, target: f32, min_zoom: f32) -> f32 {
    let floor = min_zoom.clamp(MIN_ZOOM, 1.0).min(current.max(MIN_ZOOM));
    target.clamp(floor, MAX_ZOOM)
}

/// Margine (px di schermo) attorno a un rettangolo portato in vista da `fit_rect`.
pub const FIT_MARGIN: f32 = 24.;

/// Viewport che mostra il rettangolo `origin`/`size` (coordinate canvas) centrato in
/// una vista `view` (px locali): a zoom `cap` se ci sta con `margin` per lato, altrimenti
/// allo zoom che lo fa stare; sempre entro `min_zoom` (ridotto a MIN_ZOOM..=1)..MAX_ZOOM
/// (al minimo un rettangolo enorme resta centrato ma non ci sta). `None` se la vista o il
/// rettangolo sono vuoti.
pub fn fit_rect(
    origin: Point<f32>,
    size: Size<f32>,
    view: Size<f32>,
    margin: f32,
    cap: f32,
    min_zoom: f32,
) -> Option<Viewport> {
    let (avail_w, avail_h) = (view.width - 2. * margin, view.height - 2. * margin);
    if avail_w <= 0. || avail_h <= 0. || size.width <= 0. || size.height <= 0. {
        return None;
    }
    let zoom = cap.min(avail_w / size.width).min(avail_h / size.height).clamp(min_zoom.clamp(MIN_ZOOM, 1.0), MAX_ZOOM);
    let center = point(origin.x + size.width / 2., origin.y + size.height / 2.);
    Some(Viewport {
        offset: point(view.width / 2. - center.x * zoom, view.height / 2. - center.y * zoom),
        zoom,
    })
}

/// Converte una posizione in coordinate finestra in coordinate locali al canvas.
pub fn window_to_local(p: Point<f32>, canvas_origin: Point<f32>) -> Point<f32> {
    point(p.x - canvas_origin.x, p.y - canvas_origin.y)
}

/// Fattore di zoom per un `PinchEvent` (delta 0.1 = +10%), mai <= 0.
pub fn pinch_factor(delta: f32) -> f32 {
    (1.0 + delta).max(0.05)
}

/// Fattore di zoom per cmd+scroll; `None` se lo scroll e' solo orizzontale.
pub fn scroll_zoom_factor(dy: f32) -> Option<f32> {
    if dy > 0.0 {
        Some(ZOOM_STEP)
    } else if dy < 0.0 {
        Some(1.0 / ZOOM_STEP)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    #[test]
    fn identity_viewport_maps_points_unchanged() {
        let v = Viewport::default();
        assert_eq!(v.to_screen(point(10., 20.)), point(10., 20.));
    }

    #[test]
    fn pan_moves_screen_positions() {
        let mut v = Viewport::default();
        v.pan(point(5., -5.));
        assert_eq!(v.to_screen(point(0., 0.)), point(5., -5.));
    }

    #[test]
    fn zoom_at_keeps_anchor_fixed() {
        let mut v = Viewport::default();
        v.pan(point(40., 10.));
        let anchor = point(100., 100.);
        let before = v.to_canvas(anchor);
        v.zoom_at(anchor, 2.0, MIN_ZOOM);
        let after = v.to_canvas(anchor);
        assert!((before.x - after.x).abs() < 1e-4 && (before.y - after.y).abs() < 1e-4);
        assert_eq!(v.zoom, 2.0);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut v = Viewport::default();
        v.zoom_at(point(0., 0.), 100.0, MIN_ZOOM);
        assert_eq!(v.zoom, MAX_ZOOM);
        v.zoom_at(point(0., 0.), 0.0001, MIN_ZOOM);
        assert_eq!(v.zoom, MIN_ZOOM);
    }

    #[test]
    fn reset_zoom_keeps_anchor_fixed() {
        let mut v = Viewport::default();
        v.zoom_at(point(50., 50.), 2.0, MIN_ZOOM);
        let anchor = point(200., 120.);
        let before = v.to_canvas(anchor);
        v.reset_zoom_at(anchor);
        assert_eq!(v.zoom, 1.0);
        let after = v.to_canvas(anchor);
        assert!((before.x - after.x).abs() < 1e-3 && (before.y - after.y).abs() < 1e-3);
    }

    #[test]
    fn window_position_is_converted_to_canvas_local() {
        // canvas a x=300, y=64 della finestra (dock + title bar)
        assert_eq!(window_to_local(point(350., 100.), point(300., 64.)), point(50., 36.));
    }

    #[test]
    fn pinch_factor_follows_magnification() {
        // PinchEvent::delta: 0.1 = +10%
        assert!((pinch_factor(0.1) - 1.1).abs() < 1e-6);
        assert!((pinch_factor(-0.1) - 0.9).abs() < 1e-6);
        // un delta assurdo non inverte lo zoom
        assert!(pinch_factor(-5.0) > 0.0);
    }

    #[test]
    fn scroll_zoom_factor_ignores_horizontal_only_scroll() {
        assert_eq!(scroll_zoom_factor(0.0), None);
        assert_eq!(scroll_zoom_factor(3.0), Some(ZOOM_STEP));
        assert_eq!(scroll_zoom_factor(-3.0), Some(1.0 / ZOOM_STEP));
    }

    #[test]
    fn fit_rect_centers_at_zoom_one_when_it_fits() {
        let view = gpui::size(1000., 800.);
        let v = fit_rect(point(100., 50.), gpui::size(400., 300.), view, FIT_MARGIN, 1.0, MIN_ZOOM).unwrap();
        assert_eq!(v.zoom, 1.0);
        assert_eq!(v.to_screen(point(300., 200.)), point(500., 400.), "centro del rettangolo al centro della vista");
    }

    #[test]
    fn fit_rect_zooms_out_to_fit_with_margin() {
        let view = gpui::size(1000., 800.);
        // largo: lo zoom lo fa stare in 1000 - 2*24 px
        let v = fit_rect(point(0., 0.), gpui::size(1904., 100.), view, FIT_MARGIN, 1.0, MIN_ZOOM).unwrap();
        assert!((v.zoom - 0.5).abs() < 1e-6);
        assert!((v.to_screen(point(0., 0.)).x - FIT_MARGIN).abs() < 1e-3, "margine a sinistra");
        assert!((v.to_screen(point(1904., 0.)).x - (1000. - FIT_MARGIN)).abs() < 1e-3, "margine a destra");
        // alto: decide l'altezza
        let v = fit_rect(point(0., 0.), gpui::size(100., 1504.), view, FIT_MARGIN, 1.0, MIN_ZOOM).unwrap();
        assert!((v.zoom - 0.5).abs() < 1e-6);
    }

    #[test]
    fn fit_rect_clamps_and_rejects_empty_views() {
        let view = gpui::size(1000., 800.);
        let huge = fit_rect(point(0., 0.), gpui::size(100_000., 100.), view, FIT_MARGIN, 1.0, MIN_ZOOM).unwrap();
        assert_eq!(huge.zoom, MIN_ZOOM);
        assert_eq!(huge.to_screen(point(50_000., 50.)), point(500., 400.), "centrato anche se non ci sta");
        let tiny = fit_rect(point(0., 0.), gpui::size(10., 10.), view, FIT_MARGIN, 100., MIN_ZOOM).unwrap();
        assert_eq!(tiny.zoom, MAX_ZOOM, "il cap sopra MAX_ZOOM non passa");
        assert!(fit_rect(point(0., 0.), gpui::size(10., 10.), gpui::size(40., 800.), FIT_MARGIN, 1.0, MIN_ZOOM).is_none());
        assert!(fit_rect(point(0., 0.), gpui::size(0., 10.), view, FIT_MARGIN, 1.0, MIN_ZOOM).is_none());
    }

    #[test]
    fn min_zoom_for_keeps_the_smallest_node_readable() {
        use crate::terminal_node::{OVERVIEW_MIN_REM, overview_rem};
        use gpui::px;
        assert_eq!(OVERVIEW_MIN_REM, px(8.));
        // nodo di default 800 x 560: decide l'altezza (8 x 10 / 560)
        let default = gpui::size(800., 560.);
        let z = min_zoom_for([default]);
        assert!((z - 8. * 10. / 560.).abs() < 1e-6, "{z}");
        // nodo minimo del kanban 400 x 260: sempre l'altezza (80 / 260 > 104 / 400)
        let small = gpui::size(400., 260.);
        let zs = min_zoom_for([small]);
        assert!((zs - 8. * 10. / 260.).abs() < 1e-6, "{zs}");
        // stretto e alto: decide la larghezza (8 x 13 / 350)
        let narrow = gpui::size(350., 2000.);
        assert!((min_zoom_for([narrow]) - 104. / 350.).abs() < 1e-6);
        // piu' nodi: vale il piu' esigente
        assert_eq!(min_zoom_for([default, small, default]), zs);
        // al suo zoom minimo ogni nodo ha ancora il testo della vista d'insieme, appena
        // sotto no: il minimo e' proprio dove il testo smette di leggersi
        let overview = |n: Size<f32>, z: f32| overview_rem(gpui::size(px(n.width * z), px(n.height * z)), px(16.));
        for n in [default, small, narrow] {
            let z = min_zoom_for([n]);
            assert!(overview(n, z * 1.0001).is_some(), "{n:?} a {z}");
            assert!(overview(n, z * 0.99).is_none(), "{n:?} sotto {z}");
        }
        // nessun nodo chat/terminale
        assert_eq!(min_zoom_for(std::iter::empty()), MIN_ZOOM_EMPTY);
        assert_eq!(MIN_ZOOM_EMPTY, 0.1);
        // un nodo piu' piccolo della chat minima del kanban non blocca lo zoom out di
        // tutto il canvas: il minimo si ferma a quello di MIN_CHAT_SIZE (~0,31, non 0,89)
        let min_chat = crate::kanban_layout::MIN_CHAT_SIZE;
        assert_eq!(MAX_DYNAMIC_MIN_ZOOM, min_zoom_for([min_chat]));
        assert!((MAX_DYNAMIC_MIN_ZOOM - 8. * 10. / 260.).abs() < 1e-6, "{MAX_DYNAMIC_MIN_ZOOM}");
        assert_eq!(min_zoom_for([gpui::size(160., 90.)]), MAX_DYNAMIC_MIN_ZOOM, "shell minuscola");
        assert_eq!(min_zoom_for([default, gpui::size(160., 90.)]), MAX_DYNAMIC_MIN_ZOOM);
        assert_eq!(min_zoom_for([gpui::size(20., 20.)]), MAX_DYNAMIC_MIN_ZOOM);
        // mai sotto MIN_ZOOM (nodo enorme)
        assert_eq!(min_zoom_for([gpui::size(1e6, 1e6)]), MIN_ZOOM);
        assert_eq!(MIN_ZOOM, 0.02);
    }

    #[test]
    fn clamp_zoom_stops_at_the_floor_without_forcing_a_zoom_in() {
        // sopra il minimo: lo zoom out si ferma al minimo
        assert_eq!(clamp_zoom(0.5, 0.1, 0.25), 0.25);
        assert_eq!(clamp_zoom(0.5, 0.3, 0.25), 0.3);
        assert_eq!(clamp_zoom(1.0, 10.0, 0.25), MAX_ZOOM);
        // gia' sotto il minimo (un nodo e' stato rimpicciolito): lo zoom out non va
        // piu' giu', ma non salta nemmeno al minimo
        assert_eq!(clamp_zoom(0.15, 0.15 / ZOOM_STEP, 0.25), 0.15);
        // lo zoom in da li' funziona normalmente
        assert_eq!(clamp_zoom(0.15, 0.2, 0.25), 0.2);
        // il minimo non va mai sopra 1 ne' sotto MIN_ZOOM
        assert_eq!(clamp_zoom(2.0, 0.5, 5.0), 1.0);
        assert_eq!(clamp_zoom(0.5, 0.0001, 0.0), MIN_ZOOM);
    }

    #[test]
    fn zoom_at_respects_the_floor() {
        let mut v = Viewport::default();
        for _ in 0..100 {
            v.zoom_at(point(10., 10.), 1. / ZOOM_STEP, 0.25);
        }
        assert_eq!(v.zoom, 0.25);
        let anchor = point(300., 200.);
        let before = v.to_canvas(anchor);
        v.zoom_at(anchor, 1. / ZOOM_STEP, 0.25);
        let after = v.to_canvas(anchor);
        assert!((before.x - after.x).abs() < 1e-3 && (before.y - after.y).abs() < 1e-3, "fermo, ancora al suo posto");
    }

    #[test]
    fn fit_rect_stops_at_the_floor() {
        let view = gpui::size(1000., 800.);
        let v = fit_rect(point(0., 0.), gpui::size(100_000., 100.), view, FIT_MARGIN, 1.0, 0.25).unwrap();
        assert_eq!(v.zoom, 0.25);
        assert_eq!(v.to_screen(point(50_000., 50.)), point(500., 400.), "centrato anche se non ci sta");
        // se ci sta sopra il minimo, il fit non cambia
        let v = fit_rect(point(0., 0.), gpui::size(1904., 100.), view, FIT_MARGIN, 1.0, 0.25).unwrap();
        assert!((v.zoom - 0.5).abs() < 1e-6);
    }

    #[test]
    fn roundtrip_screen_canvas() {
        let mut v = Viewport::default();
        v.pan(point(33., 44.));
        v.zoom_at(point(10., 10.), 1.7, MIN_ZOOM);
        let p = point(123., -45.);
        let back = v.to_canvas(v.to_screen(p));
        assert!((back.x - p.x).abs() < 1e-3 && (back.y - p.y).abs() < 1e-3);
    }
}
