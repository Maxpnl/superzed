//! Report di una chat arrivata in Fatto: schema, validazione, file su disco e prompt.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const MAX_ENTRIES: usize = 50;
pub const MAX_ENTRY_CHARS: usize = 2000;
pub const REPORTS_DIR: &str = ".superzed/reports";
/// Riga che separa il prompt dal transcript nello stdin di `claude -p`.
pub const TRANSCRIPT_SEPARATOR: &str = "---TRANSCRIPT---";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchAction {
    Close,
    Update,
    Comment,
}

impl TouchAction {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "close" => Some(Self::Close),
            "update" => Some(Self::Update),
            "comment" => Some(Self::Comment),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Update => "update",
            Self::Comment => "comment",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Close => "chiudi",
            Self::Update => "aggiorna Status",
            Self::Comment => "commenta",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Blocker {
    pub title: String,
    pub waiting_on: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TouchedIssue {
    pub number: u64,
    pub action: TouchAction,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub session_id: String,
    pub area: Option<String>,
    pub done: Vec<String>,
    pub blockers: Vec<Blocker>,
    pub missing: Vec<String>,
    pub problems: Vec<String>,
    pub issues_touched: Vec<TouchedIssue>,
}

/// Valida il JSON del report (vedi Global Constraints). `session_id` e' sempre quello
/// passato: il file non puo' spacciarsi per un'altra sessione.
pub fn parse_report(text: &str, session_id: &str) -> Result<Report, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("report: invalid JSON ({e})"))?;
    let obj = v.as_object().ok_or_else(|| "report: a JSON object is needed".to_string())?;
    Ok(Report {
        session_id: session_id.to_string(),
        area: opt_text(obj.get("area"), "area")?,
        done: list(obj.get("done"), "done", text_item)?,
        blockers: list(obj.get("blockers"), "blockers", blocker_item)?,
        missing: list(obj.get("missing"), "missing", text_item)?,
        problems: list(obj.get("problems"), "problems", text_item)?,
        issues_touched: list(obj.get("issues_touched"), "issues_touched", touched_item)?,
    })
}

fn list<T>(v: Option<&Value>, name: &str, item: fn(&Value, &str) -> Result<T, String>) -> Result<Vec<T>, String> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let arr = v.as_array().ok_or_else(|| format!("{name}: a list is needed"))?;
    if arr.len() > MAX_ENTRIES {
        return Err(format!("{name}: {} entries, at most {MAX_ENTRIES}", arr.len()));
    }
    arr.iter().enumerate().map(|(i, v)| item(v, &format!("{name}[{i}]"))).collect()
}

fn text(v: Option<&Value>, path: &str) -> Result<String, String> {
    let s = v.and_then(Value::as_str).ok_or_else(|| format!("{path}: a string is needed"))?.trim();
    if s.is_empty() {
        return Err(format!("{path}: empty string"));
    }
    if s.chars().count() > MAX_ENTRY_CHARS {
        return Err(format!("{path}: more than {MAX_ENTRY_CHARS} characters"));
    }
    Ok(s.to_string())
}

fn opt_text(v: Option<&Value>, path: &str) -> Result<Option<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v) => text(Some(v), path).map(Some),
    }
}

fn text_item(v: &Value, path: &str) -> Result<String, String> {
    text(Some(v), path)
}

fn blocker_item(v: &Value, path: &str) -> Result<Blocker, String> {
    let o = v.as_object().ok_or_else(|| format!("{path}: an object is needed"))?;
    Ok(Blocker {
        title: text(o.get("title"), &format!("{path}.title"))?,
        waiting_on: opt_text(o.get("waiting_on"), &format!("{path}.waiting_on"))?,
    })
}

fn touched_item(v: &Value, path: &str) -> Result<TouchedIssue, String> {
    let o = v.as_object().ok_or_else(|| format!("{path}: an object is needed"))?;
    let number = o
        .get("number")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("{path}.number: an issue number is needed"))?;
    let raw = o.get("action").and_then(Value::as_str).unwrap_or_default();
    let action = TouchAction::parse(raw).ok_or_else(|| {
        format!("{path}.action: “{}” is not close, update or comment", raw.chars().take(40).collect::<String>())
    })?;
    Ok(TouchedIssue { number, action, note: opt_text(o.get("note"), &format!("{path}.note"))? })
}

