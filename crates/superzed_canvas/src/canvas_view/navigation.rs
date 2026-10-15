//! Navigazione da tastiera tra le chat (cmd+frecce, cmd-alt-destra/sinistra).
//!
//! Il canvas ha tre aree: il kanban (chat agganciate), le Automazioni (chat delle fasce)
//! e i nodi liberi (tutto il resto tranne i contenitori). Le frecce si muovono dentro
//! l'area del nodo con il focus (o selezionato); cmd-alt-destra/sinistra cambiano area.
//! Un'area esiste se ha chat, o (per kanban/Automazioni) se il suo contenitore e' sul
//! canvas anche senza chat (Task 14): entrandoci senza chat si atterra sul contenitore.
//! La scelta del vicino e' fatta da funzioni pure; `CanvasView` ci mette le posizioni,
//! il focus e lo spostamento della viewport.
use super::*;

/// Durata dello spostamento animato della viewport verso la destinazione.
pub const NAV_PAN_MS: u64 = 160;
/// Passi dell'animazione (uno ogni `NAV_PAN_MS / NAV_PAN_STEPS`).
const NAV_PAN_STEPS: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Area {
    Kanban,
    Automations,
    Free,
}

impl Area {
    /// Ordine del ciclo di `FocusNextArea`.
    pub const ORDER: [Area; 3] = [Area::Kanban, Area::Automations, Area::Free];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Area di partenza quando niente ha il focus o e' selezionato: la prima non vuota
/// nell'ordine Kanban -> Automazioni -> Liberi, il kanban se sono tutte vuote (cosi'
/// cmd+freccia e il ciclo di area restano senza effetto, come prima di questa scelta).
pub fn default_area(has_nodes: impl Fn(Area) -> bool) -> Area {
    Area::ORDER.into_iter().find(|a| has_nodes(*a)).unwrap_or(Area::Kanban)
}

/// Se un'area esiste nel ciclo (Task 14): il kanban e le Automazioni esistono anche a
/// zero chat quando il loro contenitore e' sul canvas, non solo con chat agganciate; i
/// nodi liberi solo se ce ne sono (non hanno un contenitore).
pub fn area_exists(area: Area, members: usize, has_container: bool) -> bool {
    members > 0 || (has_container && area != Area::Free)
}

/// Area dopo (o prima di) `current` nel ciclo, saltando quelle senza nodi; `None` se
/// nessun'altra area ha nodi.
pub fn cycle_area(current: Area, forward: bool, has_nodes: impl Fn(Area) -> bool) -> Option<Area> {
    let len = Area::ORDER.len();
    let ix = Area::ORDER.iter().position(|a| *a == current).unwrap_or(0);
    (1..len)
        .map(|k| if forward { Area::ORDER[(ix + k) % len] } else { Area::ORDER[(ix + len - k) % len] })
        .find(|a| has_nodes(*a))
}

/// Vicino nel kanban. `columns`: le colonne in ordine di fase, ognuna con le sue chat
/// dall'alto in basso (id, centro verticale). Su/giu' nella colonna senza giro ai bordi;
/// sinistra/destra alla colonna adiacente non vuota, alla chat con il centro verticale
/// piu' vicino (a parita' la piu' in alto).
pub fn kanban_neighbor(columns: &[Vec<(NodeId, f32)>], current: NodeId, dir: Direction) -> Option<NodeId> {
    let (col, row) = columns
        .iter()
        .enumerate()
        .find_map(|(c, chats)| chats.iter().position(|(id, _)| *id == current).map(|r| (c, r)))?;
    let chats = &columns[col];
    match dir {
        Direction::Up => row.checked_sub(1).map(|r| chats[r].0),
        Direction::Down => chats.get(row + 1).map(|c| c.0),
        Direction::Left | Direction::Right => {
            let y = chats[row].1;
            let target = if dir == Direction::Right {
                columns[col + 1..].iter().find(|c| !c.is_empty())
            } else {
                columns[..col].iter().rev().find(|c| !c.is_empty())
            }?;
            // `min_by` tiene il primo a parita': la chat piu' in alto
            target.iter().min_by(|a, b| (a.1 - y).abs().total_cmp(&(b.1 - y).abs())).map(|c| c.0)
        }
    }
}

/// Dove va il focus chiudendo la chat `closed`, scelto prima della chiusura sull'ordine
/// delle colonne com'era: la successiva nella colonna, altrimenti la precedente; se era
/// l'unica, la piu' vicina in altezza nella colonna non vuota piu' vicina, prima a destra
/// poi a sinistra; `None` se non resta nessuna chat. Per le Automazioni le fasce sono
/// un'unica colonna.
pub fn neighbor_after_close(columns: &[Vec<(NodeId, f32)>], closed: NodeId) -> Option<NodeId> {
    // giu'/su trovano solo la successiva/precedente; destra/sinistra servono solo se era
    // l'unica della colonna
    [Direction::Down, Direction::Up, Direction::Right, Direction::Left]
        .into_iter()
        .find_map(|dir| kanban_neighbor(columns, closed, dir))
}

/// Vicino nelle Automazioni: su/giu' la fascia precedente/successiva (in ordine di
/// layout); sinistra/destra niente.
pub fn band_neighbor(bands: &[NodeId], current: NodeId, dir: Direction) -> Option<NodeId> {
    let ix = bands.iter().position(|id| *id == current)?;
    match dir {
        Direction::Up => ix.checked_sub(1).map(|i| bands[i]),
        Direction::Down => bands.get(ix + 1).copied(),
        Direction::Left | Direction::Right => None,
    }
}

/// Vicino fra i nodi liberi (id, centro): candidati i nodi il cui centro sta nel
/// semipiano della direzione; vince il punteggio piu' basso, distanza lungo l'asse
/// + 2 × distanza trasversale (a parita' il primo di `nodes`).
pub fn directional_neighbor(nodes: &[(NodeId, Point<f32>)], current: NodeId, dir: Direction) -> Option<NodeId> {
    let from = nodes.iter().find(|(id, _)| *id == current)?.1;
    nodes
        .iter()
        .filter(|(id, _)| *id != current)
        .filter_map(|(id, c)| {
            let (dx, dy) = (c.x - from.x, c.y - from.y);
            let (along, across) = match dir {
                Direction::Right => (dx, dy.abs()),
                Direction::Left => (-dx, dy.abs()),
                Direction::Down => (dy, dx.abs()),
                Direction::Up => (-dy, dx.abs()),
            };
            (along > 0.).then_some((*id, along + 2. * across))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _)| id)
}

/// Il nodo con il centro piu' vicino a `p` (a parita' il primo di `nodes`).
pub fn nearest_to(nodes: &[(NodeId, Point<f32>)], p: Point<f32>) -> Option<NodeId> {
    let dist = |c: &Point<f32>| (c.x - p.x).powi(2) + (c.y - p.y).powi(2);
    nodes.iter().min_by(|a, b| dist(&a.1).total_cmp(&dist(&b.1))).map(|(id, _)| *id)
}

/// Pan (px locali) che centra il rettangolo `origin`/`size` (coordinate canvas) nella
/// vista `view` senza cambiare zoom; `None` se e' gia' interamente visibile.
pub fn center_pan(v: Viewport, origin: Point<f32>, size: Size<f32>, view: Size<f32>) -> Option<Point<f32>> {
    let a = v.to_screen(origin);
    let b = v.to_screen(point(origin.x + size.width, origin.y + size.height));
    if a.x >= 0. && a.y >= 0. && b.x <= view.width && b.y <= view.height {
        return None;
    }
    Some(point(view.width / 2. - (a.x + b.x) / 2., view.height / 2. - (a.y + b.y) / 2.))
}

/// Uscita progressiva (ease-out) dell'animazione: `t` in 0..=1.
fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

impl CanvasView {
    /// Nodo da cui parte la navigazione: quello con il focus, altrimenti il selezionato
    /// in primo piano.
    pub(super) fn nav_current(&self, window: &Window, cx: &App) -> Option<NodeId> {
        let frames = self.model.frames();
        frames
            .iter()
            .rev()
            .find(|f| self.contents.get(&f.id).is_some_and(|c| c.focus_handle(cx).contains_focused(window, cx)))
            .or_else(|| frames.iter().rev().find(|f| self.model.is_selected(f.id)))
            .map(|f| f.id)
    }

