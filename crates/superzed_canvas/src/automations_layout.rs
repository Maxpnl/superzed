//! Layout della sezione Automazioni: una riga di chat in ordine di creazione, ognuna
//! con sopra la sua fascia di controllo. Puro.
use crate::model::NodeId;
use gpui::{Point, Size, point, size};
pub use crate::kanban_layout::{DEFAULT_CHAT_SIZE, GAP, MIN_CHAT_SIZE};

/// Altezza della fascia di controllo sopra ogni chat, in rem (96 px a 16 px di rem): il
/// testo della fascia segue il font dell'interfaccia e non viene tagliato.
pub const AUTOMATION_BAND_REMS: f32 = 6.;
/// Intestazione della sezione (barra del titolo compresa), in rem (come il kanban).
pub const AUTOMATIONS_HEADER_REMS: f32 = 4.;
/// Distanza fra il fondo del kanban e la sezione Automazioni.
pub const CONTAINER_GAP: f32 = 64.;

/// Misure della sezione che seguono il rem, in coordinate del canvas: le stesse per il
/// layout (posizioni, hit-test) e per il disegno.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub header: f32,
    pub band: f32,
}

impl Metrics {
    pub fn for_rem(rem: f32) -> Self {
        Self { header: AUTOMATIONS_HEADER_REMS * rem, band: AUTOMATION_BAND_REMS * rem }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutomationsLayout {
    pub size: Size<f32>,
    pub chats: Vec<(NodeId, Point<f32>)>,
    pub bands: Vec<(NodeId, Point<f32>)>,
}

pub fn layout(origin: Point<f32>, chats: &[NodeId], chat_size: Size<f32>, m: Metrics) -> AutomationsLayout {
    let mut sorted = chats.to_vec();
    sorted.sort();
    let band_y = origin.y + m.header + GAP;
    let mut out = AutomationsLayout { size: Size::default(), chats: Vec::new(), bands: Vec::new() };
    for (i, id) in sorted.iter().enumerate() {
        let x = origin.x + GAP + i as f32 * (chat_size.width + GAP);
        out.bands.push((*id, point(x, band_y)));
        out.chats.push((*id, point(x, band_y + m.band)));
    }
    // senza chat c'e' comunque posto per una
    let n = sorted.len().max(1) as f32;
    out.size = size(GAP + n * (chat_size.width + GAP), m.header + GAP + m.band + chat_size.height + GAP);
    out
}

/// Dimensione delle chat per una sezione di dimensione `s` con `n` chat (inverso di
/// `layout`, al minimo).
pub fn chat_size_for(s: Size<f32>, n: usize, m: Metrics) -> Size<f32> {
    let n = n.max(1) as f32;
    crate::kanban_layout::clamp_chat_size(size(
        (s.width - GAP) / n - GAP,
        s.height - m.header - m.band - 2. * GAP,
    ))
}

/// Dove stanno le Automazioni: sotto il kanban, allineate a sinistra.
pub fn below(kanban_origin: Point<f32>, kanban_size: Size<f32>) -> Point<f32> {
    point(kanban_origin.x, kanban_origin.y + kanban_size.height + CONTAINER_GAP)
}

/// Le Automazioni entrano nel kanban o nel margine sotto di lui (si sovrappongono).
pub fn overlaps_below(ko: Point<f32>, ks: Size<f32>, ao: Point<f32>, asz: Size<f32>) -> bool {
    let horizontal = ao.x < ko.x + ks.width && ao.x + asz.width > ko.x;
    let vertical = ao.y < ko.y + ks.height + CONTAINER_GAP && ao.y + asz.height > ko.y;
    horizontal && vertical
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    const D: Size<f32> = DEFAULT_CHAT_SIZE;
    const M: Metrics = Metrics { header: 64., band: 96. };

    #[test]
    fn metrics_follow_the_rem() {
        assert_eq!(Metrics::for_rem(16.), M, "a 16 px come prima");
        assert_eq!(Metrics::for_rem(24.), Metrics { header: 96., band: 144. });
        let big = Metrics::for_rem(24.);
        let l = layout(point(0., 0.), &[NodeId(1)], D, big);
        assert_eq!(l.bands[0].1.y, 96. + GAP);
        assert_eq!(l.chats[0].1.y, 96. + GAP + 144., "la chat sta sotto la fascia piu' alta");
        assert_eq!(chat_size_for(l.size, 1, big), D);
    }

    #[test]
    fn empty_section_has_room_for_one_chat() {
        let l = layout(point(0., 0.), &[], D, M);
        assert!(l.chats.is_empty() && l.bands.is_empty());
        assert_eq!(l.size, size(GAP + D.width + GAP, M.header + GAP + M.band + D.height + GAP));
        assert_eq!(l.size, size(832., 752.));
    }

    #[test]
    fn chats_sit_in_a_row_under_their_band_by_node_id() {
        let l = layout(point(100., 50.), &[NodeId(9), NodeId(3)], D, M);
        assert_eq!(l.bands, vec![(NodeId(3), point(116., 130.)), (NodeId(9), point(116. + 816., 130.))]);
        assert_eq!(l.chats, vec![(NodeId(3), point(116., 226.)), (NodeId(9), point(932., 226.))]);
        assert_eq!(l.size.width, GAP + 2. * (D.width + GAP));
    }

    #[test]
    fn chat_size_for_inverts_the_layout_and_clamps() {
        let want = size(700., 500.);
        let s = layout(point(0., 0.), &[NodeId(1), NodeId(2)], want, M).size;
        assert_eq!(chat_size_for(s, 2, M), want);
        assert_eq!(chat_size_for(layout(point(0., 0.), &[], want, M).size, 0, M), want, "zero chat vale una");
        assert_eq!(chat_size_for(size(10., 10.), 1, M), MIN_CHAT_SIZE);
    }

    #[test]
    fn below_and_overlap() {
        let (ko, ks) = (point(0., 0.), size(1000., 700.));
        assert_eq!(below(ko, ks), point(0., 764.));
        assert!(overlaps_below(ko, ks, point(10., 740.), size(832., 752.)), "dentro il margine");
        assert!(!overlaps_below(ko, ks, point(0., 764.), size(832., 752.)));
        assert!(!overlaps_below(ko, ks, point(1100., 100.), size(832., 752.)), "affiancate: niente");
        assert!(!overlaps_below(ko, ks, point(0., -900.), size(832., 752.)), "sopra: niente");
    }
}
