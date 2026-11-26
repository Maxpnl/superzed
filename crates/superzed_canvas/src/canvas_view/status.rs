//! Stato del progetto per il nodo Stato: attese (issue `blocker` su GitHub, con la copia
//! locale in `.superzed/waiting.json`), worktree e righe delle chat. Si rilegge ogni
//! `STATUS_REFRESH`; le attese chieste da una chat (`superzed-wait`) arrivano qui.
use super::*;
use crate::worktrees::{Worktree, read_worktrees};
use collections::HashSet;
use std::path::Path;
use superzed_done::{DoneEnv, WAITING_FILE, WaitOn, WaitingItem};
use superzed_ipc::IpcReply;
use superzed_notifications::{NewNotification, NotificationStore, Severity};
use superzed_sessions::SessionState;
use superzed_sources::Runner;
use superzed_sources::github::PmConfig;

pub const STATUS_REFRESH: Duration = Duration::from_secs(60);
const STATUS_SIZE: (f32, f32) = (720., 520.);
/// Sorgente delle notifiche delle attese.
pub const WAITING_SOURCE: &str = "Waiting";

#[derive(Clone, Debug, PartialEq)]
pub enum WaitingState {
    NotLoaded,
    /// Nessun remote GitHub (o nessuna cartella): il motivo.
    Unavailable(String),
    Fresh { items: Vec<WaitingItem>, read_at: i64 },
    /// `gh` non risponde: la copia locale, con l'errore.
    Stale { items: Vec<WaitingItem>, read_at: i64, error: String },
}

impl WaitingState {
    pub fn items(&self) -> &[WaitingItem] {
        match self {
            WaitingState::Fresh { items, .. } | WaitingState::Stale { items, .. } => items,
            WaitingState::NotLoaded | WaitingState::Unavailable(_) => &[],
        }
    }

