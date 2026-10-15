//! JSON del canale `notify` (script `superzed-notify`): campi mancanti o sbagliati non
//! fanno perdere la notifica.
use crate::model::{NewNotification, Severity};
use std::path::PathBuf;

pub const MAX_DETAILS: usize = 1 << 20;
const MAX_TITLE_CHARS: usize = 200;
/// Riassunto e prompt suggerito oltre questi byte si tagliano (il testo intero va nel
/// dettaglio): restano leggibili nella card e nel prompt di Risolvi.
pub const MAX_SHORT_TEXT: usize = 4 * 1024;
const TRUNCATED: &str = "\n\n---\n*[superzed] dettaglio troncato a 1 MiB*";

#[derive(Clone, Debug, PartialEq)]
pub struct NotifyInput {
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    pub details: Option<String>,
    pub suggested_prompt: Option<String>,
}

impl NotifyInput {
    pub fn into_new(self, project_root: Option<PathBuf>, source: String) -> NewNotification {
        NewNotification {
            severity: self.severity,
            title: self.title,
            summary: self.summary,
            details: self.details,
            suggested_prompt: self.suggested_prompt,
            project_root,
            source,
        }
    }
}

fn text(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn truncate(mut s: String) -> String {
    if s.len() <= MAX_DETAILS {
        return s;
    }
    let mut end = MAX_DETAILS;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str(TRUNCATED);
    s
}

/// `s` entro `MAX_SHORT_TEXT` byte: tagliato su un confine di carattere, con "…".
/// `None` se era gia' entro il limite.
fn cap_short(s: &str) -> Option<String> {
    if s.len() <= MAX_SHORT_TEXT {
        return None;
    }
    let mut end = MAX_SHORT_TEXT - '…'.len_utf8();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    Some(format!("{}…", &s[..end]))
}

/// Riassunto e prompt suggerito troppo lunghi: nella notifica la versione tagliata, nel
/// dettaglio (il `.md`) il testo intero, in coda.
fn cap_short_fields(mut input: NotifyInput) -> NotifyInput {
    let mut full = String::new();
    if let Some(capped) = cap_short(&input.summary) {
        full.push_str(&format!("\n\n## Riassunto completo\n\n{}", input.summary));
        input.summary = capped;
    }
    if let Some(prompt) = input.suggested_prompt.as_mut()
        && let Some(capped) = cap_short(prompt)
    {
        full.push_str(&format!("\n\n## Prompt suggerito completo\n\n{prompt}"));
        *prompt = capped;
    }
    if !full.is_empty() {
        let details = input.details.take().unwrap_or_default();
        input.details = Some(truncate(format!("{details}{full}").trim_start().to_string()));
    }
    input
}

pub fn parse_notify_payload(v: &serde_json::Value) -> NotifyInput {
    if let Some(raw) = v.as_str() {
        return cap_short_fields(NotifyInput {
            severity: Severity::Info,
            title: "(untitled)".into(),
            summary: raw.trim().to_string(),
            details: None,
            suggested_prompt: None,
        });
    }
    let title = text(v, "title")
        .map(|t| t.chars().take(MAX_TITLE_CHARS).collect())
        .unwrap_or_else(|| "(untitled)".to_string());
    cap_short_fields(NotifyInput {
        severity: v.get("severity").and_then(|s| s.as_str()).and_then(Severity::parse).unwrap_or(Severity::Info),
        title,
        summary: text(v, "summary").unwrap_or_default(),
        details: v.get("details").and_then(|d| d.as_str()).filter(|d| !d.trim().is_empty()).map(|d| truncate(d.to_string())),
        suggested_prompt: text(v, "suggested_prompt"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn full_payload() {
        let p = parse_notify_payload(&json!({"severity": "critico", "title": " DB ", "summary": "s",
            "details": "trace", "suggested_prompt": "p"}));
        assert_eq!(p.severity, Severity::Critical);
        assert_eq!((p.title.as_str(), p.summary.as_str()), ("DB", "s"));
        assert_eq!(p.details.as_deref(), Some("trace"));
        assert_eq!(p.suggested_prompt.as_deref(), Some("p"));
    }

    #[test]
    fn missing_or_wrong_fields_get_defaults() {
        let p = parse_notify_payload(&json!({"severity": "boh", "title": "", "details": 3, "suggested_prompt": ""}));
        assert_eq!(p.severity, Severity::Info);
        assert_eq!(p.title, "(untitled)");
        assert_eq!(p.summary, "");
        assert_eq!(p.details, None);
        assert_eq!(p.suggested_prompt, None);
        let p = parse_notify_payload(&json!([1, 2]));
        assert_eq!((p.severity, p.title.as_str()), (Severity::Info, "(untitled)"));
    }

    #[test]
    fn non_json_body_becomes_a_summary() {
        // il canale notify consegna come stringa un corpo che non e' JSON
        let p = parse_notify_payload(&json!("disco pieno su db1\n"));
        assert_eq!((p.severity, p.title.as_str(), p.summary.as_str()), (Severity::Info, "(untitled)", "disco pieno su db1"));
    }

    #[test]
    fn huge_details_are_truncated_with_a_notice() {
        let big = "è".repeat(MAX_DETAILS); // 2 byte per carattere: oltre il limite
        let p = parse_notify_payload(&json!({"title": "x", "details": big}));
        let d = p.details.unwrap();
        assert!(d.len() <= MAX_DETAILS + 200);
        assert!(d.ends_with("*[superzed] dettaglio troncato a 1 MiB*"));
        let long_title = "t".repeat(500);
        assert_eq!(parse_notify_payload(&json!({"title": long_title})).title.chars().count(), 200);
    }

    // Revisione finale: riassunto e prompt suggerito si fermano a 4 KiB (su un confine di
    // carattere, con "…"); il testo intero resta nel dettaglio.
    #[test]
    fn long_summary_and_prompt_are_capped_and_kept_whole_in_the_details() {
        let summary = format!("a{}", "è".repeat(5000)); // 1 + 10000 byte: il taglio cade a meta' di una "è"
        let prompt = "p".repeat(MAX_SHORT_TEXT + 10);
        let p = parse_notify_payload(&json!({"title": "x", "summary": summary, "suggested_prompt": prompt, "details": "trace"}));
        assert!(p.summary.len() <= MAX_SHORT_TEXT, "{}", p.summary.len());
        assert!(p.summary.ends_with('…') && p.summary.starts_with("aè"));
        let sp = p.suggested_prompt.unwrap();
        assert!(sp.len() <= MAX_SHORT_TEXT && sp.ends_with('…'));
        let d = p.details.unwrap();
        assert!(d.starts_with("trace"));
        assert!(d.contains(&summary) && d.contains(&prompt), "testo intero nel dettaglio");
        // senza dettaglio: il testo intero ne crea uno
        let p = parse_notify_payload(&json!({"summary": summary}));
        assert!(p.details.unwrap().contains(&summary));
        // sotto il limite nulla cambia
        let p = parse_notify_payload(&json!({"summary": "breve", "suggested_prompt": "fai"}));
        assert_eq!((p.summary.as_str(), p.suggested_prompt.as_deref(), p.details), ("breve", Some("fai"), None));
        // anche un corpo non JSON
        let p = parse_notify_payload(&json!(summary));
        assert!(p.summary.len() <= MAX_SHORT_TEXT && p.details.unwrap().contains(&summary));
    }
}