    /// Area di un nodo: il contenitore e le sue chat stanno nella sua area.
    fn area_of(&self, id: NodeId) -> Area {
        match self.model.frame(id).map(|f| (f.kind, f.dock)) {
            Some((NodeKind::Kanban, _) | (_, Some(DockKind::Kanban))) => Area::Kanban,
            Some((NodeKind::Automations, _) | (_, Some(DockKind::Automations))) => Area::Automations,
            _ => Area::Free,
        }
    }

    /// Colonne del kanban in ordine di fase, ognuna dall'alto (id, centro verticale).
    fn kanban_columns(&self, cx: &App) -> Vec<Vec<(NodeId, f32)>> {
        let mut columns = vec![Vec::new(); superzed_sessions::Phase::ALL.len()];
        let mut chats = self.docked_chats(cx);
        chats.sort_by_key(|(id, _)| *id);
        for (id, phase) in chats {
            let col = superzed_sessions::Phase::ALL.iter().position(|p| *p == phase).unwrap_or(0);
            if let Some(f) = self.model.frame(id) {
                columns[col].push((id, f.origin.y + f.size.height / 2.));
            }
        }
        for column in &mut columns {
            column.sort_by(|a, b| a.1.total_cmp(&b.1));
        }
        columns
    }

    /// Nodi liberi (id, centro), in ordine di id: tutti tranne i contenitori e le chat
    /// agganciate.
    fn free_nodes(&self) -> Vec<(NodeId, Point<f32>)> {
        let mut nodes: Vec<_> = self
            .model
            .frames()
            .iter()
            .filter(|f| f.dock.is_none() && !matches!(f.kind, NodeKind::Kanban | NodeKind::Automations))
            .map(|f| (f.id, point(f.origin.x + f.size.width / 2., f.origin.y + f.size.height / 2.)))
            .collect();
        nodes.sort_by_key(|(id, _)| *id);
        nodes
    }

    /// Nodi di un'area su cui si puo' andare (il contenitore no).
    fn area_members(&self, area: Area, cx: &App) -> Vec<NodeId> {
        match area {
            Area::Kanban => self.kanban_columns(cx).into_iter().flatten().map(|(id, _)| id).collect(),
            Area::Automations => self.dock_members(DockKind::Automations),
            Area::Free => self.free_nodes().into_iter().map(|(id, _)| id).collect(),
        }
    }

    /// Il contenitore dell'area (nessuno per i nodi liberi), se e' sul canvas.
    fn area_container(&self, area: Area) -> Option<NodeId> {
        match area {
            Area::Kanban => self.container_node(DockKind::Kanban),
            Area::Automations => self.container_node(DockKind::Automations),
            Area::Free => None,
        }
    }

    /// Se l'area esiste nel ciclo (Task 14): sue chat, o il contenitore su canvas per
    /// kanban/Automazioni anche a zero chat.
    fn has_area(&self, area: Area, cx: &App) -> bool {
        area_exists(area, self.area_members(area, cx).len(), self.area_container(area).is_some())
    }

    /// Dove si arriva entrando in un'area: l'ultimo nodo con il focus li' (se c'e'
    /// ancora), altrimenti la prima chat del kanban, la prima fascia, il contenitore
    /// se non ha chat (Task 14), o il nodo libero piu' vicino al centro della viewport.
    fn area_entry(&self, area: Area, cx: &App) -> Option<NodeId> {
        let members = self.area_members(area, cx);
        if let Some(last) = self.nav_last.get(&area)
            && members.contains(last)
        {
            return Some(*last);
        }
        match area {
            Area::Kanban | Area::Automations => members.first().copied().or_else(|| self.area_container(area)),
            Area::Free => nearest_to(&self.free_nodes(), self.viewport.to_canvas(self.local_center())),
        }
    }

    /// cmd+freccia: il vicino nell'area del nodo corrente. Senza un nodo dell'area (niente
    /// focus, o il focus sul contenitore) si entra nell'area.
    pub(crate) fn focus_chat(&mut self, dir: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.nav_current(window, cx);
        let area = current.map_or_else(|| default_area(|a| self.has_area(a, cx)), |id| self.area_of(id));
        let target = match current.filter(|id| self.area_members(area, cx).contains(id)) {
            None => self.area_entry(area, cx),
            Some(id) => match area {
                Area::Kanban => kanban_neighbor(&self.kanban_columns(cx), id, dir),
                Area::Automations => band_neighbor(&self.dock_members(DockKind::Automations), id, dir),
                Area::Free => directional_neighbor(&self.free_nodes(), id, dir),
            },
        };
        if let Some(target) = target {
            self.land_on(target, window, cx);
        }
    }

