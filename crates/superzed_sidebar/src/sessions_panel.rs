use editor::Editor;
use gpui::{
    Action, App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    AnyElement, ClickEvent, Div, Stateful, Pixels, Render, SharedString, Subscription, WeakEntity, Window, div, px, relative,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use superzed_accounts::{Account, AccountStore};
use superzed_accounts::meter::{self, LimitsRow, WindowMeter};
use superzed_actions::SidebarView;
use superzed_canvas::{NodeId, NodeKind, canvas_for, golden_color, node_color};
use superzed_sessions::{Session, SessionState, SessionStore};
use util::ResultExt as _;
use crate::panel_ui::{group_header, group_is_open, matches_query, panel_header, pill, pill_background, status_dot, status_dot_size};
use ui::{ContextMenu, Disclosure, Icon, IconButton, IconName, IconSize, KeyBinding, Label, LabelSize, Tooltip, prelude::*, right_click_menu};
use workspace::{
    MultiWorkspace, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    superzed_tabs::project_tabs,
};

/// Riga della vista Sessioni: un nodo terminale del canvas di un progetto, con
/// la sua sessione claude (se ce l'ha).
pub struct SessionRow {
    pub node: NodeId,
    pub kind: NodeKind,
    pub title: String,
    pub session: Option<Session>,
    /// File toccati dalla chat ancora non committati (`superzed_canvas::uncommitted_files`).
    pub uncommitted: Vec<superzed_canvas::UncommittedFile>,
    /// Chiesto a claude di committare prima di chiudersi.
    pub committing: bool,
    /// Nodo golden (cmd-g): stella, prima nel suo rango, fisso in alto se aspetta o ha finito.
    pub golden: bool,
}

/// Gruppo di righe di un progetto (una tab), nell'ordine di visualizzazione:
/// il gruppo attivo per primo.
pub struct SessionGroup {
    pub tab_ix: usize,
    /// Chiave dello stato aperto/chiuso del gruppo (percorso del progetto o etichetta).
    pub key: SharedString,
    pub label: SharedString,
    /// Ramo git del repository attivo del progetto, se c'e'.
    pub branch: Option<SharedString>,
    pub color: Option<u8>,
    pub is_active: bool,
    pub workspace: Entity<Workspace>,
    pub rows: Vec<SessionRow>,
}

fn multi_workspace(workspace: &WeakEntity<Workspace>, cx: &App) -> Option<Entity<MultiWorkspace>> {
    workspace.upgrade()?.read(cx).multi_workspace()?.upgrade()
}

/// Vista Sessioni: elenca i nodi terminale di tutti i progetti aperti, raggruppati
/// per tab di progetto (2a: sottoprogetto sessioni globali).
pub struct SessionsPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    filter: Entity<Editor>,
    _filter_sub: Subscription,
    store_sub: Option<Subscription>,
    canvas_subs: HashMap<EntityId, Subscription>,
    git_subs: HashMap<EntityId, Subscription>,
    mw_sub: Option<Subscription>,
    /// Gruppi chiusi col disclosure (per chiave del gruppo), solo in memoria.
    collapsed: HashSet<SharedString>,
    /// Riga sotto il mouse (tab, nodo): mostra la ✕.
    hovered: Option<(usize, NodeId)>,
    /// Sezione «Limits» aperta (di default), solo in memoria.
    limits_open: bool,
    accounts_sub: Option<Subscription>,
    /// Vista piatta ordinata per priorita' invece che per progetto (salvata nel KV store).
    by_priority: bool,
}

const BY_PRIORITY_KEY: &str = "superzed_sessions_by_priority";

/// Chiave d'ordine della vista per priorita': rango dello stato, golden prima, poi il
/// cambio di stato piu' recente; i terminali (senza sessione) vanno in fondo al rango di `Idle`.
pub fn priority_key(row: &SessionRow) -> (u8, bool, std::cmp::Reverse<Option<std::time::Instant>>) {
    match &row.session {
        Some(session) => (
            superzed_sessions::priority_rank(&session.state),
            !row.golden,
            std::cmp::Reverse(Some(session.state_changed_at)),
        ),
        // `Reverse(None)` e' maggiore di ogni `Reverse(Some(_))`: dopo le chat dello stesso rango
        None => (superzed_sessions::priority_rank(&SessionState::Idle), !row.golden, std::cmp::Reverse(None)),
    }
}

/// Golden che aspetta una risposta o ha finito: fissa sopra la lista, anche scorrendo.
pub fn is_pinned(row: &SessionRow) -> bool {
    row.golden
        && row
            .session
            .as_ref()
            .is_some_and(|s| superzed_sessions::attention(&s.state) != superzed_sessions::Attention::None)
}

/// Toglie dai gruppi le righe fisse (`is_pinned`) e le restituisce per priorita', con
/// l'indice del gruppo da cui vengono. I gruppi rimasti vuoti restano: li toglie il chiamante.
pub fn take_pinned<G>(groups: &mut [G], rows: impl Fn(&mut G) -> &mut Vec<SessionRow>) -> Vec<(usize, SessionRow)> {
    let mut pinned = Vec::new();
    for (group_ix, group) in groups.iter_mut().enumerate() {
        let rows = rows(group);
        let (take, keep): (Vec<_>, Vec<_>) = std::mem::take(rows).into_iter().partition(is_pinned);
        *rows = keep;
        pinned.extend(take.into_iter().map(|row| (group_ix, row)));
    }
    pinned.sort_by_key(|(_, row)| priority_key(row));
    pinned
}

