//! Scrittura sulla board con `gh`, solo argv: `auth status`, poi (se servono) id del
//! progetto e campi, ricerca delle issue gia' create da questa sessione (marker nel
//! corpo), poi per ogni azione `issue create|edit|comment|close` e `project item-add|item-edit`.
use crate::preview::{Action, IssueDraft};
use crate::report::safe_session_id;
use crate::result::{FoundIssue, ItemReport};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use superzed_sources::github::{GH_MISSING, GH_SCOPE, PmConfig, check_scopes, classify_gh_error};
use superzed_sources::{RunError, Runner};

pub const FIELD_KIND: &str = "Kind";
pub const FIELD_STATUS: &str = "Status";
pub const FIELD_AREA: &str = "Area";
pub const FIELD_WAITING: &str = "Waiting on";
const LIST_LIMIT: &str = "100";
/// Issue di una sessione: fino a 3 × `MAX_ENTRIES` voci nuove (missing, problems, blockers).
const MARKER_LIMIT: &str = "1000";

#[derive(Clone, Debug, PartialEq)]
pub struct FieldOption {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoardField {
    pub id: String,
    pub name: String,
    /// Vuoto per i campi di testo.
    pub options: Vec<FieldOption>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Board {
    pub project_id: String,
    pub fields: Vec<BoardField>,
}

pub fn marker(session: &str, item: usize) -> String {
    format!("<!-- superzed:session={session}:item={item} -->")
}

pub fn marker_query(session: &str) -> String {
    format!("superzed:session={session}")
}

pub fn issue_body(body: &str, session: &str, item: usize) -> String {
    format!("{}\n\n{}", body.trim_end(), marker(session, item))
}

pub fn issue_url(repo: &str, number: u64) -> String {
    format!("https://github.com/{repo}/issues/{number}")
}

fn s(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

pub fn auth_status_args() -> Vec<String> {
    s(&["auth", "status"])
}

pub fn project_view_args(cfg: &PmConfig) -> Vec<String> {
    s(&["project", "view", &cfg.project_number.to_string(), "--owner", &cfg.owner, "--format", "json"])
}

pub fn field_list_args(cfg: &PmConfig) -> Vec<String> {
    s(&["project", "field-list", &cfg.project_number.to_string(), "--owner", &cfg.owner, "--format", "json", "--limit", LIST_LIMIT])
}

pub fn marker_search_args(cfg: &PmConfig, session: &str) -> Vec<String> {
    let search = format!("--search=\"{}\" in:body", marker_query(session));
    s(&["issue", "list", "--repo", &cfg.repo, "--state", "all", &search, "--json", "number,url,body", "--limit", MARKER_LIMIT])
}

pub fn issue_create_args(cfg: &PmConfig, title: &str, body: &str) -> Vec<String> {
    s(&["issue", "create", "--repo", &cfg.repo, &format!("--title={title}"), &format!("--body={body}")])
}

pub fn issue_edit_args(cfg: &PmConfig, number: u64, title: &str, body: &str) -> Vec<String> {
    s(&["issue", "edit", &number.to_string(), "--repo", &cfg.repo, &format!("--title={title}"), &format!("--body={body}")])
}

/// Aggiornamento di una issue per link (una issue creata fuori dal repo della config).
pub fn issue_edit_url_args(url: &str, title: &str, body: &str) -> Vec<String> {
    s(&["issue", "edit", url, &format!("--title={title}"), &format!("--body={body}")])
}

pub fn item_add_args(cfg: &PmConfig, url: &str) -> Vec<String> {
    s(&["project", "item-add", &cfg.project_number.to_string(), "--owner", &cfg.owner, "--url", url, "--format", "json"])
}

/// Opzione di un campo a scelta (per nome, senza badare a maiuscole) o testo libero.
pub fn item_edit_args(board: &Board, item_id: &str, field_name: &str, value: &str) -> Result<Vec<String>, String> {
    let field = find_field(&board.fields, field_name).ok_or_else(|| format!("the board has no “{field_name}” field"))?;
    let mut args = s(&["project", "item-edit", "--id", item_id, "--project-id", &board.project_id, "--field-id", &field.id]);
    if field.options.is_empty() {
        args.push(format!("--text={value}"));
    } else {
        let option = field
            .options
            .iter()
            .find(|o| o.name.trim().eq_ignore_ascii_case(value.trim()))
            .ok_or_else(|| format!("the board field “{}” has no “{value}” option", field.name))?;
        args.push("--single-select-option-id".into());
        args.push(option.id.clone());
    }
    Ok(args)
}

pub fn comment_args(cfg: &PmConfig, number: u64, body: &str) -> Vec<String> {
    s(&["issue", "comment", &number.to_string(), "--repo", &cfg.repo, &format!("--body={body}")])
}

pub fn close_args(cfg: &PmConfig, number: u64, comment: Option<&str>) -> Vec<String> {
    let mut args = s(&["issue", "close", &number.to_string(), "--repo", &cfg.repo]);
    if let Some(c) = comment {
        args.push(format!("--comment={c}"));
    }
    args
}

/// Il link e' una issue di `repo` (owner e nome senza badare alle maiuscole, come GitHub).
pub fn in_repo(issue: &FoundIssue, repo: &str) -> bool {
    issue.url.eq_ignore_ascii_case(&issue_url(repo, issue.number))
}

/// Link della issue creata: l'ultima riga `https://…/issues/<n>` dello stdout, solo se e'
/// di `repo` (un link di un altro repository non si aggiunge mai alla board). Errore con
/// la issue quando e' stata creata fuori da `repo`: la riprova la aggiorna, non ne crea un'altra.
pub fn parse_created_issue(stdout: &str, repo: &str) -> Result<FoundIssue, (String, Option<FoundIssue>)> {
    let url = stdout
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("https://"))
        .ok_or_else(|| ("gh issue create: issue link missing".to_string(), None))?;
    let number = url
        .rsplit_once("/issues/")
        .and_then(|(_, n)| n.parse::<u64>().ok())
        .ok_or_else(|| (format!("gh issue create: unexpected link “{url}”"), None))?;
    let issue = FoundIssue { number, url: url.to_string() };
    if !in_repo(&issue, repo) {
        return Err((foreign_error(&issue, repo), Some(issue)));
    }
    Ok(issue)
}

fn foreign_error(issue: &FoundIssue, repo: &str) -> String {
    format!("gh issue create: issue #{} created outside “{repo}”: “{}”", issue.number, issue.url)
}

fn json_id(json: &str, what: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| format!("{what}: non-JSON output"))?;
    v.get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{what}: id missing"))
}

pub fn parse_project_id(json: &str) -> Result<String, String> {
    json_id(json, "gh project view")
}

pub fn parse_item_id(json: &str) -> Result<String, String> {
    json_id(json, "gh project item-add")
}

pub fn parse_fields(json: &str) -> Result<Vec<BoardField>, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "gh project field-list: non-JSON output".to_string())?;
    let fields = v.get("fields").and_then(Value::as_array).ok_or_else(|| "gh project field-list: “fields” missing".to_string())?;
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    Ok(fields
        .iter()
        .map(|f| BoardField {
            id: text(f, "id"),
            name: text(f, "name"),
            options: f
                .get("options")
                .and_then(Value::as_array)
                .map(|opts| opts.iter().map(|o| FieldOption { id: text(o, "id"), name: text(o, "name") }).collect())
                .unwrap_or_default(),
        })
        .collect())
}

