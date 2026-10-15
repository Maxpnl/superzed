use std::path::{Path, PathBuf};

/// Id dell'account di sistema (`~/.claude`); e' anche il valore di default di `Session::account_id`.
pub const SYSTEM_ACCOUNT: &str = "system";

/// Chat di un account: al lavoro adesso e attive nell'ultima ora (per lo scheduler).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccountLoad {
    pub working: u32,
    pub recent: u32,
}

/// Una finestra dei `rate_limits` della status line: % usata e reset (epoch s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateWindow {
    pub used_percent: f32,
    pub resets_at: Option<i64>,
}

/// `rate_limits` dell'ultima status line che li portava.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RateLimits {
    pub five_hour: Option<RateWindow>,
    pub seven_day: Option<RateWindow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Question,
    Permission,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Nodo avviato, nessun evento ancora.
    Starting,
    Idle,
    Working,
    NeedsInput(InputKind),
    /// Ha finito e l'utente non ha ancora guardato il nodo.
    Done,
    Failed(String),
    /// claude e' uscito: nel nodo resta la shell.
    Ended,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub session_id: Option<String>,
    pub state: SessionState,
    pub context_left: Option<u8>,
    pub model: Option<String>,
    pub account: String,
    /// Id dell'account (`SYSTEM_ACCOUNT` = `~/.claude`); `account` e' solo l'etichetta.
    pub account_id: String,
    /// Ultima volta che la chat e' stata vista lavorare (carico dello scheduler).
    pub last_active: Option<std::time::Instant>,
    pub rate_limits: Option<RateLimits>,
    pub transcript: Option<PathBuf>,
    pub phase: crate::Phase,
    /// Ultimo cambio di `state`: ordina la vista per priorita' e fa ripartire il
    /// rimbalzo dell'avviso solo a un nuovo ingresso nello stato.
    pub state_changed_at: std::time::Instant,
    /// File scritti da claude con Edit/Write/MultiEdit/NotebookEdit in questa sessione
    /// (path assoluti, solo in memoria). Le modifiche via Bash non si vedono.
    pub touched_files: std::collections::BTreeSet<PathBuf>,
    /// Quando la chat e' nata e quando claude ha finito di rispondere l'ultima volta:
    /// nel `data` del nodo, cosi' sopravvivono al riavvio.
    pub times: SessionTimes,
}

/// Secondi unix (orologio di sistema, non `Instant`: si salvano e si confrontano col
/// calendario). `None` = mai visto.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionTimes {
    pub opened_at: Option<i64>,
    pub replied_at: Option<i64>,
}

impl SessionTimes {
    pub fn from_data(data: &serde_json::Value) -> Self {
        Self {
            opened_at: data.get("opened_at").and_then(|v| v.as_i64()),
            replied_at: data.get("replied_at").and_then(|v| v.as_i64()),
        }
    }