impl SessionsPanel {
    pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Filter sessions…", window, cx);
            editor
        });
        let _filter_sub = cx.observe(&filter, |_, _, cx| cx.notify());
        let by_priority = db::kvp::KeyValueStore::global(cx)
            .read_kvp(BY_PRIORITY_KEY)
            .log_err()
            .flatten()
            .is_some_and(|value| value == "true");
        Self {
            workspace,
            focus: cx.focus_handle(),
            filter,
            _filter_sub,
            store_sub: None,
            canvas_subs: HashMap::new(),
            git_subs: HashMap::new(),
            mw_sub: None,
            collapsed: HashSet::new(),
            hovered: None,
            limits_open: true,
            accounts_sub: None,
            by_priority,
        }
    }

    /// Righe del solo progetto del pannello (compatibilita' col test esistente).
    pub fn rows(&self, cx: &App) -> Vec<(NodeId, String)> {
        self.workspace
            .upgrade()
            .and_then(|ws| canvas_for(ws.read(cx)))
            .map(|canvas| canvas.read(cx).terminal_nodes(cx))
            .unwrap_or_default()
    }

    pub fn limits_open(&self) -> bool {
        self.limits_open
    }

    pub fn set_limits_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.limits_open = open;
        cx.notify();
    }

    pub fn filter_editor(&self) -> &Entity<Editor> {
        &self.filter
    }

    /// Numero di sessioni di tutti i progetti (senza filtro), per l'intestazione.
    pub fn total(&self, cx: &App) -> usize {
        self.groups(cx).iter().map(|g| g.rows.len()).sum()
    }

    pub fn groups(&self, cx: &App) -> Vec<SessionGroup> {
        let Some(mw) = multi_workspace(&self.workspace, cx) else {
            return Vec::new();
        };
        let store = SessionStore::global(cx);
        let mut groups: Vec<SessionGroup> = project_tabs(mw.read(cx), cx)
            .into_iter()
            .enumerate()
            .filter_map(|(tab_ix, tab)| {
                let workspace = tab.workspace?;
                let canvas = canvas_for(workspace.read(cx))?;
                let canvas = canvas.read(cx);
                let rows: Vec<SessionRow> = canvas
                    .terminal_nodes(cx)
                    .into_iter()
                    .map(|(node, title)| SessionRow {
                        node,
                        kind: canvas.model().frame(node).map_or(NodeKind::Shell, |f| f.kind),
                        title,
                        session: canvas
                            .session_key(node)
                            .and_then(|key| store.as_ref()?.read(cx).get(key).cloned()),
                        uncommitted: canvas.node_uncommitted_files(node, cx),
                        committing: canvas.is_committing(node),
                        golden: canvas.model().frame(node).is_some_and(|f| f.golden),
                    })
                    .collect();
                (!rows.is_empty()).then(|| SessionGroup {
                    tab_ix,
                    key: tab.path.clone().unwrap_or_else(|| tab.label.clone()),
                    branch: branch_name(&workspace, cx),
                    label: tab.label,
                    color: tab.color,
                    is_active: tab.is_active,
                    workspace,
                    rows,
                })
            })
            .collect();
        groups.sort_by_key(|g| !g.is_active); // stabile: il resto resta nell'ordine delle tab
        groups
    }
}

/// Ramo git del repository attivo del progetto del workspace.
fn branch_name(workspace: &Entity<Workspace>, cx: &App) -> Option<SharedString> {
    let repo = workspace.read(cx).project().read(cx).active_repository(cx)?;
    repo.read(cx).branch.as_ref().map(|b| SharedString::from(b.name().to_string()))
}

/// La riga corrisponde al filtro (titolo, account o modello, senza distinzione di
/// maiuscole); un filtro vuoto le fa passare tutte.
pub fn row_matches(row: &SessionRow, query: &str) -> bool {
    let session = row.session.as_ref();
    let fields = std::iter::once(row.title.as_str())
        .chain(session.map(|s| s.account.as_str()))
        .chain(session.and_then(|s| s.model.as_deref()));
    matches_query(fields, query)
}

/// Tiene nei gruppi solo le righe che corrispondono al filtro e toglie i gruppi
/// rimasti vuoti.
pub fn retain_matching<G>(groups: &mut Vec<G>, query: &str, rows: impl Fn(&mut G) -> &mut Vec<SessionRow>) {
    groups.retain_mut(|g| {
        let rows = rows(g);
        rows.retain(|r| row_matches(r, query));
        !rows.is_empty()
    });
}

/// Chat del gruppo in attesa di una risposta (campanella dell'intestazione).
pub fn waiting_count(rows: &[SessionRow]) -> usize {
    rows.iter()
        .filter(|r| r.session.as_ref().is_some_and(|s| matches!(s.state, SessionState::NeedsInput(_))))
        .count()
}

/// Colore del pallino di stato di una riga (senza sessione: terminale, attenuato).
pub fn status_dot_color(state: Option<&SessionState>) -> Color {
    match state {
        Some(SessionState::Working) => Color::Success,
        Some(SessionState::NeedsInput(_) | SessionState::Done) => Color::Warning,
        Some(SessionState::Failed(_)) => Color::Error,
        Some(SessionState::Ended) => Color::Disabled,
        Some(SessionState::Idle | SessionState::Starting) | None => Color::Muted,
    }
}

/// Colore della pill del contesto: le soglie di `pct_color`, verde sotto la soglia normale.
pub fn ctx_pill_color(left: u8) -> Color {
    match superzed_sessions::ui::pct_color(left) {
        Color::Default => Color::Success,
        alert => alert,
    }
}

/// Seconda riga: la fase per le chat claude, «Terminal» per i terminali,
/// «Committing…» mentre la chat committa prima di chiudersi.
pub fn row_subtitle(kind: NodeKind, session: Option<&Session>, committing: bool) -> &'static str {
    if committing {
        return "Committing…";
    }
    match kind {
        NodeKind::Claude => session.map_or(superzed_sessions::Phase::New, |s| s.phase).label(),
        _ => "Terminal",
    }
}

/// Seconda riga con il progetto davanti (vista per priorita'): una pill accanto al titolo
/// lo schiaccerebbe alla larghezza di default del pannello.
pub fn subtitle_with_project(project: Option<&str>, subtitle: &str) -> SharedString {
    match project {
        Some(project) => format!("{project} · {subtitle}").into(),
        None => SharedString::new(subtitle),
    }
}

/// Stato vuoto con il filtro attivo.
pub fn no_match_text(query: &str) -> String {
    format!("No sessions match “{}”", query.trim())
}

