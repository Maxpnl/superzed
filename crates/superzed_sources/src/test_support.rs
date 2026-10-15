//! Finti per i test (anche di altri crate, feature `test-support`): mai `gh`, `ssh` o rete.
use crate::runner::{RunError, RunOutput, Runner};
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub struct RunCall {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

impl RunCall {
    /// `programma arg1 arg2 …`, per cercarci dentro.
    pub fn line(&self) -> String {
        std::iter::once(self.program.clone()).chain(self.args.iter().cloned()).collect::<Vec<_>>().join(" ")
    }
}

/// Risposte per sottostringa della riga di comando: vince l'ago piu' lungo che compare.
/// Un comando senza risposta fallisce con "comando non previsto".
#[derive(Default)]
pub struct FakeRunner {
    rules: Mutex<Vec<(String, Result<RunOutput, RunError>)>>,
    calls: Mutex<Vec<RunCall>>,
}

impl FakeRunner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn on(&self, needle: &str, result: Result<RunOutput, RunError>) {
        let mut rules = self.rules.lock().unwrap();
        rules.retain(|(n, _)| n != needle);
        rules.push((needle.to_string(), result));
    }

    pub fn calls(&self) -> Vec<RunCall> {
        self.calls.lock().unwrap().clone()
    }

    pub fn count(&self, needle: &str) -> usize {
        self.calls().iter().filter(|c| c.line().contains(needle)).count()
    }
}

impl Runner for FakeRunner {
    fn run(&self, program: &str, args: &[String], stdin: Option<String>) -> BoxFuture<'static, Result<RunOutput, RunError>> {
        let call = RunCall { program: program.to_string(), args: args.to_vec(), stdin };
        let line = call.line();
        self.calls.lock().unwrap().push(call);
        let result = self
            .rules
            .lock()
            .unwrap()
            .iter()
            .filter(|(needle, _)| line.contains(needle.as_str()))
            .max_by_key(|(needle, _)| needle.len())
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| Err(RunError::Failed(format!("comando non previsto: {line}"))));
        Box::pin(async move { result })
    }
}

pub fn ok(stdout: &str) -> Result<RunOutput, RunError> {
    Ok(RunOutput { success: true, code: Some(0), stdout: stdout.to_string(), stderr: String::new() })
}

pub fn fail(code: i32, stderr: &str) -> Result<RunOutput, RunError> {
    Ok(RunOutput { success: false, code: Some(code), stdout: String::new(), stderr: stderr.to_string() })
}

/// `gh auth status` (gh 2.72: su stdout).
pub const AUTH_OK: &str = "github.com\n  ✓ Logged in to github.com account Maxpnl (keyring)\n  - Active account: true\n  - Token scopes: 'gist', 'project', 'read:org', 'repo', 'workflow'\n";
pub const AUTH_NO_SCOPE: &str = "github.com\n  ✓ Logged in to github.com account Maxpnl (keyring)\n  - Token scopes: 'gist', 'read:org', 'repo'\n";

/// `gh project item-list 3 --owner acme --format json --limit 500` (forma reale, dati finti).
pub const PROJECT_ITEMS: &str = r#"{"items":[
 {"id":"PVTI_1","title":"Import utenti","content":{"type":"Issue","number":1,"repository":"acme/acme_app","title":"Import utenti","url":"https://github.com/acme/acme_app/issues/1"},"status":"In Progress","kind":"Task","area":"frontend","waiting on":""},
 {"id":"PVTI_2","content":{"type":"Issue","number":2,"repository":"acme/acme_app"},"status":"Blocked","kind":"Task","area":"frontend","waiting on":"design"},
 {"id":"PVTI_3","content":{"type":"Issue","number":3,"repository":"acme/acme_app"},"status":"Backlog","kind":"Question","area":"backend"},
 {"id":"PVTI_4","content":{"type":"Issue","number":4,"repository":"acme/acme_app"},"status":"Done","kind":"Task","area":"frontend"},
 {"id":"PVTI_5","content":{"type":"Issue","number":5,"repository":"acme/acme_app"},"status":"Backlog","kind":"Deliverable","area":"frontend"},
 {"id":"PVTI_6","title":"Gestione dei webhook","content":{"body":"Da capire con design.","id":"DI_6","title":"Gestione dei webhook","type":"DraftIssue"},"status":"Backlog","area":"frontend"},
 {"id":"PVTI_7","content":{"type":"Issue","number":9,"repository":"acme/infra","title":"Ruolo ClickHouse","url":"https://github.com/acme/infra/issues/9"},"status":"In Progress","kind":"Task","area":"provisioning"},
 {"id":"PVTI_8","title":"Revisione listini","content":{"body":"","id":"DI_8","title":"Revisione listini","type":"DraftIssue"},"status":"In review"}
],"totalCount":8}"#;