    /// Una chat gia' esistente prima di questi campi: dal file del transcript, creazione
    /// e ultima modifica. Non tocca i valori gia' noti.
    pub fn fill_from_transcript(&mut self, transcript: &Path) {
        if self.opened_at.is_some() && self.replied_at.is_some() {
            return;
        }
        let Ok(meta) = std::fs::metadata(transcript) else {
            return;
        };
        let unix = |t: std::io::Result<std::time::SystemTime>| {
            t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64)
        };
        if self.opened_at.is_none() {
            self.opened_at = unix(meta.created()).or_else(|| unix(meta.modified()));
        }
        if self.replied_at.is_none() {
            self.replied_at = unix(meta.modified());
        }
    }
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Session {
    pub fn set_state(&mut self, state: SessionState) {
        if self.state != state {
            self.state = state;
            self.state_changed_at = std::time::Instant::now();
        }
    }

    /// Al lavoro adesso, oppure vista lavorare o ha risposto da meno di `window`.
    /// L'ultima risposta (`times.replied_at`, orologio di sistema) vale anche dopo
    /// un riavvio, quando `last_active` non c'e' ancora.
    pub fn active_within(&self, window: std::time::Duration, now: std::time::Instant, now_unix: i64) -> bool {
        if self.state == SessionState::Working {
            return true;
        }
        let seen_working = self.last_active.is_some_and(|t| now.saturating_duration_since(t) <= window);
        let replied = self.times.replied_at.is_some_and(|t| now_unix.saturating_sub(t) <= window.as_secs() as i64);
        seen_working || replied
    }

    pub fn new(account: String, session_id: Option<String>) -> Self {
        Self {
            session_id,
            state: SessionState::Starting,
            context_left: None,
            model: None,
            account,
            account_id: SYSTEM_ACCOUNT.to_string(),
            last_active: None,
            rate_limits: None,
            transcript: None,
            phase: crate::Phase::New,
            state_changed_at: std::time::Instant::now(),
            touched_files: Default::default(),
            times: SessionTimes::default(),
        }
    }

    /// `data` del nodo claude in `project.json`.
    pub fn save_data(&self) -> serde_json::Value {
        let mut data = serde_json::json!({ "phase": self.phase.as_str() });
        if let Some(id) = &self.session_id {
            data["session_id"] = serde_json::Value::String(id.clone());
        }
        if self.account_id != SYSTEM_ACCOUNT {
            data["account"] = serde_json::Value::String(self.account_id.clone());
        }
        if let Some(t) = self.times.opened_at {
            data["opened_at"] = serde_json::Value::from(t);
        }
        if let Some(t) = self.times.replied_at {
            data["replied_at"] = serde_json::Value::from(t);
        }
        data
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookOutcome {
    pub session_id_changed: bool,
    /// `/clear` nella sessione (`SessionStart` con `source: "clear"`).
    pub cleared: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attention {
    None,
    Done,
    NeedsInput,
}

pub fn attention(state: &SessionState) -> Attention {
    match state {
        SessionState::NeedsInput(_) => Attention::NeedsInput,
        SessionState::Done => Attention::Done,
        _ => Attention::None,
    }
}

/// Posizione nella vista per priorita' del pannello Sessions (0 = in cima).
pub fn priority_rank(state: &SessionState) -> u8 {
    match state {
        SessionState::NeedsInput(_) => 0,
        SessionState::Done => 1,
        SessionState::Failed(_) => 2,
        SessionState::Working => 3,
        SessionState::Idle | SessionState::Starting => 4,
        SessionState::Ended => 5,
    }
}

pub const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

fn record_touched_file(session: &mut Session, payload: &serde_json::Value) {
    if !str_field(payload, "tool_name").is_some_and(|tool| EDIT_TOOLS.contains(&tool)) {
        return;
    }
    let Some(input) = payload.get("tool_input") else {
        return;
    };
    let Some(raw) = str_field(input, "file_path").or_else(|| str_field(input, "notebook_path")) else {
        return;
    };
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        // senza cwd un path relativo si risolverebbe contro la cartella di superzed
        match str_field(payload, "cwd") {
            Some(cwd) => Path::new(cwd).join(path),
            None => return,
        }
    };
    session.touched_files.insert(path);
}

fn str_field<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(|v| v.as_str())
}

/// Applica un evento hook di Claude Code. `focused`: il nodo ha il focus (uno
/// `Stop` visto dall'utente non resta "da guardare").
pub fn apply_hook(session: &mut Session, payload: &serde_json::Value, focused: bool) -> HookOutcome {
    let mut outcome = HookOutcome::default();
    let Some(event) = str_field(payload, "hook_event_name") else {
        return outcome;
    };
    let next = match event {
        "SessionStart" => {
            if let Some(id) = str_field(payload, "session_id")
                && session.session_id.as_deref() != Some(id)
            {
                session.session_id = Some(id.to_string());
                outcome.session_id_changed = true;
            }
            if let Some(path) = str_field(payload, "transcript_path") {
                let path = PathBuf::from(path);
                session.times.fill_from_transcript(&path);
                session.transcript = Some(path);
            }
            outcome.cleared = str_field(payload, "source") == Some("clear");
            Some(SessionState::Idle)
        }
        "UserPromptSubmit" => Some(SessionState::Working),
        "PostToolUse" => {
            record_touched_file(session, payload);
            Some(SessionState::Working)
        }
        "PreToolUse" => Some(match str_field(payload, "tool_name") {
            Some("AskUserQuestion") => SessionState::NeedsInput(InputKind::Question),
            _ => SessionState::Working,
        }),
        "PermissionRequest" => Some(SessionState::NeedsInput(InputKind::Permission)),
        // Spec amendata (review finale, finding 2): un rifiuto o un tool approvato che
        // fallisce chiudono il NeedsInput senza che claude mandi Stop; senza questo la
        // campana/spinner restano accesi.
        "PermissionDenied" | "PostToolUseFailure" => Some(SessionState::Working),
        "Notification" => match str_field(payload, "notification_type") {
            Some("permission_prompt") => Some(SessionState::NeedsInput(InputKind::Permission)),
            Some("elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => {
                Some(SessionState::NeedsInput(InputKind::Other))
            }
            Some("idle_prompt") => match &session.state {
                SessionState::Working | SessionState::NeedsInput(_) => Some(SessionState::Idle),
                _ => None,
            },
            _ => None,
        },
        "Stop" => {
            session.times.replied_at = Some(now_unix());
            Some(if focused { SessionState::Idle } else { SessionState::Done })
        }
        "StopFailure" => {
            session.times.replied_at = Some(now_unix());
            Some(SessionState::Failed(str_field(payload, "error_type").unwrap_or("unknown").to_string()))
        }
        "SessionEnd" => Some(SessionState::Ended),
        _ => None,
    };
    if let Some(next) = next {
        session.set_state(next);
    }
    outcome
}

/// Applica il JSON della status line: % di contesto rimasto e modello.
pub fn apply_status(session: &mut Session, payload: &serde_json::Value) {
    if let Some(left) = payload
        .pointer("/context_window/remaining_percentage")
        .and_then(|v| v.as_f64())
    {
        session.context_left = Some(left.round().clamp(0., 100.) as u8);
    }
    if let Some(name) = payload.pointer("/model/display_name").and_then(|v| v.as_str()) {
        session.model = Some(name.to_string());
    }
    // limiti dell'account (gratis, a ogni status line): li unisce alle quote superzed_accounts
    if let Some(rl) = payload.get("rate_limits").filter(|v| v.is_object()) {
        let window = |name: &str| {
            let w = rl.get(name)?;
            Some(RateWindow {
                used_percent: w.get("used_percentage")?.as_f64()? as f32,
                resets_at: w.get("resets_at").and_then(|v| v.as_i64()),
            })
        };
        session.rate_limits = Some(RateLimits { five_hour: window("five_hour"), seven_day: window("seven_day") });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> serde_json::Value {
        let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn session() -> Session {
        Session::new("alice".into(), Some("11111111-1111-4111-8111-111111111111".into()))
    }

    fn after(names: &[&str]) -> Session {
        let mut s = session();
        for n in names {
            apply_hook(&mut s, &fx(n), false);
        }
        s
    }

    #[test]
    fn transitions_table() {
        assert_eq!(session().state, SessionState::Starting);
        assert_eq!(after(&["session_start"]).state, SessionState::Idle);
        assert_eq!(after(&["session_start", "user_prompt_submit"]).state, SessionState::Working);
        assert_eq!(after(&["user_prompt_submit", "pre_tool_use_ask"]).state, SessionState::NeedsInput(InputKind::Question));
        assert_eq!(after(&["pre_tool_use_ask", "post_tool_use_ask"]).state, SessionState::Working);
        assert_eq!(after(&["user_prompt_submit", "permission_request"]).state, SessionState::NeedsInput(InputKind::Permission));
        assert_eq!(after(&["user_prompt_submit", "notification_permission"]).state, SessionState::NeedsInput(InputKind::Permission));
        assert_eq!(after(&["user_prompt_submit", "notification_elicitation"]).state, SessionState::NeedsInput(InputKind::Other));
        assert_eq!(after(&["permission_request", "pre_tool_use_bash"]).state, SessionState::Working);
        assert_eq!(after(&["user_prompt_submit", "stop"]).state, SessionState::Done);
        assert_eq!(after(&["user_prompt_submit", "stop", "notification_idle"]).state, SessionState::Done, "idle_prompt non cambia uno stato gia' Done");
        assert_eq!(after(&["user_prompt_submit", "stop_failure"]).state, SessionState::Failed("rate_limit".into()));
        assert_eq!(after(&["stop", "session_end"]).state, SessionState::Ended);
        // Spec amendata (review finale, finding 2): rifiuto/fallimento chiudono il NeedsInput
        // senza Stop; idle_prompt chiude Working o NeedsInput quando claude non manda Stop
        // dopo un Esc o un permesso negato.
        assert_eq!(
            after(&["user_prompt_submit", "permission_request", "permission_denied"]).state,
            SessionState::Working,
            "un rifiuto chiude il NeedsInput"
        );
        assert_eq!(
            after(&["user_prompt_submit", "post_tool_use_failure"]).state,
            SessionState::Working,
            "un tool approvato che fallisce resta Working"
        );
        assert_eq!(
            after(&["user_prompt_submit", "notification_idle"]).state,
            SessionState::Idle,
            "idle_prompt chiude un Working senza Stop"
        );
        assert_eq!(
            after(&["user_prompt_submit", "permission_request", "notification_idle"]).state,
            SessionState::Idle,
            "idle_prompt chiude un NeedsInput (dopo un Esc)"
        );
    }

    #[test]
    fn stop_while_focused_goes_idle() {
        let mut s = after(&["user_prompt_submit"]);
        apply_hook(&mut s, &fx("stop"), true);
        assert_eq!(s.state, SessionState::Idle);
    }

    #[test]
    fn session_start_updates_id_and_transcript() {
        let mut s = session();
        assert!(!apply_hook(&mut s, &fx("session_start"), false).session_id_changed, "stesso id");
        assert!(s.transcript.as_ref().unwrap().ends_with("11111111-1111-4111-8111-111111111111.jsonl"));
        assert!(apply_hook(&mut s, &fx("session_start_clear"), false).session_id_changed);
        assert_eq!(s.session_id.as_deref(), Some("22222222-2222-4222-8222-222222222222"));
    }

    #[test]
    fn clear_is_reported_only_for_source_clear() {
        let mut s = session();
        assert!(!apply_hook(&mut s, &fx("session_start"), false).cleared, "startup");
        assert!(apply_hook(&mut s, &fx("session_start_clear"), false).cleared);
        let resume = serde_json::json!({"hook_event_name": "SessionStart", "source": "resume"});
        assert!(!apply_hook(&mut s, &resume, false).cleared);
    }

    #[test]
    fn malformed_payload_is_ignored() {
        let mut s = after(&["user_prompt_submit"]);
        apply_hook(&mut s, &serde_json::json!({"nope": 1}), false);
        apply_hook(&mut s, &serde_json::json!([1, 2]), false);
        assert_eq!(s.state, SessionState::Working);
    }

    #[test]
    fn status_sets_context_and_model() {
        let mut s = session();
        apply_status(&mut s, &fx("status"));
        assert_eq!(s.context_left, Some(63));
        assert_eq!(s.model.as_deref(), Some("Opus 5.5"));
        apply_status(&mut s, &fx("status_null"));
        assert_eq!(s.context_left, Some(63), "null non cancella l'ultimo valore");
        apply_status(&mut s, &serde_json::json!({"context_window": {"remaining_percentage": 140}}));
        assert_eq!(s.context_left, Some(100), "limitato a 100");
    }

    #[test]
    fn save_data_has_the_account_only_when_not_system() {
        let mut s = session();
        s.phase = crate::Phase::Plan;
        assert_eq!(s.account_id, SYSTEM_ACCOUNT);
        assert_eq!(
            s.save_data(),
            serde_json::json!({"phase": "plan", "session_id": "11111111-1111-4111-8111-111111111111"})
        );
        s.account_id = "a1".into();
        assert_eq!(s.save_data()["account"], "a1");
    }

    #[test]
    fn times_round_trip_through_data_and_stop_sets_replied_at() {
        let mut s = session();
        assert_eq!(s.save_data().get("opened_at"), None, "mai visto: niente campo");
        let data = serde_json::json!({"opened_at": 1_700_000_000, "replied_at": "no"});
        s.times = SessionTimes::from_data(&data);
        assert_eq!(s.times, SessionTimes { opened_at: Some(1_700_000_000), replied_at: None });
        assert_eq!(s.save_data()["opened_at"], 1_700_000_000);
        assert!(s.save_data().get("replied_at").is_none());
        let before = now_unix();
        apply_hook(&mut s, &fx("stop"), false);
        let replied = s.times.replied_at.expect("Stop segna la risposta");
        assert!(replied >= before);
        assert_eq!(s.save_data()["replied_at"], replied);
        let mut failed = session();
        apply_hook(&mut failed, &fx("stop_failure"), false);
        assert!(failed.times.replied_at.is_some(), "anche un fallimento chiude la risposta");
    }

    #[test]
    fn times_fall_back_to_the_transcript_file_only_when_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{}\n").unwrap();
        let mut t = SessionTimes::default();
        t.fill_from_transcript(&path);
        assert!(t.opened_at.is_some() && t.replied_at.is_some());
        let mut known = SessionTimes { opened_at: Some(1), replied_at: Some(2) };
        known.fill_from_transcript(&path);
        assert_eq!(known, SessionTimes { opened_at: Some(1), replied_at: Some(2) }, "i valori noti restano");
        let mut missing = SessionTimes::default();
        missing.fill_from_transcript(&dir.path().join("nope.jsonl"));
        assert_eq!(missing, SessionTimes::default());
        // SessionStart con un transcript esistente riempie i campi della sessione
        let mut s = session();
        let payload = serde_json::json!({"hook_event_name": "SessionStart", "transcript_path": path.to_string_lossy()});
        apply_hook(&mut s, &payload, false);
        assert!(s.times.replied_at.is_some());
    }

    #[test]
    fn status_reads_rate_limits() {
        let mut s = session();
        apply_status(&mut s, &fx("status"));
        assert_eq!(
            s.rate_limits,
            Some(RateLimits { five_hour: Some(RateWindow { used_percent: 12., resets_at: Some(1790424600) }), seven_day: None })
        );
        apply_status(&mut s, &fx("status_null"));
        assert!(s.rate_limits.is_some(), "uno status senza rate_limits non cancella gli ultimi");
        apply_status(
            &mut s,
            &serde_json::json!({"rate_limits": {"seven_day": {"used_percentage": 40.5, "resets_at": 1790900000}}}),
        );
        let rl = s.rate_limits.unwrap();
        assert_eq!(rl.seven_day, Some(RateWindow { used_percent: 40.5, resets_at: Some(1790900000) }));
        assert_eq!(rl.five_hour, None, "ogni status porta i suoi limiti");
    }

    #[test]
    fn attention_order() {
        assert_eq!(attention(&SessionState::Working), Attention::None);
        assert_eq!(attention(&SessionState::Done), Attention::Done);
        assert_eq!(attention(&SessionState::NeedsInput(InputKind::Other)), Attention::NeedsInput);
        assert!(Attention::NeedsInput > Attention::Done && Attention::Done > Attention::None);
    }

    fn post_tool_use(tool: &str, input: serde_json::Value, cwd: Option<&str>) -> serde_json::Value {
        let mut payload = serde_json::json!({"hook_event_name": "PostToolUse", "tool_name": tool, "tool_input": input});
        if let Some(cwd) = cwd {
            payload["cwd"] = serde_json::Value::String(cwd.to_string());
        }
        payload
    }

    #[test]
    fn edit_tools_record_the_touched_file() {
        let mut s = session();
        apply_hook(&mut s, &post_tool_use("Edit", serde_json::json!({"file_path": "/repo/a.rs"}), None), false);
        apply_hook(&mut s, &post_tool_use("Write", serde_json::json!({"file_path": "/repo/b.rs"}), None), false);
        apply_hook(&mut s, &post_tool_use("MultiEdit", serde_json::json!({"file_path": "/repo/a.rs"}), None), false);
        apply_hook(&mut s, &post_tool_use("NotebookEdit", serde_json::json!({"notebook_path": "/repo/n.ipynb"}), None), false);
        let touched: Vec<_> = s.touched_files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert_eq!(touched, vec!["/repo/a.rs", "/repo/b.rs", "/repo/n.ipynb"]);
        assert_eq!(s.state, SessionState::Working);
    }

    #[test]
    fn other_tools_and_relative_paths_without_cwd_are_ignored() {
        let mut s = session();
        apply_hook(&mut s, &post_tool_use("Bash", serde_json::json!({"command": "sed -i x a.rs"}), Some("/repo")), false);
        apply_hook(&mut s, &post_tool_use("Read", serde_json::json!({"file_path": "/repo/r.rs"}), None), false);
        apply_hook(&mut s, &post_tool_use("Edit", serde_json::json!({"file_path": "rel.rs"}), None), false);
        apply_hook(&mut s, &post_tool_use("Edit", serde_json::json!({}), Some("/repo")), false);
        assert!(s.touched_files.is_empty());
        apply_hook(&mut s, &post_tool_use("Edit", serde_json::json!({"file_path": "src/rel.rs"}), Some("/repo")), false);
        assert_eq!(s.touched_files.iter().next().map(|p| p.as_path()), Some(Path::new("/repo/src/rel.rs")));
    }

    #[test]
    fn state_changed_at_moves_only_when_the_state_changes() {
        let mut s = after(&["user_prompt_submit"]);
        let old = s.state_changed_at.checked_sub(std::time::Duration::from_secs(60)).unwrap();
        s.state_changed_at = old;
        apply_hook(&mut s, &fx("user_prompt_submit"), false);
        assert_eq!(s.state_changed_at, old, "Working -> Working non conta");
        apply_hook(&mut s, &fx("stop"), false);
        assert!(s.state_changed_at > old, "Working -> Done conta");
    }

    #[test]
    fn active_within_counts_working_recent_last_active_or_recent_reply() {
        let now = std::time::Instant::now();
        let now_unix = 1_000_000;
        let window = std::time::Duration::from_secs(3600);
        let ago = |secs: u64| now.checked_sub(std::time::Duration::from_secs(secs)).unwrap();
        let mut s = Session::new("a".into(), None);
        assert!(!s.active_within(window, now, now_unix), "mai attiva");
        s.state = SessionState::Working;
        assert!(s.active_within(window, now, now_unix), "al lavoro conta sempre");
        s.state = SessionState::Idle;
        s.last_active = Some(ago(600));
        assert!(s.active_within(window, now, now_unix));
        s.last_active = Some(ago(7200));
        assert!(!s.active_within(window, now, now_unix));
        s.times.replied_at = Some(now_unix - 600);
        assert!(s.active_within(window, now, now_unix), "risposta recente salvata nel nodo");
        s.times.replied_at = Some(now_unix - 7200);
        assert!(!s.active_within(window, now, now_unix));
    }

    #[test]
    fn priority_rank_order() {
        let ranks = [
            SessionState::NeedsInput(InputKind::Permission),
            SessionState::Done,
            SessionState::Failed("x".into()),
            SessionState::Working,
            SessionState::Idle,
            SessionState::Ended,
        ]
        .iter()
        .map(priority_rank)
        .collect::<Vec<_>>();
        assert_eq!(ranks, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(priority_rank(&SessionState::Starting), priority_rank(&SessionState::Idle));
        assert_eq!(priority_rank(&SessionState::NeedsInput(InputKind::Question)), 0);
    }
}
