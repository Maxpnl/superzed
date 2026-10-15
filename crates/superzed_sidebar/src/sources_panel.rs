//! Vista Fonti (cmd-6): connessioni globali (GitHub, HyperDX, Zabbix, ClickHouse, tunnel
//! SSH) con lo stato dei tunnel (apri/chiudi a mano), aggiunta, modifica ed eliminazione,
//! e la spunta "usata da questo progetto" (project.json → `sources`).
use crate::connection_form::open_connection_form;
use crate::panel_ui::{group_header, group_is_open, matches_query, panel_header, pill, pill_background, status_dot, status_dot_size};
use editor::Editor;
use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, EntityId, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, Pixels,
    Render, SharedString, Subscription, Task, WeakEntity, Window, px,
};
use std::collections::HashSet;
use std::time::{Duration, Instant};
use superzed_actions::SidebarView;
use superzed_canvas::{CanvasView, canvas_for};
use superzed_sources::{ClickhouseTransport, Connection, ConnectionConfig, ConnectionKind, ConnectionStore, TunnelState, format_forwards};
use ui::{
    Button, ButtonStyle, Checkbox, ContextMenu, Icon, IconButton, IconName, IconSize, Label, LabelSize, PopoverMenu, PopoverTrigger, ToggleState, Tooltip,
    prelude::*,
};
use workspace::{
    Toast, Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::NotificationId,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionRow {
    pub id: String,
    pub name: String,
    pub kind: ConnectionKind,
    pub detail: String,
    /// Solo per i tunnel: stato che guida bottoni e colore (mai il testo).
    pub tunnel_status: Option<TunnelStatus>,
    /// Solo per le fonti da pannello: usata da questo progetto.
    pub used: Option<bool>,
    /// Errore da mostrare sotto la riga (tunnel in errore o configurazione non valida).
    pub error: Option<SourceError>,
}

/// Errore di una riga: `summary` sotto la riga (fino a 3 righe), `full` nel tooltip.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceError {
    pub summary: String,
    pub full: String,
}

/// Ordine dei gruppi per tipo nella vista.
pub const GROUP_ORDER: [ConnectionKind; 5] =
    [ConnectionKind::Github, ConnectionKind::Hyperdx, ConnectionKind::Clickhouse, ConnectionKind::Zabbix, ConnectionKind::SshTunnel];

/// Gruppo di righe di un tipo di connessione.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceGroup {
    pub kind: ConnectionKind,
    pub rows: Vec<ConnectionRow>,
}

/// Righe raggruppate per tipo nell'ordine di `GROUP_ORDER`; nel gruppo l'ordine di
/// creazione; i gruppi vuoti non ci sono.
pub fn group_rows(rows: Vec<ConnectionRow>) -> Vec<SourceGroup> {
    let mut groups: Vec<SourceGroup> = GROUP_ORDER.iter().map(|&kind| SourceGroup { kind, rows: Vec::new() }).collect();
    for row in rows {
        if let Some(group) = groups.iter_mut().find(|g| g.kind == row.kind) {
            group.rows.push(row);
        }
    }
    groups.retain(|g| !g.rows.is_empty());
    groups
}

/// La riga corrisponde al filtro: nome, dettaglio (host, repo, URL) o tipo.
pub fn source_matches(row: &ConnectionRow, query: &str) -> bool {
    matches_query([row.name.as_str(), row.detail.as_str(), row.kind.label()], query)
}

/// Colore del pallino e del testo della pill di stato.
pub fn status_color(status: TunnelStatus) -> Color {
    match status {
        TunnelStatus::Open => Color::Success,
        TunnelStatus::Opening => Color::Warning,
        TunnelStatus::Failed => Color::Error,
        TunnelStatus::Closed => Color::Muted,
    }
}

/// Testo breve della pill di stato.
pub fn status_text(status: TunnelStatus) -> &'static str {
    match status {
        TunnelStatus::Open => "Open",
        TunnelStatus::Opening => "Opening…",
        TunnelStatus::Failed => "Failed",
        TunnelStatus::Closed => "Closed",
    }
}

