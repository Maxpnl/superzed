//! Automazioni del canvas: la definizione di ogni chat agganciata alla sezione
//! Automazioni, il form che le crea e le modifica, e le righe delle fasce di controllo.
use super::*;
use crate::automation::{AutomationDef, AutomationDraft, Decision, Outcome, Readiness, RunPhase};
use crate::automation_form::{self, AccountChoice};
use crate::schedule;
use chrono::{DateTime, Local};

/// Stato in memoria di un'automazione: la definizione (salvata nel `data` del nodo) e
/// l'esecuzione in corso.
pub struct AutomationRuntime {
    pub def: AutomationDef,
    /// Da quando si contano le scadenze di un'automazione mai eseguita.
    pub anchor: DateTime<Local>,
    pub phase: RunPhase,
    /// Notifiche arrivate durante l'esecuzione in corso.
    pub notified: u32,
    /// Quando e' partito il prompt dell'esecuzione in corso.
    pub prompt_sent: Option<DateTime<Local>>,
    /// Perche' l'ultima esecuzione e' fallita, se lo sappiamo (solo in memoria).
    pub note: Option<String>,
    pub(crate) _timeout: Option<Task<()>>,
}

impl AutomationRuntime {
    /// `now`: ora di creazione (o del ripristino di una definizione che non la salvava);
    /// l'ancora e' la creazione salvata nella definizione.
    pub fn new(mut def: AutomationDef, now: DateTime<Local>) -> Self {
        let anchor = *def.created_at.get_or_insert(now);
        Self { def, anchor, phase: RunPhase::Idle, notified: 0, prompt_sent: None, note: None, _timeout: None }
    }
}

/// Attesa tra il paste del prompt e l'Invio (claude deve aver ricevuto tutto il testo).
pub const ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// Nota mostrata nella fascia quando dopo il `/clear` claude non ha dato segni di vita.
pub const NO_RESPONSE: &str = "claude not responding";

/// Una fascia di controllo sopra una chat delle Automazioni.
#[derive(Clone, Debug, PartialEq)]
pub struct AutomationRow {
    pub node: NodeId,
    pub name: String,
    pub schedule: String,
    /// Prossima esecuzione (o "in pausa"); l'errore se la frequenza non e' valida.
    pub next: Result<String, String>,
    pub last_outcome: Option<String>,
    /// L'ultima esecuzione e' fallita.
    pub failed: bool,
    /// Quando e' partita l'ultima esecuzione («oggi 09:00», «ieri 18:30», data).
    pub last_run: Option<String>,
    pub paused: bool,
    pub running: bool,
    /// La chat dell'automazione aspetta una risposta (permesso, domanda).
    pub waiting_input: bool,
    /// Angolo in alto a sinistra della fascia, in coordinate del canvas.
    pub band: Point<f32>,
    pub width: f32,
    /// Altezza della fascia (in rem, convertita), la stessa usata dal layout.
    pub height: f32,
}

impl CanvasView {
    pub fn now(&self) -> DateTime<Local> {
        #[cfg(test)]
        if let Some(t) = self.test_now {
            return t;
        }
        Local::now()
    }

