//! Modello di una notifica: gravita' (i colori vengono dal tema, in chi disegna), stato
//! e testo del prompt suggerito per risolverla.
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Gravita' di una notifica, dal meno al piu' grave (l'ordine e' anche l'`Ord`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Note,
    Info,
    Warning,
    Error,
    Critical,
}

impl Severity {
    pub const ALL: [Severity; 5] = [Severity::Note, Severity::Info, Severity::Warning, Severity::Error, Severity::Critical];

    pub fn label(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
            Severity::Critical => "critical",
        }
    }

    /// Parola italiana usata nei testi per claude (prompt «Risolvi» e file dei dettagli),
    /// che restano in italiano anche con l'interfaccia in inglese.
    pub fn prompt_label(self) -> &'static str {
        match self {
            Severity::Note => "nota",
            Severity::Info => "info",
            Severity::Warning => "avviso",
            Severity::Error => "errore",
            Severity::Critical => "critico",
        }
    }

    pub fn parse(s: &str) -> Option<Severity> {
        match s.trim().to_lowercase().as_str() {
            "nota" | "note" => Some(Severity::Note),
            "info" => Some(Severity::Info),
            "avviso" | "warning" | "warn" => Some(Severity::Warning),
            "errore" | "error" => Some(Severity::Error),
            "critico" | "critical" => Some(Severity::Critical),
            _ => None,
        }
    }

    /// Errore e critico contano come "allerta" (es. badge nella sidebar).
    pub fn alerts(self) -> bool {
        self >= Severity::Error
    }
}

/// Stato di una notifica nell'archivio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationState {
    New,
    Taken,
    Archived,
}

/// Azione di una notifica interna di superzed (es. «Rivedi» il report di una chat in
/// Fatto): `kind` dice chi la gestisce, `arg` e' il suo argomento. La card la mostra al
/// posto di «Risolvi».
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationAction {
    pub kind: String,
    pub label: String,
    pub arg: String,
}

impl NotificationAction {
    /// Etichetta da mostrare: le notifiche salvate prima dell'interfaccia in inglese hanno
    /// ancora «Rivedi».
    pub fn display_label(&self) -> String {
        match self.label.as_str() {
            "Rivedi" => "Review".to_string(),
            label => label.to_string(),
        }
    }
}

/// Notifica salvata nell'archivio.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub created_at: DateTime<Local>,
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    pub details_path: Option<PathBuf>,
    pub suggested_prompt: Option<String>,
    pub project_root: Option<PathBuf>,
    pub source: String,
    pub state: NotificationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<NotificationAction>,
}

/// Dati di una notifica in arrivo, prima che l'archivio le assegni id e data.
#[derive(Clone, Debug, PartialEq)]
pub struct NewNotification {
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    pub details: Option<String>,
    pub suggested_prompt: Option<String>,
    pub project_root: Option<PathBuf>,
    pub source: String,
}

/// Ordine di visualizzazione: piu' gravi prima, poi le piu' recenti.
pub fn sort_for_display(list: &mut [Notification]) {
    list.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.created_at.cmp(&a.created_at)));
}

/// Testo relativo tra due istanti (in inglese). Un orologio spostato all'indietro
/// (esito negativo) non produce date assurde: diventa "ora".
pub fn relative_time(then: DateTime<Local>, now: DateTime<Local>) -> String {
    let minutes = (now - then).num_minutes();
    if minutes < 1 {
        "now".to_string()
    } else if minutes < 60 {
        format!("{minutes} min ago")
    } else if minutes < 24 * 60 {
        format!("{} h ago", minutes / 60)
    } else {
        format!("{} d ago", minutes / (24 * 60))
    }
}

/// Nome dell'ultima componente della cartella del progetto, o "senza progetto".
pub fn project_label(root: Option<&Path>) -> String {
    root.and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no project".to_string())
}

