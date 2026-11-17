//! Connessioni caricate (entity globale): salvataggio con i segreti a parte, tunnel e
//! costruzione delle fonti per i pannelli del canvas.
use crate::clickhouse::{ChTransport, ClickhouseClient, DEFAULT_SSH_COMMAND, HyperdxSource, LogQuery, check_ssh_command};
use crate::connections::{
    ClickhouseConfig, ClickhouseTransport, Connection, ConnectionConfig, ConnectionsFile, TunnelConfig, connections_path,
    read_connections, unique_id, validate, write_connections,
};
use crate::github::{self, GithubSource};
use crate::item::{FilterField, Source, SourceKind};
use crate::runner::{ProcessRunner, Runner};
use crate::secrets::{KeychainSecrets, SecretStore};
use crate::tunnel::{SshTunnelCommand, TunnelCommand, TunnelSupervisor, rewrite_to_local};
use crate::zabbix::ZabbixSource;
use crate::{clickhouse, zabbix};
use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use http_client::HttpClient;
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

pub struct SourceDeps {
    pub runner: Arc<dyn Runner>,
    pub http: Arc<dyn HttpClient>,
    pub secrets: Rc<dyn SecretStore>,
    pub tunnel_command: Arc<dyn TunnelCommand>,
}

pub struct ConnectionStore {
    path: PathBuf,
    connections: Vec<Connection>,
    foreign: Vec<Value>,
    warnings: Vec<String>,
    /// Il file c'e' ma non si legge: niente salvataggi (non si sovrascrive).
    load_error: Option<String>,
    deps: SourceDeps,
    tunnels: Entity<TunnelSupervisor>,
    /// Operazioni sul Portachiavi in coda: una dopo l'altra, mai sovrapposte sullo stesso URL.
    secret_ops: Option<Task<()>>,
    /// Per connessione ClickHouse, per la sessione: l'utente e' readonly=1 e rifiuta
    /// `max_execution_time` nell'URL (condiviso dai client ricostruiti a ogni aggiornamento).
    ch_readonly_fallback: RefCell<HashMap<String, Arc<AtomicBool>>>,
}

enum SecretOp {
    Write(String, String),
    Delete(String),
}

struct GlobalConnectionStore(Entity<ConnectionStore>);
impl Global for GlobalConnectionStore {}

pub fn filter_schema_for(kind: SourceKind) -> Vec<FilterField> {
    match kind {
        SourceKind::Github => github::schema(),
        SourceKind::Hyperdx => clickhouse::hyperdx_schema(),
        SourceKind::Zabbix => zabbix::schema(),
    }
}

impl ConnectionStore {
    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalConnectionStore>().map(|g| g.0.clone())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn get(&self, id: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn tunnels(&self) -> &Entity<TunnelSupervisor> {
        &self.tunnels
    }

    /// Id delle voci non capite (conservate nel file): non si riusano.
    fn foreign_ids(&self) -> Vec<String> {
        self.foreign.iter().filter_map(|v| v.get("id")?.as_str().map(String::from)).collect()
    }

    pub fn new_id(&self, name: &str) -> String {
        let mut taken = self.connections.clone();
        taken.extend(self.foreign_ids().into_iter().map(|id| Connection {
            name: id.clone(),
            id,
            config: ConnectionConfig::Github(Default::default()),
        }));
        unique_id(name, &taken)
    }

    /// Rilegge connections.json (es. dopo averlo corretto a mano).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let (file, load_error) = load(&self.path);
        let old = std::mem::replace(&mut self.connections, file.connections);
        self.foreign = file.foreign;
        self.warnings = file.warnings;
        self.load_error = load_error;
        for o in &old {
            let new = self.get(&o.id).cloned();
            self.sync_tunnel(o, new.as_ref(), cx);
            if new.is_none_or(|n| n.config != o.config) {
                self.ch_readonly_fallback.borrow_mut().remove(&o.id);
            }
        }
        cx.notify();
    }