fn norm(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, ' ' | '_')).collect::<String>().to_lowercase()
}

pub fn find_field<'a>(fields: &'a [BoardField], name: &str) -> Option<&'a BoardField> {
    fields.iter().find(|f| norm(&f.name) == norm(name))
}

/// Voci gia' create da `session`: indice del marker → issue (la prima trovata vince).
pub fn find_markers(json: &str, session: &str) -> Result<BTreeMap<usize, FoundIssue>, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "gh issue list: non-JSON output".to_string())?;
    let prefix = format!("<!-- superzed:session={session}:item=");
    let mut found = BTreeMap::new();
    for issue in v.as_array().into_iter().flatten() {
        let (Some(number), Some(url)) = (issue.get("number").and_then(Value::as_u64), issue.get("url").and_then(Value::as_str)) else {
            continue;
        };
        let body = issue.get("body").and_then(Value::as_str).unwrap_or_default();
        for (at, _) in body.match_indices(&prefix) {
            let rest = &body[at + prefix.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty()
                && rest[digits.len()..].starts_with(" -->")
                && let Ok(item) = digits.parse::<usize>()
            {
                found.entry(item).or_insert_with(|| FoundIssue { number, url: url.to_string() });
            }
        }
    }
    Ok(found)
}

/// Per scrivere serve `project`: `read:project` basta solo a leggere la board.
fn check_write_scope(auth_output: &str) -> Result<(), String> {
    let Some(line) = auth_output.lines().find(|l| l.contains("Token scopes:")) else {
        return Ok(());
    };
    let scopes = line.split_once("Token scopes:").map(|(_, s)| s).unwrap_or("");
    let has = scopes.split(',').map(|s| s.trim().trim_matches(['\'', '"'])).any(|s| s == "project");
    if has { Ok(()) } else { Err(GH_SCOPE.into()) }
}

