//! Nodo Stato: attese, worktree e chat del progetto, letti dal canvas
//! (`canvas_view/status.rs`). Il nodo mostra e non salva niente.
use crate::age::short_age;
use crate::canvas_view::{CanvasView, ChatRow, ChatState, WaitingState};
use crate::model::NodeId;
use crate::node::NodeContent;
use crate::worktrees::{Head, Worktree};
use chrono::{Local, TimeZone as _};
use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    SharedString, Subscription, WeakEntity, Window, rems,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use superzed_done::WaitingItem;
use ui::{IconButton, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*, utils::WithRemSize};

fn seconds(delta: i64) -> Duration {
    Duration::from_secs(u64::try_from(delta).unwrap_or(0))
}

/// Chi si aspetta: «you» se e' l'utente di `gh`.
pub fn waiting_who(item: &WaitingItem) -> String {
    if item.mine {
        return "you".to_string();
    }
    item.waiting_on.clone().unwrap_or_else(|| "—".to_string())
}

pub fn worktree_summary(worktree: &Worktree) -> String {
    if worktree.missing {
        return "missing".to_string();
    }
    if worktree.merged() {
        return "already on main".to_string();
    }
    let mut parts = Vec::new();
    match (worktree.ahead, worktree.behind) {
        (Some(0), Some(0)) => {}
        (Some(ahead), Some(behind)) => parts.push(format!("+{ahead} −{behind}")),
        _ => {}
    }
    if worktree.dirty > 0 {
        parts.push(format!("{} dirty", worktree.dirty));
    }
    if parts.is_empty() { "clean".to_string() } else { parts.join(" · ") }
}

pub fn chat_label(row: &ChatRow) -> String {
    match (row.state, row.age) {
        (ChatState::NeedsReply, Some(age)) => format!("needs your reply · {}", short_age(age)),
        (ChatState::NeedsReply, None) => "needs your reply".to_string(),
        (ChatState::Idle, Some(age)) => format!("idle {}", short_age(age)),
        (ChatState::Idle, None) => "idle".to_string(),
        (ChatState::Running, _) => "running".to_string(),
        (ChatState::Failed, _) => "failed".to_string(),
    }
}

pub fn waiting_header(state: &WaitingState, now: i64) -> String {
    match state {
        WaitingState::NotLoaded => "WAITING".to_string(),
        WaitingState::Unavailable(reason) => format!("WAITING · not available: {reason}"),
        WaitingState::Fresh { items, .. } => format!("WAITING ({})", items.len()),
        WaitingState::Stale { read_at: None, error, .. } => format!("WAITING · not available: {error}"),
        WaitingState::Stale { items, read_at: Some(read_at), error } => {
            format!("WAITING ({}) · offline copy, {} old · {error}", items.len(), short_age(seconds(now - read_at)))
        }
    }
}

