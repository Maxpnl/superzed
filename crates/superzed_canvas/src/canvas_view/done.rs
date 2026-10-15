//! Pipeline Fatto → GitHub, lato canvas. Una chat del kanban spostata a mano in Fatto
//! riceve (solo se claude e' vivo e libero: la regola delle automazioni) il prompt che
//! chiede il report; se la chat non puo' riceverlo, resta occupata oltre `REPORT_WAIT` o il
//! file non arriva valido, il report lo fa `claude -p` sul transcript. Fatto arrivato da
//! solo (hook) non scrive mai nella chat: subito `claude -p`. Report pronto → notifica con
//! «Rivedi» → anteprima → «Scrivi su GitHub», che scrive in un task dell'app (sopravvive al
//! canvas) salvando l'esito dopo ogni voce. Un report valido gia' presente (rientro in
//! Fatto) non si rifa': si ripropone la notifica, o si dice che e' gia' tutto scritto.
use super::*;
use crate::automation::Readiness;
use collections::HashSet;
use gpui::Global;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use superzed_done::{Action, DoneEnv, Preview, Report, RunResult, WriteRun};
use superzed_notifications::{NewNotification, NotificationAction, NotificationFilter, NotificationState, NotificationStore, Severity};
use superzed_sources::Runner;
use superzed_sources::github::PmConfig;

/// Quanto si aspetta la chat occupata, e poi il file del report.
pub const REPORT_WAIT: Duration = Duration::from_secs(180);
/// Ogni quanto si ricontrollano chat e file.
pub const REPORT_POLL: Duration = Duration::from_secs(2);
/// Prompt scritto, file del report non valido e claude libero da tanto: non lo sistemera',
/// si passa a `claude -p` senza aspettare `REPORT_WAIT`.
pub const IDLE_GRACE: Duration = Duration::from_secs(30);

pub const STATUS_WAITING: &str = "report: waiting for the chat to be free";
pub const STATUS_PROMPTED: &str = "report: asked the chat";
pub const STATUS_HEADLESS: &str = "report: claude -p running";
pub const STATUS_READY: &str = "report ready";
pub const STATUS_WRITING: &str = "GitHub: writing";
pub const STATUS_WRITTEN: &str = "GitHub updated";
pub const STATUS_ALREADY_WRITTEN: &str = "already written to GitHub";
/// Inizio dello stato di un report fallito (`report_failed_status`), riconosciuto da
/// `done_status_view`.
pub const STATUS_REPORT_FAILED: &str = "report failed";

pub fn report_failed_status(error: &str) -> String {
    format!("{STATUS_REPORT_FAILED}: {error}")
}

/// Livello dello stato Done nella barra del titolo: ne decide colore e icona.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoneLevel {
    Error,
    InProgress,
    Ready,
    Written,
}