/// `gh …`: stdout se riesce, altrimenti un errore leggibile.
async fn gh(runner: &Arc<dyn Runner>, args: Vec<String>, cfg: &PmConfig) -> Result<String, String> {
    match runner.run("gh", &args, None).await {
        Err(RunError::NotFound(_)) => Err(GH_MISSING.into()),
        Err(RunError::Failed(e)) => Err(format!("gh: {e}")),
        Ok(out) if out.success => Ok(out.stdout),
        Ok(out) => Err(classify_gh_error(&format!("{}\n{}", out.stderr, out.stdout), cfg)),
    }
}

async fn load_board(runner: &Arc<dyn Runner>, cfg: &PmConfig) -> Result<Board, String> {
    let project_id = parse_project_id(&gh(runner, project_view_args(cfg), cfg).await?)?;
    let fields = parse_fields(&gh(runner, field_list_args(cfg), cfg).await?)?;
    Ok(Board { project_id, fields })
}

/// Valori dei campi della board per una issue nuova (Kind, Status, Area, Waiting on).
fn draft_fields(d: &IssueDraft) -> Vec<(&'static str, String)> {
    let mut values = vec![(FIELD_KIND, d.kind.name().to_string()), (FIELD_STATUS, d.status.name().to_string())];
    if let Some(area) = &d.area {
        values.push((FIELD_AREA, area.clone()));
    }
    if let Some(w) = &d.waiting_on {
        values.push((FIELD_WAITING, w.clone()));
    }
    values
}

async fn upsert(
    runner: &Arc<dyn Runner>,
    cfg: &PmConfig,
    session: &str,
    board: &Board,
    known: &BTreeMap<usize, FoundIssue>,
    d: &IssueDraft,
) -> ItemReport {
    let report = |result: Result<String, String>, issue: Option<FoundIssue>| ItemReport { key: d.key.clone(), result, issue };
    // campi e opzioni controllati prima di creare: niente issue a meta' per una board diversa
    let fields = draft_fields(d);
    if let Some(e) = fields.iter().find_map(|(f, v)| item_edit_args(board, "-", f, v).err()) {
        return report(Err(e), None);
    }
    let body = issue_body(&d.body, session, d.item);
    let issue = match known.get(&d.item) {
        // creata fuori dal repo della config: si aggiorna per link, mai sulla board
        Some(found) if !in_repo(found, &cfg.repo) => {
            let result = gh(runner, issue_edit_url_args(&found.url, &d.title, &body), cfg)
                .await
                .and_then(|_| Err(format!("{} (updated, not added to the board)", foreign_error(found, &cfg.repo))));
            return report(result, Some(found.clone()));
        }
        Some(found) => match gh(runner, issue_edit_args(cfg, found.number, &d.title, &body), cfg).await {
            Ok(_) => found.clone(),
            Err(e) => return report(Err(e), Some(found.clone())),
        },
        None => match gh(runner, issue_create_args(cfg, &d.title, &body), cfg).await {
            Err(e) => return report(Err(e), None),
            Ok(out) => match parse_created_issue(&out, &cfg.repo) {
                Ok(created) => created,
                Err((e, created)) => return report(Err(e), created),
            },
        },
    };
    let result = async {
        let item_id = parse_item_id(&gh(runner, item_add_args(cfg, &issue.url), cfg).await?)?;
        for (field, value) in &fields {
            gh(runner, item_edit_args(board, &item_id, field, value)?, cfg).await?;
        }
        Ok::<_, String>(issue.url.clone())
    }
    .await;
    report(result, Some(issue))
}

async fn set_status(
    runner: &Arc<dyn Runner>,
    cfg: &PmConfig,
    board: &Board,
    number: u64,
    status: crate::preview::Status,
    comment: Option<&str>,
) -> Result<String, String> {
    // item-add restituisce l'elemento gia' sulla board, se c'e'
    let item_id = parse_item_id(&gh(runner, item_add_args(cfg, &issue_url(&cfg.repo, number)), cfg).await?)?;
    gh(runner, item_edit_args(board, &item_id, FIELD_STATUS, status.name())?, cfg).await?;
    if let Some(c) = comment {
        gh(runner, comment_args(cfg, number, c), cfg).await?;
    }
    Ok(format!("#{number} → {}", status.name()))
}

