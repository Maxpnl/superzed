//! Dati finti per i test (anche di altri crate, feature `test-support`): mai `gh` o `claude` veri.
use std::path::Path;
use std::sync::Arc;
use superzed_sources::test_support::{AUTH_OK, FakeRunner, ok};

/// Id di sessione di claude (forma reale).
pub const SID: &str = "11111111-1111-4111-8111-111111111111";

/// Report valido: una cosa fatta, un blocker, una mancanza, un problema, due issue toccate.
pub const REPORT_JSON: &str = r#"{"session_id": "11111111-1111-4111-8111-111111111111",
 "done": ["Import utenti da CSV"],
 "blockers": [{"title": "Policy di annullamento", "waiting_on": "design"}],
 "missing": ["Test dell'export PDF su Safari"],
 "problems": ["Quale formato per la nota di credito?"],
 "issues_touched": [{"number": 12, "action": "close", "note": "Import completato"}, {"number": 8, "action": "comment", "note": "Serve l'accesso allo staging"}]}"#;

/// Scrive `<root>/.git/config` con l'origin di acme_app.
pub fn write_git_origin(root: &Path) {
    let dir = root.join(".git");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config"), "[remote \"origin\"]\n\turl = git@github.com:acme/acme_app.git\n").unwrap();
}

/// Stdout di `gh issue create` (e di `gh issue edit`).
pub const CREATED_URL: &str = "https://github.com/acme/acme_app/issues/42";

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

/// `gh` finto per la scrittura su acme/acme_app: nessuna issue gia' creata.
pub fn write_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on("gh auth status", ok(AUTH_OK));
    r.on("gh label create", ok(""));
    r.on("gh issue list --repo acme/acme_app --state all --search=", ok("[]"));
    r.on("gh issue create", ok(&format!("{CREATED_URL}\n")));
    r.on("gh issue edit", ok(&format!("{CREATED_URL}\n")));
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
