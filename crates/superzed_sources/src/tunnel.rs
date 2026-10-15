//! Tunnel SSH (`ssh -N -L …`): aperti quando un pannello li usa (o a mano dalla vista
//! Fonti), `Open` quando le porte locali rispondono, riaperti con backoff finche' servono,
//! chiusi dopo `idle_minutes` senza utenti (salvo `keep_open`) e tutti all'uscita dall'app.
use crate::connections::{TunnelConfig, safe_host};
use collections::HashMap;
use gpui::{Context, Subscription, Task};
use futures::AsyncReadExt as _;
use std::{
    collections::VecDeque,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use util::command::{Child, Stdio, new_command};

pub const TICK: Duration = Duration::from_secs(1);
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
pub const MAX_BACKOFF_SECS: u64 = 60;
const CONNECT_PROBE: Duration = Duration::from_millis(100);
/// Coda dello stderr di ssh tenuta in memoria.
const STDERR_TAIL: usize = 8 * 1024;
/// Aperto almeno cosi' a lungo: il backoff riparte da 1 s (niente giri da 1 s se il tunnel va e viene).
const STABLE_OPEN: Duration = Duration::from_secs(30);
/// Controlli concessi, dopo l'uscita di ssh, perche' la lettura dello stderr arrivi in fondo.
const EXIT_GRACE_POLLS: u8 = 5;

/// Attesa prima del tentativo `attempt` (1 → 1 s, 2 → 2 s, 3 → 4 s … massimo 60 s).
pub fn backoff(attempt: u32) -> Duration {
    let secs = 1u64.checked_shl(attempt.saturating_sub(1)).unwrap_or(u64::MAX);
    Duration::from_secs(secs.min(MAX_BACKOFF_SECS))
}

pub fn ssh_args(config: &TunnelConfig, ports: &[u16]) -> Vec<String> {
    let mut args: Vec<String> =
        ["-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=30", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
            .map(String::from)
            .to_vec();
    for (forward, port) in config.forwards.iter().zip(ports) {
        args.push("-L".into());
        args.push(format!("127.0.0.1:{port}:{}:{}", forward.remote_host, forward.remote_port));
    }
    args.push("--".into());
    args.push(config.host.clone());
    args
}

pub fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// Host e porta dell'URL sostituiti con `127.0.0.1:<porta locale del tunnel>`.
pub fn rewrite_to_local(url: &str, port: u16) -> Result<String, String> {
    let bad = || format!("invalid URL “{url}”");
    let mut u = url::Url::parse(url).map_err(|_| bad())?;
    u.set_host(Some("127.0.0.1")).map_err(|_| bad())?;
    u.set_port(Some(port)).map_err(|_| bad())?;
    Ok(u.to_string())
}

#[derive(Clone, Debug, PartialEq)]
pub enum TunnelState {
    Closed,
    Opening,
    Open,
    Failed { stderr: String, retry_at: Instant },
}

pub trait TunnelCommand: Send + Sync + 'static {
    fn command(&self, config: &TunnelConfig, ports: &[u16]) -> (String, Vec<String>);
}

pub struct SshTunnelCommand;

impl TunnelCommand for SshTunnelCommand {
    fn command(&self, config: &TunnelConfig, ports: &[u16]) -> (String, Vec<String>) {
        ("/usr/bin/ssh".to_string(), ssh_args(config, ports))
    }
}

struct Tunnel {
    config: TunnelConfig,
    state: TunnelState,
    users: usize,
    /// Aperto a mano dalla vista Fonti.
    held: bool,
    attempts: u32,
    ports: Vec<u16>,
    child: Option<Child>,
    /// Stderr di ssh, letto di continuo da un thread (la pipe non si riempie mai).
    stderr: Option<Arc<Mutex<StderrTail>>>,
    /// Controlli fatti dopo l'uscita di ssh aspettando la fine dello stderr.
    exit_polls: u8,
    opened_at: Option<Instant>,
    open_since: Option<Instant>,
    idle_since: Option<Instant>,
    /// Configurazione non valida: nessun nuovo tentativo finche' non cambia o non si riapre a mano.
    config_error: bool,
}

impl Tunnel {
    fn new(config: TunnelConfig) -> Self {
        Self {
            config,
            state: TunnelState::Closed,
            users: 0,
            held: false,
            attempts: 0,
            ports: Vec::new(),
            child: None,
            stderr: None,
            exit_polls: 0,
            opened_at: None,
            open_since: None,
            idle_since: None,
            config_error: false,
        }
    }

    fn wanted(&self) -> bool {
        self.users > 0 || self.held || self.config.keep_open
    }

    /// Il processo muore con il `Child` (`kill_on_drop`).
    fn stop(&mut self, state: TunnelState) {
        self.child = None;
        self.stderr = None;
        self.open_since = None;
        self.state = state;
    }

    fn fail(&mut self, stderr: String, now: Instant) {
        self.child = None;
        self.stderr = None;
        self.open_since = None;
        self.attempts += 1;
        self.state = TunnelState::Failed { stderr, retry_at: now + backoff(self.attempts) };
    }
}

pub struct TunnelSupervisor {
    tunnels: HashMap<String, Tunnel>,
    command: Arc<dyn TunnelCommand>,
    test_now: Option<Instant>,
    _ticker: Option<Task<()>>,
    _quit: Subscription,
}

impl TunnelSupervisor {
    /// `ticking = false` nei test: si chiama `poll` a mano con l'orologio del test.
    pub fn new(command: Arc<dyn TunnelCommand>, ticking: bool, cx: &mut Context<Self>) -> Self {
        let ticker = ticking.then(|| {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |s, cx| s.poll(cx)).is_err() {
                    break;
                }
            })
        });
        // all'uscita dall'app nessun ssh resta vivo
        let quit = cx.on_app_quit(|this, cx| {
            this.close_all(cx);
            async {}
        });
        Self { tunnels: HashMap::default(), command, test_now: None, _ticker: ticker, _quit: quit }
    }

    pub fn now(&self) -> Instant {
        self.test_now.unwrap_or_else(Instant::now)
    }

    pub fn set_now_for_test(&mut self, now: Option<Instant>) {
        self.test_now = now;
    }

    pub fn state(&self, id: &str) -> TunnelState {
        self.tunnels.get(id).map(|t| t.state.clone()).unwrap_or(TunnelState::Closed)
    }

    pub fn users(&self, id: &str) -> usize {
        self.tunnels.get(id).map_or(0, |t| t.users)
    }

    /// Porte locali dell'ultimo avvio (anche mentre si apre).
    pub fn ports(&self, id: &str) -> Vec<u16> {
        self.tunnels.get(id).map(|t| t.ports.clone()).unwrap_or_default()
    }

    /// Porte locali, solo a tunnel aperto.
    pub fn local_ports(&self, id: &str) -> Option<Vec<u16>> {
        let t = self.tunnels.get(id)?;
        (t.state == TunnelState::Open).then(|| t.ports.clone())
    }

    pub fn pid(&self, id: &str) -> Option<u32> {
        self.tunnels.get(id)?.child.as_ref().map(|c| c.id())
    }

    /// La configurazione nuova vale dal prossimo avvio.
    fn entry(&mut self, id: &str, config: &TunnelConfig) -> &mut Tunnel {
        let t = self.tunnels.entry(id.to_string()).or_insert_with(|| Tunnel::new(config.clone()));
        if t.config != *config {
            t.config = config.clone();
            t.config_error = false;
        }
        t
    }

    pub fn acquire(&mut self, id: &str, config: &TunnelConfig, cx: &mut Context<Self>) {
        let t = self.entry(id, config);
        t.users += 1;
        t.idle_since = None;
        if t.state == TunnelState::Closed {
            self.spawn(id, cx);
        }
        cx.notify();
    }

    pub fn release(&mut self, id: &str, cx: &mut Context<Self>) {
        let now = self.now();
        if let Some(t) = self.tunnels.get_mut(id) {
            t.users = t.users.saturating_sub(1);
            if t.users == 0 && !t.held {
                t.idle_since = Some(now);
            }
        }
        cx.notify();
    }

    /// Riapre un tunnel chiuso (a mano o per inattivita') che qualcuno usa ancora.
    pub fn ensure_open(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.tunnels.get(id).is_some_and(|t| t.state == TunnelState::Closed && t.wanted()) {
            self.spawn(id, cx);
        }
    }

    /// Apertura a mano (vista Fonti): resta aperto finche' non si chiude a mano.
    pub fn hold(&mut self, id: &str, config: &TunnelConfig, cx: &mut Context<Self>) {
        let t = self.entry(id, config);
        t.held = true;
        t.idle_since = None;
        if matches!(t.state, TunnelState::Closed | TunnelState::Failed { .. }) {
            self.spawn(id, cx);
        }
        cx.notify();
    }

    /// Chiusura a mano: non riparte da solo (lo riapre il prossimo aggiornamento di un pannello).
    pub fn close(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(t) = self.tunnels.get_mut(id) {
            t.held = false;
            t.attempts = 0;
            t.idle_since = None;
            t.stop(TunnelState::Closed);
        }
        cx.notify();
    }

    /// Configurazione modificata (form o file ricaricato): il processo in corso usa gli
    /// argomenti vecchi, quindi si chiude e, se qualcuno lo usa, riparte subito con i nuovi.
    /// Un tunnel chiuso resta chiuso: il prossimo `ensure_open` usa gia' la configurazione nuova.
    pub fn reconfigure(&mut self, id: &str, config: &TunnelConfig, cx: &mut Context<Self>) {
        let Some(t) = self.tunnels.get_mut(id) else {
            return;
        };
        if t.config == *config {
            return;
        }
        t.config = config.clone();
        t.config_error = false;
        t.attempts = 0;
        if t.state == TunnelState::Closed {
            cx.notify();
            return;
        }
        t.stop(TunnelState::Closed);
        if t.wanted() {
            self.spawn(id, cx);
        }
        cx.notify();
    }

    /// Ultimo avvio rifiutato per una configurazione non valida (nessun nuovo tentativo).
    pub fn config_error(&self, id: &str) -> bool {
        self.tunnels.get(id).is_some_and(|t| t.config_error)
    }

    pub fn close_all(&mut self, cx: &mut Context<Self>) {
        for t in self.tunnels.values_mut() {
            t.held = false;
            t.attempts = 0;
            t.stop(TunnelState::Closed);
        }
        cx.notify();
    }

    fn spawn(&mut self, id: &str, cx: &mut Context<Self>) {
        let now = self.now();
        let command = self.command.clone();
        let Some(t) = self.tunnels.get_mut(id) else {
            return;
        };
        // mai un host che ssh potrebbe leggere come opzione (anche se il file e' stato scritto a mano)
        if let Err(e) = check_config(&t.config) {
            t.fail(e, now);
            t.config_error = true;
            cx.notify();
            return;
        }
        t.config_error = false;
        let ports: Result<Vec<u16>, String> = t
            .config
            .forwards
            .iter()
            .map(|f| match f.local_port {
                0 => free_port().map_err(|e| format!("no free local port: {e}")),
                // una porta fissa gia' presa risponderebbe al posto del tunnel
                p => TcpListener::bind(("127.0.0.1", p)).map(|_| p).map_err(|_| format!("local port {p} in use")),
            })
            .collect();
        let started = ports.and_then(|ports| {
            let (program, args) = command.command(&t.config, &ports);
            let mut cmd = new_command(&program);
            cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
            cmd.spawn().map(|child| (ports, child)).map_err(|e| format!("starting {program} failed: {e}"))
        });
        match started {
            Ok((ports, mut child)) => {
                t.stderr = Some(drain_stderr(child.stderr.take()));
                t.exit_polls = 0;
                t.ports = ports;
                t.child = Some(child);
                t.state = TunnelState::Opening;
                t.opened_at = Some(now);
                t.open_since = None;
            }
            Err(e) => t.fail(e, now),
        }
        cx.notify();
    }

    pub fn poll(&mut self, cx: &mut Context<Self>) {
        let now = self.now();
        let ids: Vec<String> = self.tunnels.keys().cloned().collect();
        let mut changed = false;
        for id in ids {
            let Some(t) = self.tunnels.get_mut(&id) else {
                continue;
            };
            let mut respawn = false;
            match t.state.clone() {
                TunnelState::Opening | TunnelState::Open => {
                    if t.open_since.is_some_and(|o| now.duration_since(o) >= STABLE_OPEN) {
                        t.attempts = 0;
                    }
                    let exited = t.child.as_mut().is_none_or(|c| c.try_status().ok().flatten().is_some());
                    let stderr_done = t.stderr.as_ref().is_none_or(|s| s.lock().unwrap_or_else(|e| e.into_inner()).done);
                    if exited && !stderr_done && t.exit_polls < EXIT_GRACE_POLLS {
                        // lo stderr non e' ancora stato letto tutto: si guarda al prossimo giro
                        t.exit_polls += 1;
                    } else if exited {
                        let stderr = t.stderr.as_ref().map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).text()).unwrap_or_default();
                        let stderr = if stderr.trim().is_empty() { "ssh exited without messages".to_string() } else { last_lines(&stderr, 3) };
                        t.fail(stderr, now);
                        changed = true;
                    } else if t.state == TunnelState::Opening {
                        if ports_answer(&t.ports) {
                            t.state = TunnelState::Open;
                            t.open_since = Some(now);
                            changed = true;
                        } else if t.opened_at.is_some_and(|o| now.duration_since(o) >= OPEN_TIMEOUT) {
                            t.fail(format!("tunnel not ready within {} s", OPEN_TIMEOUT.as_secs()), now);
                            changed = true;
                        }
                    }
                }
                TunnelState::Failed { retry_at, .. } => respawn = t.wanted() && !t.config_error && now >= retry_at,
                TunnelState::Closed => {}
            }
            let idle_limit = Duration::from_secs(u64::from(t.config.idle_minutes) * 60);
            let idle = t.users == 0 && !t.held && !t.config.keep_open && t.idle_since.is_some_and(|s| now.duration_since(s) >= idle_limit);
            if idle && t.state != TunnelState::Closed {
                t.attempts = 0;
                t.stop(TunnelState::Closed);
                changed = true;
                respawn = false;
            }
            if respawn {
                self.spawn(&id, cx);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }
}

