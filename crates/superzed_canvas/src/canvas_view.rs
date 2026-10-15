use crate::automations_layout;
use crate::kanban_layout;
use crate::model::{CanvasModel, DockKind, MIN_NODE_HEIGHT, MIN_NODE_WIDTH, NodeFrame, NodeId, NodeKind, idle_chats};
use crate::node::NodeContent;
use crate::note_node::NoteNode;
use crate::persistence::{self, LoadOutcome, NodeRecord, ProjectFile, TabRecord, ViewportRecord};
use crate::source_node::{SourceNode, SourcePanelEvent, SourcePanelView};
use crate::terminal_node::{self, TerminalNode};
use crate::viewport::{self, Viewport, ZOOM_STEP};
use collections::HashMap;
use editor::Editor;
use fs::Fs;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, IntoElement,
    KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PinchEvent, Pixels, Point, Render, ScrollWheelEvent, SharedString, Size, Subscription, Task,
    WeakEntity, Window, canvas, div, point, px, rems, size,
};
use settings::Settings as _;
use std::{cell::Cell, path::PathBuf, rc::Rc, sync::Arc, time::Duration};
use superzed_actions::{CanvasZoomIn, CanvasZoomOut, CanvasZoomReset};
use terminal::Terminal;
use terminal_view::TerminalView;
use ui::{Label, LabelSize, prelude::*, utils::WithRemSize};
use util::ResultExt as _;
use workspace::{Toast, Workspace, notifications::NotificationId, superzed_layout::SuperzedProjectState};

mod automations;
pub use automations::{AutomationRow, AutomationRuntime};
mod commit_close;
mod done;
mod follow;
pub use done::DoneStage;
mod navigation;
mod scroll_route;
mod shortcut_hints;
pub use scroll_route::PAN_LATCH_MS;

pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const TERMINAL_SIZE: (f32, f32) = (640., 400.);

/// Dove e quando salvare. `None` finche' il progetto non ha un worktree.
struct Persist {
    fs: Arc<dyn Fs>,
    root: PathBuf,
    loaded: bool,
    /// Il file non si e' potuto spostare in .bak: non va sovrascritto.
    save_blocked: bool,
    pending_save: Option<Task<()>>,
}

/// Sessione claude di un nodo al momento dello spawn.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSessionInit {
    pub key: String,
    pub account: String,
    /// Id dell'account (`None` = sistema), salvato nel `data` del nodo come `account`.
    pub account_id: Option<String>,
    pub session_id: Option<String>,
    pub phase: superzed_sessions::Phase,
    pub times: superzed_sessions::SessionTimes,
}

/// Card di una sessione claude, per il kanban.
#[derive(Clone, Debug)]
pub struct SessionCard {
    pub node: NodeId,
    pub title: String,
    pub session: superzed_sessions::Session,
}

/// Chiave della sessione di un nodo e le sottoscrizioni di focus che la tengono aggiornata.
struct NodeSession {
    key: String,
    _focus: [Subscription; 2],
}

pub const NODE_COLORS: u8 = 8;
const NOTE_SIZE: (f32, f32) = (320., 220.);
const NOTE_MIN_SIZE: (f32, f32) = (120., 80.);

/// Nodi che si ridimensionano dalla propria maniglia: note, chat, shell e fonti non
/// agganciate (quelle agganciate prendono la dimensione dal contenitore, che ha la sua).
fn free_resizable(frame: &NodeFrame) -> bool {
    match frame.kind {
        NodeKind::Note | NodeKind::Mail => true,
        NodeKind::Shell | NodeKind::Claude | NodeKind::Source => frame.dock.is_none(),
        NodeKind::Kanban | NodeKind::Automations => false,
    }
}

fn min_free_size(kind: NodeKind) -> (f32, f32) {
    match kind {
        NodeKind::Note => NOTE_MIN_SIZE,
        _ => (MIN_NODE_WIDTH, MIN_NODE_HEIGHT),
    }
}
const KANBAN_SIZE: (f32, f32) = (960., 360.);
/// Distanza fra un pannello di una fonte e la chat aperta da una sua riga.
pub const BESIDE_GAP: f32 = 40.;
/// Lato della maniglia del kanban (px di schermo, a ogni zoom) e quanto sporge fuori
/// dal suo angolo.
const KANBAN_GRIP: f32 = 20.;
const KANBAN_GRIP_OUTSIDE: f32 = 6.;
/// Altezza della barra del titolo di un nodo, in rem (scalati dallo zoom).
pub(crate) const TITLE_BAR_REMS: f32 = 2.3;
/// Fattore di restringimento di stato Done e meta nella barra del titolo: cedono tutto
/// lo spazio prima che il titolo (fattore 1) si restringa.
const SHRINK_BEFORE_TITLE: f32 = 1000.;

/// Colore dei nodi golden (bordo, stella).
pub fn golden_color(cx: &App) -> Hsla {
    cx.theme().colors().terminal_ansi_yellow
}

/// Stella di un nodo golden: barra del titolo, vista d'insieme e nodo in avvio.
fn golden_star(id: u64, cx: &App) -> impl IntoElement {
    div()
        .debug_selector(move || format!("superzed-node-golden-{id}"))
        .flex_none()
        .child(ui::Icon::new(ui::IconName::StarFilled).size(ui::IconSize::Small).color(Color::Custom(golden_color(cx))))
}

/// Colore del bordo di un nodo (e della tab di progetto): i colori dei
/// partecipanti del tema, cosi' seguono il tema chiaro/scuro.
pub fn node_color(ix: Option<u8>, cx: &App) -> Option<Hsla> {
    ix.map(|i| cx.theme().players().color_for_participant(u32::from(i % NODE_COLORS)).cursor)
}

/// Listener di scroll del wrapper di un nodo. GPUI consegna lo scroll in fase
/// Bubble in ordine inverso di paint: il contenuto del nodo lo riceve prima,
/// poi il wrapper lo ferma, cosi' non arriva al pan del canvas. Con cmd passa:
/// e' lo zoom del canvas.
pub(crate) fn node_scroll_guard(e: &ScrollWheelEvent, cx: &mut App) {
    if !e.modifiers.platform {
        cx.stop_propagation();
    }
}

fn to_f32(p: Point<Pixels>) -> Point<f32> {
    point(f32::from(p.x), f32::from(p.y))
}

pub(crate) enum DragState {
    Pan { last: Point<Pixels> },
    /// Sposta tutti i nodi selezionati. `moved`: il mouse si e' mosso davvero (un click
    /// senza movimento sul titolo non e' un rilascio sul kanban).
    Move { last: Point<Pixels>, moved: bool },
    /// Resize di un nodo libero (`free_resizable`): la dimensione del frame segue il mouse
    /// (al minimo `min_free_size`).
    ResizeNode { id: NodeId, start_mouse: Point<Pixels>, start_size: Size<f32>, min: (f32, f32) },
    /// Resize di un contenitore. `rows`: righe del
    /// contenitore al mouse-down, cosi' la mappa dimensione del contenitore → dimensione
    /// delle chat non cambia a meta' drag.
    ResizeContainer { kind: DockKind, start_mouse: Point<Pixels>, start_size: Size<f32>, rows: usize },
}

/// Stato di un contenitore (kanban o Automazioni): dimensione delle sue chat e `data`
/// letto dal file (per risalvarne le altre chiavi).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContainerState {
    pub chat_size: Size<f32>,
    pub data: serde_json::Value,
}

impl Default for ContainerState {
    fn default() -> Self {
        Self { chat_size: kanban_layout::DEFAULT_CHAT_SIZE, data: serde_json::Value::Null }
    }
}

/// Solo test (anche di altri crate): PTY al posto del vero claude. Mai impostato dall'app.
pub struct ClaudePtyOverride(pub task::SpawnInTerminal);
impl gpui::Global for ClaudePtyOverride {}

/// Aprendo un progetto si creano il kanban e le Automazioni se mancano. Assente = si';
/// i test che partono da un canvas vuoto lo mettono a `false`.
pub struct DefaultContainers(pub bool);
impl gpui::Global for DefaultContainers {}

pub struct CanvasView {
    pub(crate) workspace: WeakEntity<Workspace>,
    pub(crate) viewport: Viewport,
    pub(crate) model: CanvasModel,
    pub(crate) contents: HashMap<NodeId, Box<dyn NodeContent>>,
    pub(crate) focus_handle: FocusHandle,
    /// Mai a fuoco: sta in un elemento vuoto con il contesto `CanvasFocused`, cosi' il menu
    /// contestuale del nodo trova e mostra le scorciatoie di Rinomina/Colore/Chiudi anche
    /// quando il canvas non ha il focus (il menu lo prende all'apertura).
    menu_keys: FocusHandle,
    /// Bounds dell'elemento canvas nella finestra, registrati a ogni prepaint.
    canvas_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Zoom su un nodo chiesto quando il canvas non aveva ancora i limiti (mai disegnato,
    /// es. la tab di un altro progetto appena attivata): si applica appena il prepaint li
    /// registra (`zoom_to_node_when_ready`).
    pending_zoom: Rc<Cell<Option<NodeId>>>,
    pub(crate) drag: Option<DragState>,
    space_held: bool,
    /// Gesto di pan a due dita in corso (`scroll_route`).
    pan_latch: scroll_route::PanLatch,
    _subscriptions: Vec<Subscription>,
    persist: Option<Persist>,
    /// Ultimo stato per progetto visto sul workspace (per salvare solo se cambia).
    last_state: Option<SuperzedProjectState>,
    node_subscriptions: HashMap<NodeId, Subscription>,
    mail_views: HashMap<NodeId, Entity<crate::mail_node::MailPanelView>>,
    last_warning: Option<String>,
    /// Nodi terminale, in aggiunta a `contents` (che li vede solo come `dyn NodeContent`).
    terminals: HashMap<NodeId, (Entity<TerminalView>, Entity<Terminal>)>,
    /// Chiave di sessione claude dei nodi che ne hanno una (Task 6, 2a).
    sessions: HashMap<NodeId, NodeSession>,
    /// Fix round 1 (Task 6 review, finding 2): chiave di sessione di un nodo restaurato
    /// il cui PTY non e' ancora partito (`contents` non ha ancora nulla per l'id, quindi
    /// `node_save_data` non troverebbe il session_id). Tolta quando il nodo si aggancia,
    /// viene rimosso o lo spawn fallisce.
    pending_sessions: HashMap<NodeId, String>,
    /// Nodi terminale gia' nel modello il cui PTY sta ancora partendo (si disegnano con un
    /// loader). Il valore identifica lo spawn: l'id del nodo puo' cambiare nel frattempo
    /// (`renumber_node` a fine caricamento), il biglietto no.
    starting_terminals: HashMap<NodeId, u64>,
    next_start_ticket: u64,
    // superzed: il MultiWorkspace arriva dopo la creazione del workspace; l'aggancio si
    // ritenta finche' non c'e' (`ensure_multi_workspace_subscription`).
    mw_subscription: Option<Subscription>,
    /// Un salvataggio chiesto mentre il file si caricava (scartato da `schedule_save`):
    /// si fa a fine caricamento (restore di un nodo claude con un id nuovo).
    save_after_load: bool,
    /// La prossima chat claude creata da `open_terminal_with_spec` nasce agganciata a quel
    /// contenitore: lo legge e lo azzera la chiamata stessa (`new_claude_in_kanban`).
    dock_next_claude: Option<DockKind>,
    /// Stato dei contenitori presenti (al piu' uno per tipo per canvas): la dimensione
    /// delle loro chat cambia con il resize e si salva nel loro `data` (`chat_size`).
    pub(crate) containers: HashMap<DockKind, ContainerState>,
    /// Definizione e stato delle chat agganciate alle Automazioni (Task 10).
    pub(crate) automations: HashMap<NodeId, AutomationRuntime>,
    /// Pipeline Fatto → GitHub delle chat del kanban (sottoprogetto 4).
    pub(crate) done_pipeline: done::DonePipeline,
    /// Come `dock_next_claude`: la prossima chat claude nuova nasce con questa
    /// definizione (`create_automation`).
    next_claude_automation: Option<crate::automation::AutomationDef>,
    /// Come `dock_next_claude`: la prossima chat claude nuova nasce in questo punto
    /// (accanto al pannello di una fonte, `open_chat_beside`).
    next_claude_origin: Option<Point<f32>>,
    /// Variabili d'ambiente per la prossima chat claude nuova (consumate da `open_new_claude`).
    next_claude_exports: Vec<(String, String)>,
    /// Connessioni della vista Fonti usate dal progetto (project.json → `sources`).
    project_sources: Vec<String>,
    /// Spunte cambiate mentre project.json si caricava: `apply_file` sostituisce la lista,
    /// `finish_load` le riapplica (in ordine).
    pending_source_changes: Vec<(String, bool)>,
    /// Nodi di tipo sconosciuto letti da project.json: si riscrivono tali e quali.
    foreign_nodes: Vec<serde_json::Value>,
    /// Solo test: l'ora vista dalle automazioni.
    #[cfg(test)]
    pub(crate) test_now: Option<chrono::DateTime<chrono::Local>>,
    /// Solo test: PTY dei nodi claude al posto del vero `claude` (lo spec della
    /// sessione resta quello vero).
    #[cfg(test)]
    pub(crate) test_claude_pty: Option<task::SpawnInTerminal>,
    /// Script dell'ultimo nodo claude lanciato, prima delle sostituzioni di test.
    last_claude_command: Option<String>,
    /// Account scelto nel picker per la prossima chat nuova (lo legge e lo azzera
    /// `account_for_new_chat`).
    pub(crate) next_claude_account: Option<String>,
    /// Ultima `attention` vista, per notificare il workspace (campana/pallino sulla
    /// tab) solo quando cambia (Task 9).
    last_attention: superzed_sessions::Attention,
    /// Tick delle automazioni (ogni `automation::TICK`).
    _automation_ticker: Task<()>,
    /// Misure (dal rem) dell'ultimo layout dei contenitori: se al disegno il rem e'
    /// cambiato senza passare dalle impostazioni (cmd-+ non salvato) si rifa' il layout.
    layout_metrics: Option<(kanban_layout::Metrics, automations_layout::Metrics)>,
    /// Ultimo nodo raggiunto (o lasciato) per area con la navigazione da tastiera.
    nav_last: HashMap<navigation::Area, NodeId>,
    /// Spostamento animato della viewport verso la destinazione della navigazione.
    nav_pan: Task<()>,
    /// Pannello delle scorciatoie tenendo premuto cmd, ctrl o alt (`shortcut_hints`).
    shortcut_hints: shortcut_hints::ShortcutHints,
    /// Fase di ogni chat agganciata all'ultimo relayout del kanban: un cambio di colonna
    /// della chat con il focus sposta la visuale (`follow`).
    kanban_phases: HashMap<NodeId, superzed_sessions::Phase>,
    /// Rilascio di un drag in corso: i suoi cambi di colonna non spostano la visuale.
    dropping: bool,
    /// Chat a cui e' stato chiesto di committare prima di chiudersi (`commit_close`).
    pub(crate) commit_closes: HashMap<NodeId, commit_close::CommitClose>,
    /// Lancia `git status` per il controllo finale di `commit_close` (finto nei test).
    pub(crate) git_runner: Arc<dyn superzed_sources::runner::Runner>,
}

/// Dissolvenza della vista d'insieme: dall'ultima risposta, o dalla nascita se la chat non
/// ha mai risposto. Una chat al lavoro o che aspetta l'utente non sbiadisce mai: e' viva
/// qualunque sia la data. `None` = niente date, niente dissolvenza.
pub(crate) fn overview_freshness(session: &superzed_sessions::Session, now: chrono::DateTime<chrono::Local>) -> Option<crate::age::Freshness> {
    use superzed_sessions::SessionState;
    if matches!(session.state, SessionState::Working | SessionState::NeedsInput(_)) {
        return None;
    }
    session.times.replied_at.or(session.times.opened_at).map(|t| crate::age::Freshness::of(t, now))
}

impl CanvasView {
    pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        // un key-up perso cambiando focus non deve lasciare lo spazio "premuto"
        let blur = cx.on_blur(&focus_handle, window, |this, _, _| this.space_held = false);
        // nel registro dei canvas: le notifiche si instradano per chiave di nodo
        let weak = cx.entity().downgrade();
        crate::registry::register(weak, cx);
        let mut this = Self {
            workspace,
            viewport: Viewport::default(),
            model: CanvasModel::default(),
            contents: HashMap::default(),
            focus_handle,
            menu_keys: cx.focus_handle(),
            canvas_bounds: Rc::new(Cell::new(Bounds::default())),
            pending_zoom: Rc::new(Cell::new(None)),
            drag: None,
            space_held: false,
            pan_latch: scroll_route::PanLatch::default(),
            _subscriptions: vec![blur],
            persist: None,
            last_state: None,
            node_subscriptions: HashMap::default(),
            mail_views: HashMap::default(),
            last_warning: None,
            terminals: HashMap::default(),
            sessions: HashMap::default(),
            pending_sessions: HashMap::default(),
            starting_terminals: HashMap::default(),
            next_start_ticket: 0,
            mw_subscription: None,
            save_after_load: false,
            dock_next_claude: None,
            containers: HashMap::default(),
            automations: HashMap::default(),
            done_pipeline: Default::default(),
            next_claude_automation: None,
            next_claude_origin: None,
            next_claude_exports: Vec::new(),
            project_sources: Vec::new(),
            pending_source_changes: Vec::new(),
            foreign_nodes: Vec::new(),
            #[cfg(test)]
            test_now: None,
            #[cfg(test)]
            test_claude_pty: None,
            last_claude_command: None,
            layout_metrics: None,
            next_claude_account: None,
            last_attention: superzed_sessions::Attention::None,
            _automation_ticker: Task::ready(()),
            nav_last: HashMap::default(),
            nav_pan: Task::ready(()),
            shortcut_hints: Default::default(),
            kanban_phases: HashMap::default(),
            dropping: false,
            commit_closes: HashMap::default(),
            git_runner: Arc::new(superzed_sources::runner::ProcessRunner),
        };
        // superzed: scheduler delle automazioni
        this._automation_ticker = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(crate::automation::TICK).await;
            if this.update(cx, |c, cx| c.automation_tick(cx)).is_err() {
                break;
            }
        });
        // superzed: font dell'editor, si registra per il progetto attivo quando cambia.
        // `observe_buffer_font_size_adjustment` non legge il workspace: puo' restare qui,
        // sincrona (a differenza delle sottoscrizioni sotto, che vanno in `defer_in`).
        this._subscriptions.push(theme_settings::observe_buffer_font_size_adjustment(
            cx,
            |this, cx| this.record_editor_font(cx),
        ));
        // superzed: tab chiusa (workspace rilasciato) o uscita dall'app: si terminano i
        // processi dei nodi e si scrive subito un salvataggio ancora in debounce.
        // il rem dell'interfaccia cambia l'intestazione del kanban e le fasce delle
        // Automazioni: il layout (posizioni delle chat, hit-test) le segue subito, come il
        // disegno (un rem cambiato fuori dalle impostazioni lo riprende `render`)
        this._subscriptions.push(cx.observe_global::<settings::SettingsStore>(|this, cx| this.sync_layout_metrics(cx)));
        this._subscriptions.push(cx.on_release(|this, cx| this.on_released(cx)));
        // il focus esce dal canvas (anche da un suo terminale) o la finestra cambia
        // attivazione: il suggerimento delle scorciatoie ricomincia da capo
        let focus = this.focus_handle.clone();
        this._subscriptions.push(cx.on_focus_out(&focus, window, |this, _, _, cx| this.hints_reset(cx)));
        this._subscriptions.push(cx.observe_window_activation(window, |this, _, cx| this.hints_reset(cx)));
        // un tasto non modificatore (anche una scorciatoia gia' risolta, che non arriva ai
        // listener di tasto) chiude il suggerimento delle scorciatoie
        let hints = cx.entity().downgrade();
        this._subscriptions.push(cx.intercept_keystrokes(move |_, window, cx| {
            hints
                .update(cx, |c, cx| {
                    if c.focus_handle.contains_focused(window, cx) {
                        c.hints_key_pressed(window, cx);
                    }
                })
                .ok();
        }));
        this._subscriptions.push(cx.on_app_quit(|this, cx| {
            let save = this.take_pending_save(cx);
            async move {
                if let Some(save) = save {
                    save.await;
                }
            }
        }));
        // superzed: sessioni claude (2a). Un id nuovo (/clear) va salvato; ogni cambio
        // di stato ridisegna i titoli.
        if let Some(store) = superzed_sessions::SessionStore::global(cx) {
            this._subscriptions.push(cx.observe(&store, |this, _, cx| {
                cx.notify();
                // una fase cambiata sposta la chat agganciata nella sua colonna
                this.relayout_containers(cx);
                // claude uscito in una chat di automazione al lavoro: esecuzione chiusa
                this.automation_sessions_changed(cx);
                // una chat con commit chiesto interrotta o uscita: niente chiusura
                this.commit_close_sessions_changed(cx);
                let attention = this.attention(cx);
                if this.last_attention != attention {
                    this.last_attention = attention;
                    if let Some(ws) = this.workspace.upgrade() {
                        ws.update(cx, |_, cx| cx.notify());
                    }
                }
            }));
            this._subscriptions.push(cx.subscribe(&store, |this, _, event, cx| match event {
                superzed_sessions::SessionEvent::SessionIdChanged { key }
                | superzed_sessions::SessionEvent::PhaseChanged { key } => {
                    if this.sessions.values().any(|s| &s.key == key)
                        || this.pending_sessions.values().any(|k| k == key)
                    {
                        this.schedule_save(cx);
                    }
                    // il /clear di un'automazione e' arrivato: ora il prompt
                    if matches!(event, superzed_sessions::SessionEvent::SessionIdChanged { .. }) {
                        this.automation_cleared(key, cx);
                    }
                    // una chat del kanban entrata in Fatto chiede il report (o esce e lo ferma)
                    if matches!(event, superzed_sessions::SessionEvent::PhaseChanged { .. }) {
                        this.done_phase_changed(key, cx);
                    }
                }
                superzed_sessions::SessionEvent::Stopped { key, failed } => {
                    if let Some(node) = this.node_for_session_key(key) {
                        this.automation_stopped(node, *failed, cx);
                        this.commit_close_stopped(node, *failed, cx);
                    }
                }
                superzed_sessions::SessionEvent::Cleared { key } => {
                    // anche senza id nuovo: il primo dei due fa partire il prompt, una volta
                    this.automation_cleared(key, cx);
                    // /clear: una chat del kanban oltre Nuove riparte da Nuove, come uno spostamento
                    // a mano; Altri e Analisi si compilano a mano, quindi restano dove sono
                    if let Some(node) = this.node_for_session_key(key)
                        && this.model.frame(node).is_some_and(|f| f.kind == NodeKind::Claude && f.in_kanban())
                        && this.kanban_node().is_some()
                        && superzed_sessions::SessionStore::global(cx)
                            .is_some_and(|store| store.read(cx).get(key).is_some_and(|session| session.phase > superzed_sessions::Phase::New))
                    {
                        this.set_session_phase(node, superzed_sessions::Phase::New, cx);
                    }
                }
                // le quote le legge `superzed_accounts`
                superzed_sessions::SessionEvent::RateLimits { .. } => {}
            }));
        }
        // superzed: `init` crea il canvas dentro `cx.observe_new::<Workspace>`, quindi il
        // workspace e' ancora in lease (in aggiornamento) quando `new` viene eseguito: leggerlo
        // qui panica ("cannot read Workspace while it is already being updated"). `defer_in`
        // rimanda l'aggancio alla fine del ciclo di effetti corrente, quando il lease e' tornato
        // libero (deviazione dal codice del brief, che leggeva subito `this.workspace.upgrade()`).
        cx.defer_in(window, |this, window, cx| {
            if let Some(workspace_entity) = this.workspace.upgrade() {
                // stato per progetto: si salva quando cambia
                this._subscriptions.push(cx.observe(&workspace_entity, |this, ws, cx| {
                    let state = ws.read(cx).superzed_state().clone();
                    if this.last_state.as_ref() != Some(&state) {
                        this.last_state = Some(state);
                        this.schedule_save(cx);
                    }
                }));
                // il worktree puo' arrivare dopo la creazione del workspace
                let project = workspace_entity.read(cx).project().clone();
                this._subscriptions.push(cx.subscribe_in(
                    &project,
                    window,
                    |this, _, event: &project::Event, window, cx| {
                        if matches!(event, project::Event::WorktreeAdded(_)) {
                            this.start_load(window, cx);
                        }
                    },
                ));
                this.start_load(window, cx);
                this.ensure_multi_workspace_subscription(window, cx);
            }
        });
        this
    }

    pub fn model(&self) -> &CanvasModel {
        &self.model
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// Centro del canvas in coordinate locali.
    pub(crate) fn local_center(&self) -> Point<f32> {
        let b = self.canvas_bounds.get();
        point(f32::from(b.size.width) / 2., f32::from(b.size.height) / 2.)
    }

    pub(crate) fn window_to_local(&self, p: Point<Pixels>) -> Point<f32> {
        viewport::window_to_local(to_f32(p), to_f32(self.canvas_bounds.get().origin))
    }

    /// Aggiunge un nodo al centro della viewport (a cascata se occupato).
    pub fn add_node(
        &mut self,
        kind: NodeKind,
        content: Box<dyn NodeContent>,
        size: Size<f32>,
        title: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> NodeId {
        let center = self.viewport.to_canvas(self.local_center());
        let origin = self.model.placement_near(center, size);
        let id = self.model.add(kind, origin, size, title);
        self.contents.insert(id, content);
        self.apply_font_zoom(id, cx);
        self.nodes_changed(cx);
        id
    }

    pub fn insert_restored_node(
        &mut self,
        frame: NodeFrame,
        content: Box<dyn NodeContent>,
        cx: &mut Context<Self>,
    ) {
        let id = frame.id;
        self.model.insert_restored(frame);
        self.contents.insert(id, content);
        self.apply_font_zoom(id, cx);
        cx.notify();
    }

    pub fn remove_nodes(&mut self, ids: &[NodeId], cx: &mut Context<Self>) {
        self.free_chats_if_container_removed(ids, cx);
        self.drop_sessions(ids, cx);
        for id in ids {
            if let Some(content) = self.contents.remove(id) {
                content.on_close(cx);
            }
            self.node_subscriptions.remove(id);
            self.mail_views.remove(id);
            self.terminals.remove(id);
            self.pending_sessions.remove(id);
            self.starting_terminals.remove(id);
            if self.pending_zoom.get() == Some(*id) {
                self.pending_zoom.set(None);
            }
            self.remove_automation(*id, cx);
            self.commit_closes.remove(id);
        }
        if !self.model.remove(ids).is_empty() {
            self.nodes_changed(cx);
        }
    }

    #[cfg(test)]
    pub(crate) fn terminal_content(&self, id: NodeId) -> Option<TerminalNode> {
        self.terminals.get(&id).map(|(view, terminal)| TerminalNode {
            view: view.clone(),
            terminal: terminal.clone(),
            session_key: self.session_key(id).map(str::to_string),
        })
    }

    fn project_cwd(&self, cx: &App) -> Option<PathBuf> {
        self.project_root(cx).map(|(_, root)| root)
    }

    /// Cartella del progetto (primo worktree visibile), se c'e'.
    pub fn project_root_path(&self, cx: &App) -> Option<PathBuf> {
        self.project_cwd(cx)
    }

    /// Da dove viene una notifica con questa chiave di nodo: progetto e sorgente.
    /// Per una chat di automazione la sorgente e' il nome dell'automazione, e una
    /// notifica durante un'esecuzione conta per il suo esito.
    pub fn notification_origin(&mut self, key: &str, cx: &mut Context<Self>) -> Option<(Option<PathBuf>, String)> {
        let node = self.node_for_session_key(key)?;
        let source = match self.automations.get_mut(&node) {
            Some(rt) => {
                if rt.phase == crate::automation::RunPhase::Running {
                    rt.notified += 1;
                }
                rt.def.name.clone()
            }
            None => "chat".to_string(),
        };
        Some((self.project_root_path(cx), source))
    }

    /// Account di sistema (quello dello store, o costruito qui se lo store non c'e').
    pub fn system_account(&self, cx: &App) -> superzed_accounts::Account {
        superzed_accounts::AccountStore::global(cx)
            .map(|s| s.read(cx).system().clone())
            .unwrap_or_else(|| {
                superzed_accounts::Account::system(superzed_sessions::launch::account_label(None, util::paths::home_dir()))
            })
    }

    /// Come `claude_spec_for`, sull'account di sistema.
    pub fn claude_spec(
        &self,
        saved: &superzed_sessions::SavedSession,
        key: &str,
        initial_prompt: Option<&str>,
        cx: &App,
    ) -> (task::SpawnInTerminal, NodeSessionInit) {
        self.claude_spec_for(&self.system_account(cx), saved, key, initial_prompt, cx)
    }

    /// Spec del nodo claude su un account: transcript, statusline dell'utente e resume
    /// dalla sua config dir (resume della sessione salvata se il transcript esiste,
    /// altrimenti un id nuovo); env degli hook se l'ipc e' partito; `CLAUDE_CONFIG_DIR`
    /// nello script se non e' il sistema.
    pub fn claude_spec_for(
        &self,
        account: &superzed_accounts::Account,
        saved: &superzed_sessions::SavedSession,
        key: &str,
        initial_prompt: Option<&str>,
        cx: &App,
    ) -> (task::SpawnInTerminal, NodeSessionInit) {
        use superzed_sessions::launch;
        let store = superzed_sessions::SessionStore::global(cx);
        let store = store.as_ref().map(|s| s.read(cx));
        let config_dir = account.config_dir.clone().unwrap_or_else(|| {
            store
                .map(|s| s.config_dir().to_path_buf())
                .unwrap_or_else(|| launch::system_config_dir(util::paths::home_dir()))
        });
        let mut claude = launch::claude_launch(
            store.and_then(|s| s.hook_env()),
            key,
            saved.session_id.as_deref(),
            |id| launch::transcript_exists(&config_dir, id),
            || uuid::Uuid::new_v4().to_string(),
            launch::user_statusline(&config_dir),
            initial_prompt,
        );
        claude.config_dir = account.config_dir.clone();
        let init = NodeSessionInit {
            key: key.to_string(),
            account: account.label.clone(),
            account_id: (!account.is_system()).then(|| account.id.clone()),
            session_id: Some(claude.session_id.clone()),
            phase: saved.phase,
            // chat nuova (niente sessione salvata): nasce adesso
            times: superzed_sessions::SessionTimes {
                opened_at: saved.times.opened_at.or_else(|| saved.session_id.is_none().then(superzed_sessions::now_unix)),
                replied_at: saved.times.replied_at,
            },
        };
        let spec = terminal_node::claude_node_spec(&terminal_node::login_shell(), self.project_cwd(cx), &claude);
        (spec, init)
    }

    /// Account di una chat nuova: quello scelto nel picker, altrimenti lo scheduler
    /// (tutti al limite → sistema con avviso).
    fn account_for_new_chat(&mut self, cx: &mut Context<Self>) -> superzed_accounts::Account {
        if let Some(id) = self.next_claude_account.take() {
            if id == superzed_accounts::SYSTEM_ID {
                return self.system_account(cx);
            }
            let chosen = superzed_accounts::AccountStore::global(cx).and_then(|s| s.read(cx).account(&id).cloned());
            return chosen.unwrap_or_else(|| {
                // tolto dal registro mentre il picker era aperto: sistema, con avviso (come al restore)
                self.warn(format!("The account “{id}” is no longer registered: the chat starts on the system account"), cx);
                self.system_account(cx)
            });
        }
        match superzed_accounts::pick_for_new_chat(cx, self.project_cwd(cx).as_deref()) {
            Some((account, all_excluded)) => {
                if all_excluded {
                    self.warn("All Claude accounts are at their limit: the chat starts on the system account".into(), cx);
                }
                account
            }
            None => self.system_account(cx),
        }
    }

    /// Account di una chat ripristinata: quello salvato; se non e' piu' registrato, il sistema
    /// con avviso. Senza AccountStore si parte in silenzio sul sistema ma l'id salvato resta
    /// nel `data` del nodo (al prossimo avvio con lo store la chat torna sul suo account).
    fn account_for_restore(&mut self, saved: Option<&str>, cx: &mut Context<Self>) -> superzed_accounts::Account {
        let Some(id) = saved.filter(|id| *id != superzed_accounts::SYSTEM_ID) else {
            return self.system_account(cx);
        };
        let Some(store) = superzed_accounts::AccountStore::global(cx) else {
            // config dir del sistema (niente export), ma l'id salvato: `account_id` lo conserva
            return superzed_accounts::Account { id: id.to_string(), ..self.system_account(cx) };
        };
        match store.read(cx).account(id).cloned() {
            Some(account) => account,
            None => {
                self.warn(format!("The account “{id}” is no longer registered: the chat restarts on the system account"), cx);
                self.system_account(cx)
            }
        }
    }

    /// Chat nuova su un account scelto a mano (picker): nel kanban o libera.
    pub fn new_claude_with_account(&mut self, account_id: String, in_kanban: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.next_claude_account = Some(account_id);
        if in_kanban {
            self.new_claude_in_kanban(window, cx);
        } else {
            self.open_terminal(NodeKind::Claude, None, &serde_json::Value::Null, window, cx);
        }
        self.next_claude_account = None;
    }

    /// Chat nuova (account dello scheduler) con un prompt gia' completo: nel kanban o libera.
    pub fn new_claude_from_prompt(&mut self, prompt: String, in_kanban: bool, window: &mut Window, cx: &mut Context<Self>) -> String {
        if in_kanban {
            self.new_claude_in_kanban_with_prompt(Some(prompt), window, cx)
        } else {
            self.open_new_claude(Some(&prompt), window, cx)
        }
    }

    /// Prompt che usa `{mail_file}`: prima scrive in `.superzed/mail/inbox.json` i thread
    /// del filtro arrivati dopo l'ultimo controllo, poi apre la chat (senza invio). L'ultimo
    /// controllo avanza solo a inbox scritta: un errore non fa perdere email.
    pub fn open_mail_prompt(
        &mut self,
        body: String,
        board_writes_auto: bool,
        mut placeholders: crate::prompt_picker::Placeholders,
        in_kanban: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use superzed_sources::gmail::{
            INBOX_FILE, INBOX_MAX_THREADS, MAIL_DIR, STATE_FILE, inbox_json, inbox_truncated, parse_last_check, since_query,
            state_json, truncate_bodies,
        };
        let settings = self.mail_view().map(|view| view.read(cx).settings());
        let Some((account, query)) = settings.filter(|(account, query)| !account.trim().is_empty() && !query.trim().is_empty())
        else {
            self.warn("Set the Gmail account and the project query in the mail node first".to_string(), cx);
            return;
        };
        let Some((fs, root)) = self.project_root(cx) else {
            self.warn("The mail prompt needs a project folder".to_string(), cx);
            return;
        };
        let client = superzed_sources::GmailClient::new(Self::mail_runner(cx), account.trim());
        let now = chrono::Utc::now();
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<(PathBuf, bool), String> = async {
                let dir = root.join(MAIL_DIR);
                let state_path = dir.join(STATE_FILE);
                let last_check = fs.load(&state_path).await.ok().and_then(|text| parse_last_check(&text));
                let since = since_query(last_check, now);
                let base = superzed_sources::project_query(&query, "").unwrap_or_default();
                let full_query = format!("{base} {since}");
                let summaries = client.search(&full_query, INBOX_MAX_THREADS).await?;
                let truncated = inbox_truncated(summaries.len());
                let mut threads = Vec::new();
                for summary in summaries {
                    let mut thread = client.thread(&summary.id, true).await?;
                    truncate_bodies(&mut thread, superzed_sources::CONTEXT_MAX_CHARS);
                    threads.push(thread);
                }
                fs.create_dir(&dir).await.map_err(|error| format!("{}: {error}", dir.display()))?;
                let ignore = dir.join(".gitignore");
                if !fs.is_file(&ignore).await {
                    fs.atomic_write(ignore.clone(), "*\n".to_string())
                        .await
                        .map_err(|error| format!("{}: {error}", ignore.display()))?;
                }
                let inbox = dir.join(INBOX_FILE);
                fs.atomic_write(inbox.clone(), inbox_json(client.account(), &base, &since, truncated, &threads))
                    .await
                    .map_err(|error| format!("{}: {error}", inbox.display()))?;
                fs.atomic_write(state_path.clone(), state_json(now))
                    .await
                    .map_err(|error| format!("{}: {error}", state_path.display()))?;
                Ok((inbox, truncated))
            }
            .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok((inbox, truncated)) => {
                    placeholders.mail_file = Some(inbox);
                    let prompt = crate::prompt_picker::full_prompt(&body, &placeholders, board_writes_auto);
                    this.next_claude_exports = no_send_exports();
                    this.new_claude_from_prompt(prompt, in_kanban, window, cx);
                    if truncated {
                        this.warn(
                            format!(
                                "Mail prompt: more than {INBOX_MAX_THREADS} new threads, the oldest were left out — narrow the project query in the mail node"
                            ),
                            cx,
                        );
                    }
                }
                Err(message) => this.warn(format!("Mail prompt: {message}"), cx),
            })
        })
        .detach_and_log_err(cx);
    }

    /// Crea un nodo terminale (`restored = None`) o ricrea quello salvato.
    /// Il frame ripristinato entra subito nel modello, cosi' un salvataggio fatto
    /// mentre il PTY parte non lo perde; il contenuto arriva quando il terminale e' pronto.
    pub fn open_terminal(
        &mut self,
        kind: NodeKind,
        restored: Option<NodeFrame>,
        saved: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            NodeKind::Claude => {
                let saved = superzed_sessions::SavedSession::from_data(saved);
                let key = uuid::Uuid::new_v4().to_string();
                // restore: l'account salvato; chat nuova (cmd-n): lo scheduler
                let account = if restored.is_some() {
                    self.account_for_restore(saved.account.as_deref(), cx)
                } else {
                    self.account_for_new_chat(cx)
                };
                let (spec, init) = self.claude_spec_for(&account, &saved, &key, None, cx);
                let spec = self.claude_pty(spec, cx);
                self.open_claude_terminal(restored, saved.session_id.as_deref(), spec, init, window, cx);
            }
            _ => {
                let spec = terminal_node::shell_node_spec(&terminal_node::login_shell(), self.project_cwd(cx));
                self.open_terminal_with_spec(kind, restored, spec, None, window, cx);
            }
        }
    }

    /// Ricorda il comando vero e applica le sostituzioni di test (mai il vero claude nei test).
    fn claude_pty(&mut self, spec: task::SpawnInTerminal, cx: &App) -> task::SpawnInTerminal {
        self.last_claude_command = spec.args.get(2).cloned();
        #[cfg(test)]
        let spec = self.test_claude_pty.clone().unwrap_or(spec);
        cx.try_global::<ClaudePtyOverride>().map(|o| o.0.clone()).unwrap_or(spec)
    }

    /// Script del nodo claude lanciato per ultimo (prima delle sostituzioni di test).
    pub fn last_claude_command(&self) -> Option<&str> {
        self.last_claude_command.as_deref()
    }

    /// Chat claude nuova (sessione nuova, prompt iniziale opzionale). Dove nasce lo decidono
    /// i flag letti da `open_terminal_with_spec` (`dock_next_claude`).
    /// Restituisce la chiave di sessione della chat (il nodo nasce dopo, quando parte il PTY).
    pub(crate) fn open_new_claude(&mut self, initial_prompt: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> String {
        let saved = superzed_sessions::SavedSession::default();
        let key = uuid::Uuid::new_v4().to_string();
        let account = self.account_for_new_chat(cx);
        let (mut spec, init) = self.claude_spec_for(&account, &saved, &key, initial_prompt, cx);
        terminal_node::prepend_exports(&mut spec, &std::mem::take(&mut self.next_claude_exports));
        let spec = self.claude_pty(spec, cx);
        self.open_claude_terminal(None, None, spec, init, window, cx);
        key
    }

    /// Fix wave finale (finding 1): un restore il cui `claude_spec` ha lanciato un id nuovo
    /// (transcript del salvato mancante) non fa mai arrivare un `SessionStart` diverso da
    /// quello gia' registrato: nessun `SessionIdChanged`, quindi mai salvato in project.json.
    /// Se l'id salvato e quello lanciato differiscono si programma subito un salvataggio (lo
    /// store e' gia' registrato con l'id nuovo, `node_save_data` lo trova via
    /// `pending_sessions`). Fattorizzato fuori da `open_terminal` cosi' i test possono passare
    /// uno spec di test (niente claude vero) e verificare comunque la regola.
    fn open_claude_terminal(
        &mut self,
        restored: Option<NodeFrame>,
        saved_session: Option<&str>,
        spec: task::SpawnInTerminal,
        init: NodeSessionInit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_resave = match (saved_session, init.session_id.as_deref()) {
            (Some(saved), Some(launched)) => saved != launched,
            _ => false,
        };
        self.open_terminal_with_spec(NodeKind::Claude, restored, spec, Some(init), window, cx);
        if needs_resave {
            if self.persist.as_ref().is_some_and(|p| !p.loaded) {
                self.save_after_load = true;
            } else {
                self.schedule_save(cx);
            }
        }
    }

    /// Come `open_terminal`, con il comando gia' deciso (i test usano una shell
    /// senza i file di configurazione dell'utente). Pubblico: la vista Sessioni
    /// globale lo usa nei test da `superzed_sidebar`.
    pub fn open_terminal_with_spec(
        &mut self,
        kind: NodeKind,
        restored: Option<NodeFrame>,
        spec: task::SpawnInTerminal,
        session: Option<NodeSessionInit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // letto qui, in modo sincrono: due creazioni ravvicinate non si scambiano il flag
        let dock_new = self.dock_next_claude.take().filter(|_| kind == NodeKind::Claude && restored.is_none());
        let automation = self.next_claude_automation.take().filter(|_| kind == NodeKind::Claude && restored.is_none());
        let place_at = self.next_claude_origin.take().filter(|_| kind == NodeKind::Claude && restored.is_none());
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        // una chat di automazione (nuova o ripristinata) e' una sessione di fondo
        let background =
            automation.is_some() || restored.as_ref().is_some_and(|f| self.automations.contains_key(&f.id));
        if let (Some(init), Some(store)) = (&session, superzed_sessions::SessionStore::global(cx)) {
            let init = init.clone();
            store.update(cx, |s, cx| {
                let key = init.key.clone();
                s.register(init.key, init.account, init.session_id, init.phase, init.times, cx);
                if let Some(id) = &init.account_id {
                    s.set_account_id(&key, id, cx);
                }
                if background {
                    s.set_background(&key, true);
                }
            });
        }
        let session_key = session.as_ref().map(|s| s.key.clone());
        let is_new = restored.is_none();
        // il nodo entra subito nel modello (con un loader al posto del terminale): far
        // partire claude puo' richiedere secondi e intanto il kanban vede gia' la sessione.
        // Una chat ripristinata tiene il suo frame; uno spawn fallito toglie il nodo.
        let id = match restored {
            Some(frame) => {
                let id = frame.id;
                self.model.insert_restored(frame);
                id
            }
            None => {
                let size = size(TERMINAL_SIZE.0, TERMINAL_SIZE.1);
                let center = self.viewport.to_canvas(self.local_center());
                let origin = self.model.placement_near(center, size);
                let id = self.model.add(kind, origin, size, spec.label.clone());
                // chat aperta da un pannello: accanto al pannello, non al centro della vista
                if let Some(origin) = place_at
                    && dock_new.is_none()
                    && let Some(f) = self.model.frame_mut(id)
                {
                    f.origin = origin;
                }
                if let Some(k) = dock_new
                    && self.container_node(k).is_some()
                    && let Some(f) = self.model.frame_mut(id)
                {
                    f.dock = Some(k);
                }
                id
            }
        };
        // Fix round 1 (Task 6 review, finding 2): `contents` resta vuoto finche' il PTY non
        // parte; un salvataggio in quella finestra perderebbe il session_id. `node_save_data`
        // ripiega su questa mappa finche' il nodo non si aggancia (o si toglie/fallisce lo spawn).
        if let Some(key) = &session_key {
            self.pending_sessions.insert(id, key.clone());
        }
        self.next_start_ticket += 1;
        let ticket = self.next_start_ticket;
        self.starting_terminals.insert(id, ticket);
        // una chat in un contenitore va subito al suo posto
        self.relayout_containers(cx);
        if is_new {
            self.nodes_changed(cx);
            let free_beside_panel = place_at.is_some() && dock_new.is_none();
            let docked = self.model.frame(id).is_some_and(|f| f.dock.is_some());
            // in fondo a Nuove (o accanto al pannello) puo' finire fuori dall'area visibile
            if free_beside_panel || docked {
                self.reveal(id, cx);
            }
        } else {
            cx.notify();
        }
        let project = workspace.read(cx).project().clone();
        let workspace_id = workspace.read(cx).database_id();
        let weak_ws = self.workspace.clone();
        let task = project.update(cx, |p, cx| p.create_terminal_task(spec, cx));
        cx.spawn_in(window, async move |canvas, cx| {
            let result = task.await;
            canvas
                .update_in(cx, |canvas, window, cx| {
                    // l'id attuale del nodo (puo' essere stato rinumerato); nessuno se il nodo
                    // e' stato chiuso mentre il PTY partiva
                    let id = canvas.starting_terminals.iter().find(|(_, t)| **t == ticket).map(|(id, _)| *id);
                    if let Some(id) = id {
                        canvas.starting_terminals.remove(&id);
                    }
                    let unregister = |cx: &mut App| {
                        if let (Some(key), Some(store)) = (&session_key, superzed_sessions::SessionStore::global(cx)) {
                            store.update(cx, |s, cx| s.unregister(key, cx));
                        }
                    };
                    let terminal = match result {
                        Ok(terminal) => terminal,
                        Err(e) => {
                            if let Some(id) = id {
                                canvas.pending_sessions.remove(&id);
                                canvas.model.remove(&[id]);
                                canvas.nodes_changed(cx);
                            }
                            unregister(cx);
                            canvas.warn(format!("Terminal did not start: {e}"), cx);
                            return;
                        }
                    };
                    let Some(id) = id else {
                        // il nodo e' stato chiuso mentre il PTY stava ancora partendo
                        // (contents non aveva ancora nulla per questo id, quindi remove_nodes
                        // non ha potuto chiamare on_close ne' terminare il processo): tocca
                        // a noi, altrimenti resterebbe un terminale mai disegnato e mai chiuso
                        crate::process::terminate_terminal(terminal.read(cx), cx);
                        unregister(cx);
                        return;
                    };
                    // zoom stabile: il PTY dei nodi si ridimensiona solo se cambia la griglia
                    terminal.update(cx, |t, _| t.superzed_set_grid_only_resize(true));
                    let view = cx.new(|cx| {
                        TerminalView::new(
                            terminal.clone(),
                            weak_ws,
                            workspace_id,
                            project.downgrade(),
                            window,
                            cx,
                        )
                    });
                    let content =
                        TerminalNode { view: view.clone(), terminal: terminal.clone(), session_key: session_key.clone() };
                    canvas.contents.insert(id, Box::new(content));
                    canvas.pending_sessions.remove(&id);
                    canvas.apply_font_zoom(id, cx);
                    cx.notify();
                    if is_new {
                        // solo una chat davvero agganciata alle Automazioni ha la definizione:
                        // se la sezione e' sparita mentre il PTY partiva, la chat resta libera
                        if let Some(def) = automation {
                            if canvas.model.frame(id).is_some_and(|f| f.dock == Some(DockKind::Automations)) {
                                if let Some(f) = canvas.model.frame_mut(id) {
                                    f.title = def.name.clone();
                                }
                                let now = canvas.now();
                                canvas.automations.insert(id, AutomationRuntime::new(def, now));
                                canvas.nodes_changed(cx);
                            } else {
                                if let (Some(key), Some(store)) =
                                    (&session_key, superzed_sessions::SessionStore::global(cx))
                                {
                                    store.update(cx, |s, _| s.set_background(key, false));
                                }
                                canvas.warn(
                                    format!("Automation “{}” not created: the Automations section no longer exists", def.name),
                                    cx,
                                );
                            }
                        }
                        view.focus_handle(cx).focus(window, cx);
                    }
                    canvas.terminals.insert(id, (view.clone(), terminal.clone()));
                    canvas.subscribe_terminal_exit(id, &terminal, cx);
                    if let Some(key) = session_key.clone() {
                        canvas.track_session_focus(id, key, &view, window, cx);
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Focus sul terminale del nodo → lo store sa quale sessione l'utente sta guardando.
    fn track_session_focus(
        &mut self,
        id: NodeId,
        key: String,
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = view.focus_handle(cx);
        let (k_in, k_out) = (key.clone(), key.clone());
        let focus_in = cx.on_focus_in(&handle, window, move |_, _, cx| {
            if let Some(store) = superzed_sessions::SessionStore::global(cx) {
                let key = k_in.clone();
                store.update(cx, |s, cx| s.set_focused(Some(key), cx));
            }
        });
        let focus_out = cx.on_focus_out(&handle, window, move |_, _, _, cx| {
            if let Some(store) = superzed_sessions::SessionStore::global(cx) {
                let key = k_out.clone();
                store.update(cx, |s, cx| {
                    if s.focused_key() == Some(key.as_str()) {
                        s.set_focused(None, cx);
                    }
                });
            }
        });
        // il nodo nuovo ha gia' il focus: on_focus_in non scatterebbe piu'
        if handle.is_focused(window)
            && let Some(store) = superzed_sessions::SessionStore::global(cx)
        {
            let k = key.clone();
            store.update(cx, |s, cx| s.set_focused(Some(k), cx));
        }
        self.sessions.insert(id, NodeSession { key, _focus: [focus_in, focus_out] });
    }

    /// L'utente ha interrotto claude nel nodo (Esc/ctrl-c).
    fn interrupt_session(&self, id: NodeId, cx: &mut App) {
        if let Some(key) = self.session_key(id).map(str::to_string)
            && let Some(store) = superzed_sessions::SessionStore::global(cx)
        {
            store.update(cx, |s, cx| s.user_interrupted(&key, cx));
        }
    }

    /// Il nodo mostra la vista d'insieme: nodo terminale (chat o shell) a uno zoom in
    /// cui il terminale non disegna il testo (`terminal_node::overview_at`).
    pub fn node_overview(&self, id: NodeId, cx: &App) -> bool {
        self.model.frame(id).is_some_and(|f| f.kind.is_terminal())
            && self.contents.contains_key(&id)
            && terminal_node::overview_at(self.viewport.zoom, cx)
    }

    pub fn session_key(&self, id: NodeId) -> Option<&str> {
        self.sessions.get(&id).map(|s| s.key.as_str())
    }

    /// Solo test: il testo dell'account nella barra del titolo del nodo (`render_node`).
    #[cfg(test)]
    pub(crate) fn node_account_text(&self, id: NodeId, cx: &App) -> Option<String> {
        let key = self.session_key(id)?;
        let store = superzed_sessions::SessionStore::global(cx)?;
        store.read(cx).get(key).map(|s| s.account.clone())
    }

    /// Nodo della sessione con questa chiave (anche se il PTY sta ancora partendo).
    pub fn node_for_session_key(&self, key: &str) -> Option<NodeId> {
        self.sessions
            .iter()
            .find(|(_, s)| s.key == key)
            .map(|(id, _)| *id)
            .or_else(|| self.pending_sessions.iter().find(|(_, k)| k.as_str() == key).map(|(id, _)| *id))
    }

    /// La sessione piu' urgente del canvas, per la campana sulla tab.
    pub fn attention(&self, cx: &App) -> superzed_sessions::Attention {
        let Some(store) = superzed_sessions::SessionStore::global(cx) else {
            return superzed_sessions::Attention::None;
        };
        let store = store.read(cx);
        // le chat di automazione lavorano in fondo: niente campana
        self.sessions
            .iter()
            .filter(|(id, _)| !self.automations.contains_key(id))
            .filter_map(|(_, s)| store.get(&s.key))
            .map(|s| superzed_sessions::attention(&s.state))
            .max()
            .unwrap_or(superzed_sessions::Attention::None)
    }

    // superzed: usato dalla vista Sessioni globale (test) per verificare il focus
    // dopo il cambio di tab.
    pub fn node_focus_handle(&self, id: NodeId, cx: &App) -> Option<FocusHandle> {
        self.contents.get(&id).map(|c| c.focus_handle(cx))
    }

    /// Card dei nodi claude con una sessione nello store, per il kanban.
    pub fn session_cards(&self, cx: &App) -> Vec<SessionCard> {
        let Some(store) = superzed_sessions::SessionStore::global(cx) else {
            return Vec::new();
        };
        let store = store.read(cx);
        let mut cards: Vec<SessionCard> = self
            .model
            .frames()
            .iter()
            .filter(|f| f.kind == NodeKind::Claude)
            .filter_map(|f| {
                let session = store.get(self.session_key(f.id)?)?.clone();
                Some(SessionCard { node: f.id, title: self.node_title(f, cx), session })
            })
            .collect();
        cards.sort_by_key(|c| c.node);
        cards
    }

    /// Spostamento a mano della card del nodo.
    pub fn set_session_phase(&mut self, node: NodeId, phase: superzed_sessions::Phase, cx: &mut Context<Self>) {
        if let (Some(key), Some(store)) =
            (self.session_key(node).map(str::to_string), superzed_sessions::SessionStore::global(cx))
        {
            // la pipeline Fatto → GitHub distingue uno spostamento a mano da un hook
            if store.read(cx).get(&key).is_some_and(|s| s.phase != phase) {
                self.done_pipeline.mark_manual(&key);
            }
            store.update(cx, |s, cx| s.set_phase(&key, phase, cx));
        }
    }

    fn drop_sessions(&mut self, ids: &[NodeId], cx: &mut Context<Self>) {
        self.done_pipeline.forget(ids);
        let keys: Vec<String> = ids.iter().filter_map(|id| self.sessions.remove(id)).map(|s| s.key).collect();
        if let (false, Some(store)) = (keys.is_empty(), superzed_sessions::SessionStore::global(cx)) {
            store.update(cx, |s, cx| keys.iter().for_each(|k| s.unregister(k, cx)));
        }
    }

    /// Come `remove_nodes` ma senza `on_close`: i processi sono gia' finiti.
    fn remove_nodes_after_exit(&mut self, ids: &[NodeId], cx: &mut Context<Self>) {
        self.free_chats_if_container_removed(ids, cx);
        self.drop_sessions(ids, cx);
        for id in ids {
            self.contents.remove(id);
            self.terminals.remove(id);
            self.node_subscriptions.remove(id);
            self.pending_sessions.remove(id);
            self.starting_terminals.remove(id);
            self.remove_automation(*id, cx);
            self.commit_closes.remove(id);
        }
        if !self.model.remove(ids).is_empty() {
            self.nodes_changed(cx);
        }
    }

    pub fn new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) -> NodeId {
        let note = NoteNode::new("", window, cx);
        let focus = note.focus_handle(cx);
        let editor = note.editor().clone();
        let id = self.add_node(NodeKind::Note, Box::new(note), size(NOTE_SIZE.0, NOTE_SIZE.1), "note", cx);
        self.watch_note(id, &editor, cx);
        focus.focus(window, cx);
        id
    }

    /// Pannello di una fonte al centro della vista, con il focus.
    pub fn new_source_panel(&mut self, connection: &str, window: &mut Window, cx: &mut Context<Self>) -> NodeId {
        let root = self.project_root_path(cx);
        let title = superzed_sources::ConnectionStore::global(cx)
            .and_then(|s| s.read(cx).get(connection).map(|c| c.name.clone()))
            .unwrap_or_else(|| connection.to_string());
        let connection = connection.to_string();
        let view = cx.new(|cx| SourcePanelView::new(connection, superzed_sources::Filter::new(), root, window, cx));
        let (w, h) = crate::source_node::SOURCE_SIZE;
        let id = self.add_node(NodeKind::Source, Box::new(SourceNode::new(view.clone())), size(w, h), title, cx);
        self.watch_source(id, &view, window, cx);
        self.focus_node(id, window, cx);
        id
    }

    pub fn mail_runner(cx: &App) -> Arc<dyn superzed_sources::Runner> {
        superzed_sources::ConnectionStore::global(cx)
            .map(|store| store.read(cx).runner())
            .unwrap_or_else(|| Arc::new(superzed_sources::ProcessRunner))
    }

    pub fn mail_node(&self) -> Option<NodeId> {
        self.model.frames().iter().find(|f| f.kind == NodeKind::Mail).map(|f| f.id)
    }

    pub fn mail_view(&self) -> Option<Entity<crate::mail_node::MailPanelView>> {
        let id = self.mail_node()?;
        self.mail_views.get(&id).cloned()
    }

    fn new_mail_view(
        &mut self,
        data: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::mail_node::MailPanelView> {
        let (account, query) = crate::mail_node::mail_data(data);
        let runner = Self::mail_runner(cx);
        cx.new(|cx| crate::mail_node::MailPanelView::new(account, query, runner, window, cx))
    }

    /// Il nodo Posta del progetto, creato se manca: a destra del kanban (sotto le
    /// Automazioni crescerebbe sopra di lui quando se ne aggiungono). Come i contenitori,
    /// nasce prima di `loaded` e da solo non scrive project.json.
    pub fn ensure_mail_node(&mut self, window: &mut Window, cx: &mut Context<Self>) -> NodeId {
        if let Some(id) = self.mail_node() {
            return id;
        }
        let visible = self.viewport.to_canvas(point(0., 0.));
        let origin = self
            .kanban_node()
            .and_then(|kanban| self.model.frame(kanban))
            .map(|f| point(f.origin.x + f.size.width + automations_layout::CONTAINER_GAP, f.origin.y))
            .unwrap_or(visible);
        let view = self.new_mail_view(&serde_json::Value::Null, window, cx);
        let (width, height) = crate::mail_node::MAIL_SIZE;
        let id = self.model.add(NodeKind::Mail, origin, size(width, height), crate::mail_node::MAIL_TITLE);
        self.contents.insert(id, Box::new(crate::mail_node::MailNode::new(view.clone())));
        self.apply_font_zoom(id, cx);
        self.watch_mail(id, &view, window, cx);
        cx.notify();
        id
    }

    fn watch_mail(
        &mut self,
        id: NodeId,
        view: &Entity<crate::mail_node::MailPanelView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mail_views.insert(id, view.clone());
        let subscription = cx.subscribe_in(view, window, |this, _, event: &crate::mail_node::MailPanelEvent, window, cx| match event {
            crate::mail_node::MailPanelEvent::SettingsChanged => this.schedule_save(cx),
            crate::mail_node::MailPanelEvent::OpenChat { prompt } => {
                this.new_claude_in_kanban_without_send(prompt.clone(), window, cx);
            }
        });
        self.node_subscriptions.insert(id, subscription);
    }

    /// Eventi del pannello: filtro cambiato → salvataggio; "Apri chat" → chat accanto al
    /// pannello o nel kanban. Il nodo si ritrova dalla vista (`renumber_node` puo' cambiarne l'id).
    fn watch_source(&mut self, id: NodeId, view: &Entity<SourcePanelView>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(view, window, |this, view, event: &SourcePanelEvent, window, cx| match event {
            SourcePanelEvent::FilterChanged => this.schedule_save(cx),
            SourcePanelEvent::OpenChat { prompt, in_kanban: true } => {
                this.new_claude_in_kanban_with_prompt(Some(prompt.clone()), window, cx);
            }
            SourcePanelEvent::OpenChat { prompt, in_kanban: false } => {
                let node = this
                    .contents
                    .iter()
                    .find(|(_, c)| c.view().entity_id() == view.entity_id())
                    .map(|(id, _)| *id);
                if let Some(node) = node {
                    this.open_chat_beside(node, prompt.clone(), window, cx);
                }
            }
        });
        self.node_subscriptions.insert(id, sub);
    }

    /// Chat claude nuova (scheduler degli account) a destra del nodo, con il prompt.
    pub fn open_chat_beside(&mut self, node: NodeId, prompt: String, window: &mut Window, cx: &mut Context<Self>) {
        self.next_claude_origin =
            self.model.frame(node).map(|f| point(f.origin.x + f.size.width + BESIDE_GAP, f.origin.y));
        self.open_new_claude(Some(&prompt), window, cx);
        // se `open_terminal_with_spec` e' uscito prima di leggerlo, non resta acceso
        self.next_claude_origin = None;
    }

    pub fn source_view(&self, id: NodeId) -> Option<Entity<SourcePanelView>> {
        self.contents.get(&id)?.view().downcast::<SourcePanelView>().ok()
    }

    pub fn project_sources(&self) -> &[String] {
        &self.project_sources
    }

    /// Spunta "usata da questo progetto" della vista Fonti.
    pub fn set_project_source(&mut self, id: &str, used: bool, cx: &mut Context<Self>) {
        // durante il caricamento il file sostituira' la lista: la modifica si riapplica dopo
        // e si salva a fine caricamento (`schedule_save` qui non fa niente)
        if self.persist.as_ref().is_some_and(|p| !p.loaded) {
            self.pending_source_changes.retain(|(s, _)| s != id);
            self.pending_source_changes.push((id.to_string(), used));
            self.save_after_load = true;
        }
        if !toggle_source(&mut self.project_sources, id, used) {
            return;
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub fn kanban_node(&self) -> Option<NodeId> {
        self.container_node(DockKind::Kanban)
    }

    /// Il contenitore di quel tipo, se c'e' (uno per tipo per canvas).
    pub fn container_node(&self, kind: DockKind) -> Option<NodeId> {
        let node_kind = kind.node_kind();
        self.model.frames().iter().find(|f| f.kind == node_kind).map(|f| f.id)
    }

    /// Chat claude agganciate a quel contenitore, in ordine di `NodeId`.
    pub fn dock_members(&self, kind: DockKind) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self
            .model
            .frames()
            .iter()
            .filter(|f| f.kind == NodeKind::Claude && f.dock == Some(kind))
            .map(|f| f.id)
            .collect();
        ids.sort();
        ids
    }

    /// Dimensione delle chat di quel contenitore (default se non c'e').
    pub fn chat_size(&self, kind: DockKind) -> Size<f32> {
        self.containers.get(&kind).map(|c| c.chat_size).unwrap_or(kanban_layout::DEFAULT_CHAT_SIZE)
    }

    /// Rem dell'interfaccia (lo stesso di `window.rem_size()`, che segue `ui_font_size`
    /// anche quando e' aggiustato senza salvarlo): serve anche dove non c'e' la finestra.
    pub(crate) fn ui_rem(&self, cx: &App) -> f32 {
        f32::from(theme_settings::ThemeSettings::get_global(cx).ui_font_size(cx))
    }

    /// Intestazione del kanban dal rem dell'interfaccia: la stessa per layout, resize,
    /// zoom sulla colonna e disegno.
    pub fn kanban_metrics(&self, cx: &App) -> kanban_layout::Metrics {
        kanban_layout::Metrics::for_rem(self.ui_rem(cx))
    }

    /// Rifa' il layout dei contenitori se le misure dal rem sono cambiate dall'ultima
    /// volta (impostazioni o font aggiustato senza salvarlo).
    pub(crate) fn sync_layout_metrics(&mut self, cx: &mut Context<Self>) {
        let metrics = (self.kanban_metrics(cx), self.automations_metrics(cx));
        if self.layout_metrics != Some(metrics) {
            self.layout_metrics = Some(metrics);
            self.relayout_containers(cx);
        }
    }

    pub fn set_chat_size(&mut self, kind: DockKind, size: Size<f32>) {
        self.containers.entry(kind).or_default().chat_size = size;
    }

    /// Il contenitore di cui `id` e' il nodo, se lo e'.
    fn container_kind_of(&self, id: NodeId) -> Option<DockKind> {
        DockKind::ALL.into_iter().find(|k| self.container_node(*k) == Some(id))
    }

    /// Un solo kanban per canvas: lo crea se manca, poi gli da' il focus.
    pub fn open_kanban(&mut self, window: &mut Window, cx: &mut Context<Self>) -> NodeId {
        let id = match self.kanban_node() {
            // esiste gia': panoramica, tutto il kanban in vista
            Some(id) => {
                self.relayout_kanban(cx);
                self.zoom_to_node(id, cx);
                id
            }
            None => {
                // un kanban nuovo parte dalla dimensione di default delle chat
                self.containers.insert(DockKind::Kanban, ContainerState::default());
                let node = crate::kanban_node::KanbanNode::new(cx.entity().downgrade(), cx);
                let id = self.add_node(NodeKind::Kanban, Box::new(node), size(KANBAN_SIZE.0, KANBAN_SIZE.1), "kanban", cx);
                // Others e Analysis stanno prima di New: senza lo spostamento il primo colpo
                // d'occhio mostrerebbe quelle e il + di New resterebbe fuori vista
                let chat_size = self.chat_size(DockKind::Kanban);
                if let Some(frame) = self.model.frame_mut(id) {
                    let origin = frame.origin;
                    frame.origin.x -= kanban_layout::column_origin(origin, superzed_sessions::Phase::New, chat_size).x - origin.x;
                }
                self.nodes_changed(cx);
                id
            }
        };
        self.focus_node(id, window, cx);
        self.relayout_kanban(cx);
        id
    }

    /// Chat claude agganciate al kanban, con la fase della loro sessione (`New` se
    /// la sessione non c'e' ancora). Un nodo ripristinato il cui PTY sta partendo ha
    /// la chiave solo in `pending_sessions`.
    pub fn docked_chats(&self, cx: &App) -> Vec<(NodeId, superzed_sessions::Phase)> {
        let store = superzed_sessions::SessionStore::global(cx);
        let store = store.as_ref().map(|s| s.read(cx));
        self.model
            .frames()
            .iter()
            .filter(|f| f.kind == NodeKind::Claude && f.in_kanban())
            .map(|f| {
                let key = self.session_key(f.id).or_else(|| self.pending_sessions.get(&f.id).map(String::as_str));
                let phase = key
                    .and_then(|k| store.and_then(|s| s.get(k)))
                    .map(|s| s.phase)
                    .unwrap_or(superzed_sessions::Phase::New);
                (f.id, phase)
            })
            .collect()
    }

    /// Dispone le chat agganciate nelle colonne della loro fase e ridimensiona il
    /// kanban. Durante un drag di nodi non tocca niente (si riposiziona al rilascio),
    /// tranne quando si trascina il kanban stesso: le sue chat lo seguono subito
    /// (Ruling 3). Una chat selezionata insieme al kanban si muove dello stesso delta,
    /// quindi resta comunque al suo posto nel layout.
    pub fn relayout_kanban(&mut self, cx: &mut Context<Self>) {
        let Some(kanban) = self.kanban_node() else {
            return;
        };
        if matches!(self.drag, Some(DragState::Move { .. })) && !self.model.is_selected(kanban) {
            return;
        }
        let origin = self.model.frame(kanban).map(|f| f.origin).unwrap_or_default();
        let chats = self.docked_chats(cx);
        let follow = self.column_change_to_follow(&chats, cx);
        let chat_size = self.chat_size(DockKind::Kanban);
        let layout = kanban_layout::layout(origin, &chats, chat_size, self.kanban_metrics(cx));
        let mut changed = false;
        if let Some(f) = self.model.frame_mut(kanban)
            && f.size != layout.size
        {
            f.size = layout.size;
            changed = true;
        }
        for (id, pos) in layout.positions {
            if let Some(f) = self.model.frame_mut(id)
                && (f.origin != pos || f.size != chat_size)
            {
                f.origin = pos;
                f.size = chat_size;
                changed = true;
            }
        }
        self.model.send_to_back(kanban);
        if changed {
            cx.notify();
            self.schedule_save(cx);
        }
        if let Some(id) = follow {
            self.follow_chat(id, cx);
        }
    }

    /// Dispone le chat delle Automazioni in una riga sotto le loro fasce e ridimensiona la
    /// sezione (stesse regole del kanban durante un drag). Se il kanban e' cresciuto fin
    /// dentro la sezione (o nel margine sotto di lui), la sezione scende sotto il kanban.
    pub fn relayout_automations(&mut self, cx: &mut Context<Self>) {
        let Some(section) = self.container_node(DockKind::Automations) else {
            return;
        };
        if matches!(self.drag, Some(DragState::Move { .. })) && !self.model.is_selected(section) {
            return;
        }
        let Some(mut origin) = self.model.frame(section).map(|f| f.origin) else {
            return;
        };
        let chats = self.dock_members(DockKind::Automations);
        let chat_size = self.chat_size(DockKind::Automations);
        let metrics = self.automations_metrics(cx);
        let mut layout = automations_layout::layout(origin, &chats, chat_size, metrics);
        let kanban = self.kanban_node().and_then(|k| self.model.frame(k)).map(|f| (f.origin, f.size));
        if let Some((ko, ks)) = kanban
            && self.drag.is_none()
            && automations_layout::overlaps_below(ko, ks, origin, layout.size)
        {
            origin = automations_layout::below(ko, ks);
            layout = automations_layout::layout(origin, &chats, chat_size, metrics);
        }
        let mut changed = false;
        if let Some(f) = self.model.frame_mut(section)
            && (f.origin != origin || f.size != layout.size)
        {
            f.origin = origin;
            f.size = layout.size;
            changed = true;
        }
        for (id, pos) in layout.chats {
            if let Some(f) = self.model.frame_mut(id)
                && (f.origin != pos || f.size != chat_size)
            {
                f.origin = pos;
                f.size = chat_size;
                changed = true;
            }
        }
        self.model.send_to_back(section);
        if changed {
            cx.notify();
            self.schedule_save(cx);
        }
    }

    /// Layout di tutti i contenitori: prima il kanban, poi le Automazioni (che dipendono
    /// da dove finisce il kanban).
    pub fn relayout_containers(&mut self, cx: &mut Context<Self>) {
        self.relayout_kanban(cx);
        self.relayout_automations(cx);
    }

    /// Aggiunge il nodo di un contenitore vuoto in `origin`, sotto tutti gli altri. Non
    /// chiama `nodes_changed`: niente relayout ne' salvataggio qui.
    fn insert_container(&mut self, kind: DockKind, origin: Point<f32>, cx: &mut Context<Self>) -> NodeId {
        let d = kanban_layout::DEFAULT_CHAT_SIZE;
        let (node_size, title) = match kind {
            DockKind::Kanban => (kanban_layout::layout(origin, &[], d, self.kanban_metrics(cx)).size, "kanban"),
            DockKind::Automations => (automations_layout::layout(origin, &[], d, self.automations_metrics(cx)).size, "automations"),
        };
        let id = self.model.add(kind.node_kind(), origin, node_size, title);
        let canvas = cx.entity().downgrade();
        let content: Box<dyn NodeContent> = match kind {
            DockKind::Kanban => Box::new(crate::kanban_node::KanbanNode::new(canvas, cx)),
            DockKind::Automations => Box::new(crate::automations_node::AutomationsNode::new(canvas, cx)),
        };
        self.contents.insert(id, content);
        self.containers.insert(kind, ContainerState::default());
        self.apply_font_zoom(id, cx);
        self.model.clear_selection();
        self.model.send_to_back(id);
        cx.notify();
        id
    }

    /// Il contenitore di quel tipo, creato se manca (senza focus e senza un salvataggio
    /// suo): il kanban in alto a sinistra della vista, le Automazioni sotto il kanban (o
    /// in alto a sinistra della vista se il kanban non c'e').
    pub fn ensure_container(&mut self, kind: DockKind, cx: &mut Context<Self>) -> NodeId {
        if let Some(id) = self.container_node(kind) {
            return id;
        }
        let visible = self.viewport.to_canvas(point(0., 0.));
        let origin = match kind {
            DockKind::Kanban => visible,
            DockKind::Automations => self
                .kanban_node()
                .and_then(|k| self.model.frame(k))
                .map(|f| automations_layout::below(f.origin, f.size))
                .unwrap_or(visible),
        };
        let id = self.insert_container(kind, origin, cx);
        self.relayout_containers(cx);
        id
    }

    /// Aggancia una chat claude al kanban nella fase data (spostamento a mano).
    pub fn dock(&mut self, node: NodeId, phase: superzed_sessions::Phase, cx: &mut Context<Self>) {
        let has_kanban = self.kanban_node().is_some();
        let Some(f) = self.model.frame_mut(node) else { return };
        if f.kind != NodeKind::Claude || !has_kanban {
            return;
        }
        f.dock = Some(DockKind::Kanban);
        self.set_session_phase(node, phase, cx);
        // `nodes_changed` rifa il layout (e `relayout_kanban` non chiama `nodes_changed`)
        self.nodes_changed(cx);
    }

    /// Sgancia la chat: resta dov'e', torna alla dimensione di un terminale.
    pub fn undock(&mut self, node: NodeId, cx: &mut Context<Self>) {
        if let Some(f) = self.model.frame_mut(node)
            && f.in_kanban()
        {
            f.dock = None;
            f.size = size(TERMINAL_SIZE.0, TERMINAL_SIZE.1);
            // fuori dal kanban: niente report (ne' prompt in una chat ormai libera)
            self.done_pipeline.forget(&[node]);
            self.nodes_changed(cx);
        }
    }

    /// Nuova chat claude agganciata al kanban, in Nuove (pulsante + e cmd-n nel kanban).
    /// Crea il kanban se manca; il focus va alla chat nuova quando il PTY e' pronto.
    pub fn new_claude_in_kanban(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_claude_in_kanban_with_prompt(None, window, cx);
    }

    /// Chat nel kanban che con gog puo' leggere e creare bozze ma non inviare: l'invio e'
    /// solo il clic nel nodo Posta.
    pub fn new_claude_in_kanban_without_send(&mut self, prompt: String, window: &mut Window, cx: &mut Context<Self>) -> String {
        self.next_claude_exports = no_send_exports();
        self.new_claude_in_kanban_with_prompt(Some(prompt), window, cx)
    }

    /// Chat nuova nel kanban in Nuove, con un prompt iniziale (Risolvi di una notifica).
    /// Restituisce la chiave di sessione della chat: il suo nodo, quando c'e', lo trova
    /// `node_for_session_key`.
    pub fn new_claude_in_kanban_with_prompt(&mut self, prompt: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> String {
        if self.kanban_node().is_none() {
            self.open_kanban(window, cx);
        }
        self.dock_next_claude = Some(DockKind::Kanban);
        let key = self.open_new_claude(prompt.as_deref(), window, cx);
        // se `open_terminal_with_spec` e' uscito prima di leggerlo (niente workspace), non resta acceso
        self.dock_next_claude = None;
        key
    }

    /// Porta in vista l'angolo in alto a sinistra del nodo (barra del titolo e un po' di
    /// contenuto) se cade fuori dal canvas visibile; niente se e' gia' visibile.
    pub fn reveal(&mut self, id: NodeId, cx: &mut Context<Self>) {
        const MARGIN: f32 = 16.;
        let Some(f) = self.model.frame(id) else {
            return;
        };
        let bounds = self.canvas_bounds.get().size;
        let (w, h) = (f32::from(bounds.width), f32::from(bounds.height));
        if w <= 2. * MARGIN || h <= 2. * MARGIN {
            return;
        }
        let zoom = self.viewport.zoom;
        let p = self.viewport.to_screen(f.origin);
        // l'area da mostrare: il nodo, ridotto alla viewport meno i margini
        let (aw, ah) = ((f.size.width * zoom).min(w - 2. * MARGIN), (f.size.height * zoom).min(h - 2. * MARGIN));
        let shift = |start: f32, len: f32, view: f32| {
            if start < 0. {
                MARGIN - start
            } else if start + len > view {
                view - MARGIN - (start + len)
            } else {
                0.
            }
        };
        let (dx, dy) = (shift(p.x, aw, w), shift(p.y, ah, h));
        if dx != 0. || dy != 0. {
            self.viewport.pan(point(dx, dy));
            self.viewport_changed(cx);
        }
    }

    /// Porta in vista un rettangolo del canvas, centrato: a zoom 1 se ci sta con il
    /// margine, altrimenti allo zoom che lo fa stare (`viewport::fit_rect`).
    pub fn zoom_to_rect(&mut self, origin: Point<f32>, size: Size<f32>, cx: &mut Context<Self>) {
        // un fit sostituisce qualunque pan di navigazione ancora in corso, altrimenti
        // continuerebbe ad aggiungere il suo resto e sfaserebbe il centraggio
        self.nav_pan = Task::ready(());
        let view = self.canvas_bounds.get().size;
        let view = gpui::size(f32::from(view.width), f32::from(view.height));
        let Some(v) = viewport::fit_rect(origin, size, view, viewport::FIT_MARGIN, 1.0, self.min_zoom()) else {
            return;
        };
        self.viewport = v;
        self.apply_font_zoom_all(cx);
        self.viewport_changed(cx);
    }

    /// Doppio click sul titolo e `ZoomToFocusedNode`: il nodo centrato.
    pub fn zoom_to_node(&mut self, id: NodeId, cx: &mut Context<Self>) {
        if let Some(f) = self.model.frame(id) {
            let (origin, size) = (f.origin, f.size);
            self.zoom_to_rect(origin, size, cx);
        }
    }

    /// `zoom_to_node` anche per un canvas mai disegnato: senza limiti (vuoti finche' il
    /// canvas non passa dal prepaint) lo zoom resta in attesa e si applica al frame dopo
    /// quello che li registra. Una nuova richiesta sostituisce quella in attesa; la
    /// rimozione del nodo la cancella.
    pub fn zoom_to_node_when_ready(&mut self, id: NodeId, cx: &mut Context<Self>) {
        let b = self.canvas_bounds.get().size;
        if b.width > px(0.) && b.height > px(0.) {
            self.pending_zoom.set(None);
            self.zoom_to_node(id, cx);
        } else {
            self.pending_zoom.set(Some(id));
        }
    }

    /// Il nodo con il focus (o il primo selezionato) centrato.
    pub fn zoom_to_focused_node(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(id) = self.target_nodes(window, cx).first().copied() {
            self.zoom_to_node(id, cx);
        }
    }

    /// Doppio click sull'intestazione di una colonna del kanban: la colonna (intestazione
    /// e chat) in vista.
    pub fn zoom_to_kanban_column(&mut self, phase: superzed_sessions::Phase, cx: &mut Context<Self>) {
        let Some(origin) = self.kanban_node().and_then(|k| self.model.frame(k)).map(|f| f.origin) else {
            return;
        };
        let count = self.docked_chats(cx).iter().filter(|(_, p)| *p == phase).count();
        let (o, s) =
            kanban_layout::column_rect(origin, phase, self.chat_size(DockKind::Kanban), count, self.kanban_metrics(cx));
        self.zoom_to_rect(o, s, cx);
    }

    /// Il focus e' nel kanban o in una chat agganciata.
    pub fn focus_in_kanban(&self, window: &Window, cx: &App) -> bool {
        let Some(kanban) = self.kanban_node() else {
            return false;
        };
        self.model
            .frames()
            .iter()
            .filter(|f| f.id == kanban || (f.kind == NodeKind::Claude && f.in_kanban()))
            .any(|f| self.contents.get(&f.id).is_some_and(|c| c.focus_handle(cx).contains_focused(window, cx)))
    }

    /// Sposta la chat agganciata di una colonna (avanti o indietro); niente se non e'
    /// agganciata o e' gia' al bordo.
    pub fn move_docked(&mut self, node: NodeId, forward: bool, cx: &mut Context<Self>) {
        if self.kanban_node().is_none() {
            return;
        }
        let Some(phase) = self.docked_chats(cx).into_iter().find(|(id, _)| *id == node).map(|(_, p)| p) else {
            return;
        };
        let Some(target) = (if forward { phase.next() } else { phase.prev() }) else {
            return;
        };
        self.set_session_phase(node, target, cx);
        self.relayout_kanban(cx);
    }

    /// `move_docked` per ogni chat agganciata selezionata (focus sul kanban).
    fn move_selected_docked(&mut self, forward: bool, cx: &mut Context<Self>) {
        let selected: Vec<NodeId> = self.model.selection().iter().copied().collect();
        for id in selected {
            self.move_docked(id, forward, cx);
        }
    }

    /// Un contenitore sta per essere chiuso: le sue chat tornano libere dove sono, con la
    /// loro dimensione (affiancate come nel layout, senza sovrapporsi).
    fn free_chats_if_container_removed(&mut self, ids: &[NodeId], cx: &mut Context<Self>) {
        for kind in DockKind::ALL {
            if !self.container_node(kind).is_some_and(|c| ids.contains(&c)) {
                continue;
            }
            // il prossimo contenitore di quel tipo parte dal default
            self.containers.remove(&kind);
            let docked: Vec<NodeId> =
                self.model.frames().iter().filter(|f| f.dock == Some(kind)).map(|f| f.id).collect();
            // chat tornate libere: la pipeline Fatto → GitHub si ferma
            self.done_pipeline.forget(&docked);
            let mut dropped = 0;
            for id in docked {
                if let Some(f) = self.model.frame_mut(id) {
                    f.dock = None;
                }
                // senza la sezione le chat delle Automazioni perdono la definizione
                if kind == DockKind::Automations && self.automations.remove(&id).is_some() {
                    self.set_automation_background(id, false, cx);
                    dropped += 1;
                }
            }
            if dropped > 0 {
                self.warn(format!("Automations removed: {dropped}"), cx);
            }
        }
    }

    /// Resize di un contenitore dalla maniglia: la dimensione chiesta diventa quella delle
    /// chat (al minimo); il layout ricalcola poi il contenitore esatto e ridispone le chat.
    fn resize_container(&mut self, kind: DockKind, requested: Size<f32>, rows: usize, cx: &mut Context<Self>) {
        match kind {
            DockKind::Kanban => {
                let metrics = self.kanban_metrics(cx);
                self.set_chat_size(kind, kanban_layout::chat_size_for(requested, rows, metrics));
                self.nodes_changed(cx);
            }
            DockKind::Automations => {
                let metrics = self.automations_metrics(cx);
                self.set_chat_size(kind, automations_layout::chat_size_for(requested, rows, metrics));
                self.nodes_changed(cx);
            }
        }
    }

    /// Primo piano, ma un contenitore resta sempre sotto le sue chat.
    fn bring_to_front(&mut self, id: NodeId) {
        if self.container_kind_of(id).is_some() {
            self.model.send_to_back(id);
        } else {
            self.model.bring_to_front(id);
        }
    }

    #[cfg(test)]
    pub(crate) fn note_editor(&self, id: NodeId) -> Option<Entity<Editor>> {
        // solo per i test: i NoteNode espongono il loro editor
        self.contents
            .get(&id)
            .and_then(|c| c.view().downcast::<Editor>().ok())
    }

    pub(crate) fn node_save_data(&self, id: NodeId, cx: &App) -> serde_json::Value {
        // la dimensione delle chat e' del contenitore: sta nel suo `data`
        if let Some(kind) = self.container_kind_of(id) {
            let state = self.containers.get(&kind).cloned().unwrap_or_default();
            return kanban_layout::chat_size_to_data(&state.data, state.chat_size);
        }
        // una chat delle Automazioni porta con se' la sua definizione
        let value = self.node_content_data(id, cx);
        crate::automation::data_with_automation(value, self.automation(id))
    }

    fn node_content_data(&self, id: NodeId, cx: &App) -> serde_json::Value {
        if let Some(content) = self.contents.get(&id) {
            return content.save_data(cx);
        }
        // Fix round 1 (Task 6 review, finding 2): il nodo restaurato non ha ancora un
        // `TerminalNode` (il PTY sta ancora partendo) ma la sua sessione e' gia' nello
        // store: si legge da li', come fa `TerminalNode::save_data` una volta agganciato.
        if let Some(key) = self.pending_sessions.get(&id) {
            return superzed_sessions::SessionStore::global(cx)
                .and_then(|s| s.read(cx).get(key).map(|session| session.save_data()))
                .unwrap_or(serde_json::Value::Null);
        }
        serde_json::Value::Null
    }

    /// Zoom minimo del canvas: dove la vista d'insieme di ogni chat/terminale ha ancora
    /// testo (`viewport::min_zoom_for`). Vale per ogni zoom out e per il fit.
    pub fn min_zoom(&self) -> f32 {
        viewport::min_zoom_for(self.model.frames().iter().filter(|f| f.kind.is_terminal()).map(|f| f.size))
    }

    /// Unico punto dello zoom per rotella, pinch e scorciatoie: il minimo si applica qui.
    pub fn zoom_by(&mut self, anchor_local: Point<f32>, factor: f32, cx: &mut Context<Self>) {
        // idem: uno zoom (rotella, pinch, scorciatoia) cancella un pan di navigazione
        // ancora in corso invece di sommarcisi per il resto dei suoi 160 ms
        self.nav_pan = Task::ready(());
        let min_zoom = self.min_zoom();
        self.viewport.zoom_at(anchor_local, factor, min_zoom);
        self.apply_font_zoom_all(cx);
        self.viewport_changed(cx);
    }

    pub fn zoom_reset(&mut self, cx: &mut Context<Self>) {
        self.nav_pan = Task::ready(());
        let anchor = self.local_center();
        self.viewport.reset_zoom_at(anchor);
        self.apply_font_zoom_all(cx);
        self.viewport_changed(cx);
    }

    fn apply_font_zoom(&self, id: NodeId, cx: &mut App) {
        // lo zoom e' solo del canvas: ogni nodo ha il font allo zoom del canvas
        if let Some(content) = self.contents.get(&id) {
            content.set_font_zoom(self.viewport.zoom, cx);
        }
    }

    fn apply_font_zoom_all(&self, cx: &mut App) {
        for frame in self.model.frames() {
            if let Some(content) = self.contents.get(&frame.id) {
                content.set_font_zoom(self.viewport.zoom, cx);
            }
        }
    }

    /// Nodi terminale in ordine di creazione (id), per la vista Sessioni: non lo
    /// z-order, che il click su una riga cambia.
    pub fn terminal_nodes(&self, cx: &App) -> Vec<(NodeId, String)> {
        let mut nodes: Vec<_> = self
            .model
            .frames()
            .iter()
            .filter(|f| f.kind.is_terminal())
            .map(|f| (f.id, self.node_title(f, cx)))
            .collect();
        nodes.sort_by_key(|(id, _)| *id);
        nodes
    }

    /// Titolo mostrato: per un nodo claude mai rinominato, quello che claude da' al
    /// terminale; altrimenti il titolo del nodo.
    pub fn node_title(&self, frame: &NodeFrame, cx: &App) -> String {
        if frame.kind == NodeKind::Claude
            && frame.title == "claude"
            && let Some((_, terminal)) = self.terminals.get(&frame.id)
            && let Some(title) = terminal_node::claude_title(&terminal.read(cx).breadcrumb_text)
        {
            return title;
        }
        legacy_title(frame.kind, &frame.title).to_string()
    }

    /// Seleziona il nodo, lo porta in primo piano e gli da' il focus.
    pub fn focus_node(&mut self, id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(content) = self.contents.get(&id) else {
            return;
        };
        let focus = content.focus_handle(cx);
        self.model.select(id, false);
        self.bring_to_front(id);
        focus.focus(window, cx);
        cx.notify();
    }

    /// Qualcosa nei nodi e' cambiato (aggiunta, rimozione, frame). Nel Task 9
    /// qui si programma anche il salvataggio.
    pub(crate) fn nodes_changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        self.schedule_save(cx);
        // non ricorsivo: `relayout_containers` non chiama `nodes_changed`
        self.relayout_containers(cx);
    }

    pub(crate) fn viewport_changed(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        self.schedule_save(cx);
    }

    /// Scroll in fase Bubble: cmd+scroll zooma. Il pan si decide prima, in cattura
    /// (`scroll_route`); qui arriva solo lo scroll che nessun nodo ha tenuto.
    fn on_scroll(&mut self, e: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        if e.modifiers.platform {
            let delta = e.delta.pixel_delta(window.line_height());
            if let Some(factor) = viewport::scroll_zoom_factor(f32::from(delta.y)) {
                let anchor = self.window_to_local(e.position);
                self.zoom_by(anchor, factor, cx);
            }
        } else {
            self.pan_by_scroll(e, window, cx);
        }
    }

    fn on_pinch(&mut self, e: &PinchEvent, _: &mut Window, cx: &mut Context<Self>) {
        // capture_pinch ignora l'hitbox: filtrare sui bounds del canvas
        if !self.canvas_bounds.get().contains(&e.position) {
            return;
        }
        let anchor = self.window_to_local(e.position);
        self.zoom_by(anchor, viewport::pinch_factor(e.delta), cx);
    }

    fn on_key_down(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // solo con il focus sul canvas stesso: lo spazio scritto in una nota o in un
        // terminale non deve attivare il pan
        if e.keystroke.key == "space" && self.focus_handle.is_focused(window) {
            if !e.is_held {
                self.space_held = true;
            }
            cx.stop_propagation();
        }
    }

    fn on_key_up(&mut self, e: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if e.keystroke.key == "space" {
            self.space_held = false;
        }
    }

    fn on_background_mouse_down(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        if self.space_held {
            // un pan a mano cancella un pan di navigazione ancora in corso, altrimenti
            // i due si sommerebbero per il resto dei suoi 160 ms
            self.nav_pan = Task::ready(());
            self.drag = Some(DragState::Pan { last: e.position });
        } else {
            self.model.clear_selection();
        }
        cx.notify();
    }

    /// Chiamato dal listener di finestra: riceve i movimenti anche sopra i nodi
    /// e fuori dal canvas finche' il trascinamento e' attivo.
    pub(crate) fn on_drag_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            // mouse up perso (es. fuori dalla finestra): il trascinamento finisce qui
            self.drag = None;
            return;
        }
        let zoom = self.viewport.zoom;
        match drag {
            DragState::Pan { last } => {
                let delta = e.position - *last;
                *last = e.position;
                self.viewport.pan(to_f32(delta));
                self.viewport_changed(cx);
            }
            DragState::Move { last, moved } => {
                let delta = to_f32(e.position - *last);
                *last = e.position;
                if delta.x != 0. || delta.y != 0. {
                    *moved = true;
                }
                self.model.move_selected(point(delta.x / zoom, delta.y / zoom));
                self.nodes_changed(cx);
            }
            DragState::ResizeContainer { kind, start_mouse, start_size, rows } => {
                let d = to_f32(e.position - *start_mouse);
                let (kind, start_size, rows) = (*kind, *start_size, *rows);
                let requested = size(start_size.width + d.x / zoom, start_size.height + d.y / zoom);
                if self.container_node(kind).is_some() {
                    self.resize_container(kind, requested, rows, cx);
                }
            }
            DragState::ResizeNode { id, start_mouse, start_size, min } => {
                let d = to_f32(e.position - *start_mouse);
                let requested =
                    size((start_size.width + d.x / zoom).max(min.0), (start_size.height + d.y / zoom).max(min.1));
                if let Some(frame) = self.model.frame_mut(*id) {
                    frame.size = requested;
                }
                self.nodes_changed(cx);
            }
        }
    }

    pub(crate) fn end_drag(&mut self, _: &MouseUpEvent, cx: &mut Context<Self>) {
        self.finish_node_drag(cx);
    }

    /// Fine di un drag. Per un `Move`, ogni chat claude selezionata il cui centro del
    /// titolo cade su una colonna del kanban vi si aggancia con quella fase (spostamento
    /// a mano); fuori dal kanban, se era agganciata, torna libera. Poi il layout rimette
    /// le chat agganciate al loro posto.
    pub(crate) fn finish_node_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        // i relayout del rilascio non spostano la visuale (Task 15)
        self.dropping = true;
        self.finish_node_drag_inner(drag, cx);
        self.dropping = false;
    }

    fn finish_node_drag_inner(&mut self, drag: DragState, cx: &mut Context<Self>) {
        cx.notify();
        // un Move senza movimento (click sul titolo) non e' un rilascio: niente aggancio
        let moved_nodes = matches!(
            drag,
            DragState::Move { moved: true, .. } | DragState::ResizeContainer { .. } | DragState::ResizeNode { .. }
        );
        // una chat delle Automazioni portata fuori dalla sezione si libera e perde la
        // definizione; dentro la sezione il layout la rimette al suo posto. Prima del
        // kanban: liberata, se cade su una colonna vi si aggancia come ogni chat libera.
        if matches!(drag, DragState::Move { moved: true, .. })
            && let Some((a_origin, a_size)) = self
                .container_node(DockKind::Automations)
                .and_then(|a| self.model.frame(a))
                .map(|f| (f.origin, f.size))
        {
            let section = Bounds::new(a_origin, a_size);
            let released: Vec<NodeId> = self
                .model
                .frames()
                .iter()
                .filter(|f| {
                    f.kind == NodeKind::Claude
                        && self.model.is_selected(f.id)
                        && f.dock == Some(DockKind::Automations)
                        && !section.contains(&point(f.origin.x + f.size.width / 2., f.origin.y + 10.))
                })
                .map(|f| f.id)
                .collect();
            for id in released {
                self.remove_automation(id, cx);
                if let Some(f) = self.model.frame_mut(id) {
                    f.dock = None;
                    f.size = size(TERMINAL_SIZE.0, TERMINAL_SIZE.1);
                }
            }
        }
        if matches!(drag, DragState::Move { moved: true, .. }) {
            // prima si decide per tutte, poi si applica: `dock`/`undock` rifanno il
            // layout e sposterebbero le altre chat selezionate non ancora valutate
            let drops = self.kanban_drops();
            for (id, column, docked) in drops {
                match column {
                    Some(phase) => self.dock(id, phase, cx),
                    None if docked => self.undock(id, cx),
                    None => {}
                }
            }
        }
        // una chat agganciata trascinata torna al suo posto nella colonna; un pan non
        // ha cambiato i nodi e non va salvato
        if moved_nodes {
            self.nodes_changed(cx);
        } else {
            self.relayout_containers(cx);
        }
    }

    /// Dove andrebbe al rilascio ogni chat claude selezionata: `(chat, colonna del kanban
    /// sotto il centro del suo titolo, era agganciata)`. Serve al rilascio e, durante il
    /// drag, all'evidenziazione della colonna: le due coincidono.
    fn kanban_drops(&self) -> Vec<(NodeId, Option<superzed_sessions::Phase>, bool)> {
        let Some((k_origin, k_size)) = self.kanban_node().and_then(|k| self.model.frame(k)).map(|f| (f.origin, f.size))
        else {
            return Vec::new();
        };
        let chat_size = self.chat_size(DockKind::Kanban);
        let section = self
            .container_node(DockKind::Automations)
            .and_then(|a| self.model.frame(a))
            .map(|f| Bounds::new(f.origin, f.size));
        self.model
            .frames()
            .iter()
            .filter(|f| f.kind == NodeKind::Claude && self.model.is_selected(f.id))
            .filter_map(|f| {
                let mut width = f.size.width;
                if f.dock == Some(DockKind::Automations) {
                    // una chat rimasta nelle Automazioni torna al suo posto col layout;
                    // portata fuori si libera (dimensione di un terminale) e poi conta
                    // come le altre. Una chat libera sopra le Automazioni resta libera.
                    let title = point(f.origin.x + f.size.width / 2., f.origin.y + 10.);
                    if !section.is_some_and(|s| !s.contains(&title)) {
                        return None;
                    }
                    width = TERMINAL_SIZE.0;
                }
                let title = point(f.origin.x + width / 2., f.origin.y + 10.);
                Some((f.id, kanban_layout::column_at(k_origin, k_size, chat_size, title), f.in_kanban()))
            })
            .collect()
    }

    /// Colonne del kanban dove cadrebbero le chat trascinate (vuoto senza drag).
    pub(crate) fn kanban_drop_columns(&self) -> Vec<superzed_sessions::Phase> {
        if !matches!(self.drag, Some(DragState::Move { moved: true, .. })) {
            return Vec::new();
        }
        let columns: Vec<_> = self.kanban_drops().into_iter().filter_map(|(_, column, _)| column).collect();
        superzed_sessions::Phase::ALL.into_iter().filter(|p| columns.contains(p)).collect()
    }

    fn on_title_mouse_down(
        &mut self,
        id: NodeId,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // doppio click sul titolo: il nodo in vista, centrato (rinomina: menu o F2)
        if e.click_count == 2 {
            self.focus_node(id, window, cx);
            self.zoom_to_node(id, cx);
            return;
        }
        // superzed: la selezione (anche shift-additiva) e' gia' applicata da
        // `on_body_mouse_down` in fase capture, che gira anche per i click sul titolo;
        // rifarla qui raddoppierebbe il toggle additivo di uno shift-click,
        // annullandolo (deviazione dal codice del brief, trovata dal test
        // `shift_click_extends_selection_and_drag_moves_all_selected`).
        self.bring_to_front(id);
        self.focus_handle.focus(window, cx);
        if self.model.is_selected(id) {
            self.drag = Some(DragState::Move { last: e.position, moved: false });
        }
        cx.notify();
    }

    /// Mouse-down sulla maniglia di un contenitore (la dimensione delle sue chat) o di un
    /// nodo libero (la sua dimensione); le chat agganciate non hanno resize.
    fn on_resize_mouse_down(&mut self, id: NodeId, e: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some((node_kind, start_size, resizable)) =
            self.model.frame(id).map(|f| (f.kind, f.size, free_resizable(f)))
        else {
            return;
        };
        let Some(kind) = self.container_kind_of(id) else {
            if resizable {
                let min = min_free_size(node_kind);
                self.drag = Some(DragState::ResizeNode { id, start_mouse: e.position, start_size, min });
                self.bring_to_front(id);
                cx.notify();
            }
            return;
        };
        let rows = match kind {
            DockKind::Kanban => kanban_layout::rows(&self.docked_chats(cx)),
            // una riga sola: la larghezza si divide fra le sue chat
            DockKind::Automations => self.dock_members(DockKind::Automations).len(),
        };
        self.drag = Some(DragState::ResizeContainer { kind, start_mouse: e.position, start_size, rows });
        self.bring_to_front(id);
        cx.notify();
    }

    /// Click nel contenuto: selezione e primo piano, senza fermare l'evento
    /// (il terminale o l'editor devono ricevere il click).
    fn on_body_mouse_down(&mut self, id: NodeId, e: &MouseDownEvent, cx: &mut Context<Self>) {
        // spazio + trascinamento sul kanban fa pan come sullo sfondo (il kanban copre
        // spesso tutta la viewport)
        if self.space_held
            && e.button == MouseButton::Left
            && self.model.frame(id).is_some_and(|f| matches!(f.kind, NodeKind::Kanban | NodeKind::Automations))
        {
            // come nel pan sullo sfondo (Task 15): un pan a mano cancella un pan di
            // navigazione ancora in corso, altrimenti i due si sommerebbero
            self.nav_pan = Task::ready(());
            self.drag = Some(DragState::Pan { last: e.position });
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if e.modifiers.shift {
            self.model.select(id, true);
        } else if !self.model.is_selected(id) {
            self.model.select(id, false);
        }
        self.bring_to_front(id);
        cx.notify();
    }

    /// Nodo che contiene il focus (es. cmd-w dentro un terminale), altrimenti la selezione.
    pub fn target_nodes(&self, window: &Window, cx: &App) -> Vec<NodeId> {
        let focused = self
            .model
            .frames()
            .iter()
            .rev()
            .find(|f| {
                self.contents
                    .get(&f.id)
                    .is_some_and(|c| c.focus_handle(cx).contains_focused(window, cx))
            })
            .map(|f| f.id);
        match focused {
            Some(id) => vec![id],
            None => self.model.selection().iter().copied().collect(),
        }
    }

    pub fn close_nodes_with_confirm(
        &mut self,
        ids: Vec<NodeId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // il nodo Posta e' fisso: chiuderlo toglierebbe la posta al progetto
        let ids: Vec<NodeId> = ids
            .into_iter()
            .filter(|id| self.model.frame(*id).is_none_or(|f| f.kind != NodeKind::Mail))
            .collect();
        if ids.is_empty() {
            return;
        }
        let (clean, dirty) = commit_close::split_dirty(&ids, |id| self.node_uncommitted_files(id, cx).len());
        if !dirty.is_empty() {
            self.confirm_close_dirty(clean, dirty, window, cx);
            return;
        }
        let nodes: Vec<(String, bool)> = ids
            .iter()
            .map(|id| match self.model.frame(*id) {
                Some(f) => (self.node_title(f, cx), f.kind.is_terminal()),
                None => (String::new(), false),
            })
            .collect();
        let message = close_confirm_message(&nodes);
        // la vicina si sceglie ora: dopo la chiusura il kanban si ridispone
        let landing = self.landing_after_close(&ids, window, cx);
        let canvas = cx.entity().downgrade();
        let Some(workspace) = self.workspace.upgrade() else {
            // senza workspace (test del canvas da solo) non c'e' dove mostrare la conferma
            return;
        };
        workspace.update(cx, |ws, cx| {
            workspace::superzed_confirm::superzed_confirm_action(
                ws,
                message,
                "Close",
                true,
                move |window, cx| {
                    canvas
                        .update(cx, |c, cx| {
                            c.remove_nodes(&ids, cx);
                            match landing.filter(|id| c.model.frame(*id).is_some()) {
                                // la vicina prende il focus e viene inquadrata a zoom 1
                                // (come il doppio clic sul titolo), non solo centrata
                                Some(next) => {
                                    c.land_on(next, window, cx);
                                    c.zoom_to_node(next, cx);
                                }
                                None => c.focus_handle.focus(window, cx),
                            }
                        })
                        .ok();
                },
                window,
                cx,
            )
        });
    }

    /// cmd-shift-w: propone di chiudere le chat del progetto ne' golden ne' attive
    /// nell'ultima ora (`idle_chats`), con «Close project» come bottone secondario
    /// (invio e y vanno li', come prima di questa proposta).
    pub fn close_idle_chats_with_confirm(
        &mut self,
        project: SharedString,
        close_project: Box<dyn FnOnce(&mut Window, &mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = std::time::Instant::now();
        let now_unix = superzed_sessions::now_unix();
        let store = superzed_sessions::SessionStore::global(cx);
        let plan = idle_chats(self.model.frames(), |id| {
            self.session_key(id)
                .and_then(|key| store.as_ref()?.read(cx).get(key))
                .is_some_and(|s| s.active_within(superzed_sessions::store::RECENT_WINDOW, now, now_unix))
        });
        let titles: Vec<String> =
            plan.close.iter().filter_map(|id| self.model.frame(*id)).map(|f| self.node_title(f, cx)).collect();
        let message = idle_close_message(&project, &titles, plan.golden, plan.active);
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        if plan.close.is_empty() {
            workspace.update(cx, |ws, cx| {
                workspace::superzed_confirm::superzed_confirm_action(
                    ws,
                    format!("{message} Close the project?"),
                    "Close project",
                    true,
                    close_project,
                    window,
                    cx,
                )
            });
            return;
        }
        let ids = plan.close;
        let (clean, dirty) = commit_close::split_dirty(&ids, |id| self.node_uncommitted_files(id, cx).len());
        let landing = self.landing_after_close(&ids, window, cx);
        let canvas = cx.entity().downgrade();
        workspace.update(cx, |ws, cx| {
            workspace::superzed_confirm::superzed_confirm_with_secondary(
                ws,
                message,
                "Close chats",
                true,
                move |window, cx| {
                    canvas
                        .update(cx, |c, cx| {
                            if dirty.is_empty() {
                                c.remove_nodes(&ids, cx);
                                c.land_after_close(landing, window, cx);
                            } else {
                                // file non committati: il modale a tre bottoni di cmd-w
                                c.confirm_close_dirty(clean, dirty, window, cx);
                            }
                        })
                        .ok();
                },
                workspace::superzed_confirm::SecondaryAction {
                    label: "Close project".into(),
                    disabled_reason: None,
                    on_click: close_project,
                },
                window,
                cx,
            )
        });
    }

    /// Titolo nuovo del nodo; per una chat delle Automazioni e' anche il nome
    /// dell'automazione (fascia e titolo restano uguali).
    pub fn set_node_title(&mut self, id: NodeId, title: String, cx: &mut Context<Self>) {
        if let Some(runtime) = self.automations.get_mut(&id) {
            runtime.def.name = title.clone();
        }
        if let Some(f) = self.model.frame_mut(id) {
            f.title = title;
        }
        self.nodes_changed(cx);
    }

    pub fn rename_node(&mut self, id: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(frame) = self.model.frame(id) else {
            return;
        };
        // superzed: pre-riempie col titolo mostrato (`legacy_title`), non con quello
        // salvato in italiano prima dell'interfaccia in inglese
        let current = legacy_title(frame.kind, &frame.title).to_string();
        let canvas = cx.entity().downgrade();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |ws, cx| {
            crate::text_prompt::text_prompt(
                ws,
                "Rename node",
                &current,
                move |title, _, cx| {
                    if title.is_empty() {
                        return;
                    }
                    canvas.update(cx, |c, cx| c.set_node_title(id, title, cx)).ok();
                },
                window,
                cx,
            )
        });
    }

    /// Nessun colore → 0 → 1 … → NODE_COLORS-1 → nessun colore.
    pub fn cycle_color(&mut self, ids: &[NodeId], cx: &mut Context<Self>) {
        for id in ids {
            if let Some(f) = self.model.frame_mut(*id) {
                f.color = match f.color {
                    None => Some(0),
                    Some(c) if c + 1 < NODE_COLORS => Some(c + 1),
                    Some(_) => None,
                };
            }
        }
        self.nodes_changed(cx);
    }

    /// Golden ↔ normale per i nodi dati (cmd-g: quello con il focus, o i selezionati).
    pub fn toggle_golden(&mut self, ids: &[NodeId], cx: &mut Context<Self>) {
        for id in ids {
            if let Some(f) = self.model.frame_mut(*id) {
                f.golden = !f.golden;
            }
        }
        self.nodes_changed(cx);
    }

    fn render_node(
        &self,
        frame: &NodeFrame,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some(content) = self.contents.get(&frame.id) else {
            return self
                .starting_terminals
                .contains_key(&frame.id)
                .then(|| self.render_starting_node(frame, window, cx));
        };
        let zoom = self.viewport.zoom;
        let p = self.viewport.to_screen(frame.origin);
        let colors = cx.theme().colors();
        let id = frame.id.0;
        let docked = frame.kind == NodeKind::Claude && frame.in_kanban() && self.kanban_node().is_some();
        // superzed: stato della sessione claude (2a)
        let session = self
            .session_key(frame.id)
            .and_then(|key| superzed_sessions::SessionStore::global(cx).map(|s| (key, s)))
            .and_then(|(key, store)| store.read(cx).get(key).cloned());
        // claude aspetta l'utente: bordo e barra del titolo nel colore di avviso
        let needs_input =
            session.as_ref().is_some_and(|s| matches!(s.state, superzed_sessions::SessionState::NeedsInput(_)));
        // claude al lavoro: bordo ciano e barra/sfondo appena tinti (`working_color`)
        let working = session.as_ref().is_some_and(|s| s.state == superzed_sessions::SessionState::Working);
        let work = superzed_sessions::ui::working_color(cx);
        // spessore del bordo costante: la selezione e' un anello a parte (`selection_ring`)
        let golden = frame.golden;
        let border = if needs_input {
            cx.theme().status().warning
        } else if working {
            work.opacity(0.8)
        } else if golden {
            golden_color(cx)
        } else {
            node_color(frame.color, cx).unwrap_or(colors.border)
        };
        let head_bg = if needs_input {
            cx.theme().status().warning_background
        } else if working {
            colors.title_bar_background.blend(work.opacity(0.12))
        } else if golden {
            colors.title_bar_background.blend(golden_color(cx).opacity(0.18))
        } else {
            colors.title_bar_background
        };
        let dragging = matches!(self.drag, Some(DragState::Move { .. }));
        let status = session.as_ref().and_then(|s| {
            let icon = superzed_sessions::ui::status_icon(&s.state, cx)?;
            let tooltip = superzed_sessions::ui::status_tooltip(&s.state);
            Some(
                div()
                    .id(("superzed-node-status", id))
                    .debug_selector(move || format!("superzed-node-status-{id}"))
                    .flex_none()
                    .child(icon)
                    .when_some(tooltip, |d, text| d.tooltip(ui::Tooltip::text(text))),
            )
        });
        let overview = self.node_overview(frame.id, cx);
        let ctx_label = |s: &superzed_sessions::Session| {
            superzed_sessions::ui::context_label(s.context_left).map(|(text, color)| {
                div()
                    .id(("superzed-node-ctx", id))
                    .flex_none()
                    .debug_selector(move || format!("superzed-node-ctx-{id}"))
                    .tooltip(ui::Tooltip::text("Context left"))
                    .child(Label::new(text).size(LabelSize::Default).color(color))
            })
        };
        let head = if overview {
            // vista d'insieme: il testo del terminale non si disegna; al suo posto e al
            // posto della barra, titolo, stato e fase/ctx in un rem adattato al nodo sullo
            // schermo (`overview_rem`), cosi' il contenuto previsto ci sta sempre. Fa da
            // barra del titolo: trascinamento, doppio click e menu contestuale come prima.
            let screen = size(px(frame.size.width * zoom), px(frame.size.height * zoom));
            let fit = terminal_node::overview_rem(screen, window.rem_size());
            let body = match fit {
                Some(rem) => {
                    let phase = session.as_ref().filter(|_| docked).map(|s| {
                        Label::new(s.phase.label()).size(LabelSize::Default).color(Color::Muted)
                    });
                    let ctx = session.as_ref().and_then(ctx_label);
                    let has_meta = phase.is_some() || ctx.is_some();
                    let now = chrono::Local::now();
                    // nata (+) a sinistra, ultima risposta (freccia) a destra, ai bordi:
                    // si distinguono dalla posizione e dall'icona, senza parole. Ogni
                    // lato puo' restringersi (`min_w_0` + puntini) cosi' le due non si
                    // sovrappongono mai in un nodo stretto.
                    let dates = session.as_ref().map(|s| s.times).filter(|t| t.opened_at.is_some() || t.replied_at.is_some()).map(|t| {
                        let side = |icon: IconName, at: Option<i64>, which: &'static str| {
                            at.map(|at| {
                                h_flex()
                                    .debug_selector(move || format!("superzed-node-overview-{which}-{id}"))
                                    .min_w_0()
                                    .gap(rems(0.25))
                                    .child(Icon::new(icon).size(IconSize::XSmall).color(Color::Muted))
                                    .child(
                                        Label::new(crate::age::age_label(at, now))
                                            .size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                            })
                        };
                        h_flex()
                            .debug_selector(move || format!("superzed-node-overview-dates-{id}"))
                            .flex_none()
                            .w_full()
                            .justify_between()
                            .when(t.opened_at.is_none(), |d| d.justify_end())
                            .gap(rems(0.5))
                            .children(side(IconName::Plus, t.opened_at, "opened"))
                            .children(side(IconName::ReplyArrowRight, t.replied_at, "replied"))
                    });
                    let fade = session.as_ref().and_then(|s| overview_freshness(s, now)).map(crate::age::Freshness::opacity);
                    // in alto, non centrato: se qualcosa non ci sta si taglia solo in basso;
                    // titolo su 2 righe al massimo, poi icona e meta (misure in
                    // `OVERVIEW_REF_H_REMS`)
                    WithRemSize::new(rem)
                        .size_full()
                        .child(
                            v_flex()
                                .debug_selector(move || format!("superzed-node-overview-{id}"))
                                .size_full()
                                .items_center()
                                .justify_start()
                                .gap(rems(0.5))
                                .p(rems(0.75))
                                .when(golden, |d| d.child(golden_star(id, cx)))
                                .child(
                                    div()
                                        .debug_selector(move || format!("superzed-node-overview-title-{id}"))
                                        .flex_none()
                                        .max_w_full()
                                        .text_center()
                                        .child(
                                            Label::new(self.node_title(frame, cx))
                                                .size(LabelSize::Large)
                                                .weight(FontWeight::BOLD)
                                                .line_clamp(2),
                                        ),
                                )
                                // al lavoro: la pillola «Working» al posto dello spinner
                                .map(|d| {
                                    if working {
                                        d.child(
                                            div()
                                                .debug_selector(move || format!("superzed-node-status-{id}"))
                                                .flex_none()
                                                .child(superzed_sessions::ui::working_pill(cx)),
                                        )
                                    } else {
                                        d.children(status)
                                    }
                                })
                                .when(has_meta, |d| {
                                    d.child(
                                        h_flex()
                                            .debug_selector(move || format!("superzed-node-overview-meta-{id}"))
                                            .flex_none()
                                            .max_w_full()
                                            .overflow_hidden()
                                            .gap(rems(0.5))
                                            .children(phase)
                                            .children(ctx),
                                    )
                                })
                                .children(dates)
                                // chat vecchia: il contenuto sbiadisce (`age::Freshness`)
                                .when_some(fade, |d, opacity| d.opacity(opacity)),
                        )
                        .into_any_element()
                }
                // testo illeggibile: solo sfondo, bordo e l'icona di stato centrata se ci sta
                None => {
                    let icon_fits = screen.width.min(screen.height) >= terminal_node::OVERVIEW_MIN_REM * 2.;
                    WithRemSize::new(terminal_node::OVERVIEW_MIN_REM)
                        .size_full()
                        .child(
                            div()
                                .debug_selector(move || format!("superzed-node-overview-{id}"))
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(icon_fits, |d| d.children(status)),
                        )
                        .into_any_element()
                }
            };
            div()
                .debug_selector(move || format!("superzed-node-title-{id}"))
                .w(screen.width)
                .h(screen.height)
                .overflow_hidden()
                .bg(if needs_input {
                    cx.theme().status().warning_background
                } else if working {
                    colors.editor_background.blend(work.opacity(0.06))
                } else {
                    colors.editor_background
                })
                .map(|d| if dragging { d.cursor_grabbing() } else { d.cursor_grab() })
                .block_mouse_except_scroll()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.on_title_mouse_down(NodeId(id), e, window, cx);
                    }),
                )
                .child(body)
                .into_any_element()
        } else {
            // l'etichetta dell'account solo se c'e' da scegliere (piu' di un account)
            let many_accounts = superzed_accounts::AccountStore::global(cx)
                .is_some_and(|store| store.read(cx).accounts().len() > 1);
            let meta = session.as_ref().map(|s| {
                h_flex()
                    .min_w_0()
                    .flex_shrink(SHRINK_BEFORE_TITLE)
                    .gap(rems(0.5))
                    .children(ctx_label(s))
                    .when(many_accounts, |d| {
                        d.child(
                            div()
                                .min_w_0()
                                .debug_selector(move || format!("superzed-node-account-{id}"))
                                .child(
                                    Label::new(s.account.clone())
                                        .size(LabelSize::Default)
                                        .color(Color::Muted)
                                        .truncate(),
                                ),
                        )
                    })
            });
            let title_text = self.node_title(frame, cx);
            // la barra del titolo e' in rem, dentro un rem scalato: testo e altezza
            // seguono lo zoom (nello spike il testo veniva tagliato). `WithRemSize` non
            // implementa `InteractiveElement` (solo `Styled`/`ParentElement`): debug_selector,
            // cursor e on_mouse_down vanno sul div interno, non su di lei (deviazione dal
            // brief, che li mette direttamente su `WithRemSize`).
            WithRemSize::new(window.rem_size() * zoom)
                .child(
                    div()
                        .debug_selector(move || format!("superzed-node-title-{id}"))
                        .flex_none()
                        .h(rems(TITLE_BAR_REMS))
                        .px(rems(0.5))
                        .gap(rems(0.4))
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .bg(head_bg)
                        .map(|d| if dragging { d.cursor_grabbing() } else { d.cursor_grab() })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.on_title_mouse_down(NodeId(id), e, window, cx);
                            }),
                        )
                        .when(golden, |d| d.child(golden_star(id, cx)))
                        .children(status)
                        // il titolo e' l'ultimo a restringersi: stato Done e meta cedono prima
                        .child(
                            div()
                                .id(("superzed-node-title-text", id))
                                .flex_grow(1.)
                                .min_w_0()
                                .tooltip(ui::Tooltip::text(title_text.clone()))
                                .child(
                                    Label::new(title_text)
                                        .size(LabelSize::Large)
                                        .weight(FontWeight::BOLD)
                                        .truncate(),
                                ),
                        )
                        .children(self.done_status(NodeId(id)).map(|status| {
                            let (text, level, detail) = done::done_status_view(status);
                            let color = match level {
                                done::DoneLevel::Error => Color::Error,
                                done::DoneLevel::InProgress => Color::Muted,
                                done::DoneLevel::Ready => Color::Accent,
                                done::DoneLevel::Written => Color::Success,
                            };
                            let spinner = (level == done::DoneLevel::InProgress).then(|| {
                                ui::Icon::new(ui::IconName::LoadCircle)
                                    .size(ui::IconSize::Small)
                                    .color(Color::Muted)
                            });
                            div()
                                .id(("superzed-done-status", id))
                                .debug_selector(move || format!("superzed-done-status-{id}"))
                                .min_w_0()
                                .max_w(rems(12.))
                                .flex_shrink(SHRINK_BEFORE_TITLE)
                                .flex()
                                .items_center()
                                .gap(rems(0.25))
                                .tooltip(ui::Tooltip::text(detail))
                                .children(spinner)
                                .child(
                                    div().min_w_0().child(
                                        Label::new(text).size(LabelSize::Small).color(color).truncate(),
                                    ),
                                )
                                // report pronto: apre la revisione, come «GitHub…» nel menu
                                .when(level == done::DoneLevel::Ready, |d| {
                                    d.cursor_pointer().on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                            cx.stop_propagation();
                                            this.github_for_node(NodeId(id), window, cx);
                                        }),
                                    )
                                })
                        }))
                        .children(meta),
                )
                .into_any_element()
        };
        let kanban_open_top = frame.kind == NodeKind::Kanban
            && crate::kanban_node::header_overhang(
                self.kanban_metrics(cx).header,
                f32::from(window.rem_size()),
                zoom,
            ) > 0.;
        let canvas = cx.entity().downgrade();
        let extra = content.context_menu_entries();
        // superzed: chat del kanban in Fatto → anteprima delle modifiche alla board (sottoprogetto 4)
        let github = self.is_done_chat(NodeId(id), cx);
        let menu_keys = self.menu_keys.clone();
        let golden = frame.golden;
        let closable = frame.kind != NodeKind::Mail;
        let title = ui::right_click_menu(("superzed-node-menu", id))
            .trigger(move |_, _, _| head)
            .menu(move |window, cx| {
                let canvas = canvas.clone();
                let menu_keys = menu_keys.clone();
                let extra = extra.iter().map(|(l, a)| (l.clone(), a.boxed_clone())).collect::<Vec<_>>();
                ui::ContextMenu::build(window, cx, move |mut menu, _, _| {
                    // le scorciatoie si cercano nel contesto `CanvasFocused` (vedi `menu_keys`)
                    menu = menu.context(menu_keys);
                    for (label, action) in extra {
                        // come `ContextMenu::action` senza contesto: l'azione parte dal
                        // focus corrente, non da `menu_keys`
                        let dispatch = action.boxed_clone();
                        menu = menu.entry(label, Some(action), move |window, cx| {
                            window.dispatch_action(dispatch.boxed_clone(), cx)
                        });
                    }
                    if github {
                        let c0 = canvas.clone();
                        menu = menu.entry("GitHub…", None, move |window, cx| {
                            c0.update(cx, |c, cx| c.github_for_node(NodeId(id), window, cx)).ok();
                        });
                    }
                    let (c1, c2, c3, c4) = (canvas.clone(), canvas.clone(), canvas.clone(), canvas.clone());
                    let menu = menu.item(
                        ui::ContextMenuEntry::new("Rename")
                            .icon(ui::IconName::Pencil)
                            .action(Box::new(superzed_actions::RenameNode))
                            .handler(move |window, cx| {
                                c1.update(cx, |c, cx| c.rename_node(NodeId(id), window, cx)).ok();
                            }),
                    )
                    .item(
                        ui::ContextMenuEntry::new("Next color")
                            .icon(ui::IconName::Circle)
                            .action(Box::new(superzed_actions::CycleNodeColor))
                            .handler(move |_, cx| {
                                c2.update(cx, |c, cx| c.cycle_color(&[NodeId(id)], cx)).ok();
                            }),
                    )
                    .item(
                        ui::ContextMenuEntry::new(if golden { "Not golden" } else { "Golden" })
                            .icon(if golden { ui::IconName::StarFilled } else { ui::IconName::Star })
                            .action(Box::new(superzed_actions::ToggleGolden))
                            .handler(move |_, cx| {
                                c4.update(cx, |c, cx| c.toggle_golden(&[NodeId(id)], cx)).ok();
                            }),
                    );
                    if !closable {
                        return menu;
                    }
                    menu.separator().item(
                        ui::ContextMenuEntry::new("Close")
                            .icon(ui::IconName::Close)
                            .action(Box::new(superzed_actions::CloseNode))
                            .handler(move |window, cx| {
                                c3.update(cx, |c, cx| c.close_nodes_with_confirm(vec![NodeId(id)], window, cx))
                                    .ok();
                            }),
                    )
                })
            });
        Some(
            div()
                .id(("superzed-node", id))
                .absolute()
                .left(px(p.x))
                .top(px(p.y))
                .w(px(frame.size.width * zoom))
                .h(px(frame.size.height * zoom))
                .flex()
                .flex_col()
                .bg(colors.panel_background)
                .border_1()
                .border_color(border)
                .rounded_md()
                // kanban a zoom molto basso: le intestazioni salgono oltre il bordo superiore,
                // che si dipinge sopra i figli e ne taglierebbe il testo; lo disegnano loro
                .when(kanban_open_top, |d| {
                    d.border_t_0().debug_selector(|| "superzed-kanban-open-top".into())
                })
                // il kanban no: le intestazioni crescono sopra il suo riquadro a zoom basso
                .when(frame.kind != NodeKind::Kanban, |d| d.overflow_hidden())
                .capture_any_mouse_down(cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.on_body_mouse_down(NodeId(id), e, cx);
                }))
                // superzed: Esc/ctrl-c nel nodo claude interrompe senza hook di claude.
                // Nel terminale sono legati a `terminal::SendKeystroke` (un tasto con un
                // binding non arriva ai listener key_down): si ascolta l'azione in capture,
                // che non ferma la propagazione; key_down resta per i tasti senza binding.
                .capture_action(cx.listener(move |this, a: &terminal_view::SendKeystroke, _, cx| {
                    if matches!(a.keystroke(), "escape" | "ctrl-c") {
                        this.interrupt_session(NodeId(id), cx);
                    }
                }))
                .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                    if superzed_sessions::store::is_interrupt_key(&e.keystroke) {
                        this.interrupt_session(NodeId(id), cx);
                    }
                }))
                // oltre la soglia di lettura il terminale resta montato (focus, tasti) ma
                // non disegna il testo (`TerminalView::superzed_text_visible`): sotto la
                // vista d'insieme, che si disegna dopo e lo copre
                .map(|d| {
                    let body = div()
                        .debug_selector(move || format!("superzed-node-body-{id}"))
                        .min_h_0()
                        .child(content.view());
                    if overview {
                        d.child(body.absolute().inset_0().overflow_hidden()).child(title)
                    } else {
                        let body = body.flex_1();
                        let body = if frame.kind == NodeKind::Kanban { body } else { body.overflow_hidden() };
                        d.child(title).child(body)
                    }
                })
                // il kanban non ha contenuto scorrevole e copre la viewport: il suo
                // scroll arriva al pan del canvas. Gli altri nodi tengono lo scroll che
                // `scroll_route` lascia loro (nodo con il focus, fuori da un pan agganciato)
                .when(!matches!(frame.kind, NodeKind::Kanban | NodeKind::Automations), |d| {
                    d.on_scroll_wheel(|e, _, cx| node_scroll_guard(e, cx))
                })
                // chat agganciata: cmd-shift-destra/sinistra la spostano di colonna anche
                // con il focus nel suo terminale
                .when(docked, |d| {
                    d.key_context("KanbanChat")
                        .on_action(cx.listener(move |this, _: &superzed_actions::MoveCardNext, _, cx| {
                            this.move_docked(NodeId(id), true, cx)
                        }))
                        .on_action(cx.listener(move |this, _: &superzed_actions::MoveCardPrev, _, cx| {
                            this.move_docked(NodeId(id), false, cx)
                        }))
                })
                // kanban con il focus: cmd-shift-destra/sinistra spostano le chat
                // agganciate selezionate (binding nel contesto `Kanban`, dentro il nodo)
                .when(frame.kind == NodeKind::Kanban, |d| {
                    d.on_action(cx.listener(|this, _: &superzed_actions::MoveCardNext, _, cx| {
                        this.move_selected_docked(true, cx)
                    }))
                    .on_action(cx.listener(|this, _: &superzed_actions::MoveCardPrev, _, cx| {
                        this.move_selected_docked(false, cx)
                    }))
                })
                // click e selezione solo al nodo in primo piano; lo scroll passa
                // (cmd+scroll deve arrivare allo zoom del canvas)
                .block_mouse_except_scroll()
                .into_any_element(),
        )
    }

    /// Nodo terminale il cui PTY sta partendo: cornice, titolo trascinabile e un loader
    /// al posto del terminale.
    fn render_starting_node(&self, frame: &NodeFrame, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let zoom = self.viewport.zoom;
        let p = self.viewport.to_screen(frame.origin);
        let colors = cx.theme().colors();
        let id = frame.id.0;
        let dragging = matches!(self.drag, Some(DragState::Move { .. }));
        let spinner = || ui::Icon::new(ui::IconName::LoadCircle).color(Color::Muted);
        let starting_label = match frame.kind {
            NodeKind::Claude => "Starting Claude…",
            _ => "Starting terminal…",
        };
        let title = self.node_title(frame, cx);
        let spinner_head = spinner();
        let spinner_body = spinner();
        div()
            .id(("superzed-node", id))
            .debug_selector(move || format!("superzed-node-starting-{id}"))
            .absolute()
            .left(px(p.x))
            .top(px(p.y))
            .w(px(frame.size.width * zoom))
            .h(px(frame.size.height * zoom))
            .flex()
            .flex_col()
            .bg(colors.panel_background)
            .border_1()
            .border_color(node_color(frame.color, cx).unwrap_or(colors.border))
            .rounded_md()
            .overflow_hidden()
            .capture_any_mouse_down(cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                this.on_body_mouse_down(NodeId(id), e, cx);
            }))
            .on_scroll_wheel(|e, _, cx| node_scroll_guard(e, cx))
            .child(
                WithRemSize::new(window.rem_size() * zoom).child(
                    div()
                        .flex_none()
                        .h(rems(TITLE_BAR_REMS))
                        .px(rems(0.5))
                        .gap(rems(0.4))
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .bg(colors.title_bar_background)
                        .map(|d| if dragging { d.cursor_grabbing() } else { d.cursor_grab() })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.on_title_mouse_down(NodeId(id), e, window, cx);
                            }),
                        )
                        .when(frame.golden, |d| d.child(golden_star(id, cx)))
                        .child(div().flex_none().child(spinner_head))
                        .child(
                            div().flex_grow(1.).min_w_0().child(
                                Label::new(title).size(LabelSize::Large).weight(FontWeight::BOLD).truncate(),
                            ),
                        ),
                ),
            )
            .child(
                WithRemSize::new(window.rem_size() * zoom).flex_1().child(
                    v_flex()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .gap(rems(0.5))
                        .child(spinner_body)
                        .child(Label::new(starting_label).color(Color::Muted)),
                ),
            )
            .block_mouse_except_scroll()
            .into_any_element()
    }

    /// Anello della selezione: sopra il nodo e appena fuori dal suo bordo, cosi' si vede
    /// anche sui nodi colorati e il contenuto non si sposta (il bordo resta di 1 px).
    fn selection_ring(&self, frame: &NodeFrame, cx: &App) -> Option<AnyElement> {
        if !self.model.is_selected(frame.id)
            || !(self.contents.contains_key(&frame.id) || self.starting_terminals.contains_key(&frame.id))
        {
            return None;
        }
        const RING: f32 = 2.;
        let zoom = self.viewport.zoom;
        let p = self.viewport.to_screen(frame.origin);
        let id = frame.id.0;
        // superzed: kanban a zoom molto basso, l'intestazione sale sopra il bordo del nodo
        // (`header_overhang`); l'anello deve salire con lei, non attraversarne il testo
        let overhang = if frame.kind == NodeKind::Kanban {
            crate::kanban_node::header_overhang(self.kanban_metrics(cx).header, self.ui_rem(cx), zoom)
        } else {
            0.
        };
        Some(
            div()
                .debug_selector(move || format!("superzed-node-ring-{id}"))
                .absolute()
                .left(px(p.x - RING))
                .top(px(p.y - RING - overhang))
                .w(px(frame.size.width * zoom + 2. * RING))
                .h(px(frame.size.height * zoom + 2. * RING + overhang))
                .border_2()
                .border_color(cx.theme().colors().border_focused)
                .rounded_md()
                .into_any_element(),
        )
    }

    /// Maniglia di resize di un contenitore: a cavallo del suo angolo in basso a destra e
    /// disegnata dopo tutti i nodi, cosi' resta raggiungibile a ogni zoom anche quando le
    /// chat agganciate coprono il margine del contenitore; con un segno d'angolo visibile.
    fn container_grip(&self, kind: DockKind, cx: &Context<Self>) -> Option<AnyElement> {
        let node = self.container_node(kind)?;
        let frame = self.model.frame(node)?;
        self.contents.get(&node)?;
        Some(self.resize_grip(frame, cx))
    }

    /// Maniglia di resize di un nodo libero: disegnata subito dopo il nodo, cosi' un nodo
    /// sopra di esso la copre.
    fn free_node_grip(&self, frame: &NodeFrame, cx: &Context<Self>) -> Option<AnyElement> {
        (free_resizable(frame) && self.contents.contains_key(&frame.id)).then(|| self.resize_grip(frame, cx))
    }

    fn resize_grip(&self, frame: &NodeFrame, cx: &Context<Self>) -> AnyElement {
        let zoom = self.viewport.zoom;
        let p = self.viewport.to_screen(frame.origin);
        let corner = point(p.x + frame.size.width * zoom, p.y + frame.size.height * zoom);
        let id = frame.id.0;
        let mark = cx.theme().colors().text_muted;
        div()
            .id(("superzed-container-grip", id))
            .debug_selector(move || format!("superzed-node-resize-{id}"))
            .absolute()
            .left(px(corner.x - KANBAN_GRIP + KANBAN_GRIP_OUTSIDE))
            .top(px(corner.y - KANBAN_GRIP + KANBAN_GRIP_OUTSIDE))
            .size(px(KANBAN_GRIP))
            .flex()
            .items_end()
            .justify_end()
            .cursor(gpui::CursorStyle::ResizeUpLeftDownRight)
            .child(
                div()
                    .mr(px(KANBAN_GRIP_OUTSIDE))
                    .mb(px(KANBAN_GRIP_OUTSIDE))
                    .size(px(KANBAN_GRIP / 2.))
                    .border_r_2()
                    .border_b_2()
                    .border_color(mark),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.on_resize_mouse_down(NodeId(id), e, cx);
                }),
            )
            .block_mouse_except_scroll()
            .into_any_element()
    }

    // superzed: font dell'editor per progetto (Task 17).

    fn multi_workspace(&self, cx: &App) -> Option<Entity<workspace::MultiWorkspace>> {
        self.workspace.upgrade()?.read(cx).multi_workspace()?.upgrade()
    }

    pub fn is_active_workspace(&self, cx: &App) -> bool {
        match (self.multi_workspace(cx), self.workspace.upgrade()) {
            (Some(mw), Some(ws)) => mw.read(cx).workspace() == &ws,
            _ => false,
        }
    }

    /// Il workspace riceve il suo MultiWorkspace dopo la creazione: ci si aggancia
    /// appena c'e' (da `new`, a fine caricamento e al primo render).
    fn ensure_multi_workspace_subscription(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mw_subscription.is_some() {
            return;
        }
        let Some(mw) = self.multi_workspace(cx) else {
            return;
        };
        self.mw_subscription = Some(cx.subscribe_in(
            &mw,
            window,
            |this, _, event: &workspace::MultiWorkspaceEvent, window, cx| {
                if matches!(event, workspace::MultiWorkspaceEvent::ActiveWorkspaceChanged { .. })
                    && this.is_active_workspace(cx)
                {
                    this.apply_editor_font(cx);
                    // `activate` da' il focus al workspace dopo aver emesso l'evento:
                    // si decide dopo, quando il suo focus e' gia' stato applicato
                    cx.defer_in(window, |this, window, cx| {
                        this.focus_canvas_if_editor_empty(window, cx)
                    });
                }
            },
        ));
    }

    /// Progetto senza file aperti: il pane attivo non e' disegnato, quindi il focus
    /// che `MultiWorkspace::activate` gli da' (fallback_focus_handle) finisce in un
    /// elemento invisibile e i tasti del canvas non arrivano. Lo si sposta sul canvas.
    fn focus_canvas_if_editor_empty(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_active_workspace(cx) {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |ws, cx| {
            if ws.superzed_has_center_items(cx) {
                return;
            }
            let pane_focus = ws.active_pane().focus_handle(cx);
            if window.focused(cx).is_none() || pane_focus.contains_focused(window, cx) {
                ws.superzed_focus_canvas(window, cx);
            }
        });
    }

    fn record_editor_font(&mut self, cx: &mut Context<Self>) {
        if !self.is_active_workspace(cx) {
            return;
        }
        let settings = theme_settings::ThemeSettings::get_global(cx);
        let current = settings.buffer_font_size(cx);
        let configured = settings.buffer_font_size_settings();
        let value = (current != configured).then_some(current);
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |ws, cx| ws.update_superzed_state(|s| s.editor_font_size = value, cx));
        }
    }

    pub fn apply_editor_font(&self, cx: &mut App) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        match workspace.read(cx).superzed_state().editor_font_size {
            Some(size) => theme_settings::adjust_buffer_font_size(cx, |_| size),
            None => theme_settings::reset_buffer_font_size(cx),
        }
    }

    fn project_root(&self, cx: &App) -> Option<(Arc<dyn Fs>, PathBuf)> {
        let workspace = self.workspace.upgrade()?;
        let project = workspace.read(cx).project().read(cx);
        let root = project
            .visible_worktrees(cx)
            .next()?
            .read(cx)
            .abs_path()
            .to_path_buf();
        Some((project.fs().clone(), root))
    }

    fn start_load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.persist.is_some() {
            return;
        }
        let Some((fs, root)) = self.project_root(cx) else {
            return;
        };
        self.persist = Some(Persist {
            fs: fs.clone(),
            root: root.clone(),
            loaded: false,
            save_blocked: false,
            pending_save: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = persistence::load_project_file(fs.as_ref(), &root).await;
            this.update_in(cx, |this, window, cx| this.finish_load(outcome, window, cx))
                .log_err();
        })
        .detach();
    }

    fn finish_load(&mut self, outcome: LoadOutcome, window: &mut Window, cx: &mut Context<Self>) {
        let had_changes = !self.model.frames().is_empty();
        match outcome {
            LoadOutcome::Missing => {}
            LoadOutcome::Loaded(file) => self.apply_file(file, window, cx),
            LoadOutcome::Recovered { backup, reason } => {
                let root = self.persist.as_ref().map(|p| p.root.clone()).unwrap_or_default();
                let blocked = backup == persistence::project_file_path(&root);
                if let Some(persist) = self.persist.as_mut() {
                    persist.save_blocked = blocked;
                }
                let message = if blocked {
                    format!("project.json is not readable ({reason}); the canvas will not be saved")
                } else {
                    format!("project.json is not readable ({reason}); moved to {}", backup.display())
                };
                self.warn(message, cx);
            }
        }
        // senza il suo contenitore nessuna chat e' agganciata: un `dock` rimasto nel file
        // verrebbe risalvato e un contenitore aperto dopo la aggancerebbe
        let stale: Vec<NodeId> = self
            .model
            .frames()
            .iter()
            .filter(|f| f.dock.is_some_and(|k| self.container_node(k).is_none()))
            .map(|f| f.id)
            .collect();
        for id in stale {
            if let Some(f) = self.model.frame_mut(id) {
                f.dock = None;
            }
            if self.automations.remove(&id).is_some() {
                self.set_automation_background(id, false, cx);
            }
        }
        // i contenitori di default (anche quelli chiusi tornano), prima di `loaded`: da
        // soli non scrivono project.json
        if cx.try_global::<DefaultContainers>().map_or(true, |d| d.0) {
            self.ensure_container(DockKind::Kanban, cx);
            self.ensure_container(DockKind::Automations, cx);
            self.ensure_mail_node(window, cx);
        }
        for (id, used) in std::mem::take(&mut self.pending_source_changes) {
            toggle_source(&mut self.project_sources, &id, used);
        }
        if let Some(persist) = self.persist.as_mut() {
            persist.loaded = true;
        }
        // superzed: il MultiWorkspace puo' essere arrivato nel frattempo; e se questo
        // progetto e' quello attivo, il font caricato ora va applicato subito.
        self.ensure_multi_workspace_subscription(window, cx);
        if self.is_active_workspace(cx) {
            self.apply_editor_font(cx);
            // superzed: all'avvio il focus va al pane attivo, invisibile se non ci sono file
            cx.defer_in(window, |this, window, cx| this.focus_canvas_if_editor_empty(window, cx));
        }
        // nodi creati mentre si caricava: ora si possono salvare insieme a quelli letti
        if had_changes || std::mem::take(&mut self.save_after_load) {
            self.schedule_save(cx);
        }
        self.relayout_containers(cx);
        cx.notify();
    }

    fn apply_file(&mut self, file: ProjectFile, window: &mut Window, cx: &mut Context<Self>) {
        self.viewport.offset = point(file.viewport.offset[0], file.viewport.offset[1]);
        self.viewport.zoom = file
            .viewport
            .zoom
            .clamp(crate::viewport::MIN_ZOOM, crate::viewport::MAX_ZOOM);
        // nodi creati mentre si caricava (es. una nota con id 1) non devono essere
        // sostituiti da nodi del file con lo stesso id: si rinumerano sopra
        // anche gli id dei nodi di tipo sconosciuto restano occupati
        let foreign_ids = file.foreign_nodes.iter().filter_map(|v| v.get("id")?.as_u64()).map(NodeId);
        let file_ids: Vec<NodeId> = file.nodes.iter().map(|r| NodeId(r.id)).chain(foreign_ids).collect();
        let colliding: Vec<NodeId> = self
            .model
            .frames()
            .iter()
            .map(|f| f.id)
            .filter(|id| file_ids.contains(id))
            .collect();
        let mut next = file_ids.iter().map(|id| id.0).max().unwrap_or(0).max(self.model.max_id());
        for old in colliding {
            next += 1;
            self.renumber_node(old, NodeId(next), cx);
        }
        self.model.reserve_id(next);
        for record in &file.nodes {
            self.restore_node(record.to_frame(), &record.data, window, cx);
        }
        self.project_sources = file.sources.clone();
        self.foreign_nodes = file.foreign_nodes.clone();
        if !self.foreign_nodes.is_empty() {
            self.warn(
                format!("{} nodes of an unknown kind in project.json: not shown, but kept", self.foreign_nodes.len()),
                cx,
            );
        }
        let state = SuperzedProjectState {
            editor_width: file
                .editor_width
                .map(px)
                .unwrap_or(workspace::superzed_layout::DEFAULT_EDITOR_WIDTH),
            tab_name: file.tab.name.map(SharedString::from),
            tab_color: file.tab.color,
            editor_font_size: file.editor_font_size.map(px),
        };
        self.last_state = Some(state.clone());
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |ws, cx| ws.update_superzed_state(|s| *s = state, cx));
        }
        self.apply_font_zoom_all(cx);
    }

    /// Cambia l'id di un nodo gia' presente in tutte le mappe che lo usano.
    fn renumber_node(&mut self, old: NodeId, new: NodeId, cx: &mut Context<Self>) {
        self.model.renumber(old, new);
        if let Some(content) = self.contents.remove(&old) {
            self.contents.insert(new, content);
        }
        if let Some(session) = self.sessions.remove(&old) {
            self.sessions.insert(new, session);
        }
        if let Some(key) = self.pending_sessions.remove(&old) {
            self.pending_sessions.insert(new, key);
        }
        if let Some(ticket) = self.starting_terminals.remove(&old) {
            self.starting_terminals.insert(new, ticket);
        }
        if let Some(runtime) = self.automations.remove(&old) {
            self.automations.insert(new, runtime);
        }
        if let Some(view) = self.mail_views.remove(&old) {
            self.mail_views.insert(new, view);
        }
        let subscription = self.node_subscriptions.remove(&old);
        match self.terminals.remove(&old) {
            Some((view, terminal)) => {
                // la sottoscrizione di un terminale cattura l'id: va rifatta
                drop(subscription);
                self.subscribe_terminal_exit(new, &terminal, cx);
                self.terminals.insert(new, (view, terminal));
            }
            None => {
                if let Some(subscription) = subscription {
                    self.node_subscriptions.insert(new, subscription);
                }
            }
        }
    }

    /// Processo terminato (es. `exit`): il nodo si chiude senza segnali, il gruppo
    /// non esiste piu' e il suo pgid potrebbe essere riusato.
    fn subscribe_terminal_exit(&mut self, id: NodeId, terminal: &Entity<Terminal>, cx: &mut Context<Self>) {
        let sub = cx.subscribe(terminal, move |canvas, _, event: &terminal::Event, cx| {
            match event {
                terminal::Event::CloseTerminal => canvas.remove_nodes_after_exit(&[id], cx),
                // il titolo OSC (claude) cambia il titolo mostrato
                terminal::Event::BreadcrumbsChanged => cx.notify(),
                _ => {}
            }
        });
        self.node_subscriptions.insert(id, sub);
    }

    /// Ricrea il contenuto di un nodo salvato.
    pub fn restore_node(
        &mut self,
        mut frame: NodeFrame,
        data: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // solo le chat claude stanno in un contenitore
        if frame.kind != NodeKind::Claude {
            frame.dock = None;
        }
        match frame.kind {
            NodeKind::Note => {
                let note = NoteNode::from_data(data, window, cx);
                let editor = note.editor().clone();
                self.watch_note(frame.id, &editor, cx);
                self.insert_restored_node(frame, Box::new(note), cx);
            }
            NodeKind::Shell | NodeKind::Claude => {
                // una chat delle Automazioni senza una definizione leggibile torna libera;
                // fuori dalle Automazioni una chiave `automation` si ignora (e non si risalva)
                if frame.dock == Some(DockKind::Automations) {
                    match crate::automation::automation_from_data(data) {
                        Some(def) => {
                            let now = self.now();
                            self.automations.insert(frame.id, AutomationRuntime::new(def, now));
                        }
                        None => frame.dock = None,
                    }
                }
                let kind = frame.kind;
                self.open_terminal(kind, Some(frame), data, window, cx);
            }
            NodeKind::Source => {
                let (connection, filter) = crate::source_node::panel_data(data);
                let root = self.project_root_path(cx);
                let view = cx.new(|cx| SourcePanelView::new(connection, filter, root, window, cx));
                self.watch_source(frame.id, &view, window, cx);
                self.insert_restored_node(frame, Box::new(SourceNode::new(view)), cx);
            }
            NodeKind::Mail => {
                let view = self.new_mail_view(data, window, cx);
                self.watch_mail(frame.id, &view, window, cx);
                self.insert_restored_node(frame, Box::new(crate::mail_node::MailNode::new(view)), cx);
            }
            NodeKind::Kanban | NodeKind::Automations => {
                let kind = if frame.kind == NodeKind::Kanban { DockKind::Kanban } else { DockKind::Automations };
                let state = ContainerState { chat_size: kanban_layout::chat_size_from_data(data), data: data.clone() };
                self.containers.insert(kind, state);
                let canvas = cx.entity().downgrade();
                let node: Box<dyn NodeContent> = match kind {
                    DockKind::Kanban => Box::new(crate::kanban_node::KanbanNode::new(canvas, cx)),
                    DockKind::Automations => Box::new(crate::automations_node::AutomationsNode::new(canvas, cx)),
                };
                self.insert_restored_node(frame, node, cx);
            }
        }
    }

    /// Programma il salvataggio a ogni modifica del testo di una nota.
    fn watch_note(&mut self, id: NodeId, editor: &Entity<Editor>, cx: &mut Context<Self>) {
        let sub = cx.subscribe(editor, |this, _, event: &editor::EditorEvent, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) {
                this.schedule_save(cx);
            }
        });
        self.node_subscriptions.insert(id, sub);
    }

    fn warn(&mut self, message: String, cx: &mut Context<Self>) {
        log::warn!("superzed: {message}");
        self.last_warning = Some(message.clone());
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |ws, cx| {
                ws.show_toast(Toast::new(NotificationId::unique::<CanvasView>(), message), cx)
            });
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.persist.as_ref().is_some_and(|p| p.loaded)
    }

    pub fn has_pending_save(&self) -> bool {
        self.persist.as_ref().is_some_and(|p| p.pending_save.is_some())
    }

    pub fn last_warning(&self) -> Option<&str> {
        self.last_warning.as_deref()
    }

    pub fn snapshot(&self, cx: &App) -> ProjectFile {
        // a tab chiusa il workspace puo' essere gia' stato rilasciato: si usa l'ultimo
        // stato visto, altrimenti nome, colore e larghezza della tab andrebbero persi
        let state = match self.workspace.upgrade() {
            Some(ws) => ws.read(cx).superzed_state().clone(),
            None => self.last_state.clone().unwrap_or_default(),
        };
        ProjectFile {
            version: persistence::PROJECT_FILE_VERSION,
            viewport: ViewportRecord {
                offset: [self.viewport.offset.x, self.viewport.offset.y],
                zoom: self.viewport.zoom,
            },
            editor_width: Some(f32::from(state.editor_width)),
            editor_font_size: state.editor_font_size.map(f32::from),
            tab: TabRecord {
                name: state.tab_name.map(|n| n.to_string()),
                color: state.tab_color,
            },
            nodes: self
                .model
                .frames()
                .iter()
                .map(|f| NodeRecord::from_frame(f, self.node_save_data(f.id, cx)))
                .collect(),
            sources: self.project_sources.clone(),
            foreign_nodes: self.foreign_nodes.clone(),
        }
    }

    /// Il canvas sta per essere distrutto (tab chiusa). Il `Drop` di `Terminal` segnala
    /// solo il gruppo della shell: i job in primo piano (es. `npm run dev`, `claude`
    /// lanciato a mano) hanno un gruppo loro e resterebbero vivi.
    fn on_released(&mut self, cx: &mut App) {
        for (_, terminal) in self.terminals.values() {
            crate::process::terminate_terminal(terminal.read(cx), cx);
        }
        // Fix round 1 (Task 6 review, finding 1): lo snapshot per il salvataggio in
        // debounce va preso PRIMA di togliere le sessioni dallo store, altrimenti
        // node_save_data non trova piu' il session_id e ogni nodo claude si salva
        // con data: null (una tab chiusa perderebbe tutte le sessioni salvate).
        let save = self.take_pending_save(cx);
        if let Some(store) = superzed_sessions::SessionStore::global(cx) {
            let keys: Vec<String> = self.sessions.drain().map(|(_, s)| s.key).collect();
            store.update(cx, |s, cx| keys.iter().for_each(|k| s.unregister(k, cx)));
        }
        if let Some(save) = save {
            cx.foreground_executor().spawn(save).detach();
        }
    }

    /// Se c'e' un salvataggio in debounce lo annulla e restituisce la scrittura
    /// immediata di un'istantanea presa adesso (uscita dall'app, tab chiusa).
    fn take_pending_save(&mut self, cx: &App) -> Option<impl Future<Output = ()> + use<>> {
        let persist = self.persist.as_mut()?;
        if !persist.loaded || persist.save_blocked {
            return None;
        }
        // senza salvataggio in attesa il file e' gia' aggiornato; il debounce si annulla
        drop(persist.pending_save.take()?);
        let fs = persist.fs.clone();
        let root = persist.root.clone();
        let file = self.snapshot(cx);
        Some(async move {
            persistence::save_project_file(fs.as_ref(), &root, &file).await.log_err();
        })
    }

    /// Salvataggio con debounce: ogni chiamata sposta in avanti il timer.
    pub fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let Some(persist) = self.persist.as_mut() else {
            return;
        };
        if !persist.loaded || persist.save_blocked {
            return;
        }
        let fs = persist.fs.clone();
        let root = persist.root.clone();
        persist.pending_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let Ok(file) = this.update(cx, |this, cx| this.snapshot(cx)) else {
                return;
            };
            persistence::save_project_file(fs.as_ref(), &root, &file).await.log_err();
            this.update(cx, |this, _| {
                if let Some(p) = this.persist.as_mut() {
                    p.pending_save = None;
                }
            })
            .ok();
        }));
    }
}

impl Focusable for CanvasView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CanvasView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // superzed: ultimo punto di aggancio se il MultiWorkspace non c'era ancora in `new`.
        self.ensure_multi_workspace_subscription(window, cx);
        // un rem cambiato senza passare dalle impostazioni (cmd-+): layout, rilascio e
        // hit-test seguono le misure del disegno prima di disegnare i nodi
        self.sync_layout_metrics(cx);
        let grips = DockKind::ALL.into_iter().filter_map(|kind| self.container_grip(kind, cx)).collect::<Vec<_>>();
        let nodes = self
            .model
            .frames()
            .iter()
            .flat_map(|frame| {
                self.render_node(frame, window, cx)
                    .into_iter()
                    .chain(self.selection_ring(frame, cx))
                    .chain(self.free_node_grip(frame, cx))
            })
            .chain(grips)
            .collect::<Vec<_>>();
        let bounds_cell = self.canvas_bounds.clone();
        let pending_zoom = self.pending_zoom.clone();
        let this = cx.entity().downgrade();
        let zoom_target = this.clone();
        div()
            .id("superzed-canvas")
            .debug_selector(|| "superzed-canvas".into())
            // superzed: "CanvasFocused" solo con il focus sul canvas stesso: i tasti di
            // modifica dei nodi (frecce, backspace, invio...) non devono arrivare a
            // terminali e note
            .key_context(if self.focus_handle.is_focused(window) {
                "Canvas CanvasFocused"
            } else {
                "Canvas"
            })
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().colors().editor_background)
            .when(matches!(self.drag, Some(DragState::Move { .. })), |d| d.cursor_grabbing())
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .capture_pinch(cx.listener(Self::on_pinch))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .on_modifiers_changed(cx.listener(|this, e: &gpui::ModifiersChangedEvent, window, cx| {
                this.hints_modifiers_changed(e.modifiers, window, cx)
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_background_mouse_down))
            .on_action(cx.listener(|this, _: &CanvasZoomIn, _, cx| {
                let c = this.local_center();
                this.zoom_by(c, ZOOM_STEP, cx);
            }))
            .on_action(cx.listener(|this, _: &CanvasZoomOut, _, cx| {
                let c = this.local_center();
                this.zoom_by(c, 1. / ZOOM_STEP, cx);
            }))
            .on_action(cx.listener(|this, _: &CanvasZoomReset, _, cx| this.zoom_reset(cx)))
            .on_action(cx.listener(|this, _: &superzed_actions::SelectAllNodes, _, cx| {
                this.model.select_all();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::DeselectNodes, _, cx| {
                this.model.clear_selection();
                cx.notify();
            }))
            .on_action(cx.listener(|this, a: &superzed_actions::MoveSelectedNodes, _, cx| {
                this.model.move_selected(point(a.dx, a.dy));
                this.nodes_changed(cx);
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::FocusSelectedNode, window, cx| {
                let top = this
                    .model
                    .frames()
                    .iter()
                    .rev()
                    .find(|f| this.model.is_selected(f.id))
                    .map(|f| f.id);
                if let Some(id) = top {
                    this.focus_node(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::CloseNode, window, cx| {
                let ids = this.target_nodes(window, cx);
                this.close_nodes_with_confirm(ids, window, cx);
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::RenameNode, window, cx| {
                if let Some(id) = this.target_nodes(window, cx).first().copied() {
                    this.rename_node(id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::CycleNodeColor, window, cx| {
                let ids = this.target_nodes(window, cx);
                this.cycle_color(&ids, cx);
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::ToggleGolden, window, cx| {
                let ids = this.target_nodes(window, cx);
                this.toggle_golden(&ids, cx);
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::ZoomToFocusedNode, window, cx| {
                this.zoom_to_focused_node(window, cx)
            }))
            // navigazione da tastiera tra le chat (`navigation`)
            .on_action(cx.listener(|this, _: &superzed_actions::FocusChatUp, window, cx| {
                this.focus_chat(navigation::Direction::Up, window, cx)
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::FocusChatDown, window, cx| {
                this.focus_chat(navigation::Direction::Down, window, cx)
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::FocusChatLeft, window, cx| {
                this.focus_chat(navigation::Direction::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::FocusChatRight, window, cx| {
                this.focus_chat(navigation::Direction::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::FocusNextArea, window, cx| {
                this.focus_area(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &superzed_actions::FocusPreviousArea, window, cx| {
                this.focus_area(false, window, cx)
            }))
            // bounds del canvas (per gli anchor) e listener di finestra per i
            // trascinamenti: ricevono i movimenti anche sopra i nodi, che occludono
            .child(
                canvas(
                    move |bounds, window, _| {
                        bounds_cell.set(bounds);
                        // zoom rimandato (`zoom_to_node_when_ready`): i limiti ci sono,
                        // si applica prima del prossimo frame (qui, a disegno in corso,
                        // un notify non lo richiederebbe)
                        if bounds.size.width > px(0.) && bounds.size.height > px(0.)
                            && let Some(id) = pending_zoom.take()
                        {
                            window.on_next_frame(move |_, cx| {
                                zoom_target.update(cx, |c, cx| c.zoom_to_node(id, cx)).ok();
                            });
                        }
                        window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
                    },
                    move |_, hitbox, window, _| {
                        // lo scroll a due dita si decide qui, prima dei nodi
                        scroll_route::register_scroll_capture(this.clone(), hitbox, window);
                        let on_move = this.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                            if phase.bubble() {
                                on_move.update(cx, |c, cx| c.on_drag_move(e, window, cx)).ok();
                            }
                        });
                        let on_up = this;
                        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                            if phase.bubble() {
                                on_up.update(cx, |c, cx| c.end_drag(e, cx)).ok();
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            // vedi `menu_keys`
            .child(div().key_context("CanvasFocused").track_focus(&self.menu_keys).absolute().size_0())
            .children(nodes)
            // sopra i nodi, senza hitbox
            .children(self.render_shortcut_hints(window, cx))
    }
}

pub(crate) fn no_send_exports() -> Vec<(String, String)> {
    vec![(superzed_sources::gmail::NO_SEND_ENV.to_string(), "1".to_string())]
}

/// Titoli di default salvati prima dell'interfaccia in inglese: si mostrano in inglese
/// (un titolo scelto dall'utente resta com'e').
fn legacy_title(kind: NodeKind, title: &str) -> &str {
    match (kind, title) {
        (NodeKind::Note, "nota") => "note",
        (NodeKind::Automations, "automazioni") => "automations",
        _ => title,
    }
}

/// Domanda di conferma della chiusura: `(titolo, ha una sessione)` per nodo. Un nodo solo
/// si nomina; per un claude o un terminale si dice che la sessione finisce.
fn close_confirm_message(nodes: &[(String, bool)]) -> String {
    match nodes {
        [(title, session)] => {
            let mut message = format!("Close “{title}”?");
            if *session {
                message.push_str(" The running session will be ended.");
            }
            message
        }
        _ => format!("Close {} nodes?", nodes.len()),
    }
}

/// Titoli elencati nel messaggio di «Close idle chats»; oltre, «… and N more».
const IDLE_LIST_MAX: usize = 8;

/// Messaggio di «Close idle chats»: quante si chiudono (con i titoli) e quante
/// restano perche' golden o attive.
fn idle_close_message(project: &str, titles: &[String], golden: usize, active: usize) -> String {
    let kept: Vec<String> = [(golden, "golden"), (active, "active")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
    let keeps = if kept.is_empty() { String::new() } else { format!(" Keeps {}.", kept.join(" and ")) };
    if titles.is_empty() {
        return format!("No idle chats to close in {project}.{keeps}");
    }
    let mut message = match titles.len() {
        1 => format!("Close 1 idle chat of {project}?{keeps}"),
        count => format!("Close {count} idle chats of {project}?{keeps}"),
    };
    for title in titles.iter().take(IDLE_LIST_MAX) {
        message.push_str(&format!("\n• {title}"));
    }
    if titles.len() > IDLE_LIST_MAX {
        message.push_str(&format!("\n… and {} more", titles.len() - IDLE_LIST_MAX));
    }
    message
}

/// Aggiunge o toglie `id` dalle fonti del progetto; `false` se era gia' cosi'.
fn toggle_source(sources: &mut Vec<String>, id: &str, used: bool) -> bool {
    if sources.iter().any(|s| s == id) == used {
        return false;
    }
    if used {
        sources.push(id.to_string());
    } else {
        sources.retain(|s| s != id);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{RecordingContent, init_test};
    use superzed_sources::test_support::{FakeRunner, github_connections, github_runner};
    use gpui::{
        KeyBinding, Modifiers, MouseButton, PinchEvent, ScrollDelta, ScrollWheelEvent,
        TestAppContext, TouchPhase, VisualTestContext, WeakEntity, point, px, size,
    };

    fn canvas_window(cx: &mut TestAppContext) -> (Entity<CanvasView>, &mut VisualTestContext) {
        init_test(cx);
        let (canvas, cx) =
            cx.add_window_view(|window, cx| CanvasView::new(WeakEntity::new_invalid(), window, cx));
        cx.simulate_resize(size(px(1000.), px(800.)));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (canvas, cx)
    }

    fn frame(id: u64, x: f32, y: f32) -> NodeFrame {
        NodeFrame {
            id: NodeId(id),
            kind: NodeKind::Note,
            origin: point(x, y),
            size: size(200., 150.),
            title: format!("n{id}"),
            color: None,
            group: None,
            dock: None,
            golden: false,
        }
    }

    // Verifica manuale 2026-09-26: con due claude, il click sulla prima riga di
    // Sessioni alternava i nodi. La lista seguiva lo z-order, che il click cambia.
    #[gpui::test]
    fn terminal_nodes_order_ignores_z_order(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        canvas.update(cx, |c, cx| {
            for id in [1, 2] {
                c.model.insert_restored(NodeFrame { kind: NodeKind::Claude, ..frame(id, 0., 0.) });
            }
            c.model.bring_to_front(NodeId(1));
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            assert_eq!(ids, vec![NodeId(1), NodeId(2)]);
        });
    }

    fn scroll(x: f32, y: f32, dy: f32, cmd: bool) -> ScrollWheelEvent {
        ScrollWheelEvent {
            position: point(px(x), px(y)),
            delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
            modifiers: Modifiers { platform: cmd, ..Default::default() },
            touch_phase: TouchPhase::Moved,
        }
    }

    pub(super) fn redraw(cx: &mut VisualTestContext) {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn scroll_on_background_pans(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        cx.simulate_event(scroll(800., 700., 30., false));
        canvas.read_with(cx, |c, _| assert_eq!(c.viewport().offset, point(0., 30.)));
    }

    #[gpui::test]
    fn cmd_scroll_zooms_around_cursor(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        let before = canvas.read_with(cx, |c, _| c.viewport().to_canvas(point(500., 400.)));
        cx.simulate_event(scroll(500., 400., 10., true));
        canvas.read_with(cx, |c, _| {
            let v = c.viewport();
            assert!((v.zoom - crate::viewport::ZOOM_STEP).abs() < 1e-6);
            let after = v.to_canvas(point(500., 400.));
            assert!((before.x - after.x).abs() < 1e-3 && (before.y - after.y).abs() < 1e-3);
        });
    }

    #[gpui::test]
    fn pinch_zooms_even_over_a_node(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        canvas.update(cx, |c, cx| {
            let content = Box::new(RecordingContent::new(cx));
            c.insert_restored_node(frame(1, 100., 100.), content, cx);
        });
        redraw(cx);
        cx.simulate_event(PinchEvent {
            position: point(px(150.), px(150.)),
            delta: 0.5,
            modifiers: Modifiers::default(),
            phase: TouchPhase::Moved,
        });
        canvas.read_with(cx, |c, _| assert!((c.viewport().zoom - 1.5).abs() < 1e-6));
    }

    // Task 10: lo scroll resta al nodo solo se ha il focus; senza focus fa pan
    #[gpui::test]
    fn scroll_inside_node_pans_unless_it_has_focus(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        canvas.update(cx, |c, cx| {
            let content = Box::new(RecordingContent::new(cx));
            c.insert_restored_node(frame(1, 100., 100.), content, cx);
        });
        redraw(cx);
        cx.simulate_event(scroll(150., 180., 30., false));
        canvas.read_with(cx, |c, _| assert_eq!(c.viewport().offset, point(0., 30.), "senza focus: pan"));
        canvas.update_in(cx, |c, window, cx| c.focus_node(NodeId(1), window, cx));
        redraw(cx);
        cx.executor().advance_clock(Duration::from_millis(PAN_LATCH_MS + 1));
        cx.simulate_event(scroll(150., 210., 30., false));
        canvas.read_with(cx, |c, _| assert_eq!(c.viewport().offset, point(0., 30.), "con il focus: niente pan"));
        // cmd+scroll sopra un nodo arriva ancora allo zoom del canvas
        cx.simulate_event(scroll(150., 210., 30., true));
        canvas.read_with(cx, |c, _| assert!(c.viewport().zoom > 1.0));
    }

    #[gpui::test]
    fn zoom_actions_and_reset(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        canvas.update_in(cx, |c, window, cx| c.focus_handle.focus(window, cx));
        cx.dispatch_action(superzed_actions::CanvasZoomIn);
        cx.dispatch_action(superzed_actions::CanvasZoomIn);
        canvas.read_with(cx, |c, _| assert!(c.viewport().zoom > 1.2));
        cx.dispatch_action(superzed_actions::CanvasZoomReset);
        canvas.read_with(cx, |c, _| assert_eq!(c.viewport().zoom, 1.0));
        cx.dispatch_action(superzed_actions::CanvasZoomOut);
        canvas.read_with(cx, |c, _| assert!(c.viewport().zoom < 1.0));
    }

    // limite dello spike: un nodo creato a canvas zoomato partiva col font base.
    // Lo zoom e' solo del canvas: il font di ogni nodo e' lo zoom del canvas.
    #[gpui::test]
    fn new_node_gets_the_canvas_zoom(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        let zoom = content.zoom.clone();
        canvas.update(cx, |c, cx| {
            c.zoom_by(point(0., 0.), 2.0, cx);
            c.add_node(NodeKind::Note, Box::new(content), size(200., 150.), "n", cx);
        });
        assert_eq!(zoom.get(), 2.0);
        canvas.update(cx, |c, cx| c.zoom_reset(cx));
        assert_eq!(zoom.get(), 1.0);
    }

    #[gpui::test]
    fn title_bar_scales_with_zoom(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        canvas.update(cx, |c, cx| {
            let content = Box::new(RecordingContent::new(cx));
            c.insert_restored_node(frame(7, 10., 10.), content, cx);
        });
        redraw(cx);
        let h1 = cx.debug_bounds("superzed-node-title-7").unwrap().size.height;
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 2.0, cx));
        redraw(cx);
        let h2 = cx.debug_bounds("superzed-node-title-7").unwrap().size.height;
        assert!((f32::from(h2) / f32::from(h1) - 2.0).abs() < 0.1, "h1 = {h1:?}, h2 = {h2:?}");
    }

    #[gpui::test]
    fn space_drag_pans_only_while_space_is_held(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        canvas.update_in(cx, |c, window, cx| c.focus_handle.focus(window, cx));
        let none = Modifiers::default();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_mouse_down(point(px(500.), px(400.)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(px(540.), px(420.)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(px(540.), px(420.)), MouseButton::Left, none);
        cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("space").unwrap() });
        canvas.read_with(cx, |c, _| assert_eq!(c.viewport().offset, point(40., 20.)));
        // senza spazio il trascinamento sullo sfondo non fa pan
        cx.simulate_mouse_down(point(px(500.), px(400.)), MouseButton::Left, none);
        cx.simulate_mouse_move(point(px(600.), px(500.)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(px(600.), px(500.)), MouseButton::Left, none);
        canvas.read_with(cx, |c, _| assert_eq!(c.viewport().offset, point(40., 20.)));
    }

    #[gpui::test]
    fn note_saves_its_text_and_removal_calls_on_close(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        let id = canvas.update_in(cx, |c, window, cx| c.new_note(window, cx));
        canvas.update_in(cx, |c, window, cx| {
            let note = c.note_editor(id).unwrap();
            note.update(cx, |e, cx| e.set_text("ciao", window, cx));
        });
        canvas.read_with(cx, |c, cx| {
            assert_eq!(c.node_save_data(id, cx), serde_json::json!({ "text": "ciao" }));
        });
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        let closed = content.closed.clone();
        canvas.update(cx, |c, cx| {
            let other = c.add_node(NodeKind::Note, Box::new(content), size(200., 150.), "x", cx);
            c.remove_nodes(&[other], cx);
            assert!(c.model().frame(other).is_none());
        });
        assert!(closed.get());
    }

    #[gpui::test]
    async fn a_chat_opened_from_the_mail_cannot_send(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        cx.run_until_parked();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_claude_in_kanban_without_send("Leggi questo thread".into(), window, cx);
        });
        cx.run_until_parked();
        let command = canvas.read_with(cx, |c, _| c.last_claude_command().map(str::to_string)).unwrap();
        assert!(command.starts_with("export GOG_GMAIL_NO_SEND="), "{command}");
        canvas.update_in(cx, |c, window, cx| {
            c.new_claude_in_kanban_with_prompt(Some("altro".into()), window, cx);
        });
        cx.run_until_parked();
        let command = canvas.read_with(cx, |c, _| c.last_claude_command().map(str::to_string)).unwrap();
        assert!(!command.contains("GOG_GMAIL_NO_SEND"), "{command}");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn every_new_workspace_gets_a_canvas(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let window =
            cx.add_window(|window, cx| workspace::MultiWorkspace::test_new(project, window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        cx.run_until_parked();
        workspace.read_with(cx, |ws, _| assert!(crate::canvas_for(ws).is_some()));
        let canvas = workspace.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
        canvas.read_with(cx, |c, _| assert_eq!(c.model().frames().len(), 0));
        // l'azione parte dal focus: metterlo nel workspace, come quando l'utente preme la shortcut
        workspace.update_in(cx, |ws, window, cx| window.focus(&ws.focus_handle(cx), cx));
        cx.dispatch_action(superzed_actions::NewNote);
        cx.run_until_parked();
        let canvas = workspace.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
        canvas.read_with(cx, |c, _| assert_eq!(c.model().frames().len(), 1));
    }

    #[gpui::test]
    async fn empty_workspace_still_dispatches_focus_canvas_from_the_keyboard(
        cx: &mut TestAppContext,
    ) {
        // Regression: senza item il pane non viene disegnato (l'area editor
        // "non esiste"), ma il suo focus handle resta quello puntato da
        // `Workspace::focus_handle` upstream. Se quell'handle non resta
        // tracciato nell'albero di dispatch, `FocusCanvas`/`FocusEditor`/`NewNote`
        // dispatchati da tastiera non arrivano piu' a nessun handler quando
        // il progetto e' vuoto (o subito dopo un cambio di workspace).
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let window =
            cx.add_window(|window, cx| workspace::MultiWorkspace::test_new(project, window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        cx.run_until_parked();
        let canvas = workspace.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
        let canvas_focus = canvas.read_with(cx, |c, cx| c.focus_handle(cx));
        cx.update(|window, _| assert!(!canvas_focus.is_focused(window)));
        workspace.update_in(cx, |ws, window, cx| window.focus(&ws.focus_handle(cx), cx));
        cx.dispatch_action(superzed_actions::FocusCanvas);
        cx.run_until_parked();
        cx.update(|window, _| assert!(canvas_focus.is_focused(window), "focus al canvas"));
    }

    // Verifica manuale 2026-09-26: aprire una cartella da un progetto untitled con
    // nodi sul canvas rimpiazzava il workspace e il canvas andava perso.
    #[gpui::test]
    async fn opening_a_folder_keeps_an_untitled_canvas_with_nodes(cx: &mut TestAppContext) {
        init_test(cx);
        let app_state = cx.update(workspace::AppState::test);
        app_state.fs.as_fake().insert_tree("/p1", json!({ "f.txt": "" })).await;
        let project = project::Project::test(app_state.fs.clone(), [], cx).await;
        let window =
            cx.add_window(|window, cx| workspace::MultiWorkspace::test_new(project, window, cx));
        let untitled = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        cx.run_until_parked();
        let canvas = untitled.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.new_note(window, cx);
        });
        let open = window
            .update(cx, |mw, window, cx| {
                mw.open_project(vec!["/p1".into()], workspace::OpenMode::Activate, window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        let p1 = open.await.unwrap();
        assert_ne!(p1, untitled);
        window
            .read_with(cx, |mw, _| assert_eq!(mw.workspaces().count(), 2, "l'untitled resta"))
            .unwrap();
    }

    // Verifica manuale 2026-09-26: `cmd-t` mandava in panic l'app. Le azioni dei nodi
    // sono registrate sul Workspace e il canvas, chiamato dentro quell'update, rilegge
    // il Workspace (root del progetto) -> "already being updated".
    #[gpui::test]
    async fn new_terminal_dispatched_as_action_opens_a_node(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        workspace.update_in(cx, |ws, window, cx| window.focus(&ws.focus_handle(cx), cx));
        cx.dispatch_action(superzed_actions::NewTerminal);
        cx.run_until_parked();
        let ids: Vec<NodeId> = canvas.read_with(cx, |c, _| c.terminals.keys().copied().collect());
        assert_eq!(ids.len(), 1, "un nodo terminale aperto dall'azione");
        canvas.update(cx, |c, cx| c.remove_nodes(&ids, cx));
        cx.run_until_parked();
    }

    // Zoom stabile (spec §1, fix round 1): apre un terminale nel canvas (zoom 1,5), lascia
    // passare l'attesa del resize iniziale e restituisce id, terminale e conteggio dei resize.
    async fn zoomed_canvas_terminal(
        cx: &mut TestAppContext,
    ) -> (Entity<CanvasView>, NodeId, Entity<Terminal>, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.zoom_by(point(0., 0.), 1.5, cx);
            c.open_terminal_with_spec(NodeKind::Shell, None, zsh_spec(), None, window, cx)
        });
        cx.run_until_parked();
        redraw(cx);
        let (id, terminal) = canvas.read_with(cx, |c, _| {
            let (id, (_, t)) = c.terminals.iter().next().expect("terminale");
            (*id, t.clone())
        });
        (canvas, id, terminal, cx)
    }

    // un gesto: tanti piccoli passi di zoom (con il nodo che cresce, per attraversare
    // piu' cambi di griglia); restituisce quante volte e' cambiata la griglia
    fn zoom_sweep(canvas: &Entity<CanvasView>, id: NodeId, terminal: &Entity<Terminal>, cx: &mut VisualTestContext) -> usize {
        let grid = |cx: &mut VisualTestContext| {
            terminal.read_with(cx, |t, _| {
                let b = t.last_content().terminal_bounds;
                (b.num_lines(), b.num_columns())
            })
        };
        let mut changes = 0;
        for _ in 0..12 {
            let g0 = grid(cx);
            canvas.update(cx, |c, cx| {
                c.zoom_by(point(0., 0.), 1.02, cx);
                c.model.frame_mut(id).unwrap().size.height += 40.;
                cx.notify();
            });
            redraw(cx);
            if grid(cx) != g0 {
                changes += 1;
            }
        }
        changes
    }

    // Zoom stabile: nei nodi del canvas il SIGWINCH parte una sola volta, a griglia
    // ferma da 150 ms, qualunque sia il numero di cambi di griglia durante il gesto.
    #[gpui::test]
    async fn canvas_terminals_resize_the_pty_once_per_gesture(cx: &mut TestAppContext) {
        let (canvas, id, terminal, cx) = zoomed_canvas_terminal(cx).await;
        assert!(terminal.read_with(cx, |t, _| t.superzed_grid_only_resize()), "attivo sui nodi del canvas");
        let count = |cx: &mut VisualTestContext| terminal.read_with(cx, |t, _| t.superzed_pty_resize_count());
        let before = count(cx);
        let changes = zoom_sweep(&canvas, id, &terminal, cx);
        assert!(changes >= 2, "il gesto attraversa piu' griglie ({changes})");
        assert_eq!(count(cx), before, "durante il gesto: nessun resize del PTY");
        cx.executor().advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        assert_eq!(count(cx), before, "prima dei 150 ms: nessun resize");
        cx.executor().advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        assert_eq!(count(cx), before + 1, "a griglia ferma: un solo resize");
        let (pty, grid) = terminal.read_with(cx, |t, _| {
            let b = t.last_content().terminal_bounds;
            (t.superzed_last_pty_bounds().map(|p| (p.num_lines(), p.num_columns())), (b.num_lines(), b.num_columns()))
        });
        assert_eq!(pty, Some(grid), "il PTY ha la griglia dell'ultimo passo");
        close_all(&canvas, cx);
    }

    // zoom out oltre la lettura: al posto del terminale il riquadro vuoto, e ne' la griglia
    // ne' il PTY cambiano (claude non si ridisegna a 2 colonne); rientrando torna uguale
    #[gpui::test]
    async fn far_zoom_out_shows_a_placeholder_and_keeps_the_pty(cx: &mut TestAppContext) {
        let (canvas, id, terminal, cx) = zoomed_canvas_terminal(cx).await;
        cx.executor().advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        let state = |cx: &mut VisualTestContext| {
            terminal.read_with(cx, |t, _| {
                let b = t.last_content().terminal_bounds;
                ((b.num_lines(), b.num_columns()), t.superzed_pty_resize_count())
            })
        };
        let placeholder = |cx: &mut VisualTestContext| cx.debug_bounds("superzed-terminal-hidden").is_some();
        let before = state(cx);
        assert!(!placeholder(cx));
        let view = canvas.read_with(cx, |c, _| c.terminals[&id].0.clone());
        cx.update(|window, cx| window.focus(&view.focus_handle(cx), cx));
        let focused = |cx: &mut VisualTestContext| cx.update(|window, cx| view.focus_handle(cx).is_focused(window));
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 1. / 200., cx));
        redraw(cx);
        let min = canvas.read_with(cx, |c, _| c.min_zoom());
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().zoom), min, "zoom out fino al minimo");
        assert!(placeholder(cx), "sotto la soglia di lettura: solo il riquadro");
        cx.executor().advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        assert_eq!(state(cx), before, "griglia e PTY invariati");
        assert!(focused(cx), "il terminale non disegnato tiene il focus");
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 1.5 / min, cx));
        redraw(cx);
        cx.executor().advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        assert!(!placeholder(cx));
        assert_eq!(state(cx), before, "di nuovo a 1.5: stessa griglia, nessun resize");
        assert!(focused(cx), "e rientrando lo ha ancora");
        close_all(&canvas, cx);
    }

    // sotto la soglia di lettura il nodo mostra la vista d'insieme (titolo e stato in un
    // rem adattato al nodo) al posto della barra e del terminale vuoto; a zoom 1 no
    #[gpui::test]
    async fn far_zoom_out_shows_the_node_overview(cx: &mut TestAppContext) {
        let (canvas, id, _terminal, cx) = zoomed_canvas_terminal(cx).await;
        let overview = |cx: &mut VisualTestContext| {
            let drawn = cx.debug_bounds(leak(format!("superzed-node-overview-{}", id.0))).is_some();
            (canvas.read_with(cx, |c, cx| c.node_overview(id, cx)), drawn)
        };
        let set_zoom = |z: f32, cx: &mut VisualTestContext| {
            canvas.update(cx, |c, cx| {
                let f = z / c.viewport().zoom;
                c.zoom_by(point(0., 0.), f, cx)
            });
            redraw(cx);
        };
        set_zoom(1.0, cx);
        assert_eq!(overview(cx), (false, false), "a zoom 1 la barra e il terminale");
        set_zoom(0.2, cx);
        assert_eq!(overview(cx), (true, true), "a 0,2 la vista d'insieme");
        let title = cx.debug_bounds(leak(format!("superzed-node-overview-title-{}", id.0))).expect("titolo");
        let node = cx.debug_bounds(leak(format!("superzed-node-title-{}", id.0))).expect("nodo");
        // a 0,2 la chat 800 x 560 e' 160 x 112 px: rem 11,2 (112 / `OVERVIEW_REF_H_REMS`)
        assert!(title.size.height >= px(11.), "titolo leggibile: {title:?}");
        assert!(inside(node, title), "titolo {title:?} dentro il nodo {node:?}");
        assert!(cx.debug_bounds("superzed-terminal-hidden").is_some(), "il terminale resta montato");
        // zoomando non si arriva sotto il testo leggibile (`min_zoom`); il ramo senza
        // testo resta come difesa, per esempio per un nodo rimpicciolito a zoom basso
        canvas.update(cx, |c, cx| {
            c.viewport.zoom = viewport::MIN_ZOOM;
            c.apply_font_zoom_all(cx);
            c.viewport_changed(cx);
        });
        redraw(cx);
        assert_eq!(overview(cx), (true, true));
        assert!(cx.debug_bounds(leak(format!("superzed-node-overview-title-{}", id.0))).is_none(), "niente testo tagliato");
        set_zoom(1.0, cx);
        assert_eq!(overview(cx), (false, false));
        close_all(&canvas, cx);
    }

    // Task 9: lo zoom out (rotella, pinch, scorciatoia, fit) si ferma dove la vista
    // d'insieme di ogni chat ha ancora testo (`viewport::min_zoom_for`); se il minimo
    // sale (un nodo rimpicciolito) lo zoom non salta, ma non scende piu'.
    #[gpui::test]
    async fn zoom_out_stops_where_every_node_overview_has_text(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(3, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::New, cx);
            c.dock(chats[1], superzed_sessions::Phase::Plan, cx);
        });
        let free = chats[2];
        canvas.update(cx, |c, cx| {
            c.model.frame_mut(free).unwrap().size = size(500., 330.);
            c.nodes_changed(cx);
        });
        redraw(cx);
        assert_eq!(frame_of(&canvas, free, cx).size, size(500., 330.), "la chat libera tiene la sua dimensione");
        let expected = canvas.read_with(cx, |c, _| {
            viewport::min_zoom_for(c.model.frames().iter().filter(|f| f.kind.is_terminal()).map(|f| f.size))
        });
        assert!((expected - 8. * 10. / 330.).abs() < 1e-6, "decide la chat piu' piccola: {expected}");
        let zoom = |cx: &mut VisualTestContext| canvas.read_with(cx, |c, _| c.viewport().zoom);
        let zoom_out = |cx: &mut VisualTestContext| {
            canvas.update(cx, |c, cx| c.zoom_by(point(300., 200.), 1. / viewport::ZOOM_STEP, cx));
            redraw(cx);
        };
        for _ in 0..60 {
            zoom_out(cx);
        }
        assert_eq!(zoom(cx), expected, "zoom out ripetuto: fermo al minimo");
        assert_eq!(canvas.read_with(cx, |c, _| c.min_zoom()), expected);
        for id in &chats {
            let id = id.0;
            assert!(canvas.read_with(cx, |c, cx| c.node_overview(NodeId(id), cx)), "{id}: vista d'insieme");
            assert!(
                cx.debug_bounds(leak(format!("superzed-node-overview-title-{id}"))).is_some(),
                "{id}: la vista d'insieme ha il titolo, non solo lo sfondo"
            );
        }
        // il fit di un rettangolo enorme si ferma anche lui al minimo
        canvas.update(cx, |c, cx| c.zoom_to_rect(point(0., 0.), size(1e6, 1e6), cx));
        assert_eq!(zoom(cx), expected, "fit: fermo al minimo");
        // la chat libera si rimpicciolisce: il minimo sale, lo zoom resta dov'e'
        canvas.update(cx, |c, cx| {
            c.model.frame_mut(free).unwrap().size = size(300., 200.);
            c.nodes_changed(cx);
        });
        redraw(cx);
        // sotto MIN_CHAT_SIZE: il minimo sale solo fino al tetto di MIN_CHAT_SIZE
        assert_eq!(canvas.read_with(cx, |c, _| c.min_zoom()), viewport::MAX_DYNAMIC_MIN_ZOOM);
        assert!(viewport::MAX_DYNAMIC_MIN_ZOOM > expected);
        assert_eq!(zoom(cx), expected, "nessuno zoom in forzato");
        zoom_out(cx);
        assert_eq!(zoom(cx), expected, "lo zoom out non va piu' giu'");
        canvas.update(cx, |c, cx| c.zoom_by(point(300., 200.), viewport::ZOOM_STEP, cx));
        let z = zoom(cx);
        assert!(z > expected, "lo zoom in funziona");
        zoom_out(cx);
        assert_eq!(zoom(cx), z, "e il gesto dopo non torna giu'");
        close_all(&canvas, cx);
    }

    fn inside(outer: Bounds<Pixels>, inner: Bounds<Pixels>) -> bool {
        let e = px(0.5);
        inner.left() >= outer.left() - e
            && inner.right() <= outer.right() + e
            && inner.top() >= outer.top() - e
            && inner.bottom() <= outer.bottom() + e
    }

    // Task 7 (screenshot dell'utente): kanban con 5 chat a zoom 0,15-0,2. La vista
    // d'insieme di ogni chat sta dentro il nodo (titolo, stato, meta) e il testo delle
    // intestazioni sta nella sua fascia, senza il bordo del kanban che lo attraversa.
    #[gpui::test]
    async fn overview_and_kanban_headers_fit_at_a_very_low_zoom(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(5, cx).await;
        for (i, id) in chats.iter().enumerate() {
            canvas.update(cx, |c, cx| {
                c.model.frame_mut(*id).unwrap().title =
                    format!("Error in app.py while running the shell-test notification check {i}");
                c.dock(*id, superzed_sessions::Phase::New, cx)
            });
        }
        // il kanban selezionato: l'anello deve seguire l'intestazione quando sale
        // sopra il bordo del nodo (`header_overhang`), non tagliarne il testo
        canvas.update(cx, |c, cx| {
            c.model.select(kanban, false);
            cx.notify();
        });
        redraw(cx);
        for zoom in [0.15, 0.2] {
            canvas.update(cx, |c, cx| {
                let f = zoom / c.viewport().zoom;
                c.zoom_by(point(0., 0.), f, cx)
            });
            redraw(cx);
            for id in &chats {
                let id = id.0;
                assert!(canvas.read_with(cx, |c, cx| c.node_overview(NodeId(id), cx)), "zoom {zoom}: vista d'insieme");
                let node = cx.debug_bounds(leak(format!("superzed-node-title-{id}"))).expect("nodo");
                let title = cx
                    .debug_bounds(leak(format!("superzed-node-overview-title-{id}")))
                    .unwrap_or_else(|| panic!("zoom {zoom}: titolo della vista d'insieme"));
                assert!(inside(node, title), "zoom {zoom}: titolo {title:?} dentro il nodo {node:?}");
                for part in ["status", "ctx", "overview-meta"] {
                    if let Some(b) = cx.debug_bounds(leak(format!("superzed-node-{part}-{id}"))) {
                        assert!(inside(node, b), "zoom {zoom}: {part} {b:?} dentro il nodo {node:?}");
                    }
                }
            }
            let kanban_title = cx.debug_bounds(leak(format!("superzed-node-title-{}", kanban.0))).expect("barra del kanban");
            let open_top = cx.debug_bounds("superzed-kanban-open-top").is_some();
            // il kanban e' selezionato: l'anello (`superzed-node-ring-<id>`) deve salire
            // sopra l'intestazione insieme a lei, non attraversarne il testo
            let ring = cx
                .debug_bounds(leak(format!("superzed-node-ring-{}", kanban.0)))
                .unwrap_or_else(|| panic!("zoom {zoom}: anello di selezione sul kanban"));
            if open_top {
                let header_top = cx
                    .debug_bounds(leak(format!("superzed-kanban-header-{}", superzed_sessions::Phase::New.as_str())))
                    .expect("intestazione")
                    .top();
                assert!(
                    ring.top() < header_top,
                    "zoom {zoom}: l'anello {ring:?} non copre l'intestazione (top {header_top:?})"
                );
            }
            for phase in superzed_sessions::Phase::ALL {
                let p = phase.as_str();
                let header = cx.debug_bounds(leak(format!("superzed-kanban-header-{p}"))).expect("intestazione");
                for part in ["name", "count"] {
                    let b = cx
                        .debug_bounds(leak(format!("superzed-kanban-{part}-{p}")))
                        .unwrap_or_else(|| panic!("zoom {zoom}: {part} di {p}"));
                    assert!(inside(header, b), "zoom {zoom}: {part} {b:?} nella fascia {header:?}");
                    // la riga di testo sta nella fascia senza i bordi (sopra e sotto, 1 px)
                    assert!(b.size.height <= header.size.height - px(2.), "zoom {zoom}: {part} {b:?} fra i bordi di {header:?}");
                    // il bordo superiore del kanban (appena sopra la sua barra) non passa nel testo
                    let border = kanban_title.top() - px(1.);
                    let crossed = b.top() < border && b.bottom() > kanban_title.top() - px(0.5);
                    assert!(!crossed || open_top, "zoom {zoom}: il bordo del kanban taglia {part} di {p}: {b:?}");
                }
            }
            let plus = cx.debug_bounds("superzed-kanban-new-chat").expect("+");
            let header = cx.debug_bounds(leak(format!("superzed-kanban-header-{}", superzed_sessions::Phase::New.as_str()))).unwrap();
            assert!(inside(header, plus), "zoom {zoom}: + {plus:?} nella fascia {header:?}");
        }
        close_all(&canvas, cx);
    }

    // allo zoom minimo i font scendono sotto il pixel: nota, kanban e terminale si
    // disegnano senza problemi, anche tornando a 1
    #[gpui::test]
    async fn every_node_kind_renders_at_the_minimum_zoom(cx: &mut TestAppContext) {
        let (canvas, _id, _terminal, cx) = zoomed_canvas_terminal(cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.new_note(window, cx);
            c.open_kanban(window, cx);
        });
        for factor in [1. / 1000., 1000.] {
            canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), factor, cx));
            redraw(cx);
        }
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().zoom), viewport::MAX_ZOOM.min(viewport::MIN_ZOOM * 1000.));
        close_all(&canvas, cx);
    }

    // il primo resize del PTY (dai bounds di default al primo layout) parte subito:
    // il primo disegno di claude ha gia' la dimensione giusta
    #[gpui::test]
    async fn the_first_pty_resize_of_a_canvas_terminal_is_immediate(cx: &mut TestAppContext) {
        let (canvas, _id, terminal, cx) = zoomed_canvas_terminal(cx).await;
        assert_eq!(terminal.read_with(cx, |t, _| t.superzed_pty_resize_count()), 1, "senza attendere i 150 ms");
        close_all(&canvas, cx);
    }

    // senza il flag superzed il terminale si comporta come upstream: un resize per cambio
    #[gpui::test]
    async fn terminals_without_the_flag_resize_the_pty_at_every_grid_change(cx: &mut TestAppContext) {
        let (canvas, id, terminal, cx) = zoomed_canvas_terminal(cx).await;
        terminal.update(cx, |t, _| t.superzed_set_grid_only_resize(false));
        let count = |cx: &mut VisualTestContext| terminal.read_with(cx, |t, _| t.superzed_pty_resize_count());
        let before = count(cx);
        let changes = zoom_sweep(&canvas, id, &terminal, cx);
        assert!(changes >= 2);
        assert!(count(cx) - before >= changes, "almeno un resize per cambio di griglia, subito");
        close_all(&canvas, cx);
    }

    pub(super) fn zsh_spec() -> task::SpawnInTerminal {
        task::SpawnInTerminal {
            label: "claude".to_string(),
            command: Some("/bin/zsh".to_string()),
            args: vec!["-f".to_string(), "-i".to_string()],
            use_new_terminal: true,
            allow_concurrent_runs: true,
            hide: task::HideStrategy::Always,
            ..Default::default()
        }
    }

    pub(super) fn store(cx: &mut VisualTestContext) -> Entity<superzed_sessions::SessionStore> {
        cx.update(|_, cx| superzed_sessions::SessionStore::global(cx).expect("store"))
    }

    pub(super) fn hook(key: &str, payload: serde_json::Value) -> superzed_ipc::IpcMessage {
        superzed_ipc::IpcMessage { channel: superzed_ipc::Channel::Hook, node_key: key.into(), payload }
    }

    // `debug_bounds` vuole `&'static str` (upstream): per un selettore costruito a
    // runtime si leaka la stringa (Task 3, 2b: colonne e card del kanban).
    pub(super) fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    // la sessione di un nodo claude segue lo store e il suo id finisce in project.json
    #[gpui::test]
    async fn claude_node_session_id_is_saved(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        let init = NodeSessionInit {
            key: "k-1".into(),
            account: "alice".into(),
            account_id: None,
            session_id: Some("old".into()),
            phase: superzed_sessions::Phase::New,
            times: Default::default(),
        };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().expect("nodo"));
        assert_eq!(canvas.read_with(cx, |c, _| c.session_key(id).map(str::to_string)), Some("k-1".into()));
        store(cx).update(cx, |s, cx| {
            s.apply(hook("k-1", json!({"hook_event_name": "SessionStart", "session_id": "new-id"})), cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let text = fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap();
        let file = parse_project_file(&text).unwrap();
        assert_eq!(file.nodes[0].data, json!({"session_id": "new-id", "phase": "new"}));
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
        assert!(store(cx).read_with(cx, |s, _| s.get("k-1").is_none()), "chiudere il nodo toglie la sessione");
    }

    // Task 2 (2b): la fase di un nodo claude segue lo store e finisce in project.json;
    // le card del kanban (session_cards) la seguono, incl. lo spostamento a mano.
    #[gpui::test]
    async fn claude_node_phase_is_saved_and_cards_follow_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        let init = NodeSessionInit {
            key: "k-ph".into(),
            account: "a".into(),
            account_id: None,
            session_id: Some("sid".into()),
            phase: superzed_sessions::Phase::New,
            times: Default::default(),
        };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        let phase_of = |cx: &mut VisualTestContext| {
            canvas.read_with(cx, |c, cx| {
                c.session_cards(cx).iter().find(|card| card.node == id).map(|card| card.session.phase)
            })
        };
        assert_eq!(phase_of(cx), Some(superzed_sessions::Phase::New));
        store(cx).update(cx, |s, cx| {
            s.apply(
                hook(
                    "k-ph",
                    json!({"hook_event_name": "PreToolUse", "tool_name": "Skill",
                        "tool_input": {"skill": "superpowers:writing-plans"}}),
                ),
                cx,
            )
        });
        assert_eq!(phase_of(cx), Some(superzed_sessions::Phase::Plan));
        canvas.update(cx, |c, cx| c.set_session_phase(id, superzed_sessions::Phase::Spec, cx));
        assert_eq!(phase_of(cx), Some(superzed_sessions::Phase::Spec), "a mano anche indietro");
        cx.run_until_parked();
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let file = parse_project_file(&fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap()).unwrap();
        assert_eq!(file.nodes[0].data, json!({"session_id": "sid", "phase": "spec"}));
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn saved_data_without_or_with_unknown_phase_is_new(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        for (data, expected) in [
            (json!({"session_id": "x", "phase": "plan"}), superzed_sessions::Phase::Plan),
            (json!({"session_id": "x"}), superzed_sessions::Phase::New),
            (json!({"session_id": "x", "phase": "futura"}), superzed_sessions::Phase::New),
        ] {
            let saved = superzed_sessions::SavedSession::from_data(&data);
            let (_, init) = canvas.read_with(cx, |c, cx| c.claude_spec(&saved, "k", None, cx));
            assert_eq!(init.phase, expected, "{data}");
        }
    }

    #[gpui::test]
    async fn claude_spec_dates_a_new_chat_and_keeps_saved_times(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let before = superzed_sessions::now_unix();
        let (_, init) = canvas.read_with(cx, |c, cx| c.claude_spec(&superzed_sessions::SavedSession::default(), "k", None, cx));
        assert!(init.times.opened_at.is_some_and(|t| t >= before), "chat nuova: nasce adesso");
        assert_eq!(init.times.replied_at, None);
        // chat salvata: le sue date, e nessuna inventata (le riempie il transcript al SessionStart)
        let saved = superzed_sessions::SavedSession::from_data(&json!({"session_id": "x", "opened_at": 5, "replied_at": 9}));
        let (_, init) = canvas.read_with(cx, |c, cx| c.claude_spec(&saved, "k", None, cx));
        assert_eq!((init.times.opened_at, init.times.replied_at), (Some(5), Some(9)));
        let legacy = superzed_sessions::SavedSession::from_data(&json!({"session_id": "x"}));
        let (_, init) = canvas.read_with(cx, |c, cx| c.claude_spec(&legacy, "k", None, cx));
        assert_eq!(init.times, superzed_sessions::SessionTimes::default());
    }

    #[test]
    fn overview_freshness_follows_the_last_reply_unless_the_chat_is_alive() {
        use crate::age::Freshness;
        let now = chrono::Local::now();
        let days = |d: i64| Some((now - chrono::Duration::days(d)).timestamp());
        let mut s = superzed_sessions::Session::new("a".into(), None);
        assert_eq!(overview_freshness(&s, now), None, "senza date");
        s.times.opened_at = days(20);
        assert_eq!(overview_freshness(&s, now), Some(Freshness::Stale), "mai risposto: dalla nascita");
        s.times.replied_at = days(1);
        assert_eq!(overview_freshness(&s, now), Some(Freshness::Fresh), "dall'ultima risposta");
        s.times.replied_at = days(30);
        s.set_state(superzed_sessions::SessionState::Working);
        assert_eq!(overview_freshness(&s, now), None, "al lavoro: viva");
        s.set_state(superzed_sessions::SessionState::NeedsInput(superzed_sessions::InputKind::Permission));
        assert_eq!(overview_freshness(&s, now), None, "aspetta l'utente: viva");
        s.set_state(superzed_sessions::SessionState::Idle);
        assert_eq!(overview_freshness(&s, now), Some(Freshness::Stale));
    }

    #[gpui::test]
    async fn overview_shows_opened_and_replied_dates_at_the_edges(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let now = chrono::Local::now();
        let days = |d: i64| Some((now - chrono::Duration::days(d)).timestamp());
        for (key, times) in [
            ("k-d1", superzed_sessions::SessionTimes { opened_at: days(20), replied_at: days(1) }),
            ("k-d2", superzed_sessions::SessionTimes { opened_at: None, replied_at: days(0) }),
        ] {
            let init = NodeSessionInit { key: key.into(), account: "a".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times };
            canvas.update_in(cx, |c, window, cx| {
                c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
            });
        }
        cx.run_until_parked();
        let ids: Vec<u64> = canvas.read_with(cx, |c, _| c.terminals.keys().map(|id| id.0).collect());
        assert_eq!(ids.len(), 2);
        canvas.update(cx, |c, cx| {
            let f = 0.3 / c.viewport().zoom;
            c.zoom_by(point(0., 0.), f, cx)
        });
        redraw(cx);
        let with_opened = canvas.read_with(cx, |c, _| ids.iter().copied().find(|id| c.session_key(NodeId(*id)) == Some("k-d1")));
        for id in &ids {
            assert!(canvas.read_with(cx, |c, cx| c.node_overview(NodeId(*id), cx)), "vista d'insieme");
            let node = cx.debug_bounds(leak(format!("superzed-node-title-{id}"))).expect("nodo");
            let row = cx.debug_bounds(leak(format!("superzed-node-overview-dates-{id}"))).expect("riga delle date");
            assert!(inside(node, row), "riga {row:?} dentro il nodo {node:?}");
            let replied = cx.debug_bounds(leak(format!("superzed-node-overview-replied-{id}"))).expect("ultima risposta");
            assert!(inside(node, replied));
            let opened = cx.debug_bounds(leak(format!("superzed-node-overview-opened-{id}")));
            if Some(*id) == with_opened {
                let opened = opened.expect("nascita");
                assert!(inside(node, opened));
                assert!(opened.right() <= replied.left(), "nascita {opened:?} a sinistra della risposta {replied:?}");
                assert!(opened.left() - row.left() < replied.left() - opened.right(), "ai bordi, non ammucchiate");
            } else {
                assert!(opened.is_none(), "senza nascita niente icona");
                // senza nascita la risposta resta a destra
                assert!(replied.right() > row.left() + row.size.width / 2., "risposta {replied:?} a destra in {row:?}");
            }
        }
        canvas.update(cx, |c, cx| c.remove_nodes(&ids.iter().map(|id| NodeId(*id)).collect::<Vec<_>>(), cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn claude_spec_resumes_only_existing_transcripts(cx: &mut TestAppContext) {
        let config = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(config.path().join("projects/-repo")).unwrap();
        std::fs::write(config.path().join("projects/-repo/abc.jsonl"), "{}").unwrap();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let hooks = superzed_sessions::launch::HookEnv {
            socket: "/s.sock".into(),
            token: "t".into(),
            settings: "/h/settings.json".into(),
            bin_dir: "/h/bin".into(),
        };
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), Some(hooks)));
        let (spec, init) = canvas.read_with(cx, |c, cx| {
            c.claude_spec(&superzed_sessions::SavedSession { session_id: Some("abc".into()), ..Default::default() }, "k-9", None, cx)
        });
        assert!(spec.args[2].contains("--resume 'abc'"));
        assert_eq!(init.session_id.as_deref(), Some("abc"));
        assert_eq!(spec.env.get("SUPERZED_NODE").map(String::as_str), Some("k-9"));
        let (spec, init) = canvas.read_with(cx, |c, cx| {
            c.claude_spec(&superzed_sessions::SavedSession { session_id: Some("missing".into()), ..Default::default() }, "k-9", None, cx)
        });
        assert!(spec.args[2].contains("--session-id"));
        assert_ne!(init.session_id.as_deref(), Some("missing"));
    }

    // Fix wave finale (finding 1): il restore ha un id salvato ma il transcript non esiste
    // piu', quindi claude_spec ne lancia uno nuovo. `SessionStart` non fara' mai la differenza
    // (lo store e' gia' registrato con l'id nuovo), quindi tocca a open_claude_terminal
    // programmare subito il salvataggio; project.json deve finire con l'id lanciato.
    // Verifica manuale 2026-09-26 (p3): il restore avviene durante il caricamento del
    // canvas, quando i salvataggi vengono scartati; l'id nuovo non finiva mai su disco.
    #[gpui::test]
    async fn resave_requested_during_load_happens_after_load(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        // un project.json con un nodo claude il cui transcript non esiste piu'
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{
                "id": 12, "kind": "claude", "origin": [10.0, 10.0], "size": [500.0, 300.0],
                "title": "claude", "data": {"session_id": "old-id"}
            }]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let text = fs.load(Path::new("/repo/.superzed/project.json")).await.expect("salvato dopo il load");
        let saved = parse_project_file(&text).unwrap();
        let id = saved.nodes[0].data["session_id"].as_str().unwrap().to_string();
        assert_ne!(id, "old-id", "l'id lanciato, non quello senza transcript");
        let ids: Vec<_> = canvas.read_with(cx, |c, _| c.terminals.keys().copied().collect());
        canvas.update(cx, |c, cx| c.remove_nodes(&ids, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn restoring_a_claude_node_with_a_stale_transcript_resaves_the_launched_id(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        // niente file per "old-id" sotto projects/: transcript_exists torna false.
        let (_workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let (_, init) = canvas.read_with(cx, |c, cx| {
            c.claude_spec(&superzed_sessions::SavedSession { session_id: Some("old-id".into()), ..Default::default() }, "k-restore", None, cx)
        });
        assert_ne!(init.session_id.as_deref(), Some("old-id"), "senza transcript claude_spec lancia un id nuovo");
        let launched_id = init.session_id.clone().unwrap();
        let restored = NodeFrame {
            id: NodeId(11),
            kind: NodeKind::Claude,
            origin: point(10., 10.),
            size: size(500., 300.),
            title: "vecchio".to_string(),
            color: None,
            group: None,
            dock: None,
            golden: false,
        };
        canvas.update_in(cx, |c, window, cx| {
            c.open_claude_terminal(Some(restored), Some("old-id"), zsh_spec(), init, window, cx)
        });
        cx.run_until_parked();
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let text = fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap();
        let file = parse_project_file(&text).unwrap();
        assert_eq!(
            file.nodes[0].data,
            json!({"session_id": launched_id, "phase": "new"}),
            "project.json ha l'id lanciato, non quello salvato"
        );
        canvas.update(cx, |c, cx| c.remove_nodes(&[NodeId(11)], cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn focusing_a_claude_node_marks_it_seen(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let init = NodeSessionInit { key: "k-2".into(), account: "a".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        // il nodo nuovo prende il focus: lo si toglie, poi arriva uno Stop
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        store(cx).update(cx, |s, cx| s.apply(hook("k-2", json!({"hook_event_name": "Stop"})), cx));
        assert_eq!(store(cx).read_with(cx, |s, _| s.get("k-2").unwrap().state.clone()), superzed_sessions::SessionState::Done);
        canvas.update_in(cx, |c, window, cx| c.focus_node(id, window, cx));
        cx.run_until_parked();
        assert_eq!(store(cx).read_with(cx, |s, _| s.get("k-2").unwrap().state.clone()), superzed_sessions::SessionState::Idle);
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn canvas_attention_is_the_most_urgent_session(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        for key in ["a", "b"] {
            let init = NodeSessionInit { key: key.into(), account: "x".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
            canvas.update_in(cx, |c, window, cx| {
                c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
            });
        }
        cx.run_until_parked();
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        let attention = |cx: &mut VisualTestContext| canvas.read_with(cx, |c, cx| c.attention(cx));
        assert_eq!(attention(cx), superzed_sessions::Attention::None);
        store(cx).update(cx, |s, cx| s.apply(hook("a", json!({"hook_event_name": "Stop"})), cx));
        assert_eq!(attention(cx), superzed_sessions::Attention::Done);
        store(cx).update(cx, |s, cx| s.apply(hook("b", json!({"hook_event_name": "PermissionRequest"})), cx));
        assert_eq!(attention(cx), superzed_sessions::Attention::NeedsInput);
        let tab = cx.update(|_, cx| workspace::superzed_tabs::tab_attention(&workspace, cx));
        assert_eq!(tab, workspace::superzed_tabs::TabAttention::NeedsInput, "hook installato da init");
        let ids: Vec<_> = canvas.read_with(cx, |c, _| c.terminals.keys().copied().collect());
        canvas.update(cx, |c, cx| c.remove_nodes(&ids, cx));
        cx.run_until_parked();
    }

    // Fix round 1 (Task 6 review, finding 2): un salvataggio preso mentre il PTY di un
    // nodo ripristinato sta ancora partendo non deve perdere il session_id salvato.
    #[gpui::test]
    async fn restoring_a_claude_node_keeps_its_session_id_before_the_pty_attaches(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let restored = NodeFrame {
            id: NodeId(9),
            kind: NodeKind::Claude,
            origin: point(30., 40.),
            size: size(500., 300.),
            title: "vecchio".to_string(),
            color: None,
            group: None,
            dock: None,
            golden: false,
        };
        let init = NodeSessionInit {
            key: "k-3".into(),
            account: "a".into(),
            account_id: None,
            session_id: Some("abc".into()),
            phase: superzed_sessions::Phase::New,
            times: Default::default(),
        };
        canvas.update_in(cx, |c, window, cx| {
            // niente run_until_parked qui in mezzo: il PTY non e' ancora partito, quindi
            // `contents` non ha ancora un TerminalNode per questo id
            c.open_terminal_with_spec(NodeKind::Claude, Some(restored), zsh_spec(), Some(init), window, cx)
        });
        canvas.read_with(cx, |c, cx| {
            assert_eq!(c.snapshot(cx).nodes[0].data, json!({"session_id": "abc", "phase": "new"}), "session_id non perso durante l'avvio del PTY");
        });
        cx.run_until_parked();
        canvas.update(cx, |c, cx| c.remove_nodes(&[NodeId(9)], cx));
        cx.run_until_parked();
    }

    // Verifica manuale 2026-09-26: il nodo claude deve mostrare il titolo che gli da'
    // claude (OSC), non "claude"; un nome dato dall'utente resta.
    #[gpui::test]
    async fn claude_node_shows_the_terminal_title_until_renamed(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), None, window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        let title = |cx: &mut VisualTestContext| {
            canvas.read_with(cx, |c, cx| c.terminal_nodes(cx).into_iter().find(|(n, _)| *n == id).unwrap().1)
        };
        assert_eq!(title(cx), "claude");
        let terminal = canvas.read_with(cx, |c, _| c.terminals.get(&id).unwrap().1.clone());
        terminal.update(cx, |t, _| t.input(b"printf '\\033]0;\\342\\234\\263 Fix login bug\\007'\r".to_vec()));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while title(cx) != "Fix login bug" {
            assert!(std::time::Instant::now() < deadline, "titolo OSC non arrivato: {}", title(cx));
            std::thread::sleep(Duration::from_millis(50));
            cx.run_until_parked();
        }
        canvas.update(cx, |c, cx| {
            c.model.frame_mut(id).unwrap().title = "mio".into();
            c.nodes_changed(cx);
        });
        assert_eq!(title(cx), "mio", "il nome dell'utente vince");
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn escape_in_a_claude_node_clears_the_bell(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let init = NodeSessionInit { key: "k-esc".into(), account: "a".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        // un Esc che arriva mentre zsh sta ancora partendo lo fa uscire (il nodo si chiude e
        // la sessione sparisce, ~1 volta su 15): i tasti partono solo col prompt a video
        let terminal = canvas.read_with(cx, |c, _| c.terminals[&id].1.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while terminal.read_with(cx, |t, _| t.get_content().trim().is_empty()) {
            assert!(std::time::Instant::now() < deadline, "prompt di zsh non arrivato");
            std::thread::sleep(Duration::from_millis(20));
            cx.run_until_parked();
        }
        let state = |cx: &mut VisualTestContext| store(cx).read_with(cx, |s, _| s.get("k-esc").unwrap().state.clone());
        // come il keymap vero: nel terminale Esc e ctrl-c sono azioni, non key_down
        cx.update(|_, cx| {
            let send = |k: &str| serde_json::from_value::<terminal_view::SendKeystroke>(json!(k)).unwrap();
            cx.bind_keys([
                KeyBinding::new("escape", send("escape"), Some("Terminal")),
                KeyBinding::new("ctrl-c", send("ctrl-c"), Some("Terminal")),
            ]);
        });
        store(cx).update(cx, |s, cx| s.apply(hook("k-esc", json!({"hook_event_name": "PermissionRequest"})), cx));
        assert!(matches!(state(cx), superzed_sessions::SessionState::NeedsInput(_)));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(state(cx), superzed_sessions::SessionState::Idle, "Esc nel nodo con il focus");
        store(cx).update(cx, |s, cx| s.apply(hook("k-esc", json!({"hook_event_name": "UserPromptSubmit"})), cx));
        cx.simulate_keystrokes("ctrl-c");
        cx.run_until_parked();
        assert_eq!(state(cx), superzed_sessions::SessionState::Idle, "ctrl-c interrompe il lavoro");
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn title_shows_state_context_and_account(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let init = NodeSessionInit { key: "k-3".into(), account: "alice".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| c.terminals.keys().next().unwrap().0);
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        redraw(cx);
        assert!(cx.debug_bounds(leak(format!("superzed-node-status-{id}"))).is_some(), "Starting: icona di avvio");
        assert!(cx.debug_bounds(leak(format!("superzed-node-account-{id}"))).is_none(), "un solo account: niente etichetta");
        assert!(cx.debug_bounds(leak(format!("superzed-node-ctx-{id}"))).is_none(), "contesto ancora ignoto");
        store(cx).update(cx, |s, cx| {
            s.apply(hook("k-3", json!({"hook_event_name": "UserPromptSubmit"})), cx);
            s.apply(
                superzed_ipc::IpcMessage {
                    channel: superzed_ipc::Channel::Status,
                    node_key: "k-3".into(),
                    payload: json!({"context_window": {"remaining_percentage": 55}}),
                },
                cx,
            );
        });
        redraw(cx);
        assert!(cx.debug_bounds(leak(format!("superzed-node-status-{id}"))).is_some(), "Working: icona");
        assert!(cx.debug_bounds(leak(format!("superzed-node-ctx-{id}"))).is_some());
        canvas.update(cx, |c, cx| c.remove_nodes(&[NodeId(id)], cx));
        cx.run_until_parked();
    }

    use crate::automations_layout;
    use crate::kanban_layout;
    use crate::model::DockKind;
    use crate::persistence::{PROJECT_FILE_VERSION, parse_project_file};
    use crate::test_support::workspace_with_repo;
    use serde_json::json;
    use std::path::Path;

    #[gpui::test]
    async fn open_kanban_twice_keeps_one(cx: &mut TestAppContext) {
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let first = canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        let second = canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        assert_eq!(first, second);
        let kanbans = canvas.read_with(cx, |c, _| c.model.frames().iter().filter(|f| f.kind == NodeKind::Kanban).count());
        assert_eq!(kanbans, 1);
        assert_eq!(canvas.read_with(cx, |c, _| c.kanban_node()), Some(first));
        let focused = canvas.read_with(cx, |c, cx| c.node_focus_handle(first, cx).unwrap());
        cx.update(|window, _| assert!(focused.is_focused(window), "focus sul kanban"));
    }

    #[gpui::test]
    async fn kanban_node_is_restored_from_the_file(cx: &mut TestAppContext) {
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{"id": 3, "kind": "kanban", "origin": [0.0, 0.0], "size": [960.0, 360.0], "title": "kanban"}]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.kanban_node()), Some(NodeId(3)));
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban)), kanban_layout::DEFAULT_CHAT_SIZE, "senza chat_size: default");
        let again = canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        assert_eq!(again, NodeId(3), "OpenKanban riusa quello ripristinato");
    }

    // Task 7: la sezione Automazioni si salva e si ripristina con la sua dimensione delle
    // chat; il kanban resta indipendente.
    #[gpui::test]
    async fn automations_container_is_restored_with_its_own_chat_size(cx: &mut TestAppContext) {
        let (_workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [
                {"id": 1, "kind": "kanban", "origin": [0.0, 0.0], "size": [100.0, 100.0], "title": "kanban",
                 "data": {"chat_size": [900.0, 600.0]}},
                {"id": 2, "kind": "automations", "origin": [0.0, 900.0], "size": [100.0, 100.0], "title": "automazioni",
                 "data": {"chat_size": [700.0, 500.0], "other": 1}}
            ]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.container_node(DockKind::Automations), Some(NodeId(2)));
            assert_eq!(c.chat_size(DockKind::Kanban), size(900., 600.));
            assert_eq!(c.chat_size(DockKind::Automations), size(700., 500.));
        });
        canvas.update(cx, |c, cx| c.cycle_color(&[NodeId(2)], cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let saved = parse_project_file(&fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap()).unwrap();
        let a = saved.nodes.iter().find(|n| n.id == 2).unwrap();
        assert_eq!(a.kind, NodeKind::Automations);
        assert_eq!(a.data, json!({"chat_size": [700.0, 500.0], "other": 1}));
    }

    // Spec §3: aprendo un progetto ci sono il kanban e, sotto, le Automazioni; da soli
    // non scrivono project.json.
    #[gpui::test]
    async fn default_containers_appear_without_writing_project_json(cx: &mut TestAppContext) {
        let (_ws, canvas, fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        let (k, a) = canvas.read_with(cx, |c, _| (c.container_node(DockKind::Kanban), c.container_node(DockKind::Automations)));
        let (k, a) = (k.expect("kanban di default"), a.expect("automazioni di default"));
        let (kf, af) = (frame_of(&canvas, k, cx), frame_of(&canvas, a, cx));
        assert_eq!(af.origin, automations_layout::below(kf.origin, kf.size));
        let m = canvas.read_with(cx, |c, cx| c.automations_metrics(cx));
        assert_eq!(af.size, automations_layout::layout(af.origin, &[], kanban_layout::DEFAULT_CHAT_SIZE, m).size);
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        assert!(!fs.is_file(Path::new("/repo/.superzed/project.json")).await, "solo i default: nessun file");
    }

    #[gpui::test]
    async fn closed_containers_come_back_on_the_next_load_and_existing_ones_are_kept(cx: &mut TestAppContext) {
        let file = project_json(json!([
            {"id": 5, "kind": "kanban", "origin": [40.0, 40.0], "size": [100.0, 100.0], "title": "kanban"},
            {"id": 6, "kind": "note", "origin": [0.0, -500.0], "size": [300.0, 200.0], "title": "nota", "data": {"text": "x"}}
        ]));
        let (_ws, canvas, _fs, cx) =
            crate::test_support::workspace_with_repo_opts(cx, json!({ ".superzed": { "project.json": file } }), true).await;
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.container_node(DockKind::Kanban), Some(NodeId(5)), "il kanban del file resta");
            assert_eq!(c.model().frames().iter().filter(|f| f.kind == NodeKind::Kanban).count(), 1);
            assert!(c.container_node(DockKind::Automations).is_some(), "le Automazioni chiuse tornano");
        });
    }

    #[gpui::test]
    async fn the_mail_node_exists_once_and_survives_close(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        cx.run_until_parked();
        let mail = canvas.read_with(cx, |c, _| c.mail_node()).expect("nodo Posta creato al caricamento");
        let again = canvas.update_in(cx, |c, window, cx| c.ensure_mail_node(window, cx));
        assert_eq!(mail, again);
        let note = canvas.update_in(cx, |c, window, cx| c.new_note(window, cx));
        // selezione di tutto + chiusura: la nota va, il nodo Posta resta
        canvas.update_in(cx, |c, window, cx| {
            c.model.select(note, false);
            c.model.select(mail, true);
            let targets = c.target_nodes(window, cx);
            c.close_nodes_with_confirm(targets, window, cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| {
            assert!(c.model.frame(note).is_none(), "la nota si chiude");
            assert!(c.model.frame(mail).is_some(), "il nodo Posta resta");
        });
    }

    #[gpui::test]
    async fn closing_only_the_mail_node_asks_nothing(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        cx.run_until_parked();
        let mail = canvas.read_with(cx, |c, _| c.mail_node()).unwrap();
        canvas.update_in(cx, |c, window, cx| c.close_nodes_with_confirm(vec![mail], window, cx));
        cx.run_until_parked();
        assert!(canvas.read_with(cx, |c, _| c.model.frame(mail).is_some()));
    }

    #[gpui::test]
    async fn mail_settings_are_saved_and_restored(cx: &mut TestAppContext) {
        // gog finto: senza ConnectionStore il nodo userebbe il gog vero
        let (_store, _dir) = with_sources(cx, superzed_sources::test_support::gmail_runner(), github_connections());
        let (_ws, canvas, fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        cx.run_until_parked();
        let view = canvas.read_with(cx, |c, _| c.mail_view()).unwrap();
        view.update_in(cx, |v, window, cx| {
            v.account_editor().update(cx, |e, cx| e.set_text("j.doe@example.com", window, cx));
            v.query_editor().update(cx, |e, cx| e.set_text("from:@acme.example", window, cx));
            v.apply_settings(cx);
        });
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        let saved = parse_project_file(&fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap()).unwrap();
        let record = saved.nodes.iter().find(|n| n.kind == NodeKind::Mail).expect("nodo Posta salvato");
        assert_eq!(record.data, json!({"account": "j.doe@example.com", "query": "from:@acme.example"}));
        // canvas svuotato: le impostazioni possono venire solo dal file
        canvas.update_in(cx, |c, window, cx| {
            let all: Vec<NodeId> = c.model.frames().iter().map(|f| f.id).collect();
            c.remove_nodes(&all, cx);
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(saved.clone()), window, cx);
        });
        cx.run_until_parked();
        let mails = canvas.read_with(cx, |c, _| c.model.frames().iter().filter(|f| f.kind == NodeKind::Mail).count());
        assert_eq!(mails, 1, "il nodo del file, nessun altro");
        let view = canvas.read_with(cx, |c, _| c.mail_view()).unwrap();
        assert_eq!(view.read_with(cx, |v, _| v.settings()), ("j.doe@example.com".into(), "from:@acme.example".into()));
    }

    #[gpui::test]
    async fn automations_move_down_when_the_kanban_grows_into_them(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        for _ in 0..3 {
            canvas.update_in(cx, |c, window, cx| {
                c.test_claude_pty = Some(zsh_spec());
                c.new_claude_in_kanban(window, cx);
            });
            cx.run_until_parked();
        }
        let (k, a) = canvas.read_with(cx, |c, _| (c.kanban_node().unwrap(), c.container_node(DockKind::Automations).unwrap()));
        let (kf, af) = (frame_of(&canvas, k, cx), frame_of(&canvas, a, cx));
        assert!(kf.size.height > 3. * kanban_layout::DEFAULT_CHAT_SIZE.height, "tre righe");
        assert_eq!(af.origin, automations_layout::below(kf.origin, kf.size), "scese sotto il kanban");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn automations_lay_out_their_chats_and_resize_from_the_grip(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        let a = canvas.update(cx, |c, cx| c.ensure_container(DockKind::Automations, cx));
        canvas.update(cx, |c, cx| {
            for id in &chats {
                c.model.frame_mut(*id).unwrap().dock = Some(DockKind::Automations);
            }
            c.relayout_containers(cx);
        });
        let af = frame_of(&canvas, a, cx);
        let m = canvas.read_with(cx, |c, cx| c.automations_metrics(cx));
        let l = automations_layout::layout(af.origin, &chats, kanban_layout::DEFAULT_CHAT_SIZE, m);
        assert_eq!(af.size, l.size);
        assert_eq!(frame_of(&canvas, chats[1], cx).origin, l.chats[1].1);
        assert_eq!(frame_of(&canvas, chats[1], cx).size, kanban_layout::DEFAULT_CHAT_SIZE);
        // maniglia della sezione: chat piu' piccole, stessa riga
        canvas.update(cx, |c, cx| c.resize_container(DockKind::Automations, size(1000., 700.), 2, cx));
        let small = automations_layout::chat_size_for(size(1000., 700.), 2, m);
        assert_eq!(frame_of(&canvas, chats[0], cx).size, small);
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Automations)), small);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn a_free_chat_dropped_on_the_automations_stays_free(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        let a = canvas.update(cx, |c, cx| c.ensure_container(DockKind::Automations, cx));
        let af = frame_of(&canvas, a, cx);
        release_drag_at(&canvas, chats[0], point(af.origin.x + 100., af.origin.y + 200.), cx);
        assert_eq!(frame_of(&canvas, chats[0], cx).dock, None);
        close_all(&canvas, cx);
    }

    // Revisione del Task 7: chiudere le Automazioni libera solo le loro chat; il kanban,
    // le sue chat e la sua dimensione delle chat restano.
    #[gpui::test]
    async fn closing_the_automations_frees_only_their_chats(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        let a = canvas.update(cx, |c, cx| c.ensure_container(DockKind::Automations, cx));
        canvas.update(cx, |c, cx| {
            c.set_chat_size(DockKind::Kanban, size(900., 600.));
            c.set_chat_size(DockKind::Automations, size(700., 500.));
            c.dock(chats[0], superzed_sessions::Phase::New, cx);
            c.model.frame_mut(chats[1]).unwrap().dock = Some(DockKind::Automations);
            c.relayout_containers(cx);
        });
        let docked = frame_of(&canvas, chats[1], cx);
        canvas.update(cx, |c, cx| c.remove_nodes(&[a], cx));
        cx.run_until_parked();
        let free = frame_of(&canvas, chats[1], cx);
        assert_eq!(free.dock, None, "la chat delle Automazioni torna libera");
        assert_eq!((free.origin, free.size), (docked.origin, docked.size), "resta dov'era");
        assert!(frame_of(&canvas, chats[0], cx).in_kanban(), "la chat del kanban resta agganciata");
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.container_node(DockKind::Automations), None);
            assert_eq!(c.chat_size(DockKind::Kanban), size(900., 600.), "il kanban non cambia");
            assert_eq!(c.chat_size(DockKind::Automations), kanban_layout::DEFAULT_CHAT_SIZE, "la prossima sezione parte dal default");
        });
        assert_eq!(frame_of(&canvas, chats[0], cx).size, size(900., 600.));
        close_all(&canvas, cx);
    }

    // Revisione del Task 7: una chat salvata `dock: automations` in un file senza
    // Automazioni torna libera al caricamento, e le Automazioni di default non la agganciano.
    #[gpui::test]
    async fn stale_automations_dock_is_cleared_on_load(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, _fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{"id": 20, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "claude",
                       "dock": "automations", "data": {"phase": "new"}}]
        }))
        .unwrap();
        // il file ricaricato sostituisce il canvas: niente contenitori nel file
        let defaults: Vec<NodeId> = canvas.read_with(cx, |c, _| c.model.frames().iter().map(|f| f.id).collect());
        canvas.update(cx, |c, cx| c.remove_nodes(&defaults, cx));
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(frame_of(&canvas, NodeId(20), cx).dock, None, "senza Automazioni la chat e' libera");
        assert!(canvas.read_with(cx, |c, _| c.container_node(DockKind::Automations).is_some()), "default ricreate");
        assert!(canvas.read_with(cx, |c, _| c.dock_members(DockKind::Automations).is_empty()), "e non la agganciano");
        close_all(&canvas, cx);
    }

    fn draft(name: &str, schedule: &str) -> crate::automation::AutomationDraft {
        crate::automation::AutomationDraft { name: name.into(), schedule: schedule.into(), prompt: "Leggi i log".into(), account: None }
    }

    async fn canvas_with_automation(cx: &mut TestAppContext) -> (Entity<CanvasView>, NodeId, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.create_automation(draft("Log produzione", "4h"), window, cx);
        });
        cx.run_until_parked();
        let node = canvas.read_with(cx, |c, _| *c.automations.keys().next().expect("automazione creata"));
        (canvas, node, cx)
    }

    // Criterio 2: "+" → chat claude agganciata alle Automazioni, definizione nel data,
    // titolo = nome, fascia con nome/frequenza/prossima/esito.
    #[gpui::test]
    async fn creating_an_automation_docks_a_chat_with_its_definition(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        let f = frame_of(&canvas, node, cx);
        assert_eq!(f.dock, Some(DockKind::Automations));
        assert_eq!(f.title, "Log produzione");
        let a = canvas.read_with(cx, |c, _| c.container_node(DockKind::Automations).expect("sezione creata"));
        let af = frame_of(&canvas, a, cx);
        let m = canvas.read_with(cx, |c, cx| c.automations_metrics(cx));
        assert_eq!(f.origin, automations_layout::layout(af.origin, &[node], kanban_layout::DEFAULT_CHAT_SIZE, m).chats[0].1);
        let data = canvas.read_with(cx, |c, cx| c.node_save_data(node, cx));
        assert_eq!(data["automation"]["name"], "Log produzione");
        assert_eq!(data["automation"]["schedule"], "4h");
        assert!(data["session_id"].is_string(), "la sessione resta nel data");
        let rows = canvas.read_with(cx, |c, cx| c.automation_rows(cx));
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].name.as_str(), rows[0].schedule.as_str()), ("Log produzione", "4h"));
        assert!(rows[0].next.is_ok());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn the_form_validates_and_creates(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_automation(window, cx);
        });
        cx.run_until_parked();
        let modal = workspace
            .read_with(cx, |ws, cx| ws.active_modal::<crate::automation_form::AutomationFormModal>(cx))
            .expect("form aperto");
        modal.update_in(cx, |m, window, cx| {
            m.set_fields("Log", "4m", "p", window, cx);
            m.save(window, cx);
        });
        assert!(modal.read_with(cx, |m, _| m.error().unwrap().contains("5m")), "errore mostrato, niente salvataggio");
        modal.update_in(cx, |m, window, cx| m.set_fields("Log", "lun-ven 09:00", "p", window, cx));
        assert!(modal.read_with(cx, |m, _| m.preview().starts_with("next: ")));
        assert_eq!(modal.read_with(cx, |m, _| m.account().map(str::to_string)), None, "Automatic di default");
        modal.update_in(cx, |m, window, cx| {
            m.set_account(Some("a1".into()), cx);
            m.save(window, cx);
        });
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |ws, cx| ws.active_modal::<crate::automation_form::AutomationFormModal>(cx)).is_none());
        let defs: Vec<_> =
            canvas.read_with(cx, |c, _| c.automations.values().map(|r| (r.def.schedule.clone(), r.def.account.clone())).collect());
        assert_eq!(defs, vec![("lun-ven 09:00".to_string(), Some("a1".to_string()))]);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn edit_and_pause_update_the_definition(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.update_automation(node, draft("Log staging", "1h"), window, cx);
            c.toggle_automation_pause(node, cx);
        });
        let d = canvas.read_with(cx, |c, _| c.automation(node).cloned().unwrap());
        assert_eq!((d.name.as_str(), d.schedule.as_str(), d.paused), ("Log staging", "1h", true));
        assert_eq!(frame_of(&canvas, node, cx).title, "Log staging");
        assert!(canvas.read_with(cx, |c, cx| c.automation_rows(cx)[0].paused));
        close_all(&canvas, cx);
    }

    // L'account scelto nel form vince sullo scheduler (che sceglierebbe a2).
    #[gpui::test]
    async fn automation_chat_starts_on_the_chosen_account(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 20.), (10., 30.)]);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            let d = crate::automation::AutomationDraft { account: Some("a1".into()), ..draft("Log", "4h") };
            c.create_automation(d, window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a1")), "{cmd}");
        let node = canvas.read_with(cx, |c, _| *c.automations.keys().next().unwrap());
        let data = canvas.read_with(cx, |c, cx| c.node_save_data(node, cx));
        assert_eq!((data["account"].as_str(), data["automation"]["account"].as_str()), (Some("a1"), Some("a1")));
        close_all(&canvas, cx);
    }

    // Claude non cambia account a processo avviato: la chat si riapre sul nuovo, con la
    // stessa definizione; lo stesso account non la tocca.
    #[gpui::test]
    async fn changing_the_account_reopens_the_automation_chat(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 20.), (10., 30.)]);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.create_automation(draft("Log", "4h"), window, cx);
        });
        cx.run_until_parked();
        let old = canvas.read_with(cx, |c, _| *c.automations.keys().next().unwrap());
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_save_data(old, cx))["account"], "a2", "scheduler");
        let created_at = canvas.read_with(cx, |c, _| c.automation(old).unwrap().created_at);
        canvas.update_in(cx, |c, window, cx| {
            let d = crate::automation::AutomationDraft { account: Some("a2".into()), ..draft("Log", "4h") };
            c.update_automation(old, d, window, cx);
        });
        cx.run_until_parked();
        assert!(canvas.read_with(cx, |c, _| c.automations.contains_key(&old)), "stesso account: la chat resta");
        canvas.update_in(cx, |c, window, cx| {
            let d = crate::automation::AutomationDraft { account: Some("a1".into()), ..draft("Log 2", "1h") };
            c.update_automation(old, d, window, cx);
        });
        cx.run_until_parked();
        let nodes: Vec<NodeId> = canvas.read_with(cx, |c, _| c.automations.keys().copied().collect());
        assert_eq!(nodes.len(), 1);
        let new = nodes[0];
        assert_ne!(new, old);
        assert!(canvas.read_with(cx, |c, _| c.model.frame(old).is_none()), "la vecchia chat e' chiusa");
        let def = canvas.read_with(cx, |c, _| c.automation(new).cloned().unwrap());
        assert_eq!((def.name.as_str(), def.schedule.as_str(), def.account.as_deref()), ("Log 2", "1h", Some("a1")));
        assert_eq!(def.created_at, created_at, "le scadenze continuano da dove erano");
        assert_eq!(frame_of(&canvas, new, cx).dock, Some(DockKind::Automations));
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a1")), "{cmd}");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn automations_are_saved_and_restored(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [
                {"id": 1, "kind": "automations", "origin": [0.0, 0.0], "size": [100.0, 100.0], "title": "automazioni"},
                {"id": 2, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "Log", "dock": "automations",
                 "data": {"phase": "new", "automation": {"name": "Log", "schedule": "4h", "prompt": "p", "paused": true,
                          "catch_up": false, "last_run": "2026-09-27T10:00:00+02:00", "last_outcome": "notified:2"}}},
                {"id": 3, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "rotta", "dock": "automations",
                 "data": {"phase": "new", "automation": {"name": "rotta", "schedule": "boh", "prompt": "p"}}},
                {"id": 4, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "senza", "dock": "automations",
                 "data": {"phase": "new"}}
            ]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        let d = canvas.read_with(cx, |c, _| c.automation(NodeId(2)).cloned().unwrap());
        assert!(d.paused && !d.catch_up);
        assert_eq!(d.last_outcome, Some(crate::automation::Outcome::Notified(2)));
        let rows = canvas.read_with(cx, |c, cx| c.automation_rows(cx));
        let broken = rows.iter().find(|r| r.node == NodeId(3)).unwrap();
        assert!(broken.next.is_err(), "frequenza non valida mostrata nella fascia");
        assert_eq!(frame_of(&canvas, NodeId(4), cx).dock, None, "agganciata senza definizione: libera");
        close_all(&canvas, cx);
    }

    // la sezione disegna il "+" nell'intestazione e una fascia sopra ogni chat; il "+" apre il form
    #[gpui::test]
    async fn the_section_draws_the_plus_and_a_band_per_automation(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        let workspace = canvas.read_with(cx, |c, _| c.workspace.upgrade().unwrap());
        cx.simulate_resize(size(px(1600.), px(1400.)));
        redraw(cx);
        assert!(cx.debug_bounds(leak(format!("superzed-automation-band-{}", node.0))).is_some(), "fascia disegnata");
        let plus = cx.debug_bounds("superzed-automations-new").expect("+ nell'intestazione");
        cx.simulate_click(plus.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(
            workspace.read_with(cx, |ws, cx| ws.active_modal::<crate::automation_form::AutomationFormModal>(cx)).is_some(),
            "il + apre il form"
        );
        workspace.update_in(cx, |ws, window, cx| ws.hide_modal(window, cx));
        close_all(&canvas, cx);
    }

    // Fix round 1: tasti del form come nel keymap di default (blocchi `SuperzedAutomationForm`):
    // invio nel prompt va a capo, tab/shift-tab girano fra i campi, cmd-invio salva; l'errore
    // sparisce modificando i campi.
    #[gpui::test]
    async fn the_form_keys_move_between_fields_and_enter_in_the_prompt_is_a_newline(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| {
            menu::init();
            let prompt = Some("SuperzedAutomationForm > Editor && mode == auto_height");
            cx.bind_keys([
                KeyBinding::new("enter", menu::Confirm, None),
                KeyBinding::new("cmd-enter", menu::SecondaryConfirm, None),
                KeyBinding::new("tab", menu::SelectNext, None),
                KeyBinding::new("shift-tab", menu::SelectPrevious, None),
                KeyBinding::new("escape", menu::Cancel, None),
                KeyBinding::new("cmd-enter", menu::Confirm, Some("SuperzedAutomationForm > Editor")),
                KeyBinding::new("enter", editor::actions::Newline, prompt),
                KeyBinding::new("tab", menu::SelectNext, prompt),
                KeyBinding::new("shift-tab", menu::SelectPrevious, prompt),
            ]);
        });
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_automation(window, cx);
        });
        cx.run_until_parked();
        let modal = workspace
            .read_with(cx, |ws, cx| ws.active_modal::<crate::automation_form::AutomationFormModal>(cx))
            .expect("form aperto");
        // nome vuoto: errore, poi scrivendo sparisce
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert!(modal.read_with(cx, |m, _| m.error().is_some_and(|e| e.contains("name"))));
        cx.simulate_input("Log");
        cx.run_until_parked();
        assert_eq!(modal.read_with(cx, |m, _| m.error().map(str::to_string)), None, "errore tolto modificando");
        // tab: frequenza (gia' 4h), tab: prompt
        cx.simulate_keystrokes("tab tab");
        cx.simulate_input("riga uno");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("riga due");
        cx.run_until_parked();
        assert!(
            workspace.read_with(cx, |ws, cx| ws.active_modal::<crate::automation_form::AutomationFormModal>(cx)).is_some(),
            "invio nel prompt non salva"
        );
        // shift-tab due volte: di nuovo il nome
        cx.simulate_keystrokes("shift-tab shift-tab");
        cx.simulate_input(" prod");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |ws, cx| ws.active_modal::<crate::automation_form::AutomationFormModal>(cx)).is_none());
        let defs: Vec<_> = canvas.read_with(cx, |c, _| {
            c.automations.values().map(|r| (r.def.name.clone(), r.def.schedule.clone(), r.def.prompt.clone())).collect()
        });
        assert_eq!(defs, vec![("Log prod".to_string(), "4h".to_string(), "riga uno\nriga due".to_string())]);
        close_all(&canvas, cx);
    }

    // Fix round 1: la fascia mette in pausa; chiudere la chat lo dice; rinominarla rinomina l'automazione
    #[gpui::test]
    async fn band_pause_rename_and_close(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        cx.simulate_resize(size(px(1600.), px(1400.)));
        redraw(cx);
        let pause = cx.debug_bounds(leak(format!("superzed-automation-pause-{}", node.0))).expect("pulsante Pausa");
        cx.simulate_click(pause.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().paused), "Pausa dalla fascia");
        canvas.update(cx, |c, cx| c.set_node_title(node, "Log staging".into(), cx));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().name.clone()), "Log staging");
        assert_eq!(canvas.read_with(cx, |c, cx| c.automation_rows(cx)[0].name.clone()), "Log staging");
        canvas.update(cx, |c, cx| c.remove_nodes(&[node], cx));
        assert_eq!(
            canvas.read_with(cx, |c, _| c.last_warning().map(str::to_string)).as_deref(),
            Some("Automation “Log staging” removed")
        );
        cx.run_until_parked();
    }

    // Spec §3: trascinare fuori una chat di automazione la libera e toglie la definizione.
    #[gpui::test]
    async fn dragging_an_automation_chat_out_removes_the_automation(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        let f = frame_of(&canvas, node, cx);
        release_drag_at(&canvas, node, point(f.origin.x - 3000., f.origin.y - 3000.), cx);
        assert_eq!(frame_of(&canvas, node, cx).dock, None);
        assert!(canvas.read_with(cx, |c, _| c.automation(node).is_none()));
        assert_eq!(canvas.read_with(cx, |c, _| c.last_warning().map(str::to_string)).as_deref(), Some("Automation “Log produzione” removed"));
        assert!(canvas.read_with(cx, |c, cx| c.node_save_data(node, cx)).get("automation").is_none());
        close_all(&canvas, cx);
    }

    // Review Focus 4
    #[gpui::test]
    async fn closing_the_automations_section_frees_chats_and_drops_definitions(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        let docked = frame_of(&canvas, node, cx);
        let a = canvas.read_with(cx, |c, _| c.container_node(DockKind::Automations).unwrap());
        canvas.update(cx, |c, cx| c.remove_nodes(&[a], cx));
        let free = frame_of(&canvas, node, cx);
        assert_eq!(free.dock, None);
        assert_eq!((free.origin, free.size), (docked.origin, docked.size), "resta dov'era, alla sua dimensione");
        assert!(canvas.read_with(cx, |c, _| c.automation(node).is_none() && c.terminals.contains_key(&node)), "processo vivo");
        close_all(&canvas, cx);
    }

    pub(super) async fn kanban_with_claude_chats(
        n: usize,
        cx: &mut TestAppContext,
    ) -> (Entity<CanvasView>, NodeId, Vec<NodeId>, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let kanban = canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        for i in 0..n {
            let init = NodeSessionInit {
                key: format!("kc-{i}"),
                account: "a".into(),
                account_id: None,
                session_id: None,
                phase: superzed_sessions::Phase::New,
                times: Default::default(),
            };
            canvas.update_in(cx, |c, window, cx| {
                c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
            });
        }
        cx.run_until_parked();
        let mut chats: Vec<NodeId> = canvas.read_with(cx, |c, _| c.terminals.keys().copied().collect());
        chats.sort();
        (canvas, kanban, chats, cx)
    }

    fn starting_claude(key: &str, canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) -> NodeId {
        let init = NodeSessionInit {
            key: key.into(),
            account: "a".into(),
            account_id: None,
            session_id: None,
            phase: superzed_sessions::Phase::New,
            times: Default::default(),
        };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx);
            c.node_for_session_key(key).unwrap()
        })
    }

    #[gpui::test]
    async fn a_new_chat_is_drawn_with_a_loader_until_its_pty_starts(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let id = starting_claude("start-1", &canvas, cx);
        assert!(canvas.read_with(cx, |c, _| c.terminals.is_empty()), "il PTY non e' ancora partito");
        // disegno senza far girare l'executor: il PTY resta in partenza
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(cx.debug_bounds(leak(format!("superzed-node-starting-{}", id.0))).is_some());
        cx.run_until_parked();
        redraw(cx);
        canvas.read_with(cx, |c, _| {
            assert!(c.terminals.contains_key(&id), "il terminale si aggancia allo stesso nodo");
            assert!(c.starting_terminals.is_empty());
            assert_eq!(c.model.frames().len(), 1);
        });
        assert!(cx.debug_bounds(leak(format!("superzed-node-starting-{}", id.0))).is_none());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn closing_a_chat_while_its_pty_starts_drops_the_session(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let id = starting_claude("start-2", &canvas, cx);
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| {
            assert!(c.terminals.is_empty() && c.model.frames().is_empty() && c.starting_terminals.is_empty());
        });
        assert!(store(cx).read_with(cx, |s, _| s.get("start-2").is_none()));
    }

    /// x dell'origine della colonna `phase` del kanban `k` con chat `chat_size`.
    fn column_x(k: &NodeFrame, phase: superzed_sessions::Phase, chat_size: Size<f32>) -> f32 {
        kanban_layout::column_origin(k.origin, phase, chat_size).x
    }

    fn frame_of(canvas: &Entity<CanvasView>, id: NodeId, cx: &mut VisualTestContext) -> NodeFrame {
        canvas.read_with(cx, |c, _| c.model.frame(id).unwrap().clone())
    }

    pub(super) fn close_all(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) {
        let ids: Vec<_> = canvas.read_with(cx, |c, _| c.terminals.keys().copied().collect());
        canvas.update(cx, |c, cx| c.remove_nodes(&ids, cx));
        cx.run_until_parked();
    }

    // Sottoprogetto 3: account finti a1..an (config dir in `dir`), sistema senza token.
    fn with_accounts(cx: &mut VisualTestContext, dir: &Path, quotas: &[(f32, f32)]) {
        cx.update(|_, cx| {
            superzed_accounts::test_support::fixture_store(cx, dir, quotas);
        });
        cx.run_until_parked();
    }

    fn export_of(dir: &Path, id: &str) -> String {
        format!("export CLAUDE_CONFIG_DIR={}; ", superzed_sessions::launch::shell_quote(&dir.join(format!("cfg-{id}")).to_string_lossy()))
    }

    // Spec criterio 3: cmd-n apre la chat sull'account con piu' margine; il titolo lo mostra.
    #[gpui::test]
    async fn cmd_n_opens_the_chat_on_the_account_with_most_headroom(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 20.), (10., 30.)]);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            window.focus(&c.focus_handle, cx);
        });
        cx.dispatch_action(superzed_actions::NewClaudeNode);
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a2")), "{cmd}");
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_save_data(id, cx))["account"], "a2");
        let key = canvas.read_with(cx, |c, _| c.session_key(id).unwrap().to_string());
        let session = store(cx).read_with(cx, |s, _| s.get(&key).cloned().unwrap());
        assert_eq!((session.account.as_str(), session.account_id.as_str()), ("a2", "a2"), "etichetta nel titolo e id");
        redraw(cx);
        assert!(cx.debug_bounds(leak(format!("superzed-node-account-{}", id.0))).is_some());
        close_all(&canvas, cx);
    }

    // cmd-n in una cartella da cui un account e' escluso (`exclude_from`): va sull'altro.
    #[gpui::test]
    async fn cmd_n_skips_an_account_excluded_from_the_project_folder(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| {
            superzed_accounts::test_support::fixture_store_with(cx, dir.path(), &[(10., 30.), (90., 20.)], |registry| {
                registry.accounts[0].exclude_from = vec!["/repo".into()];
            });
        });
        cx.run_until_parked();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            window.focus(&c.focus_handle, cx);
        });
        cx.dispatch_action(superzed_actions::NewClaudeNode);
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a2")), "a1 ha piu' margine ma e' escluso da /repo: {cmd}");
        close_all(&canvas, cx);
    }

    // Spec criterio 4: al riavvio la chat riprende sul suo account (transcript nella sua config dir).
    #[gpui::test]
    async fn restored_chat_resumes_on_its_own_account(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 90.), (10., 10.)]);
        std::fs::create_dir_all(dir.path().join("cfg-a1/projects/-repo")).unwrap();
        std::fs::write(dir.path().join("cfg-a1/projects/-repo/abc.jsonl"), "{}").unwrap();
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{
                "id": 12, "kind": "claude", "origin": [10.0, 10.0], "size": [500.0, 300.0],
                "title": "claude", "data": {"session_id": "abc", "phase": "plan", "account": "a1"}
            }]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a1")), "non lo scheduler (a2 ha piu' margine): {cmd}");
        assert!(cmd.contains("--resume 'abc'"), "transcript cercato nella config dir di a1: {cmd}");
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_save_data(NodeId(12), cx))["account"], "a1");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn a_saved_account_no_longer_registered_falls_back_to_system(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(10., 10.)]);
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{
                "id": 12, "kind": "claude", "origin": [10.0, 10.0], "size": [500.0, 300.0],
                "title": "claude", "data": {"session_id": "x", "account": "sparito"}
            }]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(!cmd.contains("export CLAUDE_CONFIG_DIR"), "sistema: {cmd}");
        assert!(canvas.read_with(cx, |c, _| c.last_warning.clone()).unwrap().contains("sparito"));
        assert!(canvas.read_with(cx, |c, cx| c.node_save_data(NodeId(12), cx)).get("account").is_none());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn all_accounts_at_limit_use_the_system_with_a_toast(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(99.5, 10.), (100., 10.)]);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.open_terminal(NodeKind::Claude, None, &serde_json::Value::Null, window, cx)
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(!cmd.contains("export CLAUDE_CONFIG_DIR"), "{cmd}");
        assert!(canvas.read_with(cx, |c, _| c.last_warning.clone()).unwrap().contains("at their limit"));
        close_all(&canvas, cx);
    }

    // Risolvi (kanban con prompt) e automazioni nuove passano dallo scheduler; l'automazione
    // salva il suo account accanto alla definizione (e lo riusa a ogni esecuzione: stesso PTY).
    #[gpui::test]
    async fn resolve_and_new_automations_use_the_scheduler(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 90.), (10., 10.)]);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_claude_in_kanban_with_prompt(Some("Risolvi «x»".into()), window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a2")), "{cmd}");
        canvas.update_in(cx, |c, window, cx| {
            let draft = crate::automation::AutomationDraft { name: "Log".into(), schedule: "4h".into(), prompt: "Leggi i log".into(), account: None };
            c.create_automation(draft, window, cx);
        });
        cx.run_until_parked();
        let node = canvas.read_with(cx, |c, _| *c.automations.keys().next().unwrap());
        let data = canvas.read_with(cx, |c, cx| c.node_save_data(node, cx));
        assert_eq!(data["account"], "a2");
        assert_eq!(data["automation"]["name"], "Log");
        close_all(&canvas, cx);
    }

    // Senza AccountStore (tutti gli altri test) la chat e' sul sistema, come prima.
    #[gpui::test]
    async fn without_accounts_the_chat_is_on_the_system(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.open_terminal(NodeKind::Claude, None, &serde_json::Value::Null, window, cx)
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(!cmd.contains("export CLAUDE_CONFIG_DIR"));
        close_all(&canvas, cx);
    }

    // Picker: account scelto a mano, nel kanban se richiesto.
    #[gpui::test]
    async fn chosen_account_is_used_once(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 90.), (10., 10.)]);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_claude_with_account("a1".into(), true, window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a1")), "{cmd}");
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx).len()), 1, "nel kanban");
        assert_eq!(canvas.read_with(cx, |c, _| c.next_claude_account.clone()), None, "usato una volta");
        close_all(&canvas, cx);
    }

    // Spec criterio 5: cmd-shift-n mostra i punteggi, il migliore preselezionato; Invio apre la chat.
    #[gpui::test]
    async fn pick_action_opens_the_picker_and_enter_uses_the_selected_account(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 90.), (10., 10.)]);
        cx.update(|_, cx| {
            menu::init();
            cx.bind_keys([
                gpui::KeyBinding::new("up", menu::SelectPrevious, None),
                gpui::KeyBinding::new("down", menu::SelectNext, None),
                gpui::KeyBinding::new("enter", menu::Confirm, None),
                gpui::KeyBinding::new("escape", menu::Cancel, None),
            ]);
        });
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            window.focus(&c.focus_handle, cx);
        });
        cx.dispatch_action(superzed_actions::NewClaudeNodePick);
        cx.run_until_parked();
        let modal = workspace
            .read_with(cx, |ws, cx| ws.active_modal::<superzed_accounts::picker::AccountPickerModal>(cx))
            .expect("picker aperto");
        assert_eq!(modal.read_with(cx, |m, _| m.selected_id().to_string()), "a2", "il migliore preselezionato");
        assert!(cx.debug_bounds("superzed-account-row-a1").is_some());
        cx.simulate_keystrokes("up enter");
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a1")), "{cmd}");
        assert!(workspace.read_with(cx, |ws, cx| ws.active_modal::<superzed_accounts::picker::AccountPickerModal>(cx)).is_none());
        // esc chiude senza aprire nulla
        let before = canvas.read_with(cx, |c, _| c.terminals.len());
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        cx.dispatch_action(superzed_actions::NewClaudeNodePick);
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.terminals.len()), before);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn picking_with_the_focus_in_the_kanban_docks_the_chat(cx: &mut TestAppContext) {
        let (canvas, kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        let dir = tempfile::tempdir().unwrap();
        with_accounts(cx, dir.path(), &[(10., 10.)]);
        cx.update(|_, cx| {
            menu::init();
            cx.bind_keys([gpui::KeyBinding::new("enter", menu::Confirm, None)]);
        });
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.focus_node(kanban, window, cx);
        });
        cx.dispatch_action(superzed_actions::NewClaudeNodePick);
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx).len()), 1);
        close_all(&canvas, cx);
    }

    // cmd-shift-m: picker dei prompt (progetto prima), Invio apre la chat con il prompt
    // scelto, i segnaposto sostituiti e il suffisso di sicurezza; esc non apre nulla.
    #[gpui::test]
    async fn prompt_action_opens_the_picker_and_enter_starts_a_chat_with_that_prompt(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, canvas, fs, cx) = workspace_with_repo(
            cx,
            json!({
                ".superzed": { "prompts": { "locale.md": "---\nname: Locale\n---\nPrimo corpo in {project_dir}" } }
            }),
        )
        .await;
        fs.insert_tree("/data/prompts", json!({ "globale.md": "---\nname: Globale\n---\nSecondo corpo" })).await;
        cx.update(|_, cx| {
            cx.set_global(crate::prompt_picker::PromptLibraryDirs { global: PathBuf::from("/data/prompts") });
            menu::init();
            cx.bind_keys([
                gpui::KeyBinding::new("up", menu::SelectPrevious, None),
                gpui::KeyBinding::new("down", menu::SelectNext, None),
                gpui::KeyBinding::new("enter", menu::Confirm, None),
                gpui::KeyBinding::new("escape", menu::Cancel, None),
            ]);
        });
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            window.focus(&c.focus_handle, cx);
        });
        cx.dispatch_action(superzed_actions::NewClaudeNodeFromPrompt);
        cx.run_until_parked();
        let modal = workspace
            .read_with(cx, |ws, cx| ws.active_modal::<crate::prompt_picker::PromptPickerModal>(cx))
            .expect("picker aperto");
        assert_eq!(modal.read_with(cx, |m, _| m.selected_name().to_string()), "Locale", "prima il progetto");
        assert!(cx.debug_bounds("superzed-prompt-row-global-Globale").is_some());
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains("Secondo corpo"), "{cmd}");
        assert!(cmd.contains("Regole di questa sessione"), "{cmd}");
        assert!(workspace.read_with(cx, |ws, cx| ws.active_modal::<crate::prompt_picker::PromptPickerModal>(cx)).is_none());
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx).len()), 0, "focus fuori dal kanban: chat libera");

        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        cx.dispatch_action(superzed_actions::NewClaudeNodeFromPrompt);
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains("Primo corpo in /repo"), "{cmd}");

        let before = canvas.read_with(cx, |c, _| c.terminals.len());
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        cx.dispatch_action(superzed_actions::NewClaudeNodeFromPrompt);
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.terminals.len()), before);
        close_all(&canvas, cx);
    }

    // Spec §9: con il focus nel kanban la chat col prompt nasce agganciata.
    #[gpui::test]
    async fn a_prompt_chat_started_with_the_focus_in_the_kanban_is_docked(cx: &mut TestAppContext) {
        let (canvas, kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        let fs = canvas.read_with(cx, |c, cx| c.project_root(cx).map(|(fs, _)| fs)).expect("fs");
        let fake_fs = fs.as_fake();
        fake_fs.insert_tree("/data/prompts", json!({ "globale.md": "---\nname: Globale\n---\nCorpo" })).await;
        cx.update(|_, cx| {
            cx.set_global(crate::prompt_picker::PromptLibraryDirs { global: PathBuf::from("/data/prompts") });
            menu::init();
            cx.bind_keys([gpui::KeyBinding::new("enter", menu::Confirm, None)]);
        });
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.focus_node(kanban, window, cx);
        });
        cx.dispatch_action(superzed_actions::NewClaudeNodeFromPrompt);
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx).len()), 1);
        close_all(&canvas, cx);
    }

    fn configure_mail(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) {
        let view = canvas.read_with(cx, |c, _| c.mail_view()).unwrap();
        view.update_in(cx, |v, window, cx| {
            v.account_editor().update(cx, |e, cx| e.set_text("j.doe@example.com", window, cx));
            v.query_editor().update(cx, |e, cx| e.set_text("from:@acme.example", window, cx));
            v.apply_settings(cx);
        });
        cx.run_until_parked();
    }

    fn open_mail_prompt_for_test(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) {
        let placeholders =
            crate::prompt_picker::Placeholders { project_dir: Some("/repo".into()), notifications_dir: "/n".into(), mail_file: None };
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.open_mail_prompt("Leggi {mail_file}".into(), true, placeholders, false, window, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn the_mail_prompt_writes_the_inbox_and_opens_a_no_send_chat(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let runner = superzed_sources::test_support::gmail_runner();
        // il secondo thread della ricerca finta: senza, la sua lettura fallirebbe
        runner.on(
            "gmail thread get t-100",
            superzed_sources::test_support::ok(
                r#"{"thread":{"id":"t-100","messages":[{"id":"m-101","headers":{"subject":"Atlas: listini"},"body":"x"}]}}"#,
            ),
        );
        let (_store, _dir) = with_sources(cx, runner.clone(), json!({"version": 1, "connections": []}));
        let (_ws, canvas, fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        cx.run_until_parked();
        configure_mail(&canvas, cx);
        open_mail_prompt_for_test(&canvas, cx);
        let inbox = fs.load(Path::new("/repo/.superzed/mail/inbox.json")).await.unwrap();
        assert!(inbox.contains("\"thread_id\": \"t-200\""), "{inbox}");
        assert!(inbox.contains("\"truncated\": false"), "{inbox}");
        assert!(fs.load(Path::new("/repo/.superzed/mail/state.json")).await.unwrap().contains("last_check"));
        assert_eq!(fs.load(Path::new("/repo/.superzed/mail/.gitignore")).await.unwrap(), "*\n");
        let command = canvas.read_with(cx, |c, _| c.last_claude_command().map(str::to_string)).unwrap();
        assert!(command.starts_with("export GOG_GMAIL_NO_SEND="), "{command}");
        assert!(command.contains("/repo/.superzed/mail/inbox.json"), "{command}");
        // la ricerca del prompt (max 30), non quella del nodo (max 50)
        let search = runner.calls().into_iter().find(|c| c.line().contains("gmail search --max 30 --")).unwrap();
        assert!(search.args.last().unwrap().starts_with("(from:@acme.example) after:"));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn a_failed_search_does_not_move_the_last_check(cx: &mut TestAppContext) {
        let runner = superzed_sources::test_support::gmail_runner();
        // solo la ricerca del prompt (max 30) fallisce, quella del nodo (max 50) no
        runner.on("gmail search --max 30 --", superzed_sources::test_support::fail(8, "backend error"));
        let (_store, _dir) = with_sources(cx, runner.clone(), json!({"version": 1, "connections": []}));
        let (_ws, canvas, fs, cx) = crate::test_support::workspace_with_repo_opts(cx, json!({}), true).await;
        cx.run_until_parked();
        configure_mail(&canvas, cx);
        open_mail_prompt_for_test(&canvas, cx);
        assert!(!fs.is_file(Path::new("/repo/.superzed/mail/state.json")).await);
        assert!(canvas.read_with(cx, |c, _| c.last_claude_command().is_none()), "nessuna chat senza inbox");
        assert!(canvas.read_with(cx, |c, _| c.last_warning.clone()).is_some_and(|w| w.starts_with("Mail prompt:")));
    }

    #[gpui::test]
    async fn prompt_picker_without_prompts_shows_a_toast(cx: &mut TestAppContext) {
        let (workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        fs.insert_tree("/data/prompts", json!({})).await;
        cx.update(|_, cx| cx.set_global(crate::prompt_picker::PromptLibraryDirs { global: PathBuf::from("/data/prompts") }));
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        cx.dispatch_action(superzed_actions::NewClaudeNodeFromPrompt);
        cx.run_until_parked();
        workspace.read_with(cx, |ws, _| assert!(ws.has_notification(&NotificationId::named("superzed-no-prompts".into()))));
        assert!(workspace.read_with(cx, |ws, cx| ws.active_modal::<crate::prompt_picker::PromptPickerModal>(cx)).is_none());
    }

    // Review Task 7: un id del picker non piu' registrato ripiega sul sistema con avviso.
    #[gpui::test]
    async fn a_picked_account_no_longer_registered_falls_back_to_system_with_a_toast(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(10., 10.)]);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_claude_with_account("sparito".into(), false, window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(!cmd.contains("export CLAUDE_CONFIG_DIR"), "sistema, non lo scheduler: {cmd}");
        assert!(canvas.read_with(cx, |c, _| c.last_warning.clone()).unwrap().contains("sparito"));
        // il sistema scelto a mano non avvisa
        canvas.update_in(cx, |c, window, cx| {
            c.last_warning = None;
            c.new_claude_with_account(superzed_accounts::SYSTEM_ID.into(), false, window, cx);
        });
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.last_warning.clone()), None);
        close_all(&canvas, cx);
    }

    // Review Task 7: senza AccountStore il restore parte sul sistema in silenzio e l'account
    // salvato resta nel `data` (non si perde al prossimo salvataggio).
    #[gpui::test]
    async fn without_accounts_a_restore_keeps_the_saved_account(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{
                "id": 12, "kind": "claude", "origin": [10.0, 10.0], "size": [500.0, 300.0],
                "title": "claude", "data": {"session_id": "x", "account": "a1"}
            }]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(!cmd.contains("export CLAUDE_CONFIG_DIR"), "sistema: {cmd}");
        assert_eq!(canvas.read_with(cx, |c, _| c.last_warning.clone()), None, "nessun avviso");
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_save_data(NodeId(12), cx))["account"], "a1");
        close_all(&canvas, cx);
    }

    // Review Task 7: il titolo mostra l'etichetta dell'account, non il suo id.
    #[gpui::test]
    async fn the_title_shows_the_account_label_not_its_id(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| {
            superzed_accounts::test_support::fixture_store_labeled(cx, dir.path(), &["lavoro".into()], &[(10., 10.)]);
        });
        cx.run_until_parked();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            window.focus(&c.focus_handle, cx);
        });
        cx.dispatch_action(superzed_actions::NewClaudeNode);
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_save_data(id, cx))["account"], "a1", "salvato l'id");
        redraw(cx);
        assert!(cx.debug_bounds(leak(format!("superzed-node-account-{}", id.0))).is_some());
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_account_text(id, cx)).as_deref(), Some("lavoro"), "nel titolo l'etichetta");
        close_all(&canvas, cx);
    }

    // Review Task 7: un'automazione ripristinata riprende sull'account salvato accanto alla definizione.
    #[gpui::test]
    async fn a_restored_automation_resumes_on_its_saved_account(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        with_accounts(cx, dir.path(), &[(90., 90.), (10., 10.)]);
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [
                {"id": 1, "kind": "automations", "origin": [0.0, 0.0], "size": [100.0, 100.0], "title": "automazioni"},
                {"id": 2, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "Log", "dock": "automations",
                 "data": {"phase": "new", "account": "a1", "automation": {"name": "Log", "schedule": "4h", "prompt": "p"}}}
            ]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains(&export_of(dir.path(), "a1")), "non lo scheduler (a2 ha piu' margine): {cmd}");
        let data = canvas.read_with(cx, |c, cx| c.node_save_data(NodeId(2), cx));
        assert_eq!((data["account"].as_str(), data["automation"]["name"].as_str()), (Some("a1"), Some("Log")));
        assert_eq!(frame_of(&canvas, NodeId(2), cx).dock, Some(DockKind::Automations));
        close_all(&canvas, cx);
    }

    // Spec §5 Ingresso: la chiave del nodo da' progetto e sorgente; chiave sconosciuta → "esterna".
    #[gpui::test]
    async fn notify_from_a_chat_lands_in_the_store_with_project_and_source(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let init = NodeSessionInit { key: "nk-1".into(), account: "a".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
        canvas.update_in(cx, |c, window, cx| c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx));
        cx.run_until_parked();
        cx.update(|_, cx| {
            crate::registry::route_notify("nk-1", json!({"severity": "errore", "title": "boom", "details": "trace"}), cx);
            crate::registry::route_notify("sconosciuta", json!({"title": "x"}), cx);
        });
        cx.run_until_parked();
        let all = superzed_notifications::NotificationFilter { project: None, include_archived: true };
        let list = cx.update(|_, cx| superzed_notifications::NotificationStore::global(cx).unwrap().read(cx).list(&all));
        let boom = list.iter().find(|n| n.title == "boom").expect("boom");
        assert_eq!(boom.project_root.as_deref(), Some(Path::new("/repo")));
        assert_eq!((boom.source.as_str(), boom.severity), ("chat", superzed_notifications::Severity::Error));
        assert!(std::fs::read_to_string(boom.details_path.as_ref().unwrap()).unwrap().contains("trace"));
        let x = list.iter().find(|n| n.title == "x").expect("x");
        assert_eq!((x.project_root.clone(), x.source.as_str()), (None, "external"));
        close_all(&canvas, cx);
    }

    // Spec §5 Risolvi: la chat nasce in Nuove e il prompt e' nel comando di lancio.
    #[gpui::test]
    async fn chat_with_prompt_starts_in_new_with_the_prompt_in_the_launch_command(cx: &mut TestAppContext) {
        let (canvas, _kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_claude_in_kanban_with_prompt(Some("Risolvi «x» (errore): it's broken".into()), window, cx);
        });
        cx.run_until_parked();
        let docked = canvas.read_with(cx, |c, cx| c.docked_chats(cx));
        assert_eq!(docked.len(), 1);
        assert_eq!(docked[0].1, superzed_sessions::Phase::New);
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains("--session-id"), "{cmd}");
        assert!(cmd.contains("-- 'Risolvi «x» (errore): it'\\''s broken'"), "{cmd}");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn claude_pty_override_replaces_the_real_claude(cx: &mut TestAppContext) {
        let (canvas, _kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        cx.update(|_, cx| cx.set_global(crate::ClaudePtyOverride(zsh_spec())));
        canvas.update_in(cx, |c, window, cx| c.new_claude_in_kanban(window, cx));
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx).len()), 1, "partita col PTY finto");
        assert!(canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().contains("claude")));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn docked_chats_follow_their_phase_column(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::New, cx);
            c.dock(chats[1], superzed_sessions::Phase::New, cx);
        });
        let k = frame_of(&canvas, kanban, cx);
        let (a, b) = (frame_of(&canvas, chats[0], cx), frame_of(&canvas, chats[1], cx));
        assert!(a.in_kanban() && b.in_kanban());
        assert_eq!(a.size, kanban_layout::DEFAULT_CHAT_SIZE);
        let header = canvas.read_with(cx, |c, cx| c.kanban_metrics(cx).header);
        assert_eq!(a.origin, point(column_x(&k, superzed_sessions::Phase::New, kanban_layout::DEFAULT_CHAT_SIZE) + kanban_layout::GAP, k.origin.y + header + kanban_layout::GAP));
        assert!(b.origin.y > a.origin.y, "impilate");
        // evento di fase: la seconda chat passa a Piano e cambia colonna
        store(cx).update(cx, |s, cx| {
            s.apply(hook("kc-1", json!({"hook_event_name": "PreToolUse", "tool_name": "Skill",
                "tool_input": {"skill": "superpowers:writing-plans"}})), cx)
        });
        cx.run_until_parked();
        let b = frame_of(&canvas, chats[1], cx);
        assert_eq!(b.origin.x, column_x(&k, superzed_sessions::Phase::Plan, kanban_layout::DEFAULT_CHAT_SIZE) + kanban_layout::GAP);
        assert_eq!(b.origin.y, a.origin.y, "in cima alla colonna Piano");
        let z: Vec<_> = canvas.read_with(cx, |c, _| c.model.frames().iter().map(|f| f.id).collect());
        let kz = z.iter().position(|id| *id == kanban).unwrap();
        assert!(chats.iter().all(|c| z.iter().position(|id| id == c).unwrap() > kz), "kanban sotto le chat");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn only_docked_chats_follow_the_layout(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        let before = frame_of(&canvas, chats[0], cx);
        store(cx).update(cx, |s, cx| {
            s.apply(hook("kc-0", json!({"hook_event_name": "PreToolUse", "tool_name": "Skill",
                "tool_input": {"skill": "brainstorming"}})), cx)
        });
        cx.run_until_parked();
        let after = frame_of(&canvas, chats[0], cx);
        assert!(!after.in_kanban());
        assert_eq!((after.origin, after.size), (before.origin, before.size), "libera: non si muove");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn undock_keeps_position_and_restores_size(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::Spec, cx));
        let docked = frame_of(&canvas, chats[0], cx);
        canvas.update(cx, |c, cx| c.undock(chats[0], cx));
        let free = frame_of(&canvas, chats[0], cx);
        assert!(!free.in_kanban());
        assert_eq!(free.origin, docked.origin);
        assert_eq!(free.size, size(TERMINAL_SIZE.0, TERMINAL_SIZE.1));
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Spec, "la fase resta");
        close_all(&canvas, cx);
    }

    fn phase_of_node(canvas: &Entity<CanvasView>, node: NodeId, cx: &mut VisualTestContext) -> superzed_sessions::Phase {
        canvas.read_with(cx, |c, cx| c.session_cards(cx).into_iter().find(|card| card.node == node).unwrap().session.phase)
    }

    // Spec §2: SessionStart source "clear" porta in Nuove solo le chat agganciate al kanban.
    #[gpui::test]
    async fn clear_moves_a_docked_chat_back_to_new_but_not_a_free_one(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::Plan, cx);
            c.set_session_phase(chats[1], superzed_sessions::Phase::Plan, cx);
        });
        for key in ["kc-0", "kc-1"] {
            store(cx).update(cx, |s, cx| {
                s.apply(hook(key, json!({"hook_event_name": "SessionStart", "session_id": format!("{key}-new"), "source": "clear"})), cx)
            });
        }
        cx.run_until_parked();
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::New, "agganciata: Nuove");
        assert_eq!(phase_of_node(&canvas, chats[1], cx), superzed_sessions::Phase::Plan, "libera: invariata");
        let k = frame_of(&canvas, kanban, cx);
        assert_eq!(frame_of(&canvas, chats[0], cx).origin.x, column_x(&k, superzed_sessions::Phase::New, kanban_layout::DEFAULT_CHAT_SIZE) + kanban_layout::GAP, "colonna Nuove");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn clear_leaves_a_chat_filed_in_others_or_analysis_where_it_is(cx: &mut TestAppContext) {
        use superzed_sessions::Phase;
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(3, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], Phase::Others, cx);
            c.dock(chats[1], Phase::Analysis, cx);
            c.dock(chats[2], Phase::Plan, cx);
        });
        for key in ["kc-0", "kc-1", "kc-2"] {
            store(cx).update(cx, |s, cx| {
                s.apply(hook(key, json!({"hook_event_name": "SessionStart", "session_id": format!("{key}-new"), "source": "clear"})), cx)
            });
        }
        cx.run_until_parked();
        assert_eq!(phase_of_node(&canvas, chats[0], cx), Phase::Others, "Altri: resta");
        assert_eq!(phase_of_node(&canvas, chats[1], cx), Phase::Analysis, "Analisi: resta");
        assert_eq!(phase_of_node(&canvas, chats[2], cx), Phase::New, "Plan: torna a Nuove");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn closing_the_kanban_frees_its_chats(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::New, cx));
        let docked = frame_of(&canvas, chats[0], cx);
        canvas.update(cx, |c, cx| c.remove_nodes(&[kanban], cx));
        let free = frame_of(&canvas, chats[0], cx);
        assert!(!free.in_kanban());
        assert_eq!(free.origin, docked.origin);
        assert_eq!(free.size, docked.size, "resta com'era: le chat liberate non si sovrappongono");
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban)), kanban_layout::DEFAULT_CHAT_SIZE);
        assert!(canvas.read_with(cx, |c, _| c.terminals.contains_key(&chats[0])), "il processo resta");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn in_kanban_without_kanban_or_on_non_claude_is_free(cx: &mut TestAppContext) {
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{"id": 4, "kind": "note", "origin": [5.0, 5.0], "size": [300.0, 200.0],
                       "title": "nota", "in_kanban": true, "data": {"text": "x"}}]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        let note = frame_of(&canvas, NodeId(4), cx);
        assert!(!note.in_kanban(), "una nota non sta nel kanban");
        assert_eq!(note.origin, point(5., 5.));
        canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        assert!(!frame_of(&canvas, NodeId(4), cx).in_kanban());
    }

    #[gpui::test]
    async fn dragging_a_docked_chat_is_not_snapped_back_until_release(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::New, cx));
        let start = frame_of(&canvas, chats[0], cx).origin;
        canvas.update(cx, |c, cx| {
            c.model.select(chats[0], false);
            c.drag = Some(DragState::Move { last: point(px(0.), px(0.)), moved: true });
            c.model.move_selected(point(40., 30.));
            c.nodes_changed(cx);
            c.relayout_kanban(cx);
        });
        assert_eq!(frame_of(&canvas, chats[0], cx).origin, point(start.x + 40., start.y + 30.), "durante il drag resta dove l'utente la porta");
        close_all(&canvas, cx);
    }

    fn release_drag_at(canvas: &Entity<CanvasView>, node: NodeId, title_center: Point<f32>, cx: &mut VisualTestContext) {
        // porta il centro del titolo del nodo in `title_center` (coordinate del canvas) e rilascia
        canvas.update(cx, |c, cx| {
            let f = c.model.frame(node).unwrap().clone();
            let now = point(f.origin.x + f.size.width / 2., f.origin.y + 10.);
            c.model.select(node, false);
            c.drag = Some(DragState::Move { last: point(px(0.), px(0.)), moved: true });
            c.model.move_selected(point(title_center.x - now.x, title_center.y - now.y));
            c.finish_node_drag(cx);
        });
    }

    // Task 2 revisione UI: durante il drag la colonna evidenziata e' quella del rilascio
    #[gpui::test]
    async fn dragging_a_chat_highlights_the_column_it_would_dock_in(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        let k = frame_of(&canvas, kanban, cx);
        let column = kanban_layout::column_width(kanban_layout::DEFAULT_CHAT_SIZE);
        let drag_to = |canvas: &Entity<CanvasView>, target: Point<f32>, cx: &mut VisualTestContext| {
            canvas.update(cx, |c, cx| {
                let f = c.model.frame(chats[0]).unwrap().clone();
                let now = point(f.origin.x + f.size.width / 2., f.origin.y + 10.);
                c.model.select(chats[0], false);
                c.drag = Some(DragState::Move { last: point(px(0.), px(0.)), moved: true });
                c.model.move_selected(point(target.x - now.x, target.y - now.y));
                c.nodes_changed(cx);
            });
        };
        assert!(canvas.read_with(cx, |c, _| c.kanban_drop_columns()).is_empty(), "senza drag niente");
        drag_to(&canvas, point(column_x(&k, superzed_sessions::Phase::Spec, kanban_layout::DEFAULT_CHAT_SIZE) + 0.5 * column, k.origin.y + 200.), cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.kanban_drop_columns()), vec![superzed_sessions::Phase::Spec]);
        redraw(cx);
        assert!(cx.debug_bounds("superzed-kanban-drop-spec").is_some(), "colonna Spec evidenziata");
        assert!(cx.debug_bounds("superzed-kanban-drop-plan").is_none());
        drag_to(&canvas, point(k.origin.x - 500., k.origin.y - 500.), cx);
        assert!(canvas.read_with(cx, |c, _| c.kanban_drop_columns()).is_empty(), "fuori dal kanban niente");
        redraw(cx);
        assert!(cx.debug_bounds("superzed-kanban-drop-spec").is_none());
        // il rilascio aggancia dove era l'evidenziazione
        drag_to(&canvas, point(column_x(&k, superzed_sessions::Phase::Spec, kanban_layout::DEFAULT_CHAT_SIZE) + 0.5 * column, k.origin.y + 200.), cx);
        canvas.update(cx, |c, cx| c.finish_node_drag(cx));
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Spec);
        assert!(canvas.read_with(cx, |c, _| c.kanban_drop_columns()).is_empty(), "drag finito");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn empty_kanban_columns_show_a_hint_except_new(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::Spec, cx));
        redraw(cx);
        assert!(cx.debug_bounds("superzed-kanban-empty-new").is_none(), "Nuove ha il +");
        assert!(cx.debug_bounds("superzed-kanban-empty-spec").is_none(), "Spec ha una chat");
        assert!(cx.debug_bounds("superzed-kanban-empty-brainstorming").is_some());
        assert!(cx.debug_bounds("superzed-kanban-empty-done").is_some());
        close_all(&canvas, cx);
    }

    #[test]
    fn close_confirm_names_the_node() {
        assert_eq!(close_confirm_message(&[("nota".into(), false)]), "Close “nota”?");
        assert_eq!(
            close_confirm_message(&[("bug login".into(), true)]),
            "Close “bug login”? The running session will be ended."
        );
        assert_eq!(close_confirm_message(&[("a".into(), true), ("b".into(), false)]), "Close 2 nodes?");
    }

    #[test]
    fn split_close_keeps_dirty_nodes_apart() {
        let (clean, dirty) = super::commit_close::split_dirty(
            &[NodeId(1), NodeId(2), NodeId(3)],
            |id| if id == NodeId(2) { 3 } else { 0 },
        );
        assert_eq!(clean, vec![NodeId(1), NodeId(3)]);
        assert_eq!(dirty, vec![(NodeId(2), 3)]);
    }

    // Lo `Stop` di una chat col focus la porta in `Idle`, che prima del controllo git vale
    // come interruzione. Qui un notify dello store e' gia' in coda (file toccato nello
    // stesso ciclo di effetti), quindi l'observe gira prima dell'emit `Stopped`: la
    // chiusura con commit deve sopravvivere e finire col controllo git.
    #[gpui::test]
    async fn focused_stop_starts_the_commit_check_and_closes_when_clean(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({".git": {}})).await;
        let key = "k-commit";
        let init = NodeSessionInit { key: key.into(), account: "a".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        let git = FakeRunner::new();
        git.on("status", superzed_sources::test_support::ok(""));
        let file = crate::UncommittedFile { path: "/repo/a.rs".into(), repo_root: "/repo".into(), display: "a.rs".into() };
        canvas.update_in(cx, |c, window, cx| {
            c.git_runner = git.clone();
            c.insert_commit_close_for_test(id, vec![file], Some(window.window_handle()));
            c.focus_node(id, window, cx);
        });
        store(cx).update(cx, |s, cx| {
            s.set_focused(Some(key.to_string()), cx);
            s.apply(hook(key, json!({"hook_event_name": "UserPromptSubmit"})), cx);
        });
        assert_eq!(canvas.read_with(cx, |c, _| c.commit_close_progress(id)), Some((true, false)));
        store(cx).update(cx, |s, cx| {
            s.set_focused(Some(key.to_string()), cx);
            s.apply(hook(key, json!({"hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_input": {"file_path": "/repo/a.rs"}})), cx);
            // file toccato durante il turno del commit: anche lui nel controllo finale
            s.apply(hook(key, json!({"hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_input": {"file_path": "/repo/b.rs"}})), cx);
            s.apply(hook(key, json!({"hook_event_name": "Stop"})), cx);
        });
        assert_eq!(store(cx).read_with(cx, |s, _| s.get(key).unwrap().state.clone()), superzed_sessions::SessionState::Idle);
        assert_eq!(
            canvas.read_with(cx, |c, _| c.commit_close_progress(id)),
            Some((true, true)),
            "Idle dopo lo Stop col focus: controllo git, non interruzione"
        );
        cx.run_until_parked();
        assert_eq!(
            git.count("-C /repo status --porcelain --untracked-files=all -- :(literal)a.rs :(literal)b.rs"),
            1,
            "{:?}",
            git.calls()
        );
        assert!(canvas.read_with(cx, |c, _| c.model.frame(id).is_none()), "git pulito: nodo chiuso");
        assert!(!canvas.read_with(cx, |c, _| c.is_committing(id)));
        let focus = canvas.read_with(cx, |c, _| c.focus_handle.clone());
        assert!(cx.update(|window, _| focus.is_focused(window)), "il focus torna al canvas");
    }

    async fn commit_close_chat(key: &str, git_stdout: &str, cx: &mut TestAppContext) -> (Entity<CanvasView>, NodeId, Arc<FakeRunner>, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let init = NodeSessionInit { key: key.into(), account: "a".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        let git = FakeRunner::new();
        git.on("status", superzed_sources::test_support::ok(git_stdout));
        let file = crate::UncommittedFile { path: "/repo/a.rs".into(), repo_root: "/repo".into(), display: "a.rs".into() };
        canvas.update(cx, |c, _| {
            c.git_runner = git.clone();
            c.insert_commit_close_for_test(id, vec![file], None);
        });
        (canvas, id, git, cx)
    }

    // Lo `Stop` di un turno partito prima del prompt del commit non avvia il controllo git.
    #[gpui::test]
    async fn a_stop_before_the_commit_turn_starts_is_ignored(cx: &mut TestAppContext) {
        let key = "k-early";
        let (canvas, id, git, cx) = commit_close_chat(key, "", cx).await;
        store(cx).update(cx, |s, cx| s.apply(hook(key, json!({"hook_event_name": "Stop"})), cx));
        cx.run_until_parked();
        assert!(git.calls().is_empty(), "nessun git status");
        assert_eq!(canvas.read_with(cx, |c, _| c.commit_close_progress(id)), Some((false, false)));
        assert!(canvas.read_with(cx, |c, _| c.model.frame(id).is_some()));
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
    }

    // git trova ancora file sporchi dopo il turno del commit: la chat resta aperta.
    #[gpui::test]
    async fn a_dirty_check_after_the_commit_turn_keeps_the_chat(cx: &mut TestAppContext) {
        let key = "k-dirty";
        let (canvas, id, git, cx) = commit_close_chat(key, " M a.rs\n", cx).await;
        store(cx).update(cx, |s, cx| s.apply(hook(key, json!({"hook_event_name": "UserPromptSubmit"})), cx));
        store(cx).update(cx, |s, cx| s.apply(hook(key, json!({"hook_event_name": "Stop"})), cx));
        cx.run_until_parked();
        assert_eq!(git.count("status"), 1);
        assert!(canvas.read_with(cx, |c, _| c.model.frame(id).is_some()), "file sporchi: nodo aperto");
        assert!(!canvas.read_with(cx, |c, _| c.is_committing(id)));
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
    }

    // Esc durante il turno del commit: niente hook, `Idle` senza `Stop` annulla la chiusura.
    #[gpui::test]
    async fn interrupting_the_commit_turn_keeps_the_chat_open(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let key = "k-esc";
        let init = NodeSessionInit { key: key.into(), account: "a".into(), account_id: None, session_id: None, phase: superzed_sessions::Phase::New, times: Default::default() };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        canvas.update(cx, |c, _| c.insert_commit_close_for_test(id, Vec::new(), None));
        store(cx).update(cx, |s, cx| s.apply(hook(key, json!({"hook_event_name": "UserPromptSubmit"})), cx));
        store(cx).update(cx, |s, cx| s.user_interrupted(key, cx));
        cx.run_until_parked();
        assert!(!canvas.read_with(cx, |c, _| c.is_committing(id)), "interruzione: chiusura annullata");
        assert!(canvas.read_with(cx, |c, _| c.model.frame(id).is_some()));
        canvas.update(cx, |c, cx| c.remove_nodes(&[id], cx));
        cx.run_until_parked();
    }

    // I titoli di default salvati in italiano si mostrano in inglese; quelli scelti restano.
    #[test]
    fn legacy_default_titles_are_shown_in_english() {
        assert_eq!(legacy_title(NodeKind::Note, "nota"), "note");
        assert_eq!(legacy_title(NodeKind::Automations, "automazioni"), "automations");
        assert_eq!(legacy_title(NodeKind::Shell, "nota"), "nota");
        assert_eq!(legacy_title(NodeKind::Note, "spesa"), "spesa");
    }

    // il menu del nodo trova le scorciatoie di `CanvasFocused` anche senza il focus sul canvas
    #[gpui::test]
    fn node_menu_finds_canvas_focused_shortcuts(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        cx.update(|_, cx| {
            cx.bind_keys([KeyBinding::new("f2", superzed_actions::RenameNode, Some("CanvasFocused"))]);
        });
        redraw(cx);
        let keys = canvas.read_with(cx, |c, _| c.menu_keys.clone());
        let focus = canvas.read_with(cx, |c, _| c.focus_handle.clone());
        cx.update(|window, _| {
            assert!(!focus.is_focused(window), "canvas senza focus");
            let binding = window.highest_precedence_binding_for_action_in(&superzed_actions::RenameNode, &keys);
            assert!(binding.is_some(), "F2 trovato per il menu");
        });
    }

    // la selezione e' un anello sopra il nodo: il nodo non cambia dimensione
    #[gpui::test]
    fn selection_ring_does_not_resize_the_node(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        canvas.update(cx, |c, cx| {
            let content = Box::new(RecordingContent::new(cx));
            c.insert_restored_node(frame(1, 10., 10.), content, cx);
        });
        redraw(cx);
        let before = cx.debug_bounds("superzed-node-body-1");
        assert!(cx.debug_bounds("superzed-node-ring-1").is_none(), "non selezionato: niente anello");
        canvas.update(cx, |c, cx| {
            c.model.select(NodeId(1), false);
            cx.notify();
        });
        redraw(cx);
        assert!(cx.debug_bounds("superzed-node-ring-1").is_some(), "selezionato: anello");
        assert_eq!(cx.debug_bounds("superzed-node-body-1"), before, "il contenuto resta dov'era");
    }

    #[gpui::test]
    async fn dropping_a_chat_on_a_column_docks_it_there_and_outside_frees_it(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        let k = frame_of(&canvas, kanban, cx);
        let spec_column = point(column_x(&k, superzed_sessions::Phase::Spec, kanban_layout::DEFAULT_CHAT_SIZE) + 0.5 * kanban_layout::column_width(kanban_layout::DEFAULT_CHAT_SIZE), k.origin.y + 200.);
        release_drag_at(&canvas, chats[0], spec_column, cx);
        assert!(frame_of(&canvas, chats[0], cx).in_kanban());
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Spec);
        let outside = point(k.origin.x - 500., k.origin.y - 500.);
        release_drag_at(&canvas, chats[0], outside, cx);
        assert!(!frame_of(&canvas, chats[0], cx).in_kanban(), "fuori: libera");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn only_claude_nodes_dock_on_drop(cx: &mut TestAppContext) {
        let (canvas, kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        let note = canvas.update_in(cx, |c, window, cx| c.new_note(window, cx));
        let k = frame_of(&canvas, kanban, cx);
        release_drag_at(&canvas, note, point(k.origin.x + 50., k.origin.y + 100.), cx);
        assert!(!frame_of(&canvas, note, cx).in_kanban());
    }

    #[gpui::test]
    async fn plus_and_cmd_n_in_the_kanban_create_docked_chats(cx: &mut TestAppContext) {
        let (canvas, kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_claude_in_kanban(window, cx);
        });
        cx.run_until_parked();
        let docked = canvas.read_with(cx, |c, cx| c.docked_chats(cx));
        assert_eq!(docked.len(), 1);
        assert_eq!(docked[0].1, superzed_sessions::Phase::New);
        // cmd-n con il focus sul kanban → agganciata
        canvas.update_in(cx, |c, window, cx| c.focus_node(kanban, window, cx));
        cx.dispatch_action(superzed_actions::NewClaudeNode);
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx).len()), 2);
        // cmd-n con il focus sul canvas (fuori dal kanban) → libera
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        cx.dispatch_action(superzed_actions::NewClaudeNode);
        cx.run_until_parked();
        let (docked, claude) = canvas.read_with(cx, |c, cx| {
            (c.docked_chats(cx).len(), c.model.frames().iter().filter(|f| f.kind == NodeKind::Claude).count())
        });
        assert_eq!((docked, claude), (2, 3));
        close_all(&canvas, cx);
    }

    // il kanban e' piu' largo della finestra di test: lo si porta in alto a sinistra,
    // cosi' le prime colonne sono dentro la finestra
    fn show_kanban_top_left(canvas: &Entity<CanvasView>, kanban: NodeId, cx: &mut VisualTestContext) {
        let k = frame_of(canvas, kanban, cx);
        canvas.update(cx, |c, cx| {
            c.viewport.offset = point(20. - k.origin.x, 20. - k.origin.y);
            c.viewport_changed(cx);
        });
        redraw(cx);
    }

    // Final fix 1: il kanban e' grande e senza contenuto scorrevole: lo scroll sopra una
    // colonna vuota fa pan del canvas; sopra una chat agganciata resta al terminale.
    #[gpui::test]
    async fn scroll_over_the_kanban_pans_but_not_over_a_docked_chat(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::New, cx));
        show_kanban_top_left(&canvas, kanban, cx);
        let col = cx
            .debug_bounds(leak(format!("superzed-kanban-col-{}", superzed_sessions::Phase::Analysis.as_str())))
            .expect("colonna Analysis");
        let chat = cx.debug_bounds(leak(format!("superzed-node-title-{}", chats[0].0))).expect("titolo chat");
        let before = canvas.read_with(cx, |c, _| c.viewport().offset);
        let over_column = col.center();
        cx.simulate_event(scroll(f32::from(over_column.x), f32::from(over_column.y), 30., false));
        let after = canvas.read_with(cx, |c, _| c.viewport().offset);
        assert_eq!(after, point(before.x, before.y + 30.), "scroll sul kanban: pan");
        redraw(cx);
        // sotto il titolo della chat agganciata con il focus: il suo terminale (Task 10:
        // senza focus fa pan, vedi `scroll_route`)
        canvas.update_in(cx, |c, window, cx| c.focus_node(chats[0], window, cx));
        redraw(cx);
        cx.executor().advance_clock(Duration::from_millis(PAN_LATCH_MS + 1));
        let chat = cx.debug_bounds(leak(format!("superzed-node-title-{}", chats[0].0))).unwrap_or(chat);
        let over_chat = point(chat.center().x, chat.bottom() + px(100.));
        let after = canvas.read_with(cx, |c, _| c.viewport().offset);
        cx.simulate_event(scroll(f32::from(over_chat.x), f32::from(over_chat.y), 30., false));
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().offset), after, "scroll su una chat con il focus: niente pan");
        close_all(&canvas, cx);
    }

    // Final fix 1: spazio + trascinamento sul corpo del kanban fa pan come sullo sfondo.
    #[gpui::test]
    async fn space_drag_on_the_kanban_body_pans(cx: &mut TestAppContext) {
        let (canvas, kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        show_kanban_top_left(&canvas, kanban, cx);
        let col = cx
            .debug_bounds(leak(format!("superzed-kanban-col-{}", superzed_sessions::Phase::Analysis.as_str())))
            .expect("colonna Analysis");
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        let none = Modifiers::default();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        let before = canvas.read_with(cx, |c, _| c.viewport().offset);
        let p = col.center();
        cx.simulate_mouse_down(p, MouseButton::Left, none);
        cx.simulate_mouse_move(point(p.x + px(40.), p.y + px(20.)), MouseButton::Left, none);
        cx.simulate_mouse_up(point(p.x + px(40.), p.y + px(20.)), MouseButton::Left, none);
        cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("space").unwrap() });
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().offset), point(before.x + 40., before.y + 20.));
    }

    // Final fix 2: una chat nuova in fondo a Nuove, sotto l'area visibile, viene portata in vista.
    #[gpui::test]
    async fn new_docked_chat_is_revealed(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(3, cx).await;
        cx.simulate_resize(size(px(1400.), px(900.)));
        canvas.update(cx, |c, cx| {
            for id in &chats {
                c.dock(*id, superzed_sessions::Phase::New, cx);
            }
        });
        // la viewport mostra solo la parte alta del kanban
        let k = frame_of(&canvas, kanban, cx);
        canvas.update(cx, |c, cx| {
            c.viewport.offset = point(20. - k.origin.x, 20. - k.origin.y);
            c.viewport_changed(cx);
        });
        redraw(cx);
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_claude_in_kanban(window, cx);
        });
        cx.run_until_parked();
        redraw(cx);
        let new = canvas.read_with(cx, |c, _| {
            c.model.frames().iter().find(|f| f.kind == NodeKind::Claude && !chats.contains(&f.id)).unwrap().id
        });
        let f = frame_of(&canvas, new, cx);
        assert!(f.in_kanban());
        let (visible, rect) = canvas.read_with(cx, |c, _| {
            let b = c.canvas_bounds.get();
            let v = c.viewport();
            let p = v.to_screen(f.origin);
            (
                Bounds::new(point(px(0.), px(0.)), b.size),
                Bounds::new(point(px(p.x), px(p.y)), size(px(f.size.width * v.zoom), px(f.size.height * v.zoom))),
            )
        });
        assert!(visible.size.height > px(0.) && visible.size.height < px(1300.), "canvas {visible:?}");
        assert!(visible.intersects(&rect), "la chat nuova {rect:?} deve essere visibile in {visible:?}");
        // gia' visibile: reveal non sposta la viewport
        let offset = canvas.read_with(cx, |c, _| c.viewport().offset);
        canvas.update(cx, |c, cx| c.reveal(new, cx));
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().offset), offset);
        close_all(&canvas, cx);
    }

    // Final fix 3: un click senza movimento sul titolo non e' un rilascio: una chat
    // libera sopra una colonna non si aggancia.
    #[gpui::test]
    async fn click_without_motion_on_a_title_does_not_dock(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        let k = frame_of(&canvas, kanban, cx);
        canvas.update(cx, |c, cx| {
            // kanban in alto a sinistra della viewport, chat sulla colonna Analysis
            c.viewport.offset = point(20. - k.origin.x, 20. - k.origin.y);
            let f = c.model.frame(chats[0]).unwrap().clone();
            let target = point(column_x(&k, superzed_sessions::Phase::Analysis, kanban_layout::DEFAULT_CHAT_SIZE) + 0.5 * kanban_layout::column_width(kanban_layout::DEFAULT_CHAT_SIZE), k.origin.y + 150.);
            c.model.frame_mut(chats[0]).unwrap().origin = point(target.x - f.size.width / 2., target.y - 10.);
            c.nodes_changed(cx);
        });
        redraw(cx);
        assert!(!frame_of(&canvas, chats[0], cx).in_kanban());
        let title = cx.debug_bounds(leak(format!("superzed-node-title-{}", chats[0].0))).expect("titolo");
        let none = Modifiers::default();
        cx.simulate_mouse_down(title.center(), MouseButton::Left, none);
        assert!(canvas.read_with(cx, |c, _| matches!(c.drag, Some(DragState::Move { .. }))), "drag iniziato");
        cx.simulate_mouse_up(title.center(), MouseButton::Left, none);
        cx.run_until_parked();
        assert!(!frame_of(&canvas, chats[0], cx).in_kanban(), "click senza movimento: resta libera");
        close_all(&canvas, cx);
    }

    // Final fix 4: una chat salvata `in_kanban` in un file senza kanban torna libera al caricamento.
    #[gpui::test]
    async fn stale_in_kanban_is_cleared_when_the_file_has_no_kanban(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{"id": 2, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "claude",
                       "in_kanban": true, "data": {"phase": "plan"}}]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        assert!(!frame_of(&canvas, NodeId(2), cx).in_kanban(), "senza kanban la chat e' libera");
        // OpenKanban dopo non la aggancia
        canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        assert!(!frame_of(&canvas, NodeId(2), cx).in_kanban());
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn move_keys_on_a_focused_docked_chat(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::New, cx));
        canvas.update(cx, |c, cx| c.move_docked(chats[0], true, cx));
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Brainstorming);
        canvas.update(cx, |c, cx| c.move_docked(chats[0], false, cx));
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::New);
        // Others e Analysis stanno prima di New: a sinistra si arriva fino a Others
        canvas.update(cx, |c, cx| c.move_docked(chats[0], false, cx));
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Analysis);
        canvas.update(cx, |c, cx| c.move_docked(chats[0], false, cx));
        canvas.update(cx, |c, cx| c.move_docked(chats[0], false, cx)); // bordo: niente
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Others);
        canvas.update(cx, |c, cx| c.move_docked(chats[0], true, cx));
        canvas.update(cx, |c, cx| c.move_docked(chats[0], true, cx));
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::New);
        // via azione, con il focus nel terminale della chat agganciata
        canvas.update_in(cx, |c, window, cx| c.focus_node(chats[0], window, cx));
        cx.run_until_parked();
        cx.dispatch_action(superzed_actions::MoveCardNext);
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Brainstorming);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn move_keys_on_the_focused_kanban_move_the_selected_docked_chat(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::New, cx));
        canvas.update_in(cx, |c, window, cx| {
            c.focus_node(kanban, window, cx);
            c.model.select(chats[0], false);
        });
        cx.run_until_parked();
        cx.dispatch_action(superzed_actions::MoveCardNext);
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Brainstorming);
        cx.dispatch_action(superzed_actions::MoveCardPrev);
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::New);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn dropping_a_claude_chat_and_a_shell_together_docks_only_the_claude(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Shell, None, zsh_spec(), None, window, cx)
        });
        cx.run_until_parked();
        let shell = canvas.read_with(cx, |c, _| {
            c.model.frames().iter().find(|f| f.kind == NodeKind::Shell).unwrap().id
        });
        let k = frame_of(&canvas, kanban, cx);
        canvas.update(cx, |c, cx| {
            // entrambe con il centro del titolo nella colonna Spec
            for (i, id) in [chats[0], shell].into_iter().enumerate() {
                let f = c.model.frame(id).unwrap().clone();
                let target = point(column_x(&k, superzed_sessions::Phase::Spec, kanban_layout::DEFAULT_CHAT_SIZE) + 0.5 * kanban_layout::column_width(kanban_layout::DEFAULT_CHAT_SIZE), k.origin.y + 150. + i as f32 * 60.);
                c.model.frame_mut(id).unwrap().origin =
                    point(target.x - f.size.width / 2., target.y - 10.);
            }
            c.model.select(chats[0], false);
            c.model.select(shell, true);
            c.drag = Some(DragState::Move { last: point(px(0.), px(0.)), moved: true });
            c.finish_node_drag(cx);
        });
        assert!(frame_of(&canvas, chats[0], cx).in_kanban());
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Spec);
        assert!(!frame_of(&canvas, shell, cx).in_kanban(), "solo le chat claude");
        close_all(&canvas, cx);
    }

    // Ruling 3: trascinando il kanban le sue chat agganciate si muovono con lui, subito.
    #[gpui::test]
    async fn dragging_the_kanban_moves_its_docked_chats_during_the_drag(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::Plan, cx));
        let start = frame_of(&canvas, chats[0], cx).origin;
        canvas.update(cx, |c, cx| {
            c.model.select(kanban, false);
            c.drag = Some(DragState::Move { last: point(px(0.), px(0.)), moved: true });
            c.model.move_selected(point(-120., 75.));
            c.nodes_changed(cx);
        });
        assert!(canvas.read_with(cx, |c, _| c.drag.is_some()), "ancora nel drag");
        assert_eq!(frame_of(&canvas, chats[0], cx).origin, point(start.x - 120., start.y + 75.));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn in_kanban_is_saved_and_restored(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [
                {"id": 1, "kind": "kanban", "origin": [0.0, 0.0], "size": [100.0, 100.0], "title": "kanban"},
                {"id": 2, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "claude",
                 "in_kanban": true, "data": {"phase": "plan"}}
            ]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        let chat = frame_of(&canvas, NodeId(2), cx);
        assert!(chat.in_kanban());
        assert_eq!(chat.origin.x, 5. * kanban_layout::column_width(kanban_layout::DEFAULT_CHAT_SIZE) + kanban_layout::GAP, "colonna Piano");
        canvas.update(cx, |c, cx| c.cycle_color(&[NodeId(2)], cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let saved = parse_project_file(&fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap()).unwrap();
        assert!(saved.nodes.iter().find(|n| n.id == 2).unwrap().to_frame().in_kanban());
        close_all(&canvas, cx);
    }

    // il kanban ridotto: una colonna per fase con intestazione, affiancate come il layout
    #[gpui::test]
    async fn kanban_draws_one_column_per_phase_under_the_docked_chats(cx: &mut TestAppContext) {
        let (canvas, _kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::Spec, cx));
        redraw(cx);
        let cols: Vec<_> = superzed_sessions::Phase::ALL
            .iter()
            .map(|p| cx.debug_bounds(leak(format!("superzed-kanban-col-{}", p.as_str()))).expect("colonna"))
            .collect();
        assert!(cols.windows(2).all(|w| w[0].origin.x < w[1].origin.x), "affiancate in ordine di fase");
        let title = cx.debug_bounds(leak(format!("superzed-node-title-{}", chats[0].0))).expect("titolo");
        let spec = superzed_sessions::Phase::ALL.iter().position(|p| *p == superzed_sessions::Phase::Spec).unwrap();
        assert!(cols[spec].contains(&title.center()), "la chat sta sopra la sua colonna");
        close_all(&canvas, cx);
    }

    // Fix round 1 (Task 2 review, finding 1): l'intestazione della colonna sta nella
    // fascia fra la barra del titolo del kanban e la prima chat, anche con un rem grande.
    // Revisione finale: l'intestazione del kanban e' in rem come le Automazioni, quindi a
    // zoom 1 la fascia contiene il testo senza salire sulla barra del titolo del kanban.
    async fn assert_header_above_first_chat(ui_font_size: f32, cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |s| s.theme.ui_font_size = Some(ui_font_size.into()));
            });
        });
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::Spec, cx));
        redraw(cx);
        cx.update(|window, _| assert_eq!(window.rem_size(), px(ui_font_size), "rem dal font dell'interfaccia"));
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport.zoom), 1.0, "zoom 1");
        let header = cx
            .debug_bounds(leak(format!("superzed-kanban-header-{}", superzed_sessions::Phase::Spec.as_str())))
            .expect("intestazione");
        let chat_title = cx.debug_bounds(leak(format!("superzed-node-title-{}", chats[0].0))).expect("titolo chat");
        let kanban_title = cx.debug_bounds(leak(format!("superzed-node-title-{}", kanban.0))).expect("titolo kanban");
        assert!(header.bottom() <= chat_title.top(), "rem {ui_font_size}: {header:?} sotto la chat {chat_title:?}");
        assert!(header.top() >= kanban_title.bottom(), "rem {ui_font_size}: {header:?} non sale sulla barra del kanban {kanban_title:?}");
        let text = ui_font_size * crate::kanban_node::HEADER_TEXT_REMS;
        assert!(header.size.height >= px(text), "rem {ui_font_size}: {:?} contiene il testo ({text})", header.size.height);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn kanban_header_is_not_covered_by_the_first_chat(cx: &mut TestAppContext) {
        assert_header_above_first_chat(16., cx).await;
    }

    #[gpui::test]
    async fn kanban_header_fits_with_a_20px_ui_font(cx: &mut TestAppContext) {
        assert_header_above_first_chat(20., cx).await;
    }

    #[gpui::test]
    async fn kanban_header_is_not_covered_with_a_large_ui_font(cx: &mut TestAppContext) {
        assert_header_above_first_chat(24., cx).await;
    }

    // Revisione finale, finding 2: un font dell'interfaccia cambiato senza scriverlo nelle
    // impostazioni (cmd-+, `adjust_ui_font_size`) rifa' il layout di kanban e Automazioni
    // al disegno successivo.
    #[gpui::test]
    async fn containers_relayout_when_the_ui_font_changes_outside_the_settings(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        let kanban = canvas.update_in(cx, |c, window, cx| {
            let k = c.open_kanban(window, cx);
            let init = NodeSessionInit {
                key: "kc-font".into(),
                account: "a".into(),
                account_id: None,
                session_id: None,
                phase: superzed_sessions::Phase::New,
                times: Default::default(),
            };
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx);
            k
        });
        cx.run_until_parked();
        let chat = canvas.read_with(cx, |c, _| c.terminals.keys().copied().filter(|id| *id != node).max().expect("chat del kanban"));
        canvas.update(cx, |c, cx| c.dock(chat, superzed_sessions::Phase::New, cx));
        redraw(cx);
        let before = frame_of(&canvas, chat, cx).origin.y - frame_of(&canvas, kanban, cx).origin.y;
        cx.update(|_, cx| theme_settings::adjust_ui_font_size(cx, |_| px(24.)));
        redraw(cx);
        cx.update(|window, _| assert_eq!(window.rem_size(), px(24.), "rem dal font aggiustato"));
        let m = canvas.read_with(cx, |c, cx| c.automations_metrics(cx));
        assert_eq!(m, automations_layout::Metrics::for_rem(24.));
        let a = canvas.read_with(cx, |c, _| c.container_node(DockKind::Automations).unwrap());
        let af = frame_of(&canvas, a, cx);
        let l = automations_layout::layout(af.origin, &[node], kanban_layout::DEFAULT_CHAT_SIZE, m);
        assert_eq!(af.size, l.size, "la sezione segue il font");
        assert_eq!(frame_of(&canvas, node, cx).origin, l.chats[0].1, "la chat sta sotto la fascia piu' alta");
        let after = frame_of(&canvas, chat, cx).origin.y - frame_of(&canvas, kanban, cx).origin.y;
        assert!(after > before, "la chat del kanban scende sotto l'intestazione piu' alta: {before} -> {after}");
        close_all(&canvas, cx);
    }

    // Il kanban si ridimensiona dalla maniglia, le chat agganciate no; una chat con
    // `in_kanban` ma senza kanban e' libera e si ridimensiona dalla sua.
    #[gpui::test]
    async fn resize_handle_on_the_kanban_but_not_on_docked_chats(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::New, cx));
        show_kanban_top_left(&canvas, kanban, cx);
        let handle = |id: NodeId| leak(format!("superzed-node-resize-{}", id.0));
        assert!(cx.debug_bounds(handle(kanban)).is_some(), "kanban con la maniglia");
        assert!(cx.debug_bounds(handle(chats[0])).is_none(), "chat agganciata senza maniglia");
        let press = mouse_press_at(point(px(0.), px(0.)));
        canvas.update(cx, |c, cx| {
            c.on_resize_mouse_down(kanban, &press, cx);
            assert!(matches!(c.drag, Some(DragState::ResizeContainer { .. })), "resize del kanban");
            c.drag = None;
            c.on_resize_mouse_down(chats[0], &press, cx);
            assert!(c.drag.is_none(), "niente resize di una chat agganciata");
            // senza kanban la chat e' libera e si ridimensiona dalla sua maniglia
            c.remove_nodes(&[kanban], cx);
            c.on_resize_mouse_down(chats[0], &press, cx);
            assert!(matches!(c.drag, Some(DragState::ResizeNode { .. })), "resize della chat libera");
            c.drag = None;
        });
        redraw(cx);
        assert!(cx.debug_bounds(handle(chats[0])).is_some(), "chat libera con la maniglia");
        close_all(&canvas, cx);
    }

    // una nota si ridimensiona dalla sua maniglia: lo spostamento del mouse diviso lo
    // zoom, senza scendere sotto `NOTE_MIN_SIZE`
    #[gpui::test]
    fn notes_resize_from_their_grip(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        let note = canvas.update_in(cx, |c, window, cx| c.new_note(window, cx));
        redraw(cx);
        assert!(cx.debug_bounds(leak(format!("superzed-node-resize-{}", note.0))).is_some(), "nota con la maniglia");
        let zoom = canvas.read_with(cx, |c, _| c.viewport.zoom);
        canvas.update(cx, |c, cx| c.on_resize_mouse_down(note, &mouse_press_at(point(px(100.), px(100.))), cx));
        drag_to(&canvas, point(px(100. + 80. * zoom), px(100. + 40. * zoom)), cx);
        assert_eq!(frame_of(&canvas, note, cx).size, size(NOTE_SIZE.0 + 80., NOTE_SIZE.1 + 40.));
        drag_to(&canvas, point(px(-5000.), px(-5000.)), cx);
        assert_eq!(frame_of(&canvas, note, cx).size, size(NOTE_MIN_SIZE.0, NOTE_MIN_SIZE.1));
        canvas.update(cx, |c, cx| c.finish_node_drag(cx));
        assert!(canvas.read_with(cx, |c, _| c.drag.is_none()));
    }

    fn mouse_press_at(position: Point<Pixels>) -> MouseDownEvent {
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }
    }

    fn drag_to(canvas: &Entity<CanvasView>, position: Point<Pixels>, cx: &mut VisualTestContext) {
        let e = MouseMoveEvent { position, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() };
        canvas.update_in(cx, |c, window, cx| c.on_drag_move(&e, window, cx));
    }

    // Resize del kanban: la larghezza da' quella delle chat (una per fase), l'altezza
    // quella delle chat divisa per le righe della colonna piu' piena; il layout
    // ricalcola dal vivo il kanban esatto e le chat; al minimo non si scende.
    #[gpui::test]
    async fn resizing_the_kanban_resizes_and_relays_out_its_chats(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(3, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::New, cx);
            c.dock(chats[1], superzed_sessions::Phase::New, cx);
            c.dock(chats[2], superzed_sessions::Phase::Spec, cx);
            c.viewport.zoom = 1.0;
        });
        let start = frame_of(&canvas, kanban, cx);
        let press = mouse_press_at(point(px(0.), px(0.)));
        canvas.update(cx, |c, cx| c.on_resize_mouse_down(kanban, &press, cx));
        // +800 in larghezza: 100 per colonna; +200 in altezza: 100 per ognuna delle 2 righe
        drag_to(&canvas, point(px(800.), px(200.)), cx);
        let d = kanban_layout::DEFAULT_CHAT_SIZE;
        let want = size(d.width + 100., d.height + 100.);
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban)), want);
        let k = frame_of(&canvas, kanban, cx);
        assert_eq!(k.origin, start.origin);
        let chats_now = canvas.read_with(cx, |c, cx| c.docked_chats(cx));
        let km = canvas.read_with(cx, |c, cx| c.kanban_metrics(cx));
        assert_eq!(k.size, kanban_layout::layout(k.origin, &chats_now, want, km).size, "kanban esatto dal layout");
        assert_eq!(k.size, size(start.size.width + 800., start.size.height + 200.));
        let (a, b, s) = (frame_of(&canvas, chats[0], cx), frame_of(&canvas, chats[1], cx), frame_of(&canvas, chats[2], cx));
        assert_eq!((a.size, b.size, s.size), (want, want, want), "chat piu' grandi");
        let g = kanban_layout::GAP;
        assert_eq!(b.origin, point(column_x(&k, superzed_sessions::Phase::New, want) + g, k.origin.y + km.header + g + want.height + g), "sotto la prima");
        assert_eq!(s.origin.x, column_x(&k, superzed_sessions::Phase::Spec, want) + g, "colonna Spec riposizionata");
        // troppo piccolo: al minimo
        drag_to(&canvas, point(px(-100_000.), px(-100_000.)), cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban)), kanban_layout::MIN_CHAT_SIZE);
        assert_eq!(frame_of(&canvas, chats[0], cx).size, kanban_layout::MIN_CHAT_SIZE);
        let k = frame_of(&canvas, kanban, cx);
        assert_eq!(k.size, kanban_layout::layout(k.origin, &chats_now, kanban_layout::MIN_CHAT_SIZE, km).size);
        canvas.update(cx, |c, cx| c.finish_node_drag(cx));
        assert_eq!(frame_of(&canvas, chats[0], cx).size, kanban_layout::MIN_CHAT_SIZE, "resta dopo il rilascio");
        // il drop su una colonna usa le colonne ridotte
        let k = frame_of(&canvas, kanban, cx);
        let plan = point(column_x(&k, superzed_sessions::Phase::Plan, kanban_layout::MIN_CHAT_SIZE) + 0.5 * kanban_layout::column_width(kanban_layout::MIN_CHAT_SIZE), k.origin.y + 150.);
        release_drag_at(&canvas, chats[0], plan, cx);
        assert_eq!(phase_of_node(&canvas, chats[0], cx), superzed_sessions::Phase::Plan);
        close_all(&canvas, cx);
    }

    // doppio click sul titolo di una chat agganciata: la chat centrata a zoom 1
    #[gpui::test]
    async fn double_click_on_a_docked_chat_title_centers_it(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        cx.simulate_resize(size(px(1400.), px(1000.)));
        canvas.update(cx, |c, cx| c.dock(chats[0], superzed_sessions::Phase::New, cx));
        show_kanban_top_left(&canvas, kanban, cx);
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 0.5, cx));
        redraw(cx);
        let title = cx.debug_bounds(leak(format!("superzed-node-title-{}", chats[0].0))).unwrap().center();
        double_click(cx, title);
        let f = frame_of(&canvas, chats[0], cx);
        assert!(f.in_kanban(), "resta agganciata");
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().zoom), 1.0);
        assert_centered(&canvas, f.origin, f.size, cx);
        close_all(&canvas, cx);
    }

    // doppio click sull'intestazione di una colonna: la colonna (intestazione e chat)
    // in vista, a zoom 1 se ci sta, altrimenti allo zoom che la fa stare
    #[gpui::test]
    async fn double_click_on_a_column_header_fits_the_column(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        cx.simulate_resize(size(px(1400.), px(1000.)));
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::Analysis, cx);
            c.dock(chats[1], superzed_sessions::Phase::Analysis, cx);
        });
        show_kanban_top_left(&canvas, kanban, cx);
        let header = cx
            .debug_bounds(leak(format!("superzed-kanban-header-{}", superzed_sessions::Phase::Analysis.as_str())))
            .expect("intestazione Analysis");
        double_click(cx, header.center());
        let k = frame_of(&canvas, kanban, cx);
        let (o, sz) = kanban_layout::column_rect(k.origin, superzed_sessions::Phase::Analysis, kanban_layout::DEFAULT_CHAT_SIZE, 2, canvas.read_with(cx, |c, cx| c.kanban_metrics(cx)));
        assert_centered(&canvas, o, sz, cx);
        let (v, b) = canvas.read_with(cx, |c, _| (c.viewport(), c.canvas_bounds.get().size));
        let fit = ((f32::from(b.width) - 2. * viewport::FIT_MARGIN) / sz.width)
            .min((f32::from(b.height) - 2. * viewport::FIT_MARGIN) / sz.height)
            .min(1.0);
        assert!((v.zoom - fit).abs() < 1e-5, "zoom {} invece di {fit}", v.zoom);
        assert!(v.zoom < 1.0, "due chat da 560 non stanno a zoom 1 in {b:?}");
        close_all(&canvas, cx);
    }

    // un kanban appena aperto mostra subito la colonna New con il +, come prima di Others e Analysis
    #[gpui::test]
    async fn a_new_kanban_opens_with_its_new_column_in_view(cx: &mut TestAppContext) {
        let (canvas, _kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        redraw(cx);
        let window = canvas.read_with(cx, |c, _| c.canvas_bounds.get());
        let plus = cx.debug_bounds("superzed-kanban-new-chat").expect("pulsante +");
        assert!(window.contains(&plus.center()), "+ {plus:?} fuori da {window:?}");
        close_all(&canvas, cx);
    }

    // Review: un doppio click sul + di Nuove apre una chat sola e non zooma sulla colonna
    #[gpui::test]
    async fn double_click_on_the_plus_opens_one_chat_and_does_not_zoom(cx: &mut TestAppContext) {
        let (canvas, _kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        canvas.update(cx, |c, _| c.test_claude_pty = Some(zsh_spec()));
        redraw(cx);
        let before = canvas.read_with(cx, |c, _| c.viewport());
        let plus = cx.debug_bounds("superzed-kanban-new-chat").expect("pulsante +");
        double_click(cx, plus.center());
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx).len()), 1, "una chat sola");
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport()), before, "niente zoom sulla colonna");
        close_all(&canvas, cx);
    }

    // cmd-alt-k con il kanban gia' aperto: panoramica, tutto il kanban in vista
    #[gpui::test]
    async fn open_kanban_again_fits_the_whole_kanban(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(1, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::Done, cx);
            c.viewport.offset = point(-50_000., -50_000.);
            c.viewport_changed(cx);
        });
        canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        let k = frame_of(&canvas, kanban, cx);
        assert_centered(&canvas, k.origin, k.size, cx);
        let (v, b) = canvas.read_with(cx, |c, _| (c.viewport(), c.canvas_bounds.get().size));
        let (tl, br) = (v.to_screen(k.origin), v.to_screen(point(k.origin.x + k.size.width, k.origin.y + k.size.height)));
        if v.zoom > viewport::MIN_ZOOM {
            assert!(tl.x >= 0. && tl.y >= 0., "{tl:?}");
            assert!(br.x <= f32::from(b.width) && br.y <= f32::from(b.height), "{br:?} in {b:?}");
        }
        close_all(&canvas, cx);
    }

    // Review: le righe si fissano al mouse-down; un cambio di fase a meta' drag non
    // cambia la mappa dimensione del kanban → dimensione delle chat.
    #[gpui::test]
    async fn kanban_resize_keeps_the_rows_of_the_mouse_down(cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(2, cx).await;
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::New, cx);
            c.dock(chats[1], superzed_sessions::Phase::New, cx);
            c.viewport.zoom = 1.0;
            c.on_resize_mouse_down(kanban, &mouse_press_at(point(px(0.), px(0.))), cx);
            assert!(matches!(c.drag, Some(DragState::ResizeContainer { rows: 2, .. })));
            // a meta' drag la seconda chat passa a Piano: una riga sola
            c.dock(chats[1], superzed_sessions::Phase::Plan, cx);
        });
        drag_to(&canvas, point(px(0.), px(200.)), cx);
        let d = kanban_layout::DEFAULT_CHAT_SIZE;
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban)), size(d.width, d.height + 100.), "+200 diviso le 2 righe del mouse-down");
        canvas.update(cx, |c, cx| c.finish_node_drag(cx));
        close_all(&canvas, cx);
    }

    // Review: la maniglia del kanban si prende con eventi veri a ogni zoom, anche dove
    // la chat della colonna Fatto (la piu' piena) copre il margine del kanban.
    async fn assert_kanban_grip_reachable(zoom: f32, cx: &mut TestAppContext) {
        let (canvas, kanban, chats, cx) = kanban_with_claude_chats(3, cx).await;
        cx.simulate_resize(size(px(1400.), px(900.)));
        canvas.update(cx, |c, cx| {
            c.dock(chats[0], superzed_sessions::Phase::New, cx);
            c.dock(chats[1], superzed_sessions::Phase::Done, cx);
            c.dock(chats[2], superzed_sessions::Phase::Done, cx);
        });
        redraw(cx);
        // l'angolo in basso a destra del kanban a 100 px dall'angolo del canvas
        canvas.update(cx, |c, cx| {
            let k = c.model.frame(kanban).unwrap().clone();
            let b = c.canvas_bounds.get().size;
            c.viewport.zoom = zoom;
            c.viewport.offset = point(
                f32::from(b.width) - 100. - (k.origin.x + k.size.width) * zoom,
                f32::from(b.height) - 100. - (k.origin.y + k.size.height) * zoom,
            );
            c.apply_font_zoom_all(cx);
            c.viewport_changed(cx);
        });
        redraw(cx);
        let grip = cx.debug_bounds(leak(format!("superzed-node-resize-{}", kanban.0))).expect("maniglia del kanban");
        let last = cx.debug_bounds(leak(format!("superzed-node-title-{}", chats[2].0))).expect("ultima chat di Fatto");
        assert!(grip.size.width >= px(16.) && grip.size.height >= px(16.), "zoom {zoom}: maniglia {grip:?}");
        assert!(last.right() < grip.right(), "zoom {zoom}: la chat sta a sinistra dell'angolo");
        // al centro e verso l'interno (dove a zoom basso c'e' la chat)
        for at in [grip.center(), grip.origin + point(px(2.), px(2.))] {
            cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
            canvas.read_with(cx, |c, _| {
                assert!(
                    matches!(c.drag, Some(DragState::ResizeContainer { .. })),
                    "zoom {zoom}: mouse-down in {at:?} deve ridimensionare il kanban"
                )
            });
            cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
            redraw(cx);
        }
        // con un trascinamento vero le chat crescono
        let before = canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban));
        let from = grip.center();
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(from + point(px(60.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(from + point(px(60.), px(0.)), MouseButton::Left, Modifiers::default());
        redraw(cx);
        let after = canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban));
        assert!(after.width > before.width, "zoom {zoom}: {before:?} → {after:?}");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn kanban_grip_is_reachable_at_zoom_1(cx: &mut TestAppContext) {
        assert_kanban_grip_reachable(1.0, cx).await;
    }

    #[gpui::test]
    async fn kanban_grip_is_reachable_zoomed_out(cx: &mut TestAppContext) {
        assert_kanban_grip_reachable(0.3, cx).await;
    }

    // La dimensione delle chat e' del kanban: sta nel suo `data` (con le altre chiavi)
    // e torna con lui; mancante → default.
    #[gpui::test]
    async fn kanban_chat_size_is_saved_and_restored(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [
                {"id": 1, "kind": "kanban", "origin": [0.0, 0.0], "size": [100.0, 100.0], "title": "kanban",
                 "data": {"chat_size": [1000.0, 700.0], "other": "x"}},
                {"id": 2, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "claude",
                 "in_kanban": true, "data": {"phase": "new"}}
            ]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(frame_of(&canvas, NodeId(2), cx).size, size(1000., 700.), "chat dal chat_size salvato");
        assert_eq!(frame_of(&canvas, NodeId(1), cx).size.width, kanban_layout::COLUMNS as f32 * (1000. + 2. * kanban_layout::GAP));
        // un resize cambia chat_size e si salva, tenendo le altre chiavi
        canvas.update(cx, |c, cx| {
            c.viewport.zoom = 1.0;
            c.on_resize_mouse_down(NodeId(1), &mouse_press_at(point(px(0.), px(0.))), cx);
        });
        drag_to(&canvas, point(px(-800.), px(-100.)), cx);
        canvas.update(cx, |c, cx| c.finish_node_drag(cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE * 2);
        cx.run_until_parked();
        let saved = parse_project_file(&fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap()).unwrap();
        let k = saved.nodes.iter().find(|n| n.id == 1).unwrap();
        assert_eq!(k.data, json!({"chat_size": [900.0, 600.0], "other": "x"}));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn kanban_chat_size_below_min_is_clamped_and_a_reopened_kanban_starts_at_default(cx: &mut TestAppContext) {
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [{"id": 3, "kind": "kanban", "origin": [0.0, 0.0], "size": [960.0, 360.0], "title": "kanban",
                       "data": {"chat_size": [10.0, 10.0]}}]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.set_chat_size(DockKind::Kanban, size(1234., 999.));
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban)), kanban_layout::MIN_CHAT_SIZE, "sotto il minimo: al minimo");
        // chiuso e riaperto: un kanban nuovo parte dal default
        canvas.update(cx, |c, cx| c.remove_nodes(&[NodeId(3)], cx));
        let k = canvas.update_in(cx, |c, window, cx| c.open_kanban(window, cx));
        assert_eq!(canvas.read_with(cx, |c, _| c.chat_size(DockKind::Kanban)), kanban_layout::DEFAULT_CHAT_SIZE);
        assert_eq!(
            canvas.read_with(cx, |c, cx| c.node_save_data(k, cx)),
            json!({"chat_size": [800.0, 560.0]})
        );
    }

    async fn two_projects(
        cx: &mut TestAppContext,
    ) -> (Entity<workspace::MultiWorkspace>, Entity<Workspace>, Entity<Workspace>, &'static mut VisualTestContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree("/a", json!({ "f.txt": "" })).await;
        fs.insert_tree("/b", json!({ "f.txt": "" })).await;
        let pa = project::Project::test(fs.clone(), ["/a".as_ref()], cx).await;
        let pb = project::Project::test(fs.clone(), ["/b".as_ref()], cx).await;
        let window = cx.add_window(|window, cx| workspace::MultiWorkspace::test_new(pa, window, cx));
        let mw = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let a = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let b = mw.update_in(cx, |mw, window, cx| mw.test_add_workspace(pb, window, cx));
        cx.run_until_parked();
        (mw, a, b, cx)
    }

    fn buffer_font(cx: &mut VisualTestContext) -> Pixels {
        cx.update(|_, cx| theme_settings::ThemeSettings::get_global(cx).buffer_font_size(cx))
    }

    #[gpui::test]
    async fn editor_zoom_and_canvas_zoom_are_independent(cx: &mut TestAppContext) {
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let base = buffer_font(cx);
        cx.update(|_, cx| theme_settings::adjust_buffer_font_size(cx, |s| s + px(4.)));
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| assert_eq!(c.viewport().zoom, 1.0, "l'editor non zooma il canvas"));
        workspace.read_with(cx, |ws, _| assert_eq!(ws.superzed_state().editor_font_size, Some(base + px(4.))));
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 2.0, cx));
        assert_eq!(buffer_font(cx), base + px(4.), "il canvas non cambia il font dell'editor");
    }

    #[gpui::test]
    async fn editor_font_is_per_project(cx: &mut TestAppContext) {
        let (mw, a, b, cx) = two_projects(cx).await;
        let base = cx.update(|_, cx| theme_settings::ThemeSettings::get_global(cx).buffer_font_size_settings());
        // b e' attivo: il suo font diventa base+6
        cx.update(|_, cx| theme_settings::adjust_buffer_font_size(cx, |_| base + px(6.)));
        cx.run_until_parked();
        mw.update_in(cx, |mw, window, cx| mw.activate(a.clone(), None, window, cx));
        cx.run_until_parked();
        assert_eq!(buffer_font(cx), base, "a non ha un font suo: torna quello dei settings");
        mw.update_in(cx, |mw, window, cx| mw.activate(b.clone(), None, window, cx));
        cx.run_until_parked();
        assert_eq!(buffer_font(cx), base + px(6.));
        a.read_with(cx, |ws, _| assert_eq!(ws.superzed_state().editor_font_size, None));
    }

    fn project_json(nodes: serde_json::Value) -> String {
        json!({
            "version": PROJECT_FILE_VERSION,
            "viewport": { "offset": [10.0, 20.0], "zoom": 1.5 },
            "editor_width": 500.0,
            "tab": { "name": "app", "color": 3 },
            "nodes": nodes
        })
        .to_string()
    }

    async fn saved(fs: &fs::FakeFs) -> crate::persistence::ProjectFile {
        let text = fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap();
        parse_project_file(&text).unwrap()
    }

    #[gpui::test]
    async fn loads_nodes_viewport_and_project_state(cx: &mut TestAppContext) {
        let file = project_json(json!([
            { "id": 5, "kind": "note", "origin": [1.0, 2.0], "size": [300.0, 200.0],
              "title": "idee", "data": { "text": "ciao" } }
        ]));
        let (workspace, canvas, _fs, cx) =
            workspace_with_repo(cx, json!({ ".superzed": { "project.json": file } })).await;
        canvas.read_with(cx, |c, cx| {
            assert!(c.is_loaded());
            assert_eq!(c.viewport().zoom, 1.5);
            assert_eq!(c.model().frames()[0].title, "idee");
            assert_eq!(c.node_save_data(NodeId(5), cx), json!({ "text": "ciao" }));
        });
        workspace.read_with(cx, |ws, _| {
            let s = ws.superzed_state();
            assert_eq!(s.editor_width, px(500.));
            assert_eq!(s.tab_name.as_ref().map(|n| n.as_ref()), Some("app"));
            assert_eq!(s.tab_color, Some(3));
        });
    }

    #[gpui::test]
    async fn changes_are_saved_after_debounce(cx: &mut TestAppContext) {
        let (workspace, canvas, fs, cx) = workspace_with_repo(cx, json!({ "a.txt": "" })).await;
        canvas.update_in(cx, |c, window, cx| c.new_note(window, cx));
        workspace.update(cx, |ws, cx| ws.update_superzed_state(|s| s.editor_width = px(777.), cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        let file = saved(&fs).await;
        assert_eq!(file.nodes.len(), 1);
        assert_eq!(file.editor_width, Some(777.));
        // la nota modificata si salva senza altre azioni
        let id = canvas.read_with(cx, |c, _| c.model().frames()[0].id);
        canvas.update_in(cx, |c, window, cx| {
            c.note_editor(id).unwrap().update(cx, |e, cx| e.set_text("dopo", window, cx))
        });
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(saved(&fs).await.nodes[0].data, json!({ "text": "dopo" }));
    }

    // Review Focus 2
    #[gpui::test]
    async fn no_save_before_load_completes(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let file = project_json(json!([
            { "id": 5, "kind": "note", "origin": [0.0, 0.0], "size": [300.0, 200.0], "title": "keep" }
        ]));
        fs.insert_tree("/repo", json!({ ".superzed": { "project.json": file } })).await;
        let project = project::Project::test(fs.clone(), ["/repo".as_ref()], cx).await;
        let window = cx.add_window(|window, cx| workspace::MultiWorkspace::test_new(project, window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        // prima di qualunque run_until_parked il caricamento non e' ancora finito
        let canvas = workspace.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            assert!(!c.is_loaded());
            c.new_note(window, cx);
            assert!(!c.has_pending_save(), "nessun salvataggio prima del caricamento");
        });
        cx.run_until_parked();
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        let titles: Vec<String> = saved(&fs).await.nodes.into_iter().map(|n| n.title).collect();
        assert!(titles.contains(&"keep".to_string()), "il nodo salvato non va perso: {titles:?}");
    }

    #[gpui::test]
    async fn corrupt_file_starts_empty_with_warning_and_backup(cx: &mut TestAppContext) {
        let (_ws, canvas, fs, cx) =
            workspace_with_repo(cx, json!({ ".superzed": { "project.json": "{ rotto" } })).await;
        canvas.read_with(cx, |c, _| {
            assert!(c.is_loaded());
            assert!(c.model().frames().is_empty());
            assert!(c.last_warning().unwrap().contains("project.json.bak"));
        });
        assert!(fs.is_file(Path::new("/repo/.superzed/project.json.bak")).await);
        // dopo il recupero si puo' di nuovo salvare
        canvas.update_in(cx, |c, window, cx| c.new_note(window, cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(saved(&fs).await.nodes.len(), 1);
    }

    #[gpui::test]
    async fn project_without_worktree_does_not_persist(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = project::Project::test(fs, [], cx).await;
        let window = cx.add_window(|window, cx| workspace::MultiWorkspace::test_new(project, window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        cx.run_until_parked();
        let canvas = workspace.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.new_note(window, cx);
            assert!(!c.has_pending_save());
        });
    }

    // Task 11: interazione con i nodi

    fn add_probe(canvas: &Entity<CanvasView>, id: u64, x: f32, y: f32, cx: &mut VisualTestContext) -> RecordingContent {
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        let probe = RecordingContent {
            view: content.view.clone(),
            zoom: content.zoom.clone(),
            closed: content.closed.clone(),
        };
        canvas.update(cx, |c, cx| c.insert_restored_node(frame(id, x, y), Box::new(content), cx));
        redraw(cx);
        probe
    }

    fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>, mods: Modifiers) {
        cx.simulate_mouse_down(from, MouseButton::Left, mods);
        cx.simulate_mouse_move(to, MouseButton::Left, mods);
        cx.simulate_mouse_up(to, MouseButton::Left, mods);
        redraw(cx);
    }

    #[gpui::test]
    fn dragging_the_title_moves_the_node(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        let title = cx.debug_bounds("superzed-node-title-1").unwrap().center();
        drag(cx, title, title + point(px(50.), px(30.)), Modifiers::default());
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frame(NodeId(1)).unwrap().origin, point(150., 130.));
        });
        // dopo il mouse up il movimento non trascina piu'
        cx.simulate_mouse_move(title + point(px(200.), px(200.)), None, Modifiers::default());
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frame(NodeId(1)).unwrap().origin, point(150., 130.));
        });
    }

    #[gpui::test]
    fn drag_at_zoom_two_moves_half_in_canvas_units(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 2.0, cx));
        redraw(cx);
        let title = cx.debug_bounds("superzed-node-title-1").unwrap().center();
        drag(cx, title, title + point(px(40.), px(0.)), Modifiers::default());
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frame(NodeId(1)).unwrap().origin, point(120., 100.));
        });
    }

    #[gpui::test]
    fn shift_click_extends_selection_and_drag_moves_all_selected(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        add_probe(&canvas, 2, 400., 100., cx);
        let t1 = cx.debug_bounds("superzed-node-title-1").unwrap().center();
        let t2 = cx.debug_bounds("superzed-node-title-2").unwrap().center();
        let shift = Modifiers { shift: true, ..Default::default() };
        cx.simulate_mouse_down(t1, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(t1, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_down(t2, MouseButton::Left, shift);
        cx.simulate_mouse_up(t2, MouseButton::Left, shift);
        redraw(cx);
        canvas.read_with(cx, |c, _| assert_eq!(c.model().selection().len(), 2));
        drag(cx, t2, t2 + point(px(10.), px(10.)), Modifiers::default());
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frame(NodeId(1)).unwrap().origin, point(110., 110.));
            assert_eq!(c.model().frame(NodeId(2)).unwrap().origin, point(410., 110.));
        });
    }

    #[gpui::test]
    fn clicking_a_node_body_brings_it_to_front(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        add_probe(&canvas, 2, 150., 150., cx);
        // un punto del nodo 1 non coperto dal nodo 2
        cx.simulate_mouse_down(point(px(110.), px(200.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(point(px(110.), px(200.)), MouseButton::Left, Modifiers::default());
        redraw(cx);
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frames().last().unwrap().id, NodeId(1));
            assert!(c.model().is_selected(NodeId(1)));
        });
    }

    #[gpui::test]
    fn keyboard_select_move_deselect(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        add_probe(&canvas, 2, 400., 100., cx);
        canvas.update_in(cx, |c, window, cx| c.focus_handle.focus(window, cx));
        cx.dispatch_action(superzed_actions::SelectAllNodes);
        cx.dispatch_action(superzed_actions::MoveSelectedNodes { dx: -10., dy: 5. });
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frame(NodeId(1)).unwrap().origin, point(90., 105.));
            assert_eq!(c.model().frame(NodeId(2)).unwrap().origin, point(390., 105.));
        });
        cx.dispatch_action(superzed_actions::DeselectNodes);
        canvas.read_with(cx, |c, _| assert!(c.model().selection().is_empty()));
    }

    // cmd +/-/0 con il focus in un nodo zoomano tutto il canvas (attorno al centro
    // del canvas visibile), mai il solo nodo: binding come nel keymap vero
    // ("Canvas > Editor" / "Canvas > Terminal" / "Canvas").
    #[gpui::test]
    fn zoom_keys_in_a_node_zoom_the_whole_canvas(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        let a = add_probe(&canvas, 1, 100., 100., cx);
        let b = add_probe(&canvas, 2, 400., 100., cx);
        cx.update(|_, cx| {
            cx.bind_keys([
                KeyBinding::new("cmd-=", CanvasZoomIn, Some("Canvas")),
                KeyBinding::new("cmd--", CanvasZoomOut, Some("Canvas")),
                KeyBinding::new("cmd-0", CanvasZoomReset, Some("Canvas")),
            ])
        });
        cx.update(|window, cx| a.view.focus_handle(cx).focus(window, cx));
        let center = canvas.read_with(cx, |c, _| c.local_center());
        let at_center = canvas.read_with(cx, |c, _| c.viewport().to_canvas(center));
        cx.simulate_keystrokes("cmd-=");
        let v = canvas.read_with(cx, |c, _| c.viewport());
        assert!((v.zoom - ZOOM_STEP).abs() < 1e-6);
        assert!((a.zoom.get() - ZOOM_STEP).abs() < 1e-6 && (b.zoom.get() - ZOOM_STEP).abs() < 1e-6, "tutti i nodi");
        let still = v.to_canvas(center);
        assert!((still.x - at_center.x).abs() < 1e-3 && (still.y - at_center.y).abs() < 1e-3, "attorno al centro");
        cx.simulate_keystrokes("cmd-- cmd--");
        assert!((b.zoom.get() - 1. / ZOOM_STEP).abs() < 1e-5);
        cx.simulate_keystrokes("cmd-0");
        assert_eq!((a.zoom.get(), b.zoom.get()), (1.0, 1.0));
    }

    fn double_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
        let m = Modifiers::default();
        cx.simulate_mouse_down(at, MouseButton::Left, m);
        cx.simulate_mouse_up(at, MouseButton::Left, m);
        cx.simulate_event(MouseDownEvent { button: MouseButton::Left, position: at, modifiers: m, click_count: 2, first_mouse: false });
        cx.simulate_event(MouseUpEvent { button: MouseButton::Left, position: at, modifiers: m, click_count: 2 });
        redraw(cx);
    }

    // il centro del rettangolo (coordinate canvas) e' al centro del canvas visibile
    fn assert_centered(canvas: &Entity<CanvasView>, origin: Point<f32>, sz: Size<f32>, cx: &mut VisualTestContext) {
        let (v, center) = canvas.read_with(cx, |c, _| (c.viewport(), c.local_center()));
        let p = v.to_screen(point(origin.x + sz.width / 2., origin.y + sz.height / 2.));
        assert!((p.x - center.x).abs() < 0.5 && (p.y - center.y).abs() < 0.5, "{p:?} non al centro {center:?}");
    }

    #[gpui::test]
    fn double_click_on_a_title_centers_the_node_at_zoom_one(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        let probe = add_probe(&canvas, 1, 600., 500., cx);
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 0.5, cx));
        redraw(cx);
        let title = cx.debug_bounds("superzed-node-title-1").unwrap().center();
        double_click(cx, title);
        let f = canvas.read_with(cx, |c, _| c.model().frame(NodeId(1)).unwrap().clone());
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport().zoom), 1.0);
        assert_eq!(probe.zoom.get(), 1.0, "font allo zoom nuovo");
        assert_centered(&canvas, f.origin, f.size, cx);
        assert_eq!(f.title, "n1", "il doppio click non rinomina");
        cx.update(|window, cx| assert!(probe.view.focus_handle(cx).is_focused(window), "focus nel nodo"));
    }

    #[gpui::test]
    fn zoom_to_a_node_too_big_fits_it_with_margin(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 0., 0., cx);
        let big = size(3000., 1000.);
        let origin = canvas.update(cx, |c, cx| {
            c.model.frame_mut(NodeId(1)).unwrap().size = big;
            c.zoom_to_node(NodeId(1), cx);
            c.model.frame(NodeId(1)).unwrap().origin
        });
        let (v, b) = canvas.read_with(cx, |c, _| (c.viewport(), c.canvas_bounds.get().size));
        let want = (f32::from(b.width) - 2. * viewport::FIT_MARGIN) / 3000.;
        assert!((v.zoom - want).abs() < 1e-5, "zoom {} invece di {want}", v.zoom);
        assert_centered(&canvas, origin, big, cx);
    }

    // Final fix 6: un canvas mai disegnato (tab di un altro progetto) non ha ancora i
    // limiti; lo zoom richiesto aspetta il primo disegno invece di andare perso. Non si
    // appoggia al ridisegno che l'harness fa in flush_effects: i limiti si azzerano a mano.
    #[gpui::test]
    fn zoom_to_node_when_ready_waits_for_the_canvas_bounds(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 2000., 1500., cx);
        let before = canvas.read_with(cx, |c, _| c.viewport());
        canvas.update(cx, |c, cx| {
            c.canvas_bounds.set(Bounds::default());
            c.zoom_to_node_when_ready(NodeId(1), cx);
        });
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport()), before, "niente zoom senza limiti");
        assert_eq!(canvas.read_with(cx, |c, _| c.pending_zoom.get()), Some(NodeId(1)), "in attesa");
        // il frame: il prepaint registra i limiti, lo zoom rimandato si applica
        redraw(cx);
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let f = canvas.read_with(cx, |c, _| c.model().frame(NodeId(1)).unwrap().clone());
        assert_centered(&canvas, f.origin, f.size, cx);
        // gia' applicato: un altro frame non lo ripete
        canvas.update(cx, |c, cx| c.zoom_by(point(0., 0.), 0.5, cx));
        let moved = canvas.read_with(cx, |c, _| c.viewport());
        redraw(cx);
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport()), moved);
    }

    #[gpui::test]
    fn zoom_to_node_when_ready_is_dropped_with_the_node(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 2000., 1500., cx);
        let before = canvas.read_with(cx, |c, _| c.viewport());
        canvas.update(cx, |c, cx| {
            c.canvas_bounds.set(Bounds::default());
            c.zoom_to_node_when_ready(NodeId(1), cx);
            c.remove_nodes(&[NodeId(1)], cx);
        });
        assert!(canvas.read_with(cx, |c, _| c.pending_zoom.get()).is_none());
        redraw(cx);
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport()), before);
    }

    #[gpui::test]
    fn zoom_to_node_when_ready_applies_at_once_with_bounds(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 2000., 1500., cx);
        canvas.update(cx, |c, cx| c.zoom_to_node_when_ready(NodeId(1), cx));
        let f = canvas.read_with(cx, |c, _| c.model().frame(NodeId(1)).unwrap().clone());
        assert_centered(&canvas, f.origin, f.size, cx);
        assert!(canvas.read_with(cx, |c, _| c.pending_zoom.get()).is_none());
    }

    #[gpui::test]
    fn zoom_to_focused_node_key(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        let b = add_probe(&canvas, 2, 2000., 1500., cx);
        cx.update(|_, cx| {
            cx.bind_keys([KeyBinding::new("cmd-alt-f", superzed_actions::ZoomToFocusedNode, Some("Canvas"))])
        });
        cx.update(|window, cx| b.view.focus_handle(cx).focus(window, cx));
        cx.simulate_keystrokes("cmd-alt-f");
        let f = canvas.read_with(cx, |c, _| c.model().frame(NodeId(2)).unwrap().clone());
        assert_centered(&canvas, f.origin, f.size, cx);
        // senza focus in un nodo: quello selezionato
        canvas.update_in(cx, |c, window, cx| {
            window.focus(&c.focus_handle, cx);
            c.model.select(NodeId(1), false);
        });
        cx.simulate_keystrokes("cmd-alt-f");
        let f = canvas.read_with(cx, |c, _| c.model().frame(NodeId(1)).unwrap().clone());
        assert_centered(&canvas, f.origin, f.size, cx);
    }

    // un nodo libero che non e' una nota (qui una fonte) si ridimensiona trascinando la
    // maniglia nel suo angolo, senza scendere sotto il minimo del modello
    #[gpui::test]
    fn free_nodes_resize_from_their_grip(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        canvas.update(cx, |c, cx| {
            c.viewport.zoom = 1.0;
            c.insert_restored_node(NodeFrame { kind: NodeKind::Source, ..frame(1, 100., 100.) }, Box::new(content), cx)
        });
        redraw(cx);
        let grip = cx.debug_bounds("superzed-node-resize-1").expect("nodo libero con la maniglia").center();
        drag(cx, grip, grip + point(px(100.), px(50.)), Modifiers::default());
        canvas.read_with(cx, |c, _| assert_eq!(c.model().frame(NodeId(1)).unwrap().size, size(300., 200.)));
        redraw(cx);
        let grip = cx.debug_bounds("superzed-node-resize-1").unwrap().center();
        drag(cx, grip, grip - point(px(1000.), px(1000.)), Modifiers::default());
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frame(NodeId(1)).unwrap().size, size(MIN_NODE_WIDTH, MIN_NODE_HEIGHT));
            assert!(c.drag.is_none(), "rilasciato");
        });
    }

    #[gpui::test]
    fn cycle_color_wraps(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        canvas.update(cx, |c, cx| {
            for _ in 0..NODE_COLORS {
                c.cycle_color(&[NodeId(1)], cx);
            }
            assert_eq!(c.model().frame(NodeId(1)).unwrap().color, Some(NODE_COLORS - 1));
            c.cycle_color(&[NodeId(1)], cx);
            assert_eq!(c.model().frame(NodeId(1)).unwrap().color, None);
        });
    }

    #[gpui::test]
    fn toggle_golden_flips_the_flag(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        add_probe(&canvas, 1, 100., 100., cx);
        canvas.update(cx, |c, cx| {
            c.toggle_golden(&[NodeId(1)], cx);
            assert!(c.model().frame(NodeId(1)).unwrap().golden);
            c.toggle_golden(&[NodeId(1)], cx);
            assert!(!c.model().frame(NodeId(1)).unwrap().golden);
        });
    }

    #[gpui::test]
    async fn toggle_golden_action_marks_the_focused_node_and_shows_the_star(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        canvas.update(cx, |c, cx| c.insert_restored_node(frame(1, 50., 50.), Box::new(content), cx));
        redraw(cx);
        assert!(cx.debug_bounds("superzed-node-golden-1").is_none());
        canvas.update_in(cx, |c, window, cx| {
            c.model.select(NodeId(1), false);
            c.focus_handle.focus(window, cx);
        });
        cx.dispatch_action(superzed_actions::ToggleGolden);
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| assert!(c.model().frame(NodeId(1)).unwrap().golden));
        redraw(cx);
        assert!(cx.debug_bounds("superzed-node-golden-1").is_some(), "stella nella barra del titolo");
    }

    #[test]
    fn idle_close_message_lists_the_chats_and_what_stays() {
        let titles = ["Atlas".to_string(), "Docs".to_string()];
        assert_eq!(
            idle_close_message("acme", &titles, 2, 1),
            "Close 2 idle chats of acme? Keeps 2 golden and 1 active.\n• Atlas\n• Docs"
        );
        assert_eq!(idle_close_message("acme", &titles[..1], 0, 0), "Close 1 idle chat of acme?\n• Atlas");
        assert_eq!(idle_close_message("acme", &titles[..1], 0, 3), "Close 1 idle chat of acme? Keeps 3 active.\n• Atlas");
        assert_eq!(idle_close_message("acme", &[], 1, 0), "No idle chats to close in acme. Keeps 1 golden.");
        assert_eq!(idle_close_message("acme", &[], 0, 0), "No idle chats to close in acme.");
        let many: Vec<String> = (1..=12).map(|i| format!("c{i}")).collect();
        let message = idle_close_message("acme", &many, 0, 0);
        assert!(message.ends_with("• c8\n… and 4 more"), "{message}");
    }

    async fn three_chats_second_golden(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, Entity<CanvasView>, Arc<fs::FakeFs>, &'static mut VisualTestContext) {
        let (ws, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        for id in 1..=3 {
            let content = cx.update(|_, cx| RecordingContent::new(cx));
            canvas.update(cx, |c, cx| {
                c.insert_restored_node(NodeFrame { kind: NodeKind::Claude, ..frame(id, 50. * id as f32, 50.) }, Box::new(content), cx)
            });
        }
        canvas.update(cx, |c, cx| c.toggle_golden(&[NodeId(2)], cx));
        redraw(cx);
        (ws, canvas, fs, cx)
    }

    fn open_idle_close(canvas: &Entity<CanvasView>, cx: &mut VisualTestContext) -> Rc<Cell<bool>> {
        let closed_project = Rc::new(Cell::new(false));
        canvas.update_in(cx, {
            let closed_project = closed_project.clone();
            |c, window, cx| {
                c.close_idle_chats_with_confirm("acme".into(), Box::new(move |_, _| closed_project.set(true)), window, cx)
            }
        });
        cx.run_until_parked();
        closed_project
    }

    #[gpui::test]
    async fn closing_idle_chats_keeps_the_golden_one(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = three_chats_second_golden(cx).await;
        let closed_project = open_idle_close(&canvas, cx);
        let ok = cx.debug_bounds("superzed-confirm-ok").expect("dialog");
        cx.simulate_click(ok.center(), Modifiers::default());
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| {
            assert!(c.model().frame(NodeId(1)).is_none() && c.model().frame(NodeId(3)).is_none(), "le idle si chiudono");
            assert!(c.model().frame(NodeId(2)).is_some(), "la golden resta");
        });
        assert!(!closed_project.get(), "il progetto resta aperto");
    }

    #[gpui::test]
    async fn the_secondary_button_closes_the_project_instead(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = three_chats_second_golden(cx).await;
        let closed_project = open_idle_close(&canvas, cx);
        let button = cx.debug_bounds("superzed-confirm-secondary").expect("Close project");
        cx.simulate_click(button.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(closed_project.get());
        canvas.read_with(cx, |c, _| assert_eq!(c.model().frames().len(), 3, "nessuna chat chiusa"));
    }

    #[gpui::test]
    async fn close_node_asks_and_y_removes(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        let closed = content.closed.clone();
        canvas.update(cx, |c, cx| c.insert_restored_node(frame(1, 50., 50.), Box::new(content), cx));
        canvas.update_in(cx, |c, window, cx| {
            c.model.select(NodeId(1), false);
            c.focus_handle.focus(window, cx);
        });
        cx.dispatch_action(superzed_actions::CloseNode);
        cx.run_until_parked();
        cx.simulate_keystrokes("n");
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| assert!(c.model().frame(NodeId(1)).is_some(), "n annulla"));
        canvas.update_in(cx, |c, window, cx| c.focus_handle.focus(window, cx));
        cx.dispatch_action(superzed_actions::CloseNode);
        cx.run_until_parked();
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| assert!(c.model().frame(NodeId(1)).is_none()));
        assert!(closed.get(), "on_close chiamato");
    }

    #[gpui::test]
    async fn rename_node_via_prompt(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        canvas.update(cx, |c, cx| c.insert_restored_node(frame(1, 50., 50.), Box::new(content), cx));
        canvas.update_in(cx, |c, window, cx| {
            c.model.select(NodeId(1), false);
            c.focus_handle.focus(window, cx);
        });
        // superzed: test_support::init_test non carica il keymap; TextPromptModal si
        // affida a menu::Confirm/menu::Cancel, quindi servono i binding minimi (come Task 10).
        cx.update(|_, cx| {
            cx.bind_keys([
                KeyBinding::new("enter", menu::Confirm, None),
                KeyBinding::new("escape", menu::Cancel, None),
            ])
        });
        cx.dispatch_action(superzed_actions::RenameNode);
        cx.run_until_parked();
        cx.simulate_input("backend");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| assert_eq!(c.model().frame(NodeId(1)).unwrap().title, "backend"));
    }

    // il prompt di rinomina pre-riempie col titolo mostrato (inglese), non con quello
    // salvato in italiano prima dell'interfaccia in inglese (`legacy_title`)
    #[gpui::test]
    async fn rename_node_prefills_the_legacy_english_title(cx: &mut TestAppContext) {
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let content = cx.update(|_, cx| RecordingContent::new(cx));
        let mut f = frame(1, 50., 50.);
        f.title = "nota".to_string();
        canvas.update(cx, |c, cx| c.insert_restored_node(f, Box::new(content), cx));
        canvas.update_in(cx, |c, window, cx| {
            c.model.select(NodeId(1), false);
            c.focus_handle.focus(window, cx);
        });
        cx.update(|_, cx| {
            cx.bind_keys([
                KeyBinding::new("enter", menu::Confirm, None),
                KeyBinding::new("escape", menu::Cancel, None),
            ])
        });
        cx.dispatch_action(superzed_actions::RenameNode);
        cx.run_until_parked();
        // niente digitato: conferma il testo pre-riempito cosi' com'e'
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| {
            assert_eq!(
                c.model().frame(NodeId(1)).unwrap().title,
                "note",
                "pre-riempito col titolo mostrato («note»), non con quello salvato («nota»)"
            )
        });
    }

    // Task 12: nodi terminale e claude

    #[gpui::test]
    async fn new_terminal_node_follows_zoom_and_closes_on_process_exit(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.zoom_by(point(0., 0.), 2.0, cx);
            c.open_terminal(NodeKind::Shell, None, &serde_json::Value::Null, window, cx);
        });
        cx.run_until_parked();
        let (id, terminal, view) = canvas.read_with(cx, |c, _| {
            let frame = c.model().frames()[0].clone();
            assert_eq!(frame.kind, NodeKind::Shell);
            assert_eq!(frame.title, "shell");
            let node = c.terminal_content(frame.id).expect("contenuto terminale");
            (frame.id, node.terminal.clone(), node.view)
        });
        view.read_with(cx, |v, _| assert_eq!(v.superzed_font_zoom(), Some(2.0)));
        // il processo finisce: il terminale emette CloseTerminal e il nodo sparisce
        terminal.update(cx, |_, cx| cx.emit(terminal::Event::CloseTerminal));
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| assert!(c.model().frame(id).is_none()));
    }

    #[gpui::test]
    async fn terminal_nodes_are_restored_with_their_frame(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let file = project_json(json!([
            { "id": 9, "kind": "claude", "origin": [30.0, 40.0], "size": [500.0, 300.0],
              "title": "revisione", "font_scale": 1.2 }
        ]));
        let (_ws, canvas, _fs, cx) =
            workspace_with_repo(cx, json!({ ".superzed": { "project.json": file } })).await;
        // il frame c'e' subito (anche prima che il PTY sia pronto): un salvataggio non lo perde
        canvas.read_with(cx, |c, cx| {
            let f = c.model().frame(NodeId(9)).expect("frame ripristinato");
            assert_eq!(f.title.as_str(), "revisione");
            assert_eq!(c.snapshot(cx).nodes.len(), 1);
        });
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| assert!(c.terminal_content(NodeId(9)).is_some()));
        // un font_scale salvato si ignora: il font e' allo zoom del canvas
        let view = canvas.read_with(cx, |c, _| c.terminal_content(NodeId(9)).unwrap().view);
        view.read_with(cx, |v, _| assert_eq!(v.superzed_font_zoom(), Some(1.5), "zoom del canvas 1.5, non 1.5 * 1.2"));
    }

    // Review fix 1: chiudere un nodo ripristinato mentre il PTY sta ancora partendo (create_terminal_task
    // non e' ancora stato eseguito) non deve lasciare un terminale orfano (mai disegnato, mai
    // chiuso) quando il task finisce dopo che il frame e' gia' sparito dal modello.
    #[gpui::test]
    async fn removing_a_restored_node_before_the_pty_is_ready_does_not_leak_it(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let restored = NodeFrame {
            id: NodeId(9),
            kind: NodeKind::Shell,
            origin: point(30., 40.),
            size: size(500., 300.),
            title: "vecchio".to_string(),
            color: None,
            group: None,
            dock: None,
            golden: false,
        };
        canvas.update_in(cx, |c, window, cx| {
            // open_terminal inserisce subito il frame e programma il task che crea il PTY,
            // ma non lo esegue ancora (nessun run_until_parked qui in mezzo)
            c.open_terminal(NodeKind::Shell, Some(restored), &serde_json::Value::Null, window, cx);
            // il nodo si chiude prima che il PTY sia pronto: `contents` non ha ancora nulla
            // per questo id, quindi remove_nodes non chiama on_close ne' rimuove niente li'
            c.remove_nodes(&[NodeId(9)], cx);
        });
        cx.run_until_parked();
        canvas.read_with(cx, |c, _| {
            assert!(c.model().frame(NodeId(9)).is_none());
            assert!(c.terminal_content(NodeId(9)).is_none(), "nessun terminale orfano");
        });
    }

    #[gpui::test]
    fn canvas_focused_context_only_when_canvas_itself_has_focus(cx: &mut TestAppContext) {
        let (canvas, cx) = canvas_window(cx);
        cx.update(|_, cx| {
            cx.bind_keys([gpui::KeyBinding::new(
                "backspace",
                superzed_actions::SelectAllNodes,
                Some("CanvasFocused"),
            )])
        });
        let probe = add_probe(&canvas, 1, 100., 100., cx);
        canvas.update(cx, |c, cx| {
            c.model.clear_selection();
            cx.notify();
        });
        // focus dentro il nodo: backspace non e' del canvas
        cx.update(|window, cx| probe.view.focus_handle(cx).focus(window, cx));
        redraw(cx);
        cx.simulate_keystrokes("backspace");
        canvas.read_with(cx, |c, _| assert!(c.model().selection().is_empty()));
        // focus sul canvas: backspace arriva al binding CanvasFocused
        canvas.update_in(cx, |c, window, cx| c.focus_handle.focus(window, cx));
        redraw(cx);
        cx.simulate_keystrokes("backspace");
        canvas.read_with(cx, |c, _| assert_eq!(c.model().selection().len(), 1));
    }

    // Final review 1: progetto vuoto, il focus dopo il cambio di tab non resta nel
    // pane invisibile ma va al canvas del progetto attivato
    #[gpui::test]
    async fn activating_an_empty_project_focuses_its_canvas(cx: &mut TestAppContext) {
        let (mw, a, b, cx) = two_projects(cx).await;
        let focus_of = |ws: &Entity<Workspace>, cx: &mut VisualTestContext| {
            let canvas = ws.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
            canvas.read_with(cx, |c, cx| c.focus_handle(cx))
        };
        let (fa, fb) = (focus_of(&a, cx), focus_of(&b, cx));
        mw.update_in(cx, |mw, window, cx| mw.activate(a.clone(), None, window, cx));
        cx.run_until_parked();
        cx.update(|window, _| assert!(fa.is_focused(window), "focus al canvas di a"));
        // da tastiera: NextProjectTab (azione registrata sul workspace, serve un repaint)
        redraw(cx);
        cx.dispatch_action(superzed_actions::NextProjectTab);
        cx.run_until_parked();
        mw.read_with(cx, |mw, _| assert_eq!(mw.workspace(), &b));
        cx.update(|window, _| assert!(fb.is_focused(window), "focus al canvas di b"));
    }

    fn group_alive(pgid: i32) -> bool {
        // SAFETY: segnale 0 = solo controllo di esistenza
        unsafe { libc::killpg(pgid, 0) == 0 }
    }

    /// Canvas agganciato al workspace ma non tenuto da lui: l'unico riferimento forte
    /// e' quello restituito, cosi' il test puo' rilasciarlo come fa la chiusura della tab.
    fn detached_canvas(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Entity<CanvasView> {
        let weak = workspace.downgrade();
        let canvas = cx.update(|window, cx| cx.new(|cx| CanvasView::new(weak, window, cx)));
        cx.run_until_parked();
        canvas
    }

    // Final review 2: chiudere la tab (canvas rilasciato) termina anche il job in primo
    // piano della shell, che ha un process group suo. Il job ignora SIGHUP, come un
    // processo che non muore col solo hangup: senza il SIGKILL del canvas resterebbe vivo.
    #[gpui::test]
    async fn releasing_the_canvas_kills_the_foreground_job(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, _canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let canvas = detached_canvas(&workspace, cx);
        // shell interattiva (job control) senza i file di configurazione dell'utente:
        // il test non dipende da cosa fa il suo zshrc all'avvio
        let spec = task::SpawnInTerminal {
            label: "shell".to_string(),
            command: Some("/bin/zsh".to_string()),
            args: vec!["-f".to_string(), "-i".to_string()],
            use_new_terminal: true,
            allow_concurrent_runs: true,
            hide: task::HideStrategy::Always,
            ..Default::default()
        };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Shell, None, spec, None, window, cx)
        });
        cx.run_until_parked();
        let terminal = canvas.read_with(cx, |c, _| c.terminals.values().next().expect("terminale").1.clone());
        // il leader del job (sh) e il figlio sleep sono nello stesso gruppo; upstream
        // uccide solo la shell, e l'hangup del kernel al gruppo in primo piano e' ignorato
        terminal.update(cx, |t, _| t.input(b"sh -c 'trap \"\" HUP; sleep 61 & wait'\r".to_vec()));
        #[allow(clippy::disallowed_methods, reason="test sincrono su un processo reale: non gira sull'executor di gpui")]
        let is_our_job = |pgid: i32| {
            std::process::Command::new("ps")
                .args(["-o", "command=", "-p", &pgid.to_string()])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("sleep 61"))
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let job = loop {
            let groups = terminal.read_with(cx, |t, _| crate::process::terminal_process_groups(t));
            // durante l'avvio della shell il gruppo in primo piano puo' essere un suo
            // processo di passaggio: si aspetta proprio il job lanciato qui
            if let [_shell, job] = groups[..]
                && is_our_job(job)
            {
                break job;
            }
            assert!(std::time::Instant::now() < deadline, "il job non e' partito: {groups:?}");
            std::thread::sleep(Duration::from_millis(50));
            cx.run_until_parked();
        };
        assert!(group_alive(job));
        drop(terminal);
        drop(canvas);
        // il rilascio avviene al prossimo flush degli effetti, alla fine di un update
        cx.update(|_, _| {});
        cx.run_until_parked();
        cx.executor().advance_clock(crate::process::KILL_GRACE);
        cx.run_until_parked();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while group_alive(job) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let alive = group_alive(job);
        if alive {
            crate::process::signal_groups(&[job], libc::SIGKILL);
        }
        assert!(!alive, "il job in primo piano e' sopravvissuto alla chiusura della tab");
    }

    // Final review 3: una modifica ancora in debounce si scrive subito quando il canvas
    // viene rilasciato (tab chiusa entro 500 ms), senza aspettare il timer
    #[gpui::test]
    async fn releasing_the_canvas_writes_the_pending_save(cx: &mut TestAppContext) {
        let (workspace, _canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        let canvas = detached_canvas(&workspace, cx);
        canvas.update_in(cx, |c, window, cx| {
            assert!(c.is_loaded());
            c.new_note(window, cx);
            assert!(c.has_pending_save());
        });
        drop(canvas);
        // il rilascio avviene al prossimo flush degli effetti; nessun advance_clock:
        // il debounce non e' scaduto
        cx.update(|_, _| {});
        cx.run_until_parked();
        assert_eq!(saved(&fs).await.nodes.len(), 1);
    }

    // Fix round 1 (Task 6 review, finding 1): chiudere la tab non deve azzerare il
    // session_id gia' salvato di un nodo claude: lo snapshot del salvataggio in debounce
    // va preso prima di togliere le sessioni dallo store (altrimenti node_save_data
    // troverebbe la chiave gia' sparita e scriverebbe data: null per ogni nodo claude).
    #[gpui::test]
    async fn releasing_the_canvas_keeps_a_pending_claude_session_id(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, _canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        let canvas = detached_canvas(&workspace, cx);
        let init = NodeSessionInit {
            key: "k-5".into(),
            account: "a".into(),
            account_id: None,
            session_id: Some("abc".into()),
            phase: superzed_sessions::Phase::New,
            times: Default::default(),
        };
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, zsh_spec(), Some(init), window, cx)
        });
        cx.run_until_parked();
        let id = canvas.read_with(cx, |c, _| *c.terminals.keys().next().unwrap());
        // un'altra modifica (qui: il colore) tiene un salvataggio in debounce
        canvas.update(cx, |c, cx| c.cycle_color(&[id], cx));
        assert!(canvas.read_with(cx, |c, _| c.has_pending_save()));
        drop(canvas);
        cx.update(|_, _| {});
        cx.run_until_parked();
        let text = fs.load(Path::new("/repo/.superzed/project.json")).await.unwrap();
        let file = parse_project_file(&text).unwrap();
        assert_eq!(
            file.nodes[0].data,
            json!({"session_id": "abc", "phase": "new"}),
            "chiudere la tab non deve azzerare il session_id"
        );
    }

    // Final review 3: lo stesso all'uscita dall'app (on_app_quit)
    #[gpui::test]
    async fn app_quit_writes_the_pending_save(cx: &mut TestAppContext) {
        let (_ws, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.new_note(window, cx);
            assert!(c.has_pending_save());
        });
        let app: &mut TestAppContext = cx;
        app.update(|cx| cx.shutdown());
        assert_eq!(saved(&fs).await.nodes.len(), 1);
    }

    // Final review 6: una nota creata mentre project.json si carica prende l'id 1; un
    // nodo del file con lo stesso id non deve sostituirla
    #[gpui::test]
    async fn node_created_during_load_survives_a_file_node_with_the_same_id(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let file = project_json(json!([
            { "id": 1, "kind": "note", "origin": [0.0, 0.0], "size": [300.0, 200.0],
              "title": "dal file", "data": { "text": "f" } }
        ]));
        fs.insert_tree("/repo", json!({ ".superzed": { "project.json": file } })).await;
        let project = project::Project::test(fs.clone(), ["/repo".as_ref()], cx).await;
        let window = cx.add_window(|window, cx| workspace::MultiWorkspace::test_new(project, window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let canvas = workspace.read_with(cx, |ws, _| crate::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            assert!(!c.is_loaded());
            let id = c.new_note(window, cx);
            assert_eq!(id, NodeId(1));
            c.note_editor(id).unwrap().update(cx, |e, cx| e.set_text("prima", window, cx));
        });
        cx.run_until_parked();
        canvas.read_with(cx, |c, cx| {
            assert!(c.is_loaded());
            let mut nodes: Vec<(String, serde_json::Value)> = c
                .model()
                .frames()
                .iter()
                .map(|f| (f.title.clone(), c.node_save_data(f.id, cx)))
                .collect();
            nodes.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(
                nodes,
                vec![
                    ("dal file".to_string(), json!({ "text": "f" })),
                    ("note".to_string(), json!({ "text": "prima" })),
                ]
            );
            let ids: std::collections::BTreeSet<NodeId> = c.model().frames().iter().map(|f| f.id).collect();
            assert_eq!(ids.len(), 2, "id distinti");
        });
    }

    // Task 11: scheduler delle automazioni.
    use crate::automation::RunPhase;

    pub(super) fn capture_spec(path: &Path) -> task::SpawnInTerminal {
        task::SpawnInTerminal {
            label: "claude".into(),
            command: Some("/bin/sh".into()),
            args: vec!["-c".into(), format!("stty raw -echo; exec cat > '{}'", path.display())],
            use_new_terminal: true,
            allow_concurrent_runs: true,
            hide: task::HideStrategy::Always,
            ..Default::default()
        }
    }

    pub(super) fn wait_for_file(path: &Path, needle: &str, cx: &mut VisualTestContext) -> String {
        for _ in 0..100 {
            cx.run_until_parked();
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if text.contains(needle) {
                return text;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("{needle:?} non arrivato nel PTY: {:?}", std::fs::read_to_string(path));
    }

    fn at(h: u32, m: u32) -> chrono::DateTime<chrono::Local> {
        use chrono::TimeZone as _;
        chrono::Local.with_ymd_and_hms(2026, 9, 28, h, m, 0).unwrap()
    }

    /// Chat di automazione ("4h", mai eseguita: dovuta subito) col PTY di cattura.
    async fn automation_chat(cx: &mut TestAppContext) -> (Entity<CanvasView>, NodeId, String, tempfile::TempDir, &'static mut VisualTestContext) {
        automation_chat_with(cx, true).await
    }

    /// Come `automation_chat`; con `started = false` claude non ha mai mandato `SessionStart`.
    async fn automation_chat_with(
        cx: &mut TestAppContext,
        started: bool,
    ) -> (Entity<CanvasView>, NodeId, String, tempfile::TempDir, &'static mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        let spec = capture_spec(&dir.path().join("pty.txt"));
        let (canvas, node, key, dir, cx) = automation_chat_with_spec(cx, spec, dir).await;
        if started {
            store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s0", "source": "startup"})), cx));
        }
        (canvas, node, key, dir, cx)
    }

    /// Chat di automazione con questo PTY (che scrive in `dir/pty.txt`), claude in `Starting`.
    async fn automation_chat_with_spec(
        cx: &mut TestAppContext,
        spec: task::SpawnInTerminal,
        dir: tempfile::TempDir,
    ) -> (Entity<CanvasView>, NodeId, String, tempfile::TempDir, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let path = dir.path().join("pty.txt");
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        canvas.update_in(cx, |c, window, cx| {
            c.test_now = Some(at(9, 0));
            c.test_claude_pty = Some(spec);
            c.create_automation(draft("Log", "4h"), window, cx);
        });
        cx.run_until_parked();
        let node = canvas.read_with(cx, |c, _| *c.automations.keys().next().unwrap());
        let key = canvas.read_with(cx, |c, _| c.session_key(node).unwrap().to_string());
        // il file nasce dopo `stty raw`: prima il tty tradurrebbe `\r` in `\n`
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            cx.run_until_parked();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(path.exists(), "PTY di cattura non partito");
        (canvas, node, key, dir, cx)
    }

    /// Aspetta che il testo catturato soddisfi `pred` (fallisce dopo 5 s).
    fn wait_for_text(path: &Path, what: &str, pred: impl Fn(&str) -> bool, cx: &mut VisualTestContext) -> String {
        for _ in 0..100 {
            cx.run_until_parked();
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if pred(&text) {
                return text;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("{what}: {:?}", std::fs::read_to_string(path));
    }

    /// Niente e' arrivato nel PTY (dopo una breve attesa).
    pub(super) fn pty_after_a_while(path: &Path, cx: &mut VisualTestContext) -> String {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(200));
        cx.run_until_parked();
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// Esecuzione avviata e prompt mandato (fase Running).
    fn start_run(canvas: &Entity<CanvasView>, key: &str, pty: &Path, cx: &mut VisualTestContext) {
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(pty, "/clear\r", cx);
        store(cx).update(cx, |s, cx| s.apply(hook(key, json!({"hook_event_name": "SessionStart", "session_id": "s1", "source": "clear"})), cx));
        wait_for_file(pty, "Leggi i log", cx);
    }

    // Criterio 3: scadenza con chat libera → /clear, poi (nuovo id) il prompt completo.
    #[gpui::test]
    async fn due_automation_sends_clear_then_the_prompt(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat(cx).await;
        let pty = dir.path().join("pty.txt");
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(&pty, "/clear\r", cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Clearing));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_run), Some(at(9, 0)));
        assert!(std::fs::read_to_string(&pty).unwrap().starts_with("\x15/clear\r"), "ctrl-u prima del /clear");
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s1", "source": "clear"})), cx));
        let text = wait_for_file(&pty, "superzed-notify", cx);
        assert!(text.contains("Leggi i log"));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Running));
        // l'Invio arriva dopo il paste, non insieme
        cx.executor().advance_clock(super::automations::ENTER_DELAY);
        let text = wait_for_text(&pty, "invio dopo il prompt", |t| t.ends_with('\r'), cx);
        assert!(text.ends_with("notificare nulla.\x1b[201~\r"), "{text:?}");
        close_all(&canvas, cx);
    }

    // Fasce delle Automazioni in rem: con un font dell'interfaccia piu' grande la fascia
    // cresce, la chat scende sotto di lei (layout) e il disegno ha la stessa altezza.
    #[gpui::test]
    async fn automation_bands_follow_the_interface_font(cx: &mut TestAppContext) {
        let (canvas, node, cx) = canvas_with_automation(cx).await;
        cx.simulate_resize(size(px(1600.), px(1400.)));
        cx.update(|_, cx| {
            cx.update_global::<settings::SettingsStore, _>(|store, cx| {
                store.update_user_settings(cx, |s| s.theme.ui_font_size = Some(24.0.into()));
            });
        });
        redraw(cx);
        let m = canvas.read_with(cx, |c, cx| c.automations_metrics(cx));
        assert_eq!(m, automations_layout::Metrics::for_rem(24.));
        let a = canvas.read_with(cx, |c, _| c.container_node(DockKind::Automations).unwrap());
        let af = frame_of(&canvas, a, cx);
        let l = automations_layout::layout(af.origin, &[node], kanban_layout::DEFAULT_CHAT_SIZE, m);
        assert_eq!(af.size, l.size, "la sezione si ridimensiona al cambio del font");
        assert_eq!(frame_of(&canvas, node, cx).origin, l.chats[0].1, "la chat sta sotto la fascia piu' alta");
        let row = canvas.read_with(cx, |c, cx| c.automation_rows(cx).remove(0));
        assert_eq!(row.height, 144.);
        let zoom = canvas.read_with(cx, |c, _| c.viewport.zoom);
        let band = cx.debug_bounds(leak(format!("superzed-automation-band-{}", node.0))).expect("fascia disegnata");
        assert!((f32::from(band.size.height) - 144. * zoom).abs() < 0.5, "{:?} a zoom {zoom}", band.size);
        close_all(&canvas, cx);
    }

    // Revisione del Task 11: senza `SessionStart` dopo il /clear claude non risponde:
    // niente prompt, esecuzione fallita con la nota nella fascia.
    #[gpui::test]
    async fn without_a_new_session_id_the_run_fails_and_nothing_is_typed(cx: &mut TestAppContext) {
        let (canvas, node, _key, dir, cx) = automation_chat(cx).await;
        let pty = dir.path().join("pty.txt");
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(&pty, "/clear\r", cx);
        cx.executor().advance_clock(crate::automation::CLEAR_TIMEOUT);
        assert!(!pty_after_a_while(&pty, cx).contains("Leggi i log"), "nessun prompt");
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Idle));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_outcome), Some(crate::automation::Outcome::Failed));
        let rows = canvas.read_with(cx, |c, cx| c.automation_rows(cx));
        assert_eq!(rows[0].last_outcome.as_deref(), Some("failed (claude not responding)"));
        close_all(&canvas, cx);
    }

    // Criterio 3: chat occupata → niente invio; passata un'intera scadenza → skipped_busy.
    #[gpui::test]
    async fn busy_chat_waits_then_skips_a_whole_period(cx: &mut TestAppContext) {
        let (canvas, node, key, _dir, cx) = automation_chat(cx).await;
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "UserPromptSubmit"})), cx));
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Idle), "occupata: niente");
        canvas.update(cx, |c, cx| {
            c.test_now = Some(at(13, 0));
            c.automation_tick(cx);
        });
        let d = canvas.read_with(cx, |c, _| c.automation(node).cloned().unwrap());
        assert_eq!(d.last_outcome, Some(crate::automation::Outcome::SkippedBusy));
        assert_eq!(d.last_run, Some(at(13, 0)));
        assert!(!canvas.update(cx, |c, cx| c.run_automation_now(node, cx)), "Esegui ora con la chat occupata");
        assert_eq!(canvas.read_with(cx, |c, _| c.last_warning().map(str::to_string)).as_deref(), Some("“Log” is busy"));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn paused_automation_does_not_run(cx: &mut TestAppContext) {
        let (canvas, node, _key, _dir, cx) = automation_chat(cx).await;
        canvas.update(cx, |c, cx| {
            c.toggle_automation_pause(node, cx);
            c.automation_tick(cx);
        });
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Idle));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_run), None);
        close_all(&canvas, cx);
    }

    // Criteri 4 e 3: le notifiche della chat durante l'esecuzione danno l'esito notified:N,
    // con la sorgente = nome dell'automazione; nessuna campana, nessuna fase.
    #[gpui::test]
    async fn outcome_counts_notifications_and_the_chat_never_rings(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat(cx).await;
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(&dir.path().join("pty.txt"), "/clear\r", cx);
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s1", "source": "clear"})), cx));
        cx.run_until_parked();
        cx.update(|_, cx| {
            crate::registry::route_notify(&key, json!({"severity": "errore", "title": "a"}), cx);
            crate::registry::route_notify(&key, json!({"severity": "info", "title": "b"}), cx);
        });
        store(cx).update(cx, |s, cx| {
            s.apply(hook(&key, json!({"hook_event_name": "PreToolUse", "tool_name": "Skill", "tool_input": {"skill": "brainstorming"}})), cx);
            s.apply(hook(&key, json!({"hook_event_name": "Stop"})), cx);
        });
        cx.run_until_parked();
        let d = canvas.read_with(cx, |c, _| c.automation(node).cloned().unwrap());
        assert_eq!(d.last_outcome, Some(crate::automation::Outcome::Notified(2)));
        assert_eq!(canvas.read_with(cx, |c, cx| c.attention(cx)), superzed_sessions::Attention::None, "nessuna campana");
        assert_eq!(store(cx).read_with(cx, |s, _| s.get(&key).unwrap().phase), superzed_sessions::Phase::New, "nessuna fase");
        let all = superzed_notifications::NotificationFilter { project: None, include_archived: true };
        let sources: Vec<String> = cx.update(|_, cx| {
            superzed_notifications::NotificationStore::global(cx).unwrap().read(cx).list(&all).into_iter().map(|n| n.source).collect()
        });
        assert!(sources.iter().filter(|s| *s == "Log").count() == 2, "{sources:?}");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn stop_failure_is_a_failed_run(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat(cx).await;
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(&dir.path().join("pty.txt"), "/clear\r", cx);
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s1", "source": "clear"})), cx));
        wait_for_file(&dir.path().join("pty.txt"), "Leggi i log", cx);
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "StopFailure", "error_type": "rate_limit"})), cx));
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_outcome), Some(crate::automation::Outcome::Failed));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Idle));
        close_all(&canvas, cx);
    }

    // Review Focus 5: claude uscito (`SessionEnd`) → nessuna scrittura; anche passata
    // un'intera scadenza non e' "saltata (occupata)": aspetta un claude pronto
    // (revisione finale).
    #[gpui::test]
    async fn ended_chat_is_never_typed_into(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat(cx).await;
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionEnd"})), cx));
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        assert!(!pty_after_a_while(&dir.path().join("pty.txt"), cx).contains("/clear"));
        canvas.update(cx, |c, cx| {
            c.test_now = Some(at(13, 0));
            c.automation_tick(cx);
        });
        assert!(!pty_after_a_while(&dir.path().join("pty.txt"), cx).contains("/clear"));
        let d = canvas.read_with(cx, |c, _| c.automation(node).cloned().unwrap());
        assert_eq!((d.last_outcome, d.last_run), (None, None));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn run_now_starts_immediately_when_free(cx: &mut TestAppContext) {
        let (canvas, node, _key, dir, cx) = automation_chat(cx).await;
        canvas.update(cx, |c, cx| {
            c.test_now = Some(at(9, 0));
            c.automation_tick(cx); // prima esecuzione (dovuta)
        });
        wait_for_file(&dir.path().join("pty.txt"), "/clear\r", cx);
        canvas.update(cx, |c, _| {
            c.automations.get_mut(&node).unwrap().phase = RunPhase::Idle; // esecuzione chiusa
            c.test_now = Some(at(10, 0));
        });
        assert!(canvas.update(cx, |c, cx| c.run_automation_now(node, cx)), "non dovuta, ma forzata");
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_run), Some(at(10, 0)));
        close_all(&canvas, cx);
    }

    // Revisione del Task 11: un claude che non ha ancora mandato `SessionStart`
    // (`Starting`) o una chat la cui sessione non e' piu' nello store non ricevono nulla.
    #[gpui::test]
    async fn starting_or_unregistered_chat_is_never_typed_into(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat_with(cx, false).await;
        let pty = dir.path().join("pty.txt");
        assert_eq!(store(cx).read_with(cx, |s, _| s.get(&key).unwrap().state.clone()), superzed_sessions::SessionState::Starting);
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        assert!(!pty_after_a_while(&pty, cx).contains("/clear"), "Starting: niente");
        assert!(!canvas.update(cx, |c, cx| c.run_automation_now(node, cx)), "Esegui ora su claude non partito");
        store(cx).update(cx, |s, cx| s.unregister(&key, cx));
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        assert!(!pty_after_a_while(&pty, cx).contains("/clear"), "senza sessione: niente");
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Idle));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_run), None);
        close_all(&canvas, cx);
    }

    // Revisione del Task 11: un'esecuzione senza `Stop` non resta aperta per sempre.
    #[gpui::test]
    async fn a_run_without_stop_is_closed_by_the_next_tick_once_claude_is_free(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat(cx).await;
        start_run(&canvas, &key, &dir.path().join("pty.txt"), cx);
        // al lavoro, e in attesa di un permesso: la fascia lo dice, l'esecuzione resta aperta
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "PermissionRequest"})), cx));
        canvas.update(cx, |c, cx| {
            c.test_now = Some(at(9, 1));
            c.automation_tick(cx);
        });
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Running));
        assert!(canvas.read_with(cx, |c, cx| c.automation_rows(cx)[0].waiting_input), "in attesa di input");
        // claude torna libero senza Stop: meno di un tick dal prompt non basta, dopo si'
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "Notification", "notification_type": "idle_prompt"})), cx));
        canvas.update(cx, |c, cx| {
            c.automations.get_mut(&node).unwrap().prompt_sent = Some(at(9, 1));
            c.automation_tick(cx);
        });
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Running), "meno di un tick");
        canvas.update(cx, |c, cx| {
            c.test_now = Some(at(9, 2));
            c.automation_tick(cx);
        });
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Idle));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_outcome), Some(crate::automation::Outcome::Ok));
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn claude_exiting_during_a_run_closes_it_as_failed(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat(cx).await;
        start_run(&canvas, &key, &dir.path().join("pty.txt"), cx);
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionEnd", "reason": "prompt_input_exit"})), cx));
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Idle));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().last_outcome), Some(crate::automation::Outcome::Failed));
        close_all(&canvas, cx);
    }

    // Revisione del Task 11 (round 2): la prontezza non dipende dal processo in primo
    // piano. Come nel nodo vero, il "claude" gira dentro una shell `-c` senza job control
    // (niente `exec`): il gruppo in primo piano resta quello di zsh, e l'automazione parte.
    #[gpui::test]
    async fn readiness_does_not_depend_on_the_foreground_process(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pty.txt");
        let wrapped = task::SpawnInTerminal {
            command: Some("/bin/zsh".into()),
            args: vec!["-f".into(), "-c".into(), format!("stty raw -echo; cat > '{}'; exec cat", path.display())],
            ..capture_spec(&path)
        };
        let (canvas, node, key, _dir, cx) = automation_chat_with_spec(cx, wrapped, dir).await;
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s0", "source": "startup"})), cx));
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(&path, "/clear\r", cx);
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s1", "source": "clear"})), cx));
        wait_for_file(&path, "Leggi i log", cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Running));
        close_all(&canvas, cx);
    }

    // Revisione finale (1): dopo un riavvio claude puo' restare a lungo in `Starting`: il
    // recupero aspetta, anche oltre un'intera scadenza, e parte appena claude e' pronto
    // invece di finire "saltata (occupata)".
    #[gpui::test]
    async fn a_slow_starting_claude_delays_the_catch_up_without_skipping_it(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat_with(cx, false).await;
        let pty = dir.path().join("pty.txt");
        canvas.update(cx, |c, cx| {
            c.test_now = Some(at(13, 30));
            c.automation_tick(cx);
        });
        let d = canvas.read_with(cx, |c, _| c.automation(node).cloned().unwrap());
        assert_eq!((d.last_outcome, d.last_run), (None, None), "non saltata");
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s0", "source": "startup"})), cx));
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(&pty, "/clear\r", cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Clearing));
        close_all(&canvas, cx);
    }

    // Revisione finale (3, 4): il prompt parte anche col solo `Cleared` (stesso id di
    // sessione), una volta sola anche se arriva poi `SessionIdChanged`; e' sempre un
    // bracketed paste con gli a capo, anche su un terminale mai disegnato.
    #[gpui::test]
    async fn cleared_alone_sends_the_prompt_once_as_a_bracketed_paste(cx: &mut TestAppContext) {
        let (canvas, node, key, dir, cx) = automation_chat(cx).await;
        let pty = dir.path().join("pty.txt");
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        wait_for_file(&pty, "/clear\r", cx);
        // stesso id: solo `Cleared`, niente `SessionIdChanged`
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s0", "source": "clear"})), cx));
        let text = wait_for_file(&pty, "\x1b[201~", cx);
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(node)), Some(RunPhase::Running));
        assert!(text.contains("\x1b[200~Leggi i log\n\nAl termine"), "{text:?}");
        // un secondo /clear (id nuovo) durante l'esecuzione non rimanda il prompt
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s1", "source": "clear"})), cx));
        let text = pty_after_a_while(&pty, cx);
        assert_eq!(text.matches("Leggi i log").count(), 1, "{text:?}");
        close_all(&canvas, cx);
    }

    // Revisione finale (7): la creazione si salva nella definizione; al ripristino e'
    // l'ancora (una giornaliera creata alle 8, app chiusa alle 8:30 e riaperta alle 10
    // recupera le 9). Una definizione senza `created_at` prende l'ora del ripristino.
    #[gpui::test]
    async fn restored_automations_keep_their_creation_as_anchor(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let created = at(8, 0).to_rfc3339();
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [
                {"id": 1, "kind": "automations", "origin": [0.0, 0.0], "size": [100.0, 100.0], "title": "automazioni"},
                {"id": 2, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "Mattina", "dock": "automations",
                 "data": {"phase": "new", "automation": {"name": "Mattina", "schedule": "09:00", "prompt": "p", "created_at": created}}},
                {"id": 3, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "vecchia", "dock": "automations",
                 "data": {"phase": "new", "automation": {"name": "vecchia", "schedule": "09:00", "prompt": "p"}}}
            ]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_now = Some(at(10, 0));
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        canvas.read_with(cx, |c, cx| {
            assert_eq!(c.automations[&NodeId(2)].anchor, at(8, 0));
            let rows = c.automation_rows(cx);
            let row = |id| rows.iter().find(|r| r.node == NodeId(id)).unwrap().next.clone();
            assert_eq!(row(2), Ok("next: today 09:00".to_string()), "dovuta: il recupero parte");
            assert_eq!(row(3), Ok("next: tomorrow 09:00".to_string()));
            assert_eq!(c.automations[&NodeId(3)].anchor, at(10, 0));
            assert_eq!(c.automation(NodeId(3)).unwrap().created_at, Some(at(10, 0)), "da ora in poi si salva");
            let saved = c.node_save_data(NodeId(3), cx);
            let saved_at = saved["automation"]["created_at"].as_str().and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok());
            assert_eq!(saved_at, Some(at(10, 0).fixed_offset()), "stesso istante, qualunque sia il fuso");
        });
        close_all(&canvas, cx);
    }

    // Dopo un riavvio l'automazione dovuta riparte da sola appena la chat ripristinata e'
    // pronta (`SessionStart` del claude rilanciato), senza azioni dell'utente.
    #[gpui::test]
    async fn restored_automation_runs_once_its_chat_is_ready(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let config = tempfile::tempdir().unwrap();
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| superzed_sessions::init_test(cx, config.path().to_path_buf(), None));
        let file: ProjectFile = serde_json::from_value(json!({
            "version": PROJECT_FILE_VERSION,
            "nodes": [
                {"id": 1, "kind": "automations", "origin": [0.0, 0.0], "size": [100.0, 100.0], "title": "automazioni"},
                {"id": 2, "kind": "claude", "origin": [9.0, 9.0], "size": [640.0, 400.0], "title": "Mattina", "dock": "automations",
                 "data": {"phase": "new", "automation": {"name": "Mattina", "schedule": "09:00", "prompt": "p", "created_at": at(8, 0).to_rfc3339()}}}
            ]
        }))
        .unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.test_now = Some(at(10, 0));
            c.test_claude_pty = Some(zsh_spec());
            c.persist.as_mut().unwrap().loaded = false;
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        cx.run_until_parked();
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(NodeId(2))), Some(RunPhase::Idle), "claude non ancora partito");
        let key = canvas.read_with(cx, |c, _| c.session_key(NodeId(2)).map(str::to_string)).expect("chiave della chat ripristinata");
        store(cx).update(cx, |s, cx| s.apply(hook(&key, json!({"hook_event_name": "SessionStart", "session_id": "s1", "source": "resume"})), cx));
        canvas.update(cx, |c, cx| c.automation_tick(cx));
        assert_eq!(canvas.read_with(cx, |c, _| c.automation_phase(NodeId(2))), Some(RunPhase::Clearing), "il recupero delle 9 parte");
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn a_new_automation_saves_its_creation_time(cx: &mut TestAppContext) {
        let (canvas, node, _key, _dir, cx) = automation_chat(cx).await;
        assert_eq!(canvas.read_with(cx, |c, _| c.automation(node).unwrap().created_at), Some(at(9, 0)));
        close_all(&canvas, cx);
    }

    // Sottoprogetto 5A: connessioni finte (prima del workspace, cosi' anche i restore le vedono).
    pub(super) fn with_sources(cx: &mut TestAppContext, runner: Arc<FakeRunner>, connections: serde_json::Value) -> (Entity<superzed_sources::ConnectionStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        std::fs::write(&path, connections.to_string()).unwrap();
        let deps = superzed_sources::test_support::deps(runner, http_client::FakeHttpClient::with_404_response());
        let store = cx.update(|cx| superzed_sources::init_test(cx, path, deps));
        (store, dir)
    }

    fn source_ids(canvas: &Entity<CanvasView>, node: NodeId, cx: &mut VisualTestContext) -> Vec<String> {
        let view = canvas.read_with(cx, |c, _| c.source_view(node).unwrap());
        view.read_with(cx, |v, _| v.rows().iter().map(|r| r.id.clone()).collect())
    }

    #[gpui::test]
    async fn source_panels_are_created_saved_and_restored(cx: &mut TestAppContext) {
        let (_store, _dir) = with_sources(cx, github_runner(), github_connections());
        let (_ws, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        let id = canvas.update_in(cx, |c, window, cx| c.new_source_panel("gh", window, cx));
        cx.run_until_parked();
        let frame = frame_of(&canvas, id, cx);
        assert_eq!((frame.kind, frame.title.as_str()), (NodeKind::Source, "acme-app"));
        assert_eq!(frame.size, size(crate::source_node::SOURCE_SIZE.0, crate::source_node::SOURCE_SIZE.1));
        assert_eq!(source_ids(&canvas, id, cx)[0], "acme/acme_app#2");
        let view = canvas.read_with(cx, |c, _| c.source_view(id).unwrap());
        view.update(cx, |v, cx| v.set_choice("block", "blocker", cx));
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        let file = saved(&fs).await;
        let record = file.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap();
        assert_eq!(record.data, json!({"connection": "gh", "filter": {"block": "blocker"}}));
        redraw(cx);
        assert!(cx.debug_bounds("superzed-source-row-0").is_some());
    }

    #[gpui::test]
    async fn a_saved_source_panel_comes_back_with_its_filter(cx: &mut TestAppContext) {
        let (_store, _dir) = with_sources(cx, github_runner(), github_connections());
        let file = project_json(json!([{ "id": 7, "kind": "source", "origin": [0.0, 0.0], "size": [560.0, 440.0],
            "title": "acme-app", "data": {"connection": "gh", "filter": {"block": "blocker"}} }]));
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({ ".superzed": { "project.json": file } })).await;
        cx.run_until_parked();
        assert_eq!(source_ids(&canvas, NodeId(7), cx), ["acme/acme_app#2"]);
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_save_data(NodeId(7), cx)), json!({"connection": "gh", "filter": {"block": "blocker"}}));
    }

    #[gpui::test]
    async fn open_chat_from_a_panel_row_opens_a_claude_chat_beside_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (_store, _dir) = with_sources(cx, github_runner(), github_connections());
        let (_ws, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let panel = canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_source_panel("gh", window, cx)
        });
        cx.run_until_parked();
        let view = canvas.read_with(cx, |c, _| c.source_view(panel).unwrap());
        view.update(cx, |v, cx| v.open_chat(0, false, cx));
        cx.run_until_parked();
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains("Elemento dalla fonte «acme-app»") && cmd.contains("Annullamento fattura"), "{cmd}");
        let p = frame_of(&canvas, panel, cx);
        let chat = canvas.read_with(cx, |c, _| c.model().frames().iter().find(|f| f.kind == NodeKind::Claude).cloned().unwrap());
        assert_eq!(chat.origin, point(p.origin.x + p.size.width + BESIDE_GAP, p.origin.y), "accanto al pannello");
        assert_eq!(chat.dock, None);
        close_all(&canvas, cx);
    }

    #[gpui::test]
    async fn open_chat_in_kanban_from_a_panel_row_lands_in_new(cx: &mut TestAppContext) {
        let (_store, _dir) = with_sources(cx, github_runner(), github_connections());
        let (canvas, _kanban, _chats, cx) = kanban_with_claude_chats(0, cx).await;
        let panel = canvas.update_in(cx, |c, window, cx| {
            c.test_claude_pty = Some(zsh_spec());
            c.new_source_panel("gh", window, cx)
        });
        cx.run_until_parked();
        let view = canvas.read_with(cx, |c, _| c.source_view(panel).unwrap());
        view.update(cx, |v, cx| v.open_chat(0, true, cx));
        cx.run_until_parked();
        let docked = canvas.read_with(cx, |c, cx| c.docked_chats(cx));
        assert_eq!(docked.len(), 1);
        assert_eq!(docked[0].1, superzed_sessions::Phase::New);
        assert!(canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().contains("Annullamento fattura")));
        close_all(&canvas, cx);
    }

    // Review Focus 3
    #[gpui::test]
    async fn unknown_node_kinds_are_kept_and_do_not_block_the_load(cx: &mut TestAppContext) {
        let file = project_json(json!([
            { "id": 5, "kind": "note", "origin": [1.0, 2.0], "size": [300.0, 200.0], "title": "idee", "data": { "text": "ciao" } },
            { "id": 9, "kind": "lavagna", "origin": [0.0, 0.0], "size": [10.0, 10.0], "title": "futuro", "data": { "k": 1 }, "extra": true }
        ]));
        let (_ws, canvas, fs, cx) = workspace_with_repo(cx, json!({ ".superzed": { "project.json": file } })).await;
        canvas.read_with(cx, |c, _| {
            assert_eq!(c.model().frames().len(), 1);
            assert!(c.last_warning().unwrap().contains("unknown kind"), "{:?}", c.last_warning());
        });
        let fresh = canvas.update_in(cx, |c, window, cx| c.new_note(window, cx));
        assert_eq!(fresh, NodeId(10), "l'id del nodo sconosciuto resta occupato");
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        let file = saved(&fs).await;
        assert_eq!(file.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), [5, 10]);
        assert_eq!(file.foreign_nodes, vec![json!({ "id": 9, "kind": "lavagna", "origin": [0.0, 0.0], "size": [10.0, 10.0], "title": "futuro", "data": { "k": 1 }, "extra": true })]);
    }

    #[gpui::test]
    async fn project_sources_are_saved_in_project_json(cx: &mut TestAppContext) {
        let file = project_json(json!([])).replace("\"nodes\"", "\"sources\":[\"hx\"],\"nodes\"");
        let (_ws, canvas, fs, cx) = workspace_with_repo(cx, json!({ ".superzed": { "project.json": file } })).await;
        assert_eq!(canvas.read_with(cx, |c, _| c.project_sources().to_vec()), ["hx"]);
        canvas.update(cx, |c, cx| {
            c.set_project_source("gh", true, cx);
            c.set_project_source("hx", false, cx);
            c.set_project_source("gh", true, cx);
        });
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(saved(&fs).await.sources, ["gh"]);
    }

    // la spunta della vista Fonti prima che project.json sia caricato non si perde
    #[gpui::test]
    async fn project_sources_changed_during_load_survive_it(cx: &mut TestAppContext) {
        let (_ws, canvas, fs, cx) = workspace_with_repo(cx, json!({})).await;
        let file = parse_project_file(r#"{"version":1,"sources":["hx","zb"],"nodes":[]}"#).unwrap();
        canvas.update_in(cx, |c, window, cx| {
            c.persist.as_mut().unwrap().loaded = false;
            c.set_project_source("gh", true, cx);
            c.set_project_source("zb", false, cx);
            c.finish_load(LoadOutcome::Loaded(file), window, cx);
        });
        assert_eq!(canvas.read_with(cx, |c, _| c.project_sources().to_vec()), ["hx", "gh"]);
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(saved(&fs).await.sources, ["hx", "gh"], "salvato a fine caricamento");
    }

    #[gpui::test]
    async fn new_source_panel_asks_for_the_connection(cx: &mut TestAppContext) {
        let (_empty, _dir0) = with_sources(cx, github_runner(), json!({"version": 1, "connections": []}));
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        cx.update(|_, cx| {
            menu::init();
            cx.bind_keys([
                gpui::KeyBinding::new("down", menu::SelectNext, None),
                gpui::KeyBinding::new("enter", menu::Confirm, None),
            ]);
        });
        // nessuna fonte configurata: un avviso, nessun modale
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        cx.dispatch_action(crate::source_picker::NewSourcePanel);
        cx.run_until_parked();
        workspace.read_with(cx, |ws, _| {
            assert!(ws.has_notification(&NotificationId::named("superzed-no-sources".into())))
        });
        let connections = json!({"version": 1, "connections": [
            {"id": "gh", "name": "acme-app", "kind": "github", "config": {"owner": "acme", "project_number": 3, "repo": "acme/acme_app"}},
            {"id": "zb", "name": "zabbix", "kind": "zabbix", "config": {"url": "https://zabbix.example"}},
            {"id": "t", "name": "box", "kind": "ssh_tunnel", "config": {"host": "box", "forwards": [{"remote_host": "localhost", "remote_port": 8123}]}}
        ]});
        let (store, _dir) = with_sources(cx, github_runner(), connections);
        canvas.update(cx, |c, cx| c.set_project_source("zb", true, cx));
        let choices = cx.update(|_, cx| crate::source_picker::source_choices(store.read(cx), canvas.read(cx).project_sources()));
        assert_eq!(choices.iter().map(|c| (c.id.as_str(), c.in_project)).collect::<Vec<_>>(), [("zb", true), ("gh", false)], "il tunnel non e' una fonte");
        canvas.update_in(cx, |c, window, cx| window.focus(&c.focus_handle, cx));
        cx.dispatch_action(crate::source_picker::NewSourcePanel);
        cx.run_until_parked();
        let modal = workspace
            .read_with(cx, |ws, cx| ws.active_modal::<crate::source_picker::SourcePickerModal>(cx))
            .expect("picker aperto");
        assert_eq!(modal.read_with(cx, |m, _| m.selected_id().to_string()), "zb", "prima le fonti del progetto");
        assert!(cx.debug_bounds("superzed-source-pick-gh").is_some());
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        let panel = canvas
            .read_with(cx, |c, _| c.model().frames().iter().find(|f| f.kind == NodeKind::Source).map(|f| f.id))
            .expect("pannello creato");
        assert_eq!(canvas.read_with(cx, |c, cx| c.node_save_data(panel, cx))["connection"], "gh");
    }
}
