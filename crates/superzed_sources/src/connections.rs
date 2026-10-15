//! Connessioni globali (`~/.config/superzed/connections.json`): GitHub, tunnel SSH,
//! ClickHouse, HyperDX, Zabbix. Nel file solo i riferimenti ai segreti (Portachiavi).
//! Le voci che questa versione non capisce restano da parte e si riscrivono tali e quali.
use crate::item::SourceKind;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const CONNECTIONS_VERSION: u64 = 1;
pub const DEFAULT_IDLE_MINUTES: u32 = 10;
pub const DEFAULT_DATABASE: &str = "default";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConnectionKind {
    Github,
    SshTunnel,
    Clickhouse,
    Hyperdx,
    Zabbix,
}

impl ConnectionKind {
    pub const ALL: [ConnectionKind; 5] =
        [ConnectionKind::Github, ConnectionKind::Hyperdx, ConnectionKind::Zabbix, ConnectionKind::Clickhouse, ConnectionKind::SshTunnel];

    pub fn key(self) -> &'static str {
        match self {
            ConnectionKind::Github => "github",
            ConnectionKind::SshTunnel => "ssh_tunnel",
            ConnectionKind::Clickhouse => "clickhouse",
            ConnectionKind::Hyperdx => "hyperdx",
            ConnectionKind::Zabbix => "zabbix",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            ConnectionKind::Github => "GitHub",
            ConnectionKind::SshTunnel => "SSH tunnel",
            ConnectionKind::Clickhouse => "ClickHouse",
            ConnectionKind::Hyperdx => "HyperDX",
            ConnectionKind::Zabbix => "Zabbix",
        }
    }

    /// Le connessioni che si aprono come pannello (le altre servono alle fonti).
    pub fn source_kind(self) -> Option<SourceKind> {
        match self {
            ConnectionKind::Github => Some(SourceKind::Github),
            ConnectionKind::Hyperdx => Some(SourceKind::Hyperdx),
            ConnectionKind::Zabbix => Some(SourceKind::Zabbix),
            ConnectionKind::SshTunnel | ConnectionKind::Clickhouse => None,
        }
    }
}

/// Owner, numero e repo tutti vuoti: si legge `.claude/claude-pm.local.md` del progetto.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GithubConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_days: Option<u32>,
}