/// Ultimi `STDERR_TAIL` byte dello stderr di ssh; `done` a fine lettura (EOF).
#[derive(Default)]
struct StderrTail {
    buf: VecDeque<u8>,
    done: bool,
}

impl StderrTail {
    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
        let extra = self.buf.len().saturating_sub(STDERR_TAIL);
        self.buf.drain(..extra);
    }

    fn text(&self) -> String {
        let (a, b) = self.buf.as_slices();
        String::from_utf8_lossy(&[a, b].concat()).into_owned()
    }
}

/// Legge lo stderr di ssh fino alla fine in un thread proprio (finisce quando ssh chiude lo stderr),
/// cosi' ssh non si blocca mai scrivendo su una pipe piena.
/// Generica sul lettore: lo stderr di `util::command` e' `Async<File>` su macOS e
/// `ChildStderr` di smol su Linux.
fn drain_stderr<R: futures::AsyncRead + Unpin + Send + 'static>(stderr: Option<R>) -> Arc<Mutex<StderrTail>> {
    let tail = Arc::new(Mutex::new(StderrTail::default()));
    let Some(mut stderr) = stderr else {
        tail.lock().unwrap_or_else(|e| e.into_inner()).done = true;
        return tail;
    };
    let shared = tail.clone();
    let spawned = std::thread::Builder::new().name("superzed-ssh-stderr".into()).spawn(move || {
        smol::block_on(async {
            let mut chunk = [0u8; 4096];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => shared.lock().unwrap_or_else(|e| e.into_inner()).push(&chunk[..n]),
                }
            }
        });
        shared.lock().unwrap_or_else(|e| e.into_inner()).done = true;
    });
    if let Err(e) = spawned {
        log::error!("superzed: reading ssh stderr did not start: {e}");
        tail.lock().unwrap_or_else(|e| e.into_inner()).done = true;
    }
    tail
}

