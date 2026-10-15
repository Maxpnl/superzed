//! Campi del form per tipo di connessione (vista Fonti) e conversione da/verso `Connection`.
//! I segreti digitati tornano a parte, da scrivere nel Portachiavi; il form non li rilegge.
use crate::connections::{
    ClickhouseConfig, ClickhouseTransport, Connection, ConnectionConfig, ConnectionKind, DEFAULT_DATABASE,
    DEFAULT_IDLE_MINUTES, Forward, GithubConfig, HyperdxConfig, TunnelConfig, ZabbixConfig, secret_url,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FormField {
    pub key: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
    pub secret: bool,
}

const fn field(key: &'static str, label: &'static str, placeholder: &'static str) -> FormField {
    FormField { key, label, placeholder, secret: false }
}

const fn secret(key: &'static str, label: &'static str, placeholder: &'static str) -> FormField {
    FormField { key, label, placeholder, secret: true }
}

/// Il primo campo e' sempre il nome.
pub fn form_fields(kind: ConnectionKind) -> Vec<FormField> {
    let name = field("name", "Name", "acme-app prod");
    match kind {
        ConnectionKind::Github => vec![
            name,
            field("owner", "Owner", "empty: from the project's .claude/claude-pm.local.md"),
            field("project_number", "Project number", "3"),
            field("repo", "Repo", "acme/acme_app"),
            field("stale_days", "Days before “stale”", "14"),
        ],
        ConnectionKind::SshTunnel => vec![
            name,
            field("host", "SSH host", "root@203.0.113.10"),
            field("forwards", "Forwards (local:host:port)", "0:localhost:8123"),
            field("keep_open", "Keep open (yes/no)", "no"),
            field("idle_minutes", "Close after idle minutes", "10"),
        ],
        ConnectionKind::Clickhouse => vec![
            name,
            field("url", "URL HTTP", "http://127.0.0.1:8123"),
            field("tunnel", "Tunnel (id)", "Empty: none · with a tunnel the URL is http:// (not https)"),
            field("ssh_host", "Or: SSH host (remote execution)", "root@203.0.113.10"),
            field("ssh_command", "Remote command", "empty: ClickStack's clickhouse-client"),
            field("user", "User", "reader"),
            secret("password", "Password (Keychain)", "empty: unchanged"),
            field("database", "Database", DEFAULT_DATABASE),
        ],
        ConnectionKind::Hyperdx => vec![
            name,
            field("clickhouse", "ClickHouse connection (id)", "ch-prod"),
            field("ui_url", "HyperDX URL", "https://hyperdx.example"),
            field("namespace", "service.namespace", "acme-app"),
            field("environment", "deployment.environment", "prod"),
        ],
        ConnectionKind::Zabbix => vec![
            name,
            field("url", "URL", "https://zabbix.example"),
            field("tunnel", "Tunnel (id)", "Empty: none · with a tunnel the URL is http:// (not https)"),
            secret("token", "API token (Keychain)", "empty: unchanged"),
        ],
    }
}

pub fn parse_forwards(s: &str) -> Result<Vec<Forward>, String> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let err = || format!("Forward “{part}”: use local:host:port (local 0 = any free port)");
        let mut bits = part.splitn(3, ':');
        let (Some(local), Some(host), Some(port)) = (bits.next(), bits.next(), bits.next()) else {
            return Err(err());
        };
        out.push(Forward {
            local_port: local.trim().parse().map_err(|_| err())?,
            remote_host: host.trim().to_string(),
            remote_port: port.trim().parse().map_err(|_| err())?,
        });
    }
    if out.is_empty() {
        return Err("At least one forward is needed (local:host:port)".into());
    }
    Ok(out)
}

pub fn format_forwards(forwards: &[Forward]) -> String {
    forwards.iter().map(|f| format!("{}:{}:{}", f.local_port, f.remote_host, f.remote_port)).collect::<Vec<_>>().join(", ")
}

