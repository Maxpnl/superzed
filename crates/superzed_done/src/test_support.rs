//! Dati finti per i test (anche di altri crate, feature `test-support`): mai `gh` o `claude` veri.
use std::path::Path;
use std::sync::Arc;
use superzed_sources::test_support::{AUTH_OK, FakeRunner, ok};

/// Id di sessione di claude (forma reale).
pub const SID: &str = "11111111-1111-4111-8111-111111111111";

/// `.claude/claude-pm.local.md` di acme_app (Project 3 di acme, due aree).
pub const PM_TEXT: &str = "---\nproject_number: 3\nproject_owner: acme\nrepo: \"acme/acme_app\"\nareas:\n  frontend: [\"app/**\"]\n  backend:\n    - \"sap/**\"\nquestion_assignee: Maxpnl\n---\n\nNote del progetto.\n";

/// Report valido: una cosa fatta, un blocker, una mancanza, un problema, due issue toccate.
pub const REPORT_JSON: &str = r#"{"session_id": "11111111-1111-4111-8111-111111111111", "area": "frontend",
 "done": ["Import utenti da CSV"],
 "blockers": [{"title": "Policy di annullamento", "waiting_on": "design"}],
 "missing": ["Test dell'export PDF su Safari"],
 "problems": ["Quale formato per la nota di credito?"],
 "issues_touched": [{"number": 12, "action": "close", "note": "Import completato"}, {"number": 8, "action": "update", "note": "Serve l'accesso allo staging"}]}"#;

/// Scrive `PM_TEXT` in `<root>/.claude/claude-pm.local.md`.
pub fn write_pm(root: &Path) {
    let path = root.join(superzed_sources::github::PM_CONFIG);
    std::fs::create_dir_all(path.parent().expect("cartella .claude")).unwrap();
    std::fs::write(path, PM_TEXT).unwrap();
}

/// `gh project view 3 --owner acme --format json` (forma reale, dati finti).
pub const PROJECT_VIEW: &str = r#"{"id":"PVT_kwDOA","number":3,"title":"acme-app","url":"https://github.com/orgs/acme/projects/3"}"#;

/// `gh project field-list 3 --owner acme --format json --limit 100`.
pub const FIELDS: &str = r#"{"fields":[
 {"id":"PVTF_title","name":"Title","type":"ProjectV2Field"},
 {"id":"PVTSSF_status","name":"Status","type":"ProjectV2SingleSelectField","options":[{"id":"st_backlog","name":"Backlog"},{"id":"st_progress","name":"In Progress"},{"id":"st_blocked","name":"Blocked"},{"id":"st_done","name":"Done"}]},
 {"id":"PVTSSF_kind","name":"Kind","type":"ProjectV2SingleSelectField","options":[{"id":"k_deliv","name":"Deliverable"},{"id":"k_task","name":"Task"},{"id":"k_blocker","name":"Blocker"},{"id":"k_question","name":"Question"}]},
 {"id":"PVTSSF_area","name":"Area","type":"ProjectV2SingleSelectField","options":[{"id":"a_frontend","name":"frontend"},{"id":"a_backend","name":"backend"}]},
 {"id":"PVTF_waiting","name":"Waiting on","type":"ProjectV2Field"}
],"totalCount":5}"#;

/// Stdout di `gh issue create` (e di `gh issue edit`).
pub const CREATED_URL: &str = "https://github.com/acme/acme_app/issues/42";

/// `gh project item-add … --format json`.
pub const ITEM_ADDED: &str = r#"{"id":"PVTI_new","title":"x","type":"Issue"}"#;

/// Risposta di `gh issue list --search …` con un marker di `session` per ogni (voce, numero).
pub fn marker_hits(session: &str, hits: &[(usize, u64)]) -> String {
    let issues: Vec<serde_json::Value> = hits
        .iter()
        .map(|(item, number)| {
            serde_json::json!({
                "number": number,
                "url": format!("https://github.com/acme/acme_app/issues/{number}"),
                "body": format!("testo\n\n<!-- superzed:session={session}:item={item} -->"),
            })
        })
        .collect();
    serde_json::Value::Array(issues).to_string()
}

/// `gh` finto per la scrittura sulla board di acme/acme_app: nessuna issue gia' creata.
pub fn write_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on("gh auth status", ok(AUTH_OK));
    r.on("gh project view 3 --owner acme", ok(PROJECT_VIEW));
    r.on("gh project field-list 3 --owner acme", ok(FIELDS));
    r.on("gh issue list --repo acme/acme_app --state all --search=", ok("[]"));
    r.on("gh issue create", ok(&format!("{CREATED_URL}\n")));
    r.on("gh issue edit", ok(&format!("{CREATED_URL}\n")));
    r.on("gh project item-add 3 --owner acme", ok(ITEM_ADDED));
    r.on("gh project item-edit", ok(r#"{"id":"PVTI_new"}"#));
    r.on("gh issue comment", ok("https://github.com/acme/acme_app/issues/12#issuecomment-1\n"));
    r.on("gh issue close", ok(""));
    r
}

/// Stdout di `claude -p --output-format json` con il report in un blocco ```json.
pub fn claude_output(report_json: &str) -> String {
    serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
        "result": format!("Ecco il report:\n```json\n{report_json}\n```"),
        "session_id": "headless-1"
    })
    .to_string()
}
