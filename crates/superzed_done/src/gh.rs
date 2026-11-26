//! Scrittura su GitHub con `gh`, solo argv: `auth status`, poi (se serve) l'etichetta dei
//! blocker, ricerca delle issue gia' create da questa sessione (marker nel corpo), poi per
//! ogni azione `issue create|edit|comment|close`.
use crate::preview::{Action, IssueDraft};
use crate::report::safe_session_id;
use crate::result::{FoundIssue, ItemReport};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use superzed_sources::github::{GH_MISSING, PmConfig, classify_gh_error};
use superzed_sources::{RunError, Runner};

pub const BLOCKER_LABEL: &str = "blocker";
const BLOCKER_LABEL_DESCRIPTION: &str = "Waiting on a person";
const BLOCKER_LABEL_COLOR: &str = "D93F0B";
/// Issue di una sessione: fino a 3 × `MAX_ENTRIES` voci nuove (missing, problems, blockers).
const MARKER_LIMIT: &str = "1000";

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

pub fn marker_search_args(cfg: &PmConfig, session: &str) -> Vec<String> {
    let search = format!("--search=\"{}\" in:body", marker_query(session));
    s(&["issue", "list", "--repo", &cfg.repo, "--state", "all", &search, "--json", "number,url,body", "--limit", MARKER_LIMIT])
}

pub fn label_create_args(cfg: &PmConfig) -> Vec<String> {
    s(&["label", "create", BLOCKER_LABEL, "-R", &cfg.repo, "--description", BLOCKER_LABEL_DESCRIPTION, "--color", BLOCKER_LABEL_COLOR])
}

pub fn issue_create_args(cfg: &PmConfig, title: &str, body: &str, assign_me: bool) -> Vec<String> {
    let mut args = s(&["issue", "create", "-R", &cfg.repo, &format!("--title={title}"), &format!("--body={body}"), "--label", BLOCKER_LABEL]);
    if assign_me {
        args.extend(s(&["--assignee", "@me"]));
    }
    args
}

/// Aggiunge anche l'etichetta: un blocker creato prima che esistesse non comparirebbe fra le attese.
pub fn issue_edit_args(cfg: &PmConfig, number: u64, title: &str, body: &str) -> Vec<String> {
    s(&["issue", "edit", &number.to_string(), "--repo", &cfg.repo, &format!("--title={title}"), &format!("--body={body}"), "--add-label", BLOCKER_LABEL])
}