/// Esegue le azioni in ordine; ogni voce ha il suo esito, consegnato a `on_item` appena
/// finita (chi chiama lo salva subito). `known`: issue gia' create per indice di voce nuova
/// (dall'esito salvato), prima dei marker trovati su GitHub.
pub async fn execute(
    runner: Arc<dyn Runner>,
    cfg: PmConfig,
    session_id: String,
    actions: Vec<Action>,
    known: BTreeMap<usize, FoundIssue>,
    mut on_item: impl FnMut(&ItemReport) + Send,
) -> Vec<ItemReport> {
    let mut fail_all = |e: String| {
        let out = actions.iter().map(|a| ItemReport { key: a.key().to_string(), result: Err(e.clone()), issue: None }).collect::<Vec<_>>();
        out.iter().for_each(&mut on_item);
        out
    };
    if actions.is_empty() {
        return Vec::new();
    }
    // l'id finisce nel marker e nella ricerca: solo `[A-Za-z0-9-]{1,64}`
    if !safe_session_id(&session_id) {
        return fail_all(format!("invalid session id “{}”", session_id.chars().take(64).collect::<String>()));
    }
    // gh presente, autenticato e con lo scope project (su stdout o stderr secondo la versione)
    match runner.run("gh", &auth_status_args(), None).await {
        Err(RunError::NotFound(_)) => return fail_all(GH_MISSING.into()),
        Err(RunError::Failed(e)) => return fail_all(format!("gh: {e}")),
        Ok(out) if !out.success => return fail_all(classify_gh_error(&format!("{}\n{}", out.stderr, out.stdout), &cfg)),
        Ok(out) => {
            let auth = format!("{}\n{}", out.stdout, out.stderr);
            if let Err(e) = check_scopes(&auth).and_then(|()| check_write_scope(&auth)) {
                return fail_all(e);
            }
        }
    }
    let needs_board = actions.iter().any(|a| matches!(a, Action::Upsert(_) | Action::SetStatus { .. }));
    let board = if needs_board { load_board(&runner, &cfg).await } else { Err("board not read".to_string()) };
    let has_new = actions.iter().any(|a| matches!(a, Action::Upsert(_)));
    let existing = if has_new {
        gh(&runner, marker_search_args(&cfg, &session_id), &cfg)
            .await
            .and_then(|json| find_markers(&json, &session_id))
            .map(|mut found| {
                found.extend(known);
                found
            })
            .map_err(|e| format!("search for issues already created failed: {e}"))
    } else {
        Ok(BTreeMap::new())
    };
    let mut out = Vec::new();
    for action in &actions {
        let key = action.key().to_string();
        let report = match action {
            Action::Upsert(d) => match (&board, &existing) {
                (Err(e), _) | (_, Err(e)) => ItemReport { key, result: Err(e.clone()), issue: None },
                (Ok(b), Ok(found)) => upsert(&runner, &cfg, &session_id, b, found, d).await,
            },
            Action::SetStatus { number, status, comment, .. } => {
                let result = match &board {
                    Err(e) => Err(e.clone()),
                    Ok(b) => set_status(&runner, &cfg, b, *number, *status, comment.as_deref()).await,
                };
                ItemReport { key, result, issue: None }
            }
            Action::Comment { number, body, .. } => {
                let result = gh(&runner, comment_args(&cfg, *number, body), &cfg).await.map(|_| format!("comment on #{number}"));
                ItemReport { key, result, issue: None }
            }
            Action::Close { number, comment, .. } => {
                let result = gh(&runner, close_args(&cfg, *number, comment.as_deref()), &cfg).await.map(|_| format!("#{number} closed"));
                ItemReport { key, result, issue: None }
            }
        };
        on_item(&report);
        out.push(report);
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::{IssueDraft, Kind, Status};
    use crate::test_support::{CREATED_URL, FIELDS, ITEM_ADDED, PROJECT_VIEW, SID, marker_hits, write_runner};
    use superzed_sources::test_support::{AUTH_NO_SCOPE, FakeRunner, fail, ok};

    fn cfg() -> PmConfig {
        PmConfig { owner: "acme".into(), project_number: 3, repo: "acme/acme_app".into(), stale_days: 14 }
    }

    fn board() -> Board {
        Board { project_id: "PVT_kwDOA".into(), fields: parse_fields(FIELDS).unwrap() }
    }

    fn draft(item: usize, kind: Kind, title: &str) -> Action {
        Action::Upsert(IssueDraft {
            key: format!("new:{item}"),
            item,
            title: title.into(),
            body: format!("corpo di {title}"),
            kind,
            status: kind.status(),
            area: Some("frontend".into()),
            waiting_on: (kind == Kind::Blocker).then(|| "design".to_string()),
        })
    }

    fn run(r: &Arc<FakeRunner>, actions: Vec<Action>, known: BTreeMap<usize, FoundIssue>) -> Vec<ItemReport> {
        futures::executor::block_on(execute(r.clone(), cfg(), SID.into(), actions, known, |_| {}))
    }

    fn lines(r: &FakeRunner) -> Vec<String> {
        r.calls().iter().map(|c| c.line()).collect()
    }

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn argv_are_exact_and_user_text_is_one_argument() {
        assert_eq!(strs(&project_view_args(&cfg())), ["project", "view", "3", "--owner", "acme", "--format", "json"]);
        assert_eq!(strs(&field_list_args(&cfg())), ["project", "field-list", "3", "--owner", "acme", "--format", "json", "--limit", "100"]);
        assert_eq!(
            strs(&marker_search_args(&cfg(), SID)),
            ["issue", "list", "--repo", "acme/acme_app", "--state", "all", &format!("--search=\"superzed:session={SID}\" in:body"), "--json", "number,url,body", "--limit", "1000"]
        );
        let hostile = issue_create_args(&cfg(), "--fai-danni", "$(rm -rf ~)\n\"x\" 'y'");
        assert_eq!(strs(&hostile), ["issue", "create", "--repo", "acme/acme_app", "--title=--fai-danni", "--body=$(rm -rf ~)\n\"x\" 'y'"]);
        assert_eq!(strs(&issue_edit_args(&cfg(), 40, "t", "b")), ["issue", "edit", "40", "--repo", "acme/acme_app", "--title=t", "--body=b"]);
        assert_eq!(strs(&item_add_args(&cfg(), CREATED_URL)), ["project", "item-add", "3", "--owner", "acme", "--url", CREATED_URL, "--format", "json"]);
        assert_eq!(strs(&comment_args(&cfg(), 12, "-n")), ["issue", "comment", "12", "--repo", "acme/acme_app", "--body=-n"]);
        assert_eq!(strs(&close_args(&cfg(), 12, None)), ["issue", "close", "12", "--repo", "acme/acme_app"]);
        assert_eq!(strs(&close_args(&cfg(), 12, Some("fatto"))), ["issue", "close", "12", "--repo", "acme/acme_app", "--comment=fatto"]);
        assert_eq!(issue_body("corpo\n\n", SID, 2), format!("corpo\n\n<!-- superzed:session={SID}:item=2 -->"));
        assert_eq!(issue_url("acme/acme_app", 8), "https://github.com/acme/acme_app/issues/8");
    }

    #[test]
    fn item_edit_uses_option_ids_or_text() {
        let b = board();
        assert_eq!(
            strs(&item_edit_args(&b, "PVTI_1", FIELD_STATUS, "in progress").unwrap()),
            ["project", "item-edit", "--id", "PVTI_1", "--project-id", "PVT_kwDOA", "--field-id", "PVTSSF_status", "--single-select-option-id", "st_progress"]
        );
        assert_eq!(
            strs(&item_edit_args(&b, "PVTI_1", "waiting_on", "-design").unwrap()),
            ["project", "item-edit", "--id", "PVTI_1", "--project-id", "PVT_kwDOA", "--field-id", "PVTF_waiting", "--text=-design"]
        );
        assert_eq!(item_edit_args(&b, "PVTI_1", FIELD_KIND, "Epic").unwrap_err(), "the board field “Kind” has no “Epic” option");
        assert_eq!(item_edit_args(&b, "PVTI_1", "Boh", "x").unwrap_err(), "the board has no “Boh” field");
    }

    #[test]
    fn parsers() {
        let repo = "acme/acme_app";
        assert_eq!(parse_created_issue(&format!("Creating issue\n{CREATED_URL}\n"), repo).unwrap(), FoundIssue { number: 42, url: CREATED_URL.into() });
        assert!(parse_created_issue("boh", repo).is_err());
        assert!(parse_created_issue("https://github.com/acme/acme_app/pull/3", repo).is_err());
        assert!(parse_created_issue("https://github.com/acme/altro/issues/3", repo).is_err());
        // revisione finale 4: owner/repo senza badare alle maiuscole
        let upper = "https://github.com/Acme/Acme_APP/issues/3";
        assert_eq!(parse_created_issue(upper, repo).unwrap(), FoundIssue { number: 3, url: upper.into() });
        let foreign = parse_created_issue("https://github.com/acme/altro/issues/3", repo).unwrap_err();
        assert_eq!(foreign.1, Some(FoundIssue { number: 3, url: "https://github.com/acme/altro/issues/3".into() }), "creata: si ricorda");
        assert_eq!(parse_created_issue("boh", repo).unwrap_err().1, None);
        assert!(parse_created_issue("https://github.com/acme/acme_app/issues/3/x", repo).is_err());
        assert_eq!(parse_project_id(PROJECT_VIEW).unwrap(), "PVT_kwDOA");
        assert_eq!(parse_item_id(ITEM_ADDED).unwrap(), "PVTI_new");
        assert!(parse_item_id("{}").is_err());
        let fields = parse_fields(FIELDS).unwrap();
        assert_eq!(fields.len(), 5);
        assert_eq!(find_field(&fields, "kind").unwrap().options.len(), 4);
        assert!(parse_fields("[]").is_err());
    }

    #[test]
    fn markers_are_found_only_for_this_session() {
        let json = serde_json::json!([
            {"number": 40, "url": "u40", "body": format!("a\n{}", marker(SID, 0))},
            {"number": 41, "url": "u41", "body": marker("altra-sessione", 1)},
            {"number": 44, "url": "u44", "body": format!("{} e poi {}", marker(SID, 2), "<!-- superzed:session=x:item=9 -->")},
            {"number": 45, "url": "u45", "body": format!("<!-- superzed:session={SID}:item=x -->")},
            {"number": 46, "url": "u46", "body": null}
        ])
        .to_string();
        let found = find_markers(&json, SID).unwrap();
        assert_eq!(found, BTreeMap::from([(0, FoundIssue { number: 40, url: "u40".into() }), (2, FoundIssue { number: 44, url: "u44".into() })]));
        assert!(find_markers("non json", SID).is_err());
    }

    #[test]
    fn a_new_issue_is_created_added_and_edited_in_order() {
        let r = write_runner();
        let out = run(&r, vec![draft(2, Kind::Blocker, "Policy")], BTreeMap::new());
        assert_eq!(out, [ItemReport { key: "new:2".into(), result: Ok(CREATED_URL.into()), issue: Some(FoundIssue { number: 42, url: CREATED_URL.into() }) }]);
        let l = lines(&r);
        let heads: Vec<String> = l.iter().map(|l| l.split(' ').take(3).collect::<Vec<_>>().join(" ")).collect();
        assert_eq!(
            heads,
            [
                "gh auth status", "gh project view", "gh project field-list", "gh issue list", "gh issue create",
                "gh project item-add", "gh project item-edit", "gh project item-edit", "gh project item-edit", "gh project item-edit",
            ]
        );
        assert!(l[4].contains(&format!("--body=corpo di Policy\n\n{}", marker(SID, 2))), "{}", l[4]);
        assert!(l[6].ends_with("--single-select-option-id k_blocker"));
        assert!(l[7].ends_with("--single-select-option-id st_blocked"));
        assert!(l[8].ends_with("--single-select-option-id a_frontend"));
        assert!(l[9].ends_with("--field-id PVTF_waiting --text=design"));
    }

    #[test]
    fn an_existing_marker_is_edited_not_recreated() {
        let r = write_runner();
        r.on("gh issue list --repo acme/acme_app --state all --search=", ok(&marker_hits(SID, &[(0, 40)])));
        let out = run(&r, vec![draft(0, Kind::Task, "Test"), draft(1, Kind::Question, "Formato")], BTreeMap::new());
        assert_eq!(out[0].issue.as_ref().map(|i| i.number), Some(40));
        assert_eq!(r.count("gh issue edit 40 --repo acme/acme_app --title=Test"), 1);
        assert_eq!(r.count("gh issue create"), 1, "solo la voce 1, che non c'era");
        assert_eq!(r.count("gh issue list"), 1, "una ricerca per esecuzione");
    }

    #[test]
    fn known_issues_win_over_the_search() {
        let r = write_runner();
        let known = BTreeMap::from([(0, FoundIssue { number: 42, url: CREATED_URL.into() })]);
        run(&r, vec![draft(0, Kind::Task, "Test")], known);
        assert_eq!((r.count("gh issue create"), r.count("gh issue edit 42")), (0, 1));
    }

    #[test]
    fn a_failed_search_creates_nothing() {
        let r = write_runner();
        r.on("gh issue list --repo acme/acme_app --state all --search=", fail(1, "HTTP 502: bad gateway"));
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = run(&r, vec![draft(0, Kind::Task, "Test"), close], BTreeMap::new());
        assert_eq!(out[0].result, Err("search for issues already created failed: gh: HTTP 502: bad gateway".into()));
        assert_eq!(r.count("gh issue create"), 0);
        assert_eq!(out[1].result, Ok("#12 closed".into()), "le altre voci vanno avanti");
    }

    #[test]
    fn a_board_without_the_option_fails_before_creating() {
        let r = write_runner();
        r.on("gh project field-list 3 --owner acme", ok(&FIELDS.replace(r#"{"id":"k_blocker","name":"Blocker"},"#, "")));
        let out = run(&r, vec![draft(2, Kind::Blocker, "Policy")], BTreeMap::new());
        assert_eq!(out[0].result, Err("the board field “Kind” has no “Blocker” option".into()));
        assert_eq!(r.count("gh issue create"), 0);
    }

    #[test]
    fn a_failure_after_creation_keeps_the_issue() {
        let r = write_runner();
        r.on("gh project item-add 3 --owner acme", fail(1, "HTTP 502: bad gateway"));
        let out = run(&r, vec![draft(0, Kind::Task, "Test")], BTreeMap::new());
        assert_eq!(out[0].result, Err("gh: HTTP 502: bad gateway".into()));
        assert_eq!(out[0].issue.as_ref().map(|i| i.number), Some(42), "la prossima volta si aggiorna questa");
    }

    #[test]
    fn touched_issues_close_comment_and_status() {
        let r = write_runner();
        let out = run(
            &r,
            vec![
                Action::Close { key: "touched:12".into(), number: 12, comment: Some("fatto".into()) },
                Action::SetStatus { key: "touched:8".into(), number: 8, status: Status::Blocked, comment: Some("nota".into()) },
                Action::Comment { key: "summary".into(), number: 12, body: "Riepilogo".into() },
            ],
            BTreeMap::new(),
        );
        assert!(out.iter().all(|o| o.result.is_ok()), "{out:?}");
        let l = lines(&r);
        assert_eq!(r.count("gh issue list"), 0, "nessuna issue nuova: nessuna ricerca");
        assert!(l.contains(&"gh issue close 12 --repo acme/acme_app --comment=fatto".to_string()));
        assert!(l.contains(&"gh project item-add 3 --owner acme --url https://github.com/acme/acme_app/issues/8 --format json".to_string()));
        assert!(l.iter().any(|x| x.ends_with("--field-id PVTSSF_status --single-select-option-id st_blocked")));
        assert!(l.contains(&"gh issue comment 8 --repo acme/acme_app --body=nota".to_string()));
        assert!(l.contains(&"gh issue comment 12 --repo acme/acme_app --body=Riepilogo".to_string()));
    }

    #[test]
    fn gh_missing_or_without_scope_fails_everything() {
        let r = FakeRunner::new();
        r.on("gh", Err(superzed_sources::RunError::NotFound("gh".into())));
        let close = || Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = run(&r, vec![close(), draft(0, Kind::Task, "x")], BTreeMap::new());
        assert!(out.iter().all(|o| o.result == Err(GH_MISSING.to_string())));
        assert_eq!(r.calls().len(), 1);
        let r = write_runner();
        r.on("gh auth status", ok(AUTH_NO_SCOPE));
        let out = run(&r, vec![close()], BTreeMap::new());
        assert_eq!(out[0].result, Err(superzed_sources::github::GH_SCOPE.to_string()));
        assert_eq!(r.count("gh issue close"), 0);
    }

    #[test]
    fn read_only_project_scope_cannot_write() {
        let r = write_runner();
        r.on("gh auth status", ok(&superzed_sources::test_support::AUTH_OK.replace("'project'", "'read:project'")));
        let out = run(&r, vec![Action::Close { key: "touched:12".into(), number: 12, comment: None }], BTreeMap::new());
        assert_eq!(out[0].result, Err(superzed_sources::github::GH_SCOPE.to_string()));
        assert_eq!(r.calls().len(), 1);
    }

    #[test]
    fn a_foreign_issue_url_is_never_added_to_the_board() {
        let r = write_runner();
        r.on("gh issue create", ok("https://github.com/evil/other/issues/42\n"));
        let out = run(&r, vec![draft(0, Kind::Task, "Test")], BTreeMap::new());
        let foreign = FoundIssue { number: 42, url: "https://github.com/evil/other/issues/42".into() };
        assert_eq!(out[0].result, Err(format!("gh issue create: issue #42 created outside “acme/acme_app”: “{}”", foreign.url)));
        assert_eq!(out[0].issue, Some(foreign.clone()), "revisione finale 4: la issue creata si ricorda");
        assert_eq!(r.count("gh project item-add"), 0);
        // riprova: si aggiorna quella (per link), mai una seconda create, mai sulla board
        let out = run(&r, vec![draft(0, Kind::Task, "Test")], BTreeMap::from([(0, foreign.clone())]));
        assert_eq!(r.count("gh issue create"), 1);
        assert_eq!(r.count(&format!("gh issue edit {} --title=Test", foreign.url)), 1);
        assert_eq!(r.count("gh project item-add"), 0);
        assert_eq!(out[0].issue, Some(foreign));
        assert!(out[0].result.as_ref().unwrap_err().contains("not added to the board"), "{out:?}");
    }

    #[test]
    fn a_case_different_repo_is_the_same_repo() {
        let r = write_runner();
        r.on("gh issue create", ok("https://github.com/Acme/Acme_App/issues/42\n"));
        let out = run(&r, vec![draft(0, Kind::Task, "Test")], BTreeMap::new());
        assert!(out[0].result.is_ok(), "{out:?}");
        assert_eq!(r.count("gh project item-add"), 1);
    }

    // Revisione finale 1: ogni voce si consegna appena finita (l'esito si salva voce per voce).
    #[test]
    fn every_item_is_handed_over_as_soon_as_it_is_done() {
        let r = write_runner();
        let seen = std::sync::Mutex::new(Vec::new());
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = futures::executor::block_on(execute(r.clone(), cfg(), SID.into(), vec![draft(0, Kind::Task, "Test"), close], BTreeMap::new(), |item| {
            seen.lock().unwrap().push((item.key.clone(), r.calls().len()));
        }));
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["new:0", "touched:12"]);
        assert!(seen[0].1 < seen[1].1, "la prima prima della chiusura: {seen:?}");
        assert_eq!(out.len(), 2);
        let seen = std::sync::Mutex::new(0);
        let r = FakeRunner::new();
        r.on("gh", Err(superzed_sources::RunError::NotFound("gh".into())));
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        futures::executor::block_on(execute(r, cfg(), SID.into(), vec![close], BTreeMap::new(), |_| *seen.lock().unwrap() += 1));
        assert_eq!(seen.into_inner().unwrap(), 1, "anche gli errori di tutto il giro");
    }

    #[test]
    fn a_board_that_cannot_be_read_fails_new_issues_before_creating() {
        let r = write_runner();
        r.on("gh project field-list 3 --owner acme", fail(1, "HTTP 502: bad gateway"));
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = run(&r, vec![draft(0, Kind::Task, "Test"), close], BTreeMap::new());
        assert_eq!(out[0].result, Err("gh: HTTP 502: bad gateway".into()));
        assert_eq!((r.count("gh issue create"), r.count("gh issue edit")), (0, 0));
        assert_eq!(out[1].result, Ok("#12 closed".into()));
    }

    /// Con la ricerca fallita anche una voce gia' nota non si aggiorna: si riprova dopo
    /// (scelta prudente, nessuna scrittura su dati incompleti).
    #[test]
    fn a_known_issue_with_a_failed_search_is_not_written() {
        let r = write_runner();
        r.on("gh issue list --repo acme/acme_app --state all --search=", fail(1, "HTTP 502: bad gateway"));
        let known = BTreeMap::from([(0, FoundIssue { number: 42, url: CREATED_URL.into() })]);
        let out = run(&r, vec![draft(0, Kind::Task, "Test")], known);
        assert_eq!(out[0].result, Err("search for issues already created failed: gh: HTTP 502: bad gateway".into()));
        assert_eq!((r.count("gh issue create"), r.count("gh issue edit")), (0, 0));
    }

    #[test]
    fn an_unsafe_session_id_runs_nothing() {
        let r = write_runner();
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = futures::executor::block_on(execute(r.clone(), cfg(), "x\" in:title OR \"".into(), vec![close], BTreeMap::new(), |_| {}));
        assert!(out[0].result.as_ref().unwrap_err().starts_with("invalid session id"), "{out:?}");
        assert!(r.calls().is_empty());
    }

    #[test]
    fn nothing_to_do_runs_nothing() {
        let r = write_runner();
        assert!(run(&r, Vec::new(), BTreeMap::new()).is_empty());
        assert!(r.calls().is_empty());
    }
}