    fn queue_secret_ops(&mut self, ops: Vec<SecretOp>, cx: &mut Context<Self>) {
        if ops.is_empty() {
            return;
        }
        let previous = self.secret_ops.take();
        let secrets = self.deps.secrets.clone();
        self.secret_ops = Some(cx.spawn(async move |_, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            for op in ops {
                let (task, what, url) = cx.update(|cx| match &op {
                    SecretOp::Write(url, secret) => (secrets.write(url, secret, cx), "writing", url.clone()),
                    SecretOp::Delete(url) => (secrets.delete(url, cx), "deleting", url.clone()),
                });
                if let Err(e) = task.await {
                    log::warn!("superzed: {what} the secret {url} failed: {e:#}");
                }
            }
        }));
    }

    fn save(&self, connections: &[Connection]) -> Result<(), String> {
        if let Some(e) = &self.load_error {
            return Err(format!("connections.json is not readable ({e}): fix it or move it, superzed does not overwrite it"));
        }
        let file = ConnectionsFile { connections: connections.to_vec(), foreign: self.foreign.clone(), warnings: Vec::new() };
        write_connections(&self.path, &file).map_err(|e| format!("saving connections.json failed: {e:#}"))
    }

    /// Aggiunge o sostituisce (per id); `secrets` = `(url, segreto)` da scrivere nel Portachiavi.
    pub fn upsert(&mut self, conn: Connection, secrets: Vec<(String, String)>, cx: &mut Context<Self>) -> Result<(), String> {
        let others: Vec<Connection> = self.connections.iter().filter(|c| c.id != conn.id).cloned().collect();
        validate(&conn, &others)?;
        if !self.connections.iter().any(|c| c.id == conn.id) && self.foreign_ids().contains(&conn.id) {
            return Err(format!("The id “{}” is already used by an unrecognized entry of connections.json", conn.id));
        }
        let old = self.get(&conn.id).cloned();
        let mut next = self.connections.clone();
        match next.iter_mut().find(|c| c.id == conn.id) {
            Some(slot) => *slot = conn.clone(),
            None => next.push(conn.clone()),
        }
        self.save(&next)?;
        self.connections = next;
        let mut ops = Vec::new();
        // segreto non piu' referenziato: via dal Portachiavi (prima delle scritture)
        if let Some(url) = old.as_ref().and_then(|o| o.secret_ref())
            && conn.secret_ref() != Some(url)
        {
            ops.push(SecretOp::Delete(url.to_string()));
        }
        // solo il segreto della connessione stessa
        ops.extend(secrets.into_iter().filter(|(url, _)| conn.secret_ref() == Some(url.as_str())).map(|(u, s)| SecretOp::Write(u, s)));
        self.queue_secret_ops(ops, cx);
        if let Some(old) = old {
            self.sync_tunnel(&old, Some(&conn), cx);
            if old.config != conn.config {
                // utente o URL cambiati: il ripiego readonly si riscopre
                self.ch_readonly_fallback.borrow_mut().remove(&conn.id);
            }
        }
        cx.notify();
        Ok(())
    }

    /// Un tunnel modificato riparte con la configurazione nuova (anche se un pannello lo tiene);
    /// uno sparito o diventato altro si chiude.
    fn sync_tunnel(&mut self, old: &Connection, new: Option<&Connection>, cx: &mut Context<Self>) {
        if !matches!(old.config, ConnectionConfig::SshTunnel(_)) {
            return;
        }
        match new.map(|c| &c.config) {
            Some(ConnectionConfig::SshTunnel(cfg)) => self.tunnels.update(cx, |t, cx| t.reconfigure(&old.id, cfg, cx)),
            _ => self.tunnels.update(cx, |t, cx| t.close(&old.id, cx)),
        }
    }

    /// Rifiutata se un'altra connessione la usa; il segreto esce dal Portachiavi.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let conn = self.get(id).cloned().ok_or_else(|| format!("connection “{id}” not found"))?;
        let users: Vec<&str> =
            self.connections.iter().filter(|c| c.references().iter().any(|r| r == id)).map(|c| c.id.as_str()).collect();
        if !users.is_empty() {
            return Err(format!("“{id}” is used by {}", users.join(", ")));
        }
        let next: Vec<Connection> = self.connections.iter().filter(|c| c.id != id).cloned().collect();
        self.save(&next)?;
        self.connections = next;
        if let Some(url) = conn.secret_ref() {
            self.queue_secret_ops(vec![SecretOp::Delete(url.to_string())], cx);
        }
        self.tunnels.update(cx, |t, cx| t.close(id, cx));
        self.ch_readonly_fallback.borrow_mut().remove(id);
        cx.notify();
        Ok(())
    }

    /// Tunnel da cui passa la fonte (HyperDX → ClickHouse HTTP → tunnel; Zabbix → tunnel).
    pub fn required_tunnel(&self, id: &str) -> Option<String> {
        match &self.get(id)?.config {
            // non ricorsivo: il bersaglio deve essere ClickHouse (un file scritto a mano puo' avere cicli)
            ConnectionConfig::Hyperdx(h) => match &self.get(&h.clickhouse)?.config {
                ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::Http { tunnel, .. }, .. }) => {
                    tunnel.clone()
                }
                _ => None,
            },
            ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::Http { tunnel, .. }, .. }) => {
                tunnel.clone()
            }
            ConnectionConfig::Zabbix(z) => z.tunnel.clone(),
            _ => None,
        }
    }

    fn tunnel_config(&self, id: &str) -> Option<TunnelConfig> {
        match &self.get(id)?.config {
            ConnectionConfig::SshTunnel(t) => Some(t.clone()),
            _ => None,
        }
    }

    pub fn acquire_tunnel(&mut self, tunnel_id: &str, cx: &mut Context<Self>) {
        if let Some(config) = self.tunnel_config(tunnel_id) {
            self.tunnels.update(cx, |t, cx| t.acquire(tunnel_id, &config, cx));
        }
    }

    pub fn release_tunnel(&mut self, tunnel_id: &str, cx: &mut Context<Self>) {
        self.tunnels.update(cx, |t, cx| t.release(tunnel_id, cx));
    }

    pub fn ensure_tunnel(&mut self, tunnel_id: &str, cx: &mut Context<Self>) {
        self.tunnels.update(cx, |t, cx| t.ensure_open(tunnel_id, cx));
    }

    pub fn hold_tunnel(&mut self, tunnel_id: &str, cx: &mut Context<Self>) {
        if let Some(config) = self.tunnel_config(tunnel_id) {
            self.tunnels.update(cx, |t, cx| t.hold(tunnel_id, &config, cx));
        }
    }

    pub fn close_tunnel(&mut self, tunnel_id: &str, cx: &mut Context<Self>) {
        self.tunnels.update(cx, |t, cx| t.close(tunnel_id, cx));
    }

    fn through_tunnel(&self, url: &str, tunnel: Option<&str>, cx: &App) -> Result<String, String> {
        let Some(t) = tunnel else {
            return Ok(url.to_string());
        };
        let ports = self.tunnels.read(cx).local_ports(t).ok_or_else(|| format!("tunnel “{t}” not open"))?;
        let port = *ports.first().ok_or_else(|| format!("tunnel “{t}” has no ports"))?;
        rewrite_to_local(url, port)
    }

    fn clickhouse_client(&self, ch_id: &str, ch: &ClickhouseConfig, cx: &App) -> Result<ClickhouseClient, String> {
        let transport = match &ch.transport {
            ClickhouseTransport::Http { url, tunnel } => ChTransport::Http {
                url: self.through_tunnel(url, tunnel.as_deref(), cx)?,
                user: ch.user.clone(),
                password_ref: ch.password_ref.clone(),
                database: ch.database.clone(),
            },
            ClickhouseTransport::SshExec { host, command } => {
                let command = command.clone().unwrap_or_else(|| DEFAULT_SSH_COMMAND.to_string());
                check_ssh_command(&command).map_err(|e| format!("ClickHouse via ssh {host}: {e}"))?;
                ChTransport::SshExec { host: host.clone(), command }
            }
        };
        let fallback = self.ch_readonly_fallback.borrow_mut().entry(ch_id.to_string()).or_default().clone();
        Ok(ClickhouseClient::new(transport, self.deps.runner.clone(), self.deps.http.clone(), self.deps.secrets.clone())
            .with_readonly_fallback(fallback))
    }

    /// Fonte pronta per un pannello; un tunnel richiesto deve essere gia' aperto.
    pub fn build_source(&self, id: &str, project_root: Option<&Path>, cx: &App) -> Result<Rc<dyn Source>, String> {
        let conn = self.get(id).ok_or_else(|| format!("connection “{id}” not found"))?;
        match &conn.config {
            ConnectionConfig::Github(g) => {
                let cfg = github::resolve_config(g, project_root)?;
                Ok(Rc::new(GithubSource::new(cfg, self.deps.runner.clone())))
            }
            ConnectionConfig::Hyperdx(h) => {
                let Some(ConnectionConfig::Clickhouse(ch)) = self.get(&h.clickhouse).map(|c| &c.config) else {
                    return Err(format!("“{}”: ClickHouse connection “{}” missing", conn.name, h.clickhouse));
                };
                let client = self.clickhouse_client(&h.clickhouse, ch, cx)?;
                let query =
                    LogQuery { database: ch.database.clone(), namespace: h.namespace.clone(), environment: h.environment.clone() };
                Ok(Rc::new(HyperdxSource::new(query, h.ui_url.clone(), client)))
            }
            ConnectionConfig::Zabbix(z) => {
                let url = self.through_tunnel(&z.url, z.tunnel.as_deref(), cx)?;
                Ok(Rc::new(ZabbixSource::new(url, z.token_ref.clone(), self.deps.http.clone(), self.deps.secrets.clone())))
            }
            other => Err(format!("“{}” ({}) is not a panel source", conn.name, other.kind().label())),
        }
    }
}