/// Attiva la tab del progetto, da' il focus al nodo e ci fa lo zoom (stessa logica del
/// doppio click sulla barra del titolo: `zoom_to_node`), dopo il ridisegno, quando il
/// workspace appena attivato e' nell'albero. L'attivazione e' per entita' workspace, non
/// per indice di tab: `tab_ix` e' quello dell'ultimo render e puo' non corrispondere piu'
/// a niente se le tab sono cambiate nel frattempo (progetto aperto/chiuso altrove).
///
/// Il ritocco (focus e zoom) e' rimandato con `window.spawn`, dopo che l'attivazione
/// della tab ha messo il workspace nell'albero; salta se nel frattempo la tab attiva e'
/// un'altra. Il task non garantisce un ridisegno prima di se' (su macOS si disegna solo
/// al frame del display link): il canvas di una tab mai disegnata non ha ancora i
/// limiti, quindi lo zoom passa da `zoom_to_node_when_ready`, che aspetta il primo
/// prepaint del canvas invece di andare perso.
pub fn focus_session(
    mw: &Entity<MultiWorkspace>,
    _tab_ix: usize,
    workspace: Entity<Workspace>,
    node: NodeId,
    window: &mut Window,
    cx: &mut App,
) {
    if mw.read(cx).workspace() != &workspace {
        let target = workspace.clone();
        mw.update(cx, |mw, cx| mw.activate(target, None, window, cx));
    }
    let mw = mw.downgrade();
    window
        .spawn(cx, async move |cx| {
            cx.update(|window, cx| {
                // nel frattempo l'utente puo' essere passato a un'altra tab: non la si
                // riporta indietro spostando il focus nel canvas nascosto
                if mw.upgrade().is_none_or(|mw| mw.read(cx).workspace() != &workspace) {
                    return;
                }
                if let Some(canvas) = canvas_for(workspace.read(cx)) {
                    canvas.update(cx, |c, cx| {
                        c.focus_node(node, window, cx);
                        c.zoom_to_node_when_ready(node, cx);
                    });
                }
            })
            .ok();
        })
        .detach();
}

impl EventEmitter<PanelEvent> for SessionsPanel {}

impl Focusable for SessionsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Stato vuoto: «Nessuna chat aperta» e, se c'e', la scorciatoia della nuova chat.
fn render_empty(focus: &FocusHandle, window: &Window, cx: &App) -> impl IntoElement {
    let key = KeyBinding::for_action_in(&superzed_actions::NewClaudeNode, focus, cx);
    let has_key = key.has_binding(window);
    v_flex()
        .debug_selector(|| "superzed-sessions-empty".into())
        .p_2()
        .gap_1()
        .child(Label::new("No open chats").color(Color::Muted))
        .when(has_key, |d| {
            d.child(
                h_flex()
                    .gap_1()
                    .child(Label::new("New chat:").size(LabelSize::Small).color(Color::Muted))
                    .child(key),
            )
        })
}

/// Chiude il nodo con la conferma del canvas; per un altro progetto prima ne attiva la
/// tab (per entita' workspace, non per indice: vedi `focus_session`), perche' il modale
/// di conferma sta nel workspace del canvas.
pub fn close_session(
    mw: &Entity<MultiWorkspace>,
    _tab_ix: usize,
    workspace: Entity<Workspace>,
    node: NodeId,
    window: &mut Window,
    cx: &mut App,
) {
    let other = mw.read(cx).workspace() != &workspace;
    let target = workspace.clone();
    let close = move |window: &mut Window, cx: &mut App| {
        if let Some(canvas) = canvas_for(workspace.read(cx)) {
            canvas.update(cx, |c, cx| c.close_nodes_with_confirm(vec![node], window, cx));
        }
    };
    if other {
        mw.update(cx, |mw, cx| mw.activate(target, None, window, cx));
        window.defer(cx, close);
    } else {
        close(window, cx);
    }
}

impl SessionsPanel {
    fn toggle_group(&mut self, key: SharedString, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        cx.notify();
    }

    fn toggle_by_priority(&mut self, cx: &mut Context<Self>) {
        self.by_priority = !self.by_priority;
        let kvp = db::kvp::KeyValueStore::global(cx);
        let value = self.by_priority.to_string();
        cx.background_spawn(async move {
            kvp.write_kvp(BY_PRIORITY_KEY.to_string(), value).await.log_err();
        })
        .detach();
        cx.notify();
    }