    /// cmd-alt-destra/sinistra: l'area successiva/precedente che esiste.
    pub(crate) fn focus_area(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.nav_current(window, cx);
        let area = current.map_or_else(|| default_area(|a| self.has_area(a, cx)), |id| self.area_of(id));
        // si ricorda da dove si esce, anche se ci si era arrivati con il mouse
        if let Some(id) = current
            && self.area_members(area, cx).contains(&id)
        {
            self.nav_last.insert(area, id);
        }
        let Some(next) = cycle_area(area, forward, |a| self.has_area(a, cx)) else {
            return;
        };
        if let Some(target) = self.area_entry(next, cx) {
            self.land_on(target, window, cx);
        }
    }

    /// Chat su cui atterrare dopo aver chiuso `ids` (da calcolare prima della chiusura):
    /// solo per una singola chat del kanban o delle Automazioni con il focus o
    /// selezionata; `None` negli altri casi (focus al canvas come prima).
    pub(super) fn landing_after_close(&self, ids: &[NodeId], window: &Window, cx: &App) -> Option<NodeId> {
        let [id] = ids else {
            return None;
        };
        if self.nav_current(window, cx) != Some(*id) && !self.model.is_selected(*id) {
            return None;
        }
        let columns = match self.area_of(*id) {
            Area::Kanban => self.kanban_columns(cx),
            Area::Automations => {
                vec![self.dock_members(DockKind::Automations).into_iter().map(|id| (id, 0.)).collect()]
            }
            Area::Free => return None,
        };
        neighbor_after_close(&columns, *id)
    }