/// Errore di un tunnel: l'ultima riga di stderr e cosa succede adesso (nuovo tentativo
/// fra N secondi, o nessuno se la configurazione non e' valida); nel tooltip tutto stderr.
pub fn tunnel_error(state: &TunnelState, config_error: bool, now: Instant) -> Option<SourceError> {
    let TunnelState::Failed { stderr, retry_at } = state else {
        return None;
    };
    let note = if config_error {
        "Invalid tunnel configuration".to_string()
    } else {
        format!("Retrying in {} s", retry_at.saturating_duration_since(now).as_secs())
    };
    let stderr = stderr.trim();
    let last = stderr.lines().last().unwrap_or("").trim().trim_end_matches('.');
    let summary = if last.is_empty() { note.clone() } else { format!("{last}. {note}") };
    let full = if stderr.is_empty() { note } else { format!("{stderr}\n{note}") };
    Some(SourceError { summary, full })
}

/// Stato vuoto con il filtro attivo.
pub fn no_match_text(query: &str) -> String {
    format!("No sources match “{}”", query.trim())
}

/// Icona del tipo di connessione (intestazione del gruppo).
pub fn kind_icon(kind: ConnectionKind) -> IconName {
    match kind {
        ConnectionKind::Github => IconName::Github,
        ConnectionKind::Hyperdx => IconName::Reader,
        ConnectionKind::Clickhouse => IconName::DatabaseZap,
        ConnectionKind::Zabbix => IconName::SignalHigh,
        ConnectionKind::SshTunnel => IconName::ArrowRightLeft,
    }
}

/// Stato di un tunnel per la riga della vista: decide pallino, pill e bottone apri/chiudi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunnelStatus {
    Closed,
    Opening,
    Open,
    Failed,
}

pub fn tunnel_status(state: &TunnelState) -> TunnelStatus {
    match state {
        TunnelState::Closed => TunnelStatus::Closed,
        TunnelState::Opening => TunnelStatus::Opening,
        TunnelState::Open => TunnelStatus::Open,
        TunnelState::Failed { .. } => TunnelStatus::Failed,
    }
}

/// Seconda riga: campi brevi etichettati (repo, project, host, ports, tunnel, scope);
/// gli URL parlano da soli.
pub fn connection_detail(conn: &Connection) -> String {
    let with_tunnel = |base: String, tunnel: &Option<String>| match tunnel {
        Some(t) => format!("{base} · tunnel {t}"),
        None => base,
    };
    match &conn.config {
        ConnectionConfig::Github(g) if g.from_project() => "from .claude/claude-pm.local.md".into(),
        ConnectionConfig::Github(g) => {
            let project = match (g.owner.as_deref(), g.project_number) {
                (Some(owner), Some(n)) => Some(format!("project {owner}/{n}")),
                (None, Some(n)) => Some(format!("project {n}")),
                (Some(owner), None) => Some(format!("owner {owner}")),
                (None, None) => None,
            };
            g.repo.as_deref().map(|r| format!("repo {r}")).into_iter().chain(project).collect::<Vec<_>>().join(" · ")
        }
        ConnectionConfig::SshTunnel(t) => format!("host {} · ports {}", t.host, format_forwards(&t.forwards)),
        ConnectionConfig::Clickhouse(c) => match &c.transport {
            ClickhouseTransport::Http { url, tunnel } => with_tunnel(url.clone(), tunnel),
            ClickhouseTransport::SshExec { host, .. } => format!("ssh {host}"),
        },
        ConnectionConfig::Hyperdx(h) => {
            let scope: Vec<&str> = [h.namespace.as_deref(), h.environment.as_deref()].into_iter().flatten().collect();
            if scope.is_empty() {
                format!("clickhouse {}", h.clickhouse)
            } else {
                format!("clickhouse {} · scope {}", h.clickhouse, scope.join("/"))
            }
        }
        ConnectionConfig::Zabbix(z) => with_tunnel(z.url.clone(), &z.tunnel),
    }
}

pub struct SourcesPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    store_sub: Option<Subscription>,
    /// Canvas osservato (le spunte "usata da questo progetto" cambiano anche da li').
    canvas_sub: Option<(EntityId, Subscription)>,
    project_sources: Vec<String>,
    /// Ridisegno ogni secondo (conto alla rovescia) solo mentre un tunnel e' in errore.
    countdown: Option<Task<()>>,
    filter: Entity<Editor>,
    _filter_sub: Subscription,
    /// Gruppi chiusi col disclosure, solo in memoria.
    collapsed: HashSet<ConnectionKind>,
    /// Riga sotto il mouse (id della connessione): mostra le azioni.
    hovered: Option<String>,
}