/// Il report nello stesso schema del file (per `write_report`).
pub fn report_json(r: &Report) -> String {
    let v = json!({
        "session_id": r.session_id,
        "area": r.area,
        "done": r.done,
        "blockers": r.blockers.iter().map(|b| json!({"title": b.title, "waiting_on": b.waiting_on})).collect::<Vec<_>>(),
        "missing": r.missing,
        "problems": r.problems,
        "issues_touched": r.issues_touched.iter()
            .map(|t| json!({"number": t.number, "action": t.action.as_str(), "note": t.note}))
            .collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

/// Una riga per la card della notifica.
pub fn report_summary(r: &Report) -> String {
    format!(
        "{} done · {} blockers · {} missing · {} problems · {} issues touched",
        r.done.len(),
        r.blockers.len(),
        r.missing.len(),
        r.problems.len(),
        r.issues_touched.len()
    )
}

/// Markdown del report (dettagli della notifica e riepilogo sull'issue principale); le
/// sezioni vuote non compaiono.
pub fn report_markdown(r: &Report) -> String {
    let mut out = String::new();
    let mut section = |title: &str, lines: Vec<String>| {
        if lines.is_empty() {
            return;
        }
        out.push_str(&format!("## {title}\n"));
        for line in lines {
            out.push_str(&format!("- {}\n", line.replace('\n', " ")));
        }
        out.push('\n');
    };
    section("Fatto", r.done.clone());
    section(
        "Blocchi",
        r.blockers
            .iter()
            .map(|b| match &b.waiting_on {
                Some(w) => format!("{} (in attesa di {w})", b.title),
                None => b.title.clone(),
            })
            .collect(),
    );
    section("Manca", r.missing.clone());
    section("Problemi", r.problems.clone());
    section(
        "Issue toccate",
        r.issues_touched
            .iter()
            .map(|t| format!("#{} {}{}", t.number, t.action.label(), t.note.as_ref().map(|n| format!(": {n}")).unwrap_or_default()))
            .collect(),
    );
    out.trim_end().to_string()
}

/// Id di sessione usabile come nome di file (gli id di claude sono UUID).
pub fn safe_session_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn checked(id: &str) -> Result<&str, String> {
    if safe_session_id(id) {
        Ok(id)
    } else {
        Err(format!("invalid session id “{}”", id.chars().take(64).collect::<String>()))
    }
}

pub fn report_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    Ok(root.join(REPORTS_DIR).join(format!("{}.json", checked(id)?)))
}

pub fn result_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    Ok(root.join(REPORTS_DIR).join(format!("{}.result.json", checked(id)?)))
}

/// Crea `.superzed/reports` (con un `.gitignore` che la tiene fuori da git) e la restituisce.
pub fn ensure_reports_dir(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join(REPORTS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n").map_err(|e| format!("{}: {e}", ignore.display()))?;
    }
    Ok(dir)
}

/// Scrive in `<file>.tmp` e rinomina: chi legge vede il file vecchio o quello nuovo, mai a meta'.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_report(root: &Path, id: &str) -> Result<Report, String> {
    let path = report_path(root, id)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_report(&text, id),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!("report missing: {}", path.display())),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn write_report(root: &Path, r: &Report) -> Result<(), String> {
    ensure_reports_dir(root)?;
    write_atomic(&report_path(root, &r.session_id)?, &report_json(r))
}

fn schema_example(session_id: &str) -> String {
    format!(
        r#"{{"session_id": "{session_id}", "area": "<area o null>", "done": ["…"], "blockers": [{{"title": "…", "waiting_on": "<chi o cosa si aspetta, o null>"}}], "missing": ["…"], "problems": ["…"], "issues_touched": [{{"number": 12, "action": "close|update|comment", "note": "…"}}]}}"#
    )
}