    /// Destinazione della navigazione: una chat o un terminale prende il focus del
    /// terminale, un altro nodo resta selezionato con il focus al canvas; se non e'
    /// interamente visibile la viewport si sposta per centrarlo (zoom invariato).
    pub(super) fn land_on(&mut self, id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        self.nav_last.insert(self.area_of(id), id);
        let terminal = self.model.frame(id).is_some_and(|f| f.kind.is_terminal()) && self.contents.contains_key(&id);
        if terminal {
            self.focus_node(id, window, cx);
        } else {
            self.model.select(id, false);
            self.bring_to_front(id);
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
        let Some((origin, size)) = self.model.frame(id).map(|f| (f.origin, f.size)) else {
            return;
        };
        let view = self.canvas_bounds.get().size;
        let view = gpui::size(f32::from(view.width), f32::from(view.height));
        if let Some(delta) = center_pan(self.viewport, origin, size, view) {
            self.animate_pan(delta, cx);
        }
    }

    /// Pan della viewport di `delta` px: animato, oppure subito con `reduce_motion`. Ogni
    /// passo aggiunge la sua parte (un pan dell'utente nel frattempo si somma).
    pub(super) fn animate_pan(&mut self, delta: Point<f32>, cx: &mut Context<Self>) {
        if cx.reduce_motion() {
            self.nav_pan = Task::ready(());
            self.viewport.pan(delta);
            self.viewport_changed(cx);
            return;
        }
        let step = Duration::from_millis(NAV_PAN_MS) / NAV_PAN_STEPS;
        self.nav_pan = cx.spawn(async move |this, cx| {
            let mut done = 0.;
            for i in 1..=NAV_PAN_STEPS {
                cx.background_executor().timer(step).await;
                let t = ease_out(i as f32 / NAV_PAN_STEPS as f32);
                let part = t - done;
                done = t;
                let moved = this.update(cx, |c, cx| {
                    c.viewport.pan(point(delta.x * part, delta.y * part));
                    c.viewport_changed(cx);
                });
                if moved.is_err() {
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::tests::{close_all, kanban_with_claude_chats, redraw, zsh_spec};
    use super::*;
    use crate::test_support::{RecordingContent, init_test};
    use gpui::{Modifiers, ScrollDelta, TestAppContext, TouchPhase, VisualTestContext, WeakEntity};
    use superzed_sessions::Phase;

    const K: Area = Area::Kanban;
    const A: Area = Area::Automations;
    const F: Area = Area::Free;

    fn n(id: u64) -> NodeId {
        NodeId(id)
    }

    #[test]
    fn default_area_is_the_first_non_empty_one_in_order() {
        assert_eq!(default_area(|_| true), K, "kanban ha nodi: si parte li'");
        assert_eq!(default_area(|a| a != K), A, "kanban vuoto: si parte dalle Automazioni");
        assert_eq!(default_area(|a| a == F), F, "solo i nodi liberi: si parte li'");
        assert_eq!(default_area(|_| false), K, "tutte vuote: kanban di default (nessun effetto)");
    }

    // Task 14: un'area con il contenitore sul canvas esiste anche a zero chat; i nodi
    // liberi non hanno un contenitore, contano solo le chat.
    #[test]
    fn area_exists_counts_the_container_even_without_chats() {
        assert!(area_exists(A, 0, true), "Automazioni: contenitore sul canvas ma 0 chat");
        assert!(!area_exists(A, 0, false), "Automazioni: ne' chat ne' contenitore");
        assert!(area_exists(K, 3, false), "kanban con chat: esiste comunque");
        assert!(area_exists(K, 3, true), "kanban con chat e contenitore: esiste");
        assert!(!area_exists(F, 0, true), "i nodi liberi non hanno un contenitore: il flag non conta");
        assert!(area_exists(F, 2, false), "i nodi liberi contano solo le chat/i nodi");
    }

    // Task 14: cycle_area/default_area con un has_nodes che riflette la nuova regola
    // (chat, o contenitore per kanban/Automazioni) includono l'area vuota col
    // contenitore e continuano a saltare quella davvero assente.
    #[test]
    fn cycle_and_default_area_include_a_container_only_area() {
        // kanban con chat; Automazioni: contenitore ma 0 chat; liberi vuoti
        let with_container = |a: Area| area_exists(a, if a == K { 3 } else { 0 }, a == A);
        assert_eq!(default_area(with_container), K);
        assert_eq!(cycle_area(K, true, with_container), Some(A), "le Automazioni contano anche senza chat");
        assert_eq!(cycle_area(A, true, with_container), Some(K), "i liberi restano vuoti: saltati, si torna al kanban");
        // stessa scena ma senza il contenitore delle Automazioni: tornano a essere saltate
        let without_container = |a: Area| area_exists(a, if a == K { 3 } else { 0 }, false);
        assert_eq!(cycle_area(K, true, without_container), None, "nessun'altra area esiste: niente");
    }

    #[test]
    fn areas_cycle_skipping_the_empty_ones() {
        let all = |_: Area| true;
        assert_eq!(cycle_area(K, true, all), Some(A));
        assert_eq!(cycle_area(A, true, all), Some(F));
        assert_eq!(cycle_area(F, true, all), Some(K), "si torna al kanban");
        assert_eq!(cycle_area(K, false, all), Some(F));
        assert_eq!(cycle_area(F, false, all), Some(A));
        let no_automations = |a: Area| a != A;
        assert_eq!(cycle_area(K, true, no_automations), Some(F), "Automazioni vuote: saltate");
        assert_eq!(cycle_area(F, false, no_automations), Some(K));
        assert_eq!(cycle_area(K, true, |a| a == K), None, "solo il kanban ha nodi: niente");
        assert_eq!(cycle_area(K, true, |_| false), None);
        // l'area corrente vuota (nessun focus: kanban) passa comunque alle altre
        assert_eq!(cycle_area(K, true, |a| a == F), Some(F));
        assert_eq!(cycle_area(K, false, |a| a == A), Some(A));
    }

    // New: 1 (y 100), 2 (y 700); Brainstorm vuota; Spec: 3 (y 100), 4 (y 700), 5 (y 1300);
    // Plan vuota; Implement: 6 (y 1300); Done vuota
    fn columns() -> Vec<Vec<(NodeId, f32)>> {
        vec![
            vec![(n(1), 100.), (n(2), 700.)],
            vec![],
            vec![(n(3), 100.), (n(4), 700.), (n(5), 1300.)],
            vec![],
            vec![(n(6), 1300.)],
            vec![],
        ]
    }

    #[test]
    fn kanban_up_down_stay_in_the_column_without_wrapping() {
        let c = columns();
        assert_eq!(kanban_neighbor(&c, n(1), Direction::Down), Some(n(2)));
        assert_eq!(kanban_neighbor(&c, n(2), Direction::Up), Some(n(1)));
        assert_eq!(kanban_neighbor(&c, n(2), Direction::Down), None, "in fondo: niente giro");
        assert_eq!(kanban_neighbor(&c, n(3), Direction::Up), None, "in cima: niente giro");
        assert_eq!(kanban_neighbor(&c, n(4), Direction::Down), Some(n(5)));
        assert_eq!(kanban_neighbor(&c, n(99), Direction::Down), None, "non nel kanban");
    }

    #[test]
    fn kanban_left_right_skip_empty_columns_to_the_nearest_height() {
        let c = columns();
        assert_eq!(kanban_neighbor(&c, n(2), Direction::Right), Some(n(4)), "salta Brainstorm, stessa altezza");
        assert_eq!(kanban_neighbor(&c, n(1), Direction::Right), Some(n(3)));
        assert_eq!(kanban_neighbor(&c, n(5), Direction::Left), Some(n(2)), "la piu' vicina in altezza");
        assert_eq!(kanban_neighbor(&c, n(3), Direction::Right), Some(n(6)), "salta Plan; l'unica chat");
        assert_eq!(kanban_neighbor(&c, n(6), Direction::Left), Some(n(5)));
        assert_eq!(kanban_neighbor(&c, n(6), Direction::Right), None, "a destra solo colonne vuote");
        assert_eq!(kanban_neighbor(&c, n(1), Direction::Left), None, "prima colonna");
        // a parita' di distanza la piu' in alto
        let tie = vec![vec![(n(1), 400.)], vec![(n(2), 100.), (n(3), 700.)]];
        assert_eq!(kanban_neighbor(&tie, n(1), Direction::Right), Some(n(2)));
    }

    #[test]
    fn after_close_the_next_chat_then_the_previous_then_the_nearest_column() {
        let c = columns();
        assert_eq!(neighbor_after_close(&c, n(4)), Some(n(5)), "in mezzo: la successiva");
        assert_eq!(neighbor_after_close(&c, n(1)), Some(n(2)), "in cima: la successiva");
        assert_eq!(neighbor_after_close(&c, n(5)), Some(n(4)), "ultima: la precedente");
        assert_eq!(neighbor_after_close(&c, n(2)), Some(n(1)));
        // unica di Implement (Plan e Done vuote accanto): a destra niente, a sinistra
        // Spec, la piu' vicina in altezza
        assert_eq!(neighbor_after_close(&c, n(6)), Some(n(5)), "unica: colonna non vuota a sinistra");
        // unica con colonne non vuote da entrambi i lati: prima a destra
        let both = vec![vec![(n(1), 100.)], vec![], vec![(n(2), 700.)], vec![], vec![(n(3), 100.), (n(4), 700.)]];
        assert_eq!(neighbor_after_close(&both, n(2)), Some(n(4)), "prima a destra, alla stessa altezza");
        assert_eq!(neighbor_after_close(&[vec![], vec![(n(7), 100.)], vec![]], n(7)), None, "kanban vuoto");
        assert_eq!(neighbor_after_close(&c, n(99)), None, "non nel kanban");
        // Automazioni: un'unica colonna di fasce
        let bands = [vec![(n(3), 0.), (n(7), 0.), (n(9), 0.)]];
        assert_eq!(neighbor_after_close(&bands, n(7)), Some(n(9)));
        assert_eq!(neighbor_after_close(&bands, n(9)), Some(n(7)));
        assert_eq!(neighbor_after_close(&[vec![(n(3), 0.)]], n(3)), None, "ultima fascia: canvas");
    }

    #[test]
    fn automations_up_down_move_between_bands() {
        let bands = [n(3), n(7), n(9)];
        assert_eq!(band_neighbor(&bands, n(7), Direction::Down), Some(n(9)));
        assert_eq!(band_neighbor(&bands, n(7), Direction::Up), Some(n(3)));
        assert_eq!(band_neighbor(&bands, n(9), Direction::Down), None);
        assert_eq!(band_neighbor(&bands, n(3), Direction::Up), None);
        assert_eq!(band_neighbor(&bands, n(7), Direction::Left), None, "sinistra/destra niente");
        assert_eq!(band_neighbor(&bands, n(7), Direction::Right), None);
        assert_eq!(band_neighbor(&bands, n(1), Direction::Down), None);
    }

    #[test]
    fn free_nodes_pick_by_axis_plus_twice_the_cross_distance() {
        let nodes = [
            (n(1), point(0., 0.)),
            // a destra: 2 e' piu' lontano sull'asse ma allineato, 3 vicino ma spostato
            (n(2), point(500., 0.)),   // 500 + 0
            (n(3), point(200., 200.)), // 200 + 400
            // sopra/sotto
            (n(4), point(100., -300.)), // su: 300 + 200
            (n(5), point(0., 400.)),    // giu': 400 + 0
            (n(6), point(-30., 300.)),  // giu': 300 + 60
        ];
        assert_eq!(directional_neighbor(&nodes, n(1), Direction::Right), Some(n(2)));
        assert_eq!(directional_neighbor(&nodes, n(1), Direction::Up), Some(n(4)));
        assert_eq!(directional_neighbor(&nodes, n(1), Direction::Down), Some(n(6)), "400 > 300 + 2 × 30");
        assert_eq!(directional_neighbor(&nodes, n(1), Direction::Left), Some(n(6)), "solo 6 ha il centro a sinistra");
        assert_eq!(directional_neighbor(&nodes, n(2), Direction::Right), None, "nessun candidato");
        // centro sulla linea (distanza lungo l'asse zero): non e' nel semipiano
        let line = [(n(1), point(0., 0.)), (n(2), point(0., 300.))];
        assert_eq!(directional_neighbor(&line, n(1), Direction::Right), None);
        assert_eq!(directional_neighbor(&line, n(9), Direction::Right), None, "nodo corrente sparito");
    }

    #[test]
    fn nearest_to_a_point() {
        let nodes = [(n(1), point(0., 0.)), (n(2), point(100., 100.)), (n(3), point(90., 110.))];
        assert_eq!(nearest_to(&nodes, point(95., 104.)), Some(n(2)));
        assert_eq!(nearest_to(&nodes, point(10., -5.)), Some(n(1)));
        assert_eq!(nearest_to(&nodes, point(89., 111.)), Some(n(3)));
        assert_eq!(nearest_to(&[], point(0., 0.)), None);
    }

    #[test]
    fn center_pan_only_when_not_fully_visible() {
        let view = size(1000., 800.);
        let v = Viewport { offset: point(0., 0.), zoom: 0.5 };
        // a zoom 0,5 il nodo (100,100)-(500,400) sta a (50,50)-(250,200): visibile
        assert_eq!(center_pan(v, point(100., 100.), size(400., 300.), view), None);
        // (1800,100) → schermo (900,50)-(1100,200): esce a destra; centro → (500,400)
        let pan = center_pan(v, point(1800., 100.), size(400., 300.), view).unwrap();
        assert_eq!(pan, point(500. - 1000., 400. - 125.));
        // piu' grande della vista: si centra comunque
        let big = center_pan(Viewport::default(), point(0., 0.), size(2000., 100.), view).unwrap();
        assert_eq!(big, point(500. - 1000., 400. - 50.));
    }

    // ----- GPUI: tasti del keymap vero (nel terminale i tasti sono azioni del keymap) -----

    pub(in crate::canvas_view) fn bind_real_keymap(cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            let keymap = include_str!("../../../../assets/keymaps/default-macos.json");
            let bindings = match settings::KeymapFile::load(keymap, cx) {
                settings::KeymapFileLoadResult::Success { key_bindings }
                | settings::KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
                settings::KeymapFileLoadResult::JsonParseFailure { error } => panic!("keymap: {error}"),
            };
            cx.bind_keys(bindings);
        });
    }

    fn terminal_focused(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> bool {
        let view = canvas.read_with(cx, |c, _| c.terminals.get(&id).map(|(v, _)| v.clone()).unwrap());
        cx.update(|window, cx| view.focus_handle(cx).is_focused(window))
    }

    fn selected(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) -> Vec<NodeId> {
        canvas.read_with(cx, |c, _| c.model().selection().iter().copied().collect())
    }

    fn focus_chat(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) {
        canvas.update_in(cx, |c, window, cx| c.focus_node(id, window, cx));
        redraw(cx);
        assert!(terminal_focused(canvas, id, cx));
    }

    fn keys(cx: &mut VisualTestContext, keys: &str) {
        cx.simulate_keystrokes(keys);
        cx.executor().advance_clock(Duration::from_millis(NAV_PAN_MS + 50));
        redraw(cx);
    }

    // due chat in Nuove, Brainstorm vuota, due in Spec
    async fn kanban_2_0_2(cx: &mut TestAppContext) -> (Entity<CanvasView>, Vec<NodeId>, &'static mut VisualTestContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(4, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], Phase::New, cx);
            c.dock(chats[1], Phase::New, cx);
            c.dock(chats[2], Phase::Spec, cx);
            c.dock(chats[3], Phase::Spec, cx);
        });
        cx.run_until_parked();
        bind_real_keymap(cx);
        (canvas, chats, cx)
    }

    #[gpui::test]
    async fn cmd_down_in_a_kanban_chat_focuses_the_chat_below(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_2_0_2(cx).await;
        focus_chat(&canvas, chats[0], cx);
        keys(cx, "cmd-down");
        assert!(terminal_focused(&canvas, chats[1], cx), "focus nel terminale della chat sotto");
        assert_eq!(selected(&canvas, cx), vec![chats[1]]);
        keys(cx, "cmd-down");
        assert!(terminal_focused(&canvas, chats[1], cx), "in fondo alla colonna: niente");
        keys(cx, "cmd-up");
        assert!(terminal_focused(&canvas, chats[0], cx));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn cmd_right_goes_to_the_nearest_chat_in_the_next_column(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_2_0_2(cx).await;
        focus_chat(&canvas, chats[1], cx);
        keys(cx, "cmd-right");
        assert!(terminal_focused(&canvas, chats[3], cx), "Brainstorm vuota saltata, seconda riga di Spec");
        keys(cx, "cmd-right");
        assert!(terminal_focused(&canvas, chats[3], cx), "a destra solo colonne vuote: niente");
        keys(cx, "cmd-up cmd-left");
        assert!(terminal_focused(&canvas, chats[0], cx), "prima riga di Nuove");
        close_all(&canvas, cx);
    }

    // ----- Task 13: chiudendo una chat del kanban il focus passa alla vicina -----

    // tre chat in Nuove, una in Spec
    async fn kanban_3_0_1(cx: &mut TestAppContext) -> (Entity<CanvasView>, Vec<NodeId>, &'static mut VisualTestContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(4, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], Phase::New, cx);
            c.dock(chats[1], Phase::New, cx);
            c.dock(chats[2], Phase::New, cx);
            c.dock(chats[3], Phase::Spec, cx);
        });
        cx.run_until_parked();
        bind_real_keymap(cx);
        (canvas, chats, cx)
    }

    // cmd-w sul terminale con il focus, poi la risposta alla conferma
    fn close_with(cx: &mut VisualTestContext, answer: &str) {
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        keys(cx, answer);
        cx.run_until_parked();
    }

    fn exists(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> bool {
        canvas.read_with(cx, |c, _| c.model().frame(id).is_some())
    }

    #[gpui::test]
    async fn closing_a_kanban_chat_focuses_the_next_then_the_previous(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_3_0_1(cx).await;
        focus_chat(&canvas, chats[1], cx);
        close_with(cx, "y");
        assert!(!exists(&canvas, chats[1], cx), "chiusa");
        assert!(terminal_focused(&canvas, chats[2], cx), "in mezzo: focus alla successiva");
        assert_eq!(selected(&canvas, cx), vec![chats[2]]);
        close_with(cx, "y");
        assert!(!exists(&canvas, chats[2], cx));
        assert!(terminal_focused(&canvas, chats[0], cx), "ultima: focus alla precedente");
        close_all(&canvas, cx);
    }

    // la vicina viene anche inquadrata: zoom riportato a 1 e chat centrata nella vista
    #[gpui::test]
    async fn closing_a_kanban_chat_frames_the_next_at_zoom_one(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_3_0_1(cx).await;
        focus_chat(&canvas, chats[1], cx);
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 0.6, cx));
        cx.run_until_parked();
        close_with(cx, "y");
        assert!(terminal_focused(&canvas, chats[2], cx));
        let (zoom, center, view) = canvas.read_with(cx, |c, _| {
            let f = c.model().frame(chats[2]).unwrap();
            let p = c.viewport().to_screen(point(f.origin.x + f.size.width / 2., f.origin.y + f.size.height / 2.));
            let b = c.canvas_bounds.get().size;
            (c.viewport().zoom, p, (f32::from(b.width), f32::from(b.height)))
        });
        assert_eq!(zoom, 1., "zoom resettato");
        assert!((center.x - view.0 / 2.).abs() < 1. && (center.y - view.1 / 2.).abs() < 1., "centrata: {center:?} in {view:?}");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn closing_the_only_chat_of_a_column_focuses_the_nearby_column(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_3_0_1(cx).await;
        focus_chat(&canvas, chats[3], cx);
        close_with(cx, "y");
        assert!(!exists(&canvas, chats[3], cx));
        assert!(terminal_focused(&canvas, chats[0], cx), "Spec vuota: la chat piu' vicina in altezza in Nuove");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn cancelling_the_close_keeps_the_focus(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_3_0_1(cx).await;
        focus_chat(&canvas, chats[1], cx);
        close_with(cx, "n");
        assert!(exists(&canvas, chats[1], cx), "annullata");
        assert!(terminal_focused(&canvas, chats[1], cx), "il focus resta dov'era");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn closing_the_last_kanban_chat_focuses_the_canvas(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], Phase::New, cx));
        cx.run_until_parked();
        bind_real_keymap(cx);
        focus_chat(&canvas, chats[0], cx);
        close_with(cx, "y");
        assert!(!exists(&canvas, chats[0], cx));
        let canvas_focus = canvas.read_with(cx, |c, _| c.focus_handle.clone());
        assert!(cx.update(|window, _| canvas_focus.is_focused(window)), "kanban vuoto: focus al canvas");
        assert_eq!(selected(&canvas, cx), Vec::<NodeId>::new());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn cmd_alt_right_goes_to_automations_and_back(cx: &mut TestAppContext) {
        let (canvas, chats, cx) = kanban_2_0_2(cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.create_automation(
                crate::automation::AutomationDraft { name: "Log".into(), schedule: "4h".into(), prompt: "Leggi".into(), account: None },
                window,
                cx,
            );
        });
        cx.run_until_parked();
        let automation = canvas.read_with(cx, |c, _| *c.automations.keys().next().unwrap());
        focus_chat(&canvas, chats[3], cx);
        keys(cx, "cmd-alt-right");
        assert!(terminal_focused(&canvas, automation, cx), "prima fascia delle Automazioni");
        assert_eq!(selected(&canvas, cx), vec![automation]);
        // nodi liberi vuoti: saltati, si torna all'ultima chat del kanban
        keys(cx, "cmd-alt-right");
        assert!(terminal_focused(&canvas, chats[3], cx), "ultima chat del kanban con il focus");
        keys(cx, "cmd-alt-left");
        assert!(terminal_focused(&canvas, automation, cx), "all'indietro: Automazioni");
        close_all(&canvas, cx);
    }

    // Kanban rimosso (nessun contenitore: davvero assente), Automazioni con una fascia:
    // senza focus ne' selezione cmd-freccia e il ciclo di area devono entrare dalle
    // Automazioni, non restare fermi su un kanban che non esiste.
    async fn empty_kanban_with_one_automation(
        cx: &mut TestAppContext,
    ) -> (Entity<CanvasView>, NodeId, &'static mut VisualTestContext) {
        let (canvas, kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        canvas.update(cx, |c, cx| c.remove_nodes(&[kanban], cx));
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.create_automation(
                crate::automation::AutomationDraft { name: "Log".into(), schedule: "4h".into(), prompt: "Leggi".into(), account: None },
                window,
                cx,
            );
        });
        cx.run_until_parked();
        let automation = canvas.read_with(cx, |c, _| *c.automations.keys().next().unwrap());
        bind_real_keymap(cx);
        // niente focus su un contenuto e niente selezione: solo il focus del canvas
        canvas.update_in(cx, |c, window, cx| {
            window.focus(&c.focus_handle, cx);
            c.model.clear_selection();
        });
        redraw(cx);
        (canvas, automation, cx)
    }

