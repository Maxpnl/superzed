//! Definizione di un'automazione (salvata nel `data` del nodo claude a cui e'
//! agganciata) e decisione presa a ogni tick. Puro: niente GPUI, niente PTY.
use crate::schedule;
use chrono::{DateTime, Local};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::time::Duration;

/// Ogni quanto il tick delle automazioni controlla le chat agganciate.
pub const TICK: Duration = Duration::from_secs(30);
/// Tempo massimo di attesa perche' una chat occupata si liberi prima di rinunciare.
pub const CLEAR_TIMEOUT: Duration = Duration::from_secs(10);

/// Istruzioni fisse aggiunte in coda al prompt dell'utente: come notificare i
/// problemi trovati (canale `superzed-notify`).
pub const NOTIFY_INSTRUCTIONS: &str = "Al termine, per ogni problema trovato esegui `superzed-notify` (se non e' nel PATH: \"$SUPERZED_NOTIFY\") passando su stdin un JSON `{\"severity\": \"nota|info|avviso|errore|critico\", \"title\": \"…\", \"summary\": \"…\", \"details\": \"markdown con stacktrace, log rilevanti, file e righe, passi per riprodurre\", \"suggested_prompt\": \"…\"}`, una chiamata per problema. Se non trovi problemi non notificare nulla.";

/// Prompt effettivo mandato alla sessione claude: quello dell'utente piu' le
/// istruzioni di notifica.
pub fn full_prompt(user: &str) -> String {
    format!("{}\n\n{NOTIFY_INSTRUCTIONS}", user.trim())
}

/// Il prompt come bracketed paste (`ESC[200~ … ESC[201~`), sempre: si scrive solo in un
/// claude vivo, che lo accetta, e la modalita' nella cache del terminale puo' essere
/// vecchia (terminale mai disegnato). Gli a capo restano; un ESC dentro chiuderebbe il
/// paste prima del tempo e si toglie.
pub fn bracketed_paste(text: &str) -> String {
    format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
}

/// Esito dell'ultima esecuzione di un'automazione.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Notified(u32),
    Failed,
    SkippedBusy,
}

impl Outcome {
    pub fn label(&self) -> String {
        match self {
            Outcome::Ok => "succeeded".to_string(),
            Outcome::Notified(1) => "1 notification".to_string(),
            Outcome::Notified(n) => format!("{n} notifications"),
            Outcome::Failed => "failed".to_string(),
            Outcome::SkippedBusy => "skipped (busy)".to_string(),
        }
    }

    fn to_wire(self) -> String {
        match self {
            Outcome::Ok => "ok".to_string(),
            Outcome::Notified(n) => format!("notified:{n}"),
            Outcome::Failed => "failed".to_string(),
            Outcome::SkippedBusy => "skipped_busy".to_string(),
        }
    }

    fn from_wire(s: &str) -> Option<Outcome> {
        match s {
            "ok" => Some(Outcome::Ok),
            "failed" => Some(Outcome::Failed),
            "skipped_busy" => Some(Outcome::SkippedBusy),
            _ => s.strip_prefix("notified:").and_then(|n| n.parse().ok()).map(Outcome::Notified),
        }
    }
}

impl Serialize for Outcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire())
    }
}

impl<'de> Deserialize<'de> for Outcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Outcome::from_wire(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown outcome: {s}")))
    }
}

fn yes() -> bool {
    true
}

/// Esito cosi' com'e' salvato nel `data` del nodo: un valore sconosciuto (versione
/// futura, dato corrotto) diventa `None` invece di far fallire tutta la definizione.
fn lenient_outcome<'de, D>(deserializer: D) -> Result<Option<Outcome>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().and_then(Outcome::from_wire))
}

/// Definizione di un'automazione, salvata nel `data` del nodo claude a cui e'
/// agganciata (chiave `"automation"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationDef {
    pub name: String,
    pub schedule: String,
    pub prompt: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default = "yes")]
    pub catch_up: bool,
    #[serde(default)]
    pub last_run: Option<DateTime<Local>>,
    #[serde(default, deserialize_with = "lenient_outcome")]
    pub last_outcome: Option<Outcome>,
    /// Quando e' nata l'automazione: da qui si contano le scadenze finche' non e' mai
    /// stata eseguita (anche dopo un riavvio). Assente nelle definizioni piu' vecchie.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Local>>,
    /// Account scelto per la chat; `None` = quello dello scheduler alla creazione.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// I soli campi modificabili dal form di creazione/modifica.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationDraft {
    pub name: String,
    pub schedule: String,
    pub prompt: String,
    pub account: Option<String>,
}

impl AutomationDef {
    pub fn from_draft(d: AutomationDraft) -> Self {
        AutomationDef {
            name: d.name,
            schedule: d.schedule,
            prompt: d.prompt,
            paused: false,
            catch_up: true,
            last_run: None,
            last_outcome: None,
            created_at: None,
            account: d.account,
        }
    }