/// File letto; `Some(errore)` se c'e' ma non si legge (allora non si riscrive).
fn load(path: &Path) -> (ConnectionsFile, Option<String>) {
    let (file, load_error) = match read_connections(path) {
        Ok(Some(file)) => (file, None),
        Ok(None) => (ConnectionsFile::default(), None),
        Err(e) => {
            log::warn!("superzed: {e:#}: no connections, the file will not be overwritten");
            (ConnectionsFile::default(), Some(format!("{e:#}")))
        }
    };
    for w in &file.warnings {
        log::warn!("superzed: connections.json: {w}");
    }
    (file, load_error)
}

fn new_store(cx: &mut App, path: PathBuf, deps: SourceDeps, ticking: bool) -> Entity<ConnectionStore> {
    let (file, load_error) = load(&path);
    let command = deps.tunnel_command.clone();
    let tunnels = cx.new(|cx| TunnelSupervisor::new(command, ticking, cx));
    let store = cx.new(|cx| {
        cx.observe(&tunnels, |_, _, cx| cx.notify()).detach();
        ConnectionStore {
            path,
            connections: file.connections,
            foreign: file.foreign,
            warnings: file.warnings,
            load_error,
            deps,
            tunnels,
            secret_ops: None,
            ch_readonly_fallback: RefCell::default(),
        }
    });
    cx.set_global(GlobalConnectionStore(store.clone()));
    store
}