impl SourcesPanel {
    pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // lo store notifica anche i cambi dei tunnel
        let store_sub = ConnectionStore::global(cx).map(|s| cx.observe(&s, |_, _, cx| cx.notify()));
        let filter = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Filter sources…", window, cx);
            editor
        });
        let _filter_sub = cx.observe(&filter, |_, _, cx| cx.notify());
        Self {
            workspace,
            focus: cx.focus_handle(),
            store_sub,
            canvas_sub: None,
            project_sources: Vec::new(),
            countdown: None,
            filter,
            _filter_sub,
            collapsed: HashSet::new(),
            hovered: None,
        }
    }

    pub fn filter_editor(&self) -> &Entity<Editor> {
        &self.filter
    }

    /// Numero di connessioni (senza filtro), per l'intestazione.
    pub fn total(&self, cx: &App) -> usize {
        ConnectionStore::global(cx).map_or(0, |s| s.read(cx).connections().len())
    }

    pub fn countdown_active(&self) -> bool {
        self.countdown.is_some()
    }

    /// Qualche tunnel in errore che riprovera' (il testo mostra i secondi che mancano).
    fn any_tunnel_retrying(cx: &App) -> bool {
        let Some(store) = ConnectionStore::global(cx) else {
            return false;
        };
        let store = store.read(cx);
        let tunnels = store.tunnels().read(cx);
        store.connections().iter().any(|c| {
            c.kind() == ConnectionKind::SshTunnel && matches!(tunnels.state(&c.id), TunnelState::Failed { .. }) && !tunnels.config_error(&c.id)
        })
    }

    fn sync_countdown(&mut self, cx: &mut Context<Self>) {
        if !Self::any_tunnel_retrying(cx) {
            self.countdown = None;
            return;
        }
        if self.countdown.is_some() {
            return;
        }
        self.countdown = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let go_on = this
                    .update(cx, |_, cx| {
                        let retrying = Self::any_tunnel_retrying(cx);
                        if retrying {
                            cx.notify();
                        }
                        retrying
                    })
                    .unwrap_or(false);
                if !go_on {
                    break;
                }
            }
            this.update(cx, |p, _| p.countdown = None).ok();
        }));
    }

    /// Osserva il canvas del progetto: ridisegno solo quando le fonti del progetto cambiano.
    fn sync_canvas_sub(&mut self, cx: &mut Context<Self>) {
        let Some(canvas) = self.canvas(cx) else {
            self.canvas_sub = None;
            return;
        };
        if self.canvas_sub.as_ref().is_some_and(|(id, _)| *id == canvas.entity_id()) {
            return;
        }
        self.project_sources = canvas.read(cx).project_sources().to_vec();
        let sub = cx.observe(&canvas, |p, canvas, cx| {
            let now = canvas.read(cx).project_sources();
            if p.project_sources != now {
                p.project_sources = now.to_vec();
                cx.notify();
            }
        });
        self.canvas_sub = Some((canvas.entity_id(), sub));
    }

    fn canvas(&self, cx: &App) -> Option<Entity<CanvasView>> {
        canvas_for(self.workspace.upgrade()?.read(cx))
    }

    pub fn rows(&self, cx: &App) -> Vec<ConnectionRow> {
        let Some(store) = ConnectionStore::global(cx) else {
            return Vec::new();
        };
        let store = store.read(cx);
        let tunnels = store.tunnels().read(cx);
        let now = tunnels.now();
        let used = self.canvas(cx).map(|c| c.read(cx).project_sources().to_vec()).unwrap_or_default();
        store
            .connections()
            .iter()
            .map(|c| ConnectionRow {
                id: c.id.clone(),
                name: c.name.clone(),
                kind: c.kind(),
                detail: connection_detail(c),
                tunnel_status: (c.kind() == ConnectionKind::SshTunnel).then(|| tunnel_status(&tunnels.state(&c.id))),
                used: c.kind().source_kind().map(|_| used.contains(&c.id)),
                error: (c.kind() == ConnectionKind::SshTunnel)
                    .then(|| tunnel_error(&tunnels.state(&c.id), tunnels.config_error(&c.id), now))
                    .flatten(),
            })
            .collect()
    }

    /// File illeggibile o voci messe da parte.
    pub fn banner(&self, cx: &App) -> Option<String> {
        let store = ConnectionStore::global(cx)?;
        let s = store.read(cx);
        if let Some(e) = s.load_error() {
            return Some(format!("connections.json is not readable: {e}. Changes are not saved until you fix it and reload it."));
        }
        (!s.warnings().is_empty()).then(|| s.warnings().join("\n"))
    }

    /// Rilegge connections.json (dopo averlo corretto a mano).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(store) = ConnectionStore::global(cx) {
            store.update(cx, |s, cx| s.reload(cx));
        }
    }

    pub fn set_used(&mut self, id: &str, used: bool, cx: &mut Context<Self>) {
        if let Some(canvas) = self.canvas(cx) {
            canvas.update(cx, |c, cx| c.set_project_source(id, used, cx));
        }
        cx.notify();
    }

    pub fn open_form(&mut self, kind: ConnectionKind, existing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let existing = existing.and_then(|id| ConnectionStore::global(cx)?.read(cx).get(&id).cloned());
        if let Some(ws) = self.workspace.upgrade() {
            ws.update(cx, |ws, cx| open_connection_form(ws, kind, existing, window, cx));
        }
    }

    /// Domanda di conferma: il nome della connessione e se il progetto corrente la usa.
    pub fn delete_message(&self, id: &str, cx: &App) -> String {
        let name = ConnectionStore::global(cx).and_then(|s| s.read(cx).get(id).map(|c| c.name.clone())).unwrap_or_else(|| id.to_string());
        let used = self.canvas(cx).is_some_and(|c| c.read(cx).project_sources().iter().any(|s| s == id));
        if used {
            format!("Delete the connection “{name}”? The current project uses it: its panels will be left without a source.")
        } else {
            format!("Delete the connection “{name}”?")
        }
    }

    /// Conferma (y/n), poi elimina; una connessione usata da altre resta, con un avviso.
    pub fn delete(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.workspace.upgrade() else {
            return;
        };
        let message = self.delete_message(id, cx);
        let (id, weak_ws) = (id.to_string(), ws.downgrade());
        ws.update(cx, |ws, cx| {
            workspace::superzed_confirm::superzed_confirm_action(
                ws,
                message,
                "Delete",
                true,
                move |_, cx| {
                    let Some(store) = ConnectionStore::global(cx) else {
                        return;
                    };
                    if let Err(e) = store.update(cx, |s, cx| s.remove(&id, cx)) {
                        cx.defer(move |cx| {
                            weak_ws
                                .update(cx, |ws, cx| ws.show_toast(Toast::new(NotificationId::named("superzed-sources-error".into()), e), cx))
                                .ok();
                        });
                    }
                },
                window,
                cx,
            )
        });
    }

    pub fn open_tunnel(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(store) = ConnectionStore::global(cx) {
            store.update(cx, |s, cx| s.hold_tunnel(id, cx));
        }
    }

    pub fn close_tunnel(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(store) = ConnectionStore::global(cx) {
            store.update(cx, |s, cx| s.close_tunnel(id, cx));
        }
    }

    /// Apre sul canvas del progetto il pannello della fonte (come `NewSourcePanel`).
    pub fn add_panel(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(canvas) = self.canvas(cx) {
            canvas.update(cx, |c, cx| {
                c.new_source_panel(id, window, cx);
            });
        }
    }

    fn toggle_group(&mut self, kind: ConnectionKind, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&kind) {
            self.collapsed.insert(kind);
        }
        cx.notify();
    }

    fn render_group_header(&self, group: &SourceGroup, open: bool, cx: &Context<Self>) -> impl IntoElement {
        let kind = group.kind;
        let key = kind.key();
        let toggle = cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_group(kind, cx));
        group_header(
            SharedString::from(format!("superzed-sources-group-{key}")),
            SharedString::from(format!("superzed-sources-disclosure-{key}")),
            open,
            toggle,
            cx,
        )
        .debug_selector(move || format!("superzed-sources-group-{key}"))
        .child(Icon::new(kind_icon(kind)).size(IconSize::Small).color(Color::Muted))
        .child(div().flex_1().min_w_0().child(Label::new(kind.label()).weight(FontWeight::BOLD).truncate()))
        .child(Label::new(group.rows.len().to_string()).size(LabelSize::Small).color(Color::Muted))
    }

    /// Azioni della riga sotto il mouse: apri/chiudi il tunnel, pannello sul canvas (solo
    /// fonti da pannello), modifica, elimina (con conferma).
    fn render_actions(&self, row: &ConnectionRow, cx: &mut Context<Self>) -> Div {
        let id = row.id.clone();
        let action = |name: &str, icon: IconName, tip: &'static str| {
            let selector = format!("superzed-source-{name}-{id}");
            let button = IconButton::new(SharedString::from(selector.clone()), icon).icon_size(IconSize::Small).tooltip(Tooltip::text(tip));
            (selector, button)
        };
        let with_selector = |(selector, button): (String, IconButton)| div().debug_selector(move || selector).child(button);
        let mut buttons = h_flex().flex_none().gap_0p5();
        if let Some(status) = row.tunnel_status {
            let tid = id.clone();
            buttons = buttons.child(with_selector(if status == TunnelStatus::Closed {
                let (sel, b) = action("tunnel-open", IconName::PlayOutlined, "Open the tunnel");
                (sel, b.on_click(cx.listener(move |p, _, _, cx| p.open_tunnel(&tid, cx))))
            } else {
                let (sel, b) = action("tunnel-close", IconName::Stop, "Close the tunnel");
                (sel, b.on_click(cx.listener(move |p, _, _, cx| p.close_tunnel(&tid, cx))))
            }));
        }
        if row.kind.source_kind().is_some() {
            let pid = id.clone();
            let (sel, b) = action("add-panel", IconName::SquarePlus, "Add a panel to the canvas");
            buttons = buttons.child(with_selector((sel, b.on_click(cx.listener(move |p, _, window, cx| p.add_panel(&pid, window, cx))))));
        }
        let (kind, edit_id, delete_id) = (row.kind, id.clone(), id.clone());
        let (sel, edit) = action("edit", IconName::Pencil, "Edit");
        buttons = buttons.child(with_selector((sel, edit.on_click(cx.listener(move |p, _, window, cx| p.open_form(kind, Some(edit_id.clone()), window, cx))))));
        let (sel, delete) = action("delete", IconName::Trash, "Delete");
        buttons.child(with_selector((sel, delete.on_click(cx.listener(move |p, _, window, cx| p.delete(&delete_id, window, cx))))))
    }

    /// Riga: pallino di stato (slot fisso anche senza stato), nome, pill di stato e, al
    /// passaggio, le azioni; sotto il dettaglio in monospace, l'errore e la spunta del progetto.
    fn render_row(&self, row: ConnectionRow, cx: &mut Context<Self>) -> impl IntoElement {
        let id = row.id.clone();
        let hover_bg = cx.theme().colors().element_hover;
        let hovered = self.hovered.as_deref() == Some(id.as_str());
        let dot = match row.tunnel_status {
            Some(status) => status_dot(status_color(status), cx),
            None => div().flex_none().size(status_dot_size()),
        };
        let status_pill = row.tunnel_status.map(|status| {
            let color = status_color(status);
            let selector = format!("superzed-source-status-{id}");
            pill(pill_background(color, cx))
                .debug_selector(move || selector)
                .child(Label::new(status_text(status)).size(LabelSize::Small).weight(FontWeight::BOLD).color(color))
        });
        let actions = hovered.then(|| self.render_actions(&row, cx));
        let used = row.used.map(|used| {
            let (used_id, selector) = (id.clone(), format!("superzed-source-used-{id}"));
            let view = cx.entity().downgrade();
            // spunta e testo cliccabili insieme
            div().debug_selector(move || selector).child(
                Checkbox::new(SharedString::from(format!("superzed-source-used-{id}")), ToggleState::from(used))
                    .label("Used by this project")
                    .label_size(LabelSize::Small)
                    .on_click(move |state, _, cx| {
                        let used = *state == ToggleState::Selected;
                        view.update(cx, |p, cx| p.set_used(&used_id, used, cx)).ok();
                    }),
            )
        });
        let error = row.error.map(|e| {
            let selector = format!("superzed-source-error-{id}");
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector)
                .min_w_0()
                .tooltip(Tooltip::text(e.full))
                .child(Label::new(e.summary).size(LabelSize::Small).color(Color::Error).line_clamp(3))
        });
        let (detail_selector, row_selector, hover_id) =
            (format!("superzed-source-detail-{id}"), format!("superzed-source-connection-{id}"), id.clone());
        // rientro delle righe sotto: sotto il nome (pallino + spazio)
        let indent = status_dot_size() + rems(0.375);
        v_flex()
            .id(SharedString::from(format!("superzed-source-row-{id}")))
            .debug_selector(move || row_selector)
            .px_2()
            .py_1()
            .gap_0p5()
            .rounded_md()
            .hover(move |d| d.bg(hover_bg))
            .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                let next = if *over { Some(hover_id.clone()) } else { this.hovered.clone().filter(|h| *h != hover_id) };
                if this.hovered != next {
                    this.hovered = next;
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .gap_1p5()
                    .child(dot)
                    .child(
                        div()
                            .id(SharedString::from(format!("superzed-source-name-{id}")))
                            .flex_1()
                            .min_w_0()
                            .tooltip(Tooltip::text(row.name.clone()))
                            .child(Label::new(row.name.clone()).truncate()),
                    )
                    .children(status_pill)
                    .children(actions),
            )
            .child(
                v_flex()
                    .pl(indent)
                    .gap_0p5()
                    .child(
                        div()
                            .id(SharedString::from(detail_selector.clone()))
                            .debug_selector(move || detail_selector)
                            .min_w_0()
                            .tooltip(Tooltip::text(row.detail.clone()))
                            .child(Label::new(row.detail).size(LabelSize::Small).color(Color::Muted).buffer_font(cx).truncate()),
                    )
                    .children(error)
                    .children(used),
            )
    }
}