    /// Sostituisce nome, frequenza, prompt e account; stato di esecuzione invariato.
    pub fn apply_draft(&mut self, d: AutomationDraft) {
        self.name = d.name;
        self.schedule = d.schedule;
        self.prompt = d.prompt;
        self.account = d.account;
    }

    pub fn draft(&self) -> AutomationDraft {
        AutomationDraft {
            name: self.name.clone(),
            schedule: self.schedule.clone(),
            prompt: self.prompt.clone(),
            account: self.account.clone(),
        }
    }
}

/// Valida i campi del form: nome e prompt obbligatori, frequenza in un formato valido.
pub fn validate_draft(
    name: &str,
    schedule: &str,
    prompt: &str,
    account: Option<String>,
) -> Result<AutomationDraft, String> {
    let name = name.trim().to_string();
    let prompt = prompt.trim().to_string();
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    if prompt.is_empty() {
        return Err("prompt is required".to_string());
    }
    schedule::parse(schedule)?;
    Ok(AutomationDraft { name, schedule: schedule.trim().to_string(), prompt, account })
}

/// Legge la definizione dell'automazione dal `data` del nodo, se presente e valida.
pub fn automation_from_data(data: &serde_json::Value) -> Option<AutomationDef> {
    serde_json::from_value(data.get("automation")?.clone()).ok()
}

/// Aggiunge (o toglie) la definizione dell'automazione al `data` del nodo, senza
/// toccare le altre chiavi.
pub fn data_with_automation(base: serde_json::Value, def: Option<&AutomationDef>) -> serde_json::Value {
    let Some(def) = def else {
        return base;
    };
    let mut obj = match base {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert("automation".to_string(), serde_json::to_value(def).expect("AutomationDef always serializes"));
    serde_json::Value::Object(obj)
}

/// Se la chat aggancianta e' pronta a ricevere il prompt dell'automazione al prossimo tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    Busy,
    Unavailable,
}

/// Fase di avvio di un'esecuzione: prima si svuota il prompt della chat, poi si manda.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPhase {
    Idle,
    Clearing,
    Running,
}

/// Cosa fare al tick per una data automazione.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Nothing,
    Run,
    SkipBusy,
}