pub fn init(cx: &mut App) {
    let deps = SourceDeps {
        runner: Arc::new(ProcessRunner),
        http: cx.http_client(),
        secrets: Rc::new(KeychainSecrets),
        tunnel_command: Arc::new(SshTunnelCommand),
    };
    new_store(cx, connections_path(), deps, true);
}

/// Store senza tick dei tunnel (i test chiamano `poll`).
pub fn init_test(cx: &mut App, path: PathBuf, deps: SourceDeps) -> Entity<ConnectionStore> {
    new_store(cx, path, deps, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::{ClickhouseConfig, ClickhouseTransport, ConnectionConfig, Forward, GithubConfig, HyperdxConfig, TunnelConfig, ZabbixConfig, secret_url};
    use crate::secrets::MemorySecrets;
    use crate::test_support::{FakeRunner, FixedTunnelCommand, deps_with, github_connections};
    use gpui::TestAppContext;
    use http_client::FakeHttpClient;
    use serde_json::json;

    fn setup(cx: &mut TestAppContext, file: Option<&str>) -> (Entity<ConnectionStore>, Rc<MemorySecrets>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        if let Some(text) = file {
            std::fs::write(&path, text).unwrap();
        }
        let secrets = Rc::new(MemorySecrets::default());
        let deps = deps_with(FakeRunner::new(), FakeHttpClient::with_404_response(), secrets.clone(), FixedTunnelCommand::new("/bin/sleep", &["30"]));
        let store = cx.update(|cx| init_test(cx, path, deps));
        (store, secrets, dir)
    }

    fn tunnel(id: &str) -> Connection {
        Connection { id: id.into(), name: id.into(), config: ConnectionConfig::SshTunnel(TunnelConfig { host: "box".into(), forwards: vec![Forward { local_port: 0, remote_host: "localhost".into(), remote_port: 8123 }], keep_open: false, idle_minutes: 10 }) }
    }

    fn clickhouse(id: &str, tunnel: Option<&str>) -> Connection {
        Connection { id: id.into(), name: id.into(), config: ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::Http { url: "http://ch.internal:8123".into(), tunnel: tunnel.map(String::from) }, user: None, password_ref: None, database: "default".into() }) }
    }

    fn hyperdx(id: &str, ch: &str) -> Connection {
        Connection { id: id.into(), name: id.into(), config: ConnectionConfig::Hyperdx(HyperdxConfig { clickhouse: ch.into(), ui_url: None, namespace: None, environment: None }) }
    }

    // Review Focus 1
    #[gpui::test]
    fn upsert_keeps_secrets_in_the_keychain_only(cx: &mut TestAppContext) {
        let (store, secrets, _dir) = setup(cx, None);
        assert!(store.read_with(cx, |s, _| s.connections().is_empty() && s.load_error().is_none()));
        let mut ch = clickhouse("ch", None);
        if let ConnectionConfig::Clickhouse(c) = &mut ch.config {
            c.password_ref = Some(secret_url("ch"));
        }
        store.update(cx, |s, cx| s.upsert(ch.clone(), vec![(secret_url("ch"), "SEGRETO".into())], cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(secrets.get("superzed:ch").as_deref(), Some("SEGRETO"));
        let text = std::fs::read_to_string(store.read_with(cx, |s, _| s.path().to_path_buf())).unwrap();
        assert!(text.contains("superzed:ch") && !text.contains("SEGRETO"), "{text}");
        assert_eq!(store.read_with(cx, |s, _| s.get("ch").cloned()), Some(ch));
        let invalid = Connection { name: " ".into(), ..clickhouse("x", None) };
        assert!(store.update(cx, |s, cx| s.upsert(invalid, vec![], cx)).unwrap_err().contains("Name"));
    }

    // Review Focus 2
    #[gpui::test]
    fn corrupt_file_is_never_overwritten(cx: &mut TestAppContext) {
        let (store, _secrets, dir) = setup(cx, Some("{ rotto"));
        assert!(store.read_with(cx, |s, _| s.load_error().is_some()));
        let err = store.update(cx, |s, cx| s.upsert(tunnel("t"), vec![], cx)).unwrap_err();
        assert!(err.contains("not readable"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.path().join("connections.json")).unwrap(), "{ rotto");
    }

    // Review Focus 2
    #[gpui::test]
    fn unknown_entries_survive_a_save(cx: &mut TestAppContext) {
        let file = json!({"version": 1, "connections": [{"id": "gm", "name": "posta", "kind": "gmail", "config": {"x": 1}}]}).to_string();
        let (store, _secrets, dir) = setup(cx, Some(&file));
        assert_eq!(store.read_with(cx, |s, _| s.warnings().len()), 1);
        store.update(cx, |s, cx| s.upsert(tunnel("t"), vec![], cx)).unwrap();
        let text = std::fs::read_to_string(dir.path().join("connections.json")).unwrap();
        assert!(text.contains("gmail") && text.contains("ssh_tunnel"), "{text}");
    }

    #[gpui::test]
    fn remove_refuses_referenced_connections_and_deletes_the_secret(cx: &mut TestAppContext) {
        let (store, secrets, _dir) = setup(cx, None);
        let mut ch = clickhouse("ch", None);
        if let ConnectionConfig::Clickhouse(c) = &mut ch.config {
            c.password_ref = Some(secret_url("ch"));
        }
        store.update(cx, |s, cx| {
            s.upsert(ch, vec![(secret_url("ch"), "pw".into())], cx).unwrap();
            s.upsert(hyperdx("hx", "ch"), vec![], cx).unwrap();
        });
        let err = store.update(cx, |s, cx| s.remove("ch", cx)).unwrap_err();
        assert!(err.contains("hx"), "{err}");
        store.update(cx, |s, cx| {
            s.remove("hx", cx).unwrap();
            s.remove("ch", cx).unwrap();
        });
        cx.run_until_parked();
        assert_eq!(secrets.get("superzed:ch"), None);
        assert!(store.read_with(cx, |s, _| s.connections().is_empty()));
        assert!(store.update(cx, |s, cx| s.remove("nessuna", cx)).is_err());
    }

    #[gpui::test]
    fn sources_are_built_through_references_and_tunnels(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (store, _secrets, _dir) = setup(cx, None);
        store.update(cx, |s, cx| {
            s.upsert(tunnel("t"), vec![], cx).unwrap();
            s.upsert(clickhouse("ch", Some("t")), vec![], cx).unwrap();
            s.upsert(hyperdx("hx", "ch"), vec![], cx).unwrap();
            s.upsert(Connection { id: "gh".into(), name: "gh".into(), config: ConnectionConfig::Github(GithubConfig::default()) }, vec![], cx).unwrap();
            let zabbix = ZabbixConfig { url: "http://zabbix.internal/zabbix".into(), token_ref: None, tunnel: Some("t".into()) };
            s.upsert(Connection { id: "zb".into(), name: "zb".into(), config: ConnectionConfig::Zabbix(zabbix) }, vec![], cx).unwrap();
        });
        assert_eq!(store.read_with(cx, |s, _| s.required_tunnel("hx")), Some("t".to_string()));
        assert_eq!(store.read_with(cx, |s, _| s.required_tunnel("zb")), Some("t".to_string()));
        let err = cx.update(|cx| store.read(cx).build_source("zb", None, cx).err()).unwrap();
        assert!(err.contains("tunnel “t” not open"), "{err}");
        assert_eq!(store.read_with(cx, |s, _| s.required_tunnel("gh")), None);
        let err = cx.update(|cx| store.read(cx).build_source("hx", None, cx).err()).unwrap();
        assert!(err.contains("tunnel “t” not open"), "{err}");
        // tunnel aperto: la fonte si costruisce (la porta locale la apre il test al posto di ssh)
        store.update(cx, |s, cx| s.acquire_tunnel("t", cx));
        let port = store.read_with(cx, |s, cx| s.tunnels().read(cx).ports("t"))[0];
        let _listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        store.update(cx, |s, cx| s.tunnels().clone().update(cx, |t, cx| t.poll(cx)));
        let built = cx.update(|cx| store.read(cx).build_source("hx", None, cx).map(|s| s.kind()));
        assert_eq!(built, Ok(SourceKind::Hyperdx));
        assert_eq!(cx.update(|cx| store.read(cx).build_source("zb", None, cx).map(|s| s.kind())), Ok(SourceKind::Zabbix));
        // GitHub dal progetto
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(dir.path().join(".claude/claude-pm.local.md"), "---\nproject_owner: o\nproject_number: 1\nrepo: o/r\n---\n").unwrap();
        assert_eq!(cx.update(|cx| store.read(cx).build_source("gh", Some(dir.path()), cx).map(|s| s.kind())), Ok(SourceKind::Github));
        assert!(cx.update(|cx| store.read(cx).build_source("gh", None, cx).err()).is_some());
        assert!(cx.update(|cx| store.read(cx).build_source("t", None, cx).err()).unwrap().contains("is not a panel source"));
        assert!(cx.update(|cx| store.read(cx).build_source("nessuna", None, cx).err()).unwrap().contains("not found"));
        // modificare il tunnel usato lo riavvia con la configurazione nuova
        let mut edited = tunnel("t");
        if let ConnectionConfig::SshTunnel(t) = &mut edited.config {
            t.idle_minutes = 5;
        }
        store.update(cx, |s, cx| s.upsert(edited, vec![], cx)).unwrap();
        assert_eq!(store.read_with(cx, |s, cx| s.tunnels().read(cx).state("t")), crate::tunnel::TunnelState::Opening);
        store.update(cx, |s, cx| s.close_tunnel("t", cx));
    }

    #[gpui::test]
    fn dropped_secret_ref_leaves_the_keychain_and_unsafe_ssh_is_refused(cx: &mut TestAppContext) {
        let file = json!({"version": 1, "connections": [
            {"id": "ssh", "name": "ssh", "kind": "clickhouse", "config": {"transport": {"ssh_exec": {"host": "box", "command": "clickhouse-client"}}}},
            {"id": "hx", "name": "hx", "kind": "hyperdx", "config": {"clickhouse": "ssh"}},
            {"id": "ssh-ok", "name": "ssh-ok", "kind": "clickhouse", "config": {"transport": {"ssh_exec": {"host": "box"}}}},
            {"id": "hx-ok", "name": "hx-ok", "kind": "hyperdx", "config": {"clickhouse": "ssh-ok"}}
        ]})
        .to_string();
        let (store, secrets, _dir) = setup(cx, Some(&file));
        let err = cx.update(|cx| store.read(cx).build_source("hx", None, cx).err()).unwrap();
        assert!(err.contains("--readonly=1"), "{err}");
        // comando di default (sola lettura), nessun tunnel
        assert_eq!(store.read_with(cx, |s, _| s.required_tunnel("hx-ok")), None);
        assert_eq!(cx.update(|cx| store.read(cx).build_source("hx-ok", None, cx).map(|s| s.kind())), Ok(SourceKind::Hyperdx));
        let mut ch = clickhouse("ch", None);
        if let ConnectionConfig::Clickhouse(c) = &mut ch.config {
            c.password_ref = Some(secret_url("ch"));
        }
        store.update(cx, |s, cx| s.upsert(ch, vec![(secret_url("ch"), "pw".into())], cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(secrets.get("superzed:ch").as_deref(), Some("pw"));
        store.update(cx, |s, cx| s.upsert(clickhouse("ch", None), vec![], cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(secrets.get("superzed:ch"), None);
    }

    #[gpui::test]
    fn hand_written_cycles_do_not_recurse(cx: &mut TestAppContext) {
        let file = json!({"version": 1, "connections": [
            {"id": "a", "name": "a", "kind": "hyperdx", "config": {"clickhouse": "b"}},
            {"id": "b", "name": "b", "kind": "hyperdx", "config": {"clickhouse": "a"}}
        ]})
        .to_string();
        let (store, _secrets, _dir) = setup(cx, Some(&file));
        assert_eq!(store.read_with(cx, |s, _| s.connections().len()), 2);
        assert_eq!(store.read_with(cx, |s, _| s.required_tunnel("a")), None);
        let err = cx.update(|cx| store.read(cx).build_source("a", None, cx).err()).unwrap();
        assert!(err.contains("ClickHouse connection “b” missing"), "{err}");
    }

    #[gpui::test]
    fn foreign_ids_are_never_reused(cx: &mut TestAppContext) {
        let file = json!({"version": 1, "connections": [{"id": "posta", "name": "posta", "kind": "gmail", "config": {}}]}).to_string();
        let (store, _secrets, _dir) = setup(cx, Some(&file));
        assert_eq!(store.read_with(cx, |s, _| s.new_id("Posta")), "posta-2");
        let err = store.update(cx, |s, cx| s.upsert(tunnel("posta"), vec![], cx)).unwrap_err();
        assert!(err.contains("unrecognized"), "{err}");
    }

    #[gpui::test]
    fn only_the_own_secret_is_written_and_keychain_ops_stay_in_order(cx: &mut TestAppContext) {
        let (store, secrets, _dir) = setup(cx, None);
        let mut ch = clickhouse("ch", None);
        if let ConnectionConfig::Clickhouse(c) = &mut ch.config {
            c.password_ref = Some(secret_url("ch"));
        }
        let pair = |id: &str, v: &str| (secret_url(id), v.to_string());
        store.update(cx, |s, cx| {
            s.upsert(ch.clone(), vec![pair("altro", "x"), pair("ch", "vecchio")], cx).unwrap();
            // rimossa e subito ricreata: la cancellazione non deve vincere sulla scrittura nuova
            s.remove("ch", cx).unwrap();
            s.upsert(ch.clone(), vec![pair("ch", "nuovo")], cx).unwrap();
        });
        cx.run_until_parked();
        assert_eq!(secrets.get("superzed:altro"), None);
        assert_eq!(secrets.get("superzed:ch").as_deref(), Some("nuovo"));
    }

    #[gpui::test]
    fn reload_recovers_a_fixed_file(cx: &mut TestAppContext) {
        let (store, _secrets, dir) = setup(cx, Some("{ rotto"));
        assert!(store.read_with(cx, |s, _| s.load_error().is_some()));
        std::fs::write(dir.path().join("connections.json"), github_connections().to_string()).unwrap();
        store.update(cx, |s, cx| s.reload(cx));
        assert!(store.read_with(cx, |s, _| s.load_error().is_none() && s.get("gh").is_some()));
        store.update(cx, |s, cx| s.upsert(tunnel("t"), vec![], cx)).unwrap();
    }

    #[gpui::test]
    fn an_edited_tunnel_in_use_restarts_with_the_new_config(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let command = crate::test_support::RecordingTunnelCommand::new();
        let deps = deps_with(FakeRunner::new(), FakeHttpClient::with_404_response(), Rc::default(), command.clone());
        let store = cx.update(|cx| init_test(cx, path.clone(), deps));
        store.update(cx, |s, cx| {
            s.upsert(tunnel("t"), vec![], cx).unwrap();
            s.upsert(clickhouse("ch", Some("t")), vec![], cx).unwrap();
            // un pannello lo usa
            s.acquire_tunnel("t", cx);
        });
        assert_eq!(command.hosts(), ["box"]);
        let mut edited = tunnel("t");
        if let ConnectionConfig::SshTunnel(t) = &mut edited.config {
            t.host = "root@10.0.0.2".into();
        }
        store.update(cx, |s, cx| s.upsert(edited, vec![], cx)).unwrap();
        assert_eq!(command.hosts(), ["box", "root@10.0.0.2"], "riavviato con l'host nuovo");
        assert_eq!(store.read_with(cx, |s, cx| s.tunnels().read(cx).users("t")), 1, "il pannello lo tiene ancora");
        // corretto a mano nel file e ricaricato
        let text = std::fs::read_to_string(&path).unwrap().replace("root@10.0.0.2", "root@10.0.0.3");
        std::fs::write(&path, text).unwrap();
        store.update(cx, |s, cx| s.reload(cx));
        assert_eq!(command.hosts().last().map(String::as_str), Some("root@10.0.0.3"));
        store.update(cx, |s, cx| s.tunnels().clone().update(cx, |t, cx| t.close_all(cx)));
    }

    #[gpui::test]
    async fn a_readonly_clickhouse_user_falls_back_once_per_connection(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let http = FakeHttpClient::create(move |req| {
            let uri = req.uri().to_string();
            log.lock().unwrap().push(uri.clone());
            async move {
                if uri.contains("max_execution_time") {
                    let body = "Code: 164. DB::Exception: Cannot modify 'max_execution_time' setting in readonly mode. (READONLY)";
                    return Ok(http_client::Response::builder().status(500).body(http_client::AsyncBody::from(body)).unwrap());
                }
                Ok(http_client::Response::builder().status(200).body(http_client::AsyncBody::from("")).unwrap())
            }
        });
        let deps = deps_with(FakeRunner::new(), http, Rc::default(), FixedTunnelCommand::new("/bin/sleep", &["30"]));
        let store = cx.update(|cx| init_test(cx, path, deps));
        store.update(cx, |s, cx| {
            s.upsert(clickhouse("ch", None), vec![], cx).unwrap();
            s.upsert(hyperdx("hx", "ch"), vec![], cx).unwrap();
        });
        let list = |cx: &mut TestAppContext| {
            cx.update(|cx| store.read(cx).build_source("hx", None, cx).unwrap().list(&Default::default(), cx))
        };
        list(cx).await.unwrap();
        // fonte ricostruita (es. al prossimo aggiornamento): si ricorda il ripiego
        list(cx).await.unwrap();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen[0].contains("max_execution_time=30"));
        assert!(seen[1..].iter().all(|u| !u.contains("max_execution_time") && u.contains("readonly=1")), "{seen:?}");
    }

    #[test]
    fn schemas_per_kind() {
        let keys = |k: SourceKind| filter_schema_for(k).into_iter().map(|f| f.key).collect::<Vec<_>>();
        assert_eq!(keys(SourceKind::Github), ["block", "area", "text"]);
        assert_eq!(keys(SourceKind::Hyperdx), ["service", "severity", "window", "text"]);
        assert_eq!(keys(SourceKind::Zabbix), ["severity", "groups", "text"]);
    }
}
