//! Script e settings degli hook di claude, scritti nella data dir di superzed e
//! passati a claude con `--settings` (nessun file dell'utente viene toccato).
use crate::launch::shell_quote;
use anyhow::Context as _;
use std::path::{Path, PathBuf};

pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "PermissionRequest",
    "PermissionDenied",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

const HOOK_SH: &str = r#"#!/bin/sh
# superzed: inoltra il JSON di Claude Code al socket di superzed ($1 = canale)
if [ -z "${SUPERZED_NODE-}" ] || [ ! -S "${SUPERZED_HOOK_SOCK-}" ]; then
  cat >/dev/null 2>&1
  exit 0
fi
{ printf 'superzed1 %s %s %s\n' "$1" "$SUPERZED_NODE" "$SUPERZED_HOOK_TOKEN"; cat; } \
  | /usr/bin/nc -U -w 2 "$SUPERZED_HOOK_SOCK" >/dev/null 2>&1
exit 0
"#;

const STATUSLINE_SH: &str = r#"#!/bin/sh
# superzed: status line di claude -> socket di superzed, poi quella dell'utente
input=$(cat)
printf '%s' "$input" | sh "$(dirname "$0")/hook.sh" status
if [ -n "${SUPERZED_USER_STATUSLINE-}" ]; then
  printf '%s' "$input" | sh -c "$SUPERZED_USER_STATUSLINE"
fi
exit 0
"#;

/// Nome dello script delle notifiche, in `<dir>/bin` (cartella messa nel `PATH` di claude).
pub const NOTIFY_SCRIPT: &str = "superzed-notify";

const NOTIFY_SH: &str = r#"#!/bin/sh
# superzed-notify: manda una notifica a superzed (JSON su stdin, oppure argomenti)
usage() {
  echo "usage: superzed-notify < notification.json" >&2
  echo "       superzed-notify --severity note|info|warning|error|critical --title T [--summary S] [--details-file F] [--suggested-prompt P]" >&2
}
if [ -z "${SUPERZED_NODE-}" ] || [ -z "${SUPERZED_HOOK_TOKEN-}" ] || [ ! -S "${SUPERZED_HOOK_SOCK-}" ]; then
  echo "superzed-notify: superzed not reachable (SUPERZED_NODE, the token or the socket is missing)" >&2
  exit 1
fi
# tutto a byte: in un locale UTF-8 sed rifiuta i byte non validi e non stampa niente
LC_ALL=C
export LC_ALL
# stdin -> stringa JSON: prima UTF-8 valido (via byte non validi e un carattere spezzato da
# head -c), poi via i caratteri di controllo (anche CR; restano tab e a capo), escape di \ " tab
json_str() {
  /usr/bin/iconv -f UTF-8 -t UTF-8 -c 2>/dev/null \
    | /usr/bin/tr -d '\000-\010\013-\037' \
    | /usr/bin/sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e "s/$(printf '\t')/\\\\t/g" \
    | /usr/bin/awk 'BEGIN { printf "\"" } { if (NR > 1) printf "%s", "\\n"; printf "%s", $0 } END { printf "\"" }'
}
if [ $# -eq 0 ]; then
  if [ -t 0 ]; then usage; exit 1; fi
  # un byte oltre il limite per accorgersi del troppo (niente JSON troncato)
  body=$(/usr/bin/head -c 4194305)
  if [ -z "$body" ]; then echo "superzed-notify: empty notification" >&2; usage; exit 1; fi
  if [ ${#body} -gt 4194304 ]; then echo "superzed-notify: notification too long (max 4 MiB)" >&2; exit 1; fi
else
  sev=info title="" summary="" details="" prompt=""
  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help) usage; exit 0 ;;
      --severity|--title|--summary|--details-file|--suggested-prompt)
        if [ $# -lt 2 ]; then echo "superzed-notify: missing value for $1" >&2; exit 1; fi
        case "$1" in
          --severity) sev=$2 ;;
          --title) title=$2 ;;
          --summary) summary=$2 ;;
          --details-file)
            if [ ! -f "$2" ] || [ ! -r "$2" ]; then echo "superzed-notify: file not readable: $2" >&2; exit 1; fi
            details=$2 ;;
          --suggested-prompt) prompt=$2 ;;
        esac
        shift 2 ;;
      *) echo "superzed-notify: unknown argument: $1" >&2; usage; exit 1 ;;
    esac
  done
  body="{\"severity\":$(printf '%s' "$sev" | json_str),\"title\":$(printf '%s' "$title" | json_str),\"summary\":$(printf '%s' "$summary" | json_str)"
  if [ -n "$details" ]; then body="$body,\"details\":$(/usr/bin/head -c 1048577 "$details" | json_str)"; fi
  if [ -n "$prompt" ]; then body="$body,\"suggested_prompt\":$(printf '%s' "$prompt" | json_str)"; fi
  body="$body}"
