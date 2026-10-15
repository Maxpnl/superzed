//! Store globale delle sessioni: avvio dell'ipc, scrittura degli hook,
//! pompa dei messaggi dal socket verso lo stato dei nodi.
use crate::launch::{HookEnv, system_config_dir};
use crate::model::{self, Session, SessionState};
use collections::{HashMap, HashSet};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global};
use std::path::{Path, PathBuf};
use superzed_ipc::{Channel, IpcMessage, IpcServer};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// `SessionStart` ha portato un id nuovo (`/clear`, fork): il canvas lo salva.
    SessionIdChanged { key: String },
    /// La fase e' cambiata (evento automatico o spostamento a mano).
    PhaseChanged { key: String },
    /// `/clear` nella sessione: il canvas decide se riportarla in Nuove.
    Cleared { key: String },
    /// Fine di un turno di claude (`Stop`, o `StopFailure` con `failed`).
    Stopped { key: String, failed: bool },
    /// Una status line con `rate_limits`: le quote dell'account si aggiornano.
    RateLimits { key: String },
}

/// Finestra entro cui una chat che ha lavorato conta nel carico del suo account.
pub const RECENT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60 * 60);

pub struct SessionStore {
    sessions: HashMap<String, Session>,
    focused: Option<String>,
    /// Sessioni di fondo (automazioni): niente fase automatica, niente campana.
    background: HashSet<String>,
    hook_env: Option<HookEnv>,
    config_dir: PathBuf,
    _server: Option<IpcServer>,
}

struct GlobalSessionStore(Entity<SessionStore>);
impl Global for GlobalSessionStore {}

impl EventEmitter<SessionEvent> for SessionStore {}

impl SessionStore {
    pub fn global(cx: &App) -> Option<Entity<SessionStore>> {
        cx.try_global::<GlobalSessionStore>().map(|g| g.0.clone())
    }