    #[gpui::test]
    async fn cmd_right_with_nothing_focused_enters_automations_when_the_kanban_is_absent(cx: &mut TestAppContext) {
        let (canvas, automation, cx) = empty_kanban_with_one_automation(cx).await;
        keys(cx, "cmd-right");
        assert!(terminal_focused(&canvas, automation, cx), "kanban assente: si entra dalle Automazioni");
        close_all(&canvas, cx);
    }

    // Con niente a fuoco l'area di default (le Automazioni, kanban assente) e' gia'
    // "quella corrente": FocusNextArea cicla via da li' come farebbe da un nodo vero in
    // quell'area, quindi con una sola area esistente non c'e' dove andare (niente, come
    // gia' capita quando l'unica area con nodi e' quella con il focus).
    #[gpui::test]
    async fn cmd_alt_right_with_nothing_focused_is_a_noop_when_automations_is_the_only_existing_area(
        cx: &mut TestAppContext,
    ) {
        let (canvas, automation, cx) = empty_kanban_with_one_automation(cx).await;
        keys(cx, "cmd-alt-right");
        assert!(!terminal_focused(&canvas, automation, cx), "nessun'altra area esiste: niente da raggiungere");
        assert_eq!(selected(&canvas, cx), Vec::<NodeId>::new());
        close_all(&canvas, cx);
    }