/// Decide cosa fare per un'automazione a un dato tick.
pub fn decide(def: &AutomationDef, anchor: DateTime<Local>, now: DateTime<Local>, readiness: Readiness, running: bool) -> Decision {
    if def.paused || running {
        return Decision::Nothing;
    }
    let Ok(schedule) = crate::schedule::parse(&def.schedule) else {
        return Decision::Nothing;
    };
    let due = crate::schedule::next_run(&schedule, def.last_run, anchor, def.catch_up, now);
    if now < due {
        return Decision::Nothing;
    }
    match readiness {
        Readiness::Ready => Decision::Run,
        // occupata: si riprova al prossimo tick; passata un'intera scadenza si salta
        Readiness::Busy if now >= schedule.next_after(due) => Decision::SkipBusy,
        Readiness::Busy => Decision::Nothing,
        // claude non ancora pronto (in avvio dopo un riavvio, uscito): si aspetta che lo
        // sia, senza mai segnare "saltata" (il recupero parte appena e' pronto)
        Readiness::Unavailable => Decision::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    use serde_json::json;

    fn t(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 28, h, m, 0).unwrap()
    }

    fn def(schedule: &str) -> AutomationDef {
        AutomationDef::from_draft(AutomationDraft { name: "Log".into(), schedule: schedule.into(), prompt: "Leggi i log".into(), account: None })
    }

    #[test]
    fn definition_round_trips_in_node_data() {
        let mut d = def("4h");
        d.last_run = Some(t(10, 0));
        d.last_outcome = Some(Outcome::Notified(3));
        let data = data_with_automation(json!({"session_id": "s", "phase": "new"}), Some(&d));
        assert_eq!(data["session_id"], "s");
        assert_eq!(data["automation"]["last_outcome"], "notified:3");
        assert_eq!(data["automation"]["catch_up"], true);
        assert_eq!(automation_from_data(&data), Some(d.clone()));
        assert_eq!(data_with_automation(serde_json::Value::Null, Some(&d))["automation"]["name"], "Log");
        assert_eq!(data_with_automation(json!({"phase": "new"}), None), json!({"phase": "new"}));
        // esito sconosciuto: la definizione resta, l'esito no
        let odd = json!({"automation": {"name": "x", "schedule": "1h", "prompt": "p", "last_outcome": "boh"}});
        let parsed = automation_from_data(&odd).unwrap();
        assert_eq!((parsed.last_outcome, parsed.catch_up, parsed.paused), (None, true, false));
        assert_eq!(automation_from_data(&json!({"session_id": "s"})), None);
    }

    // Revisione finale: dopo un riavvio claude puo' metterci a partire (Starting →
    // Unavailable): il recupero aspetta che sia pronto, anche oltre un'intera scadenza,
    // e non diventa mai "saltata (occupata)".
    #[test]
    fn an_unavailable_chat_never_skips_it_waits() {
        let anchor = t(9, 0);
        let mut d = def("09:00");
        d.last_run = Some(t(9, 0) - chrono::Duration::days(3));
        for now in [t(9, 0), t(12, 0), t(23, 0), t(9, 0) + chrono::Duration::days(2)] {
            assert_eq!(decide(&d, anchor, now, Readiness::Unavailable, false), Decision::Nothing, "{now}");
        }
        assert_eq!(decide(&d, anchor, t(23, 0), Readiness::Ready, false), Decision::Run, "appena pronta parte");
        assert_eq!(decide(&d, anchor, t(9, 0) + chrono::Duration::days(1), Readiness::Busy, false), Decision::SkipBusy, "solo occupata salta");
    }

    // Revisione finale: la creazione si salva, cosi' l'ancora non riparte a ogni riavvio;
    // una definizione vecchia senza `created_at` si legge ancora.
    #[test]
    fn created_at_is_saved_and_optional() {
        let mut d = def("09:00");
        assert_eq!(d.created_at, None);
        d.created_at = Some(t(8, 0));
        let data = data_with_automation(json!({}), Some(&d));
        assert_eq!(automation_from_data(&data).unwrap().created_at, Some(t(8, 0)));
        let old = json!({"automation": {"name": "x", "schedule": "09:00", "prompt": "p"}});
        assert_eq!(automation_from_data(&old).unwrap().created_at, None);
        // creata alle 8, app chiusa alle 8:30, riaperta alle 10: il recupero delle 9 parte
        assert_eq!(decide(&d, d.created_at.unwrap(), t(10, 0), Readiness::Ready, false), Decision::Run);
    }

    #[test]
    fn prompt_is_always_a_bracketed_paste_keeping_newlines() {
        let p = bracketed_paste("riga 1\nriga 2\r\n\x1b[31mriga 3");
        assert_eq!(p, "\x1b[200~riga 1\nriga 2\r\n[31mriga 3\x1b[201~");
    }

    #[test]
    fn outcomes() {
        for (o, s) in [(Outcome::Ok, "ok"), (Outcome::Notified(2), "notified:2"), (Outcome::Failed, "failed"), (Outcome::SkippedBusy, "skipped_busy")] {
            assert_eq!(serde_json::to_value(&o).unwrap(), json!(s));
            assert_eq!(serde_json::from_value::<Outcome>(json!(s)).unwrap(), o);
        }
        assert_eq!(Outcome::Notified(2).label(), "2 notifications");
        assert_eq!(Outcome::Notified(1).label(), "1 notification");
        assert_eq!(Outcome::SkippedBusy.label(), "skipped (busy)");
    }

    #[test]
    fn full_prompt_appends_the_notify_instructions() {
        let p = full_prompt("Leggi i log");
        assert!(p.starts_with("Leggi i log\n\n"));
        assert!(p.ends_with(NOTIFY_INSTRUCTIONS));
        assert!(NOTIFY_INSTRUCTIONS.contains("superzed-notify") && NOTIFY_INSTRUCTIONS.contains("$SUPERZED_NOTIFY"));
    }

    #[test]
    fn draft_validation() {
        assert_eq!(
            validate_draft(" Log ", "4h", " p ", Some("a1".into())).unwrap(),
            AutomationDraft { name: "Log".into(), schedule: "4h".into(), prompt: "p".into(), account: Some("a1".into()) }
        );
        assert!(validate_draft("", "4h", "p", None).unwrap_err().contains("name"));
        assert!(validate_draft("x", "4m", "p", None).unwrap_err().contains("5m"));
        assert!(validate_draft("x", "4h", "  ", None).unwrap_err().contains("prompt"));
    }

    #[test]
    fn decide_table() {
        let anchor = t(9, 0);
        let d = def("4h"); // mai eseguita, catch_up: dovuta all'ancora
        assert_eq!(decide(&d, anchor, t(9, 0), Readiness::Ready, false), Decision::Run);
        assert_eq!(decide(&d, anchor, t(9, 0), Readiness::Ready, true), Decision::Nothing, "gia' in corso");
        assert_eq!(decide(&d, anchor, t(10, 0), Readiness::Busy, false), Decision::Nothing, "occupata: aspetta");
        assert_eq!(decide(&d, anchor, t(13, 0), Readiness::Busy, false), Decision::SkipBusy, "passata un'intera scadenza");
        assert_eq!(decide(&d, anchor, t(13, 0), Readiness::Unavailable, false), Decision::Nothing, "claude non pronto: aspetta, mai saltata");
        let mut paused = d.clone();
        paused.paused = true;
        assert_eq!(decide(&paused, anchor, t(9, 0), Readiness::Ready, false), Decision::Nothing);
        let mut done = d;
        done.last_run = Some(t(9, 0));
        assert_eq!(decide(&done, anchor, t(12, 59), Readiness::Ready, false), Decision::Nothing);
        assert_eq!(decide(&done, anchor, t(13, 0), Readiness::Ready, false), Decision::Run);
        let broken = def("boh");
        assert_eq!(decide(&broken, anchor, t(23, 0), Readiness::Ready, false), Decision::Nothing, "frequenza non valida: mai");
    }
}