    fn render_header(&self, total: usize, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let by_priority = self.by_priority;
        let toggle = IconButton::new("superzed-sessions-by-priority", IconName::ArrowDown10)
            .icon_size(IconSize::Small)
            .toggle_state(by_priority)
            .tooltip(Tooltip::text(if by_priority { "Group by project" } else { "Sort by priority" }))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_by_priority(cx)))
            .into_any_element();
        panel_header("superzed-sessions", "Sessions", total, &self.filter, Some(toggle), window, cx)
    }

    fn render_group_header(&self, group: &SessionGroup, open: bool, cx: &Context<Self>) -> Stateful<Div> {
        let colors = cx.theme().colors();
        let tab_ix = group.tab_ix;
        // avatar: iniziale sul colore della tab; il testo usa lo sfondo dell'editor, con
        // cui i colori dei partecipanti sono pensati per contrastare (tema chiaro e scuro)
        let initial: SharedString = group.label.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default().into();
        let (avatar_bg, avatar_text) = match node_color(group.color, cx) {
            Some(color) => (color, Color::Custom(colors.editor_background)),
            None => (colors.element_background, Color::Default),
        };
        let avatar = div()
            .flex_none()
            .size(rems(1.25))
            .rounded_sm()
            .bg(avatar_bg)
            .flex()
            .items_center()
            .justify_center()
            .child(Label::new(initial).size(LabelSize::Small).weight(FontWeight::BOLD).color(avatar_text));
        let branch = group.branch.clone().map(|b| {
            h_flex()
                .min_w_0()
                .gap_0p5()
                .child(Icon::new(IconName::GitBranch).size(IconSize::Small).color(Color::Muted))
                .child(Label::new(b).size(LabelSize::Small).color(Color::Muted).truncate())
        });
        let waiting = waiting_count(&group.rows);
        let key = group.key.clone();
        let toggle = cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_group(key.clone(), cx));
        group_header(("superzed-sessions-group", tab_ix), ("superzed-sessions-disclosure", tab_ix), open, toggle, cx)
            .debug_selector(move || format!("superzed-sessions-group-{tab_ix}"))
            .child(avatar)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1p5()
                    .child(div().flex_none().max_w_2_3().child(Label::new(group.label.clone()).weight(FontWeight::BOLD).truncate()))
                    .children(branch),
            )
            .when(waiting > 0, |d| {
                d.child(
                    h_flex()
                        .flex_none()
                        .gap_0p5()
                        .child(Icon::new(IconName::Bell).size(IconSize::Small).color(Color::Warning))
                        .child(Label::new(waiting.to_string()).size(LabelSize::Small).color(Color::Warning)),
                )
            })
            .child(Label::new(group.rows.len().to_string()).size(LabelSize::Small).color(Color::Muted))
    }

    fn render_row(
        &self,
        mw: &Entity<MultiWorkspace>,
        tab_ix: usize,
        workspace: &Entity<Workspace>,
        project: Option<SharedString>,
        row: SessionRow,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let node = row.node;
        let status_id = tab_ix * 10_000 + node.0 as usize;
        let session = row.session.as_ref();
        // pallino di stato, col tooltip dell'icona di stato del nodo
        let state = session.map(|s| &s.state);
        let dot = status_dot(status_dot_color(state), cx)
            .id(("superzed-session-status", status_id))
            .debug_selector(move || format!("superzed-session-status-{status_id}"))
            .when_some(state.and_then(superzed_sessions::ui::status_tooltip), |d, tip| d.tooltip(Tooltip::text(tip)));
        // tipo: chat claude (il colore agente dei temi base e' verde come «Working»,
        // quindi l'ambra di warning) o terminale
        let kind_color = if row.kind == NodeKind::Claude { status.warning } else { status.info };
        let kind = div().flex_none().size(rems(0.625)).rounded_sm().bg(kind_color);
        let uncommitted = (!row.uncommitted.is_empty()).then(|| {
            let tip = row.uncommitted.iter().map(|f| f.display.as_str()).collect::<Vec<_>>().join("\n");
            h_flex()
                .id(("superzed-session-uncommitted", status_id))
                .debug_selector(move || format!("superzed-session-uncommitted-{status_id}"))
                .flex_none()
                .gap_0p5()
                .tooltip(Tooltip::text(tip))
                .child(Icon::new(IconName::GitBranch).size(IconSize::XSmall).color(Color::Modified))
                .child(Label::new(row.uncommitted.len().to_string()).size(LabelSize::Small).color(Color::Modified))
        });
        let golden = row.golden;
        let star = golden.then(|| {
            Icon::new(IconName::StarFilled)
                .size(IconSize::XSmall)
                .color(Color::Custom(golden_color(cx)))
        });
        let title = SharedString::from(row.title);
        let model = session.and_then(|s| s.model.clone());
        let title_tip = title.clone();
        let account = session.map(|s| {
            pill(colors.element_background)
                .max_w(rems(5.))
                .child(Label::new(s.account.clone()).size(LabelSize::Small).color(Color::Muted).truncate())
        });
        let ctx = session.and_then(|s| s.context_left).map(|left| {
            let color = ctx_pill_color(left);
            pill(pill_background(color, cx)).child(Label::new(format!("{left}%")).size(LabelSize::Small).weight(FontWeight::BOLD).color(color))
        });
        let hovered = self.hovered == Some((tab_ix, node));
        let focused = canvas_for(workspace.read(cx))
            .and_then(|c| c.read(cx).node_focus_handle(node, cx))
            .is_some_and(|f| f.contains_focused(window, cx));
        let close = hovered.then(|| {
            let (mw, ws) = (mw.clone(), workspace.clone());
            div().debug_selector(move || format!("superzed-session-close-{tab_ix}-{}", node.0)).child(
                IconButton::new(("superzed-session-close", status_id), IconName::Close)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Close"))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        close_session(&mw, tab_ix, ws.clone(), node, window, cx)
                    }),
            )
        });
        let subtitle = subtitle_with_project(project.as_deref(), row_subtitle(row.kind, session, row.committing));
        let (mw, ws) = (mw.clone(), workspace.clone());
        // rientro della seconda riga: sotto il titolo (pallino + tipo + spazi)
        let indent = status_dot_size() + rems(0.625 + 2. * 0.375);
        let row_element = v_flex()
            .id(SharedString::from(format!("superzed-session-{tab_ix}-{}", node.0)))
            .debug_selector(move || format!("superzed-session-{tab_ix}-{}", node.0))
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .when(focused, |d| d.bg(colors.element_selected))
            // la riga col focus resta col suo sfondo anche sotto il mouse: l'hover si
            // applica solo alle righe senza focus
            .when(!focused, |d| d.hover(|d| d.bg(colors.element_hover)))
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                let next = if *over { Some((tab_ix, node)) } else { this.hovered.filter(|h| *h != (tab_ix, node)) };
                if this.hovered != next {
                    this.hovered = next;
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .gap_1p5()
                    .child(dot)
                    .child(kind)
                    .children(star)
                    .child(
                        div()
                            .id(SharedString::from(format!("superzed-session-title-{tab_ix}-{}", node.0)))
                            .flex_1()
                            .min_w_0()
                            .tooltip(move |_, cx| match &model {
                                Some(model) => Tooltip::with_meta(title_tip.clone(), None, model.clone(), cx),
                                None => Tooltip::simple(title_tip.clone(), cx),
                            })
                            .child(Label::new(title).truncate()),
                    )
                    .children(uncommitted)
                    .children(account)
                    .children(ctx)
                    .children(close),
            )
            .child(
                div()
                    .pl(indent)
                    .child(Label::new(subtitle).size(LabelSize::Small).color(Color::Muted).buffer_font(cx).truncate()),
            )
            .on_click(move |_, window, cx| focus_session(&mw, tab_ix, ws.clone(), node, window, cx));
        let attention = state.map_or(superzed_sessions::Attention::None, superzed_sessions::attention);
        let tint = match attention {
            superzed_sessions::Attention::None => return row_element.into_any_element(),
            superzed_sessions::Attention::NeedsInput => status.warning,
            superzed_sessions::Attention::Done => colors.terminal_ansi_yellow,
        };
        row_element.bg(tint.opacity(0.12)).into_any_element()
    }
}

