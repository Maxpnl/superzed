//! Gmail in lettura e risposta via `gog` (openclaw/gogcli): argomenti, JSON e errori
//! stanno solo qui, cosi' un cambio di formato di gog si corregge in un punto.
//! L'autenticazione e' tutta di gog (`gog auth add`): superzed non tiene segreti Gmail.
use crate::runner::{RunError, RunOutput};
use chrono::{DateTime, FixedOffset};
use serde::Serialize;
use serde_json::Value;

pub const GOG: &str = "gog";
/// Variabile che fa rifiutare a gog ogni invio (bozze permesse): la ricevono le chat.
pub const NO_SEND_ENV: &str = "GOG_GMAIL_NO_SEND";
pub const MIN_GOG_VERSION: (u32, u32, u32) = (0, 44, 0);
pub const GOG_MISSING: &str = "gog not found: install it with “brew install openclaw/tap/gogcli”, then run “gog auth add <account> --services gmail”";
const BAD_OUTPUT_CHARS: usize = 200;

pub fn gog_not_authorized(account: &str) -> String {
    format!("gog has no Gmail access for “{account}”: run “gog auth add {account} --services gmail”")
}

pub fn gog_too_old(found: &str) -> String {
    let (major, minor, patch) = MIN_GOG_VERSION;
    format!("gog {found} is too old (need v{major}.{minor}.{patch}): run “brew upgrade gogcli”")
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadSummary {
    pub id: String,
    pub subject: String,
    pub from: String,
    pub date: Option<DateTime<FixedOffset>>,
    pub unread: bool,
    pub message_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MailMessage {
    #[serde(rename = "message_id")]
    pub id: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub reply_to: String,
    pub date: String,
    pub body: String,
    pub attachments: Vec<String>,
    #[serde(skip)]
    pub unread: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MailThread {
    #[serde(rename = "thread_id")]
    pub id: String,
    pub subject: String,
    pub messages: Vec<MailMessage>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub id: String,
    pub message_id: String,
    pub thread_id: String,
}

/// La query del progetto resta fra parentesi: `a OR b` seguito dalla ricerca del nodo
/// diventerebbe `a OR (b ricerca)` e allargherebbe il filtro invece di restringerlo.
pub fn project_query(project: &str, search: &str) -> Option<String> {
    let project = project.trim();
    if project.is_empty() {
        return None;
    }
    let search = search.trim();
    Some(if search.is_empty() { format!("({project})") } else { format!("({project}) {search}") })
}

pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let token = text.split_whitespace().next()?;
    let mut parts = token.trim_start_matches('v').split('.').map(|part| part.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

fn bad_output(what: &str, text: &str) -> String {
    let start: String = text.chars().take(BAD_OUTPUT_CHARS).collect();
    format!("unexpected output from gog {what}: {start}")
}

fn parse_json(what: &str, text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|_| bad_output(what, text))
}

fn string_at(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn parse_search(text: &str) -> Result<Vec<ThreadSummary>, String> {
    let root = parse_json("gmail search", text)?;
    let threads = root.get("threads").and_then(Value::as_array).ok_or_else(|| bad_output("gmail search", text))?;
    Ok(threads
        .iter()
        .filter_map(|thread| {
            let id = thread.get("id").and_then(Value::as_str).filter(|id| !id.is_empty())?.to_string();
            let labels = thread.get("labels").and_then(Value::as_array);
            Some(ThreadSummary {
                id,
                subject: string_at(thread, "subject"),
                from: string_at(thread, "from"),
                date: thread.get("internalDateIso").and_then(Value::as_str).and_then(|d| DateTime::parse_from_rfc3339(d).ok()),
                unread: labels.is_some_and(|labels| labels.iter().any(|l| l.as_str() == Some("UNREAD"))),
                message_count: thread.get("messageCount").and_then(Value::as_u64).unwrap_or(1) as usize,
            })
        })
        .collect())
}

pub fn parse_thread(text: &str) -> Result<MailThread, String> {
    let root = parse_json("gmail thread get", text)?;
    let thread = root.get("thread").ok_or_else(|| bad_output("gmail thread get", text))?;
    let messages: Vec<MailMessage> = thread
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_output("gmail thread get", text))?
        .iter()
        .map(|message| {
            let headers = message.get("headers").cloned().unwrap_or(Value::Null);
            MailMessage {
                id: string_at(message, "id"),
                from: string_at(&headers, "from"),
                to: string_at(&headers, "to"),
                cc: string_at(&headers, "cc"),
                reply_to: string_at(&headers, "reply_to"),
                date: string_at(&headers, "date"),
                body: string_at(message, "body"),
                attachments: message
                    .get("attachments")
                    .and_then(Value::as_array)
                    .map(|list| list.iter().map(|a| string_at(a, "filename")).filter(|name| !name.is_empty()).collect())
                    .unwrap_or_default(),
                unread: message
                    .get("labelIds")
                    .and_then(Value::as_array)
                    .is_some_and(|labels| labels.iter().any(|l| l.as_str() == Some("UNREAD"))),
            }
        })
        .collect();
    let subject = thread
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|first| first.get("headers"))
        .map(|headers| string_at(headers, "subject"))
        .unwrap_or_default();
    Ok(MailThread { id: string_at(thread, "id"), subject, messages })
}

pub fn parse_message_body(text: &str) -> Result<String, String> {
    let root = parse_json("gmail get", text)?;
    root.get("body").and_then(Value::as_str).map(str::to_string).ok_or_else(|| bad_output("gmail get", text))
}

pub fn parse_drafts(text: &str) -> Result<Vec<Draft>, String> {
    let root = parse_json("gmail drafts list", text)?;
    let drafts = root.get("drafts").and_then(Value::as_array).ok_or_else(|| bad_output("gmail drafts list", text))?;
    Ok(drafts
        .iter()
        .filter_map(|draft| {
            let thread_id = draft.get("threadId").and_then(Value::as_str).filter(|t| !t.is_empty())?;
            Some(Draft { id: string_at(draft, "id"), message_id: string_at(draft, "messageId"), thread_id: thread_id.to_string() })
        })
        .collect())
}

/// Exit 4 di gog = token mancante o scaduto; per il resto il messaggio dell'envelope
/// JSON su stdout, altrimenti stderr.
pub fn gog_failure(output: &RunOutput, account: &str) -> String {
    if output.code == Some(4) {
        return gog_not_authorized(account);
    }
    let from_json = serde_json::from_str::<Value>(output.stdout.trim())
        .ok()
        .and_then(|root| root.get("error")?.get("message")?.as_str().map(str::to_string));
    let message = from_json.unwrap_or_else(|| output.stderr.trim().to_string());
    format!("gog: {}", message.lines().next().unwrap_or_default())
}

pub fn run_failure(error: RunError) -> String {
    match error {
        RunError::NotFound(_) => GOG_MISSING.to_string(),
        RunError::Failed(message) => message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{RunError, RunOutput};
    use crate::test_support::{GOG_AUTH_ERROR, GOG_DRAFTS, GOG_MESSAGE_1, GOG_SEARCH, GOG_THREAD, GOG_VERSION};

    #[test]
    fn project_query_is_always_parenthesized_and_empty_means_not_configured() {
        assert_eq!(project_query("  ", "fattura"), None);
        assert_eq!(project_query("from:a OR from:b", ""), Some("(from:a OR from:b)".into()));
        assert_eq!(project_query("from:a OR from:b", " fattura "), Some("(from:a OR from:b) fattura".into()));
    }

    #[test]
    fn version_is_read_with_or_without_v_and_build_info() {
        assert_eq!(parse_version(GOG_VERSION), Some((0, 44, 0)));
        assert_eq!(parse_version("0.45.2"), Some((0, 45, 2)));
        assert_eq!(parse_version("dev"), None);
    }

    #[test]
    fn search_reads_threads_with_missing_optional_fields() {
        let threads = parse_search(GOG_SEARCH).unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].id, "t-200");
        assert_eq!(threads[0].from, "Alice <alice@example.com>");
        assert!(threads[0].unread);
        assert_eq!(threads[0].message_count, 2);
        assert_eq!(threads[0].date.map(|d| d.to_rfc3339()), Some("2026-10-14T09:12:00+02:00".into()));
        assert!(!threads[1].unread);
        assert_eq!(threads[1].date, None);
        assert_eq!(parse_search(r#"{"count":0,"threads":[]}"#).unwrap(), vec![]);
    }

    #[test]
    fn unexpected_json_is_an_error_with_the_start_of_the_output() {
        let err = parse_search("Usage: gog gmail search").unwrap_err();
        assert!(err.contains("Usage: gog gmail search"), "{err}");
        assert!(parse_search(r#"{"items":[]}"#).is_err());
    }

    #[test]
    fn thread_reads_headers_labels_and_attachment_names() {
        let thread = parse_thread(GOG_THREAD).unwrap();
        assert_eq!(thread.id, "t-200");
        assert_eq!(thread.subject, "Policy di annullamento approvata");
        assert_eq!(thread.messages.len(), 2);
        assert_eq!(thread.messages[0].attachments, vec!["policy.pdf".to_string()]);
        assert!(!thread.messages[0].unread);
        assert!(thread.messages[1].unread);
        assert_eq!(thread.messages[1].reply_to, "team@example.com");
        assert_eq!(thread.messages[1].cc, "bob@example.com");
    }

    #[test]
    fn message_body_keeps_line_breaks() {
        assert_eq!(parse_message_body(GOG_MESSAGE_1).unwrap(), "Ciao,\nla policy è approvata.\n\nPotete procedere con l'annullamento.\n");
    }

    #[test]
    fn drafts_without_thread_are_skipped() {
        assert_eq!(parse_drafts(GOG_DRAFTS).unwrap(), vec![Draft { id: "r-1".into(), message_id: "m-900".into(), thread_id: "t-200".into() }]);
    }

    #[test]
    fn failures_are_actionable() {
        let auth = RunOutput { success: false, code: Some(4), stdout: GOG_AUTH_ERROR.into(), stderr: "No auth".into() };
        assert_eq!(gog_failure(&auth, "j.doe@example.com"), gog_not_authorized("j.doe@example.com"));
        let usage = RunOutput { success: false, code: Some(2), stdout: r#"{"error":{"code":2,"message":"unknown flag --bogus","class":"usage"}}"#.into(), stderr: String::new() };
        assert_eq!(gog_failure(&usage, "a"), "gog: unknown flag --bogus");
        let plain = RunOutput { success: false, code: Some(1), stdout: String::new(), stderr: "boom\n".into() };
        assert_eq!(gog_failure(&plain, "a"), "gog: boom");
        assert_eq!(run_failure(RunError::NotFound("gog".into())), GOG_MISSING);
    }
}