impl GithubConfig {
    pub fn from_project(&self) -> bool {
        self.owner.is_none() && self.project_number.is_none() && self.repo.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Forward {
    /// 0 = porta libera scelta all'apertura.
    #[serde(default)]
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

fn default_idle() -> u32 {
    DEFAULT_IDLE_MINUTES
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub host: String,
    pub forwards: Vec<Forward>,
    #[serde(default)]
    pub keep_open: bool,
    #[serde(default = "default_idle")]
    pub idle_minutes: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickhouseTransport {
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tunnel: Option<String>,
    },
    /// `ssh <host> <command>` con la query su stdin (ClickHouse non pubblicato sull'host).
    SshExec {
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
}

fn default_database() -> String {
    DEFAULT_DATABASE.to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClickhouseConfig {
    pub transport: ClickhouseTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// URL del Portachiavi (`superzed:<id>`), mai la password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
    #[serde(default = "default_database")]
    pub database: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HyperdxConfig {
    /// Id della connessione ClickHouse.
    pub clickhouse: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZabbixConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionConfig {
    Github(GithubConfig),
    SshTunnel(TunnelConfig),
    Clickhouse(ClickhouseConfig),
    Hyperdx(HyperdxConfig),
    Zabbix(ZabbixConfig),
}

impl ConnectionConfig {
    pub fn kind(&self) -> ConnectionKind {
        match self {
            ConnectionConfig::Github(_) => ConnectionKind::Github,
            ConnectionConfig::SshTunnel(_) => ConnectionKind::SshTunnel,
            ConnectionConfig::Clickhouse(_) => ConnectionKind::Clickhouse,
            ConnectionConfig::Hyperdx(_) => ConnectionKind::Hyperdx,
            ConnectionConfig::Zabbix(_) => ConnectionKind::Zabbix,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub config: ConnectionConfig,
}

impl Connection {
    pub fn kind(&self) -> ConnectionKind {
        self.config.kind()
    }

    pub fn to_value(&self) -> Value {
        let config = match &self.config {
            ConnectionConfig::Github(c) => serde_json::to_value(c),
            ConnectionConfig::SshTunnel(c) => serde_json::to_value(c),
            ConnectionConfig::Clickhouse(c) => serde_json::to_value(c),
            ConnectionConfig::Hyperdx(c) => serde_json::to_value(c),
            ConnectionConfig::Zabbix(c) => serde_json::to_value(c),
        }
        .unwrap_or(Value::Null);
        json!({"id": self.id, "name": self.name, "kind": self.kind().key(), "config": config})
    }

    pub fn from_value(value: &Value) -> Result<Connection, String> {
        let id = value.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or("missing id")?.to_string();
        let name = value.get("name").and_then(|v| v.as_str()).unwrap_or(&id).to_string();
        let kind_key = value.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let kind = ConnectionKind::from_key(kind_key).ok_or_else(|| format!("“{id}”: unknown kind “{kind_key}”"))?;
        let config = value.get("config").cloned().unwrap_or_else(|| json!({}));
        let bad = |e: serde_json::Error| format!("“{id}”: invalid {} configuration ({e})", kind.label());
        let config = match kind {
            ConnectionKind::Github => ConnectionConfig::Github(serde_json::from_value(config).map_err(bad)?),
            ConnectionKind::SshTunnel => ConnectionConfig::SshTunnel(serde_json::from_value(config).map_err(bad)?),
            ConnectionKind::Clickhouse => ConnectionConfig::Clickhouse(serde_json::from_value(config).map_err(bad)?),
            ConnectionKind::Hyperdx => ConnectionConfig::Hyperdx(serde_json::from_value(config).map_err(bad)?),
            ConnectionKind::Zabbix => ConnectionConfig::Zabbix(serde_json::from_value(config).map_err(bad)?),
        };
        Ok(Connection { id, name, config })
    }

    /// Altre connessioni da cui questa dipende.
    pub fn references(&self) -> Vec<String> {
        match &self.config {
            ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::Http { tunnel: Some(t), .. }, .. }) => vec![t.clone()],
            ConnectionConfig::Hyperdx(h) => vec![h.clickhouse.clone()],
            ConnectionConfig::Zabbix(ZabbixConfig { tunnel: Some(t), .. }) => vec![t.clone()],
            _ => Vec::new(),
        }
    }

    pub fn secret_ref(&self) -> Option<&str> {
        match &self.config {
            ConnectionConfig::Clickhouse(c) => c.password_ref.as_deref(),
            ConnectionConfig::Zabbix(z) => z.token_ref.as_deref(),
            _ => None,
        }
    }
}

/// URL del Portachiavi del segreto di una connessione.
pub fn secret_url(id: &str) -> String {
    format!("superzed:{id}")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConnectionsFile {
    pub connections: Vec<Connection>,
    /// Voci non capite (tipo sconosciuto, configurazione non valida, id doppio): riscritte tali e quali.
    pub foreign: Vec<Value>,
    pub warnings: Vec<String>,
}

pub fn connections_path() -> PathBuf {
    paths::config_dir().join("connections.json")
}

/// Errore solo per un file che non e' JSON o di una versione sconosciuta; le singole voci
/// non valide finiscono in `foreign` con un avviso.
pub fn parse_connections(text: &str) -> Result<ConnectionsFile, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let version = root.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    if version == 0 || version > CONNECTIONS_VERSION {
        return Err(format!("unknown version {version}"));
    }
    let mut file = ConnectionsFile::default();
    for (ix, value) in root.get("connections").and_then(|c| c.as_array()).into_iter().flatten().enumerate() {
        match Connection::from_value(value) {
            Ok(conn) if file.connections.iter().any(|c| c.id == conn.id) => {
                file.warnings.push(format!("entry {}: duplicate id “{}” (kept, ignored)", ix + 1, conn.id));
                file.foreign.push(value.clone());
            }
            Ok(conn) => file.connections.push(conn),
            Err(e) => {
                file.warnings.push(format!("entry {}: {e} (kept, ignored)", ix + 1));
                file.foreign.push(value.clone());
            }
        }
    }
    Ok(file)
}

pub fn read_connections(path: &Path) -> anyhow::Result<Option<ConnectionsFile>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    parse_connections(&text).map(Some).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

/// Scrittura atomica: connessioni capite, poi quelle messe da parte.
pub fn write_connections(path: &Path, file: &ConnectionsFile) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let list: Vec<Value> = file.connections.iter().map(Connection::to_value).chain(file.foreign.iter().cloned()).collect();
    let text = serde_json::to_string_pretty(&json!({"version": CONNECTIONS_VERSION, "connections": list}))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `utente@host`, host o IP; mai un'opzione di ssh (`-…`).
pub fn safe_host(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-@:".contains(c))
}

/// Identificatore SQL semplice (nome del database).
pub fn safe_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn http_url_ok(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// `others`: tutte le connessioni tranne quella da validare.
pub fn validate(conn: &Connection, others: &[Connection]) -> Result<(), String> {
    if conn.name.trim().is_empty() {
        return Err("Name missing".into());
    }
    if !valid_id(&conn.id) {
        return Err(format!("Invalid id “{}”", conn.id));
    }
    if others.iter().any(|c| c.id == conn.id) {
        return Err(format!("The id “{}” is already used", conn.id));
    }
    let reference = |id: &str, kind: ConnectionKind| -> Result<(), String> {
        match others.iter().find(|c| c.id == id) {
            None => Err(format!("Connection “{id}” missing")),
            Some(c) if c.kind() != kind => Err(format!("“{id}” is not a {} connection", kind.label())),
            Some(_) => Ok(()),
        }
    };
    match &conn.config {
        ConnectionConfig::Github(g) => {
            if !g.from_project() {
                if g.owner.as_deref().is_none_or(|o| o.trim().is_empty()) {
                    return Err("GitHub: owner missing".into());
                }
                if g.project_number.is_none_or(|n| n == 0) {
                    return Err("GitHub: Project number missing".into());
                }
                let repo_ok = g.repo.as_deref().and_then(|r| r.split_once('/')).is_some_and(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'));
                if !repo_ok {
                    return Err("GitHub: repo in the form owner/name".into());
                }
            }
        }
        ConnectionConfig::SshTunnel(t) => {
            if !safe_host(&t.host) {
                return Err(format!("Tunnel: invalid host “{}”", t.host));
            }
            if t.forwards.is_empty() {
                return Err("Tunnel: at least one forward is needed".into());
            }
            if t.forwards.iter().any(|f| !safe_host(&f.remote_host) || f.remote_port == 0) {
                return Err("Tunnel: invalid forward".into());
            }
            if t.idle_minutes == 0 {
                return Err("Tunnel: idle minutes must be at least 1".into());
            }
        }
        ConnectionConfig::Clickhouse(c) => {
            match &c.transport {
                ClickhouseTransport::Http { url, tunnel } => {
                    if !http_url_ok(url) {
                        return Err(format!("ClickHouse: invalid URL “{url}”"));
                    }
                    if let Some(t) = tunnel {
                        reference(t, ConnectionKind::SshTunnel)?;
                    }
                }
                ClickhouseTransport::SshExec { host, command } => {
                    if !safe_host(host) {
                        return Err(format!("ClickHouse: invalid SSH host “{host}”"));
                    }
                    if command.as_deref().is_some_and(|c| c.trim().is_empty()) {
                        return Err("ClickHouse: empty SSH command".into());
                    }
                    if let Some(c) = command {
                        crate::clickhouse::check_ssh_command(c).map_err(|e| format!("ClickHouse: {e}"))?;
                    }
                }
            }
            if !safe_ident(&c.database) {
                return Err(format!("ClickHouse: invalid database “{}”", c.database));
            }
        }
        ConnectionConfig::Hyperdx(h) => {
            reference(&h.clickhouse, ConnectionKind::Clickhouse)?;
            if h.ui_url.as_deref().is_some_and(|u| !http_url_ok(u)) {
                return Err("HyperDX: invalid UI URL".into());
            }
        }
        ConnectionConfig::Zabbix(z) => {
            if !http_url_ok(&z.url) {
                return Err(format!("Zabbix: invalid URL “{}”", z.url));
            }
            if let Some(t) = &z.tunnel {
                reference(t, ConnectionKind::SshTunnel)?;
            }
        }
    }
    Ok(())
}

/// Id dal nome (minuscole ASCII, cifre, `-`), unico fra `existing` (`-2`, `-3`, …).
pub fn unique_id(name: &str, existing: &[Connection]) -> String {
    let mut slug = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() && (c == ' ' || c == '-' || c == '_') {
            slug.push('-');
        }
    }
    let base = match slug.trim_matches('-') {
        "" => "connection".to_string(),
        s => s.chars().take(32).collect(),
    };
    let taken = |id: &str| existing.iter().any(|c| c.id == id);
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !taken(id)).unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn sample() -> Vec<Connection> {
        vec![
            Connection { id: "gh".into(), name: "acme-app".into(), config: ConnectionConfig::Github(GithubConfig { owner: Some("acme".into()), project_number: Some(3), repo: Some("acme/acme_app".into()), stale_days: None }) },
            Connection { id: "t".into(), name: "box prod".into(), config: ConnectionConfig::SshTunnel(TunnelConfig { host: "root@203.0.113.10".into(), forwards: vec![Forward { local_port: 0, remote_host: "localhost".into(), remote_port: 8123 }], keep_open: false, idle_minutes: 10 }) },
            Connection { id: "ch".into(), name: "ch prod".into(), config: ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::Http { url: "http://127.0.0.1:8123".into(), tunnel: Some("t".into()) }, user: Some("reader".into()), password_ref: Some(secret_url("ch")), database: "default".into() }) },
            Connection { id: "hx".into(), name: "log app".into(), config: ConnectionConfig::Hyperdx(HyperdxConfig { clickhouse: "ch".into(), ui_url: Some("https://hyperdx.example".into()), namespace: Some("acme-app".into()), environment: Some("prod".into()) }) },
            Connection { id: "zb".into(), name: "zabbix".into(), config: ConnectionConfig::Zabbix(ZabbixConfig { url: "https://zabbix.example".into(), token_ref: Some(secret_url("zb")), tunnel: None }) },
        ]
    }

    #[test]
    fn roundtrip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/connections.json");
        let file = ConnectionsFile { connections: sample(), ..Default::default() };
        write_connections(&path, &file).unwrap();
        assert_eq!(read_connections(&path).unwrap(), Some(file));
        assert_eq!(read_connections(&dir.path().join("nessuno.json")).unwrap(), None);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""kind": "ssh_tunnel""#), "{text}");
        assert!(text.contains(r#""password_ref": "superzed:ch""#));
    }

    // Review Focus 2
    #[test]
    fn unknown_and_invalid_entries_are_kept_aside() {
        let text = json!({"version": 1, "connections": [
            {"id": "gh", "name": "a", "kind": "github", "config": {"owner": "o", "project_number": 1, "repo": "o/r"}},
            {"id": "gm", "name": "posta", "kind": "gmail", "config": {"futuro": true}},
            {"id": "t", "name": "rotto", "kind": "ssh_tunnel", "config": {"forwards": "no"}},
            {"id": "gh", "name": "doppio", "kind": "github", "config": {}},
            42
        ]}).to_string();
        let file = parse_connections(&text).unwrap();
        assert_eq!(file.connections.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["gh"]);
        assert_eq!(file.foreign.len(), 4, "conservate per la riscrittura");
        assert_eq!(file.warnings.len(), 4);
        assert!(file.warnings[0].contains("gmail"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        write_connections(&path, &file).unwrap();
        let again = read_connections(&path).unwrap().unwrap();
        assert_eq!(again.foreign, file.foreign, "tali e quali");
        assert!(parse_connections(r#"{"version": 2, "connections": []}"#).unwrap_err().contains("version"));
        assert!(parse_connections("{ rotto").is_err());
        assert_eq!(parse_connections(r#"{"version": 1}"#).unwrap(), ConnectionsFile::default());
    }

    #[test]
    fn validation() {
        let all = sample();
        for c in &all {
            let others: Vec<_> = all.iter().filter(|o| o.id != c.id).cloned().collect();
            assert_eq!(validate(c, &others), Ok(()), "{}", c.id);
        }
        let others: Vec<_> = all.iter().filter(|o| o.id != "hx").cloned().collect();
        let mut hx = all[3].clone();
        assert!(validate(&hx, &all).unwrap_err().contains("already used"));
        hx.config = ConnectionConfig::Hyperdx(HyperdxConfig { clickhouse: "t".into(), ui_url: None, namespace: None, environment: None });
        assert!(validate(&hx, &others).unwrap_err().contains("ClickHouse"), "riferimento del tipo sbagliato");
        hx.config = ConnectionConfig::Hyperdx(HyperdxConfig { clickhouse: "manca".into(), ui_url: None, namespace: None, environment: None });
        assert!(validate(&hx, &others).unwrap_err().contains("missing"));
        let mut t = all[1].clone();
        t.config = ConnectionConfig::SshTunnel(TunnelConfig { host: "-oProxyCommand=touch /tmp/x".into(), forwards: vec![Forward { local_port: 0, remote_host: "localhost".into(), remote_port: 8123 }], keep_open: false, idle_minutes: 10 });
        assert!(validate(&t, &[]).unwrap_err().contains("host"));
        let mut ch = all[2].clone();
        ch.config = ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::SshExec { host: "box".into(), command: None }, user: None, password_ref: None, database: "default; DROP TABLE x".into() });
        assert!(validate(&ch, &[]).unwrap_err().contains("database"));
        ch.config = ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::SshExec { host: "box".into(), command: Some("clickhouse-client -f JSONEachRow".into()) }, user: None, password_ref: None, database: "default".into() });
        assert_eq!(validate(&ch, &[]), Err("ClickHouse: the SSH command must contain --readonly=1".into()));
        ch.config = ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::SshExec { host: "box".into(), command: Some("clickhouse-client --readonly=1".into()) }, user: None, password_ref: None, database: "default".into() });
        assert_eq!(validate(&ch, &[]), Ok(()));
        let gh = Connection { id: "g".into(), name: "g".into(), config: ConnectionConfig::Github(GithubConfig { owner: Some("o".into()), project_number: Some(0), repo: Some("senza-barra".into()), stale_days: None }) };
        assert!(validate(&gh, &[]).is_err());
        let from_project = Connection { id: "g".into(), name: "g".into(), config: ConnectionConfig::Github(GithubConfig::default()) };
        assert_eq!(validate(&from_project, &[]), Ok(()), "tutto vuoto = dal progetto");
        let unnamed = Connection { name: "  ".into(), ..from_project };
        assert!(validate(&unnamed, &[]).unwrap_err().contains("Name"));
    }

    #[test]
    fn ids_come_from_the_name() {
        let all = sample();
        assert_eq!(unique_id("CH prod!", &all), "ch-prod");
        assert_eq!(unique_id("gh", &all), "gh-2");
        assert_eq!(unique_id("  ", &all), "connection");
        assert_eq!(unique_id("Città è", &[]), "citt");
    }

    #[test]
    fn references_follow_the_config() {
        let all = sample();
        assert_eq!(all[2].references(), vec!["t".to_string()]);
        assert_eq!(all[3].references(), vec!["ch".to_string()]);
        assert!(all[0].references().is_empty());
        assert_eq!(all[4].secret_ref(), Some("superzed:zb"));
        assert_eq!(ConnectionKind::Hyperdx.source_kind(), Some(SourceKind::Hyperdx));
        assert_eq!(ConnectionKind::SshTunnel.source_kind(), None);
        assert_eq!(ConnectionKind::from_key("ssh_tunnel"), Some(ConnectionKind::SshTunnel));
    }
}