/// Riga di una finestra nella sezione «Limits» (Task 23): nome (colonna fissa) | barra
/// alta 6px che prende lo spazio restante | percentuale rimanente | reset breve
/// («1d 18h»). `key` e' l'id stabile per i selettori di debug («5h», «week», o lo slug
/// del modello); `name` e' l'etichetta mostrata.
fn render_window_row(id: &str, key: &str, name: &str, window: &WindowMeter, cx: &App) -> AnyElement {
    let sel = |what: &str| format!("superzed-limits-{what}-{id}-{key}");
    let (label_sel, bar_sel, pct_sel, reset_sel) = (sel("label"), sel("bar"), sel("pct"), sel("reset"));
    let (pct, color) = meter::pct_label(window.remaining);
    h_flex()
        .id(SharedString::from(format!("superzed-limits-block-{id}-{key}")))
        .w_full()
        .gap_2()
        .child(
            div()
                .flex_none()
                .w(rems(3.5))
                .debug_selector(move || label_sel)
                .child(Label::new(name.to_string()).size(LabelSize::Small).color(Color::Muted).single_line()),
        )
        .child(
            meter::meter(window.remaining, rems(1.), rems(0.375), cx)
                .flex_1()
                .min_w(rems(2.))
                .debug_selector(move || bar_sel),
        )
        .child(
            h_flex()
                .flex_none()
                .w(rems(2.5))
                .justify_end()
                .debug_selector(move || pct_sel)
                .child(Label::new(pct).size(LabelSize::Small).weight(FontWeight::BOLD).color(color).single_line()),
        )
        .child(
            h_flex()
                .flex_none()
                .w(rems(3.5))
                .justify_end()
                .debug_selector(move || reset_sel)
                .children(
                    window
                        .short_reset
                        .clone()
                        .map(|reset| Label::new(reset).size(LabelSize::Small).color(Color::Muted).single_line()),
                ),
        )
        .when_some(window.reset.clone(), |d, reset| d.tooltip(Tooltip::text(reset)))
        .into_any_element()
}

/// «Log in» di un account sloggato nella sezione «Limits»: apre una chat libera su
/// quell'account; claude, senza credenziali nella sua config dir, chiede il login.
fn open_login_chat(workspace: &WeakEntity<Workspace>, account_id: String, window: &mut Window, cx: &mut App) {
    let Some(canvas) = workspace.upgrade().and_then(|ws| canvas_for(ws.read(cx))) else {
        return;
    };
    // a fine update: il canvas rilegge il workspace
    window.defer(cx, move |window, cx| {
        canvas.update(cx, |c, cx| c.new_claude_with_account(account_id, false, window, cx));
    });
}

/// Voce del menu col tasto destro su un account della sezione «Limits»: una cartella di
/// progetto aperta e se l'account ne e' gia' escluso (`exclude_from`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExclusionEntry {
    pub label: SharedString,
    pub folder: PathBuf,
    pub excluded: bool,
}

/// Una voce per cartella aperta (la stessa cartella in due tab conta una volta).
pub fn exclusion_entries(account: &Account, folders: &[(SharedString, PathBuf)], home: &Path) -> Vec<ExclusionEntry> {
    let mut entries: Vec<ExclusionEntry> = Vec::new();
    for (label, folder) in folders {
        if entries.iter().any(|e| &e.folder == folder) {
            continue;
        }
        entries.push(ExclusionEntry { label: label.clone(), folder: folder.clone(), excluded: account.excludes_folder(folder, home) });
    }
    entries
}

/// Cartelle dei progetti aperti (etichetta della tab, cartella del canvas): le stesse che
/// la chat nuova confronta con `exclude_from`.
fn open_project_folders(workspace: &WeakEntity<Workspace>, cx: &App) -> Vec<(SharedString, PathBuf)> {
    let Some(mw) = multi_workspace(workspace, cx) else {
        return Vec::new();
    };
    project_tabs(mw.read(cx), cx)
        .into_iter()
        .filter_map(|tab| {
            let canvas = canvas_for(tab.workspace?.read(cx))?;
            Some((tab.label, canvas.read(cx).project_root_path(cx)?))
        })
        .collect()
}

/// Menu col tasto destro sul nome di un account: una voce spuntabile per ogni progetto
/// aperto, «Exclude from <progetto>», che scrive `exclude_from` nel registro.
fn exclusion_menu(account_id: String, folders: Vec<(SharedString, PathBuf)>, window: &mut Window, cx: &mut App) -> Entity<ContextMenu> {
    ContextMenu::build(window, cx, move |mut menu, _, cx| {
        let Some(store) = AccountStore::global(cx) else {
            return menu;
        };
        let Some(account) = store.read(cx).account(&account_id).cloned() else {
            return menu;
        };
        let entries = exclusion_entries(&account, &folders, util::paths::home_dir());
        if entries.is_empty() {
            return menu.header("No project open");
        }
        for entry in entries {
            let store = store.clone();
            let account_id = account_id.clone();
            let folder = entry.folder.clone();
            let excluded = entry.excluded;
            menu = menu.toggleable_entry(format!("Exclude from {}", entry.label), excluded, IconPosition::Start, None, move |_, cx| {
                store.update(cx, |s, cx| s.set_excluded_from(&account_id, &folder, !excluded, cx)).log_err();
            });
        }
        menu
    })
}

/// Un account: il nome (tooltip con l'email) sopra, poi una riga per finestra (5h, Week
/// e i limiti per modello); «No data» senza quote; sloggato: «Login needed» e il
/// pulsante «Log in» al posto delle quote in cache.
fn render_limits_row(row: LimitsRow, workspace: &WeakEntity<Workspace>, folders: &[(SharedString, PathBuf)], cx: &App) -> AnyElement {
    let id = row.id.clone();
    let row_selector = format!("superzed-limits-row-{id}");
    let name_selector = format!("superzed-limits-name-{id}");
    let tip: SharedString = row.email.clone().unwrap_or_else(|| row.label.clone()).into();
    let name_div = div()
        .id(SharedString::from(format!("superzed-limits-name-{id}")))
        .w_full()
        .debug_selector(move || name_selector)
        .tooltip(Tooltip::text(tip))
        .child(Label::new(row.label.clone()).size(LabelSize::Small).truncate());
    // l'account di sistema non si esclude: il registro non lo contiene
    let name = if id == superzed_accounts::SYSTEM_ID {
        name_div.into_any_element()
    } else {
        let folders = folders.to_vec();
        let account_id = id.clone();
        right_click_menu(SharedString::from(format!("superzed-limits-account-menu-{id}")))
            .trigger(move |_, _, _| name_div)
            .menu(move |window, cx| exclusion_menu(account_id.clone(), folders.clone(), window, cx))
            .into_any_element()
    };
    let body = if row.login_needed {
        let selector = format!("superzed-limits-login-needed-{id}");
        let button_selector = format!("superzed-limits-login-{id}");
        let workspace = workspace.clone();
        let account_id = id.clone();
        h_flex()
            .w_full()
            .gap_2()
            .justify_between()
            .child(div().debug_selector(move || selector).child(Label::new("Login needed").size(LabelSize::Small).color(Color::Warning)))
            .child(
                div().debug_selector(move || button_selector).child(
                    Button::new(SharedString::from(format!("superzed-limits-login-{id}")), "Log in")
                        .style(ButtonStyle::Filled)
                        .label_size(LabelSize::Small)
                        .on_click(move |_, window, cx| open_login_chat(&workspace, account_id.clone(), window, cx)),
                ),
            )
            .into_any_element()
    } else if row.has_data() {
        v_flex()
            .w_full()
            .gap_1()
            .children(row.windows().into_iter().map(|(key, name, window)| render_window_row(&id, &key, &name, window, cx)))
            .into_any_element()
    } else {
        let selector = format!("superzed-limits-no-data-{id}");
        div()
            .debug_selector(move || selector)
            .child(Label::new("No data").size(LabelSize::Small).color(Color::Muted))
            .into_any_element()
    };
    v_flex()
        .debug_selector(move || row_selector)
        .w_full()
        .gap_1()
        .px_1()
        .py_1()
        .child(name)
        .child(body)
        .into_any_element()
}