    /// "+" della sezione: apre il form di una nuova automazione.
    pub fn new_automation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let accounts = self.automation_account_choices(cx);
        let canvas = cx.entity().downgrade();
        workspace.update(cx, |ws, cx| {
            automation_form::open_automation_form(
                ws,
                "New automation",
                None,
                accounts,
                move |d, window, cx| {
                    canvas.update(cx, |c, cx| c.create_automation(d, window, cx)).ok();
                },
                window,
                cx,
            )
        });
    }

    /// Voci del menu Account del form: "Automatic", poi gli account registrati.
    fn automation_account_choices(&self, cx: &App) -> Vec<AccountChoice> {
        let mut choices = vec![AccountChoice::automatic()];
        if let Some(store) = superzed_accounts::AccountStore::global(cx) {
            choices.extend(store.read(cx).accounts().iter().map(|a| AccountChoice {
                id: Some(a.id.clone()),
                label: a.email.clone().unwrap_or_else(|| a.label.clone()).into(),
            }));
        }
        choices
    }

    /// Chat claude nuova agganciata alle Automazioni (create se mancano), con la sua
    /// definizione e il nome come titolo.
    pub fn create_automation(&mut self, draft: AutomationDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.open_automation_chat(AutomationDef::from_draft(draft), window, cx);
    }

    /// Chat claude nuova per `def`, sull'account della definizione (o dello scheduler).
    fn open_automation_chat(&mut self, def: AutomationDef, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_container(DockKind::Automations, cx);
        self.dock_next_claude = Some(DockKind::Automations);
        self.next_claude_account = def.account.clone();
        self.next_claude_automation = Some(def);
        self.open_new_claude(None, window, cx);
        // se `open_terminal_with_spec` e' uscito prima di leggerli, non restano accesi
        self.dock_next_claude = None;
        self.next_claude_automation = None;
        self.next_claude_account = None;
    }

    /// Pulsante Modifica della fascia: il form con i campi attuali.
    pub fn edit_automation(&mut self, node: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(initial) = self.automation(node).map(AutomationDef::draft) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let accounts = self.automation_account_choices(cx);
        let canvas = cx.entity().downgrade();
        workspace.update(cx, |ws, cx| {
            automation_form::open_automation_form(
                ws,
                "Edit automation",
                Some(initial),
                accounts,
                move |d, window, cx| {
                    canvas.update(cx, |c, cx| c.update_automation(node, d, window, cx)).ok();
                },
                window,
                cx,
            )
        });
    }

    /// Nome, frequenza, prompt e account nuovi; il titolo della chat segue il nome. Un
    /// account diverso da quello della chat la riapre su quello (claude non cambia account
    /// a processo avviato).
    pub fn update_automation(&mut self, node: NodeId, draft: AutomationDraft, window: &mut Window, cx: &mut Context<Self>) {
        let relaunch = draft.account.as_deref().is_some_and(|id| self.chat_account_id(node, cx).as_deref() != Some(id));
        let Some(runtime) = self.automations.get_mut(&node) else {
            return;
        };
        runtime.def.apply_draft(draft);
        if relaunch {
            self.relaunch_automation_chat(node, window, cx);
            return;
        }
        let name = runtime.def.name.clone();
        if let Some(f) = self.model.frame_mut(node) {
            f.title = name;
        }
        self.nodes_changed(cx);
    }

    /// Account della chat del nodo, dallo store delle sessioni (`None` se non e' nota).
    fn chat_account_id(&self, node: NodeId, cx: &App) -> Option<String> {
        let key = self.session_key(node)?;
        let store = superzed_sessions::SessionStore::global(cx)?;
        store.read(cx).get(key).map(|s| s.account_id.clone())
    }

    /// Chiude la chat dell'automazione e ne apre una nuova con la stessa definizione
    /// (ultima esecuzione e scadenze comprese).
    fn relaunch_automation_chat(&mut self, node: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        // tolta prima: `remove_nodes` la annuncerebbe come rimossa
        let Some(runtime) = self.automations.remove(&node) else {
            return;
        };
        self.remove_nodes(&[node], cx);
        self.open_automation_chat(runtime.def, window, cx);
    }

    pub fn toggle_automation_pause(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let Some(runtime) = self.automations.get_mut(&node) else {
            return;
        };
        runtime.def.paused = !runtime.def.paused;
        self.nodes_changed(cx);
    }

    /// Intestazione e fasce delle Automazioni dal rem dell'interfaccia (lo stesso di
    /// `window.rem_size()`, che segue `ui_font_size`): servono anche dove non c'e' la
    /// finestra (layout, hit-test).
    pub fn automations_metrics(&self, cx: &App) -> automations_layout::Metrics {
        automations_layout::Metrics::for_rem(self.ui_rem(cx))
    }

    pub fn automation(&self, node: NodeId) -> Option<&AutomationDef> {
        self.automations.get(&node).map(|r| &r.def)
    }

    /// Le fasce di controllo, una per chat delle Automazioni con una definizione.
    pub fn automation_rows(&self, cx: &App) -> Vec<AutomationRow> {
        let Some(origin) = self
            .container_node(DockKind::Automations)
            .and_then(|a| self.model.frame(a))
            .map(|f| f.origin)
        else {
            return Vec::new();
        };
        let members = self.dock_members(DockKind::Automations);
        let chat_size = self.chat_size(DockKind::Automations);
        let metrics = self.automations_metrics(cx);
        let layout = automations_layout::layout(origin, &members, chat_size, metrics);
        let now = self.now();
        layout
            .bands
            .into_iter()
            .filter_map(|(node, band)| {
                let runtime = self.automations.get(&node)?;
                let def = &runtime.def;
                let next = if def.paused {
                    Ok("paused".to_string())
                } else {
                    schedule::parse(&def.schedule)
                        .map(|s| {
                            let next = schedule::next_run(&s, def.last_run, runtime.anchor, def.catch_up, now);
                            format!("next: {}", schedule::describe_next(next, now))
                        })
                        .map_err(|e| format!("invalid schedule: {e}"))
                };
                let last_outcome = def.last_outcome.map(|o| match (&runtime.note, o) {
                    (Some(note), Outcome::Failed) => format!("{} ({note})", o.label()),
                    _ => o.label(),
                });
                let waiting_input = matches!(
                    self.chat_session_state(node, cx),
                    Some(superzed_sessions::SessionState::NeedsInput(_))
                );
                Some(AutomationRow {
                    node,
                    name: def.name.clone(),
                    schedule: def.schedule.clone(),
                    next,
                    last_outcome,
                    failed: def.last_outcome == Some(Outcome::Failed),
                    last_run: def.last_run.map(|t| schedule::describe_past(t, now)),
                    paused: def.paused,
                    running: runtime.phase != RunPhase::Idle,
                    waiting_input,
                    band,
                    width: chat_size.width,
                    height: metrics.band,
                })
            })
            .collect()
    }

    /// Toglie la definizione (la chat resta, libera) e lo dice con un toast.
    pub fn remove_automation(&mut self, node: NodeId, cx: &mut Context<Self>) {
        if let Some(runtime) = self.automations.remove(&node) {
            self.set_automation_background(node, false, cx);
            self.warn(format!("Automation “{}” removed", runtime.def.name), cx);
            self.schedule_save(cx);
        }
    }

    /// Sessione di fondo nello store (nessuna fase automatica, nessuna campana) per la
    /// chat del nodo, se la sua chiave e' nota.
    pub(crate) fn set_automation_background(&self, node: NodeId, on: bool, cx: &mut App) {
        let key = self.session_key(node).map(str::to_string).or_else(|| self.pending_sessions.get(&node).cloned());
        if let (Some(key), Some(store)) = (key, superzed_sessions::SessionStore::global(cx)) {
            store.update(cx, |s, _| s.set_background(&key, on));
        }
    }

    /// Stato della sessione claude della chat del nodo, se registrata nello store.
    pub(crate) fn chat_session_state(&self, node: NodeId, cx: &App) -> Option<superzed_sessions::SessionState> {
        let key = self.session_key(node)?;
        let store = superzed_sessions::SessionStore::global(cx)?;
        store.read(cx).get(key).map(|s| s.state.clone())
    }

    /// Se nella chat del nodo si puo' scrivere adesso: solo in un claude vivo e libero. Un
    /// claude che non ha ancora mandato `SessionStart` (`Starting`), uscito (`Ended`) o
    /// senza sessione non riceve nulla.
    pub(crate) fn chat_readiness(&self, node: NodeId, cx: &App) -> Readiness {
        use superzed_sessions::SessionState;
        // niente controllo del processo in primo piano: il nodo lancia claude dentro
        // `$SHELL -l -c "…"` senza job control, quindi il gruppo in primo piano resta quello
        // della shell per tutta la vita di claude (sembrerebbe sempre "zsh")
        if !self.terminals.contains_key(&node) {
            return Readiness::Unavailable;
        }
        match self.chat_session_state(node, cx) {
            Some(SessionState::Idle | SessionState::Done | SessionState::Failed(_)) => Readiness::Ready,
            Some(SessionState::Working | SessionState::NeedsInput(_)) => Readiness::Busy,
            Some(SessionState::Starting | SessionState::Ended) | None => Readiness::Unavailable,
        }
    }

    /// Tick delle automazioni (ogni `TICK`): avvia quelle dovute con la chat libera,
    /// segna come saltate quelle rimaste occupate per un'intera scadenza.
    pub fn automation_tick(&mut self, cx: &mut Context<Self>) {
        if self.automations.is_empty() {
            return;
        }
        let now = self.now();
        let nodes: Vec<NodeId> = self.automations.keys().copied().collect();
        for node in nodes {
            self.close_stale_run(node, now, cx);
            let readiness = self.chat_readiness(node, cx);
            let Some(rt) = self.automations.get_mut(&node) else {
                continue;
            };
            match crate::automation::decide(&rt.def, rt.anchor, now, readiness, rt.phase != RunPhase::Idle) {
                Decision::Run => self.start_automation_run(node, cx),
                Decision::SkipBusy => {
                    rt.def.last_outcome = Some(Outcome::SkippedBusy);
                    rt.def.last_run = Some(now);
                    self.schedule_save(cx);
                }
                Decision::Nothing => {}
            }
        }
        cx.notify();
    }

    /// Avvio di un'esecuzione: `/clear`, poi il prompt al nuovo id di sessione (o dopo
    /// `CLEAR_TIMEOUT`).
    fn start_automation_run(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let Some((_, terminal)) = self.terminals.get(&node).cloned() else {
            return;
        };
        let now = self.now();
        let Some(rt) = self.automations.get_mut(&node) else {
            return;
        };
        rt.def.last_run = Some(now);
        rt.phase = RunPhase::Clearing;
        rt.notified = 0;
        rt.prompt_sent = None;
        rt.note = None;
        // ctrl-u svuota un input scritto a meta', poi /clear
        terminal.update(cx, |t, _| t.input("\x15/clear\r".as_bytes()));
        // se il SessionStart del /clear non arriva claude non sta rispondendo: niente prompt
        rt._timeout = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::automation::CLEAR_TIMEOUT).await;
            this.update(cx, |c, cx| {
                if c.automation_phase(node) == Some(RunPhase::Clearing) {
                    c.fail_automation_run(node, Some(NO_RESPONSE.to_string()), cx);
                }
            })
            .ok();
        }));
        self.schedule_save(cx);
        cx.notify();
    }

    /// `SessionIdChanged` o `Cleared` della chiave: se la sua chat di automazione stava
    /// svuotando il prompt, parte il prompt. Il secondo dei due trova la fase gia'
    /// `Running` e non fa nulla.
    pub(crate) fn automation_cleared(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(node) = self.node_for_session_key(key)
            && self.automation_phase(node) == Some(RunPhase::Clearing)
        {
            self.automation_send_prompt(node, cx);
        }
    }

    /// Incolla `text` nella chat del nodo (sempre bracketed paste: claude e' vivo e lo
    /// accetta; la modalita' in cache del terminale e' vecchia finche' non e' disegnato, e
    /// senza ogni a capo manderebbe una riga da sola) e dopo `ENTER_DELAY` manda Invio. Chi
    /// chiama ha gia' controllato `chat_readiness == Ready`. Il `Task` restituito porta
    /// l'Invio: va tenuto finche' non e' partito.
    pub(crate) fn type_prompt(&self, node: NodeId, text: &str, cx: &mut Context<Self>) -> Option<Task<()>> {
        let terminal = self.terminals.get(&node).map(|(_, t)| t.clone())?;
        let paste = crate::automation::bracketed_paste(text);
        terminal.update(cx, |t, _| t.input(paste.into_bytes()));
        let weak_terminal = terminal.downgrade();
        Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(ENTER_DELAY).await;
            weak_terminal.update(cx, |t, _| t.input("\r".as_bytes())).ok();
        }))
    }

    /// Il prompt completo (con le istruzioni di notifica) seguito da Invio.
    pub(crate) fn automation_send_prompt(&mut self, node: NodeId, cx: &mut Context<Self>) {
        let Some(rt) = self.automations.get(&node) else {
            return;
        };
        if rt.phase != RunPhase::Clearing || !self.terminals.contains_key(&node) {
            return;
        }
        let prompt = crate::automation::full_prompt(&rt.def.prompt);
        // ricontrollo: il /clear ha portato a un claude vivo e libero
        if self.chat_readiness(node, cx) != Readiness::Ready {
            self.fail_automation_run(node, Some(NO_RESPONSE.to_string()), cx);
            return;
        }
        let enter = self.type_prompt(node, &prompt, cx);
        let now = self.now();
        let Some(rt) = self.automations.get_mut(&node) else {
            return;
        };
        rt.phase = RunPhase::Running;
        rt.prompt_sent = Some(now);
        // l'Invio dopo che claude ha ricevuto tutto il paste
        rt._timeout = enter;
        cx.notify();
    }

    /// Chiude l'esecuzione in corso come fallita (senza scrivere altro nella chat).
    fn fail_automation_run(&mut self, node: NodeId, note: Option<String>, cx: &mut Context<Self>) {
        let Some(rt) = self.automations.get_mut(&node) else {
            return;
        };
        rt.def.last_outcome = Some(Outcome::Failed);
        rt.phase = RunPhase::Idle;
        rt.note = note;
        rt.prompt_sent = None;
        rt._timeout = None;
        self.schedule_save(cx);
        cx.notify();
    }

    /// Esecuzione rimasta aperta senza `Stop`: passato almeno un tick dal prompt, una chat
    /// non piu' al lavoro la chiude (libera → ok/notifiche, uscita o in errore → fallita).
    fn close_stale_run(&mut self, node: NodeId, now: DateTime<Local>, cx: &mut Context<Self>) {
        use superzed_sessions::SessionState;
        let Some(rt) = self.automations.get(&node) else {
            return;
        };
        let Some(sent) = rt.prompt_sent.filter(|_| rt.phase == RunPhase::Running) else {
            return;
        };
        let tick = chrono::Duration::from_std(crate::automation::TICK).unwrap_or(chrono::Duration::seconds(30));
        if now < sent + tick {
            return;
        }
        match self.chat_session_state(node, cx) {
            Some(SessionState::Working | SessionState::NeedsInput(_)) => {}
            Some(SessionState::Idle | SessionState::Done) => self.automation_stopped(node, false, cx),
            _ => self.automation_stopped(node, true, cx),
        }
    }

    /// Lo store e' cambiato: una chat di automazione con claude uscito durante
    /// un'esecuzione la chiude subito come fallita.
    pub(crate) fn automation_sessions_changed(&mut self, cx: &mut Context<Self>) {
        if self.automations.is_empty() {
            return;
        }
        let ended: Vec<NodeId> = self
            .automations
            .iter()
            // non in Clearing: `/clear` manda `SessionEnd` (reason "clear") prima del nuovo `SessionStart`
            .filter(|(_, rt)| rt.phase == RunPhase::Running)
            .map(|(id, _)| *id)
            .filter(|id| {
                matches!(self.chat_session_state(*id, cx), Some(superzed_sessions::SessionState::Ended) | None)
            })
            .collect();
        for node in ended {
            self.fail_automation_run(node, Some("claude exited".to_string()), cx);
        }
    }

    /// Fine di un turno della chat: chiude l'esecuzione in corso con il suo esito.
    pub(crate) fn automation_stopped(&mut self, node: NodeId, failed: bool, cx: &mut Context<Self>) {
        let Some(rt) = self.automations.get_mut(&node) else {
            return;
        };
        // uno Stop mentre si svuota il prompt (o fuori da un'esecuzione) non conta
        if rt.phase != RunPhase::Running {
            return;
        }
        rt.def.last_outcome = Some(if failed {
            Outcome::Failed
        } else if rt.notified > 0 {
            Outcome::Notified(rt.notified)
        } else {
            Outcome::Ok
        });
        rt.phase = RunPhase::Idle;
        rt.prompt_sent = None;
        rt.note = if failed { Some("claude error".to_string()) } else { None };
        self.schedule_save(cx);
        cx.notify();
    }

    /// Pulsante "Esegui ora": parte subito se la chat e' libera, anche se non e' dovuta.
    pub fn run_automation_now(&mut self, node: NodeId, cx: &mut Context<Self>) -> bool {
        let Some(rt) = self.automations.get(&node) else {
            return false;
        };
        let name = rt.def.name.clone();
        if rt.phase != RunPhase::Idle || self.chat_readiness(node, cx) != Readiness::Ready {
            self.warn(format!("“{name}” is busy"), cx);
            return false;
        }
        self.start_automation_run(node, cx);
        true
    }

    pub fn automation_phase(&self, node: NodeId) -> Option<RunPhase> {
        self.automations.get(&node).map(|r| r.phase)
    }
}