/// Stato Done → (testo breve per la barra, livello, dettaglio completo per il tooltip).
pub fn done_status_view(status: &str) -> (&'static str, DoneLevel, String) {
    let detail = capitalize(status);
    match status {
        STATUS_WAITING | STATUS_PROMPTED | STATUS_HEADLESS => ("Report in progress…", DoneLevel::InProgress, detail),
        STATUS_WRITING => ("Writing to GitHub…", DoneLevel::InProgress, detail),
        STATUS_READY => (
            "Report ready",
            DoneLevel::Ready,
            "Report ready: click to open the review and write to GitHub".into(),
        ),
        STATUS_WRITTEN | STATUS_ALREADY_WRITTEN => ("Written to GitHub", DoneLevel::Written, detail),
        s if s.starts_with(STATUS_REPORT_FAILED) => ("Report failed", DoneLevel::Error, detail),
        _ => ("GitHub: error", DoneLevel::Error, detail),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Cosa serve per scrivere e chiudere una scrittura, anche senza il canvas.
pub(crate) struct WriteJob {
    session_id: String,
    chat_title: String,
    root: PathBuf,
    cfg: PmConfig,
    /// Il report dell'anteprima (la sua copia finisce nell'esito alla prima scrittura).
    report: Report,
    previous: RunResult,
    project_root: Option<PathBuf>,
}

/// Scritture su GitHub in corso in tutta l'app, per (root del progetto, id di sessione):
/// una alla volta per sessione anche fra canvas diversi.
#[derive(Default)]
struct DoneWrites(HashSet<(PathBuf, String)>);

impl Global for DoneWrites {}

pub(crate) fn write_running(root: &Path, session_id: &str, cx: &App) -> bool {
    cx.try_global::<DoneWrites>().is_some_and(|w| w.0.contains(&(root.to_path_buf(), session_id.to_string())))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoneStage {
    /// La chat e' occupata (o claude parte): si aspetta.
    WaitingChat,
    /// Prompt scritto: si aspetta il file.
    Prompted,
    /// `claude -p` in corso.
    Headless,
}

/// Invio del prompt non ancora partito. Se la pipeline finisce prima (chat uscita da Fatto
/// o dal kanban, nodo chiuso), al posto dell'Invio parte Ctrl-U: nella chat non resta il
/// testo incollato.
struct PendingEnter(Arc<AtomicBool>);

impl Drop for PendingEnter {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct DoneRun {
    session_id: String,
    root: PathBuf,
    areas: Vec<String>,
    stage: DoneStage,
    /// Controlli fatti nella fase corrente.
    polls: u32,
    /// Controlli consecutivi con il prompt scritto, un file del report non valido e claude libero.
    idle_polls: u32,
    _enter: Option<PendingEnter>,
    _task: Task<()>,
}

#[derive(Default)]
pub(crate) struct DonePipeline {
    runs: HashMap<NodeId, DoneRun>,
    /// Ultimo stato mostrato nella barra del titolo della chat.
    status: HashMap<NodeId, String>,
    /// Chiavi spostate a mano (`set_session_phase`) il cui `PhaseChanged` deve ancora arrivare.
    manual: HashSet<String>,
}

impl DonePipeline {
    /// Nodi chiusi o usciti dal kanban: niente piu' attese ne' stato.
    pub(crate) fn forget(&mut self, ids: &[NodeId]) {
        for id in ids {
            self.runs.remove(id);
            self.status.remove(id);
        }
    }

    /// Il prossimo `PhaseChanged` della chiave viene da uno spostamento a mano.
    pub(crate) fn mark_manual(&mut self, key: &str) {
        self.manual.insert(key.to_string());
    }
}

fn wait_polls() -> u32 {
    (REPORT_WAIT.as_millis() / REPORT_POLL.as_millis()) as u32
}

fn idle_polls() -> u32 {
    (IDLE_GRACE.as_millis() / REPORT_POLL.as_millis()) as u32
}

pub(crate) fn review_action(session_id: &str) -> NotificationAction {
    NotificationAction { kind: superzed_done::REVIEW_ACTION.into(), label: "Review".into(), arg: session_id.into() }
}

/// L'azione «Review» della sessione, riconosciuta da tipo e argomento (mai dall'etichetta:
/// le notifiche salvate prima dell'interfaccia in inglese hanno «Rivedi»).
pub(crate) fn is_review_of(action: Option<&NotificationAction>, session_id: &str) -> bool {
    action.is_some_and(|a| a.kind == superzed_done::REVIEW_ACTION && a.arg == session_id)
}

/// Notifica della pipeline (sorgente "Fatto → GitHub").
fn notify_done(
    project_root: Option<PathBuf>,
    severity: Severity,
    title: String,
    summary: String,
    details: Option<String>,
    action: Option<NotificationAction>,
    cx: &mut App,
) {
    let Some(store) = NotificationStore::global(cx) else {
        return;
    };
    let n = NewNotification {
        severity,
        title,
        summary,
        details,
        suggested_prompt: None,
        project_root,
        source: superzed_done::NOTIFICATION_SOURCE.into(),
    };
    store.update(cx, |s, cx| {
        match action {
            Some(a) => s.add_with_action(n, a, cx),
            None => s.add(n, cx),
        };
    });
}

/// Scritto tutto: le notifiche «Rivedi» ancora nuove della sessione (report pronto, voci
/// non scritte) passano a prese in carico.
fn take_review_notifications(session_id: &str, cx: &mut App) {
    let Some(store) = NotificationStore::global(cx) else {
        return;
    };
    let ids: Vec<String> = store
        .read(cx)
        .list(&NotificationFilter { include_archived: true, ..Default::default() })
        .into_iter()
        .filter(|n| n.state == NotificationState::New && is_review_of(n.action.as_ref(), session_id))
        .map(|n| n.id)
        .collect();
    store.update(cx, |s, cx| {
        for id in &ids {
            s.set_state(id, NotificationState::Taken, cx);
        }
    });
}

fn items_written(n: usize) -> String {
    if n == 1 { "1 item written".to_string() } else { format!("{n} items written") }
}

/// Scrittura su GitHub in un task dell'app, non del canvas: chiudere il canvas (o la
/// finestra) a meta' non la interrompe, e l'esito si salva comunque dopo ogni voce.
fn spawn_done_write(
    canvas: WeakEntity<CanvasView>,
    runner: Arc<dyn Runner>,
    job: WriteJob,
    actions: Vec<Action>,
    path: PathBuf,
    cx: &mut App,
) {
    let key = (job.root.clone(), job.session_id.clone());
    cx.default_global::<DoneWrites>().0.insert(key.clone());
    let (cfg, report, previous) = (job.cfg.clone(), job.report.clone(), job.previous.clone());
    let sent = actions.clone();
    cx.spawn(async move |cx| {
        let run = cx
            .background_spawn(async move { superzed_done::write_and_save(runner, cfg, &report, actions, previous, path).await })
            .await;
        cx.update(|cx| {
            cx.default_global::<DoneWrites>().0.remove(&key);
            done_written(canvas, job, &sent, run, cx);
        });
    })
    .detach();
}

/// Notifica dell'esito (le voci scritte da questa scrittura), stato nella chat e pannelli
/// GitHub ricaricati se il canvas c'e' ancora. Le voci fallite hanno etichette leggibili
/// (`sent`: le azioni di questa scrittura, con i testi modificati).
fn done_written(canvas: WeakEntity<CanvasView>, job: WriteJob, sent: &[Action], run: WriteRun, cx: &mut App) {
    let WriteJob { session_id, chat_title, project_root, report, .. } = job;
    let failed = run.result.failed();
    let (severity, title, mut summary, action) = if failed.is_empty() {
        let now = run.reports.iter().filter(|r| r.result.is_ok()).count();
        (Severity::Info, format!("GitHub updated: {chat_title}"), items_written(now), None)
    } else {
        let what = if failed.len() == 1 { "1 item not written".to_string() } else { format!("{} items not written", failed.len()) };
        let lines: Vec<String> = failed.iter().map(|(k, e)| format!("{}: {e}", superzed_done::item_label(k, sent, &report))).collect();
        (Severity::Error, format!("GitHub: {what} ({chat_title})"), lines.join("\n"), Some(review_action(&session_id)))
    };
    if let Some(e) = &run.save_error {
        summary.push_str(&format!("\nOutcome not saved: {e}"));
    }
    if failed.is_empty() {
        take_review_notifications(&session_id, cx);
    }
    let status = if failed.is_empty() { STATUS_WRITTEN.to_string() } else { title.clone() };
    notify_done(project_root, severity, title, summary, None, action, cx);
    canvas
        .update(cx, |c, cx| {
            if let Some(node) = c.node_for_session_id(&session_id, cx) {
                c.set_done_status(node, status, cx);
            }
            c.refresh_github_panels(cx);
        })
        .ok();
}

impl CanvasView {
    pub fn done_status(&self, node: NodeId) -> Option<&str> {
        self.done_pipeline.status.get(&node).map(String::as_str)
    }

    pub fn done_stage(&self, node: NodeId) -> Option<DoneStage> {
        self.done_pipeline.runs.get(&node).map(|r| r.stage)
    }

    pub(crate) fn set_done_status(&mut self, node: NodeId, status: impl Into<String>, cx: &mut Context<Self>) {
        self.done_pipeline.status.insert(node, status.into());
        cx.notify();
    }

    /// Ferma la pipeline del nodo e toglie lo stato.
    fn stop_done(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let had_run = self.done_pipeline.runs.remove(&node).is_some();
        let had_status = self.done_pipeline.status.remove(&node).is_some();
        if had_run || had_status {
            cx.notify();
        }
    }

    pub(crate) fn session_of(&self, node: NodeId, cx: &App) -> Option<superzed_sessions::Session> {
        let key = self.session_key(node)?;
        superzed_sessions::SessionStore::global(cx)?.read(cx).get(key).cloned()
    }

    /// Chat claude agganciata al kanban (che c'e').
    fn is_kanban_chat(&self, node: NodeId) -> bool {
        self.kanban_node().is_some() && self.model.frame(node).is_some_and(|f| f.kind == NodeKind::Claude && f.in_kanban())
    }

    /// Chat del kanban in Fatto (menu "GitHub…").
    pub fn is_done_chat(&self, node: NodeId, cx: &App) -> bool {
        self.is_kanban_chat(node) && self.session_of(node, cx).is_some_and(|s| s.phase == superzed_sessions::Phase::Done)
    }

    /// claude non ha ancora mandato `SessionStart`: si aspetta come una chat occupata.
    fn chat_starting(&self, node: NodeId, cx: &App) -> bool {
        self.terminals.contains_key(&node)
            && matches!(self.chat_session_state(node, cx), Some(superzed_sessions::SessionState::Starting))
    }

    pub(crate) fn chat_title_of(&self, node: Option<NodeId>, cx: &App) -> String {
        node.and_then(|n| self.model.frame(n)).map(|f| self.node_title(f, cx)).unwrap_or_else(|| "closed chat".to_string())
    }

    pub(crate) fn done_notify(
        &self,
        severity: Severity,
        title: String,
        summary: String,
        details: Option<String>,
        action: Option<NotificationAction>,
        cx: &mut Context<Self>,
    ) {
        notify_done(self.project_root_path(cx), severity, title, summary, details, action, cx);
    }

    /// C'e' gia' una notifica «Rivedi» ancora nuova per la sessione.
    fn review_pending(&self, session_id: &str, cx: &App) -> bool {
        let Some(store) = NotificationStore::global(cx) else {
            return false;
        };
        store
            .read(cx)
            .list(&NotificationFilter { include_archived: true, ..Default::default() })
            .iter()
            .any(|n| n.state == NotificationState::New && is_review_of(n.action.as_ref(), session_id))
    }

    /// `PhaseChanged`: una chat del kanban entrata in Fatto avvia la pipeline; uscita da
    /// Fatto (o dal kanban) la ferma e toglie lo stato.
    pub(crate) fn done_phase_changed(&mut self, key: &str, cx: &mut Context<Self>) {
        let manual = self.done_pipeline.manual.remove(key);
        let Some(node) = self.node_for_session_key(key) else {
            return;
        };
        if self.is_done_chat(node, cx) {
            self.start_done(node, !manual, cx);
            return;
        }
        self.stop_done(node, cx);
    }

    /// Id di sessione, root del report e aree; ogni errore ferma la pipeline prima del prompt.
    fn prepare_done(&self, node: NodeId, cx: &App) -> Result<(String, PathBuf, Vec<String>), String> {
        let env = DoneEnv::get(cx).ok_or_else(|| "pipeline not initialized".to_string())?;
        let session = self.session_of(node, cx).ok_or_else(|| "no claude session".to_string())?;
        let session_id = session
            .session_id
            .filter(|id| superzed_done::safe_session_id(id))
            .ok_or_else(|| "the session has no id yet".to_string())?;
        let root = env.root(self.project_root_path(cx)).ok_or_else(|| "the project has no folder".to_string())?;
        let (_, areas) = superzed_done::load_pm(&root)?;
        Ok((session_id, root, areas))
    }

    /// Chiede il report per la chat (come uno spostamento a mano in Fatto).
    pub fn start_done_pipeline(&mut self, node: NodeId, cx: &mut Context<Self>) {
        self.start_done(node, false, cx);
    }

    /// Un report valido gia' salvato (con il suo esito) si tiene: solo la notifica, o lo
    /// stato "già scritto" se su GitHub c'e' gia' tutto. Se no il report si chiede nel PTY
    /// se claude e' libero, dopo se e' occupato o sta partendo, subito con `claude -p` se
    /// non c'e' o se Fatto e' arrivato da solo (`automatic`: mai nella chat, dove l'utente
    /// o claude stanno lavorando). Una pipeline gia' in corso non riparte.
    fn start_done(&mut self, node: NodeId, automatic: bool, cx: &mut Context<Self>) {
        if self.done_pipeline.runs.contains_key(&node) {
            return;
        }
        let (session_id, root, areas) = match self.prepare_done(node, cx) {
            Ok(x) => x,
            Err(e) => {
                self.set_done_status(node, format!("GitHub: {e}"), cx);
                return;
            }
        };
        // rientro in Fatto: il report (e l'esito) di prima restano validi
        if let Ok(source) = superzed_done::review_source(&root, &session_id) {
            if superzed_done::fully_written(&source.report, &areas, &source.previous) {
                self.set_done_status(node, STATUS_ALREADY_WRITTEN, cx);
            } else {
                self.raise_review(node, &source.report, cx);
            }
            return;
        }
        if let Err(e) = superzed_done::ensure_reports_dir(&root) {
            self.set_done_status(node, report_failed_status(&e), cx);
            return;
        }
        let readiness = self.chat_readiness(node, cx);
        if automatic || (readiness == Readiness::Unavailable && !self.chat_starting(node, cx)) {
            self.start_headless(node, session_id, root, areas, cx);
            return;
        }
        let prompted = readiness == Readiness::Ready;
        let enter = if prompted { self.send_report_prompt(node, &root, &session_id, &areas, cx) } else { None };
        let stage = if prompted { DoneStage::Prompted } else { DoneStage::WaitingChat };
        let task = self.spawn_done_poll(node, cx);
        self.done_pipeline
            .runs
            .insert(node, DoneRun { session_id, root, areas, stage, polls: 0, idle_polls: 0, _enter: enter, _task: task });
        self.set_done_status(node, if prompted { STATUS_PROMPTED } else { STATUS_WAITING }, cx);
    }

    /// Prompt incollato nella chat; l'Invio parte dopo `ENTER_DELAY` se la pipeline c'e'
    /// ancora, se no Ctrl-U cancella il testo incollato.
    fn send_report_prompt(&self, node: NodeId, root: &Path, session_id: &str, areas: &[String], cx: &mut Context<Self>) -> Option<PendingEnter> {
        let path = superzed_done::report_path(root, session_id).ok()?;
        let terminal = self.terminals.get(&node).map(|(_, t)| t.clone())?;
        let paste = crate::automation::bracketed_paste(&superzed_done::chat_prompt(&path, session_id, areas));
        terminal.update(cx, |t, _| t.input(paste.into_bytes()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let terminal = terminal.downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(super::automations::ENTER_DELAY).await;
            let key = if flag.load(Ordering::SeqCst) { "\x15" } else { "\r" };
            terminal.update(cx, |t, _| t.input(key.as_bytes())).ok();
        })
        .detach();
        Some(PendingEnter(cancelled))
    }

    fn spawn_done_poll(&self, node: NodeId, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REPORT_POLL).await;
            match this.update(cx, |c, cx| c.done_poll(node, cx)) {
                Ok(true) => {}
                _ => break,
            }
        })
    }

    /// Un controllo; `false` quando l'attesa e' finita (report, fallback o pipeline tolta).
    fn done_poll(&mut self, node: NodeId, cx: &mut Context<Self>) -> bool {
        // uscita dal kanban (sganciata, kanban chiuso) o da Fatto: niente prompt ne' claude -p
        if !self.is_done_chat(node, cx) {
            self.stop_done(node, cx);
            return false;
        }
        let readiness = self.chat_readiness(node, cx);
        let starting = self.chat_starting(node, cx);
        let Some(run) = self.done_pipeline.runs.get_mut(&node) else {
            return false;
        };
        run.polls += 1;
        let timed_out = run.polls >= wait_polls();
        let (stage, root, session_id, areas) = (run.stage, run.root.clone(), run.session_id.clone(), run.areas.clone());
        match stage {
            DoneStage::WaitingChat => {
                if readiness == Readiness::Ready {
                    let enter = self.send_report_prompt(node, &root, &session_id, &areas, cx);
                    if let Some(run) = self.done_pipeline.runs.get_mut(&node) {
                        run.stage = DoneStage::Prompted;
                        run.polls = 0;
                        run._enter = enter;
                    }
                    self.set_done_status(node, STATUS_PROMPTED, cx);
                    return true;
                }
                if (readiness == Readiness::Busy || starting) && !timed_out {
                    return true;
                }
                self.start_headless(node, session_id, root, areas, cx);
                false
            }
            DoneStage::Prompted => match superzed_done::read_report(&root, &session_id) {
                Ok(report) => {
                    self.done_report_ready(node, report, false, cx);
                    false
                }
                Err(_) => {
                    // file non valido e claude libero: si concede `IDLE_GRACE`, poi claude -p
                    let invalid = superzed_done::report_path(&root, &session_id).is_ok_and(|p| p.exists());
                    run.idle_polls = if invalid && readiness == Readiness::Ready { run.idle_polls + 1 } else { 0 };
                    let gave_up = run.idle_polls >= idle_polls();
                    // assente o scritto a meta': si aspetta finche' c'e' tempo e claude e' vivo
                    if !timed_out && !gave_up && readiness != Readiness::Unavailable {
                        return true;
                    }
                    self.start_headless(node, session_id, root, areas, cx);
                    false
                }
            },
            DoneStage::Headless => false,
        }
    }

    /// `claude -p` sul transcript, sull'account dello scheduler, in background, da una
    /// cartella temporanea.
    fn start_headless(&mut self, node: NodeId, session_id: String, root: PathBuf, areas: Vec<String>, cx: &mut Context<Self>) {
        let Some(env) = DoneEnv::get(cx) else {
            return;
        };
        let Some(transcript) = self.session_of(node, cx).and_then(|s| s.transcript) else {
            self.done_pipeline.runs.remove(&node);
            self.done_failed(node, "session transcript not available".into(), cx);
            return;
        };
        let config_dir = superzed_accounts::pick_for_new_chat(cx, Some(root.as_path())).and_then(|(account, _)| account.config_dir);
        let cwd = std::env::temp_dir().join("superzed-done");
        let argv = superzed_done::claude_argv(
            config_dir.as_deref(),
            util::paths::home_dir(),
            &cwd,
            std::env::vars().map(|(k, _)| k),
            std::env::var("PATH").ok().as_deref(),
        );
        let (runner, sid, prompt_areas) = (env.claude.clone(), session_id.clone(), areas.clone());
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    std::fs::create_dir_all(&cwd).map_err(|e| format!("{}: {e}", cwd.display()))?;
                    let tail = superzed_done::transcript_tail(&transcript, superzed_done::TRANSCRIPT_TAIL)?;
                    let stdin = superzed_done::headless_stdin(&sid, &prompt_areas, &tail);
                    superzed_done::headless_report(runner, argv, stdin, sid).await
                })
                .await;
            this.update(cx, |c, cx| match result {
                Ok(report) => c.done_report_ready(node, report, true, cx),
                Err(e) => {
                    c.done_pipeline.runs.remove(&node);
                    c.done_failed(node, e, cx);
                }
            })
            .ok();
        });
        self.done_pipeline.runs.insert(
            node,
            DoneRun { session_id, root, areas, stage: DoneStage::Headless, polls: 0, idle_polls: 0, _enter: None, _task: task },
        );
        self.set_done_status(node, STATUS_HEADLESS, cx);
    }

    /// Report valido: salvato (se viene da `claude -p`), notifica con «Rivedi».
    fn done_report_ready(&mut self, node: NodeId, report: Report, save: bool, cx: &mut Context<Self>) {
        let Some(run) = self.done_pipeline.runs.remove(&node) else {
            return;
        };
        if save && let Err(e) = superzed_done::write_report(&run.root, &report) {
            self.done_failed(node, e, cx);
            return;
        }
        self.raise_review(node, &report, cx);
    }

    /// Stato "report pronto" e notifica con «Rivedi», se non ce n'e' gia' una nuova per la
    /// stessa sessione.
    fn raise_review(&mut self, node: NodeId, report: &Report, cx: &mut Context<Self>) {
        self.set_done_status(node, STATUS_READY, cx);
        if self.review_pending(&report.session_id, cx) {
            return;
        }
        let title = self.chat_title_of(Some(node), cx);
        self.done_notify(
            Severity::Info,
            format!("Report ready: {title}"),
            superzed_done::report_summary(report),
            Some(superzed_done::report_markdown(report)),
            Some(review_action(&report.session_id)),
            cx,
        );
    }

    fn done_failed(&mut self, node: NodeId, error: String, cx: &mut Context<Self>) {
        let title = self.chat_title_of(Some(node), cx);
        self.set_done_status(node, report_failed_status(&error), cx);
        self.done_notify(Severity::Error, format!("Report failed: {title}"), error, None, None, cx);
    }

    pub(crate) fn node_for_session_id(&self, session_id: &str, cx: &App) -> Option<NodeId> {
        let store = superzed_sessions::SessionStore::global(cx)?;
        let store = store.read(cx);
        self.sessions
            .iter()
            .find(|(_, s)| store.get(&s.key).and_then(|s| s.session_id.as_deref()) == Some(session_id))
            .map(|(id, _)| *id)
    }

    /// Azione di una notifica interna («Rivedi» della pipeline).
    pub fn open_notification_action(&mut self, action: &NotificationAction, window: &mut Window, cx: &mut Context<Self>) {
        if action.kind == superzed_done::REVIEW_ACTION {
            self.open_done_review(&action.arg, window, cx);
        } else {
            self.warn(format!("Action “{}” not handled by this version of superzed", action.kind), cx);
        }
    }

    /// Voce "GitHub…" del menu di una chat in Fatto: l'anteprima se il report c'e', se no lo chiede.
    pub fn github_for_node(&mut self, node: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_id) = self.session_of(node, cx).and_then(|s| s.session_id) else {
            return;
        };
        let has_report = DoneEnv::get(cx)
            .and_then(|env| env.root(self.project_root_path(cx)))
            .is_some_and(|root| superzed_done::review_source(&root, &session_id).is_ok());
        if has_report {
            self.open_done_review(&session_id, window, cx);
        } else {
            self.start_done_pipeline(node, cx);
            self.warn("Report in progress: a notification arrives when it is ready".into(), cx);
        }
    }

    /// Modale "Aggiorna GitHub" per la sessione, dal report (la copia salvata alla prima
    /// scrittura, se c'e') e dall'esito precedente.
    pub fn open_done_review(&mut self, session_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = DoneEnv::get(cx) else {
            self.warn("Done → GitHub pipeline not initialized".into(), cx);
            return;
        };
        let Some(root) = env.root(self.project_root_path(cx)) else {
            self.warn("The project has no folder".into(), cx);
            return;
        };
        if write_running(&root, session_id, cx) {
            self.warn("Already writing to GitHub for this chat".into(), cx);
            return;
        }
        let (cfg, areas) = match superzed_done::load_pm(&root) {
            Ok(x) => x,
            Err(e) => {
                self.warn(format!("GitHub: {e}"), cx);
                return;
            }
        };
        let source = match superzed_done::review_source(&root, session_id) {
            Ok(s) => s,
            Err(e) => {
                self.warn(format!("Session report not available: {e}"), cx);
                return;
            }
        };
        let mut preview = Preview::from_report(&source.report, &areas, &source.previous);
        preview.repo = Some(cfg.repo.clone());
        let title = self.chat_title_of(self.node_for_session_id(session_id, cx), cx);
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let canvas = cx.entity().downgrade();
        let job = WriteJob {
            session_id: session_id.to_string(),
            chat_title: title.clone(),
            root,
            cfg,
            report: source.report,
            previous: source.previous,
            project_root: self.project_root_path(cx),
        };
        let warning = source.warning;
        workspace.update(cx, |ws, cx| {
            crate::done_review::open_done_review(
                ws,
                title,
                preview,
                warning,
                move |preview, _, cx| {
                    canvas.update(cx, |c, cx| c.write_done(job, preview, cx)).ok();
                },
                window,
                cx,
            )
        });
    }

    /// «Scrivi su GitHub» (l'unico chiamante): `gh` (runner di `DoneEnv`) in un task dell'app.
    fn write_done(&mut self, job: WriteJob, preview: Preview, cx: &mut Context<Self>) {
        let Some(env) = DoneEnv::get(cx) else {
            self.warn("Done → GitHub pipeline not initialized: nothing written".into(), cx);
            return;
        };
        let actions = superzed_done::actions(&preview);
        if actions.is_empty() {
            self.warn("Nothing to write to GitHub".into(), cx);
            return;
        }
        if write_running(&job.root, &job.session_id, cx) {
            self.warn("Already writing to GitHub for this chat".into(), cx);
            return;
        }
        let path = match superzed_done::result_path(&job.root, &job.session_id) {
            Ok(p) => p,
            Err(e) => {
                self.warn(format!("GitHub: {e}"), cx);
                return;
            }
        };
        if let Some(node) = self.node_for_session_id(&job.session_id, cx) {
            self.set_done_status(node, STATUS_WRITING, cx);
        }
        spawn_done_write(cx.entity().downgrade(), env.gh.clone(), job, actions, path, cx);
    }

    /// I pannelli delle fonti GitHub aperti sul canvas si ricaricano (5A).
    fn refresh_github_panels(&mut self, cx: &mut Context<Self>) {
        let store = superzed_sources::ConnectionStore::global(cx);
        let views: Vec<Entity<SourcePanelView>> =
            self.model.frames().iter().filter(|f| f.kind == NodeKind::Source).filter_map(|f| self.source_view(f.id)).collect();
        for view in views {
            let github = store
                .as_ref()
                .and_then(|s| s.read(cx).get(view.read(cx).connection()).map(|c| c.kind() == superzed_sources::ConnectionKind::Github))
                .unwrap_or(false);
            if github {
                view.update(cx, |v, cx| v.refresh(cx));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use super::super::tests::{capture_spec, close_all, hook, leak, pty_after_a_while, redraw, store, wait_for_file};
    use crate::test_support::workspace_with_repo;
    use gpui::{TestAppContext, VisualTestContext};
    use serde_json::json;
    use superzed_done::test_support::{REPORT_JSON, SID, claude_output, write_pm, write_runner};
    use superzed_notifications::{Notification, NotificationFilter};
    use superzed_sessions::Phase;
    use superzed_sources::test_support::{FakeRunner, fail, ok};
    use crate::done_review::DoneReviewModal;
    use super::super::tests::with_sources;
    use superzed_done::{RunResult, test_support::marker_hits};

    #[test]
    fn done_status_maps_to_short_text_level_and_detail() {
        for status in [STATUS_WAITING, STATUS_PROMPTED, STATUS_HEADLESS] {
            let (text, level, detail) = done_status_view(status);
            assert_eq!((text, level), ("Report in progress…", DoneLevel::InProgress), "{status}");
            assert!(detail.starts_with("Report: "), "dettaglio tecnico nel tooltip: {detail}");
        }
        assert_eq!(done_status_view(STATUS_HEADLESS).2, "Report: claude -p running");
        let (text, level, _) = done_status_view(STATUS_WRITING);
        assert_eq!((text, level), ("Writing to GitHub…", DoneLevel::InProgress));
        let (text, level, detail) = done_status_view(STATUS_READY);
        assert_eq!((text, level), ("Report ready", DoneLevel::Ready));
        assert!(detail.contains("review"), "{detail}");
        for status in [STATUS_WRITTEN, STATUS_ALREADY_WRITTEN] {
            assert_eq!(done_status_view(status).0, "Written to GitHub");
            assert_eq!(done_status_view(status).1, DoneLevel::Written);
        }
        assert_eq!(done_status_view(STATUS_ALREADY_WRITTEN).2, "Already written to GitHub");
        let (text, level, detail) = done_status_view(&report_failed_status("disk full"));
        assert_eq!((text, level), ("Report failed", DoneLevel::Error));
        assert_eq!(detail, "Report failed: disk full");
        let (text, level, detail) = done_status_view("GitHub: 2 items not written (chat)");
        assert_eq!((text, level), ("GitHub: error", DoneLevel::Error));
        assert_eq!(detail, "GitHub: 2 items not written (chat)");
        let (text, level, _) = done_status_view("GitHub: the project has no folder");
        assert_eq!((text, level), ("GitHub: error", DoneLevel::Error));
    }

    // Le notifiche «Rivedi» salvate prima dell'interfaccia in inglese restano riconosciute:
    // conta l'azione (tipo e sessione), non l'etichetta.
    #[test]
    fn review_notifications_are_matched_by_kind_and_session_not_label() {
        let old = NotificationAction { kind: superzed_done::REVIEW_ACTION.into(), label: "Rivedi".into(), arg: "s1".into() };
        assert!(is_review_of(Some(&old), "s1"));
        assert!(is_review_of(Some(&review_action("s1")), "s1"));
        assert!(!is_review_of(Some(&old), "s2"));
        assert!(!is_review_of(Some(&NotificationAction { kind: "other".into(), ..old }), "s1"));
        assert!(!is_review_of(None, "s1"));
    }
    use superzed_sources::test_support::{github_connections, github_runner};

    pub(crate) const KEY: &str = "done-chat";
    const TRANSCRIPT: &str = "{\"type\":\"user\",\"message\":\"importa gli utenti\"}\n";

    pub(crate) struct Chat {
        pub canvas: Entity<CanvasView>,
        pub workspace: Entity<Workspace>,
        pub node: NodeId,
        pub dir: tempfile::TempDir,
        pub gh: Arc<FakeRunner>,
        pub claude: Arc<FakeRunner>,
    }

    /// Chat claude agganciata al kanban (in Implementazione) col PTY di cattura, config
    /// claude-pm e transcript in `dir` (la root della pipeline), senza focus; `started`:
    /// claude ha mandato `SessionStart` (Idle, transcript noto).
    pub(crate) async fn done_chat(cx: &mut TestAppContext, started: bool) -> (Chat, &'static mut VisualTestContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        write_pm(dir.path());
        std::fs::write(dir.path().join("transcript.jsonl"), TRANSCRIPT).unwrap();
        let (workspace, canvas, _fs, cx) = workspace_with_repo(cx, json!({})).await;
        let (gh, claude) = (write_runner(), FakeRunner::new());
        cx.update(|_, cx| superzed_done::init_test(cx, gh.clone(), claude.clone(), Some(dir.path().to_path_buf())));
        let init = NodeSessionInit {
            key: KEY.into(),
            account: "a".into(),
            account_id: None,
            session_id: Some(SID.into()),
            phase: Phase::Implementing,
            times: Default::default(),
        };
        let spec = capture_spec(&dir.path().join("pty.txt"));
        canvas.update_in(cx, |c, window, cx| {
            c.open_kanban(window, cx);
            c.open_terminal_with_spec(NodeKind::Claude, None, spec, Some(init), window, cx);
        });
        cx.run_until_parked();
        let node = canvas.read_with(cx, |c, _| c.node_for_session_key(KEY).expect("chat creata"));
        canvas.update(cx, |c, cx| c.dock(node, Phase::Implementing, cx));
        if started {
            let transcript = dir.path().join("transcript.jsonl");
            store(cx).update(cx, |s, cx| {
                s.apply(hook(KEY, json!({"hook_event_name": "SessionStart", "session_id": SID, "source": "startup", "transcript_path": transcript})), cx)
            });
        }
        // l'utente e' altrove: `to_done` (un cambio di fase dello store, come un hook)
        // scrive subito; i test del focus lo rimettono
        store(cx).update(cx, |s, cx| s.set_focused(None, cx));
        cx.run_until_parked();
        (Chat { canvas, workspace, node, dir, gh, claude }, cx)
    }

    /// Spostamento a mano in Fatto (drag nel kanban): il prompt va nella chat.
    pub(crate) fn to_done(chat: &Chat, cx: &mut VisualTestContext) {
        chat.canvas.update(cx, |c, cx| c.set_session_phase(chat.node, Phase::Done, cx));
        cx.run_until_parked();
    }

    /// Spostamento a mano fuori da Fatto.
    fn out_of_done(chat: &Chat, cx: &mut VisualTestContext) {
        chat.canvas.update(cx, |c, cx| c.set_session_phase(chat.node, Phase::Implementing, cx));
        cx.run_until_parked();
    }

    /// `n` controlli della pipeline (ognuno `REPORT_POLL` di orologio finto).
    pub(crate) fn tick(n: u32, cx: &mut VisualTestContext) {
        for _ in 0..n {
            cx.executor().advance_clock(REPORT_POLL);
            cx.run_until_parked();
        }
    }

    pub(crate) fn wait_polls() -> u32 {
        super::wait_polls()
    }

    pub(crate) fn notifications(cx: &mut VisualTestContext) -> Vec<Notification> {
        cx.update(|_, cx| {
            let store = superzed_notifications::NotificationStore::global(cx).unwrap();
            store.read(cx).list(&NotificationFilter { include_archived: true, ..Default::default() })
        })
    }

    fn status(chat: &Chat, cx: &mut VisualTestContext) -> Option<String> {
        chat.canvas.read_with(cx, |c, _| c.done_status(chat.node).map(str::to_string))
    }

    fn pty(chat: &Chat) -> std::path::PathBuf {
        chat.dir.path().join("pty.txt")
    }

    fn write_report_file(chat: &Chat, text: &str) {
        std::fs::write(superzed_done::report_path(chat.dir.path(), SID).unwrap(), text).unwrap();
    }

    fn assert_review_notification(chat: &Chat, cx: &mut VisualTestContext) {
        let list = notifications(cx);
        assert_eq!(list.len(), 1, "{list:?}");
        let n = &list[0];
        assert!(n.title.starts_with("Report ready: "), "{}", n.title);
        assert_eq!(n.severity, Severity::Info);
        assert_eq!(n.source, superzed_done::NOTIFICATION_SOURCE);
        assert_eq!(n.summary, "1 done · 1 blockers · 1 missing · 1 problems · 2 issues touched");
        assert_eq!(n.project_root.as_deref(), Some(std::path::Path::new("/repo")));
        assert_eq!(
            n.action,
            Some(NotificationAction { kind: superzed_done::REVIEW_ACTION.into(), label: "Review".into(), arg: SID.into() })
        );
        assert_eq!(status(chat, cx).as_deref(), Some(STATUS_READY));
        assert_eq!(chat.canvas.read_with(cx, |c, _| c.done_stage(chat.node)), None, "pipeline finita");
    }

    #[gpui::test]
    async fn a_chat_moved_to_done_is_asked_for_the_report(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        let text = wait_for_file(&pty(&chat), "\x1b[201~", cx);
        let path = superzed_done::report_path(chat.dir.path(), SID).unwrap();
        assert!(
            text.contains(&format!("\x1b[200~superzed: questa chat e' passata in Fatto. Scrivi il report della sessione nel file {}", path.display())),
            "{text:?}"
        );
        assert!(text.contains("\"issues_touched\"") && text.contains("una fra frontend, backend"), "{text:?}");
        assert!(chat.dir.path().join(".superzed/reports/.gitignore").exists());
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_PROMPTED));
        assert!(chat.canvas.read_with(cx, |c, cx| c.is_done_chat(chat.node, cx)));
        redraw(cx);
        assert!(cx.debug_bounds(leak(format!("superzed-done-status-{}", chat.node.0))).is_some(), "stato nella barra del titolo");
        assert!(chat.claude.calls().is_empty());
        tick(1, cx);
        assert!(wait_for_file(&pty(&chat), "\x1b[201~\r", cx).ends_with("\x1b[201~\r"), "poi Invio");
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_valid_report_file_raises_the_review_notification(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        tick(1, cx);
        assert!(notifications(cx).is_empty(), "file non ancora scritto");
        write_report_file(&chat, REPORT_JSON);
        tick(1, cx);
        assert_review_notification(&chat, cx);
        assert!(chat.claude.calls().is_empty(), "niente claude -p");
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_half_written_report_is_waited_for(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        write_report_file(&chat, "{\"done\": [\"Import inv");
        tick(5, cx);
        assert!(notifications(cx).is_empty());
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_PROMPTED), "nessun errore, nessun fallback");
        write_report_file(&chat, REPORT_JSON);
        tick(1, cx);
        assert_review_notification(&chat, cx);
        assert!(chat.claude.calls().is_empty());
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_missing_report_falls_back_to_claude_p_after_three_minutes(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.claude.on("claude -p --output-format json", ok(&claude_output(REPORT_JSON)));
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        tick(wait_polls() - 1, cx);
        assert!(chat.claude.calls().is_empty(), "prima dei 3 minuti si aspetta");
        tick(1, cx);
        let calls = chat.claude.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, "/usr/bin/env");
        assert!(!calls[0].args.iter().any(|a| a.starts_with("CLAUDE_CONFIG_DIR=")), "senza AccountStore: sistema");
        let cwd = std::env::temp_dir().join("superzed-done").display().to_string();
        assert_eq!(&calls[0].args[..2], ["-C".to_string(), cwd], "da una cartella temporanea");
        assert!(calls[0].args.ends_with(&["--tools".to_string(), String::new()]), "{:?}", calls[0].args);
        let stdin = calls[0].stdin.clone().unwrap();
        assert!(stdin.contains(superzed_done::TRANSCRIPT_SEPARATOR) && stdin.contains("importa gli utenti"), "{stdin}");
        assert_eq!(superzed_done::read_report(chat.dir.path(), SID).unwrap().missing.len(), 1, "salvato nel file");
        assert_review_notification(&chat, cx);
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn an_ended_chat_is_never_typed_into_and_goes_headless(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.claude.on("claude -p --output-format json", ok(&claude_output(REPORT_JSON)));
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "SessionEnd"})), cx));
        to_done(&chat, cx);
        assert_eq!(chat.claude.calls().len(), 1, "subito, senza aspettare");
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"), "niente nel PTY");
        assert_review_notification(&chat, cx);
        close_all(&chat.canvas, cx);
    }

    // Controller: `claude -p` gira sull'account scelto dallo scheduler, come una chat nuova.
    #[gpui::test]
    async fn the_headless_report_runs_on_the_scheduler_account(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        let accounts = tempfile::tempdir().unwrap();
        cx.update(|_, cx| {
            superzed_accounts::test_support::fixture_store(cx, accounts.path(), &[(90., 20.), (10., 30.)]);
        });
        cx.run_until_parked();
        chat.claude.on("claude -p --output-format json", ok(&claude_output(REPORT_JSON)));
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "SessionEnd"})), cx));
        to_done(&chat, cx);
        let calls = chat.claude.calls();
        assert_eq!(calls.len(), 1);
        let expected = format!("CLAUDE_CONFIG_DIR={}", accounts.path().join("cfg-a2").display());
        assert!(calls[0].args.contains(&expected), "{:?}", calls[0].args);
        assert_review_notification(&chat, cx);
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_busy_chat_is_waited_for_then_prompted(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "UserPromptSubmit"})), cx));
        to_done(&chat, cx);
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_WAITING));
        tick(3, cx);
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"), "occupata: niente nel PTY");
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "Stop"})), cx));
        tick(1, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_PROMPTED));
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_chat_busy_for_three_minutes_goes_headless(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.claude.on("claude -p --output-format json", ok(&claude_output(REPORT_JSON)));
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "UserPromptSubmit"})), cx));
        to_done(&chat, cx);
        tick(wait_polls(), cx);
        assert_eq!(chat.claude.calls().len(), 1);
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"));
        assert_review_notification(&chat, cx);
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_failed_headless_report_is_an_error_notification(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.claude.on("claude -p --output-format json", fail(1, "Credit balance is too low"));
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "SessionEnd"})), cx));
        to_done(&chat, cx);
        let list = notifications(cx);
        assert_eq!(list.len(), 1);
        assert!(list[0].title.starts_with("Report failed: ") && list[0].severity == Severity::Error, "{list:?}");
        assert_eq!(list[0].summary, "claude -p: exited with 1 (Credit balance is too low)");
        assert_eq!(list[0].action, None);
        assert!(status(&chat, cx).unwrap().starts_with("report failed: "));
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn without_claude_pm_config_nothing_happens(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        std::fs::remove_file(chat.dir.path().join(superzed_sources::github::PM_CONFIG)).unwrap();
        to_done(&chat, cx);
        let s = status(&chat, cx).unwrap();
        assert!(s.starts_with("GitHub: claude-pm configuration missing: "), "{s}");
        assert_eq!(chat.canvas.read_with(cx, |c, _| c.done_stage(chat.node)), None);
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"));
        tick(wait_polls(), cx);
        assert!(chat.claude.calls().is_empty() && notifications(cx).is_empty());
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn leaving_done_stops_the_pipeline(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        store(cx).update(cx, |s, cx| s.set_phase(KEY, Phase::Implementing, cx));
        cx.run_until_parked();
        assert_eq!((status(&chat, cx), chat.canvas.read_with(cx, |c, _| c.done_stage(chat.node))), (None, None));
        tick(wait_polls(), cx);
        assert!(chat.claude.calls().is_empty() && notifications(cx).is_empty());
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_free_chat_is_left_alone(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        let kanban = chat.canvas.read_with(cx, |c, _| c.kanban_node().unwrap());
        chat.canvas.update(cx, |c, cx| c.remove_nodes(&[kanban], cx)); // la chat resta, libera
        cx.run_until_parked();
        to_done(&chat, cx);
        assert_eq!(status(&chat, cx), None);
        assert!(!chat.canvas.read_with(cx, |c, cx| c.is_done_chat(chat.node, cx)));
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"));
        close_all(&chat.canvas, cx);
    }

    // Review 1: una chat uscita dal kanban (sganciata o kanban chiuso) non riceve nulla.
    async fn left_kanban_while_waiting(cx: &mut TestAppContext, close_kanban: bool) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.claude.on("claude -p --output-format json", ok(&claude_output(REPORT_JSON)));
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "UserPromptSubmit"})), cx));
        to_done(&chat, cx);
        assert_eq!(chat.canvas.read_with(cx, |c, _| c.done_stage(chat.node)), Some(DoneStage::WaitingChat));
        if close_kanban {
            let kanban = chat.canvas.read_with(cx, |c, _| c.kanban_node().unwrap());
            chat.canvas.update(cx, |c, cx| c.remove_nodes(&[kanban], cx));
        } else {
            chat.canvas.update(cx, |c, cx| c.undock(chat.node, cx));
        }
        cx.run_until_parked();
        assert_eq!((status(&chat, cx), chat.canvas.read_with(cx, |c, _| c.done_stage(chat.node))), (None, None));
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "Stop"})), cx));
        tick(wait_polls() + 1, cx);
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"), "chat libera: niente nel PTY");
        assert!(chat.claude.calls().is_empty() && notifications(cx).is_empty());
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn an_undocked_chat_is_never_prompted(cx: &mut TestAppContext) {
        left_kanban_while_waiting(cx, false).await;
    }

    #[gpui::test]
    async fn closing_the_kanban_stops_the_pipeline(cx: &mut TestAppContext) {
        left_kanban_while_waiting(cx, true).await;
    }

    // Review 2: rientrare in Fatto tiene report ed esito, non richiede nulla e non duplica
    // la notifica ancora nuova.
    #[gpui::test]
    async fn reentering_done_keeps_the_report_and_its_result(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        write_report_file(&chat, REPORT_JSON);
        tick(1, cx);
        assert_review_notification(&chat, cx);
        let result = superzed_done::result_path(chat.dir.path(), SID).unwrap();
        let result_text = r#"{"version": 1, "items": {"summary": {"ok": true, "message": "scritto"}}}"#;
        std::fs::write(&result, result_text).unwrap();
        let report_text = std::fs::read_to_string(superzed_done::report_path(chat.dir.path(), SID).unwrap()).unwrap();
        store(cx).update(cx, |s, cx| s.set_phase(KEY, Phase::Implementing, cx));
        cx.run_until_parked();
        assert_eq!(status(&chat, cx), None);
        to_done(&chat, cx);
        assert_eq!(std::fs::read_to_string(superzed_done::report_path(chat.dir.path(), SID).unwrap()).unwrap(), report_text);
        assert_eq!(std::fs::read_to_string(&result).unwrap(), result_text);
        tick(wait_polls(), cx);
        assert_eq!(pty_after_a_while(&pty(&chat), cx).matches("\x1b[200~superzed:").count(), 1, "nessun secondo prompt");
        assert_review_notification(&chat, cx); // una sola: quella nuova resta
        assert!(chat.claude.calls().is_empty());
        // presa in carico: al rientro si ripropone
        let id = notifications(cx)[0].id.clone();
        cx.update(|_, cx| {
            let store = superzed_notifications::NotificationStore::global(cx).unwrap();
            store.update(cx, |s, cx| s.set_state(&id, superzed_notifications::NotificationState::Taken, cx));
        });
        store(cx).update(cx, |s, cx| s.set_phase(KEY, Phase::Implementing, cx));
        to_done(&chat, cx);
        let list = notifications(cx);
        assert_eq!(list.len(), 2, "{list:?}");
        assert!(list.iter().all(|n| n.action == Some(review_action(SID))));
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_READY));
        assert!(result.exists());
        close_all(&chat.canvas, cx);
    }

    /// Fatto da un hook (`finishing-a-development-branch`), con o senza il focus nella chat.
    fn automatic_done(focused: bool, cx: &mut VisualTestContext) {
        if focused {
            store(cx).update(cx, |s, cx| s.set_focused(Some(KEY.into()), cx));
        }
        let skill = json!({"hook_event_name": "PreToolUse", "tool_name": "Skill", "tool_input": {"skill": "superpowers:finishing-a-development-branch"}});
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, skill), cx));
        cx.run_until_parked();
        assert_eq!(store(cx).read_with(cx, |s, _| s.get(KEY).unwrap().phase), Phase::Done);
        store(cx).update(cx, |s, cx| s.apply(hook(KEY, json!({"hook_event_name": "Stop"})), cx));
        cx.run_until_parked();
    }

    // Revisione finale 2: Fatto arrivato da solo non scrive mai nella chat, subito claude -p.
    async fn automatic_done_goes_headless(cx: &mut TestAppContext, focused: bool) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.claude.on("claude -p --output-format json", ok(&claude_output(REPORT_JSON)));
        automatic_done(focused, cx);
        assert_eq!(chat.claude.calls().len(), 1, "subito, senza aspettare");
        tick(wait_polls() + 1, cx);
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"), "mai nella chat");
        assert_eq!(chat.claude.calls().len(), 1);
        assert_review_notification(&chat, cx);
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn an_automatic_done_goes_headless_while_focused(cx: &mut TestAppContext) {
        automatic_done_goes_headless(cx, true).await;
    }

    #[gpui::test]
    async fn an_automatic_done_goes_headless_even_without_focus(cx: &mut TestAppContext) {
        automatic_done_goes_headless(cx, false).await;
    }

    #[gpui::test]
    async fn a_manual_move_types_even_while_focused(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        store(cx).update(cx, |s, cx| s.set_focused(Some(KEY.into()), cx));
        chat.canvas.update(cx, |c, cx| c.set_session_phase(chat.node, Phase::Done, cx));
        cx.run_until_parked();
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_PROMPTED));
        close_all(&chat.canvas, cx);
    }

    // Review 4: claude che parte (niente SessionStart) si aspetta, non va subito in claude -p.
    #[gpui::test]
    async fn a_starting_chat_is_waited_for(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, false).await;
        to_done(&chat, cx);
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_WAITING));
        tick(3, cx);
        assert!(chat.claude.calls().is_empty() && notifications(cx).is_empty());
        let transcript = chat.dir.path().join("transcript.jsonl");
        store(cx).update(cx, |s, cx| {
            s.apply(hook(KEY, json!({"hook_event_name": "SessionStart", "session_id": SID, "source": "startup", "transcript_path": transcript})), cx)
        });
        tick(1, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_PROMPTED));
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_chat_starting_for_three_minutes_goes_headless(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, false).await;
        to_done(&chat, cx);
        tick(wait_polls(), cx);
        assert!(!pty_after_a_while(&pty(&chat), cx).contains("superzed:"));
        assert_eq!(chat.claude.calls().len(), 0, "senza transcript niente claude -p");
        assert!(status(&chat, cx).unwrap().starts_with("report failed: "));
        close_all(&chat.canvas, cx);
    }

    fn with_report(chat: &Chat) {
        superzed_done::write_report(chat.dir.path(), &superzed_done::parse_report(REPORT_JSON, SID).unwrap()).unwrap();
    }

    fn review(chat: &Chat, cx: &mut VisualTestContext) -> Entity<DoneReviewModal> {
        chat.canvas.update_in(cx, |c, window, cx| c.open_done_review(SID, window, cx));
        cx.run_until_parked();
        chat.workspace.read_with(cx, |ws, cx| ws.active_modal::<DoneReviewModal>(cx)).expect("anteprima aperta")
    }

    fn review_open(chat: &Chat, cx: &mut VisualTestContext) -> bool {
        chat.workspace.read_with(cx, |ws, cx| ws.active_modal::<DoneReviewModal>(cx).is_some())
    }

    fn write_all(modal: &Entity<DoneReviewModal>, cx: &mut VisualTestContext) {
        modal.update_in(cx, |m, window, cx| m.write(window, cx));
        cx.run_until_parked();
    }

    fn result_of(chat: &Chat) -> RunResult {
        RunResult::read(&superzed_done::result_path(chat.dir.path(), SID).unwrap())
    }

    #[gpui::test]
    async fn review_and_write_run_the_expected_gh_sequence(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        with_report(&chat);
        write_all(&review(&chat, cx), cx);
        let gh = &chat.gh;
        assert!(gh.calls()[0].line().starts_with("gh auth status"));
        for (needle, n) in [
            ("gh project view 3 --owner acme", 1),
            ("gh project field-list 3 --owner acme", 1),
            ("gh issue list --repo acme/acme_app --state all --search=", 1),
            ("gh issue create", 3),
            ("gh project item-add", 4),
            ("gh project item-edit", 11),
            ("gh issue close 12 --repo acme/acme_app --comment=Import completato", 1),
            ("gh issue comment 8 --repo acme/acme_app --body=Serve l'accesso allo staging", 1),
            ("gh issue comment 12 --repo acme/acme_app --body=Riepilogo della sessione superzed", 1),
        ] {
            assert_eq!(gh.count(needle), n, "{needle}");
        }
        assert!(chat.claude.calls().is_empty());
        let result = result_of(&chat);
        assert_eq!((result.items.len(), result.failed().len(), result.known_issues().len()), (6, 0, 3));
        let outcome = notifications(cx).into_iter().find(|n| n.title.starts_with("GitHub updated: ")).expect("notifica di esito");
        assert_eq!((outcome.severity, outcome.summary.as_str(), outcome.action.clone()), (Severity::Info, "6 items written", None));
        assert_eq!(chat.canvas.read_with(cx, |c, _| c.done_status(chat.node).map(str::to_string)), Some(STATUS_WRITTEN.into()));
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn a_second_write_never_creates_the_same_issue_twice(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        with_report(&chat);
        write_all(&review(&chat, cx), cx);
        assert_eq!(chat.gh.count("gh issue create"), 3);
        // di nuovo: tutto gia' scritto
        let modal = review(&chat, cx);
        assert_eq!(modal.read_with(cx, |m, cx| m.pending(cx)), 0);
        cx.dispatch_action(menu::Cancel);
        cx.run_until_parked();
        // esito perso (crash prima del salvataggio): i marker trovati su GitHub evitano i doppioni
        std::fs::remove_file(superzed_done::result_path(chat.dir.path(), SID).unwrap()).unwrap();
        chat.gh.on("gh issue list --repo acme/acme_app --state all --search=", ok(&marker_hits(SID, &[(0, 40), (1, 41), (2, 42)])));
        write_all(&review(&chat, cx), cx);
        assert_eq!(chat.gh.count("gh issue create"), 3, "nessuna create in piu'");
        assert_eq!((chat.gh.count("gh issue edit 40"), chat.gh.count("gh issue edit 41"), chat.gh.count("gh issue edit 42")), (1, 1, 1));
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn failed_items_are_retried_alone(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        with_report(&chat);
        chat.gh.on("gh issue close", fail(1, "HTTP 502: bad gateway"));
        write_all(&review(&chat, cx), cx);
        let failed = notifications(cx).into_iter().find(|n| n.severity == Severity::Error).expect("notifica d'errore");
        assert!(failed.title.starts_with("GitHub: 1 item not written ("), "{}", failed.title);
        assert_eq!(failed.summary, "#12 (close): gh: HTTP 502: bad gateway");
        assert_eq!(failed.action, Some(review_action(SID)), "«Rivedi» per riprovare");
        chat.gh.on("gh issue close", ok(""));
        let creates = chat.gh.count("gh issue create");
        let modal = review(&chat, cx);
        assert_eq!(modal.read_with(cx, |m, cx| m.pending(cx)), 1, "solo la chiusura");
        write_all(&modal, cx);
        assert_eq!(chat.gh.count("gh issue create"), creates);
        assert_eq!(chat.gh.count("gh issue close 12"), 2);
        assert!(result_of(&chat).failed().is_empty());
        let last = notifications(cx).into_iter().rfind(|n| n.title.starts_with("GitHub updated: ")).unwrap();
        assert_eq!(last.summary, "1 item written", "revisione finale 6: solo le voci di questa scrittura");
        let failed = notifications(cx).into_iter().find(|n| n.severity == Severity::Error).unwrap();
        assert_eq!(failed.state, NotificationState::Taken, "riprovata con successo: presa in carico");
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn the_notification_and_the_node_menu_open_the_review(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        write_report_file(&chat, REPORT_JSON);
        tick(1, cx);
        let action = notifications(cx)[0].action.clone().expect("Rivedi");
        chat.canvas.update_in(cx, |c, window, cx| c.open_notification_action(&action, window, cx));
        cx.run_until_parked();
        assert!(review_open(&chat, cx));
        cx.dispatch_action(menu::Cancel);
        cx.run_until_parked();
        assert!(!review_open(&chat, cx));
        assert!(chat.canvas.read_with(cx, |c, cx| c.is_done_chat(chat.node, cx)), "voce GitHub… nel menu");
        chat.canvas.update_in(cx, |c, window, cx| c.github_for_node(chat.node, window, cx));
        cx.run_until_parked();
        assert!(review_open(&chat, cx), "GitHub… con il report pronto: anteprima");
        assert!(chat.gh.calls().is_empty(), "aprire l'anteprima non chiama gh");
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn github_on_a_chat_without_report_asks_for_it(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.canvas.update_in(cx, |c, window, cx| c.github_for_node(chat.node, window, cx));
        cx.run_until_parked();
        assert!(!review_open(&chat, cx));
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        assert_eq!(chat.canvas.read_with(cx, |c, _| c.done_stage(chat.node)), Some(DoneStage::Prompted));
        close_all(&chat.canvas, cx);
    }

    #[gpui::test]
    async fn github_panels_refresh_after_a_write(cx: &mut TestAppContext) {
        let sources = github_runner();
        let (_store, _sources_dir) = with_sources(cx, sources.clone(), github_connections());
        let (chat, cx) = done_chat(cx, true).await;
        with_report(&chat);
        chat.canvas.update_in(cx, |c, window, cx| {
            c.new_source_panel("gh", window, cx);
        });
        cx.run_until_parked();
        let before = sources.count("gh project item-list");
        assert!(before >= 1);
        write_all(&review(&chat, cx), cx);
        assert_eq!(sources.count("gh project item-list"), before + 1, "il pannello GitHub si ricarica");
        close_all(&chat.canvas, cx);
    }

    /// `gh` finto che si ferma su un comando finche' non si apre il cancello.
    struct Gated {
        inner: Arc<FakeRunner>,
        needle: &'static str,
        gate: futures::future::Shared<futures::channel::oneshot::Receiver<()>>,
    }

    impl superzed_sources::Runner for Gated {
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

    // Revisione finale 1: la scrittura non e' del canvas (sopravvive se si chiude), salva
    // l'esito dopo ogni voce e blocca una seconda scrittura della stessa sessione.
    #[gpui::test]
    async fn the_write_survives_the_canvas_and_saves_every_item(cx: &mut TestAppContext) {
        use futures::FutureExt as _;
        let (chat, cx) = done_chat(cx, true).await;
        with_report(&chat);
        let (open, gate) = futures::channel::oneshot::channel::<()>();
        let gated = Arc::new(Gated { inner: chat.gh.clone(), needle: "gh issue close", gate: gate.shared() });
        let root = chat.dir.path().to_path_buf();
        let report = superzed_done::read_report(&root, SID).unwrap();
        let (cfg, areas) = superzed_done::load_pm(&root).unwrap();
        let preview = Preview::from_report(&report, &areas, &RunResult::default());
        let job = WriteJob {
            session_id: SID.into(),
            chat_title: "Import utenti".into(),
            root: root.clone(),
            cfg,
            report: report.clone(),
            previous: RunResult::default(),
            project_root: Some("/repo".into()),
        };
        let path = superzed_done::result_path(&root, SID).unwrap();
        // canvas gia' chiuso: nessuno a cui tornare
        cx.update(|_, cx| spawn_done_write(WeakEntity::new_invalid(), gated, job, superzed_done::actions(&preview), path, cx));
        cx.run_until_parked();
        assert!(cx.update(|_, cx| write_running(&root, SID, cx)), "guardia globale");
        chat.canvas.update_in(cx, |c, window, cx| c.open_done_review(SID, window, cx));
        assert!(!review_open(&chat, cx), "stessa sessione: niente seconda scrittura");
        assert_eq!(
            chat.canvas.read_with(cx, |c, _| c.last_warning().map(str::to_string)).as_deref(),
            Some("Already writing to GitHub for this chat")
        );
        let mid = result_of(&chat);
        assert!(["new:0", "new:1", "new:2"].iter().all(|k| mid.is_written(k)), "salvate prima della fine: {mid:?}");
        assert!(!mid.is_written("touched:12") && mid.snapshot(SID) == Some(report.clone()));
        // interrotta qui (app chiusa): la riprova rifa' solo il resto, mai i commenti gia' fatti
        let retry = Preview::from_report(&report, &areas, &mid);
        assert_eq!(superzed_done::actions(&retry).iter().map(superzed_done::Action::key).collect::<Vec<_>>(), ["touched:12", "touched:8", "summary"]);
        open.send(()).unwrap();
        cx.run_until_parked();
        assert!(!cx.update(|_, cx| write_running(&root, SID, cx)));
        assert_eq!((result_of(&chat).items.len(), result_of(&chat).failed().len()), (6, 0));
        let outcome = notifications(cx).into_iter().find(|n| n.title.starts_with("GitHub updated: ")).expect("notifica anche senza canvas");
        assert_eq!(outcome.summary, "6 items written");
        assert_eq!((chat.gh.count("gh issue close"), chat.gh.count("gh issue comment 12")), (1, 1));
        close_all(&chat.canvas, cx);
    }

    // Revisione finale 6: dopo una scrittura completa la notifica «Report pronto» e' presa in
    // carico; rientrare in Fatto dice "già scritto" e non ripropone «Rivedi».
    #[gpui::test]
    async fn a_fully_written_report_is_not_raised_again(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        write_report_file(&chat, REPORT_JSON);
        tick(1, cx);
        assert_eq!(notifications(cx)[0].state, NotificationState::New);
        write_all(&review(&chat, cx), cx);
        let list = notifications(cx);
        let ready = list.iter().find(|n| n.title.starts_with("Report ready: ")).unwrap();
        assert_eq!(ready.state, NotificationState::Taken, "scritto tutto: presa in carico");
        out_of_done(&chat, cx);
        to_done(&chat, cx);
        assert_eq!(status(&chat, cx).as_deref(), Some(STATUS_ALREADY_WRITTEN));
        assert_eq!(notifications(cx).len(), list.len(), "nessuna nuova «Rivedi»");
        assert_eq!(pty_after_a_while(&pty(&chat), cx).matches("\x1b[200~superzed:").count(), 1, "nessun prompt");
        close_all(&chat.canvas, cx);
    }

    // Revisione finale 6: pipeline tolta prima dell'Invio → Ctrl-U, nessun testo lasciato.
    #[gpui::test]
    async fn a_run_stopped_before_enter_clears_the_prompt(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        out_of_done(&chat, cx);
        tick(1, cx);
        let text = wait_for_file(&pty(&chat), "\x1b[201~\x15", cx);
        assert!(!text.contains('\r'), "nessun Invio: {text:?}");
        close_all(&chat.canvas, cx);
    }

    // Revisione finale 6: file non valido e claude libero da `IDLE_GRACE`: claude -p subito.
    #[gpui::test]
    async fn an_invalid_report_with_an_idle_chat_falls_back_early(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        chat.claude.on("claude -p --output-format json", ok(&claude_output(REPORT_JSON)));
        to_done(&chat, cx);
        wait_for_file(&pty(&chat), "\x1b[201~", cx);
        write_report_file(&chat, "{\"done\": \"non una lista\"}");
        tick(idle_polls() - 1, cx);
        assert!(chat.claude.calls().is_empty(), "prima dei 30 s si aspetta");
        tick(1, cx);
        assert_eq!(chat.claude.calls().len(), 1);
        assert_review_notification(&chat, cx);
        close_all(&chat.canvas, cx);
    }

    // Revisione finale 5: dopo la prima scrittura l'anteprima viene dalla copia; un report
    // riscritto non rimappa le voci e il modale lo dice.
    #[gpui::test]
    async fn a_rewritten_report_keeps_the_snapshot_and_warns(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        with_report(&chat);
        chat.gh.on("gh issue close", fail(1, "HTTP 502: bad gateway"));
        write_all(&review(&chat, cx), cx);
        let mut other = superzed_done::parse_report(REPORT_JSON, SID).unwrap();
        other.missing.clear(); // gli indici delle voci nuove scorrerebbero
        superzed_done::write_report(chat.dir.path(), &other).unwrap();
        let modal = review(&chat, cx);
        modal.read_with(cx, |m, cx| {
            assert_eq!(m.warning(), Some(superzed_done::REPORT_CHANGED));
            assert_eq!(m.preview().new_issues.len(), 3, "le voci della copia");
            assert_eq!(m.pending(cx), 1, "solo la chiusura fallita");
        });
        redraw(cx);
        assert!(cx.debug_bounds("superzed-done-warning").is_some());
        close_all(&chat.canvas, cx);
    }

    // Revisione finale 6: «Rivedi» con l'anteprima gia' aperta non la chiude.
    #[gpui::test]
    async fn review_again_keeps_the_modal_open(cx: &mut TestAppContext) {
        let (chat, cx) = done_chat(cx, true).await;
        with_report(&chat);
        let first = review(&chat, cx);
        let again = review(&chat, cx);
        assert_eq!(first, again, "la stessa, aperta");
        close_all(&chat.canvas, cx);
    }
}