    // kanban assente, Automazioni E nodi liberi non vuoti: il ciclo deve partire dalle
    // Automazioni (la prima area esistente), non dal kanban; FocusNextArea da li' va oltre,
    // ai nodi liberi (differenza osservabile solo con due aree esistenti in gara).
    #[gpui::test]
    async fn cmd_alt_right_with_nothing_focused_cycles_from_the_default_area_not_from_the_kanban(
        cx: &mut TestAppContext,
    ) {
        let (canvas, automation, cx) = empty_kanban_with_one_automation(cx).await;
        probe(&canvas, 900, 100., 100., cx);
        redraw(cx);
        canvas.update_in(cx, |c, window, cx| {
            window.focus(&c.focus_handle, cx);
            c.model.clear_selection();
        });
        redraw(cx);
        keys(cx, "cmd-alt-right");
        assert_eq!(
            selected(&canvas, cx),
            vec![NodeId(900)],
            "il ciclo parte dalle Automazioni (area di default) e va al prossimo: i nodi liberi"
        );
        assert!(!terminal_focused(&canvas, automation, cx));
        close_all(&canvas, cx);
    }

    // Task 14: il kanban ha una chat (esiste), le Automazioni hanno il contenitore ma
    // zero chat: il ciclo non le salta piu', atterra sul contenitore selezionato con il
    // focus al canvas; da li' senza nodi liberi si torna al kanban, con nodi liberi ci si
    // va, e cmd+freccia dentro l'area vuota non fa nulla.
    async fn kanban_chat_and_empty_automations_container(
        cx: &mut TestAppContext,
    ) -> (Entity<CanvasView>, NodeId, NodeId, &'static mut VisualTestContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], Phase::New, cx));
        let automations = canvas.update(cx, |c, cx| c.ensure_container(DockKind::Automations, cx));
        cx.run_until_parked();
        bind_real_keymap(cx);
        (canvas, chats[0], automations, cx)
    }

