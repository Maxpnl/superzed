/// Quoting per `sh`: tutto tra apici singoli.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

use std::path::{Path, PathBuf};

/// Variabili di nodeterm: con superzed lanciato da nodeterm, i suoi hook
/// attribuirebbero gli eventi di un nodo superzed a un nodo di nodeterm.
pub const NODETERM_VARS: &[&str] = &[
    "NODETERM_NODE_ID",
    "NODETERM_HOOK_SOCK",
    "NODETERM_HOOK_ENDPOINT",
    "NODETERM_HOOK_VERSION",
    "NODETERM_AGENT_ID",
    "NODETERM_CANVAS_CONTROL",
    "NODETERM_PERM_WAIT_SECS",
];

/// Variabili di una sessione Claude Code da cui superzed e' stato lanciato: un
/// claude che le eredita si crede una sessione figlia (niente transcript, quindi
/// niente resume). `CLAUDE_CONFIG_DIR` e' a parte (account, vedi `unset_inherited`).
pub fn inherited_claude_session_vars(names: impl Iterator<Item = String>) -> Vec<String> {
    names
        .filter(|n| {
            n == "CLAUDECODE" || n.starts_with("CLAUDE_CODE_") || n == "CLAUDE_PID" || n == "CLAUDE_EFFORT"
        })
        .collect()
}

/// Credenziali nell'ambiente che vincerebbero sull'account scelto (API key, token OAuth).
pub const CLAUDE_AUTH_VARS: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"];

/// `unset` delle variabili ereditate: account e credenziali (l'account lo sceglie
/// superzed con `CLAUDE_CONFIG_DIR`) e nodeterm.
pub fn unset_inherited() -> String {
    let mut s = String::from("unset CLAUDE_CONFIG_DIR CODEX_HOME");
    for var in CLAUDE_AUTH_VARS.iter().chain(NODETERM_VARS) {
        s.push(' ');
        s.push_str(var);
    }
    s
}

#[derive(Clone, Debug, PartialEq)]
pub struct HookEnv {
    pub socket: PathBuf,
    pub token: String,
    pub settings: PathBuf,
    /// Cartella di `superzed-notify`, messa davanti al `PATH` delle chat.
    pub bin_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClaudeLaunch {
    /// Comando `claude …` gia' quotato, da mettere nello script del nodo.
    pub command: String,
    pub env: Vec<(String, String)>,
    /// Id con cui parte la sessione (ripresa o nuova).
    pub session_id: String,
    /// Cartella da mettere davanti al `PATH` del nodo (quella di `superzed-notify`).
    pub path_prefix: Option<PathBuf>,
    /// Account non di sistema: `CLAUDE_CONFIG_DIR` del nodo (lo imposta il canvas).
    pub config_dir: Option<PathBuf>,
}

pub fn claude_launch(
    hooks: Option<&HookEnv>,
    node_key: &str,
    saved_session: Option<&str>,
    transcript_exists: impl Fn(&str) -> bool,
    new_session_id: impl FnOnce() -> String,
    user_statusline: Option<String>,
    initial_prompt: Option<&str>,
) -> ClaudeLaunch {
    let mut command = String::from("claude --dangerously-skip-permissions");
    let mut env = Vec::new();
    let mut path_prefix = None;
    if let Some(h) = hooks {
        command.push_str(&format!(" --settings {}", shell_quote(&h.settings.to_string_lossy())));
        env.push(("SUPERZED_NODE".to_string(), node_key.to_string()));
        env.push(("SUPERZED_HOOK_SOCK".to_string(), h.socket.to_string_lossy().into_owned()));
        env.push(("SUPERZED_HOOK_TOKEN".to_string(), h.token.clone()));
        env.push((
            "SUPERZED_NOTIFY".to_string(),
            h.bin_dir.join(crate::hook_files::NOTIFY_SCRIPT).to_string_lossy().into_owned(),
        ));
        path_prefix = Some(h.bin_dir.clone());
        if let Some(sl) = user_statusline {
            env.push(("SUPERZED_USER_STATUSLINE".to_string(), sl));
        }
    }
    // claude scrive il transcript solo dopo il primo messaggio: senza, niente resume
    let session_id = match saved_session {
        Some(id) if transcript_exists(id) => {
            command.push_str(&format!(" --resume {}", shell_quote(id)));
            id.to_string()
        }
        _ => {
            let id = new_session_id();
            command.push_str(&format!(" --session-id {}", shell_quote(&id)));
            // prompt iniziale posizionale: solo per una sessione nuova
            if let Some(prompt) = initial_prompt.filter(|p| !p.trim().is_empty()) {
                command.push_str(&format!(" -- {}", shell_quote(prompt)));
            }
            id
        }
    };
    ClaudeLaunch { command, env, session_id, path_prefix, config_dir: None }
}

pub fn system_config_dir(home: &Path) -> PathBuf {
    home.join(".claude")
}

/// Parte locale dell'email dell'account; "system" se non si legge.
pub fn account_label(config_dir: Option<&Path>, home: &Path) -> String {
    let file = match config_dir {
        Some(dir) => dir.join(".claude.json"),
        None => home.join(".claude.json"),
    };
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.pointer("/oauthAccount/emailAddress")?.as_str().map(str::to_string))
        .and_then(|email| email.split('@').next().map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "system".to_string())
}