/// Menu «nuova connessione» (un tipo per voce) sul bottone `trigger`.
fn add_menu<T: PopoverTrigger + ButtonCommon>(id: &'static str, trigger: T, panel: WeakEntity<SourcesPanel>) -> PopoverMenu<ContextMenu> {
    PopoverMenu::new(id).trigger(trigger).menu(move |window, cx| {
        let panel = panel.clone();
        Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
            for kind in ConnectionKind::ALL {
                let panel = panel.clone();
                menu = menu.entry(kind.label(), None, move |window, cx| {
                    panel.update(cx, |p, cx| p.open_form(kind, None, window, cx)).ok();
                });
            }
            menu
        }))
    })
}

impl EventEmitter<PanelEvent> for SourcesPanel {}

impl Focusable for SourcesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SourcesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // store creato dopo il pannello (non succede nell'app, ma costa poco)
        if self.store_sub.is_none()
            && let Some(store) = ConnectionStore::global(cx)
        {
            self.store_sub = Some(cx.observe(&store, |_, _, cx| cx.notify()));
        }
        self.sync_canvas_sub(cx);
        self.sync_countdown(cx);
        let mut rows = self.rows(cx);
        let total = rows.len();
        let query = self.filter.read(cx).text(cx);
        rows.retain(|r| source_matches(r, &query));
        let panel = cx.entity().downgrade();
        let add = div().debug_selector(|| "superzed-sources-add".into()).child(add_menu(
            "superzed-sources-add-menu",
            IconButton::new("superzed-sources-add", IconName::Plus).icon_size(IconSize::Medium).tooltip(Tooltip::text("Add a source")),
            panel.clone(),
        ));
        let banner = self.banner(cx).map(|b| {
            div()
                .debug_selector(|| "superzed-sources-banner".into())
                .mx_1()
                .mb_1p5()
                .p_2()
                .rounded_sm()
                .bg(cx.theme().status().warning_background)
                .child(Label::new(b).size(LabelSize::Small))
                .child(
                    h_flex().pt_1().justify_end().child(
                        div().debug_selector(|| "superzed-sources-reload".into()).child(
                            Button::new("superzed-sources-reload", "Reload the file")
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(|p, _, _, cx| p.reload(cx))),
                        ),
                    ),
                )
        });
        let mut content: Vec<AnyElement> = Vec::new();
        if total == 0 {
            content.push(
                v_flex()
                    .debug_selector(|| "superzed-sources-empty".into())
                    .p_2()
                    .gap_1p5()
                    .items_start()
                    .child(Label::new("No sources yet").color(Color::Muted))
                    .child(div().debug_selector(|| "superzed-sources-empty-add".into()).child(add_menu(
                        "superzed-sources-empty-add-menu",
                        Button::new("superzed-sources-empty-add", "Add source")
                            .style(ButtonStyle::Filled)
                            .start_icon(Icon::new(IconName::Plus).size(IconSize::Small)),
                        panel,
                    )))
                    .into_any_element(),
            );
        } else if rows.is_empty() {
            content.push(
                div()
                    .debug_selector(|| "superzed-sources-no-match".into())
                    .p_2()
                    .child(Label::new(no_match_text(&query)).color(Color::Muted))
                    .into_any_element(),
            );
        }
        for group in group_rows(rows) {
            // con un filtro attivo i gruppi restano espansi (le righe che combaciano non
            // devono restare nascoste sotto un gruppo chiuso), senza toccare lo stato
            // salvato di collassato/aperto.
            let open = group_is_open(self.collapsed.contains(&group.kind), &query);
            let header = self.render_group_header(&group, open, cx);
            let mut body = v_flex().gap_px().child(header);
            if open {
                let rows: Vec<AnyElement> = group.rows.into_iter().map(|r| self.render_row(r, cx).into_any_element()).collect();
                body = body.child(v_flex().pl_2().pt_0p5().gap_px().children(rows));
            }
            content.push(body.into_any_element());
        }
        v_flex()
            .key_context("SuperzedSources")
            .track_focus(&self.focus)
            .size_full()
            .p_1()
            .child(panel_header("superzed-sources", "Sources", total, &self.filter, Some(add.into_any_element()), window, cx))
            .children(banner)
            .child(
                v_flex()
                    .id("superzed-sources-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_1()
                    .children(content),
            )
    }
}