/// Errori permanenti della configurazione (niente tentativi finche' non cambia).
fn check_config(config: &TunnelConfig) -> Result<(), String> {
    if !safe_host(&config.host) {
        return Err(format!("tunnel: invalid host “{}”", config.host));
    }
    if config.forwards.is_empty() {
        return Err("tunnel: at least one forward is needed".into());
    }
    for f in &config.forwards {
        if !forward_host_ok(&f.remote_host) {
            return Err(if f.remote_host.contains(':') && !f.remote_host.starts_with('[') {
                format!("tunnel: invalid remote host “{}” (an IPv6 address goes in square brackets, e.g. [::1])", f.remote_host)
            } else {
                format!("tunnel: invalid remote host “{}”", f.remote_host)
            });
        }
        if f.remote_port == 0 {
            return Err("tunnel: remote port 0 is not valid".into());
        }
    }
    Ok(())
}

/// Host remoto di `-L`: nome o IPv4 senza `:`, oppure IPv6 tra parentesi quadre.
fn forward_host_ok(host: &str) -> bool {
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(ip) => ip.parse::<std::net::Ipv6Addr>().is_ok(),
        None => safe_host(host) && !host.contains(':'),
    }
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn ports_answer(ports: &[u16]) -> bool {
    !ports.is_empty() && ports.iter().all(|p| TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], *p)), CONNECT_PROBE).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::Forward;
    use crate::test_support::FixedTunnelCommand;
    use gpui::{AppContext as _, Entity, TestAppContext};

    fn config(keep_open: bool) -> TunnelConfig {
        TunnelConfig {
            host: "root@203.0.113.10".into(),
            forwards: vec![Forward { local_port: 0, remote_host: "localhost".into(), remote_port: 8123 }],
            keep_open,
            idle_minutes: 10,
        }
    }

    fn supervisor(cx: &mut TestAppContext, program: &str, args: &[&str]) -> (Entity<TunnelSupervisor>, Instant) {
        let command = FixedTunnelCommand::new(program, args);
        let sup = cx.new(|cx| TunnelSupervisor::new(command, false, cx));
        let t0 = Instant::now();
        sup.update(cx, |s, _| s.set_now_for_test(Some(t0)));
        (sup, t0)
    }

    fn wait_until(sup: &Entity<TunnelSupervisor>, cx: &mut TestAppContext, what: &str, pred: impl Fn(&TunnelSupervisor) -> bool) {
        for _ in 0..100 {
            sup.update(cx, |s, cx| s.poll(cx));
            if sup.read_with(cx, |s, _| pred(s)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{what}: non successo entro 5 s");
    }

    #[allow(clippy::disallowed_methods, reason="test sincrono su un processo reale: non gira sull'executor di gpui")]
    fn alive(pid: u32) -> bool {
        let out = std::process::Command::new("/bin/ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        !stat.trim().is_empty() && !stat.trim().starts_with('Z')
    }

    fn wait_dead(pid: u32) {
        for _ in 0..100 {
            if !alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("processo {pid} ancora vivo");
    }

    #[test]
    fn ssh_command_line() {
        assert_eq!(
            ssh_args(&config(false), &[51234]),
            ["-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=30", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-L", "127.0.0.1:51234:localhost:8123", "--", "root@203.0.113.10"]
        );
        assert_eq!((1..=8).map(|a| backoff(a).as_secs()).collect::<Vec<_>>(), [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff(200).as_secs(), 60);
        assert_eq!(rewrite_to_local("http://ch.internal:8123/", 51234), Ok("http://127.0.0.1:51234/".into()));
        assert_eq!(rewrite_to_local("https://zabbix.example", 4443), Ok("https://127.0.0.1:4443/".into()));
        assert!(rewrite_to_local("non un url", 1).is_err());
        assert_ne!(free_port().unwrap(), 0);
    }

    #[gpui::test]
    async fn a_failing_ssh_reports_stderr_and_retries_with_backoff(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (sup, t0) = supervisor(cx, "/bin/sh", &["-c", "echo 'root@203.0.113.10: Permission denied (publickey).' >&2; exit 255"]);
        sup.update(cx, |s, cx| s.acquire("t", &config(false), cx));
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Opening);
        wait_until(&sup, cx, "primo fallimento", |s| matches!(s.state("t"), TunnelState::Failed { .. }));
        let TunnelState::Failed { stderr, retry_at } = sup.read_with(cx, |s, _| s.state("t")) else { unreachable!() };
        assert!(stderr.contains("Permission denied (publickey)"), "{stderr}");
        assert_eq!(retry_at, t0 + Duration::from_secs(1));
        // prima di retry_at non riparte
        sup.update(cx, |s, cx| s.poll(cx));
        assert!(matches!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Failed { .. }));
        let t1 = t0 + Duration::from_secs(1);
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t1));
            s.poll(cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Opening, "riaperto");
        wait_until(&sup, cx, "secondo fallimento", |s| matches!(s.state("t"), TunnelState::Failed { .. }));
        let TunnelState::Failed { retry_at, .. } = sup.read_with(cx, |s, _| s.state("t")) else { unreachable!() };
        assert_eq!(retry_at, t1 + Duration::from_secs(2), "backoff raddoppiato");
        // nessuno lo usa piu': non riparte
        sup.update(cx, |s, cx| {
            s.release("t", cx);
            s.set_now_for_test(Some(t1 + Duration::from_secs(5)));
            s.poll(cx);
        });
        assert!(matches!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Failed { .. }));
    }

    #[gpui::test]
    async fn opens_when_the_local_port_answers_and_closes_after_idle_minutes(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (sup, t0) = supervisor(cx, "/bin/sleep", &["30"]);
        sup.update(cx, |s, cx| s.acquire("t", &config(false), cx));
        let ports = sup.read_with(cx, |s, _| s.ports("t"));
        assert_eq!(ports.len(), 1);
        assert_eq!(sup.read_with(cx, |s, _| s.local_ports("t")), None, "non ancora aperto");
        // la porta locale risponde (qui la apre il test al posto di ssh)
        let _listener = std::net::TcpListener::bind(("127.0.0.1", ports[0])).unwrap();
        sup.update(cx, |s, cx| s.poll(cx));
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Open);
        assert_eq!(sup.read_with(cx, |s, _| s.local_ports("t")), Some(ports));
        let pid = sup.read_with(cx, |s, _| s.pid("t")).unwrap();
        sup.update(cx, |s, cx| {
            s.release("t", cx);
            s.set_now_for_test(Some(t0 + Duration::from_secs(9 * 60)));
            s.poll(cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Open, "9 minuti: ancora aperto");
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0 + Duration::from_secs(10 * 60)));
            s.poll(cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Closed);
        wait_dead(pid);
    }

    #[gpui::test]
    async fn keep_open_survives_idle_and_close_all_kills_everything(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (sup, t0) = supervisor(cx, "/bin/sleep", &["30"]);
        sup.update(cx, |s, cx| {
            s.acquire("keep", &config(true), cx);
            s.acquire("other", &config(false), cx);
        });
        let listeners: Vec<std::net::TcpListener> = ["keep", "other"]
            .iter()
            .map(|id| std::net::TcpListener::bind(("127.0.0.1", sup.read_with(cx, |s, _| s.ports(id))[0])).unwrap())
            .collect();
        sup.update(cx, |s, cx| s.poll(cx));
        let pids: Vec<u32> = sup.read_with(cx, |s, _| [s.pid("keep"), s.pid("other")].into_iter().flatten().collect());
        assert_eq!(pids.len(), 2);
        sup.update(cx, |s, cx| {
            s.release("keep", cx);
            s.set_now_for_test(Some(t0 + Duration::from_secs(60 * 60)));
            s.poll(cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("keep")), TunnelState::Open, "keep_open: niente chiusura per inattivita'");
        assert_eq!(sup.read_with(cx, |s, _| s.state("other")), TunnelState::Open, "ancora usato");
        sup.update(cx, |s, cx| s.close_all(cx));
        for pid in pids {
            wait_dead(pid);
        }
        assert_eq!(sup.read_with(cx, |s, _| s.state("other")), TunnelState::Closed);
        drop(listeners);
    }

    #[gpui::test]
    async fn not_ready_in_time_is_a_failure_and_manual_hold_and_close(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (sup, t0) = supervisor(cx, "/bin/sleep", &["30"]);
        sup.update(cx, |s, cx| s.hold("t", &config(false), cx));
        assert_eq!(sup.read_with(cx, |s, _| (s.state("t"), s.users("t"))), (TunnelState::Opening, 0), "aperto a mano, senza pannelli");
        let pid = sup.read_with(cx, |s, _| s.pid("t")).unwrap();
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0 + OPEN_TIMEOUT));
            s.poll(cx);
        });
        let TunnelState::Failed { stderr, .. } = sup.read_with(cx, |s, _| s.state("t")) else { panic!("atteso Failed") };
        assert!(stderr.contains("not ready within 20 s"), "{stderr}");
        wait_dead(pid);
        sup.update(cx, |s, cx| s.close("t", cx));
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Closed);
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0 + Duration::from_secs(3600)));
            s.poll(cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Closed, "chiuso a mano: non riparte da solo");
    }

    #[gpui::test]
    async fn an_unsafe_host_never_starts_a_process(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (sup, _) = supervisor(cx, "/bin/sleep", &["30"]);
        let mut bad = config(false);
        bad.host = "-oProxyCommand=touch /tmp/pwned".into();
        sup.update(cx, |s, cx| s.acquire("t", &bad, cx));
        let TunnelState::Failed { stderr, .. } = sup.read_with(cx, |s, _| s.state("t")) else { panic!("atteso Failed") };
        assert!(stderr.contains("invalid host"), "{stderr}");
        assert_eq!(sup.read_with(cx, |s, _| s.pid("t")), None);
        let mut bad = config(false);
        bad.forwards[0].remote_host = "-x".into();
        sup.update(cx, |s, cx| s.hold("u", &bad, cx));
        assert!(matches!(sup.read_with(cx, |s, _| s.state("u")), TunnelState::Failed { .. }));
        assert_eq!(sup.read_with(cx, |s, _| s.pid("u")), None);
    }

    #[gpui::test]
    async fn a_config_error_does_not_retry_until_the_config_changes(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (sup, t0) = supervisor(cx, "/bin/sleep", &["30"]);
        let mut bad = config(false);
        bad.forwards[0].remote_host = "fe80::1".into();
        sup.update(cx, |s, cx| s.acquire("t", &bad, cx));
        let failed = sup.read_with(cx, |s, _| s.state("t"));
        let TunnelState::Failed { stderr, .. } = &failed else { panic!("atteso Failed") };
        assert!(stderr.contains("square brackets"), "{stderr}");
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0 + Duration::from_secs(3600)));
            s.poll(cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), failed, "errore di configurazione: nessun nuovo tentativo");
        // configurazione corretta (IPv6 tra parentesi quadre): si riprova
        let mut good = config(false);
        good.forwards[0].remote_host = "[::1]".into();
        sup.update(cx, |s, cx| {
            s.acquire("t", &good, cx);
            s.poll(cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Opening);
        assert!(sup.read_with(cx, |s, _| s.pid("t")).is_some());
        let pid = sup.read_with(cx, |s, _| s.pid("t")).unwrap();
        sup.update(cx, |s, cx| s.close("t", cx));
        wait_dead(pid);
        // riapertura a mano dopo un errore di configurazione: riprova subito
        sup.update(cx, |s, cx| s.hold("u", &bad, cx));
        let before = sup.read_with(cx, |s, _| s.state("u"));
        sup.update(cx, |s, cx| s.hold("u", &bad, cx));
        assert_ne!(sup.read_with(cx, |s, _| s.state("u")), before, "hold riprova (backoff avanzato)");
    }

    #[gpui::test]
    async fn a_busy_fixed_local_port_fails_without_spawning(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (sup, _) = supervisor(cx, "/bin/sleep", &["30"]);
        let busy = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = busy.local_addr().unwrap().port();
        let mut c = config(false);
        c.forwards[0].local_port = port;
        sup.update(cx, |s, cx| {
            s.acquire("t", &c, cx);
            s.poll(cx);
        });
        let TunnelState::Failed { stderr, .. } = sup.read_with(cx, |s, _| s.state("t")) else { panic!("atteso Failed, non Open grazie al listener altrui") };
        assert_eq!(stderr, format!("local port {port} in use"));
        assert_eq!(sup.read_with(cx, |s, _| s.pid("t")), None);
        drop(busy);
    }

    #[gpui::test]
    async fn stderr_is_drained_so_a_chatty_ssh_never_blocks(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("dopo-lo-stderr");
        // ~200 KB su stderr (piu' del buffer della pipe), poi un segno visibile e resta vivo
        let chatty = "s=x; for k in 1 2 3 4 5 6 7 8 9 10; do s=$s$s; done; i=0; \
            while [ $i -lt 200 ]; do printf '%s\\n' \"$s\" >&2; i=$((i+1)); done; \
            echo 'ultima riga' >&2; : > \"$1\"; exec /bin/sleep 30";
        let (sup, _) = supervisor(cx, "/bin/sh", &["-c", chatty, "sh", marker.to_str().unwrap()]);
        sup.update(cx, |s, cx| s.acquire("t", &config(false), cx));
        for _ in 0..100 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(marker.exists(), "ssh finto bloccato sullo stderr");
        sup.update(cx, |s, cx| s.poll(cx));
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Opening);
        let pid = sup.read_with(cx, |s, _| s.pid("t")).unwrap();
        assert!(alive(pid));
        sup.update(cx, |s, cx| s.close("t", cx));
        wait_dead(pid);

        // stesso fiume di stderr, poi esce: resta solo la coda (limitata)
        let dying = "s=x; for k in 1 2 3 4 5 6 7 8 9 10; do s=$s$s; done; i=0; \
            while [ $i -lt 200 ]; do printf '%s\\n' \"$s\" >&2; i=$((i+1)); done; \
            echo 'ultima riga' >&2; exit 255";
        let (sup, _) = supervisor(cx, "/bin/sh", &["-c", dying]);
        sup.update(cx, |s, cx| s.acquire("t", &config(false), cx));
        wait_until(&sup, cx, "uscita dopo lo stderr", |s| matches!(s.state("t"), TunnelState::Failed { .. }));
        let TunnelState::Failed { stderr, .. } = sup.read_with(cx, |s, _| s.state("t")) else { unreachable!() };
        assert!(stderr.ends_with("ultima riga"), "{}", &stderr[stderr.len().saturating_sub(80)..]);
        assert!(stderr.len() <= STDERR_TAIL, "{}", stderr.len());
    }

    #[gpui::test]
    async fn flapping_keeps_the_backoff_until_open_for_30_s(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        // "ssh" che resta su mezzo secondo e poi cade
        let (sup, t0) = supervisor(cx, "/bin/sleep", &["0.5"]);
        let open_now = |sup: &Entity<TunnelSupervisor>, cx: &mut TestAppContext| {
            let port = sup.read_with(cx, |s, _| s.ports("t"))[0];
            let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
            sup.update(cx, |s, cx| s.poll(cx));
            assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Open);
            listener
        };
        let retry_at = |sup: &Entity<TunnelSupervisor>, cx: &mut TestAppContext| {
            wait_until(sup, cx, "caduto", |s| matches!(s.state("t"), TunnelState::Failed { .. }));
            let TunnelState::Failed { retry_at, .. } = sup.read_with(cx, |s, _| s.state("t")) else { unreachable!() };
            retry_at
        };
        sup.update(cx, |s, cx| s.acquire("t", &config(false), cx));
        let _l1 = open_now(&sup, cx);
        assert_eq!(retry_at(&sup, cx), t0 + Duration::from_secs(1));
        let t1 = t0 + Duration::from_secs(1);
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t1));
            s.poll(cx);
        });
        let _l2 = open_now(&sup, cx);
        assert_eq!(retry_at(&sup, cx), t1 + Duration::from_secs(2), "aperto meno di 30 s: il backoff non riparte da 1 s");
        let t2 = t1 + Duration::from_secs(2);
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t2));
            s.poll(cx);
        });
        let _l3 = open_now(&sup, cx);
        sup.update(cx, |s, cx| {
            s.set_now_for_test(Some(t2 + Duration::from_secs(30)));
            s.poll(cx);
        });
        assert_eq!(retry_at(&sup, cx), t2 + Duration::from_secs(31), "aperto 30 s: backoff azzerato");
    }

    #[gpui::test]
    async fn reconfigure_restarts_a_running_tunnel_with_the_new_config(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let command = crate::test_support::RecordingTunnelCommand::new();
        let sup = cx.new(|cx| TunnelSupervisor::new(command.clone(), false, cx));
        let mut edited = config(false);
        edited.host = "root@10.0.0.2".into();
        // aperto a mano: riparte subito con la configurazione nuova
        sup.update(cx, |s, cx| s.hold("t", &config(false), cx));
        let old = sup.read_with(cx, |s, _| s.pid("t")).unwrap();
        sup.update(cx, |s, cx| s.reconfigure("t", &edited, cx));
        assert_eq!(command.hosts(), ["root@203.0.113.10", "root@10.0.0.2"]);
        assert_eq!(sup.read_with(cx, |s, _| s.state("t")), TunnelState::Opening);
        wait_dead(old);
        // stessa configurazione: niente riavvio
        sup.update(cx, |s, cx| s.reconfigure("t", &edited, cx));
        assert_eq!(command.hosts().len(), 2);
        // usato da un pannello e chiuso a mano: il prossimo ensure_open usa la configurazione nuova
        sup.update(cx, |s, cx| {
            s.acquire("u", &config(false), cx);
            s.close("u", cx);
            s.reconfigure("u", &edited, cx);
        });
        assert_eq!(sup.read_with(cx, |s, _| s.state("u")), TunnelState::Closed);
        sup.update(cx, |s, cx| s.ensure_open("u", cx));
        assert_eq!(command.hosts().last().map(String::as_str), Some("root@10.0.0.2"));
        // errore di configurazione corretto: si riprova subito, senza aspettare
        let mut bad = config(false);
        bad.host = "-bad".into();
        sup.update(cx, |s, cx| s.acquire("v", &bad, cx));
        assert!(sup.read_with(cx, |s, _| s.config_error("v")));
        sup.update(cx, |s, cx| s.reconfigure("v", &edited, cx));
        assert!(!sup.read_with(cx, |s, _| s.config_error("v")));
        assert_eq!(sup.read_with(cx, |s, _| s.state("v")), TunnelState::Opening);
        sup.update(cx, |s, cx| s.close_all(cx));
    }
}
