//! Report senza la chat: `claude -p` (account dello scheduler) sulla coda del transcript
//! JSONL della sessione, prompt e transcript su stdin, risposta JSON validata.
use crate::report::{Report, TRANSCRIPT_SEPARATOR, headless_prompt, parse_report};
use futures::future::BoxFuture;
use serde_json::Value;
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
    sync::Arc,
    time::Duration,
};
use superzed_sources::{RunError, RunOutput, Runner, augmented_path};

pub const TRANSCRIPT_TAIL: u64 = 400 * 1024;
pub const CLAUDE_TIMEOUT: Duration = Duration::from_secs(300);

/// `claude -p` puo' metterci minuti: stesso `run_process` delle fonti, timeout piu' lungo.
pub struct ClaudeProcessRunner;

impl Runner for ClaudeProcessRunner {
    fn run(&self, program: &str, args: &[String], stdin: Option<String>) -> BoxFuture<'static, Result<RunOutput, RunError>> {
        superzed_sources::runner::run_process(program, args, stdin, CLAUDE_TIMEOUT)
    }
}

/// Ultimi `max` byte del transcript, a partire da una riga intera (JSONL), UTF-8 con sostituzioni.
pub fn transcript_tail(path: &Path, max: u64) -> Result<String, String> {
    let fail = |e: std::io::Error| format!("transcript {}: {e}", path.display());
    let mut file = std::fs::File::open(path).map_err(fail)?;
    let len = file.metadata().map_err(fail)?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).map_err(fail)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(fail)?;
    // tagliato a meta' riga: si riparte dalla riga dopo
    if start > 0
        && let Some(nl) = bytes.iter().position(|b| *b == b'\n')
    {
        bytes.drain(..=nl);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Variabili degli hook di superzed: un `claude -p` che le eredita manderebbe i suoi
/// eventi al nodo della chat da cui e' partito.
pub const SUPERZED_HOOK_VARS: &[&str] = &["SUPERZED_NODE", "SUPERZED_HOOK_SOCK", "SUPERZED_HOOK_TOKEN"];

/// `/usr/bin/env` con l'account giusto: via `CLAUDE_CONFIG_DIR`, le credenziali, le
/// variabili di una sessione claude ereditate (come nei nodi), quelle di nodeterm e degli
/// hook di superzed; `~/.local/bin` nel `PATH`, cartella di lavoro `cwd` (una temporanea:
/// il report si fa solo dal transcript), nessuno strumento, un solo turno.
/// `config_dir` `None` = account di sistema (`~/.claude`).
pub fn claude_argv(
    config_dir: Option<&Path>,
    home: &Path,
    cwd: &Path,
    env_names: impl Iterator<Item = String>,
    path_env: Option<&str>,
) -> (String, Vec<String>) {
    let mut unset: Vec<String> = vec!["CLAUDE_CONFIG_DIR".into()];
    unset.extend(superzed_sessions::launch::CLAUDE_AUTH_VARS.iter().map(|v| v.to_string()));
    unset.extend(superzed_sessions::launch::NODETERM_VARS.iter().map(|v| v.to_string()));
    unset.extend(SUPERZED_HOOK_VARS.iter().map(|v| v.to_string()));
    unset.extend(superzed_sessions::launch::inherited_claude_session_vars(env_names));
    let mut args = vec!["-C".to_string(), cwd.display().to_string()];
    for name in unset {
        if !args.windows(2).any(|w: &[String]| w[0] == "-u" && w[1] == name) {
            args.push("-u".to_string());
            args.push(name);
        }
    }
    args.push(format!("PATH={}:{}", home.join(".local/bin").display(), augmented_path(path_env)));
    if let Some(dir) = config_dir {
        args.push(format!("CLAUDE_CONFIG_DIR={}", dir.display()));
    }
    // `--tools ""` = nessuno strumento (in fondo: l'opzione prende piu' valori)
    args.extend(
        ["claude", "-p", "--output-format", "json", "--max-turns", "1", "--no-session-persistence", "--tools", ""]
            .map(String::from),
    );
    ("/usr/bin/env".to_string(), args)
}

pub fn headless_stdin(session_id: &str, areas: &[String], transcript: &str) -> String {
    format!("{}\n{TRANSCRIPT_SEPARATOR}\n{transcript}", headless_prompt(session_id, areas))
}

fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("unknown error").chars().take(300).collect()
}

