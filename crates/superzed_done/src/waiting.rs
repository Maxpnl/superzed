//! Attese: le issue aperte con l'etichetta `blocker`. Si leggono con `gh`, se ne apre una su
//! richiesta (`superzed-wait`) e l'elenco si copia in `.superzed/waiting.json` per chi lo legge senza rete.
use crate::gh::{BLOCKER_LABEL, close_args, ensure_label, gh, issue_create_args, parse_created_issue};
use crate::preview::{BODY_MAX_CHARS, WAITING_PREFIX};
use crate::report::safe_session_id;
use crate::result::FoundIssue;
use serde_json::{Value, json};
use std::sync::Arc;
use superzed_sources::Runner;
use superzed_sources::github::PmConfig;

pub const WAITING_FILE: &str = ".superzed/waiting.json";

#[derive(Clone, Debug, PartialEq)]
pub struct WaitingItem {
    pub number: u64,
    pub title: String,
    pub waiting_on: Option<String>,
    pub mine: bool,
    pub created_at: i64,
    pub url: String,
    pub session: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WaitOn {
    Me,
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct WaitRequest {
    pub title: String,
    pub on: WaitOn,
    pub detail: Option<String>,
}

const TITLE_MAX_CHARS: usize = 256;
const SESSION_START: &str = "<!-- superzed:session=";

pub fn wait_marker(session: &str) -> String {
    format!("{SESSION_START}{session}:wait -->")
}

/// Sessione che ha aperto la issue: dal marker `:wait` o da quello `:item=N` della pipeline.
pub fn session_of(body: &str) -> Option<String> {
    body.match_indices(SESSION_START).find_map(|(at, _)| {
        let rest = &body[at + SESSION_START.len()..];
        let session: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        let after = &rest[session.len()..];
        let known_marker = after.starts_with(":wait -->")
            || after.strip_prefix(":item=").is_some_and(|tail| {
                let digits = tail.chars().take_while(char::is_ascii_digit).count();
                digits > 0 && tail[digits..].starts_with(" -->")
            });
        (known_marker && safe_session_id(&session)).then_some(session)
    })
}

fn text_field(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

pub fn parse_wait_request(payload: &Value) -> Result<WaitRequest, String> {
    let title = text_field(payload, "title").ok_or("superzed-wait: a title is needed")?;
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(format!("superzed-wait: the title is longer than {TITLE_MAX_CHARS} characters"));
    }
    let on = text_field(payload, "on").ok_or("superzed-wait: say who is awaited (--on me, or a name)")?;
    let on = if on.eq_ignore_ascii_case("me") { WaitOn::Me } else { WaitOn::Other(on) };
    let detail = text_field(payload, "detail");
    if detail.as_ref().is_some_and(|detail| detail.chars().count() > BODY_MAX_CHARS) {
        return Err(format!("superzed-wait: the detail is longer than {BODY_MAX_CHARS} characters"));
    }
    Ok(WaitRequest { title, on, detail })
}

pub fn wait_body(request: &WaitRequest, session: &str) -> String {
    let who = match &request.on {
        WaitOn::Me => "owner",
        WaitOn::Other(name) => name,
    };
    let text = request.detail.as_deref().unwrap_or(&request.title);
    format!("{WAITING_PREFIX}{who}\n\n{text}\n\n{}", wait_marker(session))
}

pub fn waiting_list_args(repo: &str) -> Vec<String> {
    ["issue", "list", "-R", repo, "--label", BLOCKER_LABEL, "--state", "open", "--limit", "200", "--json", "number,title,body,createdAt,url,assignees"]
        .iter()
        .map(|part| part.to_string())
        .collect()
}

/// Prima le attese assegnate a `user`, poi le piu' vecchie.
pub fn parse_waiting(json: &str, user: Option<&str>) -> Result<Vec<WaitingItem>, String> {
    let value: Value = serde_json::from_str(json).map_err(|_| "gh issue list: non-JSON output".to_string())?;
    let issues = value.as_array().ok_or("gh issue list: non-JSON output")?;
    let mut items = Vec::new();
    for issue in issues {
        let (Some(number), Some(title), Some(url)) = (
            issue.get("number").and_then(Value::as_u64),
            issue.get("title").and_then(Value::as_str),
            issue.get("url").and_then(Value::as_str),
        ) else {
            continue;
        };
        let body = issue.get("body").and_then(Value::as_str).unwrap_or_default();
        let waiting_on = body
            .lines()
            .next()
            .and_then(|first| first.strip_prefix(WAITING_PREFIX))
            .map(str::trim)
            .filter(|who| !who.is_empty())
            .map(str::to_string);
        let mine = user.is_some_and(|user| {
            issue
                .get("assignees")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|assignee| assignee.get("login").and_then(Value::as_str).is_some_and(|login| login.eq_ignore_ascii_case(user)))
        });
        // una data illeggibile non deve nascondere l'attesa: finisce in fondo come la piu' vecchia
        let created_at = issue
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
            .map_or(0, |date| date.timestamp());
        items.push(WaitingItem { number, title: title.to_string(), waiting_on, mine, created_at, url: url.to_string(), session: session_of(body) });
    }
    items.sort_by_key(|item| (!item.mine, item.created_at));
    Ok(items)
}

pub fn waiting_file_json(items: &[WaitingItem], read_at: i64) -> String {
    let items: Vec<Value> = items
        .iter()
        .map(|item| {
            json!({
                "number": item.number, "title": item.title, "waiting_on": item.waiting_on, "mine": item.mine,
                "created_at": item.created_at, "url": item.url, "session": item.session,
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "read_at": read_at, "items": items })).unwrap_or_default()
}

pub fn parse_waiting_file(text: &str) -> Option<(Vec<WaitingItem>, i64)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let read_at = value.get("read_at")?.as_i64()?;
    let items = value
        .get("items")?
        .as_array()?
        .iter()
        .map(|item| {
            Some(WaitingItem {
                number: item.get("number")?.as_u64()?,
                title: item.get("title")?.as_str()?.to_string(),
                waiting_on: item.get("waiting_on").and_then(Value::as_str).map(str::to_string),
                mine: item.get("mine")?.as_bool()?,
                created_at: item.get("created_at")?.as_i64()?,
                url: item.get("url")?.as_str()?.to_string(),
                session: item.get("session").and_then(Value::as_str).map(str::to_string),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((items, read_at))
}

/// `gh` senza repository (`api user`): la config serve solo a leggere gli errori.
fn config_for_repo(repo: &str) -> PmConfig {
    PmConfig { owner: repo.split('/').next().unwrap_or_default().to_string(), project_number: 0, repo: repo.to_string(), stale_days: 0 }
}

pub async fn gh_user(runner: &Arc<dyn Runner>) -> Result<String, String> {
    let args = ["api", "user", "--jq", ".login"].iter().map(|part| part.to_string()).collect();
    let out = gh(runner, args, &config_for_repo("")).await?;
    let login = out.trim();
    if login.is_empty() { Err("gh api user: empty login".into()) } else { Ok(login.to_string()) }
}

pub async fn read_waiting(runner: &Arc<dyn Runner>, repo: &str, user: Option<&str>) -> Result<Vec<WaitingItem>, String> {
    let json = gh(runner, waiting_list_args(repo), &config_for_repo(repo)).await?;
    parse_waiting(&json, user)
}

/// Apre l'attesa, salvo che questa sessione ne abbia gia' una aperta con lo stesso titolo.
pub async fn open_wait(runner: &Arc<dyn Runner>, cfg: &PmConfig, session: &str, request: &WaitRequest) -> Result<FoundIssue, String> {
    if !safe_session_id(session) {
        return Err(format!("invalid session id “{}”", session.chars().take(64).collect::<String>()));
    }
    let open = read_waiting(runner, &cfg.repo, None).await?;
    if let Some(existing) = open.iter().find(|item| item.session.as_deref() == Some(session) && item.title.trim() == request.title.trim()) {
        return Ok(FoundIssue { number: existing.number, url: existing.url.clone() });
    }
    ensure_label(runner, cfg).await?;
    let args = issue_create_args(cfg, &request.title, &wait_body(request, session), request.on == WaitOn::Me);
    let out = gh(runner, args, cfg).await?;
    parse_created_issue(&out, &cfg.repo).map_err(|(error, _)| error)
}

pub async fn close_waiting(runner: &Arc<dyn Runner>, cfg: &PmConfig, number: u64) -> Result<(), String> {
    gh(runner, close_args(cfg, number, None), cfg).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use superzed_sources::test_support::{FakeRunner, fail, ok};

    fn runner(r: &Arc<FakeRunner>) -> Arc<dyn Runner> {
        r.clone()
    }

    fn cfg() -> PmConfig {
        PmConfig { owner: "acme".into(), project_number: 0, repo: "acme/acme_app".into(), stale_days: 0 }
    }

    fn lines(r: &FakeRunner) -> Vec<String> {
        r.calls().iter().map(|call| call.line()).collect()
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        futures::executor::block_on(future)
    }

    const LIST: &str = r#"[
 {"number":171,"title":"SECSTORE on CAR","body":"Waiting on: Basis\n\nWebGUI returns 500","createdAt":"2026-11-09T10:00:00Z","url":"https://github.com/acme/acme_app/issues/171","assignees":[]},
 {"number":212,"title":"Vault password","body":"Waiting on: Alice (owner)\n\nx\n\n<!-- superzed:session=s-1:wait -->","createdAt":"2026-11-23T10:00:00Z","url":"https://github.com/acme/acme_app/issues/212","assignees":[{"login":"alice"}]},
 {"number":208,"title":"Go-ahead to merge","body":"no first line\n<!-- superzed:session=s-2:item=3 -->","createdAt":"2026-11-21T10:00:00Z","url":"https://github.com/acme/acme_app/issues/208","assignees":[{"login":"alice"},{"login":"bob"}]}]"#;

    #[test]
    fn waiting_items_put_mine_first_then_oldest() {
        let items = parse_waiting(LIST, Some("alice")).unwrap();
        assert_eq!(items.iter().map(|i| (i.number, i.mine)).collect::<Vec<_>>(), [(208, true), (212, true), (171, false)]);
        assert_eq!(items[1].waiting_on.as_deref(), Some("Alice (owner)"));
        assert_eq!((items[0].waiting_on.clone(), items[0].session.as_deref()), (None, Some("s-2")));
        assert_eq!(items[1].session.as_deref(), Some("s-1"));
        assert!(parse_waiting(LIST, None).unwrap().iter().all(|i| !i.mine));
        assert_eq!(parse_waiting("oops", None).unwrap_err(), "gh issue list: non-JSON output");
    }

    #[test]
    fn wait_requests() {
        let ok = parse_wait_request(&json!({"title":" Vault password ","on":"me","detail":"staging vault"})).unwrap();
        assert_eq!(ok, WaitRequest { title: "Vault password".into(), on: WaitOn::Me, detail: Some("staging vault".into()) });
        assert_eq!(parse_wait_request(&json!({"title":"x","on":"Basis team"})).unwrap().on, WaitOn::Other("Basis team".into()));
        assert_eq!(parse_wait_request(&json!({"on":"me"})).unwrap_err(), "superzed-wait: a title is needed");
        assert_eq!(parse_wait_request(&json!({"title":"x"})).unwrap_err(), "superzed-wait: say who is awaited (--on me, or a name)");
        assert!(parse_wait_request(&json!({"title":"t".repeat(257),"on":"me"})).unwrap_err().contains("256"));
        let body = wait_body(&ok, "s-1");
        assert!(body.starts_with("Waiting on: owner\n\nstaging vault") && body.ends_with("<!-- superzed:session=s-1:wait -->"), "{body}");
    }

    #[test]
    fn the_local_copy_round_trips() {
        let items = parse_waiting(LIST, Some("alice")).unwrap();
        assert_eq!(parse_waiting_file(&waiting_file_json(&items, 1_795_000_000)), Some((items, 1_795_000_000)));
        assert_eq!(parse_waiting_file("{"), None);
    }

    #[test]
    fn open_wait_creates_a_labelled_issue_once() {
        let r = FakeRunner::new();
        r.on("gh issue list", ok("[]"));
        r.on("gh label create", ok(""));
        r.on("gh issue create", ok("https://github.com/acme/acme_app/issues/212\n"));
        let request = WaitRequest { title: "Vault password".into(), on: WaitOn::Me, detail: None };
        let issue = block_on(open_wait(&runner(&r), &cfg(), "s-1", &request)).unwrap();
        assert_eq!(issue.number, 212);
        assert!(lines(&r).iter().any(|l| l.contains("--label blocker --assignee @me")));
        // seconda chiamata con lo stesso titolo dalla stessa sessione: la issue aperta vince
        r.on("gh issue list", ok(r#"[{"number":212,"title":"Vault password","body":"<!-- superzed:session=s-1:wait -->","createdAt":"2026-11-23T10:00:00Z","url":"https://github.com/acme/acme_app/issues/212","assignees":[]}]"#));
        assert_eq!(block_on(open_wait(&runner(&r), &cfg(), "s-1", &request)).unwrap().number, 212);
        assert_eq!(r.count("gh issue create"), 1);
    }

    #[test]
    fn open_wait_reports_gh_errors_and_bad_sessions() {
        let r = FakeRunner::new();
        r.on("gh issue list", fail(1, "HTTP 401: Bad credentials"));
        let request = WaitRequest { title: "x".into(), on: WaitOn::Other("Basis".into()), detail: None };
        assert!(block_on(open_wait(&runner(&r), &cfg(), "s-1", &request)).is_err());
        assert_eq!(r.count("gh issue create"), 0);
        assert!(block_on(open_wait(&runner(&r), &cfg(), "bad id", &request)).unwrap_err().starts_with("invalid session id"));
    }
}
