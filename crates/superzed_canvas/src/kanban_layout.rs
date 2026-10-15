//! Layout del kanban: dove stanno le chat agganciate e quanto e' grande il kanban.
//! Puro: dalle fasi delle chat e dalla dimensione delle chat (del kanban), niente
//! GPUI a parte i tipi geometrici.
use crate::model::NodeId;
use gpui::{Point, Size, point, size};
use superzed_sessions::Phase;

/// Una colonna per fase.
pub const COLUMNS: usize = Phase::ALL.len();

/// Dimensione delle chat agganciate di un kanban nuovo (o senza `chat_size` salvato).
pub const DEFAULT_CHAT_SIZE: Size<f32> = Size { width: 800., height: 560. };
/// Le chat agganciate non scendono sotto questa dimensione (resize del kanban).
pub const MIN_CHAT_SIZE: Size<f32> = Size { width: 400., height: 260. };
pub const GAP: f32 = 16.;
/// Intestazione del kanban (barra del titolo compresa), in rem: 64 px a 16 px di rem. In
/// rem come le Automazioni, perche' con un font dell'interfaccia grande la fascia fra la
/// barra del titolo e le chat contenga il testo delle intestazioni delle colonne.
pub const KANBAN_HEADER_REMS: f32 = 4.;

/// Misure del kanban che seguono il rem, in coordinate del canvas: le stesse per il
/// layout (posizioni, resize, zoom sulla colonna) e per il disegno.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub header: f32,
}