/// Dal JSON di `--output-format json` al report: `result` (anche dentro un blocco ```json) validato.
pub fn parse_claude_output(stdout: &str, session_id: &str) -> Result<Report, String> {
    let v: Value = serde_json::from_str(stdout.trim()).map_err(|_| "claude -p: non-JSON output".to_string())?;
    let result = v.get("result").and_then(Value::as_str).unwrap_or_default();
    if v.get("is_error").and_then(Value::as_bool) == Some(true) {
        return Err(format!("claude -p: {}", first_line(result)));
    }
    let (Some(start), Some(end)) = (result.find('{'), result.rfind('}')) else {
        return Err("claude -p: no JSON in the response".into());
    };
    if end < start {
        return Err("claude -p: no JSON in the response".into());
    }
    parse_report(&result[start..=end], session_id)
}

pub async fn headless_report(runner: Arc<dyn Runner>, argv: (String, Vec<String>), stdin: String, session_id: String) -> Result<Report, String> {
    let (program, args) = argv;
    match runner.run(&program, &args, Some(stdin)).await {
        Err(RunError::NotFound(p)) => Err(format!("“{p}” not found")),
        Err(RunError::Failed(e)) => Err(format!("claude -p: {e}")),
        // 127: `env` non ha trovato `claude` nel PATH
        Ok(out) if out.code == Some(127) => Err("claude not found: install Claude Code (“claude” must be in the PATH)".into()),
        Ok(out) if !out.success => Err(format!(
            "claude -p: exited with {} ({})",
            out.code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into()),
            first_line(&out.stderr)
        )),
        Ok(out) => parse_claude_output(&out.stdout, &session_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{REPORT_JSON, SID, claude_output};
    use superzed_sources::test_support::{FakeRunner, fail};

    fn pos(args: &[String], needle: &str) -> usize {
        args.iter().position(|a| a == needle).unwrap_or_else(|| panic!("{needle} assente in {args:?}"))
    }

    #[test]
    fn argv_for_system_and_accounts() {
        let home = Path::new("/Users/u");
        let cwd = Path::new("/tmp/superzed-headless");
        let names = ["CLAUDECODE", "HOME", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_OAUTH_TOKEN"].map(String::from);
        let (program, a) = claude_argv(None, home, cwd, names.into_iter(), Some("/usr/bin"));
        assert_eq!(program, "/usr/bin/env");
        // revisione finale 3: nessuno strumento, un solo turno, niente sessione salvata
        assert_eq!(
            &a[a.len() - 9..],
            ["claude", "-p", "--output-format", "json", "--max-turns", "1", "--no-session-persistence", "--tools", ""]
        );
        assert_eq!(&a[..2], ["-C", "/tmp/superzed-headless"], "cartella di lavoro: una temporanea");
        let unset: Vec<&str> = a.windows(2).filter(|w| w[0] == "-u").map(|w| w[1].as_str()).collect();
        let expected = ["CLAUDE_CONFIG_DIR", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"]
            .into_iter()
            .chain(superzed_sessions::launch::NODETERM_VARS.iter().copied())
            .chain(["SUPERZED_NODE", "SUPERZED_HOOK_SOCK", "SUPERZED_HOOK_TOKEN"]);
        for var in expected {
            assert_eq!(unset.iter().filter(|u| **u == var).count(), 1, "{var} tolta una volta: {a:?}");
        }
        assert!(!unset.contains(&"HOME"));
        assert!(a.contains(&"PATH=/Users/u/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin".to_string()), "{a:?}");
        assert!(!a.iter().any(|x| x.starts_with("CLAUDE_CONFIG_DIR=")), "sistema: ~/.claude");
        let (_, a) = claude_argv(Some(Path::new("/cfg/a1")), home, cwd, std::iter::empty(), None);
        // sintassi di env: prima le opzioni, poi le assegnazioni, poi il comando
        let last_unset = a.iter().rposition(|x| x == "-u").unwrap();
        assert!(last_unset < pos(&a, "CLAUDE_CONFIG_DIR=/cfg/a1") && pos(&a, "CLAUDE_CONFIG_DIR=/cfg/a1") < pos(&a, "claude"));
    }

    #[test]
    fn transcript_tail_starts_on_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let lines: String = (0..100).map(|i| format!("{{\"n\":{i}}}\n")).collect();
        std::fs::write(&path, &lines).unwrap();
        assert_eq!(transcript_tail(&path, 10_000).unwrap(), lines, "piu' piccolo del limite: tutto");
        let tail = transcript_tail(&path, 30).unwrap();
        assert!(tail.starts_with("{\"n\":") && tail.ends_with("{\"n\":99}\n"), "{tail:?}");
        assert!(tail.len() <= 30);
        std::fs::write(&path, b"{\"a\":1}\n\xff\xfe{\"b\":2}\n").unwrap();
        assert!(transcript_tail(&path, 1000).unwrap().contains("{\"b\":2}"), "UTF-8 rotto: sostituito");
        assert!(transcript_tail(&dir.path().join("no.jsonl"), 10).unwrap_err().contains("no.jsonl"));
    }

    #[test]
    fn claude_output_is_unwrapped_and_validated() {
        let r = parse_claude_output(&claude_output(REPORT_JSON), SID).unwrap();
        assert_eq!((r.session_id.as_str(), r.missing.len()), (SID, 1));
        let failed = serde_json::json!({"type": "result", "is_error": true, "result": "Credit balance is too low\naltro"}).to_string();
        assert_eq!(parse_claude_output(&failed, SID).unwrap_err(), "claude -p: Credit balance is too low");
        let prose = serde_json::json!({"type": "result", "is_error": false, "result": "Non ho trovato nulla."}).to_string();
        assert_eq!(parse_claude_output(&prose, SID).unwrap_err(), "claude -p: no JSON in the response");
        let bad = claude_output(r#"{"done": "x"}"#);
        assert_eq!(parse_claude_output(&bad, SID).unwrap_err(), "done: a list is needed");
        assert_eq!(parse_claude_output("boh", SID).unwrap_err(), "claude -p: non-JSON output");
    }

    #[test]
    fn headless_runs_claude_with_the_transcript_on_stdin() {
        let r = FakeRunner::new();
        r.on("claude -p --output-format json", superzed_sources::test_support::ok(&claude_output(REPORT_JSON)));
        let argv = claude_argv(None, Path::new("/Users/u"), Path::new("/tmp"), std::iter::empty(), None);
        let stdin = headless_stdin(SID, &["frontend".into()], "{\"t\":1}\n");
        let report = futures::executor::block_on(headless_report(r.clone(), argv.clone(), stdin, SID.into())).unwrap();
        assert_eq!(report.blockers.len(), 1);
        let call = &r.calls()[0];
        assert_eq!(call.program, "/usr/bin/env");
        let sent = call.stdin.as_deref().unwrap();
        assert!(sent.contains(&format!("\n{TRANSCRIPT_SEPARATOR}\n{{\"t\":1}}\n")) && sent.contains("una fra frontend"), "{sent}");
        let r = FakeRunner::new();
        r.on("claude -p", fail(127, "env: claude: No such file or directory"));
        let err = futures::executor::block_on(headless_report(r, argv.clone(), String::new(), SID.into())).unwrap_err();
        assert_eq!(err, "claude not found: install Claude Code (“claude” must be in the PATH)");
        let r = FakeRunner::new();
        r.on("claude -p", fail(1, "boom\naltro"));
        let err = futures::executor::block_on(headless_report(r, argv, String::new(), SID.into())).unwrap_err();
        assert_eq!(err, "claude -p: exited with 1 (boom)");
    }
}