    pub fn hook_env(&self) -> Option<&HookEnv> {
        self.hook_env.as_ref()
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Chiave del nodo col focus, se c'e'.
    pub fn focused_key(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    pub fn register(
        &mut self,
        key: String,
        account: String,
        session_id: Option<String>,
        phase: crate::Phase,
        times: crate::model::SessionTimes,
        cx: &mut Context<Self>,
    ) {
        let mut s = Session::new(account, session_id);
        s.phase = phase;
        s.times = times;
        self.sessions.insert(key, s);
        cx.notify();
    }

    pub fn unregister(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.sessions.remove(key).is_some() {
            cx.notify();
        }
        self.background.remove(key);
        if self.focused.as_deref() == Some(key) {
            self.focused = None;
        }
    }

    /// Account della sessione (dopo `register`; senza, e' il sistema).
    pub fn set_account_id(&mut self, key: &str, id: &str, cx: &mut Context<Self>) {
        if let Some(s) = self.sessions.get_mut(key)
            && s.account_id != id
        {
            s.account_id = id.to_string();
            cx.notify();
        }
    }

    /// Carico per account: chat al lavoro e chat attive entro `RECENT_WINDOW` da `now`.
    pub fn account_loads(&self, now: std::time::Instant) -> HashMap<String, crate::AccountLoad> {
        let mut out: HashMap<String, crate::AccountLoad> = HashMap::default();
        for s in self.sessions.values() {
            let load = out.entry(s.account_id.clone()).or_default();
            if s.state == SessionState::Working {
                load.working += 1;
            } else if s.last_active.is_some_and(|t| now.saturating_duration_since(t) <= RECENT_WINDOW) {
                load.recent += 1;
            }
        }
        out
    }

    /// Sessione di fondo: nessun avanzamento automatico di fase e `Stop` → `Idle`.
    pub fn set_background(&mut self, key: &str, on: bool) {
        if on {
            self.background.insert(key.to_string());
        } else {
            self.background.remove(key);
        }
    }

    pub fn get(&self, key: &str) -> Option<&Session> {
        self.sessions.get(key)
    }

    pub fn set_focused(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if let Some(k) = &key
            && let Some(session) = self.sessions.get_mut(k)
            && session.state == SessionState::Done
        {
            session.set_state(SessionState::Idle);
            cx.notify();
        }
        self.focused = key;
    }

    /// Esc o ctrl-c nel nodo: claude interrompe senza mandare hook (verifica manuale
    /// 2026-09-26), quindi lavoro e attesa finiscono qui. Se l'interruzione non e'
    /// avvenuta davvero, il prossimo evento di claude rimette lo stato giusto.
    pub fn user_interrupted(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.get_mut(key) else {
            return;
        };
        log::debug!("superzed sessions: interrupt {key} from {:?}", session.state);
        if matches!(session.state, SessionState::Working | SessionState::NeedsInput(_)) {
            session.set_state(SessionState::Idle);
            cx.notify();
        }
    }

    /// Spostamento a mano della card: vince su tutto.
    pub fn set_phase(&mut self, key: &str, phase: crate::Phase, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get_mut(key)
            && session.phase != phase
        {
            session.phase = phase;
            cx.emit(SessionEvent::PhaseChanged { key: key.to_string() });
            cx.notify();
        }
    }

    pub fn apply(&mut self, msg: IpcMessage, cx: &mut Context<Self>) {
        let focused = self.focused.as_deref() == Some(msg.node_key.as_str());
        let background = self.background.contains(&msg.node_key);
        let Some(session) = self.sessions.get_mut(&msg.node_key) else {
            return;
        };
        let before = session.clone();
        match msg.channel {
            Channel::Hook => {
                let outcome = model::apply_hook(session, &msg.payload, focused || background);
                if outcome.session_id_changed {
                    cx.emit(SessionEvent::SessionIdChanged { key: msg.node_key.clone() });
                }
                if outcome.cleared {
                    cx.emit(SessionEvent::Cleared { key: msg.node_key.clone() });
                }
                if !background
                    && let Some(phase) = crate::phase::phase_for_event(&msg.payload)
                        .and_then(|p| crate::phase::advance(session.phase, p))
                {
                    session.phase = phase;
                    cx.emit(SessionEvent::PhaseChanged { key: msg.node_key.clone() });
                }
                match msg.payload.get("hook_event_name").and_then(|v| v.as_str()) {
                    Some("Stop") => cx.emit(SessionEvent::Stopped { key: msg.node_key.clone(), failed: false }),
                    Some("StopFailure") => cx.emit(SessionEvent::Stopped { key: msg.node_key.clone(), failed: true }),
                    _ => {}
                }
            }
            Channel::Status => {
                model::apply_status(session, &msg.payload);
                if msg.payload.get("rate_limits").is_some_and(|v| v.is_object()) {
                    cx.emit(SessionEvent::RateLimits { key: msg.node_key.clone() });
                }
            }
            // le notifiche vanno al `NotifyRouter` (arriva qui solo chiamando `apply` direttamente)
            Channel::Notify => return,
        }
        if msg.channel == Channel::Hook {
            log::debug!(
                "superzed sessions: {} {} -> {:?}",
                msg.node_key,
                msg.payload.get("hook_event_name").and_then(|v| v.as_str()).unwrap_or("?"),
                session.state
            );
        }
        // l'istante non deve far ridisegnare a ogni tool
        let changed = *session != before;
        if session.state == SessionState::Working {
            session.last_active = Some(std::time::Instant::now());
        }
        if changed {
            cx.notify();
        }
    }
}

/// Tasti con cui l'utente interrompe claude.
pub fn is_interrupt_key(keystroke: &gpui::Keystroke) -> bool {
    let m = keystroke.modifiers;
    match keystroke.key.as_str() {
        "escape" => !m.modified(),
        "c" => m.control && !m.alt && !m.shift && !m.platform && !m.function,
        _ => false,
    }
}

/// Destinatario dei messaggi del canale `notify` (lo registra il crate del canvas, che
/// sa di quale progetto e' la chiave del nodo).
pub struct NotifyRouter(pub fn(&str, serde_json::Value, &mut App));
impl Global for NotifyRouter {}

/// Un messaggio dal socket: le notifiche al router, il resto allo store.
pub fn dispatch(msg: IpcMessage, store: &Entity<SessionStore>, cx: &mut App) {
    if msg.channel == Channel::Notify {
        match cx.try_global::<NotifyRouter>().map(|r| r.0) {
            Some(route) => route(&msg.node_key, msg.payload, cx),
            None => log::warn!("superzed: notification from {} has no recipient", msg.node_key),
        }
        return;
    }
    store.update(cx, |s, cx| s.apply(msg, cx));
}

fn new_store(cx: &mut App, config_dir: PathBuf, hook_env: Option<HookEnv>, server: Option<IpcServer>) -> Entity<SessionStore> {
    let store = cx.new(|_| SessionStore {
        sessions: HashMap::default(),
        focused: None,
        background: HashSet::default(),
        hook_env,
        config_dir,
        _server: server,
    });
    cx.set_global(GlobalSessionStore(store.clone()));
    store
}

/// Avvia il socket, scrive i file degli hook e consegna i messaggi allo store.
/// Se qualcosa fallisce i nodi claude partono senza hook (nessun crash).
pub fn init(cx: &mut App) {
    let config_dir = system_config_dir(util::paths::home_dir());
    let data_dir = paths::data_dir();
    let token = uuid::Uuid::new_v4().simple().to_string();
    let socket = superzed_ipc::socket_path(data_dir, &std::env::temp_dir());
    let started = crate::hook_files::write_hook_files(&data_dir.join("claude")).and_then(|settings| {
        let (server, rx) = IpcServer::start(socket.clone(), token.clone())?;
        Ok((settings, server, rx))
    });
    match started {
        Ok((settings, server, rx)) => {
            let bin_dir = data_dir.join("claude").join("bin");
            let env = HookEnv { socket, token, settings, bin_dir };
            let store = new_store(cx, config_dir, Some(env), Some(server));
            // `Entity::update` qui non ritorna un `Result` (lo store e' tenuto in
            // vita da questo stesso task tramite `_server`): il loop finisce da
            // solo quando il canale si chiude o l'app va giu' e il task viene
            // cancellato.
            cx.spawn(async move |cx| {
                while let Ok(msg) = rx.recv().await {
                    cx.update(|cx| dispatch(msg, &store, cx));
                }
            })
            .detach();
        }
        Err(e) => {
            log::error!("superzed: sessions without hooks: {e:#}");
            new_store(cx, config_dir, None, None);
        }
    }
}

pub fn init_test(cx: &mut App, config_dir: PathBuf, hook_env: Option<HookEnv>) -> Entity<SessionStore> {
    new_store(cx, config_dir, hook_env, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use superzed_ipc::{Channel, IpcMessage};

    fn msg(key: &str, channel: Channel, payload: serde_json::Value) -> IpcMessage {
        IpcMessage { channel, node_key: key.into(), payload }
    }

    #[gpui::test]
    fn notify_messages_go_to_the_router_not_the_store(cx: &mut TestAppContext) {
        thread_local! { static ROUTED: std::cell::RefCell<Vec<(String, serde_json::Value)>> = Default::default(); }
        fn route(key: &str, payload: serde_json::Value, _: &mut App) {
            ROUTED.with(|r| r.borrow_mut().push((key.to_string(), payload)));
        }
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        store.update(cx, |s, cx| s.register("k1".into(), "a".into(), None, crate::Phase::New, Default::default(), cx));
        cx.update(|cx| {
            dispatch(msg("k1", Channel::Notify, serde_json::json!({"title": "senza router"})), &store, cx);
            cx.set_global(NotifyRouter(route));
            dispatch(msg("k1", Channel::Notify, serde_json::json!({"title": "x"})), &store, cx);
            dispatch(msg("k1", Channel::Hook, serde_json::json!({"hook_event_name": "UserPromptSubmit"})), &store, cx);
        });
        ROUTED.with(|r| assert_eq!(*r.borrow(), vec![("k1".to_string(), serde_json::json!({"title": "x"}))]));
        assert_eq!(store.read_with(cx, |s, _| s.get("k1").unwrap().state.clone()), SessionState::Working, "l'hook arriva allo store");
    }

    #[gpui::test]
    fn background_sessions_do_not_advance_or_ring_and_stops_are_reported(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        let _sub = cx.update(|cx| cx.subscribe(&store, move |_, e: &SessionEvent, _| seen.borrow_mut().push(e.clone())));
        let skill = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Skill", "tool_input": {"skill": "brainstorming"}});
        store.update(cx, |s, cx| {
            s.register("bg".into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            s.set_background("bg", true);
            s.apply(msg("bg", Channel::Hook, skill.clone()), cx);
            s.apply(msg("bg", Channel::Hook, serde_json::json!({"hook_event_name": "Stop"})), cx);
            s.register("fg".into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            s.apply(msg("fg", Channel::Hook, serde_json::json!({"hook_event_name": "StopFailure", "error_type": "x"})), cx);
        });
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            assert_eq!(s.get("bg").unwrap().phase, crate::Phase::New, "niente fase automatica");
            assert_eq!(s.get("bg").unwrap().state, SessionState::Idle, "niente campana");
        });
        let stops: Vec<_> = events.borrow().iter().filter(|e| matches!(e, SessionEvent::Stopped { .. })).cloned().collect();
        assert_eq!(
            stops,
            vec![SessionEvent::Stopped { key: "bg".into(), failed: false }, SessionEvent::Stopped { key: "fg".into(), failed: true }]
        );
    }

    #[gpui::test]
    fn apply_updates_registered_sessions_only(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        store.update(cx, |s, cx| {
            s.register("k1".into(), "alice".into(), Some("id-1".into()), crate::Phase::New, Default::default(), cx);
            s.apply(msg("k1", Channel::Hook, serde_json::json!({"hook_event_name": "UserPromptSubmit"})), cx);
            s.apply(msg("zz", Channel::Hook, serde_json::json!({"hook_event_name": "Stop"})), cx);
            s.apply(msg("k1", Channel::Status, serde_json::json!({"context_window": {"remaining_percentage": 42}})), cx);
        });
        store.read_with(cx, |s, _| {
            let session = s.get("k1").unwrap();
            assert_eq!(session.state, SessionState::Working);
            assert_eq!(session.context_left, Some(42));
            assert_eq!(session.account, "alice");
            assert!(s.get("zz").is_none());
        });
    }

    #[gpui::test]
    fn unknown_key_is_ignored(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        store.update(cx, |s, cx| {
            s.register("k1".into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            s.unregister("k1", cx);
            s.apply(msg("k1", Channel::Hook, serde_json::json!({"hook_event_name": "Stop"})), cx);
        });
        store.read_with(cx, |s, _| assert!(s.get("k1").is_none()));
    }

    // Verifica manuale 2026-09-26: dopo Esc su una domanda o un permesso claude non manda
    // nessun hook; l'interruzione la vede superzed dal tasto nel nodo con il focus.
    #[gpui::test]
    fn user_interrupt_clears_working_and_needs_input(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        let state = |cx: &mut TestAppContext, k: &str| store.read_with(cx, |s, _| s.get(k).unwrap().state.clone());
        store.update(cx, |s, cx| {
            for k in ["k1", "k2", "k3"] {
                s.register(k.into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            }
            s.apply(msg("k1", Channel::Hook, serde_json::json!({"hook_event_name": "PermissionRequest"})), cx);
            s.apply(msg("k2", Channel::Hook, serde_json::json!({"hook_event_name": "UserPromptSubmit"})), cx);
            s.apply(msg("k3", Channel::Hook, serde_json::json!({"hook_event_name": "Stop"})), cx);
            s.user_interrupted("k2", cx);
        });
        assert_eq!(state(cx, "k2"), SessionState::Idle, "Working interrotto");
        assert_eq!(state(cx, "k1"), SessionState::NeedsInput(crate::InputKind::Permission), "solo quel nodo");
        store.update(cx, |s, cx| {
            s.user_interrupted("k1", cx);
            s.user_interrupted("k3", cx);
            s.user_interrupted("sconosciuta", cx);
        });
        assert_eq!(state(cx, "k1"), SessionState::Idle, "NeedsInput interrotto");
        assert_eq!(state(cx, "k3"), SessionState::Done, "Done non cambia");
    }

    #[test]
    fn interrupt_keys() {
        let k = |s: &str| gpui::Keystroke::parse(s).unwrap();
        assert!(is_interrupt_key(&k("escape")));
        assert!(is_interrupt_key(&k("ctrl-c")));
        assert!(!is_interrupt_key(&k("shift-escape")));
        assert!(!is_interrupt_key(&k("cmd-c")));
        assert!(!is_interrupt_key(&k("c")));
    }

    #[gpui::test]
    fn focus_marks_done_as_seen_and_stop_while_focused_is_idle(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        store.update(cx, |s, cx| {
            s.register("k1".into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            s.register("k2".into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            s.apply(msg("k1", Channel::Hook, serde_json::json!({"hook_event_name": "Stop"})), cx);
            s.set_focused(Some("k2".into()), cx);
            s.apply(msg("k2", Channel::Hook, serde_json::json!({"hook_event_name": "Stop"})), cx);
        });
        store.read_with(cx, |s, _| {
            assert_eq!(s.get("k1").unwrap().state, SessionState::Done);
            assert_eq!(s.get("k2").unwrap().state, SessionState::Idle);
        });
        store.update(cx, |s, cx| s.set_focused(Some("k1".into()), cx));
        store.read_with(cx, |s, _| assert_eq!(s.get("k1").unwrap().state, SessionState::Idle));
    }

    #[gpui::test]
    fn session_id_change_emits_event(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        let _sub = cx.update(|cx| {
            cx.subscribe(&store, move |_, e: &SessionEvent, _| seen.borrow_mut().push(e.clone()))
        });
        store.update(cx, |s, cx| {
            s.register("k1".into(), "a".into(), Some("old".into()), crate::Phase::New, Default::default(), cx);
            s.apply(msg("k1", Channel::Hook, serde_json::json!({"hook_event_name": "SessionStart", "session_id": "new"})), cx);
        });
        cx.run_until_parked();
        assert_eq!(*events.borrow(), vec![SessionEvent::SessionIdChanged { key: "k1".into() }]);
    }

    #[gpui::test]
    fn clear_emits_cleared_after_the_new_id(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        let _sub = cx.update(|cx| cx.subscribe(&store, move |_, e: &SessionEvent, _| seen.borrow_mut().push(e.clone())));
        store.update(cx, |s, cx| {
            s.register("k1".into(), "a".into(), Some("old".into()), crate::Phase::Plan, Default::default(), cx);
            s.apply(msg("k1", Channel::Hook, serde_json::json!({"hook_event_name": "SessionStart", "session_id": "new", "source": "clear"})), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            *events.borrow(),
            vec![SessionEvent::SessionIdChanged { key: "k1".into() }, SessionEvent::Cleared { key: "k1".into() }]
        );
        assert_eq!(store.read_with(cx, |s, _| s.get("k1").unwrap().phase), crate::Phase::Plan, "la fase la decide il canvas");
    }

    #[test]
    fn context_label_colors() {
        assert_eq!(crate::ui::context_label(None), None);
        assert_eq!(crate::ui::context_label(Some(63)), Some(("ctx 63%".into(), ui::Color::Muted)));
        assert_eq!(crate::ui::context_label(Some(15)), Some(("ctx 15%".into(), ui::Color::Warning)));
        assert_eq!(crate::ui::context_label(Some(9)), Some(("ctx 9%".into(), ui::Color::Error)));
        // "sotto" e' stretto: i confini appartengono alla fascia superiore, non a quella inferiore.
        assert_eq!(crate::ui::context_label(Some(10)), Some(("ctx 10%".into(), ui::Color::Warning)), "10 non e' sotto il 10%");
        assert_eq!(crate::ui::context_label(Some(20)), Some(("ctx 20%".into(), ui::Color::Muted)), "20 non e' sotto il 20%");
    }

    #[gpui::test]
    fn automatic_phase_only_moves_forward_and_manual_moves_anywhere(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        let _sub = cx.update(|cx| cx.subscribe(&store, move |_, e: &SessionEvent, _| seen.borrow_mut().push(e.clone())));
        let skill = |name: &str| serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Skill", "tool_input": {"skill": name}});
        store.update(cx, |s, cx| {
            s.register("k".into(), "a".into(), None, crate::Phase::Plan, Default::default(), cx);
            s.apply(msg("k", Channel::Hook, skill("brainstorming")), cx);
        });
        assert_eq!(store.read_with(cx, |s, _| s.get("k").unwrap().phase), crate::Phase::Plan, "indietro: no");
        store.update(cx, |s, cx| s.apply(msg("k", Channel::Hook, skill("superpowers:executing-plans")), cx));
        assert_eq!(store.read_with(cx, |s, _| s.get("k").unwrap().phase), crate::Phase::Implementing);
        store.update(cx, |s, cx| s.set_phase("k", crate::Phase::Brainstorming, cx));
        assert_eq!(store.read_with(cx, |s, _| s.get("k").unwrap().phase), crate::Phase::Brainstorming);
        store.update(cx, |s, cx| s.set_phase("k", crate::Phase::Brainstorming, cx)); // uguale: niente evento
        cx.run_until_parked();
        let changed = events.borrow().iter().filter(|e| matches!(e, SessionEvent::PhaseChanged { .. })).count();
        assert_eq!(changed, 2, "executing-plans + spostamento a mano");
        assert_eq!(
            store.read_with(cx, |s, _| s.get("k").unwrap().save_data()),
            serde_json::json!({"phase": "brainstorming"})
        );
    }

    #[gpui::test]
    fn loads_count_working_and_recent_chats_per_account(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        let hook = |name: &str| serde_json::json!({"hook_event_name": name});
        store.update(cx, |s, cx| {
            for k in ["w", "r", "i", "sys"] {
                s.register(k.into(), "x".into(), None, crate::Phase::New, Default::default(), cx);
            }
            for k in ["w", "r", "i"] {
                s.set_account_id(k, "a1", cx);
            }
            s.apply(msg("w", Channel::Hook, hook("UserPromptSubmit")), cx);
            s.apply(msg("r", Channel::Hook, hook("UserPromptSubmit")), cx);
            s.apply(msg("r", Channel::Hook, hook("Stop")), cx);
            s.apply(msg("sys", Channel::Hook, hook("UserPromptSubmit")), cx);
        });
        let now = std::time::Instant::now();
        let loads = store.read_with(cx, |s, _| s.account_loads(now));
        assert_eq!(loads["a1"], crate::AccountLoad { working: 1, recent: 1 });
        assert_eq!(loads[crate::SYSTEM_ACCOUNT], crate::AccountLoad { working: 1, recent: 0 });
        let later = store.read_with(cx, |s, _| s.account_loads(now + RECENT_WINDOW + std::time::Duration::from_secs(60)));
        assert_eq!(later["a1"], crate::AccountLoad { working: 1, recent: 0 }, "oltre un'ora non conta");
        assert_eq!(store.read_with(cx, |s, _| s.get("i").unwrap().save_data()["account"].clone()), "a1");
    }

    #[gpui::test]
    fn status_with_rate_limits_emits_an_event(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, "/tmp/cfg".into(), None));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        let _sub = cx.update(|cx| cx.subscribe(&store, move |_, e: &SessionEvent, _| seen.borrow_mut().push(e.clone())));
        store.update(cx, |s, cx| {
            s.register("k1".into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            s.apply(msg("k1", Channel::Status, serde_json::json!({"context_window": {"remaining_percentage": 42}})), cx);
            s.apply(
                msg("k1", Channel::Status, serde_json::json!({"rate_limits": {"five_hour": {"used_percentage": 12, "resets_at": 1790424600}}})),
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(*events.borrow(), vec![SessionEvent::RateLimits { key: "k1".into() }]);
    }

    #[gpui::test]
    fn focus_and_interrupt_move_state_changed_at(cx: &mut TestAppContext) {
        let store = cx.update(|cx| init_test(cx, PathBuf::from("/cfg"), None));
        store.update(cx, |s, cx| {
            s.register("k".into(), "a".into(), None, crate::Phase::New, Default::default(), cx);
            s.apply(msg("k", Channel::Hook, serde_json::json!({"hook_event_name": "Stop"})), cx);
            let backdated = std::time::Instant::now() - std::time::Duration::from_secs(60);
            if let Some(session) = s.sessions.get_mut("k") {
                session.state_changed_at = backdated;
            }
            s.set_focused(Some("k".into()), cx);
            let session = s.get("k").unwrap();
            assert_eq!(session.state, SessionState::Idle);
            assert!(session.state_changed_at > backdated, "Done -> Idle dal focus");

            s.apply(msg("k", Channel::Hook, serde_json::json!({"hook_event_name": "UserPromptSubmit"})), cx);
            if let Some(session) = s.sessions.get_mut("k") {
                session.state_changed_at = backdated;
            }
            s.user_interrupted("k", cx);
            let session = s.get("k").unwrap();
            assert_eq!(session.state, SessionState::Idle);
            assert!(session.state_changed_at > backdated, "Working -> Idle dall'interruzione");
        });
    }
}