/// Valori del form per una connessione esistente (segreti vuoti).
pub fn fields_of(conn: &Connection) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let mut put = |k: &str, v: Option<String>| {
        m.insert(k.to_string(), v.unwrap_or_default());
    };
    put("name", Some(conn.name.clone()));
    match &conn.config {
        ConnectionConfig::Github(g) => {
            put("owner", g.owner.clone());
            put("project_number", g.project_number.map(|n| n.to_string()));
            put("repo", g.repo.clone());
            put("stale_days", g.stale_days.map(|n| n.to_string()));
        }
        ConnectionConfig::SshTunnel(t) => {
            put("host", Some(t.host.clone()));
            put("forwards", Some(format_forwards(&t.forwards)));
            put("keep_open", Some(if t.keep_open { "yes" } else { "" }.to_string()));
            put("idle_minutes", Some(t.idle_minutes.to_string()));
        }
        ConnectionConfig::Clickhouse(c) => {
            let (url, tunnel, host, command) = match &c.transport {
                ClickhouseTransport::Http { url, tunnel } => (Some(url.clone()), tunnel.clone(), None, None),
                ClickhouseTransport::SshExec { host, command } => (None, None, Some(host.clone()), command.clone()),
            };
            put("url", url);
            put("tunnel", tunnel);
            put("ssh_host", host);
            put("ssh_command", command);
            put("user", c.user.clone());
            put("password", None);
            put("database", Some(c.database.clone()));
        }
        ConnectionConfig::Hyperdx(h) => {
            put("clickhouse", Some(h.clickhouse.clone()));
            put("ui_url", h.ui_url.clone());
            put("namespace", h.namespace.clone());
            put("environment", h.environment.clone());
        }
        ConnectionConfig::Zabbix(z) => {
            put("url", Some(z.url.clone()));
            put("tunnel", z.tunnel.clone());
            put("token", None);
        }
    }
    m
}

/// Connessione dai valori del form e segreti da scrivere (`(url, segreto)`). Un segreto
/// lasciato vuoto tiene il riferimento di `existing`. Non valida i riferimenti (lo fa `validate`).
pub fn connection_from_fields(
    kind: ConnectionKind,
    id: &str,
    fields: &BTreeMap<String, String>,
    existing: Option<&Connection>,
) -> Result<(Connection, Vec<(String, String)>), String> {
    let get = |k: &str| fields.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let number = |k: &str, what: &str| -> Result<Option<u32>, String> {
        get(k).map(|v| v.parse::<u32>().map_err(|_| format!("{what}: a number is needed"))).transpose()
    };
    let mut secrets = Vec::new();
    let mut take_secret = |k: &str| -> Option<String> {
        match get(k) {
            Some(value) => {
                secrets.push((secret_url(id), value));
                Some(secret_url(id))
            }
            None => existing.filter(|e| e.kind() == kind).and_then(|e| e.secret_ref().map(str::to_string)),
        }
    };
    let config = match kind {
        ConnectionKind::Github => ConnectionConfig::Github(GithubConfig {
            owner: get("owner"),
            project_number: number("project_number", "Project number")?,
            repo: get("repo"),
            stale_days: number("stale_days", "Days before “stale”")?,
        }),
        ConnectionKind::SshTunnel => ConnectionConfig::SshTunnel(TunnelConfig {
            host: get("host").unwrap_or_default(),
            forwards: parse_forwards(&get("forwards").unwrap_or_default())?,
            keep_open: get("keep_open").is_some_and(|v| matches!(v.to_lowercase().as_str(), "si" | "sì" | "yes" | "true")),
            idle_minutes: number("idle_minutes", "Idle minutes")?.unwrap_or(DEFAULT_IDLE_MINUTES),
        }),
        ConnectionKind::Clickhouse => {
            let transport = match get("ssh_host") {
                Some(host) => ClickhouseTransport::SshExec { host, command: get("ssh_command") },
                None => ClickhouseTransport::Http {
                    url: get("url").ok_or("ClickHouse: an HTTP URL or an SSH host is needed")?,
                    tunnel: get("tunnel"),
                },
            };
            ConnectionConfig::Clickhouse(ClickhouseConfig {
                transport,
                user: get("user"),
                password_ref: take_secret("password"),
                database: get("database").unwrap_or_else(|| DEFAULT_DATABASE.to_string()),
            })
        }
        ConnectionKind::Hyperdx => ConnectionConfig::Hyperdx(HyperdxConfig {
            clickhouse: get("clickhouse").unwrap_or_default(),
            ui_url: get("ui_url"),
            namespace: get("namespace"),
            environment: get("environment"),
        }),
        ConnectionKind::Zabbix => ConnectionConfig::Zabbix(ZabbixConfig {
            url: get("url").unwrap_or_default(),
            token_ref: take_secret("token"),
            tunnel: get("tunnel"),
        }),
    };
    let name = get("name").unwrap_or_default();
    Ok((Connection { id: id.to_string(), name, config }, secrets))
}