    #[gpui::test]
    async fn cmd_alt_right_lands_on_the_empty_automations_container_and_back_to_the_kanban(
        cx: &mut TestAppContext,
    ) {
        let (canvas, chat, automations, cx) = kanban_chat_and_empty_automations_container(cx).await;
        focus_chat(&canvas, chat, cx);
        keys(cx, "cmd-alt-right");
        assert_eq!(selected(&canvas, cx), vec![automations], "il contenitore vuoto non e' saltato");
        let canvas_focus = canvas.read_with(cx, |c, _| c.focus_handle.clone());
        assert!(cx.update(|window, _| canvas_focus.is_focused(window)), "focus al canvas, non a un terminale");
        // cmd+freccia dentro l'area vuota (il contenitore selezionato): niente
        keys(cx, "cmd-right");
        assert_eq!(selected(&canvas, cx), vec![automations], "nessuna chat nell'area: niente cambia");
        // nessun nodo libero: il ciclo torna al kanban
        keys(cx, "cmd-alt-right");
        assert!(terminal_focused(&canvas, chat, cx), "nessun nodo libero: si torna alla chat del kanban");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn cmd_alt_right_from_the_empty_automations_container_goes_to_free_nodes_when_present(
        cx: &mut TestAppContext,
    ) {
        let (canvas, chat, automations, cx) = kanban_chat_and_empty_automations_container(cx).await;
        focus_chat(&canvas, chat, cx);
        keys(cx, "cmd-alt-right");
        assert_eq!(selected(&canvas, cx), vec![automations]);
        probe(&canvas, 900, 100., 100., cx);
        redraw(cx);
        keys(cx, "cmd-alt-right");
        assert_eq!(selected(&canvas, cx), vec![NodeId(900)], "nodi liberi presenti: il ciclo continua li'");
        close_all(&canvas, cx);
    }

    fn probe(canvas: &Entity<CanvasView>, id: u64, x: f32, y: f32, cx: &mut VisualTestContext) -> RecordingContent {
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        let probe = RecordingContent { view: content.view.clone(), zoom: content.zoom.clone(), closed: content.closed.clone() };
        let frame = NodeFrame {
            id: NodeId(id),
            kind: NodeKind::Note,
            origin: point(x, y),
            size: size(200., 150.),
            title: format!("n{id}"),
            color: None,
            group: None,
            dock: None,
            golden: false,
        };
        canvas.update(cx, |c, cx| c.insert_restored_node(frame, Box::new(content), cx));
        redraw(cx);
        probe
    }

    fn centered(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> bool {
        canvas.read_with(cx, |c, _| {
            let f = c.model().frame(id).unwrap();
            let p = c.viewport().to_screen(point(f.origin.x + f.size.width / 2., f.origin.y + f.size.height / 2.));
            let center = c.local_center();
            (p.x - center.x).abs() < 0.5 && (p.y - center.y).abs() < 0.5
        })
    }

    #[gpui::test]
    fn the_viewport_pans_to_an_off_screen_destination_keeping_the_zoom(cx: &mut TestAppContext) {
        init_test(cx);
        let (canvas, cx) = cx.add_window_view(|window, cx| CanvasView::new(WeakEntity::new_invalid(), window, cx));
        cx.simulate_resize(size(px(1000.), px(800.)));
        redraw(cx);
        bind_real_keymap(cx);
        probe(&canvas, 1, 100., 100., cx);
        probe(&canvas, 2, 3000., 150., cx);
        probe(&canvas, 3, 400., 150., cx);
        canvas.update_in(cx, |c, window, cx| {
            c.viewport.zoom = 0.8;
            c.model.select(NodeId(1), false);
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        // la vicina e' visibile: niente pan; nodo non terminale: selezionato, focus al canvas
        let before = canvas.read_with(cx, |c, _| c.viewport());
        keys(cx, "cmd-right");
        assert_eq!(selected(&canvas, cx), vec![NodeId(3)]);
        let canvas_focus = canvas.read_with(cx, |c, _| c.focus_handle.clone());
        assert!(cx.update(|window, _| canvas_focus.is_focused(window)), "focus al canvas");
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport()), before, "gia' visibile: la viewport non si muove");
        // fuori schermo: pan animato fino al centro, zoom invariato
        cx.simulate_keystrokes("cmd-right");
        assert_eq!(selected(&canvas, cx), vec![NodeId(2)]);
        cx.executor().advance_clock(Duration::from_millis(NAV_PAN_MS + 50));
        redraw(cx);
        assert!(centered(&canvas, NodeId(2), cx), "centrata");
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().zoom), 0.8, "zoom invariato");
        // con reduce motion lo spostamento e' immediato
        cx.update(|_, cx| cx.set_reduce_motion(true));
        cx.simulate_keystrokes("cmd-left");
        assert_eq!(selected(&canvas, cx), vec![NodeId(3)]);
        assert!(centered(&canvas, NodeId(3), cx), "subito centrata, senza animazione");
    }