/// Aggiornamento di una issue per link (una issue creata fuori dal repo della config).
pub fn issue_edit_url_args(url: &str, title: &str, body: &str) -> Vec<String> {
    s(&["issue", "edit", url, &format!("--title={title}"), &format!("--body={body}")])
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
/// di `repo` (un link di un altro repository non conta come creato qui). Errore con
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

/// `gh …`: stdout se riesce, altrimenti un errore leggibile.
pub(crate) async fn gh(runner: &Arc<dyn Runner>, args: Vec<String>, cfg: &PmConfig) -> Result<String, String> {
    match runner.run("gh", &args, None).await {
        Err(RunError::NotFound(_)) => Err(GH_MISSING.into()),
        Err(RunError::Failed(e)) => Err(format!("gh: {e}")),
        Ok(out) if out.success => Ok(out.stdout),
        Ok(out) => Err(classify_gh_error(&format!("{}\n{}", out.stderr, out.stdout), cfg)),
    }
}

/// Crea l'etichetta dei blocker; se esiste gia' va bene.
pub async fn ensure_label(runner: &Arc<dyn Runner>, cfg: &PmConfig) -> Result<(), String> {
    create_label(runner, cfg).await.map_err(|e| format!("label “{BLOCKER_LABEL}”: {e}"))
}

async fn create_label(runner: &Arc<dyn Runner>, cfg: &PmConfig) -> Result<(), String> {
    match runner.run("gh", &label_create_args(cfg), None).await {
        Err(RunError::NotFound(_)) => Err(GH_MISSING.into()),
        Err(RunError::Failed(e)) => Err(format!("gh: {e}")),
        Ok(out) if out.success => Ok(()),
        Ok(out) if out.stderr.contains("already exists") => Ok(()),
        Ok(out) => Err(classify_gh_error(&format!("{}\n{}", out.stderr, out.stdout), cfg)),
    }
}

async fn upsert(runner: &Arc<dyn Runner>, cfg: &PmConfig, session: &str, known: &BTreeMap<usize, FoundIssue>, d: &IssueDraft) -> ItemReport {
    let report = |result: Result<String, String>, issue: Option<FoundIssue>| ItemReport { key: d.key.clone(), result, issue };
    let body = issue_body(&d.body, session, d.item);
    match known.get(&d.item) {
        // creata fuori dal repo della config: si aggiorna per link
        Some(found) if !in_repo(found, &cfg.repo) => {
            let result = gh(runner, issue_edit_url_args(&found.url, &d.title, &body), cfg)
                .await
                .and_then(|_| Err(format!("{} (updated in place)", foreign_error(found, &cfg.repo))));
            report(result, Some(found.clone()))
        }
        Some(found) => match gh(runner, issue_edit_args(cfg, found.number, &d.title, &body), cfg).await {
            Ok(_) => report(Ok(found.url.clone()), Some(found.clone())),
            Err(e) => report(Err(e), Some(found.clone())),
        },
        None => match gh(runner, issue_create_args(cfg, &d.title, &body, d.assign_me), cfg).await {
            Err(e) => report(Err(e), None),
            Ok(out) => match parse_created_issue(&out, &cfg.repo) {
                Ok(created) => report(Ok(created.url.clone()), Some(created)),
                Err((e, created)) => report(Err(e), created),
            },
        },
    }
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
    // gh presente e autenticato: lo scope `repo` basta, nessun Project
    match runner.run("gh", &auth_status_args(), None).await {
        Err(RunError::NotFound(_)) => return fail_all(GH_MISSING.into()),
        Err(RunError::Failed(e)) => return fail_all(format!("gh: {e}")),
        Ok(out) if !out.success => return fail_all(classify_gh_error(&format!("{}\n{}", out.stderr, out.stdout), &cfg)),
        Ok(_) => {}
    }
    let has_new = actions.iter().any(|a| matches!(a, Action::Upsert(_)));
    let label = if has_new { ensure_label(&runner, &cfg).await } else { Ok(()) };
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
            Action::Upsert(d) => match (&label, &existing) {
                (Err(e), _) | (_, Err(e)) => ItemReport { key, result: Err(e.clone()), issue: None },
                (Ok(()), Ok(found)) => upsert(&runner, &cfg, &session_id, found, d).await,
            },
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
    use crate::preview::IssueDraft;
    use crate::test_support::{CREATED_URL, SID, marker_hits, write_runner};
    use superzed_sources::test_support::{FakeRunner, fail, ok};

    fn cfg() -> PmConfig {
        PmConfig { owner: "acme".into(), project_number: 0, repo: "acme/acme_app".into(), stale_days: 0 }
    }

    fn draft(item: usize, title: &str, assign_me: bool) -> Action {
        Action::Upsert(IssueDraft { key: format!("new:{item}"), item, title: title.into(), body: format!("corpo di {title}"), assign_me })
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
        assert_eq!(
            strs(&marker_search_args(&cfg(), SID)),
            ["issue", "list", "--repo", "acme/acme_app", "--state", "all", &format!("--search=\"superzed:session={SID}\" in:body"), "--json", "number,url,body", "--limit", "1000"]
        );
        assert_eq!(strs(&label_create_args(&cfg())), ["label", "create", "blocker", "-R", "acme/acme_app", "--description", "Waiting on a person", "--color", "D93F0B"]);
        let hostile = issue_create_args(&cfg(), "--fai-danni", "$(rm -rf ~)\n\"x\" 'y'", false);
        assert_eq!(strs(&hostile), ["issue", "create", "-R", "acme/acme_app", "--title=--fai-danni", "--body=$(rm -rf ~)\n\"x\" 'y'", "--label", "blocker"]);
        let assigned = issue_create_args(&cfg(), "t", "b", true);
        assert_eq!(strs(&assigned), ["issue", "create", "-R", "acme/acme_app", "--title=t", "--body=b", "--label", "blocker", "--assignee", "@me"]);
        assert_eq!(strs(&issue_edit_args(&cfg(), 40, "t", "b")), ["issue", "edit", "40", "--repo", "acme/acme_app", "--title=t", "--body=b", "--add-label", "blocker"]);
        assert_eq!(strs(&comment_args(&cfg(), 12, "-n")), ["issue", "comment", "12", "--repo", "acme/acme_app", "--body=-n"]);
        assert_eq!(strs(&close_args(&cfg(), 12, None)), ["issue", "close", "12", "--repo", "acme/acme_app"]);
        assert_eq!(strs(&close_args(&cfg(), 12, Some("fatto"))), ["issue", "close", "12", "--repo", "acme/acme_app", "--comment=fatto"]);
        assert_eq!(issue_body("corpo\n\n", SID, 2), format!("corpo\n\n<!-- superzed:session={SID}:item=2 -->"));
        assert_eq!(issue_url("acme/acme_app", 8), "https://github.com/acme/acme_app/issues/8");
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
    fn a_new_blocker_is_labelled_and_assigned_without_touching_a_project() {
        let r = write_runner();
        let out = run(&r, vec![draft(3, "Staging API key for the mailer", true)], BTreeMap::new());
        assert!(out[0].result.is_ok(), "{out:?}");
        let l = lines(&r);
        assert!(l.contains(&"gh label create blocker -R acme/acme_app --description Waiting on a person --color D93F0B".to_string()), "{l:?}");
        assert!(l.iter().any(|x| x.starts_with("gh issue create -R acme/acme_app") && x.contains("--label blocker --assignee @me")), "{l:?}");
        assert_eq!(r.count("gh project"), 0);
    }

    #[test]
    fn an_existing_label_and_a_token_without_project_scope_are_fine() {
        let r = write_runner();
        r.on("gh auth status", ok("Token scopes: 'repo'"));
        r.on("gh label create", fail(1, "label with name \"blocker\" already exists"));
        assert!(run(&r, vec![draft(3, "x", false)], BTreeMap::new())[0].result.is_ok());
    }

    #[test]
    fn new_issues_are_created_in_order_after_the_label_and_the_search() {
        let r = write_runner();
        let out = run(&r, vec![draft(2, "Policy", false), draft(3, "Altra", false)], BTreeMap::new());
        assert_eq!(out[0], ItemReport { key: "new:2".into(), result: Ok(CREATED_URL.into()), issue: Some(FoundIssue { number: 42, url: CREATED_URL.into() }) });
        let l = lines(&r);
        let heads: Vec<String> = l.iter().map(|l| l.split(' ').take(3).collect::<Vec<_>>().join(" ")).collect();
        assert_eq!(heads, ["gh auth status", "gh label create", "gh issue list", "gh issue create", "gh issue create"]);
        assert!(l[3].contains(&format!("--body=corpo di Policy\n\n{}", marker(SID, 2))), "{}", l[3]);
        assert!(!l[3].contains("--assignee"), "{}", l[3]);
    }

    #[test]
    fn an_existing_marker_is_edited_not_recreated() {
        let r = write_runner();
        r.on("gh issue list --repo acme/acme_app --state all --search=", ok(&marker_hits(SID, &[(0, 40)])));
        let out = run(&r, vec![draft(0, "Test", false), draft(1, "Formato", false)], BTreeMap::new());
        assert_eq!(out[0].issue.as_ref().map(|i| i.number), Some(40));
        assert_eq!(r.count("gh issue edit 40 --repo acme/acme_app --title=Test"), 1);
        let l = lines(&r);
        let edit = l.iter().position(|line| line.starts_with("gh issue edit 40")).unwrap();
        assert!(l[edit].ends_with("--add-label blocker"), "un blocker di prima prende l'etichetta: {}", l[edit]);
        assert!(l.iter().position(|line| line.starts_with("gh label create")).unwrap() < edit, "{l:?}");
        assert_eq!(r.count("gh issue create"), 1, "solo la voce 1, che non c'era");
        assert_eq!(r.count("gh issue list"), 1, "una ricerca per esecuzione");
        assert_eq!(r.count("gh label create"), 1, "una sola etichetta per esecuzione");
    }

    #[test]
    fn known_issues_win_over_the_search() {
        let r = write_runner();
        let known = BTreeMap::from([(0, FoundIssue { number: 42, url: CREATED_URL.into() })]);
        run(&r, vec![draft(0, "Test", false)], known);
        assert_eq!((r.count("gh issue create"), r.count("gh issue edit 42")), (0, 1));
    }

    #[test]
    fn a_failed_search_creates_nothing() {
        let r = write_runner();
        r.on("gh issue list --repo acme/acme_app --state all --search=", fail(1, "HTTP 502: bad gateway"));
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = run(&r, vec![draft(0, "Test", false), close], BTreeMap::new());
        assert_eq!(out[0].result, Err("search for issues already created failed: gh: HTTP 502: bad gateway".into()));
        assert_eq!(r.count("gh issue create"), 0);
        assert_eq!(out[1].result, Ok("#12 closed".into()), "le altre voci vanno avanti");
    }

    #[test]
    fn a_label_that_cannot_be_created_fails_new_issues_before_creating() {
        let r = write_runner();
        r.on("gh label create", fail(1, "HTTP 502: bad gateway"));
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = run(&r, vec![draft(0, "Test", false), close], BTreeMap::new());
        assert_eq!(out[0].result, Err("label “blocker”: gh: HTTP 502: bad gateway".into()));
        assert_eq!((r.count("gh issue create"), r.count("gh issue edit")), (0, 0));
        assert_eq!(out[1].result, Ok("#12 closed".into()));
    }

    #[test]
    fn touched_issues_close_and_comment_without_a_label_or_a_search() {
        let r = write_runner();
        let out = run(
            &r,
            vec![
                Action::Close { key: "touched:12".into(), number: 12, comment: Some("fatto".into()) },
                Action::Comment { key: "touched:8".into(), number: 8, body: "nota".into() },
                Action::Comment { key: "summary".into(), number: 12, body: "Riepilogo".into() },
            ],
            BTreeMap::new(),
        );
        assert!(out.iter().all(|o| o.result.is_ok()), "{out:?}");
        let l = lines(&r);
        assert_eq!((r.count("gh issue list"), r.count("gh label create"), r.count("gh project")), (0, 0, 0));
        assert!(l.contains(&"gh issue close 12 --repo acme/acme_app --comment=fatto".to_string()));
        assert!(l.contains(&"gh issue comment 8 --repo acme/acme_app --body=nota".to_string()));
        assert!(l.contains(&"gh issue comment 12 --repo acme/acme_app --body=Riepilogo".to_string()));
    }

    #[test]
    fn gh_missing_fails_everything() {
        let r = FakeRunner::new();
        r.on("gh", Err(superzed_sources::RunError::NotFound("gh".into())));
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = run(&r, vec![close, draft(0, "x", false)], BTreeMap::new());
        assert!(out.iter().all(|o| o.result == Err(GH_MISSING.to_string())));
        assert_eq!(r.calls().len(), 1);
    }

    #[test]
    fn a_foreign_issue_url_is_updated_in_place_and_reported() {
        let r = write_runner();
        r.on("gh issue create", ok("https://github.com/evil/other/issues/42\n"));
        let out = run(&r, vec![draft(0, "Test", false)], BTreeMap::new());
        let foreign = FoundIssue { number: 42, url: "https://github.com/evil/other/issues/42".into() };
        assert_eq!(out[0].result, Err(format!("gh issue create: issue #42 created outside “acme/acme_app”: “{}”", foreign.url)));
        assert_eq!(out[0].issue, Some(foreign.clone()), "revisione finale 4: la issue creata si ricorda");
        // riprova: si aggiorna quella (per link), mai una seconda create
        let out = run(&r, vec![draft(0, "Test", false)], BTreeMap::from([(0, foreign.clone())]));
        assert_eq!(r.count("gh issue create"), 1);
        assert_eq!(r.count(&format!("gh issue edit {} --title=Test", foreign.url)), 1);
        assert_eq!(out[0].issue, Some(foreign));
        assert!(out[0].result.as_ref().unwrap_err().contains("created outside"), "{out:?}");
    }

    #[test]
    fn a_case_different_repo_is_the_same_repo() {
        let r = write_runner();
        r.on("gh issue create", ok("https://github.com/Acme/Acme_App/issues/42\n"));
        let out = run(&r, vec![draft(0, "Test", false)], BTreeMap::new());
        assert!(out[0].result.is_ok(), "{out:?}");
    }

    // Revisione finale 1: ogni voce si consegna appena finita (l'esito si salva voce per voce).
    #[test]
    fn every_item_is_handed_over_as_soon_as_it_is_done() {
        let r = write_runner();
        let seen = std::sync::Mutex::new(Vec::new());
        let close = Action::Close { key: "touched:12".into(), number: 12, comment: None };
        let out = futures::executor::block_on(execute(r.clone(), cfg(), SID.into(), vec![draft(0, "Test", false), close], BTreeMap::new(), |item| {
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

    /// Con la ricerca fallita anche una voce gia' nota non si aggiorna: si riprova dopo
    /// (scelta prudente, nessuna scrittura su dati incompleti).
    #[test]
    fn a_known_issue_with_a_failed_search_is_not_written() {
        let r = write_runner();
        r.on("gh issue list --repo acme/acme_app --state all --search=", fail(1, "HTTP 502: bad gateway"));
        let known = BTreeMap::from([(0, FoundIssue { number: 42, url: CREATED_URL.into() })]);
        let out = run(&r, vec![draft(0, "Test", false)], known);
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