/// Avviso (non un errore): URL `http://` verso un host remoto, senza tunnel, con una password
/// o un token (digitati ora o gia' nel Portachiavi, `stored_secret`): viaggerebbero in chiaro.
pub fn insecure_http_warning(fields: &BTreeMap<String, String>, stored_secret: bool) -> Option<String> {
    let get = |k: &str| fields.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    if get("tunnel").is_some() || get("ssh_host").is_some() {
        return None;
    }
    let url = url::Url::parse(get("url")?).ok()?;
    if url.scheme() != "http" {
        return None;
    }
    let loopback = match url.host()? {
        url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    let secret = get("password").is_some() || get("token").is_some() || stored_secret;
    (!loopback && secret).then(|| {
        "Warning: with http:// to a remote host, passwords and tokens travel in plain text. Use https:// or an SSH tunnel.".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::ConnectionKind;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn every_kind_round_trips_through_the_form() {
        let cases = [
            (ConnectionKind::Github, map(&[("name", "app"), ("owner", "acme"), ("project_number", "3"), ("repo", "acme/acme_app"), ("stale_days", "")])),
            (ConnectionKind::SshTunnel, map(&[("name", "box"), ("host", "root@1.2.3.4"), ("forwards", "0:localhost:8123, 9000:db:5432"), ("keep_open", "yes"), ("idle_minutes", "15")])),
            (ConnectionKind::Clickhouse, map(&[("name", "ch"), ("url", ""), ("tunnel", ""), ("ssh_host", "root@1.2.3.4"), ("ssh_command", ""), ("user", ""), ("password", ""), ("database", "default")])),
            (ConnectionKind::Hyperdx, map(&[("name", "log"), ("clickhouse", "ch"), ("ui_url", "https://h.example"), ("namespace", "acme-app"), ("environment", "prod")])),
            (ConnectionKind::Zabbix, map(&[("name", "zb"), ("url", "https://z.example"), ("tunnel", ""), ("token", "")])),
        ];
        for (kind, fields) in cases {
            let mut keys: Vec<&str> = form_fields(kind).iter().map(|f| f.key).collect();
            keys.sort_unstable();
            assert_eq!(keys, fields.keys().map(String::as_str).collect::<Vec<_>>(), "{kind:?}");
            let (conn, secrets) = connection_from_fields(kind, "x", &fields, None).unwrap();
            assert_eq!(conn.kind(), kind);
            assert!(secrets.is_empty());
            let mut back = fields_of(&conn);
            for (k, v) in &fields {
                assert_eq!(back.remove(k).unwrap_or_default(), *v, "{kind:?} {k}");
            }
        }
    }

    // Review Focus 1
    #[test]
    fn secrets_never_reach_the_json() {
        let fields = map(&[("name", "ch"), ("url", "http://127.0.0.1:8123"), ("user", "reader"), ("password", "SEGRETO"), ("database", "")]);
        let (conn, secrets) = connection_from_fields(ConnectionKind::Clickhouse, "ch", &fields, None).unwrap();
        assert_eq!(secrets, vec![("superzed:ch".to_string(), "SEGRETO".to_string())]);
        assert_eq!(conn.secret_ref(), Some("superzed:ch"));
        assert!(!conn.to_value().to_string().contains("SEGRETO"));
        assert!(!format!("{conn:?}").contains("SEGRETO"));
        assert_eq!(fields_of(&conn).get("password").map(String::as_str), Some(""), "il form non rilegge il segreto");
        // modifica senza ridigitare la password: il riferimento resta
        let edit = map(&[("name", "ch 2"), ("url", "http://127.0.0.1:8123"), ("password", "")]);
        let (edited, secrets) = connection_from_fields(ConnectionKind::Clickhouse, "ch", &edit, Some(&conn)).unwrap();
        assert!(secrets.is_empty());
        assert_eq!(edited.secret_ref(), Some("superzed:ch"));
        assert_eq!(edited.name, "ch 2");
    }

    #[test]
    fn plain_http_with_a_secret_to_a_remote_host_warns() {
        let warn = |pairs: &[(&str, &str)], stored: bool| insecure_http_warning(&map(pairs), stored);
        assert!(warn(&[("url", "http://ch.example:8123"), ("password", "pw")], false).unwrap().contains("plain text"));
        assert!(warn(&[("url", "http://zabbix.example"), ("token", "")], true).is_some(), "segreto gia' nel Portachiavi");
        assert_eq!(warn(&[("url", "http://ch.example:8123"), ("password", "")], false), None, "nessun segreto");
        assert_eq!(warn(&[("url", "https://ch.example"), ("password", "pw")], false), None);
        assert_eq!(warn(&[("url", "http://127.0.0.1:8123"), ("password", "pw")], false), None);
        assert_eq!(warn(&[("url", "http://localhost:8123"), ("password", "pw")], false), None);
        assert_eq!(warn(&[("url", "http://[::1]:8123"), ("password", "pw")], false), None);
        assert_eq!(warn(&[("url", "http://ch.internal:8123"), ("tunnel", "t"), ("password", "pw")], false), None, "via tunnel ssh");
        assert_eq!(warn(&[("url", "http://ch.example"), ("ssh_host", "box"), ("password", "pw")], false), None, "esecuzione via ssh");
        let tunnel = form_fields(ConnectionKind::Clickhouse).into_iter().find(|f| f.key == "tunnel").unwrap();
        assert!(tunnel.placeholder.contains("http://") && tunnel.placeholder.contains("https"), "{}", tunnel.placeholder);
        let tunnel = form_fields(ConnectionKind::Zabbix).into_iter().find(|f| f.key == "tunnel").unwrap();
        assert!(tunnel.placeholder.contains("http://"), "{}", tunnel.placeholder);
    }

    #[test]
    fn form_errors_are_readable() {
        assert!(parse_forwards("8123").unwrap_err().contains("local:host:port"));
        assert!(parse_forwards("x:localhost:8123").is_err());
        assert!(parse_forwards("").unwrap_err().contains("At least"));
        assert_eq!(format_forwards(&parse_forwards(" 0:localhost:8123 ,1:db:5432").unwrap()), "0:localhost:8123, 1:db:5432");
        let ch = map(&[("name", "ch")]);
        assert!(connection_from_fields(ConnectionKind::Clickhouse, "ch", &ch, None).unwrap_err().contains("URL"));
        let gh = map(&[("name", "g"), ("project_number", "tre")]);
        assert!(connection_from_fields(ConnectionKind::Github, "g", &gh, None).unwrap_err().contains("number"));
    }
}