/// `statusLine.command` dell'utente nella config dir, se c'e'.
pub fn user_statusline(config_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_dir.join("settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.pointer("/statusLine/command")?.as_str().map(str::to_string)
}

/// Il transcript `<config_dir>/projects/*/<id>.jsonl` esiste?
pub fn transcript_exists(config_dir: &Path, session_id: &str) -> bool {
    if session_id.is_empty() || !session_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return false;
    }
    let file = format!("{session_id}.jsonl");
    std::fs::read_dir(config_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path().join(&file).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("/a b"), "'/a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    fn hooks() -> HookEnv {
        HookEnv {
            socket: "/data dir/superzed.sock".into(),
            token: "tok".into(),
            settings: "/data dir/claude/settings.json".into(),
            bin_dir: "/data dir/claude/bin".into(),
        }
    }

    #[test]
    fn resume_only_when_transcript_exists() {
        let l = claude_launch(Some(&hooks()), "key-1", Some("abc"), |id| id == "abc", || "new".into(), None, None);
        assert_eq!(l.command, "claude --dangerously-skip-permissions --settings '/data dir/claude/settings.json' --resume 'abc'");
        assert_eq!(l.session_id, "abc");
        let l = claude_launch(Some(&hooks()), "key-1", Some("gone"), |_| false, || "new".into(), None, None);
        assert_eq!(l.command, "claude --dangerously-skip-permissions --settings '/data dir/claude/settings.json' --session-id 'new'");
        assert_eq!(l.session_id, "new");
        let l = claude_launch(Some(&hooks()), "key-1", None, |_| true, || "fresh".into(), None, None);
        assert!(l.command.ends_with("--session-id 'fresh'"));
    }

    #[test]
    fn launch_env_and_no_hooks() {
        let l = claude_launch(Some(&hooks()), "key-1", None, |_| false, || "n".into(), Some("my-status".into()), None);
        assert_eq!(
            l.env,
            vec![
                ("SUPERZED_NODE".to_string(), "key-1".to_string()),
                ("SUPERZED_HOOK_SOCK".to_string(), "/data dir/superzed.sock".to_string()),
                ("SUPERZED_HOOK_TOKEN".to_string(), "tok".to_string()),
                ("SUPERZED_NOTIFY".to_string(), "/data dir/claude/bin/superzed-notify".to_string()),
                ("SUPERZED_USER_STATUSLINE".to_string(), "my-status".to_string()),
            ]
        );
        let l = claude_launch(None, "key-1", Some("abc"), |_| true, || "n".into(), Some("x".into()), None);
        assert_eq!(l.command, "claude --dangerously-skip-permissions --resume 'abc'", "senza ipc: niente --settings, il resume resta");
        assert!(l.env.is_empty());
    }

    #[test]
    fn launch_puts_the_notify_script_on_path() {
        let l = claude_launch(Some(&hooks()), "k", None, |_| false, || "n".into(), None, None);
        assert_eq!(l.path_prefix.as_deref(), Some(Path::new("/data dir/claude/bin")));
        let l = claude_launch(None, "k", None, |_| false, || "n".into(), None, None);
        assert_eq!(l.path_prefix, None);
    }

    #[test]
    fn initial_prompt_only_for_new_sessions() {
        let l = claude_launch(Some(&hooks()), "k", None, |_| false, || "n".into(), None, Some("fai 'x'"));
        assert!(l.command.ends_with("--session-id 'n' -- 'fai '\\''x'\\'''"), "{}", l.command);
        let l = claude_launch(Some(&hooks()), "k", Some("abc"), |_| true, || "n".into(), None, Some("fai"));
        assert!(!l.command.contains(" -- "), "resume: niente prompt");
    }

    #[test]
    fn launch_quotes_paths() {
        let mut h = hooks();
        h.settings = "/it's/settings.json".into();
        let l = claude_launch(Some(&h), "k", None, |_| false, || "n".into(), None, None);
        assert!(l.command.starts_with("claude --dangerously-skip-permissions --settings '/it'\\''s/settings.json' "));
    }

    // Verifica manuale 2026-09-26: superzed lanciato da una sessione Claude Code
    // ereditava CLAUDE_CODE_CHILD_SESSION & co.: i claude dei nodi non salvavano il
    // transcript ("Transcript saving is off") e il resume non poteva funzionare.
    #[test]
    fn inherited_claude_session_vars_are_selected() {
        let names = [
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_PID",
            "CLAUDE_EFFORT",
            "CLAUDE_CONFIG_DIR",
            "PATH",
            "HOME",
            "CLAUDEX",
        ];
        let picked = inherited_claude_session_vars(names.iter().map(|s| s.to_string()));
        assert_eq!(
            picked,
            vec![
                "CLAUDECODE",
                "CLAUDE_CODE_CHILD_SESSION",
                "CLAUDE_CODE_SESSION_ID",
                "CLAUDE_CODE_MESSAGING_SOCKET",
                "CLAUDE_PID",
                "CLAUDE_EFFORT",
            ]
        );
    }

    #[test]
    fn node_script_unsets_nodeterm_and_env_credentials() {
        let unset = unset_inherited();
        assert!(unset.starts_with("unset CLAUDE_CONFIG_DIR CODEX_HOME "));
        for var in NODETERM_VARS.iter().chain(CLAUDE_AUTH_VARS) {
            assert!(unset.contains(var), "{var}");
        }
        assert_eq!(NODETERM_VARS.len(), 7);
        assert_eq!(CLAUDE_AUTH_VARS, &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"]);
    }

    #[test]
    fn launch_has_no_config_dir_of_its_own() {
        let l = claude_launch(Some(&hooks()), "k", None, |_| false, || "n".into(), None, None);
        assert_eq!(l.config_dir, None, "lo decide il canvas in base all'account");
    }

    #[test]
    fn account_label_from_claude_json() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(account_label(None, home.path()), "system", "file mancante");
        std::fs::write(home.path().join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"j.doe@example.com"}}"#).unwrap();
        assert_eq!(account_label(None, home.path()), "j.doe");
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join(".claude.json"), r#"{"oauthAccount":{}}"#).unwrap();
        assert_eq!(account_label(Some(other.path()), home.path()), "system", "email assente");
        std::fs::write(other.path().join(".claude.json"), r#"{"oauthAccount":{"emailAddress":"x@y.z"}}"#).unwrap();
        assert_eq!(account_label(Some(other.path()), home.path()), "x");
    }

    #[test]
    fn user_statusline_and_transcript() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(user_statusline(dir.path()), None);
        std::fs::write(dir.path().join("settings.json"), r#"{"statusLine":{"type":"command","command":"~/sl.sh"}}"#).unwrap();
        assert_eq!(user_statusline(dir.path()).as_deref(), Some("~/sl.sh"));
        assert!(!transcript_exists(dir.path(), "abc"));
        std::fs::create_dir_all(dir.path().join("projects/-repo")).unwrap();
        std::fs::write(dir.path().join("projects/-repo/abc.jsonl"), "{}").unwrap();
        assert!(transcript_exists(dir.path(), "abc"));
        assert!(!transcript_exists(dir.path(), "../abc"), "id non valido");
        assert_eq!(system_config_dir(Path::new("/Users/u")), PathBuf::from("/Users/u/.claude"));
    }
}