impl SessionsPanel {
    /// Sezione «Limits» in fondo al pannello, fuori dalla lista che scorre: al massimo
    /// ~45% dell'altezza, con scroll interno. Niente senza store degli account.
    fn render_limits(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let store = AccountStore::global(cx)?;
        let rows = meter::limits_rows(store.read(cx));
        let folders = open_project_folders(&self.workspace, cx);
        let open = self.limits_open;
        let toggle = cx.listener(move |this, _: &ClickEvent, _, cx| this.set_limits_open(!open, cx));
        let header = h_flex()
            .id("superzed-limits-header")
            .debug_selector(|| "superzed-limits-header".into())
            .flex_none()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(|d| d.bg(cx.theme().colors().element_hover))
            .on_click(toggle)
            .child(Disclosure::new("superzed-limits-disclosure", open).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.set_limits_open(!open, cx)
            })))
            .child(Label::new("Limits").size(LabelSize::Small).weight(FontWeight::BOLD).color(Color::Muted));
        Some(
            v_flex()
                .debug_selector(|| "superzed-limits".into())
                .flex_none()
                .max_h(relative(0.45))
                .mt_1()
                .pt_1()
                .border_t_1()
                .border_color(cx.theme().colors().border_variant)
                .child(header)
                .when(open, |d| {
                    d.child(
                        v_flex()
                            .id("superzed-limits-list")
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(rows.into_iter().map(|row| render_limits_row(row, &self.workspace, &folders, cx))),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for SessionsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // store: un'unica iscrizione, ci si riaggancia a ogni cambiamento di sessione
        // (stato, contesto, account...) di qualunque progetto.
        if self.store_sub.is_none()
            && let Some(store) = SessionStore::global(cx)
        {
            self.store_sub = Some(cx.observe(&store, |_, _, cx| cx.notify()));
        }
        // multi-workspace: un'unica iscrizione, per riaggiornarsi quando si aprono o
        // chiudono le tab di progetto.
        if self.mw_sub.is_none()
            && let Some(mw) = multi_workspace(&self.workspace, cx)
        {
            self.mw_sub = Some(cx.observe(&mw, |_, _, cx| cx.notify()));
        }
        // quote degli account: la sezione «Limits» segue lo store
        if self.accounts_sub.is_none()
            && let Some(store) = AccountStore::global(cx)
        {
            self.accounts_sub = Some(cx.observe(&store, |_, _, cx| cx.notify()));
        }
        let limits = self.render_limits(cx);

        let list = v_flex()
            .id("superzed-sessions-list")
            .debug_selector(|| "superzed-sessions-list".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .gap_1();
        let panel = v_flex().key_context("SuperzedSessions").track_focus(&self.focus).size_full().p_1();
        let Some(mw) = multi_workspace(&self.workspace, cx) else {
            return panel
                .child(self.render_header(0, window, cx))
                .child(list.child(render_empty(&self.focus, window, cx)))
                .children(limits);
        };
        let mut groups = self.groups(cx);
        // canvas: un'iscrizione per ciascun canvas comparso in un gruppo, cosi' i nodi
        // aperti/chiusi negli altri progetti aggiornano anche loro questa vista.
        for group in &groups {
            if let Some(canvas) = canvas_for(group.workspace.read(cx)) {
                self.canvas_subs
                    .entry(canvas.entity_id())
                    .or_insert_with(|| cx.observe(&canvas, |_, _, cx| cx.notify()));
            }
            // git store: l'indicatore dei file non committati segue i cambi di stato git
            let git_store = group.workspace.read(cx).project().read(cx).git_store().clone();
            self.git_subs
                .entry(git_store.entity_id())
                .or_insert_with(|| cx.subscribe(&git_store, |_, _, _: &project::git_store::GitStoreEvent, cx| cx.notify()));
        }
        let total: usize = groups.iter().map(|g| g.rows.len()).sum();
        let query = self.filter.read(cx).text(cx);
        retain_matching(&mut groups, &query, |g| &mut g.rows);
        // golden che aspettano o hanno finito: fuori dalla lista scorrevole, sopra
        let pinned = take_pinned(&mut groups, |g| &mut g.rows);
        let pinned_from: Vec<(usize, Entity<Workspace>, SharedString)> =
            groups.iter().map(|g| (g.tab_ix, g.workspace.clone(), g.label.clone())).collect();
        groups.retain(|g| !g.rows.is_empty());
        let pinned_section = (!pinned.is_empty()).then(|| {
            v_flex()
                .debug_selector(|| "superzed-sessions-pinned".into())
                .flex_none()
                .gap_px()
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(cx.theme().colors().border_variant)
                .children(pinned.into_iter().filter_map(|(group_ix, row)| {
                    let (tab_ix, workspace, label) = pinned_from.get(group_ix)?;
                    Some(self.render_row(&mw, *tab_ix, workspace, Some(label.clone()), row, window, cx))
                }))
        });
        let empty: Option<AnyElement> = match (total, groups.is_empty() && pinned_section.is_none()) {
            (0, _) => Some(render_empty(&self.focus, window, cx).into_any_element()),
            (_, true) => Some(
                div()
                    .debug_selector(|| "superzed-sessions-no-match".into())
                    .p_2()
                    .child(Label::new(no_match_text(&query)).color(Color::Muted))
                    .into_any_element(),
            ),
            _ => None,
        };
        let content: Vec<AnyElement> = if self.by_priority {
            let mut rows: Vec<(usize, Entity<Workspace>, SharedString, SessionRow)> = groups
                .into_iter()
                .flat_map(|group| {
                    let (tab_ix, workspace, label) = (group.tab_ix, group.workspace.clone(), group.label.clone());
                    group.rows.into_iter().map(move |row| (tab_ix, workspace.clone(), label.clone(), row))
                })
                .collect();
            rows.sort_by_key(|(_, _, _, row)| priority_key(row));
            vec![
                v_flex()
                    .debug_selector(|| "superzed-sessions-by-priority-list".into())
                    .gap_px()
                    .children(rows.into_iter().map(|(tab_ix, workspace, label, row)| {
                        self.render_row(&mw, tab_ix, &workspace, Some(label), row, window, cx)
                    }))
                    .into_any_element(),
            ]
        } else {
            groups
                .into_iter()
                .map(|mut group| {
                    // con un filtro attivo i gruppi restano espansi, stessa regola del pannello
                    // Sources: le righe che combaciano non devono restare nascoste sotto un
                    // gruppo chiuso, senza toccare lo stato salvato di collassato/aperto.
                    let open = group_is_open(self.collapsed.contains(&group.key), &query);
                    let header = self.render_group_header(&group, open, cx);
                    let mut rows = std::mem::take(&mut group.rows);
                    rows.sort_by_key(|row| !row.golden); // stabile: le golden in cima, il resto com'era
                    v_flex().gap_px().child(header).when(open, |d| {
                        d.child(
                            v_flex()
                                .pl_2()
                                .pt_0p5()
                                .gap_px()
                                .children(rows.into_iter().map(|row| {
                                    self.render_row(&mw, group.tab_ix, &group.workspace, None, row, window, cx)
                                })),
                        )
                    })
                    .into_any_element()
                })
                .collect()
        };
        panel
            .child(self.render_header(total, window, cx))
            .children(pinned_section)
            .child(list.children(empty).children(content))
            .children(limits)
    }
}

impl Panel for SessionsPanel {
    fn persistent_name() -> &'static str {
        "SuperzedSessionsPanel"
    }
    fn panel_key() -> &'static str {
        "SuperzedSessionsPanel"
    }
    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Left
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Left
    }
    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}
    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(240.)
    }
    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Terminal)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Sessions")
    }
    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(SidebarView(3))
    }
    fn activation_priority(&self) -> u32 {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superzed_sessions::{InputKind, Phase};

    #[test]
    fn exclusion_entries_follow_the_open_folders_and_mark_the_excluded_ones() {
        let home = Path::new("/Users/u");
        let account = Account {
            id: "a1".into(),
            label: "a1".into(),
            email: None,
            config_dir: Some("/cfg".into()),
            exclude_from: vec![PathBuf::from("~/Progetti/appunti")],
        };
        let folders = vec![
            ("appunti".into(), PathBuf::from("/Users/u/Progetti/appunti")),
            ("superzed".into(), PathBuf::from("/Users/u/Progetti/superzed")),
            ("appunti".into(), PathBuf::from("/Users/u/Progetti/appunti")),
        ];
        let entries = exclusion_entries(&account, &folders, home);
        assert_eq!(
            entries,
            vec![
                ExclusionEntry { label: "appunti".into(), folder: PathBuf::from("/Users/u/Progetti/appunti"), excluded: true },
                ExclusionEntry { label: "superzed".into(), folder: PathBuf::from("/Users/u/Progetti/superzed"), excluded: false },
            ],
            "la cartella doppia conta una volta"
        );
        assert!(exclusion_entries(&account, &[], home).is_empty());
    }

    fn row(node: u64, kind: NodeKind, title: &str, account: Option<&str>, model: Option<&str>) -> SessionRow {
        SessionRow {
            node: NodeId(node),
            kind,
            title: title.into(),
            session: account.map(|a| {
                let mut s = Session::new(a.into(), None);
                s.model = model.map(Into::into);
                s
            }),
            uncommitted: Vec::new(),
            committing: false,
            golden: false,
        }
    }

    fn with_state(mut r: SessionRow, state: SessionState) -> SessionRow {
        r.session.as_mut().unwrap().state = state;
        r
    }

    #[test]
    fn the_filter_matches_title_account_and_model_ignoring_case() {
        let chat = row(1, NodeKind::Claude, "Atlas testing", Some("j.doe"), Some("Opus 5.5"));
        let shell = row(2, NodeKind::Shell, "shell", None, None);
        assert!(row_matches(&chat, ""), "filtro vuoto: tutto");
        assert!(row_matches(&chat, "  "), "solo spazi: tutto");
        assert!(row_matches(&chat, "atlas"));
        assert!(row_matches(&chat, "DOE"));
        assert!(row_matches(&chat, "opus"));
        assert!(row_matches(&chat, " Opus 5 "), "spazi ai lati ignorati");
        assert!(!row_matches(&chat, "haiku"));
        assert!(row_matches(&shell, "SHE"));
        assert!(!row_matches(&shell, "opus"), "un terminale non ha account ne' modello");
    }

    #[test]
    fn filtering_drops_the_groups_left_empty() {
        let mut groups = vec![
            vec![row(1, NodeKind::Claude, "Atlas", Some("a"), None), row(2, NodeKind::Shell, "shell", None, None)],
            vec![row(3, NodeKind::Shell, "build", None, None)],
            vec![row(4, NodeKind::Claude, "Atlas bis", Some("b"), Some("Haiku"))],
        ];
        retain_matching(&mut groups, "ATLAS", |g| g);
        assert_eq!(groups.len(), 2, "il gruppo senza risultati sparisce");
        assert_eq!(groups[0].iter().map(|r| r.node).collect::<Vec<_>>(), vec![NodeId(1)]);
        assert_eq!(groups[1].iter().map(|r| r.node).collect::<Vec<_>>(), vec![NodeId(4)]);
        let mut all = vec![vec![row(1, NodeKind::Shell, "x", None, None)], vec![]];
        retain_matching(&mut all, "", |g| g);
        assert_eq!(all.len(), 1, "anche senza filtro un gruppo vuoto non si mostra");
    }

    #[test]
    fn waiting_counts_the_chats_that_need_input() {
        let rows = vec![
            with_state(row(1, NodeKind::Claude, "a", Some("x"), None), SessionState::NeedsInput(InputKind::Question)),
            with_state(row(2, NodeKind::Claude, "b", Some("x"), None), SessionState::NeedsInput(InputKind::Permission)),
            with_state(row(3, NodeKind::Claude, "c", Some("x"), None), SessionState::Working),
            row(4, NodeKind::Shell, "d", None, None),
        ];
        assert_eq!(waiting_count(&rows), 2);
        assert_eq!(waiting_count(&rows[2..]), 0);
    }

    #[test]
    fn dot_colors_follow_the_state() {
        assert_eq!(status_dot_color(Some(&SessionState::Working)), Color::Success);
        assert_eq!(status_dot_color(Some(&SessionState::NeedsInput(InputKind::Other))), Color::Warning);
        assert_eq!(status_dot_color(Some(&SessionState::Failed("x".into()))), Color::Error);
        assert_eq!(status_dot_color(Some(&SessionState::Idle)), Color::Muted);
        assert_eq!(status_dot_color(Some(&SessionState::Starting)), Color::Muted);
        assert_eq!(status_dot_color(Some(&SessionState::Done)), Color::Warning);
        assert_eq!(status_dot_color(Some(&SessionState::Ended)), Color::Disabled);
        assert_eq!(status_dot_color(None), Color::Muted, "terminale");
    }

    #[test]
    fn ctx_pill_is_green_above_the_warning_threshold() {
        assert_eq!(ctx_pill_color(80), Color::Success);
        assert_eq!(ctx_pill_color(20), Color::Success);
        assert_eq!(ctx_pill_color(19), Color::Warning);
        assert_eq!(ctx_pill_color(9), Color::Error);
    }

    #[test]
    fn subtitle_is_the_phase_or_terminal() {
        let mut chat = row(1, NodeKind::Claude, "a", Some("x"), None);
        assert_eq!(row_subtitle(chat.kind, chat.session.as_ref(), false), "New");
        chat.session.as_mut().unwrap().phase = Phase::Brainstorming;
        assert_eq!(row_subtitle(chat.kind, chat.session.as_ref(), false), "Brainstorm");
        assert_eq!(row_subtitle(NodeKind::Claude, None, false), "New", "chat che sta partendo");
        assert_eq!(row_subtitle(NodeKind::Shell, None, false), "Terminal");
    }

    #[test]
    fn done_dot_is_amber_like_needs_input() {
        assert_eq!(status_dot_color(Some(&SessionState::Done)), Color::Warning);
    }

    #[test]
    fn subtitle_with_project_only_in_the_priority_view() {
        assert_eq!(subtitle_with_project(Some("superzed"), "Brainstorm").to_string(), "superzed · Brainstorm");
        assert_eq!(subtitle_with_project(Some("superzed"), "Committing…").to_string(), "superzed · Committing…");
        assert_eq!(subtitle_with_project(Some("superzed"), "Terminal").to_string(), "superzed · Terminal");
        assert_eq!(subtitle_with_project(None, "Brainstorm").to_string(), "Brainstorm");
    }

    #[test]
    fn subtitle_while_committing() {
        let chat = row(1, NodeKind::Claude, "a", Some("x"), None);
        assert_eq!(row_subtitle(chat.kind, chat.session.as_ref(), true), "Committing…");
        assert_eq!(row_subtitle(chat.kind, chat.session.as_ref(), false), "New");
    }

    #[test]
    fn priority_key_orders_by_rank_then_recency_with_terminals_last() {
        let now = std::time::Instant::now();
        let earlier = now.checked_sub(std::time::Duration::from_secs(30)).unwrap();
        let mut done_old = with_state(row(1, NodeKind::Claude, "a", Some("x"), None), SessionState::Done);
        done_old.session.as_mut().unwrap().state_changed_at = earlier;
        let mut done_new = with_state(row(2, NodeKind::Claude, "b", Some("x"), None), SessionState::Done);
        done_new.session.as_mut().unwrap().state_changed_at = now;
        let waiting = with_state(row(3, NodeKind::Claude, "c", Some("x"), None), SessionState::NeedsInput(InputKind::Question));
        let idle = with_state(row(4, NodeKind::Claude, "d", Some("x"), None), SessionState::Idle);
        let shell = row(5, NodeKind::Shell, "sh", None, None);
        let mut rows = [shell, idle, done_old, waiting, done_new];
        rows.sort_by_key(priority_key);
        assert_eq!(rows.iter().map(|r| r.node.0).collect::<Vec<_>>(), vec![3, 2, 1, 4, 5]);
    }

    #[test]
    fn golden_rows_come_first_within_the_same_rank() {
        let done = with_state(row(1, NodeKind::Claude, "a", Some("x"), None), SessionState::Done);
        let mut golden_idle = with_state(row(2, NodeKind::Claude, "b", Some("x"), None), SessionState::Idle);
        golden_idle.golden = true;
        let idle = with_state(row(3, NodeKind::Claude, "c", Some("x"), None), SessionState::Idle);
        let mut golden_shell = row(4, NodeKind::Shell, "sh", None, None);
        golden_shell.golden = true;
        let shell = row(5, NodeKind::Shell, "sh2", None, None);
        let mut rows = [shell, idle, golden_shell, golden_idle, done];
        rows.sort_by_key(priority_key);
        assert_eq!(rows.iter().map(|r| r.node.0).collect::<Vec<_>>(), vec![1, 2, 4, 3, 5], "anche un terminale golden sale");
    }

    #[test]
    fn pinned_rows_are_the_golden_ones_that_wait_or_finished() {
        let mut waiting = with_state(row(1, NodeKind::Claude, "a", Some("x"), None), SessionState::NeedsInput(InputKind::Question));
        waiting.golden = true;
        let mut done = with_state(row(2, NodeKind::Claude, "b", Some("x"), None), SessionState::Done);
        done.golden = true;
        let mut golden_idle = with_state(row(3, NodeKind::Claude, "c", Some("x"), None), SessionState::Idle);
        golden_idle.golden = true;
        let plain_done = with_state(row(4, NodeKind::Claude, "d", Some("x"), None), SessionState::Done);
        let mut groups = vec![vec![plain_done, done], vec![golden_idle, waiting]];
        let pinned = take_pinned(&mut groups, |g| g);
        assert_eq!(pinned.iter().map(|(group, r)| (*group, r.node.0)).collect::<Vec<_>>(), vec![(1, 1), (0, 2)], "per priorita'");
        assert_eq!(groups[0].iter().map(|r| r.node.0).collect::<Vec<_>>(), vec![4]);
        assert_eq!(groups[1].iter().map(|r| r.node.0).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn no_match_text_quotes_the_query() {
        assert_eq!(no_match_text(" atlas "), "No sessions match “atlas”");
    }
}