fi
if printf 'superzed1 notify %s %s\n%s' "$SUPERZED_NODE" "$SUPERZED_HOOK_TOKEN" "$body" \
  | /usr/bin/nc -U -w 2 "$SUPERZED_HOOK_SOCK" >/dev/null 2>&1; then
  echo "notification sent"
else
  echo "superzed-notify: sending failed" >&2
  exit 1
fi
"#;

pub fn hook_script() -> &'static str {
    HOOK_SH
}

pub fn statusline_script() -> &'static str {
    STATUSLINE_SH
}

pub fn notify_script() -> &'static str {
    NOTIFY_SH
}

pub fn settings_json(dir: &Path) -> serde_json::Value {
    let dir = dir.to_string_lossy();
    let hook = format!("sh {} hook", shell_quote(&format!("{dir}/hook.sh")));
    let mut hooks = serde_json::Map::new();
    for event in HOOK_EVENTS {
        hooks.insert(
            event.to_string(),
            serde_json::json!([{ "hooks": [{ "type": "command", "command": hook }] }]),
        );
    }
    serde_json::json!({
        "hooks": hooks,
        "statusLine": {
            "type": "command",
            "command": format!("sh {}", shell_quote(&format!("{dir}/statusline.sh"))),
        },
    })
}

fn write_if_changed(path: &Path, content: &str) -> anyhow::Result<()> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(());
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
}