fn rules(areas: &[String]) -> String {
    let area = if areas.is_empty() { "null".to_string() } else { format!("una fra {} (o null)", areas.join(", ")) };
    format!(
        "- done: cosa e' stato fatto in questa sessione\n- blockers: cosa blocca il lavoro e chi o cosa si aspetta\n- missing: cosa manca ancora per chiudere il lavoro\n- problems: problemi o domande aperte\n- issues_touched: issue GitHub del repo toccate (close = da chiudere, update = da aggiornare, comment = da commentare) con una nota\n- area: {area}\n- al massimo {MAX_ENTRIES} voci per lista e {MAX_ENTRY_CHARS} caratteri per voce, nessuna stringa vuota"
    )
}

/// Prompt fisso scritto nella chat quando entra in Fatto.
pub fn chat_prompt(path: &Path, session_id: &str, areas: &[String]) -> String {
    format!(
        "superzed: questa chat e' passata in Fatto. Scrivi il report della sessione nel file {} (sovrascrivilo se esiste) come JSON con questo schema:\n{}\n{}\nScrivi solo quel file: niente commit, niente altre modifiche, nessuna scrittura su GitHub.",
        path.display(),
        schema_example(session_id),
        rules(areas)
    )
}

/// Prompt di `claude -p`: il transcript segue su stdin dopo `TRANSCRIPT_SEPARATOR`.
pub fn headless_prompt(session_id: &str, areas: &[String]) -> String {
    format!(
        "Qui sotto, dopo la riga {TRANSCRIPT_SEPARATOR}, c'e' la parte finale del transcript JSONL di una sessione di Claude Code. Ricava il report della sessione e rispondi solo con un oggetto JSON con questo schema, senza altro testo:\n{}\n{}",
        schema_example(session_id),
        rules(areas)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::SID;

    const FULL: &str = r#"{"session_id": "altro", "area": "frontend", "extra": {"x": 1},
      "done": ["Import utenti", "  Login  "],
      "blockers": [{"title": "Policy annullamento", "waiting_on": "design"}, {"title": "Staging", "waiting_on": ""}],
      "missing": ["Test su Firefox"], "problems": ["Crash su Safari"],
      "issues_touched": [{"number": 12, "action": "close", "note": "fatto"}, {"number": 8, "action": "comment", "note": null}]}"#;

    fn err(text: &str) -> String {
        parse_report(text, SID).unwrap_err()
    }

    #[test]
    fn a_full_report_is_parsed_and_extra_fields_ignored() {
        let r = parse_report(FULL, SID).unwrap();
        assert_eq!(r.session_id, SID, "l'id e' sempre quello della sessione");
        assert_eq!(r.area.as_deref(), Some("frontend"));
        assert_eq!(r.done, ["Import utenti", "Login"]);
        assert_eq!(
            r.blockers,
            [
                Blocker { title: "Policy annullamento".into(), waiting_on: Some("design".into()) },
                Blocker { title: "Staging".into(), waiting_on: None },
            ]
        );
        assert_eq!((r.missing.len(), r.problems.len()), (1, 1));
        assert_eq!(
            r.issues_touched,
            [
                TouchedIssue { number: 12, action: TouchAction::Close, note: Some("fatto".into()) },
                TouchedIssue { number: 8, action: TouchAction::Comment, note: None },
            ]
        );
    }

    #[test]
    fn missing_or_null_lists_are_empty() {
        assert_eq!(parse_report("{}", SID).unwrap(), Report { session_id: SID.into(), ..Default::default() });
        assert!(parse_report(r#"{"done": null, "area": null}"#, SID).unwrap().done.is_empty());
    }

    #[test]
    fn limits_and_types_are_enforced() {
        assert!(err("non json").starts_with("report: invalid JSON"));
        assert_eq!(err("[]"), "report: a JSON object is needed");
        assert_eq!(err(r#"{"done": "a"}"#), "done: a list is needed");
        assert_eq!(err(r#"{"done": ["  "]}"#), "done[0]: empty string");
        assert_eq!(err(r#"{"missing": ["a", 1]}"#), "missing[1]: a string is needed");
        let many = serde_json::json!({"problems": vec!["x"; MAX_ENTRIES + 1]}).to_string();
        assert_eq!(err(&many), "problems: 51 entries, at most 50");
        let long = serde_json::json!({"done": ["x".repeat(MAX_ENTRY_CHARS + 1)]}).to_string();
        assert_eq!(err(&long), "done[0]: more than 2000 characters");
        assert_eq!(err(r#"{"blockers": [{"waiting_on": "x"}]}"#), "blockers[0].title: a string is needed");
        assert_eq!(err(r#"{"blockers": ["x"]}"#), "blockers[0]: an object is needed");
        for bad in [r#"0"#, r#""12""#, r#"-1"#] {
            let text = format!(r#"{{"issues_touched": [{{"number": {bad}, "action": "close"}}]}}"#);
            assert_eq!(err(&text), "issues_touched[0].number: an issue number is needed", "{bad}");
        }
        assert_eq!(
            err(r#"{"issues_touched": [{"number": 3, "action": "delete"}]}"#),
            "issues_touched[0].action: “delete” is not close, update or comment"
        );
        // al limite si passa; i caratteri si contano come caratteri, non byte
        let edge = serde_json::json!({"done": vec!["è".repeat(MAX_ENTRY_CHARS); MAX_ENTRIES]}).to_string();
        assert_eq!(parse_report(&edge, SID).unwrap().done.len(), MAX_ENTRIES);
    }

    #[test]
    fn session_ids_are_path_safe() {
        assert!(safe_session_id(SID));
        for bad in ["", "../x", "a/b", "a b", "a.json", &"a".repeat(65)] {
            assert!(!safe_session_id(bad), "{bad}");
        }
        let root = Path::new("/p");
        assert_eq!(report_path(root, SID).unwrap(), Path::new("/p/.superzed/reports").join(format!("{SID}.json")));
        assert_eq!(result_path(root, SID).unwrap(), Path::new("/p/.superzed/reports").join(format!("{SID}.result.json")));
        assert!(report_path(root, "../x").unwrap_err().contains("invalid session id"));
    }

    #[test]
    fn files_roundtrip_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let reports = ensure_reports_dir(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(reports.join(".gitignore")).unwrap(), "*\n");
        assert!(read_report(dir.path(), SID).unwrap_err().contains("report missing"));
        let r = parse_report(FULL, SID).unwrap();
        write_report(dir.path(), &r).unwrap();
        assert_eq!(read_report(dir.path(), SID).unwrap(), r);
        assert!(!reports.join(format!("{SID}.json.tmp")).exists(), "niente file temporanei");
    }

    #[test]
    fn prompts_carry_path_schema_and_rules() {
        let areas = vec!["frontend".to_string(), "backend".to_string()];
        let path = Path::new("/p/.superzed/reports/x.json");
        let chat = chat_prompt(path, SID, &areas);
        assert!(chat.starts_with("superzed: questa chat e' passata in Fatto. Scrivi il report della sessione nel file /p/.superzed/reports/x.json"), "{chat}");
        for needle in [SID, "\"issues_touched\"", "area: una fra frontend, backend (o null)", "al massimo 50 voci", "nessuna scrittura su GitHub"] {
            assert!(chat.contains(needle), "{needle}: {chat}");
        }
        let headless = headless_prompt(SID, &[]);
        assert!(headless.contains(TRANSCRIPT_SEPARATOR) && headless.contains("rispondi solo con un oggetto JSON"));
        assert!(headless.contains("- area: null"), "{headless}");
    }

    #[test]
    fn summary_and_markdown() {
        let r = parse_report(FULL, SID).unwrap();
        assert_eq!(report_summary(&r), "2 done · 2 blockers · 1 missing · 1 problems · 2 issues touched");
        let md = report_markdown(&r);
        assert!(md.starts_with("## Fatto\n- Import utenti\n- Login\n"), "{md}");
        assert!(md.contains("## Blocchi\n- Policy annullamento (in attesa di design)\n- Staging\n"), "{md}");
        assert!(md.contains("- #12 chiudi: fatto\n- #8 commenta"), "{md}");
        let empty = Report { session_id: SID.into(), done: vec!["solo questo".into()], ..Default::default() };
        assert_eq!(report_markdown(&empty), "## Fatto\n- solo questo");
    }
}
