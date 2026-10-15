//! Token OAuth di claude dal Keychain di macOS. Il token resta in memoria: mai in
//! log, errori o `Debug`.
use crate::Account;
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use sha2::{Digest as _, Sha256};
use std::path::Path;
use unicode_normalization::UnicodeNormalization as _;

pub const SYSTEM_SERVICE: &str = "Claude Code-credentials";

/// Servizio del Keychain in cui claude salva le credenziali di una config dir:
/// `SYSTEM_SERVICE-<primi 8 hex di sha256(path NFC senza / finale)>`.
pub fn keychain_service(config_dir: Option<&Path>) -> String {
    let Some(dir) = config_dir else {
        return SYSTEM_SERVICE.to_string();
    };
    let raw = dir.to_string_lossy();
    let trimmed = if raw.len() > 1 { raw.trim_end_matches('/') } else { &*raw };
    let nfc: String = trimmed.nfc().collect();
    let digest = Sha256::digest(nfc.as_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{SYSTEM_SERVICE}-{hex}")
}

#[derive(Clone, PartialEq)]
pub struct OauthToken {
    pub access_token: String,
    pub expires_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for OauthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OauthToken")
            .field("access_token", &"<hidden>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// `{claudeAiOauth: {accessToken, expiresAt (ms)}}`.
pub fn parse_credentials(json: &str) -> Option<OauthToken> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let access_token = oauth.get("accessToken")?.as_str().filter(|t| !t.is_empty())?.to_string();
    let expires_at = oauth
        .get("expiresAt")
        .and_then(|e| e.as_i64())
        .and_then(DateTime::from_timestamp_millis);
    Some(OauthToken { access_token, expires_at })
}

/// Da dove arrivano i token (nei test: `test_support::FakeTokens`). Bloccante.
pub trait TokenSource: Send + Sync + 'static {
    /// `Ok(None)`: nessuna credenziale per l'account.
    fn token(&self, account: &Account) -> anyhow::Result<Option<OauthToken>>;

    /// `true`: `token` puo' restare bloccato a lungo (un dialogo del Portachiavi) e gira
    /// su un thread dedicato, non su un worker dell'executor. Le sorgenti in memoria
    /// dei test tornano `false` e restano deterministiche.
    fn needs_thread(&self) -> bool {
        true
    }
}

pub struct KeychainTokenSource;

impl TokenSource for KeychainTokenSource {
    fn token(&self, account: &Account) -> anyhow::Result<Option<OauthToken>> {
        let service = keychain_service(account.config_dir.as_deref());
        let user = std::env::var("USER").ok();
        read_token(user.as_deref(), &service, |args| {
            #[allow(clippy::disallowed_methods, reason = "gira sul thread dedicato del portachiavi (needs_thread), fuori dall'executor di gpui")]
            let out = std::process::Command::new("/usr/bin/security")
                .args(args)
                .stdin(std::process::Stdio::null())
                .output()?;
            Ok((out.status.code(), out.stdout))
        })
    }
}

/// `security find-generic-password -a <user> -s <service> -w` attraverso `run`
/// (codice di uscita, stdout). Senza `$USER` non si cerca: nessun token.
/// 0 → credenziali, 44 → voce assente; altro → errore con il solo codice (mai l'output).
pub(crate) fn read_token(
    user: Option<&str>,
    service: &str,
    run: impl FnOnce(&[&str]) -> std::io::Result<(Option<i32>, Vec<u8>)>,
) -> anyhow::Result<Option<OauthToken>> {
    let Some(user) = user.filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    let (code, stdout) = run(&["find-generic-password", "-a", user, "-s", service, "-w"]).context("security did not start")?;
    match code {
        Some(0) => Ok(parse_credentials(&String::from_utf8_lossy(&stdout))),
        Some(44) => Ok(None),
        Some(code) => anyhow::bail!("security: exit code {code}"),
        None => anyhow::bail!("security killed by a signal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn service_names() {
        assert_eq!(keychain_service(None), "Claude Code-credentials");
        assert_eq!(
            keychain_service(Some(Path::new("/Users/u/Library/Application Support/node-terminal/claude-accounts/abc"))),
            "Claude Code-credentials-5632beab"
        );
    }

    // Review Focus 2
    #[test]
    fn service_hash_is_nfc_and_ignores_the_trailing_slash() {
        let nfc = keychain_service(Some(Path::new("/tmp/caff\u{e8}")));
        assert_eq!(nfc, "Claude Code-credentials-f68b4125");
        assert_eq!(keychain_service(Some(Path::new("/tmp/caffe\u{300}"))), nfc, "NFD → NFC");
        assert_eq!(keychain_service(Some(Path::new("/tmp/caff\u{e8}/"))), nfc, "senza / finale");
    }

    #[test]
    fn credentials_json() {
        let t = parse_credentials(r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-1","expiresAt":1790424600000,"scopes":[]}}"#).unwrap();
        assert_eq!(t.access_token, "sk-ant-oat-1");
        assert_eq!(t.expires_at, DateTime::from_timestamp_millis(1790424600000));
        assert!(parse_credentials("{\"claudeAiOauth\":{\"accessToken\":\"\"}}\n").is_none());
        assert!(parse_credentials("non json").is_none());
        assert_eq!(parse_credentials(r#"{"claudeAiOauth":{"accessToken":"x"}}"#).unwrap().expires_at, None);
    }

    // Nessun `security` vero: il comando e' sostituito da una closure.
    #[test]
    fn keychain_exit_codes_and_missing_user() {
        let json = br#"{"claudeAiOauth":{"accessToken":"sk-ant-SEGRETO"}}"#.to_vec();
        let never = |_: &[&str]| -> std::io::Result<(Option<i32>, Vec<u8>)> { panic!("senza $USER non si lancia security") };
        assert!(read_token(None, "svc", never).unwrap().is_none(), "$USER mancante: nessun token");
        let args = std::cell::RefCell::new(Vec::new());
        let ok = read_token(Some("alice"), "svc", |a: &[&str]| {
            *args.borrow_mut() = a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            Ok((Some(0), json.clone()))
        });
        assert_eq!(ok.unwrap().unwrap().access_token, "sk-ant-SEGRETO");
        assert_eq!(*args.borrow(), ["find-generic-password", "-a", "alice", "-s", "svc", "-w"]);
        assert!(read_token(Some("u"), "svc", |_: &[&str]| Ok((Some(44), Vec::new()))).unwrap().is_none(), "44: voce assente");
        let err = read_token(Some("u"), "svc", |_: &[&str]| Ok((Some(51), json.clone()))).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("51") && !text.contains("SEGRETO"), "{text}");
        assert!(read_token(Some("u"), "svc", |_: &[&str]| Ok((None, Vec::new()))).is_err(), "terminato da un segnale");
    }
}
