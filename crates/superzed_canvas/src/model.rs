use gpui::{Point, Size, point, size};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MIN_NODE_WIDTH: f32 = 160.;
pub const MIN_NODE_HEIGHT: f32 = 90.;
/// Scostamento tra nodi creati nello stesso punto.
const CASCADE: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Note,
    Shell,
    Claude,
    Kanban,
    Automations,
    /// Pannello di una fonte (GitHub, HyperDX, Zabbix): `source_node`.
    Source,
    /// Posta del progetto (Gmail via gog): `mail_node`, uno per progetto, non chiudibile.
    Mail,
}

impl NodeKind {
    pub fn is_terminal(self) -> bool {
        matches!(self, NodeKind::Shell | NodeKind::Claude)
    }
}

/// Tipo di contenitore di chat claude (uno per tipo per canvas).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockKind {
    Kanban,
    Automations,
}

impl DockKind {
    pub const ALL: [DockKind; 2] = [DockKind::Kanban, DockKind::Automations];

    /// Il tipo di nodo del contenitore.
    pub fn node_kind(self) -> NodeKind {
        match self {
            DockKind::Kanban => NodeKind::Kanban,
            DockKind::Automations => NodeKind::Automations,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeFrame {
    pub id: NodeId,
    pub kind: NodeKind,
    pub origin: Point<f32>,
    pub size: Size<f32>,
    pub title: String,
    /// Indice nella palette dei colori dei nodi (vedi canvas_view::node_color).
    pub color: Option<u8>,
    /// Gruppo opzionale: nel sottoprogetto 1 e' solo salvato, lo usano i successivi.
    pub group: Option<String>,
    /// Chat claude agganciata a un contenitore (kanban o Automazioni): posizione e
    /// dimensione le decide il layout del contenitore.
    pub dock: Option<DockKind>,
    /// In evidenza (cmd-g): bordo dorato, in cima alle Sessioni, risparmiato da
    /// «Close idle chats».
    pub golden: bool,
}

impl NodeFrame {
    pub fn in_kanban(&self) -> bool {
        self.dock == Some(DockKind::Kanban)
    }
}

/// Chat che «Close idle chats» chiuderebbe, e quante ne risparmia e perche'.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IdleChats {
    pub close: Vec<NodeId>,
    pub golden: usize,
    pub active: usize,
}

/// Chat claude libere o nel kanban che non sono golden ne' `active`. Le chat delle
/// Automazioni non si toccano: chiuderle cancellerebbe l'automazione.
pub fn idle_chats(frames: &[NodeFrame], active: impl Fn(NodeId) -> bool) -> IdleChats {
    let mut plan = IdleChats::default();
    for frame in frames {
        if frame.kind != NodeKind::Claude || frame.dock == Some(DockKind::Automations) {
            continue;
        }
        if frame.golden {
            plan.golden += 1;
        } else if active(frame.id) {
            plan.active += 1;
        } else {
            plan.close.push(frame.id);
        }
    }
    plan
}

/// Nodi del canvas in ordine di disegno: l'ultimo e' in cima.
#[derive(Debug, Default)]
pub struct CanvasModel {
    frames: Vec<NodeFrame>,
    selection: BTreeSet<NodeId>,
    next_id: u64,
}

impl CanvasModel {
    pub fn frames(&self) -> &[NodeFrame] {
        &self.frames
    }

    pub fn frame(&self, id: NodeId) -> Option<&NodeFrame> {
        self.frames.iter().find(|f| f.id == id)
    }

    pub fn frame_mut(&mut self, id: NodeId) -> Option<&mut NodeFrame> {
        self.frames.iter_mut().find(|f| f.id == id)
    }

    /// Aggiunge un nodo in cima e lo seleziona da solo.
    pub fn add(
        &mut self,
        kind: NodeKind,
        origin: Point<f32>,
        size: Size<f32>,
        title: impl Into<String>,
    ) -> NodeId {
        self.next_id += 1;
        let id = NodeId(self.next_id);
        self.frames.push(NodeFrame {
            id,
            kind,
            origin,
            size: clamp_size(size),
            title: title.into(),
            color: None,
            group: None,
            dock: None,
            golden: false,
        });
        self.selection.clear();
        self.selection.insert(id);
        id
    }

    /// Aggiunge un nodo letto da project.json, tenendone l'id.
    pub fn insert_restored(&mut self, mut frame: NodeFrame) {
        frame.size = clamp_size(frame.size);
        self.next_id = self.next_id.max(frame.id.0);
        self.frames.retain(|f| f.id != frame.id);
        self.frames.push(frame);
    }

    /// Id piu' alto mai assegnato o ripristinato.
    pub fn max_id(&self) -> u64 {
        self.next_id
    }

    /// Tiene occupati gli id fino a `id` (nodi di tipo sconosciuto letti da project.json).
    pub fn reserve_id(&mut self, id: u64) {
        self.next_id = self.next_id.max(id);
    }

    /// Cambia l'id di un nodo (frame e selezione). Serve quando un nodo creato durante
    /// il caricamento ha lo stesso id di uno letto da project.json.
    pub fn renumber(&mut self, old: NodeId, new: NodeId) {
        if let Some(frame) = self.frame_mut(old) {
            frame.id = new;
            self.next_id = self.next_id.max(new.0);
            if self.selection.remove(&old) {
                self.selection.insert(new);
            }
        }
    }

    pub fn remove(&mut self, ids: &[NodeId]) -> Vec<NodeFrame> {
        let mut removed = Vec::new();
        self.frames.retain(|f| {
            if ids.contains(&f.id) {
                removed.push(f.clone());
                false
            } else {
                true
            }
        });
        for id in ids {
            self.selection.remove(id);
        }
        removed
    }

    pub fn bring_to_front(&mut self, id: NodeId) {
        if let Some(ix) = self.frames.iter().position(|f| f.id == id) {
            let frame = self.frames.remove(ix);
            self.frames.push(frame);
        }
    }

    /// Porta il nodo in fondo allo z-order (il kanban sta sotto le sue chat).
    pub fn send_to_back(&mut self, id: NodeId) {
        if let Some(ix) = self.frames.iter().position(|f| f.id == id) {
            let frame = self.frames.remove(ix);
            self.frames.insert(0, frame);
        }
    }

    /// `additive` (shift-click) aggiunge o toglie il nodo; altrimenti lo seleziona da solo.
    pub fn select(&mut self, id: NodeId, additive: bool) {
        if self.frame(id).is_none() {
            return;
        }
        if additive {
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
        } else {
            self.selection.clear();
            self.selection.insert(id);
        }
    }

    pub fn select_all(&mut self) {
        self.selection = self.frames.iter().map(|f| f.id).collect();
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    pub fn selection(&self) -> &BTreeSet<NodeId> {
        &self.selection
    }

    pub fn is_selected(&self, id: NodeId) -> bool {
        self.selection.contains(&id)
    }

    pub fn move_selected(&mut self, delta: Point<f32>) {
        for frame in &mut self.frames {
            if self.selection.contains(&frame.id) {
                frame.origin = point(frame.origin.x + delta.x, frame.origin.y + delta.y);
            }
        }
    }

    /// Origine per un nodo nuovo centrato su `center` (coordinate canvas);
    /// se un nodo ha gia' quell'origine si scende a cascata.
    pub fn placement_near(&self, center: Point<f32>, size: Size<f32>) -> Point<f32> {
        let mut origin = point(center.x - size.width / 2., center.y - size.height / 2.);
        while self
            .frames
            .iter()
            .any(|f| (f.origin.x - origin.x).abs() < 1. && (f.origin.y - origin.y).abs() < 1.)
        {
            origin = point(origin.x + CASCADE, origin.y + CASCADE);
        }
        origin
    }
}

fn clamp_size(s: Size<f32>) -> Size<f32> {
    size(s.width.max(MIN_NODE_WIDTH), s.height.max(MIN_NODE_HEIGHT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    fn model_with(n: usize) -> (CanvasModel, Vec<NodeId>) {
        let mut m = CanvasModel::default();
        let ids = (0..n)
            .map(|i| m.add(NodeKind::Note, point(i as f32 * 10., 0.), size(200., 100.), format!("n{i}")))
            .collect();
        (m, ids)
    }

    #[test]
    fn add_puts_node_on_top_and_selects_only_it() {
        let (m, ids) = model_with(3);
        assert_eq!(m.frames().last().unwrap().id, ids[2]);
        assert_eq!(m.selection().iter().copied().collect::<Vec<_>>(), vec![ids[2]]);
        assert!(ids[0] < ids[1] && ids[1] < ids[2]);
    }

    #[test]
    fn restored_ids_are_kept_and_new_ids_do_not_collide() {
        let mut m = CanvasModel::default();
        m.insert_restored(NodeFrame {
            id: NodeId(41),
            kind: NodeKind::Shell,
            origin: point(0., 0.),
            size: size(300., 200.),
            title: "shell".into(),
            color: None,
            group: None,
            dock: None,
            golden: false,
        });
        let fresh = m.add(NodeKind::Note, point(0., 0.), size(200., 100.), "n");
        assert!(fresh > NodeId(41));
        assert!(m.frame(NodeId(41)).is_some());
    }

    #[test]
    fn new_frames_are_not_golden() {
        let (m, ids) = model_with(1);
        assert!(!m.frame(ids[0]).unwrap().golden);
    }

    #[test]
    fn idle_chats_skip_golden_active_automations_and_non_chats() {
        let mut m = CanvasModel::default();
        let mut add = |kind: NodeKind, title: &str| m.add(kind, point(0., 0.), size(200., 100.), title);
        let idle = add(NodeKind::Claude, "idle");
        let golden = add(NodeKind::Claude, "golden");
        let active = add(NodeKind::Claude, "active");
        let golden_active = add(NodeKind::Claude, "golden active");
        let automation = add(NodeKind::Claude, "auto");
        let kanban_chat = add(NodeKind::Claude, "kanban");
        add(NodeKind::Shell, "sh");
        add(NodeKind::Note, "n");
        add(NodeKind::Source, "src");
        m.frame_mut(golden).unwrap().golden = true;
        m.frame_mut(golden_active).unwrap().golden = true;
        m.frame_mut(automation).unwrap().dock = Some(DockKind::Automations);
        m.frame_mut(kanban_chat).unwrap().dock = Some(DockKind::Kanban);
        let plan = idle_chats(m.frames(), |id| id == active || id == golden_active);
        assert_eq!(plan.close, vec![idle, kanban_chat]);
        assert_eq!(plan.golden, 2, "una golden attiva conta come golden");
        assert_eq!(plan.active, 1);
    }

    #[test]
    fn remove_drops_frames_and_their_selection() {
        let (mut m, ids) = model_with(3);
        m.select(ids[0], false);
        m.select(ids[1], true);
        let removed = m.remove(&[ids[0], ids[1]]);
        assert_eq!(removed.len(), 2);
        assert_eq!(m.frames().iter().map(|f| f.id).collect::<Vec<_>>(), vec![ids[2]]);
        assert!(m.selection().is_empty());
        assert!(m.remove(&[NodeId(999)]).is_empty());
    }

    #[test]
    fn bring_to_front_changes_z_order_only() {
        let (mut m, ids) = model_with(3);
        m.bring_to_front(ids[0]);
        assert_eq!(
            m.frames().iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![ids[1], ids[2], ids[0]]
        );
    }

    #[test]
    fn select_additive_toggles_and_plain_select_replaces() {
        let (mut m, ids) = model_with(3);
        m.select(ids[0], false);
        m.select(ids[1], true);
        assert!(m.is_selected(ids[0]) && m.is_selected(ids[1]));
        m.select(ids[1], true);
        assert!(!m.is_selected(ids[1]));
        m.select(ids[2], false);
        assert_eq!(m.selection().len(), 1);
        m.select_all();
        assert_eq!(m.selection().len(), 3);
        m.clear_selection();
        assert!(m.selection().is_empty());
    }

    #[test]
    fn move_selected_moves_only_selected_nodes() {
        let (mut m, ids) = model_with(3);
        m.select(ids[0], false);
        m.select(ids[2], true);
        m.move_selected(point(5., 7.));
        assert_eq!(m.frame(ids[0]).unwrap().origin, point(5., 7.));
        assert_eq!(m.frame(ids[1]).unwrap().origin, point(10., 0.));
        assert_eq!(m.frame(ids[2]).unwrap().origin, point(25., 7.));
    }

    #[test]
    fn placement_near_centers_and_cascades_on_collision() {
        let mut m = CanvasModel::default();
        let first = m.placement_near(point(500., 400.), size(200., 100.));
        assert_eq!(first, point(400., 350.));
        m.add(NodeKind::Note, first, size(200., 100.), "a");
        let second = m.placement_near(point(500., 400.), size(200., 100.));
        assert_eq!(second, point(424., 374.));
    }

    #[test]
    fn terminal_kinds() {
        assert!(NodeKind::Shell.is_terminal() && NodeKind::Claude.is_terminal());
        assert!(!NodeKind::Note.is_terminal());
        assert!(!NodeKind::Source.is_terminal());
    }

    #[test]
    fn reserved_ids_are_never_reused() {
        let mut m = CanvasModel::default();
        m.reserve_id(9);
        assert_eq!(m.add(NodeKind::Note, point(0., 0.), size(200., 100.), "n"), NodeId(10));
        m.reserve_id(3);
        assert_eq!(m.max_id(), 10);
    }
}