/// Scrive i tre file in `dir` e `dir/bin/superzed-notify`; ritorna il path di `settings.json`.
pub fn write_hook_files(dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    write_if_changed(&dir.join("hook.sh"), HOOK_SH)?;
    write_if_changed(&dir.join("statusline.sh"), STATUSLINE_SH)?;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    let notify = bin.join(NOTIFY_SCRIPT);
    write_if_changed(&notify, NOTIFY_SH)?;
    // sempre, anche se il contenuto non e' cambiato: claude lo esegue per nome dal PATH
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&notify, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("permissions of {}", notify.display()))?;
    let settings = dir.join("settings.json");
    write_if_changed(&settings, &serde_json::to_string_pretty(&settings_json(dir))?)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use std::io::Write as _;
    use superzed_ipc::{Channel, IpcServer};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[allow(clippy::disallowed_methods, reason="test sincrono su un processo reale: non gira sull'executor di gpui")]
    fn run(script: &Path, arg: Option<&str>, env: &[(&str, &str)], stdin: &str) -> std::process::Output {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(script);
        if let Some(a) = arg {
            cmd.arg(a);
        }
        cmd.env_clear().env("PATH", "/usr/bin:/bin");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    #[allow(clippy::disallowed_methods, reason="test sincrono su un processo reale: non gira sull'executor di gpui")]
    fn run_args(script: &Path, args: &[&str], env: &[(&str, &str)], stdin: &str) -> std::process::Output {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(script).args(args);
        cmd.env_clear().env("PATH", "/usr/bin:/bin");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        // lo script puo' uscire prima di leggere tutto (errori, limite): pipe chiusa ammessa
        let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
        child.wait_with_output().unwrap()
    }

    #[allow(clippy::disallowed_methods, reason="timeout di un test su socket reali: fuori dall'executor di gpui")]
    fn recv(rx: &smol::channel::Receiver<superzed_ipc::IpcMessage>) -> Option<superzed_ipc::IpcMessage> {
        smol::block_on(smol::future::or(async { rx.recv().await.ok() }, async {
            smol::Timer::after(Duration::from_secs(3)).await;
            None
        }))
    }

    #[test]
    fn settings_json_has_every_event_and_status_line() {
        let v = settings_json(Path::new("/data/claude"));
        for event in HOOK_EVENTS {
            let cmd = v["hooks"][event][0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, "sh '/data/claude/hook.sh' hook", "{event}");
            assert_eq!(v["hooks"][event][0]["hooks"][0]["type"], "command");
        }
        assert_eq!(HOOK_EVENTS.len(), 11);
        assert_eq!(v["statusLine"]["type"], "command");
        assert_eq!(v["statusLine"]["command"], "sh '/data/claude/statusline.sh'");
    }

    #[test]
    fn settings_json_quotes_paths_with_spaces() {
        let v = settings_json(Path::new("/Users/u/Library/Application Support/it's"));
        assert_eq!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"],
            "sh '/Users/u/Library/Application Support/it'\\''s/hook.sh' hook"
        );
    }

    #[test]
    fn write_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let settings = write_hook_files(dir.path()).unwrap();
        assert_eq!(settings, dir.path().join("settings.json"));
        let mtime = std::fs::metadata(dir.path().join("hook.sh")).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        write_hook_files(dir.path()).unwrap();
        assert_eq!(std::fs::metadata(dir.path().join("hook.sh")).unwrap().modified().unwrap(), mtime);
        let parsed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(parsed, settings_json(dir.path()));
    }

    #[test]
    fn hook_script_delivers_to_socket() {
        let dir = tempfile::tempdir().unwrap();
        write_hook_files(dir.path()).unwrap();
        let sock = dir.path().join("s.sock");
        let (_server, rx) = IpcServer::start(sock.clone(), TOKEN.into()).unwrap();
        let out = run(
            &dir.path().join("hook.sh"),
            Some("hook"),
            &[("SUPERZED_NODE", "node-7"), ("SUPERZED_HOOK_SOCK", sock.to_str().unwrap()), ("SUPERZED_HOOK_TOKEN", TOKEN)],
            r#"{"hook_event_name":"Stop"}"#,
        );
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        let msg = recv(&rx).expect("messaggio");
        assert_eq!((msg.channel, msg.node_key.as_str()), (Channel::Hook, "node-7"));
        assert_eq!(msg.payload["hook_event_name"], "Stop");
    }

    #[test]
    fn hook_script_without_socket_exits_zero_fast() {
        let dir = tempfile::tempdir().unwrap();
        write_hook_files(dir.path()).unwrap();
        let start = Instant::now();
        let out = run(&dir.path().join("hook.sh"), Some("hook"), &[], "{}");
        assert!(out.status.success() && out.stdout.is_empty());
        let out = run(
            &dir.path().join("hook.sh"),
            Some("hook"),
            &[("SUPERZED_NODE", "n"), ("SUPERZED_HOOK_SOCK", "/nonexistent/s.sock"), ("SUPERZED_HOOK_TOKEN", TOKEN)],
            "{}",
        );
        assert!(out.status.success() && out.stdout.is_empty());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    fn notify_env(sock: &Path) -> [(&'static str, String); 3] {
        [("SUPERZED_NODE", "node-3".into()), ("SUPERZED_HOOK_SOCK", sock.to_str().unwrap().into()), ("SUPERZED_HOOK_TOKEN", TOKEN.into())]
    }

    #[test]
    fn notify_script_is_executable_and_sends_stdin_json() {
        let dir = tempfile::tempdir().unwrap();
        write_hook_files(dir.path()).unwrap();
        let script = dir.path().join("bin").join(NOTIFY_SCRIPT);
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o111, 0o111, "eseguibile");
        let sock = dir.path().join("s.sock");
        let (_server, rx) = IpcServer::start(sock.clone(), TOKEN.into()).unwrap();
        let env = notify_env(&sock);
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let out = run_args(&script, &[], &env, r#"{"severity":"errore","title":"DB giu'"}"#);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "notification sent");
        let msg = recv(&rx).expect("notifica");
        assert_eq!((msg.channel, msg.node_key.as_str()), (Channel::Notify, "node-3"));
        assert_eq!(msg.payload["title"], "DB giu'");
    }

    #[test]
    fn notify_script_builds_json_from_arguments_with_awkward_text() {
        let dir = tempfile::tempdir().unwrap();
        write_hook_files(dir.path()).unwrap();
        let script = dir.path().join("bin").join(NOTIFY_SCRIPT);
        let details = dir.path().join("d.md");
        std::fs::write(&details, "riga 1\n\t\"citata\" \\ back\x01\nriga 3\n").unwrap();
        let sock = dir.path().join("s.sock");
        let (_server, rx) = IpcServer::start(sock.clone(), TOKEN.into()).unwrap();
        let env = notify_env(&sock);
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let out = run_args(
            &script,
            &["--severity", "critico", "--title", "a \"b\" \\ c", "--summary", "s", "--details-file",
              details.to_str().unwrap(), "--suggested-prompt", "p"],
            &env,
            "",
        );
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let msg = recv(&rx).expect("notifica");
        assert_eq!(msg.payload["severity"], "critico");
        assert_eq!(msg.payload["title"], "a \"b\" \\ c");
        assert_eq!(msg.payload["summary"], "s");
        assert_eq!(msg.payload["details"], "riga 1\n\t\"citata\" \\ back\nriga 3", "controllo tolto, a capo e tab conservati");
        assert_eq!(msg.payload["suggested_prompt"], "p");
    }

    #[test]
    fn notify_script_errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        write_hook_files(dir.path()).unwrap();
        let script = dir.path().join("bin").join(NOTIFY_SCRIPT);
        let out = run_args(&script, &["--title", "x"], &[], "");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("superzed not reachable"));
        let sock = dir.path().join("s.sock");
        let (_server, _rx) = IpcServer::start(sock.clone(), TOKEN.into()).unwrap();
        let env = notify_env(&sock);
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let out = run_args(&script, &["--boh"], &env, "");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("unknown argument"));
        let out = run_args(&script, &["--details-file", "/non/esiste"], &env, "");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("file not readable"));
        let out = run_args(&script, &["--details-file", dir.path().to_str().unwrap()], &env, "");
        assert!(!out.status.success(), "una cartella non e' un file di dettagli");
        assert!(String::from_utf8_lossy(&out.stderr).contains("file not readable"));
        let out = run_args(&script, &[], &env, "");
        assert!(!out.status.success(), "stdin vuoto");
        assert!(String::from_utf8_lossy(&out.stderr).contains("empty notification"));
        let out = run_args(&script, &[], &env, &format!("{{\"title\":\"{}\"}}", "x".repeat(4 << 20)));
        assert!(!out.status.success(), "oltre 4 MiB non si manda JSON troncato");
        assert!(String::from_utf8_lossy(&out.stderr).contains("too long"));
        let no_token = [env[0], env[1]];
        let out = run_args(&script, &["--title", "x"], &no_token, "");
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("superzed not reachable"));
    }

    // Review Task 4: in un locale UTF-8 vero `sed` rifiutava i byte non validi (output vuoto,
    // dettagli persi ma "notification sent"); ora si ripulisce a UTF-8 valido e si lavora a byte.
    #[test]
    fn notify_script_survives_invalid_utf8_in_a_utf8_locale() {
        let dir = tempfile::tempdir().unwrap();
        write_hook_files(dir.path()).unwrap();
        let script = dir.path().join("bin").join(NOTIFY_SCRIPT);
        let details = dir.path().join("d.md");
        std::fs::write(&details, b"inizio \xff met\xc3\xa0\nfine \xc3").unwrap();
        let sock = dir.path().join("s.sock");
        let (_server, rx) = IpcServer::start(sock.clone(), TOKEN.into()).unwrap();
        let env = notify_env(&sock);
        let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        env.extend([("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.UTF-8")]);
        let out = run_args(&script, &["--title", "perché \"no\"", "--details-file", details.to_str().unwrap()], &env, "");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let msg = recv(&rx).expect("notifica");
        assert_eq!(msg.payload["title"], "perché \"no\"");
        assert_eq!(msg.payload["details"], "inizio  metà\nfine ", "byte non validi tolti, il resto resta");
        // taglio di `head -c` a meta' di un carattere multibyte
        let mut big = "x".repeat(1 << 20).into_bytes();
        big.extend("è dopo".as_bytes());
        std::fs::write(&details, &big).unwrap();
        let out = run_args(&script, &["--title", "t", "--details-file", details.to_str().unwrap()], &env, "");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let msg = recv(&rx).expect("notifica grande");
        assert_eq!(msg.payload["title"], "t");
        assert_eq!(msg.payload["details"].as_str().map(str::len), Some(1 << 20), "carattere spezzato tolto");
    }

    #[test]
    fn statusline_forwards_and_runs_user_statusline() {
        let dir = tempfile::tempdir().unwrap();
        write_hook_files(dir.path()).unwrap();
        let sock = dir.path().join("s.sock");
        let (_server, rx) = IpcServer::start(sock.clone(), TOKEN.into()).unwrap();
        let env = [("SUPERZED_NODE", "node-8"), ("SUPERZED_HOOK_SOCK", sock.to_str().unwrap()), ("SUPERZED_HOOK_TOKEN", TOKEN)];
        let input = r#"{"context_window":{"remaining_percentage":50}}"#;
        let out = run(&dir.path().join("statusline.sh"), None, &env, input);
        assert!(out.status.success());
        assert!(out.stdout.is_empty(), "senza status line utente non stampa nulla");
        let msg = recv(&rx).expect("status");
        assert_eq!(msg.channel, Channel::Status);
        assert_eq!(msg.payload["context_window"]["remaining_percentage"], 50);
        let mut env2 = env.to_vec();
        env2.push(("SUPERZED_USER_STATUSLINE", "cat"));
        let out = run(&dir.path().join("statusline.sh"), None, &env2, input);
        assert_eq!(String::from_utf8_lossy(&out.stdout), input, "la status line utente riceve lo stesso stdin");
        assert!(recv(&rx).is_some());
    }
}