/// `gh issue list --repo acme/acme_app --state all --limit 500 --json …`.
pub const ISSUES: &str = r#"[
 {"number":1,"title":"Import utenti da CSV","url":"https://github.com/acme/acme_app/issues/1","state":"OPEN","body":"","labels":[],"createdAt":"2026-08-20T09:00:00Z","updatedAt":"2026-09-25T09:00:00Z","closedAt":null},
 {"number":2,"title":"Annullamento fattura","url":"https://github.com/acme/acme_app/issues/2","state":"OPEN","body":"Serve la policy.\nBlocked by: #5","labels":[],"createdAt":"2026-08-10T09:00:00Z","updatedAt":"2026-09-22T09:00:00Z","closedAt":null},
 {"number":3,"title":"Quale libreria PDF usare","url":"https://github.com/acme/acme_app/issues/3","state":"OPEN","body":"Serve scegliere il modello.","labels":[],"createdAt":"2026-08-01T09:00:00Z","updatedAt":"2026-09-20T09:00:00Z","closedAt":null},
 {"number":4,"title":"Login operatore","url":"https://github.com/acme/acme_app/issues/4","state":"CLOSED","body":"","labels":[],"createdAt":"2026-08-01T09:00:00Z","updatedAt":"2026-09-24T09:00:00Z","closedAt":"2026-09-24T09:00:00Z"},
 {"number":5,"title":"Policy di annullamento","url":"https://github.com/acme/acme_app/issues/5","state":"OPEN","body":"","labels":[],"createdAt":"2026-07-01T09:00:00Z","updatedAt":"2026-08-01T09:00:00Z","closedAt":null},
 {"number":8,"title":"Crash all'export della fattura","url":"https://github.com/acme/acme_app/issues/8","state":"OPEN","body":"Stack in allegato.","labels":[{"name":"bug"}],"createdAt":"2026-09-26T08:00:00Z","updatedAt":"2026-09-26T09:00:00Z","closedAt":null}
]"#;

/// `gh issue view 8 --repo acme/acme_app --json …`.
pub const ISSUE_VIEW_8: &str = r#"{"number":8,"title":"Crash all'export della fattura","url":"https://github.com/acme/acme_app/issues/8","state":"OPEN","body":"Stack in allegato.","labels":[{"name":"bug"}],"comments":[{"author":{"login":"Maxpnl"},"body":"Succede solo su Safari","createdAt":"2026-09-26T10:00:00Z"}],"createdAt":"2026-09-26T08:00:00Z","updatedAt":"2026-09-26T10:00:00Z","closedAt":null}"#;

/// `gh` finto con la board di acme/acme_app (Project 3 di acme).
pub fn github_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on("gh auth status", ok(AUTH_OK));
    r.on("gh project item-list 3 --owner acme", ok(PROJECT_ITEMS));
    r.on("gh issue list --repo acme/acme_app", ok(ISSUES));
    r.on("gh issue view 8 --repo acme/acme_app", ok(ISSUE_VIEW_8));
    r
}

/// Tunnel finto: un comando fisso al posto di ssh (`/bin/sleep 30`, `/bin/sh -c '…; exit 1'`).
pub struct FixedTunnelCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl FixedTunnelCommand {
    pub fn new(program: &str, args: &[&str]) -> Arc<Self> {
        Arc::new(Self { program: program.to_string(), args: args.iter().map(|a| a.to_string()).collect() })
    }
}

impl crate::tunnel::TunnelCommand for FixedTunnelCommand {
    fn command(&self, _: &crate::connections::TunnelConfig, _: &[u16]) -> (String, Vec<String>) {
        (self.program.clone(), self.args.clone())
    }
}

/// Tunnel finto che registra la configurazione di ogni avvio (poi `/bin/sleep 30`).
#[derive(Default)]
pub struct RecordingTunnelCommand {
    spawned: Mutex<Vec<crate::connections::TunnelConfig>>,
}

impl RecordingTunnelCommand {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Host ssh di ogni avvio, in ordine.
    pub fn hosts(&self) -> Vec<String> {
        self.spawned.lock().unwrap().iter().map(|c| c.host.clone()).collect()
    }
}

impl crate::tunnel::TunnelCommand for RecordingTunnelCommand {
    fn command(&self, config: &crate::connections::TunnelConfig, _: &[u16]) -> (String, Vec<String>) {
        self.spawned.lock().unwrap().push(config.clone());
        ("/bin/sleep".into(), vec!["30".into()])
    }
}

pub fn deps_with(
    runner: Arc<FakeRunner>,
    http: Arc<dyn http_client::HttpClient>,
    secrets: std::rc::Rc<crate::secrets::MemorySecrets>,
    tunnel: Arc<dyn crate::tunnel::TunnelCommand>,
) -> crate::store::SourceDeps {
    crate::store::SourceDeps { runner, http, secrets, tunnel_command: tunnel }
}

/// Segreti in memoria e tunnel finti (`/bin/sleep 30`).
pub fn deps(runner: Arc<FakeRunner>, http: Arc<dyn http_client::HttpClient>) -> crate::store::SourceDeps {
    deps_with(runner, http, std::rc::Rc::default(), FixedTunnelCommand::new("/bin/sleep", &["30"]))
}