impl Panel for SourcesPanel {
    fn persistent_name() -> &'static str {
        "SuperzedSourcesPanel"
    }
    fn panel_key() -> &'static str {
        "SuperzedSourcesPanel"
    }
    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Left
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Left
    }
    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}
    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(340.)
    }
    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Server)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Sources")
    }
    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(SidebarView(6))
    }
    fn activation_priority(&self) -> u32 {
        12
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superzed_sources::{ClickhouseConfig, Forward, GithubConfig, HyperdxConfig, TunnelConfig, ZabbixConfig};

    fn row(id: &str, name: &str, kind: ConnectionKind, detail: &str) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            name: name.into(),
            kind,
            detail: detail.into(),
            tunnel_status: None,
            used: None,
            error: None,
        }
    }

    fn conn(config: ConnectionConfig) -> Connection {
        Connection { id: "x".into(), name: "x".into(), config }
    }

    #[test]
    fn pill_text_and_color_follow_the_status() {
        assert_eq!(status_color(TunnelStatus::Open), Color::Success);
        assert_eq!(status_color(TunnelStatus::Opening), Color::Warning);
        assert_eq!(status_color(TunnelStatus::Failed), Color::Error);
        assert_eq!(status_color(TunnelStatus::Closed), Color::Muted);
        assert_eq!(status_text(TunnelStatus::Open), "Open");
        assert_eq!(status_text(TunnelStatus::Opening), "Opening…");
        assert_eq!(status_text(TunnelStatus::Failed), "Failed");
        assert_eq!(status_text(TunnelStatus::Closed), "Closed");
    }

    #[test]
    fn the_filter_matches_name_detail_and_kind_ignoring_case() {
        let gh = row("gh", "acme-app", ConnectionKind::Github, "repo acme/acme_app");
        let t = row("t", "box", ConnectionKind::SshTunnel, "host root@203.0.113.10");
        assert!(source_matches(&gh, ""), "filtro vuoto: tutto");
        assert!(source_matches(&gh, "  "), "solo spazi: tutto");
        assert!(source_matches(&gh, "APP"), "nome");
        assert!(source_matches(&gh, "acme_app"), "repo");
        assert!(source_matches(&t, " 203.0 "), "host, spazi ai lati ignorati");
        assert!(source_matches(&gh, "github"), "tipo");
        assert!(source_matches(&t, "ssh tun"), "tipo");
        assert!(!source_matches(&gh, "zabbix"));
        assert!(!source_matches(&t, "acme"));
    }

    #[test]
    fn rows_are_grouped_by_kind_in_a_fixed_order_without_empty_groups() {
        let rows = vec![
            row("t", "box", ConnectionKind::SshTunnel, ""),
            row("gh1", "a", ConnectionKind::Github, ""),
            row("ch", "ch", ConnectionKind::Clickhouse, ""),
            row("hx", "log", ConnectionKind::Hyperdx, ""),
            row("gh2", "b", ConnectionKind::Github, ""),
        ];
        let groups = group_rows(rows);
        assert_eq!(
            groups.iter().map(|g| g.kind).collect::<Vec<_>>(),
            [ConnectionKind::Github, ConnectionKind::Hyperdx, ConnectionKind::Clickhouse, ConnectionKind::SshTunnel],
            "ordine fisso, Zabbix vuoto nascosto"
        );
        assert_eq!(groups[0].rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["gh1", "gh2"], "ordine di creazione nel gruppo");
        assert!(group_rows(Vec::new()).is_empty());
    }

    #[test]
    fn a_failed_tunnel_says_what_went_wrong_and_what_happens_next() {
        let now = Instant::now();
        assert_eq!(tunnel_error(&TunnelState::Open, false, now), None);
        assert_eq!(tunnel_error(&TunnelState::Closed, false, now), None);
        let failed = TunnelState::Failed { stderr: "Warning: added key\nssh: connect refused.\n".into(), retry_at: now + Duration::from_secs(5) };
        let err = tunnel_error(&failed, false, now).unwrap();
        assert_eq!(err.summary, "ssh: connect refused. Retrying in 5 s");
        assert_eq!(err.full, "Warning: added key\nssh: connect refused.\nRetrying in 5 s");
        let err = tunnel_error(&failed, true, now).unwrap();
        assert_eq!(err.summary, "ssh: connect refused. Invalid tunnel configuration", "niente nuovi tentativi promessi");
        let silent = TunnelState::Failed { stderr: " ".into(), retry_at: now };
        assert_eq!(tunnel_error(&silent, false, now).unwrap().summary, "Retrying in 0 s");
    }

    #[test]
    fn the_detail_uses_short_labeled_fields() {
        let gh = GithubConfig { owner: Some("acme".into()), project_number: Some(3), repo: Some("acme/acme_app".into()), stale_days: None };
        assert_eq!(connection_detail(&conn(ConnectionConfig::Github(gh))), "repo acme/acme_app · project acme/3");
        let only_repo = GithubConfig { repo: Some("a/b".into()), ..Default::default() };
        assert_eq!(connection_detail(&conn(ConnectionConfig::Github(only_repo))), "repo a/b");
        assert_eq!(connection_detail(&conn(ConnectionConfig::Github(GithubConfig::default()))), "from .claude/claude-pm.local.md");
        let t = TunnelConfig { host: "root@h".into(), forwards: vec![Forward { local_port: 0, remote_host: "localhost".into(), remote_port: 8123 }], keep_open: false, idle_minutes: 10 };
        assert_eq!(connection_detail(&conn(ConnectionConfig::SshTunnel(t))), "host root@h · ports 0:localhost:8123");
        let http = ClickhouseConfig { transport: ClickhouseTransport::Http { url: "http://127.0.0.1:8123".into(), tunnel: Some("box".into()) }, user: None, password_ref: None, database: "default".into() };
        assert_eq!(connection_detail(&conn(ConnectionConfig::Clickhouse(http))), "http://127.0.0.1:8123 · tunnel box");
        let ssh = ClickhouseConfig { transport: ClickhouseTransport::SshExec { host: "root@h".into(), command: None }, user: None, password_ref: None, database: "default".into() };
        assert_eq!(connection_detail(&conn(ConnectionConfig::Clickhouse(ssh))), "ssh root@h");
        let hx = HyperdxConfig { clickhouse: "ch".into(), ui_url: None, namespace: Some("app".into()), environment: Some("prod".into()) };
        assert_eq!(connection_detail(&conn(ConnectionConfig::Hyperdx(hx))), "clickhouse ch · scope app/prod");
        let hx = HyperdxConfig { clickhouse: "ch".into(), ui_url: None, namespace: None, environment: None };
        assert_eq!(connection_detail(&conn(ConnectionConfig::Hyperdx(hx))), "clickhouse ch");
        let z = ZabbixConfig { url: "https://z".into(), token_ref: None, tunnel: Some("box".into()) };
        assert_eq!(connection_detail(&conn(ConnectionConfig::Zabbix(z))), "https://z · tunnel box");
    }

    #[test]
    fn no_match_text_quotes_the_query() {
        assert_eq!(no_match_text(" box "), "No sources match “box”");
    }
}