/// Prompt da mandare a claude per risolvere la notifica: titolo e sorgente sempre,
/// poi riassunto, percorso dei dettagli completi e suggerimento se ci sono.
pub fn resolve_prompt(n: &Notification) -> String {
    let mut lines = vec![format!("Risolvi questo problema segnalato da «{}» ({}): {}", n.source, n.severity.prompt_label(), n.title)];
    if !n.summary.is_empty() {
        lines.push(n.summary.clone());
    }
    if let Some(path) = &n.details_path {
        lines.push(format!("Dettagli completi: {}", path.display()));
    }
    if let Some(p) = &n.suggested_prompt {
        lines.push(p.clone());
    }
    lines.join("\n")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 27, h, m, 0).unwrap()
    }

    pub(crate) fn sample(id: &str, severity: Severity, created: DateTime<Local>) -> Notification {
        Notification {
            id: id.into(),
            created_at: created,
            severity,
            title: id.into(),
            summary: String::new(),
            details_path: None,
            suggested_prompt: None,
            project_root: None,
            source: "chat".into(),
            state: NotificationState::New,
            action: None,
        }
    }

    #[test]
    fn severity_labels_and_parse() {
        assert_eq!(Severity::ALL.map(Severity::label), ["note", "info", "warning", "error", "critical"]);
        assert_eq!(Severity::ALL.map(Severity::prompt_label), ["nota", "info", "avviso", "errore", "critico"]);
        assert_eq!(Severity::parse("Errore"), Some(Severity::Error));
        assert_eq!(Severity::parse("warning"), Some(Severity::Warning));
        assert_eq!(Severity::parse("avviso"), Some(Severity::Warning));
        assert_eq!(Severity::parse("boh"), None);
        assert!(Severity::Critical > Severity::Error && Severity::Info > Severity::Note);
        assert!(Severity::Error.alerts() && Severity::Critical.alerts() && !Severity::Warning.alerts());
        assert_eq!(serde_json::to_string(&Severity::Warning).unwrap(), "\"warning\"");
    }

    #[test]
    fn old_italian_action_labels_are_shown_in_english() {
        let action = |label: &str| NotificationAction { kind: "done_review".into(), label: label.into(), arg: "s".into() };
        assert_eq!(action("Rivedi").display_label(), "Review");
        assert_eq!(action("Review").display_label(), "Review");
        assert_eq!(action("Open").display_label(), "Open");
    }

    #[test]
    fn display_order_is_severity_then_newest() {
        let mut list = vec![
            sample("info", Severity::Info, at(12, 0)),
            sample("err-old", Severity::Error, at(9, 0)),
            sample("crit", Severity::Critical, at(8, 0)),
            sample("err-new", Severity::Error, at(11, 0)),
        ];
        sort_for_display(&mut list);
        assert_eq!(list.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["crit", "err-new", "err-old", "info"]);
    }

    #[test]
    fn relative_time_labels() {
        assert_eq!(relative_time(at(10, 0), at(10, 0)), "now");
        assert_eq!(relative_time(at(10, 0), at(10, 3)), "3 min ago");
        assert_eq!(relative_time(at(8, 0), at(10, 30)), "2 h ago");
        let two_days = Local.with_ymd_and_hms(2026, 9, 25, 10, 0, 0).unwrap();
        assert_eq!(relative_time(two_days, at(10, 0)), "2 d ago");
        assert_eq!(relative_time(at(10, 5), at(10, 0)), "now", "nel futuro (orologio spostato)");
    }

    #[test]
    fn resolve_prompt_has_title_summary_details_and_suggestion() {
        let mut n = sample("id", Severity::Error, at(10, 0));
        n.title = "DB giu'".into();
        n.summary = "timeout".into();
        n.source = "Log produzione".into();
        n.details_path = Some("/data/notifications/id.md".into());
        n.suggested_prompt = Some("controlla il pool".into());
        assert_eq!(
            resolve_prompt(&n),
            "Risolvi questo problema segnalato da «Log produzione» (errore): DB giu'\ntimeout\nDettagli completi: /data/notifications/id.md\ncontrolla il pool"
        );
        n.details_path = None;
        n.suggested_prompt = None;
        n.summary.clear();
        assert_eq!(resolve_prompt(&n), "Risolvi questo problema segnalato da «Log produzione» (errore): DB giu'");
        assert_eq!(project_label(Some(Path::new("/Users/u/p1"))), "p1");
        assert_eq!(project_label(None), "no project");
    }
}