/// Nome di un worktree: l'ultimo segmento se sta nella cartella del progetto o fra i suoi
/// worktree (`<progetto>/.worktrees`, `<progetto>.worktrees`), altrimenti il percorso
/// intero con `~` al posto della home.
pub fn worktree_name(path: &Path, root: Option<&Path>, home: &Path) -> String {
    let near = root.is_some_and(|root| {
        let sibling = root.file_name().map(|name| {
            let mut name = name.to_os_string();
            name.push(".worktrees");
            root.with_file_name(name)
        });
        path.starts_with(root) || sibling.is_some_and(|sibling| path.starts_with(sibling))
    });
    if near && let Some(name) = path.file_name() {
        return name.to_string_lossy().into_owned();
    }
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn head_label(head: &Head) -> String {
    match head {
        Head::Branch(branch) => branch.clone(),
        Head::Detached(tag) => format!("detached {tag}"),
    }
}

/// Quello che il nodo legge dal canvas a ogni disegno.
struct Snapshot {
    waiting: WaitingState,
    /// Chat di ogni attesa (stesso ordine di `waiting.items()`), se e' ancora sul canvas.
    waiting_chats: Vec<Option<NodeId>>,
    worktrees: Result<Vec<Worktree>, String>,
    refreshing: bool,
    chats: Vec<ChatRow>,
    root: Option<PathBuf>,
    now: i64,
}

impl Snapshot {
    fn read(canvas: &CanvasView, cx: &App) -> Self {
        let waiting = canvas.status_waiting().clone();
        let waiting_chats = waiting
            .items()
            .iter()
            .map(|item| item.session.as_deref().and_then(|session| canvas.node_for_session(session, cx)))
            .collect();
        Self {
            waiting,
            waiting_chats,
            worktrees: canvas.status_worktrees().clone(),
            refreshing: canvas.status_refreshing(),
            chats: canvas.status_chat_rows(cx),
            root: canvas.project_root_path(cx),
            now: canvas.now().timestamp(),
        }
    }
}

pub struct StatusView {
    canvas: WeakEntity<CanvasView>,
    focus: FocusHandle,
    zoom: f32,
    _subscriptions: Vec<Subscription>,
}

impl StatusView {
    pub fn new(canvas: WeakEntity<CanvasView>, cx: &mut Context<Self>) -> Self {
        let subscriptions = canvas
            .upgrade()
            .map(|canvas| vec![cx.observe(&canvas, |_, _, cx| cx.notify())])
            .unwrap_or_default();
        Self { canvas, focus: cx.focus_handle(), zoom: 1.0, _subscriptions: subscriptions }
    }

    fn header(&self, snapshot: &Snapshot) -> AnyElement {
        let updated = match &snapshot.waiting {
            WaitingState::Fresh { read_at, .. } => {
                Local.timestamp_opt(*read_at, 0).single().map(|time| format!("updated {}", time.format("%H:%M")))
            }
            WaitingState::NotLoaded | WaitingState::Unavailable(_) | WaitingState::Stale { .. } => None,
        };
        let canvas = self.canvas.clone();
        h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .child(Label::new("STATUS").weight(FontWeight::BOLD))
            .child(div().flex_1())
            .children(updated.map(|text| Label::new(text).color(Color::Muted)))
            .child(
                div().debug_selector(|| "superzed-status-refresh".into()).child(
                    IconButton::new("superzed-status-refresh", IconName::RotateCw)
                        .icon_size(IconSize::Small)
                        .disabled(snapshot.refreshing)
                        .tooltip(Tooltip::text("Refresh"))
                        .on_click(move |_, _, cx| {
                            canvas.update(cx, |canvas, cx| canvas.refresh_status(cx)).ok();
                        }),
                ),
            )
            .into_any_element()
    }

    fn waiting_row(&self, item: &WaitingItem, chat: Option<NodeId>, now: i64, cx: &App) -> AnyElement {
        let number = item.number;
        let url = item.url.clone();
        let hover = cx.theme().colors().element_hover;
        let go_to_chat = chat.map(|node| {
            let canvas = self.canvas.clone();
            div().debug_selector(move || format!("superzed-status-go-to-chat-{number}")).child(
                IconButton::new(SharedString::from(format!("superzed-status-go-to-chat-{number}")), IconName::Chat)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Go to chat"))
                    .on_click(move |_, window, cx| {
                        canvas.update(cx, |canvas, cx| canvas.focus_node(node, window, cx)).ok();
                    }),
            )
        });
        let close = {
            let canvas = self.canvas.clone();
            div().debug_selector(move || format!("superzed-status-close-{number}")).child(
                IconButton::new(SharedString::from(format!("superzed-status-close-{number}")), IconName::Close)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Close"))
                    .on_click(move |_, window, cx| {
                        canvas.update(cx, |canvas, cx| canvas.close_waiting_item_with_confirm(number, window, cx)).ok();
                    }),
            )
        };
        let (dot, dot_color) = if item.mine { ("●", Color::Warning) } else { ("○", Color::Muted) };
        h_flex()
            .debug_selector(move || format!("superzed-status-waiting-{number}"))
            .id(("superzed-status-waiting", number as usize))
            .px_2()
            .gap_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child(Label::new(dot).color(dot_color))
            .child(div().flex_none().w(rems(7.)).min_w_0().child(Label::new(waiting_who(item)).truncate()))
            .child(div().flex_1().min_w_0().child(Label::new(item.title.clone()).truncate()))
            .child(Label::new(short_age(seconds(now - item.created_at))).color(Color::Muted))
            .child(Label::new(format!("#{number}")).color(Color::Muted))
            .children(go_to_chat)
            .child(close)
            .into_any_element()
    }

    fn chat_row(&self, row: &ChatRow, cx: &App) -> AnyElement {
        let node = row.node;
        let canvas = self.canvas.clone();
        let hover = cx.theme().colors().element_hover;
        let (dot, dot_color) = match row.state {
            ChatState::NeedsReply => ("●", Color::Warning),
            ChatState::Failed => ("●", Color::Error),
            ChatState::Running => ("●", Color::Accent),
            ChatState::Idle => ("○", Color::Muted),
        };
        h_flex()
            .debug_selector(move || format!("superzed-status-chat-{}", node.0))
            .id(("superzed-status-chat", node.0 as usize))
            .px_2()
            .gap_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(move |_, window, cx| {
                canvas.update(cx, |canvas, cx| canvas.focus_node(node, window, cx)).ok();
            })
            .child(Label::new(dot).color(dot_color))
            .child(div().flex_none().w(rems(12.)).min_w_0().child(Label::new(chat_label(row)).truncate()))
            .child(div().flex_1().min_w_0().child(Label::new(format!("«{}»", row.title)).truncate()))
            .into_any_element()
    }

    fn sections(&self, snapshot: Snapshot, cx: &App) -> AnyElement {
        let now = snapshot.now;
        let waiting_rows: Vec<AnyElement> = snapshot
            .waiting
            .items()
            .iter()
            .zip(snapshot.waiting_chats.iter())
            .map(|(item, chat)| self.waiting_row(item, *chat, now, cx))
            .collect();
        let no_waiting = matches!(snapshot.waiting, WaitingState::Fresh { ref items, .. } if items.is_empty());
        let worktree_rows: Vec<AnyElement> = match &snapshot.worktrees {
            Ok(worktrees) => worktrees
                .iter()
                .map(|worktree| worktree_row(worktree, snapshot.root.as_deref(), now))
                .collect(),
            Err(reason) => vec![note(format!("not available: {reason}"))],
        };
        let chat_rows: Vec<AnyElement> = snapshot.chats.iter().map(|row| self.chat_row(row, cx)).collect();
        let no_chats = chat_rows.is_empty();
        v_flex()
            .id("superzed-status-body")
            .flex_1()
            .overflow_y_scroll()
            .px_1()
            .pb_2()
            .gap_0p5()
            .child(section_title(waiting_header(&snapshot.waiting, now)))
            .when(no_waiting, |list| list.child(note("Nothing to wait for".to_string())))
            .children(waiting_rows)
            .child(section_title("WORKTREES".to_string()))
            .children(worktree_rows)
            .child(section_title("CHATS".to_string()))
            .when(no_chats, |list| list.child(note("No chats".to_string())))
            .children(chat_rows)
            .into_any_element()
    }
}

fn section_title(text: String) -> AnyElement {
    div()
        .px_2()
        .pt_2()
        .child(Label::new(text).size(LabelSize::Small).weight(FontWeight::BOLD).color(Color::Muted).truncate())
        .into_any_element()
}

fn note(text: String) -> AnyElement {
    div().px_2().child(Label::new(text).color(Color::Muted)).into_any_element()
}

fn worktree_row(worktree: &Worktree, root: Option<&Path>, now: i64) -> AnyElement {
    let name = worktree_name(&worktree.path, root, util::paths::home_dir());
    h_flex()
        .px_2()
        .gap_2()
        .child(div().flex_none().w(rems(10.)).min_w_0().child(Label::new(name).truncate()))
        .child(div().flex_none().w(rems(12.)).min_w_0().child(Label::new(head_label(&worktree.head)).color(Color::Muted).truncate()))
        .child(div().flex_1().min_w_0().child(Label::new(worktree_summary(worktree)).truncate()))
        .children(worktree.last_commit.map(|time| Label::new(short_age(seconds(now - time))).color(Color::Muted)))
        .into_any_element()
}

impl Focusable for StatusView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for StatusView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.canvas.upgrade().map(|canvas| Snapshot::read(canvas.read(cx), cx));
        let colors = cx.theme().colors();
        let (header, sections) = match snapshot {
            Some(snapshot) => (Some(self.header(&snapshot)), Some(self.sections(snapshot, cx))),
            None => (None, None),
        };
        WithRemSize::new(window.rem_size() * self.zoom).size_full().child(
            v_flex()
                .key_context("SuperzedStatus")
                .track_focus(&self.focus)
                .size_full()
                .bg(colors.panel_background)
                .children(header)
                .children(sections),
        )
    }
}