impl Metrics {
    pub fn for_rem(rem: f32) -> Self {
        Self { header: KANBAN_HEADER_REMS * rem }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct KanbanLayout {
    pub size: Size<f32>,
    pub positions: Vec<(NodeId, Point<f32>)>,
}

fn column_index(phase: Phase) -> usize {
    Phase::ALL.iter().position(|p| *p == phase).unwrap_or(0)
}

/// Larghezza di una colonna: una chat e un margine per lato.
pub fn column_width(chat_size: Size<f32>) -> f32 {
    chat_size.width + 2. * GAP
}

pub fn column_origin(kanban_origin: Point<f32>, phase: Phase, chat_size: Size<f32>) -> Point<f32> {
    point(kanban_origin.x + column_index(phase) as f32 * column_width(chat_size), kanban_origin.y)
}

/// Rettangolo di una colonna con `count` chat (almeno una riga): intestazione e chat,
/// per portarla in vista (doppio click sull'intestazione).
pub fn column_rect(
    kanban_origin: Point<f32>,
    phase: Phase,
    chat_size: Size<f32>,
    count: usize,
    m: Metrics,
) -> (Point<f32>, Size<f32>) {
    let rows = count.max(1) as f32;
    (
        column_origin(kanban_origin, phase, chat_size),
        size(column_width(chat_size), m.header + rows * (chat_size.height + GAP) + GAP),
    )
}

/// Chat nella colonna piu' piena (almeno 1: il kanban vuoto ha posto per una riga).
pub fn rows(chats: &[(NodeId, Phase)]) -> usize {
    let mut per_column = [0usize; COLUMNS];
    for (_, phase) in chats {
        per_column[column_index(*phase)] += 1;
    }
    per_column.iter().copied().max().unwrap_or(0).max(1)
}

pub fn layout(origin: Point<f32>, chats: &[(NodeId, Phase)], chat_size: Size<f32>, m: Metrics) -> KanbanLayout {
    let column = column_width(chat_size);
    let mut sorted = chats.to_vec();
    sorted.sort_by_key(|(id, _)| *id);
    let mut per_column = [0usize; COLUMNS];
    let mut positions = Vec::with_capacity(sorted.len());
    for (id, phase) in sorted {
        let col = column_index(phase);
        let row = per_column[col];
        per_column[col] += 1;
        let x = origin.x + col as f32 * column + GAP;
        let y = origin.y + m.header + GAP + row as f32 * (chat_size.height + GAP);
        positions.push((id, point(x, y)));
    }
    let rows = rows(chats) as f32;
    KanbanLayout {
        size: size(COLUMNS as f32 * column, m.header + rows * (chat_size.height + GAP) + GAP),
        positions,
    }
}

pub fn column_at(
    kanban_origin: Point<f32>,
    kanban_size: Size<f32>,
    chat_size: Size<f32>,
    p: Point<f32>,
) -> Option<Phase> {
    let (dx, dy) = (p.x - kanban_origin.x, p.y - kanban_origin.y);
    if dx < 0. || dy < 0. || dx >= kanban_size.width || dy >= kanban_size.height {
        return None;
    }
    Phase::ALL.get((dx / column_width(chat_size)) as usize).copied()
}

/// Porta una dimensione di chat al minimo (una dimensione non finita vale il default).
pub fn clamp_chat_size(s: Size<f32>) -> Size<f32> {
    if !s.width.is_finite() || !s.height.is_finite() {
        return DEFAULT_CHAT_SIZE;
    }
    size(s.width.max(MIN_CHAT_SIZE.width), s.height.max(MIN_CHAT_SIZE.height))
}

/// Inverso di `layout`: la dimensione delle chat che da' al kanban la dimensione
/// chiesta (resize dalla maniglia) con `rows` righe (`rows(chats)`); al minimo.
pub fn chat_size_for(kanban_size: Size<f32>, rows: usize, m: Metrics) -> Size<f32> {
    let rows = rows.max(1) as f32;
    clamp_chat_size(size(
        kanban_size.width / COLUMNS as f32 - 2. * GAP,
        (kanban_size.height - m.header - GAP) / rows - GAP,
    ))
}

/// `chat_size` dal `data` del nodo kanban; mancante o non valido → default, sotto il
/// minimo → al minimo.
pub fn chat_size_from_data(data: &serde_json::Value) -> Size<f32> {
    let pair = data.get("chat_size").and_then(|v| v.as_array()).and_then(|a| match a.as_slice() {
        [w, h] => Some((w.as_f64()?, h.as_f64()?)),
        _ => None,
    });
    match pair {
        Some((w, h)) => clamp_chat_size(size(w as f32, h as f32)),
        None => DEFAULT_CHAT_SIZE,
    }
}

/// Scrive `chat_size` nel `data` del nodo kanban, tenendo le altre chiavi.
pub fn chat_size_to_data(data: &serde_json::Value, chat_size: Size<f32>) -> serde_json::Value {
    let mut map = data.as_object().cloned().unwrap_or_default();
    map.insert("chat_size".into(), serde_json::json!([chat_size.width, chat_size.height]));
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;
    use serde_json::json;
    use superzed_sessions::Phase;

    const D: Size<f32> = DEFAULT_CHAT_SIZE;
    const M: Metrics = Metrics { header: 64. };
    const HEADER: f32 = M.header;

    #[test]
    fn header_follows_the_rem() {
        assert_eq!(Metrics::for_rem(16.), M, "a 16 px come prima");
        let big = Metrics::for_rem(24.);
        assert_eq!(big, Metrics { header: 96. });
        let chats = [(NodeId(1), Phase::New), (NodeId(2), Phase::New)];
        let l = layout(point(0., 0.), &chats, D, big);
        assert_eq!(l.positions[0].1, point(2. * column_width(D) + GAP, 96. + GAP), "la prima chat sotto l'intestazione piu' alta");
        assert_eq!(l.size.height, 96. + 2. * (D.height + GAP) + GAP);
        assert_eq!(chat_size_for(l.size, 2, big), D, "inverso con le stesse misure");
        assert_eq!(column_rect(point(0., 0.), Phase::New, D, 2, big).1.height, l.size.height);
    }

    #[test]
    fn empty_kanban_has_room_for_one_chat_per_column() {
        let l = layout(point(0., 0.), &[], D, M);
        assert!(l.positions.is_empty());
        assert_eq!(l.size.width, COLUMNS as f32 * column_width(D));
        assert_eq!(l.size.height, HEADER + D.height + 2. * GAP);
        assert_eq!(D, size(800., 560.));
        assert_eq!(column_width(D), 800. + 2. * GAP);
    }

    #[test]
    fn chats_stack_in_their_phase_column_by_node_id() {
        let origin = point(100., 50.);
        let chats = [
            (NodeId(7), Phase::Plan),
            (NodeId(2), Phase::New),
            (NodeId(5), Phase::New),
            (NodeId(9), Phase::Done),
        ];
        let l = layout(origin, &chats, D, M);
        let pos = |id: u64| l.positions.iter().find(|(n, _)| n.0 == id).unwrap().1;
        let cw = column_width(D);
        let new_x = 100. + 2. * cw + GAP;
        assert_eq!(pos(2), point(new_x, 50. + HEADER + GAP));
        assert_eq!(pos(5), point(new_x, 50. + HEADER + GAP + D.height + GAP), "sotto la 2");
        assert_eq!(pos(7), point(100. + 5. * cw + GAP, 50. + HEADER + GAP), "colonna Piano");
        assert_eq!(pos(9).x, 100. + 7. * cw + GAP);
        assert_eq!(l.size.height, HEADER + 2. * (D.height + GAP) + GAP, "la colonna piu' piena");
    }

    #[test]
    fn layout_uses_the_given_chat_size() {
        let s = size(500., 300.);
        let chats = [(NodeId(1), Phase::New), (NodeId(2), Phase::New), (NodeId(3), Phase::Spec)];
        let l = layout(point(0., 0.), &chats, s, M);
        let pos = |id: u64| l.positions.iter().find(|(n, _)| n.0 == id).unwrap().1;
        let cw = 500. + 2. * GAP;
        assert_eq!(pos(2), point(2. * cw + GAP, HEADER + GAP + 300. + GAP));
        assert_eq!(pos(3), point(4. * cw + GAP, HEADER + GAP));
        assert_eq!(l.size, size(COLUMNS as f32 * (500. + 2. * GAP), HEADER + 2. * (300. + GAP) + GAP));
        assert_eq!(column_origin(point(0., 0.), Phase::Spec, s), point(4. * cw, 0.));
    }

    #[test]
    fn column_at_finds_columns_and_rejects_outside() {
        let (o, s) = (point(0., 0.), layout(point(0., 0.), &[], D, M).size);
        let cw = column_width(D);
        assert_eq!(column_at(o, s, D, point(10., 10.)), Some(Phase::Others));
        assert_eq!(column_at(o, s, D, point(cw + 1., 10.)), Some(Phase::Analysis));
        assert_eq!(column_at(o, s, D, point(cw * 2. + 1., 200.)), Some(Phase::New));
        assert_eq!(column_at(o, s, D, point(cw * 4. + 1., 200.)), Some(Phase::Spec));
        assert_eq!(column_at(o, s, D, point(s.width - 1., s.height - 1.)), Some(Phase::Done));
        assert_eq!(column_at(o, s, D, point(-1., 10.)), None);
        assert_eq!(column_at(o, s, D, point(10., s.height + 1.)), None);
        assert_eq!(column_at(o, s, D, point(s.width + 1., 10.)), None);
        assert_eq!(column_origin(point(10., 20.), Phase::Plan, D), point(10. + 5. * cw, 20.));
        // con chat piu' piccole le colonne sono piu' strette
        let small = MIN_CHAT_SIZE;
        let ss = layout(o, &[], small, M).size;
        assert_eq!(column_at(o, ss, small, point(3. * column_width(small) + 1., 100.)), Some(Phase::Brainstorming));
    }

    #[test]
    fn column_rect_covers_header_and_chats() {
        let s = size(500., 300.);
        let (o, r) = column_rect(point(10., 20.), Phase::Spec, s, 2, M);
        assert_eq!(o, point(10. + 4. * column_width(s), 20.));
        assert_eq!(r, size(column_width(s), HEADER + 2. * (300. + GAP) + GAP));
        assert_eq!(column_rect(point(0., 0.), Phase::New, s, 0, M).1.height, HEADER + 300. + 2. * GAP, "vuota: una riga");
    }

    #[test]
    fn chat_size_for_inverts_the_layout_and_clamps() {
        let chats = [(NodeId(1), Phase::New), (NodeId(2), Phase::New), (NodeId(3), Phase::Plan)];
        let want = size(900., 600.);
        let k = layout(point(0., 0.), &chats, want, M).size;
        assert_eq!(rows(&chats), 2);
        assert_eq!(chat_size_for(k, rows(&chats), M), want, "due righe: l'altezza si divide");
        let k1 = layout(point(0., 0.), &[], want, M).size;
        assert_eq!(rows(&[]), 1, "kanban vuoto: una riga");
        assert_eq!(chat_size_for(k1, rows(&[]), M), want);
        assert_eq!(chat_size_for(k1, 0, M), want, "zero righe vale una");
        assert_eq!(chat_size_for(size(10., 10.), 2, M), MIN_CHAT_SIZE, "al minimo");
        assert_eq!(chat_size_for(size(COLUMNS as f32 * 1000., 10.), 1, M), size(1000. - 2. * GAP, MIN_CHAT_SIZE.height));
    }

    #[test]
    fn chat_size_from_and_to_data() {
        assert_eq!(chat_size_from_data(&serde_json::Value::Null), D);
        assert_eq!(chat_size_from_data(&json!({})), D);
        assert_eq!(chat_size_from_data(&json!({"chat_size": "big"})), D);
        assert_eq!(chat_size_from_data(&json!({"chat_size": [1.0]})), D);
        assert_eq!(chat_size_from_data(&json!({"chat_size": [1.0, "x"]})), D);
        assert_eq!(chat_size_from_data(&json!({"chat_size": [1000.0, 700.0]})), size(1000., 700.));
        assert_eq!(chat_size_from_data(&json!({"chat_size": [10, 700]})), size(MIN_CHAT_SIZE.width, 700.));
        let data = chat_size_to_data(&json!({"other": 1}), size(900., 600.));
        assert_eq!(data, json!({"other": 1, "chat_size": [900.0, 600.0]}));
        assert_eq!(chat_size_to_data(&serde_json::Value::Null, D), json!({"chat_size": [800.0, 560.0]}));
        assert_eq!(chat_size_from_data(&data), size(900., 600.));
    }
}