    fn items_mut(&mut self) -> Option<&mut Vec<WaitingItem>> {
        match self {
            WaitingState::Fresh { items, .. } | WaitingState::Stale { items, .. } => Some(items),
            WaitingState::NotLoaded | WaitingState::Unavailable(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChatState {
    NeedsReply,
    Failed,
    Running,
    Idle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatRow {
    pub node: NodeId,
    pub title: String,
    pub state: ChatState,
    pub age: Option<Duration>,
}

pub fn chat_state(state: &SessionState) -> ChatState {
    match state {
        SessionState::NeedsInput(_) | SessionState::Done => ChatState::NeedsReply,
        SessionState::Failed(_) => ChatState::Failed,
        SessionState::Working | SessionState::Starting => ChatState::Running,
        SessionState::Idle | SessionState::Ended => ChatState::Idle,
    }
}

pub(crate) struct StatusState {
    waiting: WaitingState,
    worktrees: Result<Vec<Worktree>, String>,
    refreshing: bool,
    /// Login di `gh`, letto al primo giro che ci riesce e poi tenuto.
    user: Option<String>,
    /// Attese aperte da questo canvas, per id di sessione: valgono anche prima che la
    /// lista letta da GitHub le contenga.
    opened: HashMap<String, Vec<OpenedWait>>,
    /// Numero dell'ultimo giro partito.
    refresh_sequence: u64,
    /// Voci chiuse ma non ancora confermate da `gh`: un giro partito prima della chiusura
    /// non le rimette nella lista.
    closing: HashSet<u64>,
    refresh: Task<()>,
    timer: Option<Task<()>>,
}

impl Default for StatusState {
    fn default() -> Self {
        Self {
            waiting: WaitingState::NotLoaded,
            worktrees: Ok(Vec::new()),
            refreshing: false,
            user: None,
            opened: HashMap::default(),
            refresh_sequence: 0,
            closing: HashSet::default(),
            refresh: Task::ready(()),
            timer: None,
        }
    }
}

struct OpenedWait {
    number: u64,
    title: String,
    /// `refresh_sequence` all'apertura: solo un giro partito dopo puo' dire che la issue
    /// non e' piu' aperta (uno partito prima l'ha letta quando forse non c'era ancora).
    after_refresh: u64,
}

struct PreparedWait {
    runner: Arc<dyn Runner>,
    config: PmConfig,
    session_id: String,
    request: superzed_done::WaitRequest,
}

struct StatusRead {
    waiting: WaitingState,
    user: Option<String>,
    worktrees: Result<Vec<Worktree>, String>,
}

fn write_waiting_file(path: &Path, items: &[WaitingItem], read_at: i64) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    superzed_done::write_atomic(path, &superzed_done::waiting_file_json(items, read_at))
}

fn read_waiting_file(path: &Path) -> Option<(Vec<WaitingItem>, i64)> {
    superzed_done::parse_waiting_file(&std::fs::read_to_string(path).ok()?)
}

/// Un giro, fuori dal thread della UI: worktree sempre, attese solo con il remote GitHub.
async fn read_status(runner: Arc<dyn Runner>, root: PathBuf, config: Result<PmConfig, String>, user: Option<String>) -> StatusRead {
    let worktrees = read_worktrees(&runner, &root).await;
    let config = match config {
        Ok(config) => config,
        Err(error) => return StatusRead { waiting: WaitingState::Unavailable(error), user, worktrees },
    };
    let user = match user {
        Some(user) => Some(user),
        None => superzed_done::gh_user(&runner).await.ok(),
    };
    let path = root.join(WAITING_FILE);
    let waiting = match superzed_done::read_waiting(&runner, &config.repo, user.as_deref()).await {
        Ok(items) => {
            let read_at = chrono::Utc::now().timestamp();
            if let Err(error) = write_waiting_file(&path, &items, read_at) {
                log::warn!("superzed status: local copy of the waiting items not written: {error}");
            }
            WaitingState::Fresh { items, read_at }
        }
        Err(error) => {
            let (items, read_at) = read_waiting_file(&path).unwrap_or_default();
            WaitingState::Stale { items, read_at, error }
        }
    };
    StatusRead { waiting, user, worktrees }
}

impl CanvasView {
    pub fn status_node(&self) -> Option<NodeId> {
        self.model.frames().iter().find(|frame| frame.kind == NodeKind::Status).map(|frame| frame.id)
    }

    /// Un solo nodo Stato per canvas: lo crea (e avvia le letture) se manca, poi gli da' il focus.
    pub fn open_status(&mut self, window: &mut Window, cx: &mut Context<Self>) -> NodeId {
        let id = match self.status_node() {
            Some(id) => id,
            None => {
                let node = crate::status_node::StatusNode::new(cx.entity().downgrade(), cx);
                let id = self.add_node(NodeKind::Status, Box::new(node), size(STATUS_SIZE.0, STATUS_SIZE.1), "status", cx);
                self.start_status_refresh(cx);
                id
            }
        };
        self.focus_node(id, window, cx);
        id
    }

    pub fn status_waiting(&self) -> &WaitingState {
        &self.status.waiting
    }

    pub fn status_worktrees(&self) -> &Result<Vec<Worktree>, String> {
        &self.status.worktrees
    }

    pub fn status_refreshing(&self) -> bool {
        self.status.refreshing
    }

    /// Chat claude del canvas (non le automazioni): prima chi aspetta l'utente, poi le
    /// fallite, quelle al lavoro e le ferme; a parita' la piu' vecchia prima.
    pub fn status_chat_rows(&self, cx: &App) -> Vec<ChatRow> {
        let Some(store) = superzed_sessions::SessionStore::global(cx) else {
            return Vec::new();
        };
        let store = store.read(cx);
        let now = self.now().timestamp();
        let mut rows: Vec<ChatRow> = self
            .model
            .frames()
            .iter()
            .filter(|frame| frame.kind == NodeKind::Claude && frame.dock != Some(DockKind::Automations))
            .filter(|frame| !self.automations.contains_key(&frame.id))
            .filter_map(|frame| {
                let session = store.get(self.session_key(frame.id)?)?;
                let state = chat_state(&session.state);
                let age = match state {
                    ChatState::NeedsReply => Some(session.state_changed_at.elapsed()),
                    ChatState::Failed | ChatState::Running | ChatState::Idle => session
                        .times
                        .replied_at
                        .or(session.times.opened_at)
                        .map(|then| Duration::from_secs(u64::try_from(now - then).unwrap_or(0))),
                };
                Some(ChatRow { node: frame.id, title: self.node_title(frame, cx), state, age })
            })
            .collect();
        rows.sort_by_key(|row| (row.state, std::cmp::Reverse(row.age)));
        rows
    }

    /// Un giro di lettura in background; non parte se ce n'e' gia' uno in corso.
    pub fn refresh_status(&mut self, cx: &mut Context<Self>) {
        if self.status.refreshing {
            return;
        }
        let Some(env) = DoneEnv::get(cx) else {
            return;
        };
        let Some(root) = env.root(self.project_root_path(cx)) else {
            let reason = "the project has no folder".to_string();
            self.status.waiting = WaitingState::Unavailable(reason.clone());
            self.status.worktrees = Err(reason);
            cx.notify();
            return;
        };
        let config = superzed_done::config_for(&root);
        let runner = env.gh.clone();
        let user = self.status.user.clone();
        self.status.refreshing = true;
        self.status.refresh_sequence += 1;
        let sequence = self.status.refresh_sequence;
        cx.notify();
        self.status.refresh = cx.spawn(async move |this, cx| {
            let read = cx.background_spawn(read_status(runner, root, config, user)).await;
            this.update(cx, |canvas, cx| canvas.status_read(read, sequence, cx)).ok();
        });
    }

    fn status_read(&mut self, read: StatusRead, sequence: u64, cx: &mut Context<Self>) {
        let StatusRead { mut waiting, user, worktrees } = read;
        // chiusa su GitHub (o altrove): la lista fresca non la contiene piu'
        if let WaitingState::Fresh { items, .. } = &waiting {
            for waits in self.status.opened.values_mut() {
                waits.retain(|wait| wait.after_refresh >= sequence || items.iter().any(|item| item.number == wait.number));
            }
            self.status.opened.retain(|_, waits| !waits.is_empty());
        }
        if let Some(items) = waiting.items_mut() {
            items.retain(|item| !self.status.closing.contains(&item.number));
        }
        self.status.refreshing = false;
        if user.is_some() {
            self.status.user = user;
        }
        self.status.waiting = waiting;
        self.status.worktrees = worktrees;
        cx.notify();
    }

    /// Un giro subito, poi uno ogni `STATUS_REFRESH` finche' il canvas c'e'.
    pub fn start_status_refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_status(cx);
        self.status.timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STATUS_REFRESH).await;
                if this.update(cx, |canvas, cx| canvas.refresh_status(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    pub fn stop_status_refresh(&mut self) {
        self.status.timer = None;
    }

    /// Chiude la issue dell'attesa: la riga sparisce subito; se `gh` fallisce torna con
    /// il giro successivo e arriva una notifica d'errore.
    pub fn close_waiting_item(&mut self, number: u64, cx: &mut Context<Self>) {
        let Some(env) = DoneEnv::get(cx) else {
            return;
        };
        let config = env
            .root(self.project_root_path(cx))
            .ok_or_else(|| "the project has no folder".to_string())
            .and_then(|root| superzed_done::config_for(&root));
        let config = match config {
            Ok(config) => config,
            Err(error) => {
                self.status_notify(Severity::Error, format!("Waiting item #{number} not closed"), error, cx);
                return;
            }
        };
        if let Some(items) = self.status.waiting.items_mut() {
            items.retain(|item| item.number != number);
        }
        self.status.closing.insert(number);
        cx.notify();
        let runner = env.gh.clone();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { superzed_done::close_waiting(&runner, &config, number).await }).await;
            this.update(cx, |canvas, cx| {
                canvas.status.closing.remove(&number);
                match result {
                    Ok(()) => {
                        for waits in canvas.status.opened.values_mut() {
                            waits.retain(|wait| wait.number != number);
                        }
                        canvas.status.opened.retain(|_, waits| !waits.is_empty());
                    }
                    Err(error) => {
                        canvas.status_notify(Severity::Error, format!("Waiting item #{number} not closed"), error, cx);
                        canvas.refresh_status(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// «Close» di una riga d'attesa: chiude la issue solo dopo la conferma.
    pub fn close_waiting_item_with_confirm(&mut self, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let canvas = cx.entity().downgrade();
        workspace.update(cx, |ws, cx| {
            workspace::superzed_confirm::superzed_confirm_action(
                ws,
                format!("Close issue #{number}?"),
                "Close",
                true,
                move |_, cx| {
                    canvas.update(cx, |canvas, cx| canvas.close_waiting_item(number, cx)).ok();
                },
                window,
                cx,
            )
        });
    }

    pub fn node_for_session(&self, session_id: &str, cx: &App) -> Option<NodeId> {
        let store = superzed_sessions::SessionStore::global(cx)?;
        let store = store.read(cx);
        self.sessions
            .iter()
            .find(|(_, session)| store.get(&session.key).and_then(|s| s.session_id.as_deref()) == Some(session_id))
            .map(|(id, _)| *id)
    }

    /// Attese aperte della sessione: quelle aperte da questo canvas e quelle della lista
    /// letta con quella sessione, una volta sola per numero.
    pub fn session_waits(&self, session_id: &str) -> Vec<(u64, String)> {
        let mut waits: Vec<(u64, String)> = self
            .status
            .opened
            .get(session_id)
            .map(|opened| opened.iter().map(|wait| (wait.number, wait.title.clone())).collect())
            .unwrap_or_default();
        for item in self.status.waiting.items() {
            if item.session.as_deref() == Some(session_id) && !waits.iter().any(|(number, _)| *number == item.number) {
                waits.push((item.number, item.title.clone()));
            }
        }
        waits
    }

    /// Attesa chiesta da una chat (`superzed-wait`); `false` se la chiave non e' di una
    /// chat di questo canvas (la risposta resta a chi prova il canvas successivo).
    /// Altrimenti risponde sempre con una riga: `ok #<numero> <url>` o `error: <motivo>`.
    pub fn request_wait(&mut self, key: &str, payload: serde_json::Value, reply: IpcReply, cx: &mut Context<Self>) -> bool {
        if self.node_for_session_key(key).is_none() {
            return false;
        }
        let PreparedWait { runner, config, session_id, request } = match self.prepare_wait(key, &payload, cx) {
            Ok(prepared) => prepared,
            Err(error) => {
                reply.send(&format!("error: {error}"));
                return true;
            }
        };
        cx.spawn(async move |this, cx| {
            let opened = cx.background_spawn({
                let session_id = session_id.clone();
                let request = request.clone();
                async move { superzed_done::open_wait(&runner, &config, &session_id, &request).await }
            });
            match opened.await {
                Ok(issue) => {
                    reply.send(&format!("ok #{} {}", issue.number, issue.url));
                    this.update(cx, |canvas, cx| canvas.wait_opened(session_id, issue, request, cx)).ok();
                }
                Err(error) => reply.send(&format!("error: {error}")),
            }
        })
        .detach();
        true
    }

    fn prepare_wait(&self, key: &str, payload: &serde_json::Value, cx: &App) -> Result<PreparedWait, String> {
        let request = superzed_done::parse_wait_request(payload)?;
        let session_id = superzed_sessions::SessionStore::global(cx)
            .and_then(|store| store.read(cx).get(key).and_then(|session| session.session_id.clone()))
            .ok_or_else(|| "the chat has no session yet, retry in a moment".to_string())?;
        let env = DoneEnv::get(cx).ok_or_else(|| "superzed cannot take waits right now".to_string())?;
        let root = env.root(self.project_root_path(cx)).ok_or_else(|| "the project has no folder".to_string())?;
        let config = superzed_done::config_for(&root)?;
        Ok(PreparedWait { runner: env.gh.clone(), config, session_id, request })
    }

    fn wait_opened(
        &mut self,
        session_id: String,
        issue: superzed_done::FoundIssue,
        request: superzed_done::WaitRequest,
        cx: &mut Context<Self>,
    ) {
        let after_refresh = self.status.refresh_sequence;
        let waits = self.status.opened.entry(session_id).or_default();
        if !waits.iter().any(|wait| wait.number == issue.number) {
            waits.push(OpenedWait { number: issue.number, title: request.title.clone(), after_refresh });
        }
        if request.on == WaitOn::Me {
            self.status_notify(Severity::Warning, format!("Waiting on you: {}", request.title), issue.url, cx);
        }
        self.refresh_status(cx);
    }

    fn status_notify(&self, severity: Severity, title: String, summary: String, cx: &mut Context<Self>) {
        let Some(store) = NotificationStore::global(cx) else {
            return;
        };
        let notification = NewNotification {
            severity,
            title,
            summary,
            details: None,
            suggested_prompt: None,
            project_root: self.project_root_path(cx),
            source: WAITING_SOURCE.into(),
        };
        store.update(cx, |store, cx| {
            store.add(notification, cx);
        });
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::done::tests::notifications;
    use super::super::tests::{capture_spec, close_all, hook, store};
    use super::*;
    use crate::test_support::workspace_with_repo;
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    use superzed_done::test_support::write_git_origin;
    use superzed_sessions::{InputKind, Phase, SessionTimes};
    use superzed_sources::test_support::{FakeRunner, fail, ok};

    const LIST: &str = r#"[
 {"number":171,"title":"SECSTORE on CAR","body":"Waiting on: Basis\n\nWebGUI returns 500","createdAt":"2026-11-09T10:00:00Z","url":"https://github.com/acme/acme_app/issues/171","assignees":[]},
 {"number":212,"title":"Vault password","body":"Waiting on: Alice (owner)\n\nx\n\n<!-- superzed:session=s-1:wait -->","createdAt":"2026-11-23T10:00:00Z","url":"https://github.com/acme/acme_app/issues/212","assignees":[{"login":"alice"}]},
 {"number":208,"title":"Go-ahead to merge","body":"no first line\n<!-- superzed:session=s-2:item=3 -->","createdAt":"2026-11-21T10:00:00Z","url":"https://github.com/acme/acme_app/issues/208","assignees":[{"login":"alice"},{"login":"bob"}]}]"#;

    const OPEN_212: &str = r#"[{"number":212,"title":"Vault password","body":"Waiting on: owner","createdAt":"2026-11-23T10:00:00Z","url":"https://github.com/acme/acme_app/issues/212","assignees":[]}]"#;

    const WORKTREE_LIST: &str = "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n";

    pub(crate) struct Setup {
        pub(crate) canvas: Entity<CanvasView>,
        pub(crate) dir: tempfile::TempDir,
        pub(crate) gh: Arc<FakeRunner>,
    }

    /// Canvas con la root della pipeline in un tempdir (con o senza origin) e un `gh` finto
    /// che risponde a utente, lista delle attese e lista dei worktree.
    pub(crate) async fn setup(cx: &mut TestAppContext, origin: bool) -> (Setup, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        if origin {
            write_git_origin(dir.path());
        } else {
            std::fs::create_dir_all(dir.path().join(".git")).unwrap();
            std::fs::write(dir.path().join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        }
        let (_workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let gh = FakeRunner::new();
        gh.on("gh api user", ok("alice\n"));
        gh.on("gh issue list", ok(LIST));
        gh.on("worktree list --porcelain", ok(WORKTREE_LIST));
        cx.update(|_, cx| superzed_done::init_test(cx, gh.clone(), FakeRunner::new(), Some(dir.path().to_path_buf())));
        (Setup { canvas, dir, gh }, cx)
    }

    fn refresh(setup: &Setup, cx: &mut VisualTestContext) {
        setup.canvas.update(cx, |c, cx| c.refresh_status(cx));
        cx.run_until_parked();
    }

    fn waiting(setup: &Setup, cx: &mut VisualTestContext) -> WaitingState {
        setup.canvas.read_with(cx, |c, _| c.status_waiting().clone())
    }

    fn numbers(items: &[WaitingItem]) -> Vec<u64> {
        items.iter().map(|item| item.number).collect()
    }

    /// Chat claude con quella chiave e (forse) quell'id di sessione, PTY di cattura.
    fn chat(setup: &Setup, key: &str, session_id: Option<&str>, times: SessionTimes, cx: &mut VisualTestContext) -> NodeId {
        let init = NodeSessionInit {
            key: key.into(),
            account: "a".into(),
            account_id: None,
            session_id: session_id.map(str::to_string),
            phase: Phase::New,
            times,
        };
        let spec = capture_spec(&setup.dir.path().join(format!("pty-{key}.txt")));
        setup.canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(NodeKind::Claude, None, spec, Some(init), window, cx);
        });
        cx.run_until_parked();
        setup.canvas.read_with(cx, |c, _| c.node_for_session_key(key).expect("chat creata"))
    }

    fn reply_pair() -> (IpcReply, UnixStream) {
        let (server_side, client_side) = UnixStream::pair().unwrap();
        client_side.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        (IpcReply::new(server_side), client_side)
    }

    fn answer(mut client: UnixStream) -> String {
        let mut text = String::new();
        client.read_to_string(&mut text).unwrap();
        text
    }

    /// `request_wait` sul canvas; la risposta (vuota se non ne e' arrivata nessuna) e il valore.
    fn wait(setup: &Setup, key: &str, payload: serde_json::Value, cx: &mut VisualTestContext) -> (bool, String) {
        let (reply, client) = reply_pair();
        let mine = setup.canvas.update(cx, |c, cx| c.request_wait(key, payload, reply, cx));
        cx.run_until_parked();
        (mine, answer(client))
    }

    #[gpui::test]
    async fn a_refresh_reads_waiting_and_worktrees_and_writes_the_local_copy(cx: &mut TestAppContext) {
        let (setup, cx) = setup(cx, true).await;
        refresh(&setup, cx);
        let WaitingState::Fresh { items, read_at } = waiting(&setup, cx) else {
            panic!("atteso Fresh: {:?}", waiting(&setup, cx));
        };
        assert_eq!(numbers(&items), [208, 212, 171], "mie prima (utente di gh: alice), poi le piu' vecchie");
        let worktrees = setup.canvas.read_with(cx, |c, _| c.status_worktrees().clone()).unwrap();
        assert_eq!(worktrees.len(), 1);
        assert!(!setup.canvas.read_with(cx, |c, _| c.status_refreshing()));
        let text = std::fs::read_to_string(setup.dir.path().join(WAITING_FILE)).expect("copia locale scritta");
        assert_eq!(superzed_done::parse_waiting_file(&text), Some((items, read_at)));
    }

    #[gpui::test]
    async fn gh_down_shows_the_local_copy_with_the_error_and_no_notification(cx: &mut TestAppContext) {
        let (setup, cx) = setup(cx, true).await;
        refresh(&setup, cx);
        let WaitingState::Fresh { items: fresh, .. } = waiting(&setup, cx) else {
            panic!("primo giro riuscito");
        };
        setup.gh.on("gh issue list", fail(1, "HTTP 502: bad gateway"));
        refresh(&setup, cx);
        let WaitingState::Stale { items, error, .. } = waiting(&setup, cx) else {
            panic!("atteso Stale: {:?}", waiting(&setup, cx));
        };
        assert_eq!(items, fresh, "la copia locale");
        assert!(error.contains("502"), "{error}");

        // senza nessun giro riuscito e senza file
        std::fs::remove_file(setup.dir.path().join(WAITING_FILE)).unwrap();
        setup.canvas.update(cx, |c, _| c.status = StatusState::default());
        refresh(&setup, cx);
        let WaitingState::Stale { items, error, .. } = waiting(&setup, cx) else {
            panic!("atteso Stale: {:?}", waiting(&setup, cx));
        };
        assert_eq!(items, vec![]);
        assert!(error.contains("502"), "{error}");
        assert!(notifications(cx).is_empty(), "{:?}", notifications(cx));
    }

    #[gpui::test]
    async fn no_github_remote_makes_waiting_unavailable_but_worktrees_still_load(cx: &mut TestAppContext) {
        let (setup, cx) = setup(cx, false).await;
        refresh(&setup, cx);
        assert_eq!(waiting(&setup, cx), WaitingState::Unavailable("no GitHub remote: origin is not configured".into()));
        assert_eq!(setup.gh.count("gh "), 0);
        assert_eq!(setup.canvas.read_with(cx, |c, _| c.status_worktrees().clone()).unwrap().len(), 1);
        chat(&setup, "k-1", Some("s-1"), SessionTimes::default(), cx);
        let (mine, line) = wait(&setup, "k-1", json!({"title": "Vault password", "on": "me"}), cx);
        assert!(mine);
        assert_eq!(line, "error: no GitHub remote: origin is not configured\n");
        assert_eq!(setup.gh.count("gh "), 0);
        close_all(&setup.canvas, cx);
    }

    #[gpui::test]
    async fn the_timer_refreshes_every_minute_and_never_overlaps(cx: &mut TestAppContext) {
        let (setup, cx) = setup(cx, true).await;
        let reads = || setup.gh.count("gh issue list");
        setup.canvas.update(cx, |c, cx| c.start_status_refresh(cx));
        cx.run_until_parked();
        assert_eq!(reads(), 1, "subito");
        cx.executor().advance_clock(STATUS_REFRESH);
        cx.run_until_parked();
        assert_eq!(reads(), 2, "dopo un minuto");
        setup.canvas.update(cx, |c, _| c.stop_status_refresh());
        cx.executor().advance_clock(STATUS_REFRESH);
        cx.run_until_parked();
        assert_eq!(reads(), 2, "fermato");
        setup.canvas.update(cx, |c, cx| {
            c.refresh_status(cx);
            assert!(c.status_refreshing());
            c.refresh_status(cx);
        });
        cx.run_until_parked();
        assert_eq!(reads(), 3, "il secondo non parte mentre il primo e' in corso");
    }

    #[gpui::test]
    async fn a_wait_from_a_chat_opens_the_issue_answers_and_notifies_when_it_is_on_me(cx: &mut TestAppContext) {
        let (setup, cx) = setup(cx, true).await;
        // la issue aperta compare nella lista (senza il marker: open_wait la crea lo stesso)
        setup.gh.on("gh issue list", ok(OPEN_212));
        setup.gh.on("gh label create", ok(""));
        setup.gh.on("gh issue create", ok("https://github.com/acme/acme_app/issues/212\n"));
        chat(&setup, "k-1", Some("s-1"), SessionTimes::default(), cx);
        chat(&setup, "k-2", None, SessionTimes::default(), cx);

        let lists = setup.gh.count("gh issue list");
        let (mine, line) = wait(&setup, "k-1", json!({"title": "Vault password", "on": "me"}), cx);
        assert!(mine);
        assert_eq!(line, "ok #212 https://github.com/acme/acme_app/issues/212\n");
        assert_eq!(setup.gh.count("gh issue create -R acme/acme_app"), 1);
        let list = notifications(cx);
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].title, "Waiting on you: Vault password");
        assert_eq!(list[0].source, WAITING_SOURCE);
        assert_eq!(list[0].severity, Severity::Warning);
        assert_eq!(setup.canvas.read_with(cx, |c, _| c.session_waits("s-1")), vec![(212, "Vault password".to_string())]);
        assert_eq!(setup.gh.count("gh issue list"), lists + 2, "la lista di open_wait e quella del refresh");
        assert!(matches!(waiting(&setup, cx), WaitingState::Fresh { .. }), "parte un refresh");

        setup.gh.on("gh issue create", ok("https://github.com/acme/acme_app/issues/213\n"));
        let (mine, line) = wait(&setup, "k-1", json!({"title": "SECSTORE on CAR", "on": "Basis"}), cx);
        assert!(mine);
        assert_eq!(line, "ok #213 https://github.com/acme/acme_app/issues/213\n");
        assert_eq!(notifications(cx).len(), 1, "attesa su altri: nessuna notifica");

        let (mine, line) = wait(&setup, "unknown", json!({"title": "x", "on": "me"}), cx);
        assert!(!mine);
        assert_eq!(line, "", "nessuna risposta: tocca a un altro canvas");

        let creates = setup.gh.count("gh issue create");
        let (mine, line) = wait(&setup, "k-1", json!({"on": "me"}), cx);
        assert!(mine);
        assert_eq!(line, "error: superzed-wait: a title is needed\n");
        assert_eq!(setup.gh.count("gh issue create"), creates);

        let (mine, line) = wait(&setup, "k-2", json!({"title": "x", "on": "me"}), cx);
        assert!(mine);
        assert_eq!(line, "error: the chat has no session yet, retry in a moment\n");
        close_all(&setup.canvas, cx);
    }

    /// `gh` finto che si ferma sui comandi con `needle` finche' non si apre il cancello.
    struct Gated {
        inner: Arc<FakeRunner>,
        needle: &'static str,
        gate: futures::future::Shared<futures::channel::oneshot::Receiver<()>>,
    }

    impl Runner for Gated {
        fn run(
            &self,
            program: &str,
            args: &[String],
            stdin: Option<String>,
        ) -> futures::future::BoxFuture<'static, Result<superzed_sources::RunOutput, superzed_sources::RunError>> {
            let line = std::iter::once(program.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
            let result = self.inner.run(program, args, stdin);
            let gate = line.contains(self.needle).then(|| self.gate.clone());
            Box::pin(async move {
                if let Some(gate) = gate {
                    gate.await.ok();
                }
                result.await
            })
        }
    }

    /// Chat "k-1" (sessione "s-1") con un'attesa #212 "Vault password" aperta dal canvas.
    async fn with_open_wait(cx: &mut TestAppContext) -> (Setup, &'static mut VisualTestContext) {
        let (setup, cx) = setup(cx, true).await;
        setup.gh.on("gh issue list", ok(OPEN_212));
        setup.gh.on("gh label create", ok(""));
        setup.gh.on("gh issue create", ok("https://github.com/acme/acme_app/issues/212\n"));
        setup.gh.on("gh issue close", ok(""));
        chat(&setup, "k-1", Some("s-1"), SessionTimes::default(), cx);
        let (_, line) = wait(&setup, "k-1", json!({"title": "Vault password", "on": "Basis"}), cx);
        assert_eq!(line, "ok #212 https://github.com/acme/acme_app/issues/212\n");
        (setup, cx)
    }

    fn waits(setup: &Setup, cx: &mut VisualTestContext) -> Vec<(u64, String)> {
        setup.canvas.read_with(cx, |c, _| c.session_waits("s-1"))
    }

    #[gpui::test]
    async fn a_wait_closed_from_the_node_leaves_session_waits(cx: &mut TestAppContext) {
        let (setup, cx) = with_open_wait(cx).await;
        assert_eq!(waits(&setup, cx), vec![(212, "Vault password".to_string())]);
        setup.canvas.update(cx, |c, cx| c.close_waiting_item(212, cx));
        cx.run_until_parked();
        assert_eq!(setup.gh.count("gh issue close 212"), 1);
        assert_eq!(waits(&setup, cx), vec![], "chiusa: niente piu' nel prompt del report");
        close_all(&setup.canvas, cx);
    }

    #[gpui::test]
    async fn a_wait_missing_from_a_later_fresh_read_leaves_session_waits(cx: &mut TestAppContext) {
        let (setup, cx) = with_open_wait(cx).await;
        refresh(&setup, cx);
        assert_eq!(waits(&setup, cx), vec![(212, "Vault password".to_string())], "ancora aperta su GitHub");
        setup.gh.on("gh issue list", fail(1, "HTTP 502: bad gateway"));
        refresh(&setup, cx);
        assert_eq!(waits(&setup, cx), vec![(212, "Vault password".to_string())], "una lettura fallita non dice niente");
        // chiusa direttamente su GitHub
        setup.gh.on("gh issue list", ok("[]"));
        refresh(&setup, cx);
        assert_eq!(waits(&setup, cx), vec![]);
        close_all(&setup.canvas, cx);
    }

    #[gpui::test]
    async fn a_refresh_started_before_the_wait_does_not_drop_it(cx: &mut TestAppContext) {
        use futures::FutureExt as _;
        let (setup, cx) = setup(cx, true).await;
        // la lista letta dal giro gia' partito non contiene ancora la issue
        setup.gh.on("gh issue list", ok("[]"));
        setup.gh.on("gh label create", ok(""));
        setup.gh.on("gh issue create", ok("https://github.com/acme/acme_app/issues/212\n"));
        chat(&setup, "k-1", Some("s-1"), SessionTimes::default(), cx);
        let (open, gate) = futures::channel::oneshot::channel::<()>();
        let gated = Arc::new(Gated { inner: setup.gh.clone(), needle: "worktree list", gate: gate.shared() });
        cx.update(|_, cx| superzed_done::init_test(cx, gated, FakeRunner::new(), Some(setup.dir.path().to_path_buf())));
        setup.canvas.update(cx, |c, cx| c.refresh_status(cx));
        cx.run_until_parked();
        assert!(setup.canvas.read_with(cx, |c, _| c.status_refreshing()), "giro fermo sui worktree");
        let (_, line) = wait(&setup, "k-1", json!({"title": "Vault password", "on": "Basis"}), cx);
        assert_eq!(line, "ok #212 https://github.com/acme/acme_app/issues/212\n");
        open.send(()).unwrap();
        cx.run_until_parked();
        assert!(matches!(waiting(&setup, cx), WaitingState::Fresh { ref items, .. } if items.is_empty()));
        assert_eq!(waits(&setup, cx), vec![(212, "Vault password".to_string())], "il giro partito prima non la toglie");
        refresh(&setup, cx);
        assert_eq!(waits(&setup, cx), vec![], "un giro partito dopo si'");
        close_all(&setup.canvas, cx);
    }

    #[gpui::test]
    async fn the_wait_router_answers_for_chats_of_no_open_project(cx: &mut TestAppContext) {
        let (_setup, cx) = setup(cx, true).await;
        let (reply, client) = reply_pair();
        cx.update(|_, cx| crate::registry::route_wait("unknown", json!({"title": "x", "on": "me"}), reply, cx));
        assert_eq!(answer(client), "error: this chat does not belong to an open project\n");
    }

    #[gpui::test]
    async fn chat_rows_follow_the_session_state(cx: &mut TestAppContext) {
        assert_eq!(chat_state(&SessionState::NeedsInput(InputKind::Permission)), ChatState::NeedsReply);
        assert_eq!(chat_state(&SessionState::Done), ChatState::NeedsReply);
        assert_eq!(chat_state(&SessionState::Working), ChatState::Running);
        assert_eq!(chat_state(&SessionState::Starting), ChatState::Running);
        assert_eq!(chat_state(&SessionState::Idle), ChatState::Idle);
        assert_eq!(chat_state(&SessionState::Ended), ChatState::Idle);
        assert_eq!(chat_state(&SessionState::Failed("x".into())), ChatState::Failed);

        let (setup, cx) = setup(cx, true).await;
        const DAY: i64 = 24 * 60 * 60;
        let now = chrono::Local::now().timestamp();
        let replied = |days: i64| SessionTimes { opened_at: Some(now - 10 * DAY), replied_at: Some(now - days * DAY) };
        setup.canvas.update_in(cx, |c, window, cx| {
            c.open_kanban(window, cx);
        });
        let working = chat(&setup, "free-working", Some("s-w"), SessionTimes::default(), cx);
        let done = chat(&setup, "kanban-done", Some("s-d"), SessionTimes::default(), cx);
        setup.canvas.update(cx, |c, cx| c.dock(done, Phase::Implementing, cx));
        let six_days = chat(&setup, "idle-6", Some("s-6"), replied(6), cx);
        let one_day = chat(&setup, "idle-1", Some("s-1"), replied(1), cx);
        let automation = chat(&setup, "automation", Some("s-a"), SessionTimes::default(), cx);
        setup.canvas.update(cx, |c, cx| {
            c.ensure_container(DockKind::Automations, cx);
            if let Some(frame) = c.model.frame_mut(automation) {
                frame.dock = Some(DockKind::Automations);
            }
        });
        store(cx).update(cx, |s, cx| s.set_focused(None, cx));
        store(cx).update(cx, |s, cx| {
            s.apply(hook("free-working", json!({"hook_event_name": "UserPromptSubmit"})), cx);
            s.apply(hook("kanban-done", json!({"hook_event_name": "Stop"})), cx);
            s.apply(hook("idle-6", json!({"hook_event_name": "SessionStart", "session_id": "s-6"})), cx);
            s.apply(hook("idle-1", json!({"hook_event_name": "SessionStart", "session_id": "s-1"})), cx);
            s.apply(hook("automation", json!({"hook_event_name": "UserPromptSubmit"})), cx);
        });
        cx.run_until_parked();

        let rows = setup.canvas.read_with(cx, |c, cx| c.status_chat_rows(cx));
        let shape: Vec<(NodeId, ChatState)> = rows.iter().map(|row| (row.node, row.state)).collect();
        assert_eq!(
            shape,
            [(done, ChatState::NeedsReply), (working, ChatState::Running), (six_days, ChatState::Idle), (one_day, ChatState::Idle)],
            "{rows:?}"
        );
        let days = |row: &ChatRow| row.age.map(|age| age.as_secs() as i64 / DAY);
        assert_eq!((days(&rows[2]), days(&rows[3])), (Some(6), Some(1)));
        close_all(&setup.canvas, cx);
    }

    #[gpui::test]
    async fn closing_an_item_closes_the_issue_and_drops_the_row(cx: &mut TestAppContext) {
        let (setup, cx) = setup(cx, true).await;
        setup.gh.on("gh issue close", ok(""));
        refresh(&setup, cx);
        setup.canvas.update(cx, |c, cx| {
            c.close_waiting_item(171, cx);
            assert_eq!(numbers(c.status_waiting().items()), [208, 212], "sparisce subito");
        });
        cx.run_until_parked();
        assert_eq!(setup.gh.count("gh issue close 171 --repo acme/acme_app"), 1);
        assert!(notifications(cx).is_empty());

        setup.gh.on("gh issue close", fail(1, "HTTP 403: Resource not accessible by integration"));
        let lists = setup.gh.count("gh issue list");
        setup.canvas.update(cx, |c, cx| c.close_waiting_item(212, cx));
        cx.run_until_parked();
        assert_eq!(setup.gh.count("gh issue list"), lists + 1, "refresh dopo il fallimento");
        assert_eq!(numbers(waiting(&setup, cx).items()), [208, 212, 171], "la voce torna con il refresh");
        let list = notifications(cx);
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].severity, Severity::Error);
        assert!(list[0].summary.contains("Resource not accessible by integration"), "{}", list[0].summary);
    }
}
