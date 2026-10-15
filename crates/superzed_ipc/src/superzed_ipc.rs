//! Socket Unix locale di superzed: riceve dagli script di claude un header
//! (`superzed1 <canale> <chiave> <token>`) e un corpo. Tre canali: `hook` e `status`
//! (il JSON di Claude Code, fino a `MAX_PAYLOAD`) e `notify` (le notifiche di
//! `superzed-notify`, fino a `MAX_NOTIFY_PAYLOAD`; un corpo non JSON arriva come testo).
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const MAX_PAYLOAD: usize = 1 << 20;
/// Limite del corpo del canale `notify` (i dettagli possono essere lunghi).
pub const MAX_NOTIFY_PAYLOAD: usize = 4 << 20;
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// `sun_path` di macOS e' 104 byte: si resta sotto con margine.
const MAX_SOCKET_PATH: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Hook,
    Status,
    Notify,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IpcMessage {
    pub channel: Channel,
    pub node_key: String,
    pub payload: serde_json::Value,
}

fn valid_word(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn parse_header(line: &str, token: &str) -> Option<(Channel, String)> {
    let mut parts = line.trim_end_matches(['\r', '\n']).split(' ');
    let (Some("superzed1"), Some(channel), Some(key), Some(tok), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let channel = match channel {
        "hook" => Channel::Hook,
        "status" => Channel::Status,
        "notify" => Channel::Notify,
        _ => return None,
    };
    (valid_word(key) && valid_word(tok) && tok == token).then(|| (channel, key.to_string()))
}

/// `<data_dir>/superzed.sock`, oppure un nome stabile in `tmp_dir` se il path e' troppo lungo.
pub fn socket_path(data_dir: &Path, tmp_dir: &Path) -> PathBuf {
    let preferred = data_dir.join("superzed.sock");
    if preferred.as_os_str().len() <= MAX_SOCKET_PATH {
        return preferred;
    }
    // FNV-1a: stabile tra un avvio e l'altro, senza dipendenze
    let mut hash: u32 = 0x811c9dc5;
    for b in data_dir.as_os_str().as_encoded_bytes() {
        hash = (hash ^ u32::from(*b)).wrapping_mul(0x01000193);
    }
    tmp_dir.join(format!("superzed-{hash:08x}.sock"))
}

pub struct IpcServer {
    pub path: PathBuf,
}

impl IpcServer {
    pub fn start(
        path: PathBuf,
        token: String,
    ) -> std::io::Result<(Self, smol::channel::Receiver<IpcMessage>)> {
        // l'istanza singola e' garantita altrove: un socket rimasto e' di un avvio morto
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&path)?;
        use std::os::unix::fs::PermissionsExt as _;
        // Fix wave finale (finding 4): un errore qui non deve lasciare in giro il file del
        // socket appena creato da `bind` (il prossimo `start` lo rimuoverebbe comunque, ma
        // chi chiama qui riceve un Err e non un IpcServer che lo pulirebbe col Drop).
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            drop(listener);
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        let (tx, rx) = smol::channel::unbounded();
        std::thread::Builder::new().name("superzed-ipc".into()).spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                match read_message(stream, &token) {
                    Some(msg) => {
                        if tx.send_blocking(msg).is_err() {
                            break; // nessuno ascolta piu'
                        }
                    }
                    None => log::debug!("superzed ipc: message dropped"),
                }
            }
        })?;
        Ok((Self { path }, rx))
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_message(stream: UnixStream, token: &str) -> Option<IpcMessage> {
    // on macOS setsockopt fails with EINVAL when peer has closed; data is buffered, reading proceeds without timeout
    if let Err(e) = stream.set_read_timeout(Some(READ_TIMEOUT)) {
        log::debug!("superzed ipc: read timeout not set: {e}");
    }
    let mut reader = BufReader::new(stream.take(MAX_NOTIFY_PAYLOAD as u64 + 257));
    let mut header = String::new();
    reader.read_line(&mut header).ok()?;
    if !header.ends_with('\n') {
        return None;
    }
    let (channel, node_key) = parse_header(&header, token)?;
    let mut body = Vec::new();
    reader.read_to_end(&mut body).ok()?;
    let limit = match channel {
        Channel::Notify => MAX_NOTIFY_PAYLOAD,
        Channel::Hook | Channel::Status => MAX_PAYLOAD,
    };
    if body.len() > limit {
        return None;
    }
    let payload = if channel == Channel::Notify {
        // lossy: un byte non valido o un carattere spezzato non fanno cadere il JSON;
        // una notifica in testo semplice resta valida (la interpreta il destinatario)
        let text = String::from_utf8_lossy(&body);
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::Value::String(text.trim().to_string()))
    } else {
        serde_json::from_slice(&body).ok()?
    };
    Some(IpcMessage { channel, node_key, payload })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn header_parsing() {
        assert_eq!(parse_header(&format!("superzed1 hook k-1 {TOKEN}"), TOKEN), Some((Channel::Hook, "k-1".into())));
        assert_eq!(parse_header(&format!("superzed1 status k {TOKEN}"), TOKEN), Some((Channel::Status, "k".into())));
        assert_eq!(parse_header(&format!("superzed1 other k {TOKEN}"), TOKEN), None, "canale");
        assert_eq!(parse_header("superzed1 hook k wrong", TOKEN), None, "token");
        assert_eq!(parse_header(&format!("superzed2 hook k {TOKEN}"), TOKEN), None, "versione");
        assert_eq!(parse_header(&format!("superzed1 hook a/b {TOKEN}"), TOKEN), None, "chiave");
        assert_eq!(parse_header(&format!("superzed1 hook {} {TOKEN}", "k".repeat(65)), TOKEN), None);
        assert_eq!(parse_header(&format!("superzed1 hook k {TOKEN} extra"), TOKEN), None);
    }

    #[test]
    fn socket_path_falls_back_when_too_long() {
        let short = socket_path(Path::new("/tmp/sz"), Path::new("/tmp"));
        assert_eq!(short, PathBuf::from("/tmp/sz/superzed.sock"));
        let long_dir = PathBuf::from(format!("/tmp/{}", "d".repeat(120)));
        let fallback = socket_path(&long_dir, Path::new("/var/tmp"));
        assert!(fallback.to_string_lossy().starts_with("/var/tmp/superzed-"));
        assert!(fallback.as_os_str().len() <= 100);
        assert_eq!(fallback, socket_path(&long_dir, Path::new("/var/tmp")), "stabile");
    }

    fn send(path: &Path, bytes: &[u8]) {
        let mut s = UnixStream::connect(path).unwrap();
        s.write_all(bytes).unwrap();
    }

    #[allow(clippy::disallowed_methods, reason="timeout di un test su socket reali: il crate non usa l'executor di gpui")]
    fn recv(rx: &smol::channel::Receiver<IpcMessage>) -> Option<IpcMessage> {
        smol::block_on(async {
            smol::future::or(async { rx.recv().await.ok() }, async {
                smol::Timer::after(Duration::from_secs(2)).await;
                None
            })
            .await
        })
    }

    #[test]
    fn server_delivers_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let (_server, rx) = IpcServer::start(path.clone(), TOKEN.into()).unwrap();
        send(&path, format!("superzed1 hook node-1 {TOKEN}\n{{\"hook_event_name\":\"Stop\"}}\n").as_bytes());
        let msg = recv(&rx).expect("messaggio");
        assert_eq!(msg.channel, Channel::Hook);
        assert_eq!(msg.node_key, "node-1");
        assert_eq!(msg.payload["hook_event_name"], "Stop");
    }

    #[test]
    fn wrong_token_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let (_server, rx) = IpcServer::start(path.clone(), TOKEN.into()).unwrap();
        send(&path, b"superzed1 hook node-1 nope\n{}\n");
        send(&path, b"garbage without newline");
        send(&path, format!("superzed1 hook node-1 {TOKEN}\nnot json").as_bytes());
        send(&path, format!("superzed1 status node-2 {TOKEN}\n{{}}").as_bytes());
        let msg = recv(&rx).expect("solo l'ultimo passa");
        assert_eq!(msg.node_key, "node-2");
        assert_eq!(msg.channel, Channel::Status);
    }

    #[test]
    fn stale_socket_is_replaced_and_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        std::fs::write(&path, b"stale").unwrap();
        let (server, _rx) = IpcServer::start(path.clone(), TOKEN.into()).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        drop(server);
        assert!(!path.exists());
    }

    #[test]
    fn notify_header() {
        assert_eq!(parse_header(&format!("superzed1 notify k-2 {TOKEN}"), TOKEN), Some((Channel::Notify, "k-2".into())));
    }

    #[test]
    fn notify_body_that_is_not_json_is_kept_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let (_server, rx) = IpcServer::start(path.clone(), TOKEN.into()).unwrap();
        send(&path, format!("superzed1 notify node-1 {TOKEN}\ndisco pieno").as_bytes());
        let msg = recv(&rx).expect("notifica");
        assert_eq!(msg.channel, Channel::Notify);
        assert_eq!(msg.payload, serde_json::Value::String("disco pieno".into()));
    }

    // Review Task 4: un carattere spezzato o un byte non valido non deve far cadere il JSON.
    #[test]
    fn notify_body_with_invalid_utf8_is_still_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let (_server, rx) = IpcServer::start(path.clone(), TOKEN.into()).unwrap();
        let mut bytes = format!("superzed1 notify node-1 {TOKEN}\n").into_bytes();
        bytes.extend_from_slice(b"{\"title\":\"a\xff b\",\"details\":\"x\xc3\"}");
        send(&path, &bytes);
        let msg = recv(&rx).expect("notifica");
        assert_eq!(msg.payload["title"], "a\u{fffd} b");
        assert_eq!(msg.payload["details"], "x\u{fffd}");
    }

    #[test]
    fn notify_accepts_bodies_over_the_hook_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let (_server, rx) = IpcServer::start(path.clone(), TOKEN.into()).unwrap();
        let big = format!("{{\"details\":\"{}\"}}", "x".repeat(MAX_PAYLOAD + 10));
        send(&path, format!("superzed1 hook node-1 {TOKEN}\n{big}").as_bytes());
        send(&path, format!("superzed1 notify node-2 {TOKEN}\n{big}").as_bytes());
        let msg = recv(&rx).expect("solo la notifica passa");
        assert_eq!((msg.channel, msg.node_key.as_str()), (Channel::Notify, "node-2"));
    }
}