/// `connections.json` con la sola connessione GitHub `gh`.
pub fn github_connections() -> serde_json::Value {
    serde_json::json!({"version": 1, "connections": [
        {"id": "gh", "name": "acme-app", "kind": "github", "config": {"owner": "acme", "project_number": 3, "repo": "acme/acme_app"}}
    ]})
}

/// `gog --version` (build di release).
pub const GOG_VERSION: &str = "v0.44.0 (cf42bb7 2026-10-11T18:32:54Z)\n";

/// `gog … gmail search --max 50 --json -- '(from:@acme.example) '` (forma di gog 0.44, dati finti).
pub const GOG_SEARCH: &str = r#"{
  "count": 2,
  "has_more": false,
  "nextPageToken": "",
  "threads": [
    {"id": "t-200", "date": "2026-10-14 09:12", "internalDateIso": "2026-10-14T09:12:00+02:00", "from": "Alice <alice@example.com>", "subject": "Policy di annullamento approvata", "labels": ["UNREAD", "INBOX"], "messageCount": 2},
    {"id": "t-100", "from": "j.doe@example.com", "subject": "Atlas: listini", "messageCount": 1}
  ]
}"#;

/// `gog … gmail thread get t-200 --sanitize-content --json`.
pub const GOG_THREAD: &str = r#"{
  "downloaded": null,
  "thread": {
    "id": "t-200",
    "messages": [
      {"id": "m-201", "threadId": "t-200", "labelIds": ["INBOX"], "internalDate": 1760425920000,
       "headers": {"from": "Alice <alice@example.com>", "to": "j.doe@example.com", "subject": "Policy di annullamento approvata", "date": "Tue, 14 Oct 2026 09:12:00 +0200"},
       "body": "Ciao, la policy è approvata. Potete procedere con l'annullamento.",
       "attachments": [{"filename": "policy.pdf", "size": 1200, "sizeHuman": "1.2 KB", "mimeType": "application/pdf"}]},
      {"id": "m-202", "threadId": "t-200", "labelIds": ["UNREAD", "INBOX"], "internalDate": 1760430000000,
       "headers": {"from": "Alice <alice@example.com>", "reply_to": "team@example.com", "to": "j.doe@example.com", "cc": "bob@example.com", "subject": "Re: Policy di annullamento approvata", "date": "Tue, 14 Oct 2026 10:20:00 +0200"},
       "body": "Dimenticavo: vale da lunedì."}
    ]
  }
}"#;

/// `gog … gmail get m-201 --json` (corpo decodificato, a capo conservati).
pub const GOG_MESSAGE_1: &str = r#"{"message": {"id": "m-201"}, "headers": {"from": "Alice <alice@example.com>"}, "body": "Ciao,\nla policy è approvata.\n\nPotete procedere con l'annullamento.\n", "attachments": []}"#;
pub const GOG_MESSAGE_2: &str = r#"{"message": {"id": "m-202"}, "headers": {"from": "Alice <alice@example.com>"}, "body": "Dimenticavo:\nvale da lunedì.\n"}"#;

/// `gog … gmail drafts list --json`.
pub const GOG_DRAFTS: &str = r#"{"count": 1, "drafts": [{"id": "r-1", "messageId": "m-900", "threadId": "t-200"}, {"id": "r-2"}], "has_more": false, "nextPageToken": ""}"#;

/// Errore JSON di gog per un account senza token (exit 4).
pub const GOG_AUTH_ERROR: &str = r#"{"error":{"code":4,"message":"No auth for gmail j.doe@example.com.","class":"auth_required"}}"#;

/// `gog` finto per l'account `j.doe@example.com`.
pub fn gmail_runner() -> Arc<FakeRunner> {
    let r = FakeRunner::new();
    r.on("gog --version", ok(GOG_VERSION));
    r.on("gmail search", ok(GOG_SEARCH));
    r.on("gmail thread get t-200", ok(GOG_THREAD));
    r.on("gmail get m-201", ok(GOG_MESSAGE_1));
    r.on("gmail get m-202", ok(GOG_MESSAGE_2));
    r.on("gmail drafts list", ok(GOG_DRAFTS));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_needle_wins_and_calls_are_recorded() {
        let r = FakeRunner::new();
        r.on("issue", ok("corto"));
        r.on("issue list", ok("lungo"));
        let run = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            futures::executor::block_on(r.run("gh", &args, Some("in".into())))
        };
        assert_eq!(run(&["issue", "list", "--repo", "o/r"]).unwrap().stdout, "lungo");
        assert_eq!(run(&["issue", "view", "3"]).unwrap().stdout, "corto");
        assert!(matches!(run(&["auth", "status"]), Err(RunError::Failed(e)) if e.contains("gh auth status")));
        assert_eq!(r.count("issue"), 2);
        assert_eq!(r.calls()[0].stdin.as_deref(), Some("in"));
    }
}