    // ----- Fix wave 3, punto 1: zoom/fit e pan dell'utente cancellano l'animazione di
    // navigazione in corso, invece di sommarcisi per il resto dei suoi 160 ms. -----

    // avvia la navigazione verso il nodo 2 (fuori schermo): l'animazione di pan resta
    // pendente (nessun passo e' ancora girato, l'executor non e' stato fatto avanzare).
    fn start_pending_nav_pan(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("cmd-right");
        assert_eq!(selected(canvas, cx), vec![NodeId(2)], "la navigazione e' partita verso il nodo fuori schermo");
    }

    // dopo l'azione che deve cancellare il pan pendente, l'ulteriore avanzare dell'orologio
    // (fino a dopo la fine dei 160 ms dell'animazione) non deve spostare piu' la viewport.
    fn assert_pan_animation_does_not_resume(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) {
        let expected = canvas.read_with(cx, |c, _| c.viewport());
        cx.executor().advance_clock(Duration::from_millis(NAV_PAN_MS + 50));
        redraw(cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport()), expected, "il pan di navigazione pendente non deve riprendere");
    }

    // stessa scena delle due chat probe (una vicina, una a x=3000 fuori schermo) usata da
    // `the_viewport_pans_to_an_off_screen_destination_keeping_the_zoom` sopra; qui la
    // funzione non puo' farsi restituire `cx` (l'unico modo per ottenere il
    // `&'static mut VisualTestContext` di `add_window_view` e' chiamarlo direttamente nel
    // corpo del test), quindi il setup e' ripetuto in ognuno dei test sotto.
    #[gpui::test]
    fn zoom_by_cancels_a_pending_navigation_pan(cx: &mut TestAppContext) {
        init_test(cx);
        let (canvas, cx) = cx.add_window_view(|window, cx| CanvasView::new(WeakEntity::new_invalid(), window, cx));
        cx.simulate_resize(size(px(1000.), px(800.)));
        redraw(cx);
        bind_real_keymap(cx);
        probe(&canvas, 1, 100., 100., cx);
        probe(&canvas, 2, 3000., 150., cx);
        canvas.update_in(cx, |c, window, cx| {
            c.viewport.zoom = 0.8;
            c.model.select(NodeId(1), false);
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        start_pending_nav_pan(&canvas, cx);
        canvas.update(cx, |c, cx| c.zoom_by(point(500., 400.), 1.1, cx));
        assert_pan_animation_does_not_resume(&canvas, cx);
    }

    #[gpui::test]
    fn zoom_to_rect_cancels_a_pending_navigation_pan(cx: &mut TestAppContext) {
        init_test(cx);
        let (canvas, cx) = cx.add_window_view(|window, cx| CanvasView::new(WeakEntity::new_invalid(), window, cx));
        cx.simulate_resize(size(px(1000.), px(800.)));
        redraw(cx);
        bind_real_keymap(cx);
        probe(&canvas, 1, 100., 100., cx);
        probe(&canvas, 2, 3000., 150., cx);
        canvas.update_in(cx, |c, window, cx| {
            c.viewport.zoom = 0.8;
            c.model.select(NodeId(1), false);
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        start_pending_nav_pan(&canvas, cx);
        canvas.update(cx, |c, cx| c.zoom_to_rect(point(0., 0.), size(200., 150.), cx));
        assert_pan_animation_does_not_resume(&canvas, cx);
    }

    #[gpui::test]
    fn zoom_reset_cancels_a_pending_navigation_pan(cx: &mut TestAppContext) {
        init_test(cx);
        let (canvas, cx) = cx.add_window_view(|window, cx| CanvasView::new(WeakEntity::new_invalid(), window, cx));
        cx.simulate_resize(size(px(1000.), px(800.)));
        redraw(cx);
        bind_real_keymap(cx);
        probe(&canvas, 1, 100., 100., cx);
        probe(&canvas, 2, 3000., 150., cx);
        canvas.update_in(cx, |c, window, cx| {
            c.viewport.zoom = 0.8;
            c.model.select(NodeId(1), false);
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        start_pending_nav_pan(&canvas, cx);
        canvas.update(cx, |c, cx| c.zoom_reset(cx));
        assert_pan_animation_does_not_resume(&canvas, cx);
    }

    #[gpui::test]
    fn user_scroll_pan_cancels_a_pending_navigation_pan(cx: &mut TestAppContext) {
        init_test(cx);
        let (canvas, cx) = cx.add_window_view(|window, cx| CanvasView::new(WeakEntity::new_invalid(), window, cx));
        cx.simulate_resize(size(px(1000.), px(800.)));
        redraw(cx);
        bind_real_keymap(cx);
        probe(&canvas, 1, 100., 100., cx);
        probe(&canvas, 2, 3000., 150., cx);
        canvas.update_in(cx, |c, window, cx| {
            c.viewport.zoom = 0.8;
            c.model.select(NodeId(1), false);
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        start_pending_nav_pan(&canvas, cx);
        canvas.update_in(cx, |c, window, cx| {
            let e = ScrollWheelEvent {
                position: point(px(5.), px(5.)),
                delta: ScrollDelta::Pixels(point(px(5.), px(5.))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            };
            c.pan_by_scroll(&e, window, cx);
        });
        assert_pan_animation_does_not_resume(&canvas, cx);
    }

    #[gpui::test]
    fn drag_pan_start_cancels_a_pending_navigation_pan(cx: &mut TestAppContext) {
        init_test(cx);
        let (canvas, cx) = cx.add_window_view(|window, cx| CanvasView::new(WeakEntity::new_invalid(), window, cx));
        cx.simulate_resize(size(px(1000.), px(800.)));
        redraw(cx);
        bind_real_keymap(cx);
        probe(&canvas, 1, 100., 100., cx);
        probe(&canvas, 2, 3000., 150., cx);
        canvas.update_in(cx, |c, window, cx| {
            c.viewport.zoom = 0.8;
            c.model.select(NodeId(1), false);
            window.focus(&c.focus_handle, cx);
        });
        redraw(cx);
        start_pending_nav_pan(&canvas, cx);
        canvas.update_in(cx, |c, window, cx| {
            c.space_held = true;
            let e = MouseDownEvent {
                button: MouseButton::Left,
                position: point(px(10.), px(10.)),
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            };
            c.on_background_mouse_down(&e, window, cx);
        });
        assert_pan_animation_does_not_resume(&canvas, cx);
    }
}