pub struct StatusNode {
    view: Entity<StatusView>,
}

impl StatusNode {
    pub fn new(canvas: WeakEntity<CanvasView>, cx: &mut App) -> Self {
        Self { view: cx.new(|cx| StatusView::new(canvas, cx)) }
    }
}

impl NodeContent for StatusNode {
    fn view(&self) -> AnyView {
        self.view.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }

    fn set_font_zoom(&self, zoom: f32, cx: &mut App) {
        self.view.update(cx, |view, cx| {
            view.zoom = zoom;
            cx.notify();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(number: u64, mine: bool, waiting_on: Option<&str>) -> WaitingItem {
        WaitingItem {
            number,
            title: format!("item {number}"),
            waiting_on: waiting_on.map(str::to_string),
            mine,
            created_at: 0,
            url: format!("https://github.com/acme/acme_app/issues/{number}"),
            session: None,
        }
    }

    fn three() -> Vec<WaitingItem> {
        vec![item(212, true, Some("Alice (owner)")), item(208, true, None), item(171, false, Some("Ops team"))]
    }

    /// Worktree principale (o secondario) su un ramo.
    fn worktree(dirty: usize, ahead: Option<u32>, behind: Option<u32>, secondary: bool, missing: bool) -> Worktree {
        Worktree {
            path: PathBuf::from("/home/alice/acme_app"),
            head: Head::Branch("main".to_string()),
            main: !secondary,
            missing,
            dirty,
            ahead,
            behind,
            last_commit: None,
        }
    }

    fn merged_worktree() -> Worktree {
        Worktree {
            path: PathBuf::from("/home/alice/acme_app/.worktrees/search-filters"),
            head: Head::Branch("feat/search-filters".to_string()),
            ..worktree(0, Some(0), Some(5), true, false)
        }
    }

    fn hours(count: u64) -> Duration {
        Duration::from_secs(count * 60 * 60)
    }

    fn days(count: u64) -> Duration {
        hours(count * 24)
    }

    fn row(state: ChatState, age: Option<Duration>) -> ChatRow {
        ChatRow { node: NodeId(1), title: "search filters".to_string(), state, age }
    }

    #[test]
    fn row_texts() {
        assert_eq!(waiting_who(&item(212, true, Some("Alice (owner)"))), "you");
        assert_eq!(waiting_who(&item(171, false, Some("Ops team"))), "Ops team");
        assert_eq!(waiting_who(&item(9, false, None)), "—");
        assert_eq!(worktree_summary(&worktree(0, Some(0), Some(0), false, false)), "clean");
        assert_eq!(worktree_summary(&worktree(4, Some(12), Some(3), false, false)), "+12 −3 · 4 dirty");
        assert_eq!(worktree_summary(&merged_worktree()), "already on main");
        assert_eq!(worktree_summary(&worktree(0, None, None, false, true)), "missing");
        assert_eq!(chat_label(&row(ChatState::NeedsReply, Some(hours(2)))), "needs your reply · 2h");
        assert_eq!(chat_label(&row(ChatState::Idle, Some(days(6)))), "idle 6d");
        assert_eq!(chat_label(&row(ChatState::Running, None)), "running");
        assert_eq!(chat_label(&row(ChatState::Failed, None)), "failed");
        assert_eq!(waiting_header(&WaitingState::Fresh { items: three(), read_at: 0 }, 0), "WAITING (3)");
        assert_eq!(
            waiting_header(&WaitingState::Unavailable("no GitHub remote: origin is not configured".into()), 0),
            "WAITING · not available: no GitHub remote: origin is not configured"
        );
        assert!(
            waiting_header(&WaitingState::Stale { items: three(), read_at: Some(0), error: "HTTP 502".into() }, 600)
                .starts_with("WAITING (3) · offline copy, 10m old · HTTP 502")
        );
        assert_eq!(
            waiting_header(&WaitingState::Stale { items: Vec::new(), read_at: None, error: "HTTP 502".into() }, 1_795_000_000),
            "WAITING · not available: HTTP 502",
            "senza copia locale: niente conteggio ne' eta'"
        );
    }

    #[test]
    fn worktree_names_are_short_near_the_project_and_from_home_elsewhere() {
        let root = Path::new("/home/alice/acme_app");
        let home = Path::new("/home/alice");
        assert_eq!(worktree_name(root, Some(root), home), "acme_app");
        assert_eq!(worktree_name(Path::new("/home/alice/acme_app/.worktrees/import-fix"), Some(root), home), "import-fix");
        assert_eq!(worktree_name(Path::new("/home/alice/acme_app.worktrees/import-fix"), Some(root), home), "import-fix");
        assert_eq!(worktree_name(Path::new("/home/alice/release"), Some(root), home), "~/release");
        assert_eq!(worktree_name(Path::new("/srv/acme_app2"), Some(root), home), "/srv/acme_app2");
        assert_eq!(worktree_name(Path::new("/home/alice/release"), None, home), "~/release");
    }
}
