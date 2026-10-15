//! Store globale degli account: elenco (sistema + registro), quote in cache, stato
//! per account, polling, `rate_limits` della status line e notifica di quota bassa.
use crate::quota::{self, Quota};
use crate::scheduler::{self, Candidate, Strategy};
use crate::{Account, FetchOutcome, OauthToken, TokenSource, UsageFetcher};
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use collections::{HashMap, HashSet};
use futures::{
    FutureExt as _,
    channel::oneshot,
    future::{self, BoxFuture, Either},
};
use gpui::{App, AppContext as _, BackgroundExecutor, Context, Entity, EventEmitter, Global, Subscription, Task};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use superzed_sessions::{AccountLoad, RateLimits, SessionEvent, SessionStore};

pub const POLL_TICK: Duration = Duration::from_secs(60);
pub const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);
pub const MANUAL_MIN_AGE: Duration = Duration::from_secs(5 * 60);
/// Tetto alla lettura del token: largo, il Portachiavi puo' chiedere conferma all'utente.
pub const TOKEN_TIMEOUT: Duration = Duration::from_secs(120);
/// Tetto alla richiesta delle quote (oltre il timeout HTTP): un account appeso esce dal volo.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Tetto al backoff dopo un errore di rete/HTTP (1, 2, 4, … minuti).
pub const MAX_RETRY_DELAY_MINUTES: i64 = 15;
/// Le quote cambiano a ogni status line: la cache si scrive al piu' una volta ogni tanto,
/// fuori dal main thread.
pub const CACHE_WRITE_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub enum AccountStatus {
    /// Mai letto.
    Unknown,
    Ok,
    /// Nessuna credenziale nel Keychain: escluso dalla scelta automatica.
    NoToken,
    /// Token scaduto: restano le quote in cache, l'account resta usabile.
    TokenExpired,
    /// 401/403 dall'endpoint.
    Unavailable,
    Error(String),
}

impl AccountStatus {
    /// Testo del tooltip del problema, se c'e'.
    pub fn problem(&self) -> Option<String> {
        match self {
            AccountStatus::Unknown | AccountStatus::Ok => None,
            AccountStatus::NoToken => Some("No token in the Keychain: log in with this account".into()),
            AccountStatus::TokenExpired => Some("Token expired: quota from the last read".into()),
            AccountStatus::Unavailable => Some("Quota not available for this account (401/403)".into()),
            AccountStatus::Error(e) => Some(format!("Error reading the quota: {e}")),
        }
    }

    pub fn short(&self) -> &'static str {
        match self {
            AccountStatus::Unknown => "loading",
            AccountStatus::Ok => "ok",
            AccountStatus::NoToken => "not logged in",
            AccountStatus::TokenExpired => "token expired",
            AccountStatus::Unavailable => "not available",
            AccountStatus::Error(_) => "error",
        }
    }

    /// Serve il login: nessun token nel Keychain o login revocato (401/403). Un token
    /// scaduto no: l'account resta usabile con le quote in cache.
    pub fn needs_login(&self) -> bool {
        matches!(self, AccountStatus::NoToken | AccountStatus::Unavailable)
    }
}

pub enum AccountEvent {
    Changed,
}

pub struct AccountSources {
    pub tokens: Arc<dyn TokenSource>,
    pub fetcher: Arc<dyn UsageFetcher>,
}

pub struct AccountStore {
    /// Il sistema e' sempre il primo.
    accounts: Vec<Account>,
    registry_path: PathBuf,
    nodeterm_dir: PathBuf,
    /// Strategia della chat nuova, dal registro.
    strategy: Strategy,
    statuses: HashMap<String, AccountStatus>,
    quotas: HashMap<String, Quota>,
    /// Ultima lettura conclusa (non per errori di rete/HTTP: quelli vanno in `retry`).
    last_fetch: HashMap<String, DateTime<Utc>>,
    /// Dopo errori di rete/HTTP consecutivi: quanti e quando riprovare.
    retry: HashMap<String, Retry>,
    /// Ultime quote note su disco (`None`: non si salvano).
    quota_cache_path: Option<PathBuf>,
    /// Per account: tolto a fine lettura, anche per errore o tempo scaduto.
    in_flight: HashSet<String>,
    /// Una lettura del Portachiavi alla volta (un solo dialogo visibile): tenuto dalla
    /// lettura in corso fino al ritorno o al tempo scaduto.
    keychain_gate: Arc<futures::lock::Mutex<()>>,
    /// Account con `security` ancora aperto: tolto solo quando la lettura torna davvero,
    /// non dal tempo scaduto, cosi' non si accumulano dialoghi del Portachiavi.
    keychain_busy: Arc<Mutex<HashSet<String>>>,
    /// Chiavi `LowQuota::key` gia' notificate.
    warned: HashSet<String>,
    sources: AccountSources,
    test_now: Option<DateTime<Utc>>,
    /// Scrittura della cache in attesa o in corso (una alla volta).
    cache_write: Option<Task<()>>,
    /// Quote cambiate durante una scrittura in corso: se ne fa un'altra alla fine.
    cache_dirty: bool,
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, Debug)]
struct Retry {
    failures: u32,
    at: DateTime<Utc>,
}

/// Esito di `read_account`.
enum ReadOutcome {
    /// `security` dell'account ancora aperto: nessuna lettura nuova.
    KeychainBusy,
    Done {
        status: AccountStatus,
        value: Option<serde_json::Value>,
        /// Errore di rete/HTTP (non 401/403): si riprova con backoff.
        transient: bool,
    },
}

struct GlobalAccountStore(Entity<AccountStore>);
impl Global for GlobalAccountStore {}
impl EventEmitter<AccountEvent> for AccountStore {}

impl AccountStore {
    pub fn global(cx: &App) -> Option<Entity<AccountStore>> {
        cx.try_global::<GlobalAccountStore>().map(|g| g.0.clone())
    }

    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    pub fn strategy(&self) -> Strategy {
        self.strategy
    }

    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    pub fn system(&self) -> &Account {
        &self.accounts[0]
    }

    pub fn status(&self, id: &str) -> AccountStatus {
        self.statuses.get(id).cloned().unwrap_or(AccountStatus::Unknown)
    }

    /// Una lettura del Portachiavi per l'account e' ancora aperta (anche oltre il tempo).
    pub fn keychain_busy(&self, id: &str) -> bool {
        self.keychain_busy.lock().unwrap().contains(id)
    }

    pub fn quota(&self, id: &str) -> Option<&Quota> {
        self.quotas.get(id)
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.test_now.unwrap_or_else(Utc::now)
    }

    /// Solo test: orologio fisso (reset, cache di 5 min).
    pub fn set_now_for_test(&mut self, now: Option<DateTime<Utc>>) {
        self.test_now = now;
    }

    /// Legge token e quote degli account non letti da almeno `min_age` (e non gia' in volo).
    /// Dopo un errore di rete/HTTP l'account si rilegge allo scadere del backoff, oppure
    /// subito con un refresh manuale (`min_age <= MANUAL_MIN_AGE`). Senza backoff in corso,
    /// una finestra nota gia' resettata (in cache) rende l'account dovuto non appena
    /// passano i `MANUAL_MIN_AGE` minimi dall'ultima lettura, anche prima di `min_age`:
    /// altrimenti la barra resta a "100% left, Resets now" fino al prossimo poll di 15
    /// min. Con un backoff in corso (rete/HTTP fallita) resta il solo backoff, cosi' un
    /// account che fallisce non martella l'endpoint.
    pub fn refresh_due(&mut self, min_age: Duration, cx: &mut Context<Self>) {
        let now = self.now();
        let due: Vec<Account> = self
            .accounts
            .iter()
            .filter(|a| !self.in_flight.contains(&a.id))
            .filter(|a| match self.retry.get(&a.id) {
                Some(retry) => min_age <= MANUAL_MIN_AGE || now >= retry.at,
                None => {
                    let elapsed = self.last_fetch.get(&a.id).map(|t| (now - *t).to_std().unwrap_or_default());
                    elapsed.is_none_or(|e| e >= min_age)
                        || (elapsed.is_none_or(|e| e >= MANUAL_MIN_AGE) && self.has_passed_reset(&a.id, now))
                }
            })
            .cloned()
            .collect();
        for account in due {
            self.fetch(account, now, cx);
        }
    }

    /// Una finestra nota (5h, settimana o per modello) di questo account ha gia' un
    /// `resets_at` nel passato: la quota in cache e' scaduta e va riletta.
    fn has_passed_reset(&self, id: &str, now: DateTime<Utc>) -> bool {
        self.quotas.get(id).is_some_and(|q| {
            [q.session.as_ref(), q.weekly.as_ref()]
                .into_iter()
                .flatten()
                .chain(q.scoped.iter().map(|(_, l)| l))
                .any(|l| l.is_reset(now))
        })
    }

    /// C'e' almeno una finestra (5h, settimana o per modello, di un account qualsiasi) con
    /// un reset noto, passato o futuro: la sola condizione che rende utile il tick da 60s
    /// (`POLL_TICK`) sopra, che tiene giusto il conto alla rovescia dei reset in status bar
    /// e nella sezione «Limits».
    fn has_reset_to_show(&self) -> bool {
        self.quotas.values().any(|q| {
            [q.session.as_ref(), q.weekly.as_ref()]
                .into_iter()
                .flatten()
                .chain(q.scoped.iter().map(|(_, l)| l))
                .any(|l| l.resets_at.is_some())
        })
    }

    /// Un task per account: uno lento o appeso non ferma gli altri.
    fn fetch(&mut self, account: Account, now: DateTime<Utc>, cx: &mut Context<Self>) {
        let id = account.id.clone();
        self.in_flight.insert(id.clone());
        let tokens = self.sources.tokens.clone();
        let fetcher = self.sources.fetcher.clone();
        let busy = self.keychain_busy.clone();
        let gate = self.keychain_gate.clone();
        let executor = cx.background_executor().clone();
        // Keychain e HTTP fuori dal thread principale
        let work = cx.background_spawn(read_account(account, tokens, gate, busy, fetcher, now, executor));
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |s, cx| s.finish_fetch(&id, outcome, now, cx)).ok();
        })
        .detach();
    }

    fn finish_fetch(&mut self, id: &str, outcome: ReadOutcome, now: DateTime<Utc>, cx: &mut Context<Self>) {
        self.in_flight.remove(id);
        let (mut status, value, transient) = match outcome {
            ReadOutcome::Done { status, value, transient } => (status, value, transient),
            // si salta: uno stato buono resta, si riprova al prossimo tick
            ReadOutcome::KeychainBusy => {
                if matches!(self.status(id), AccountStatus::Unknown | AccountStatus::Error(_)) {
                    self.statuses.insert(id.to_string(), AccountStatus::Error("waiting for the Keychain".into()));
                    cx.emit(AccountEvent::Changed);
                    cx.notify();
                }
                return;
            }
        };
        if transient {
            let failures = self.retry.get(id).map_or(0, |r| r.failures) + 1;
            self.retry.insert(id.to_string(), Retry { failures, at: now + retry_delay(failures) });
        } else {
            self.retry.remove(id);
            self.last_fetch.insert(id.to_string(), now);
        }
        if let Some(v) = value {
            match quota::parse_usage(&v, now) {
                Some(q) => {
                    let merged = quota::merge(self.quotas.remove(id), q);
                    self.quotas.insert(id.to_string(), merged);
                    self.save_quota_cache(cx);
                }
                None => status = AccountStatus::Error("quota response not recognized".into()),
            }
        }
        self.statuses.insert(id.to_string(), status);
        self.check_low_quota(id, cx);
        cx.emit(AccountEvent::Changed);
        cx.notify();
    }

    /// Dopo `CACHE_WRITE_DELAY`, in background, con le quote di quel momento.
    fn save_quota_cache(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.quota_cache_path.clone() else {
            return;
        };
        if self.cache_write.is_some() {
            self.cache_dirty = true;
            return;
        }
        self.cache_write = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CACHE_WRITE_DELAY).await;
            let Ok(quotas) = this.update(cx, |s, _| {
                s.cache_dirty = false;
                s.quotas.clone()
            }) else {
                return;
            };
            let written = cx.background_spawn(async move { crate::quota_cache::write(&path, &quotas) }).await;
            if let Err(e) = written {
                log::warn!("superzed: quota cache not written: {e:#}");
            }
            this.update(cx, |s, cx| {
                s.cache_write = None;
                if std::mem::take(&mut s.cache_dirty) {
                    s.save_quota_cache(cx);
                }
            })
            .ok();
        }));
    }

    /// `rate_limits` di una chat dell'account: piu' recenti del fetch, vincono.
    pub fn merge_status_line(&mut self, account_id: &str, rl: &RateLimits, cx: &mut Context<Self>) {
        if self.account(account_id).is_none() {
            return;
        }
        let merged = quota::merge(self.quotas.remove(account_id), quota::from_status_line(rl, self.now()));
        self.quotas.insert(account_id.to_string(), merged);
        self.save_quota_cache(cx);
        self.check_low_quota(account_id, cx);
        cx.emit(AccountEvent::Changed);
        cx.notify();
    }

    fn check_low_quota(&mut self, id: &str, cx: &mut Context<Self>) {
        let (Some(q), Some(account)) = (self.quotas.get(id), self.account(id)) else {
            return;
        };
        let label = account.label.clone();
        let warnings = quota::low_quota_warnings(q, self.now());
        for w in warnings {
            if !self.warned.insert(w.key(id)) {
                continue;
            }
            if let Some(store) = superzed_notifications::NotificationStore::global(cx) {
                store.update(cx, |n, cx| {
                    n.add(
                        superzed_notifications::NewNotification {
                            severity: superzed_notifications::Severity::Warning,
                            title: format!("Quota almost used up: {label}"),
                            summary: w.summary(),
                            details: None,
                            suggested_prompt: None,
                            project_root: None,
                            source: "superzed".to_string(),
                        },
                        cx,
                    );
                });
            }
        }
    }

    /// Rilegge nodeterm, unisce per id (non toglie nulla), salva; torna quanti nuovi.
    pub fn reimport(&mut self, cx: &mut Context<Self>) -> usize {
        let imported = crate::registry::read_nodeterm(&self.nodeterm_dir);
        let (merged, added) = crate::registry::merge_import(&self.accounts[1..], imported);
        if let Err(e) = crate::registry::write_registry(&self.registry_path, self.strategy, &merged) {
            log::warn!("superzed: account registry not written: {e:#}");
        }
        self.accounts.truncate(1);
        self.accounts.extend(merged);
        self.refresh_due(Duration::ZERO, cx);
        cx.emit(AccountEvent::Changed);
        cx.notify();
        added
    }

    /// Esclude (o riammette) l'account dalla scelta automatica in `folder` e salva il registro.
    pub fn set_excluded_from(&mut self, account_id: &str, folder: &Path, excluded: bool, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let home = util::paths::home_dir();
        let account = self
            .accounts
            .iter_mut()
            .find(|a| a.id == account_id && !a.is_system())
            .with_context(|| format!("account {account_id} not in the registry"))?;
        account.set_excluded(folder, excluded, home);
        crate::registry::write_registry(&self.registry_path, self.strategy, &self.accounts[1..])?;
        cx.emit(AccountEvent::Changed);
        cx.notify();
        Ok(())
    }

    /// `project`: cartella del progetto, per le esclusioni `exclude_from` degli account.
    pub fn candidates(&self, loads: &HashMap<String, AccountLoad>, project: Option<&Path>) -> Vec<Candidate> {
        let home = util::paths::home_dir();
        self.accounts
            .iter()
            .map(|a| Candidate {
                id: a.id.clone(),
                quota: self.quotas.get(&a.id).cloned(),
                load: loads.get(&a.id).copied().unwrap_or_default(),
                usable: !matches!(self.status(&a.id), AccountStatus::NoToken | AccountStatus::Unavailable),
                excluded_here: project.is_some_and(|p| a.excluded_from(p, home)),
            })
            .collect()
    }

    /// Account per una chat nuova (nel progetto `project`) e se erano tutti esclusi.
    pub fn pick(&self, loads: &HashMap<String, AccountLoad>, project: Option<&Path>) -> (Account, bool) {
        let p = scheduler::pick(self.strategy, &self.candidates(loads, project), self.now());
        let account = self.account(&p.id).cloned().unwrap_or_else(|| self.system().clone());
        (account, p.all_excluded)
    }
}

/// Lettura del token: al massimo una per account alla volta (`busy`, liberato solo quando
/// `token` torna). Una sorgente bloccante gira su un thread dedicato.
fn read_token(
    account: Account,
    tokens: Arc<dyn TokenSource>,
    busy: Arc<Mutex<HashSet<String>>>,
) -> Option<BoxFuture<'static, anyhow::Result<Option<OauthToken>>>> {
    let id = account.id.clone();
    if !busy.lock().unwrap().insert(id.clone()) {
        return None;
    }
    if !tokens.needs_thread() {
        let token = tokens.token(&account);
        busy.lock().unwrap().remove(&id);
        return Some(future::ready(token).boxed());
    }
    let (tx, rx) = oneshot::channel();
    let thread_busy = busy.clone();
    let thread_id = id.clone();
    let spawned = std::thread::Builder::new().name("superzed-keychain".into()).spawn(move || {
        let token = tokens.token(&account);
        thread_busy.lock().unwrap().remove(&thread_id);
        tx.send(token).ok();
    });
    if let Err(e) = spawned {
        busy.lock().unwrap().remove(&id);
        return Some(future::ready(Err(anyhow::anyhow!("Keychain thread did not start: {e}"))).boxed());
    }
    Some(
        rx.map(|r| r.unwrap_or_else(|_| Err(anyhow::anyhow!("Keychain read interrupted"))))
            .boxed(),
    )
}

/// Attesa prima di riprovare dopo `failures` errori di rete/HTTP consecutivi.
fn retry_delay(failures: u32) -> chrono::Duration {
    let minutes = 1i64 << failures.saturating_sub(1).min(10);
    chrono::Duration::minutes(minutes.min(MAX_RETRY_DELAY_MINUTES))
}

/// Token e quote di un account, ognuno con il suo tetto. Gira sull'executor in background.
/// Le letture del token passano una alla volta da `gate` (un dialogo del Portachiavi alla
/// volta); le richieste HTTP restano in parallelo.
async fn read_account(
    account: Account,
    tokens: Arc<dyn TokenSource>,
    gate: Arc<futures::lock::Mutex<()>>,
    busy: Arc<Mutex<HashSet<String>>>,
    fetcher: Arc<dyn UsageFetcher>,
    now: DateTime<Utc>,
    executor: BackgroundExecutor,
) -> ReadOutcome {
    let done = |status, value, transient| ReadOutcome::Done { status, value, transient };
    let token = {
        let _turn = gate.lock().await;
        let Some(token) = read_token(account, tokens, busy) else {
            return ReadOutcome::KeychainBusy;
        };
        match future::select(token, executor.timer(TOKEN_TIMEOUT)).await {
            Either::Left((token, _)) => token,
            Either::Right(_) => {
                return done(AccountStatus::Error("timed out reading the Keychain".into()), None, false);
            }
        }
    };
    let token = match token {
        Err(e) => return done(AccountStatus::Error(format!("{e:#}")), None, false),
        Ok(None) => return done(AccountStatus::NoToken, None, false),
        Ok(Some(t)) if t.expires_at.is_some_and(|e| e <= now) => return done(AccountStatus::TokenExpired, None, false),
        Ok(Some(t)) => t,
    };
    match future::select(fetcher.fetch(token.access_token), executor.timer(FETCH_TIMEOUT)).await {
        Either::Left((FetchOutcome::Ok(v), _)) => done(AccountStatus::Ok, Some(v), false),
        Either::Left((FetchOutcome::Unauthorized, _)) => done(AccountStatus::Unavailable, None, false),
        Either::Left((FetchOutcome::Error(e), _)) => done(AccountStatus::Error(e), None, true),
        Either::Right(_) => done(AccountStatus::Error("timed out reading the quota".into()), None, true),
    }
}

pub(crate) fn new_store(
    cx: &mut App,
    registry_path: PathBuf,
    nodeterm_dir: PathBuf,
    quota_cache_path: Option<PathBuf>,
    system: Account,
    sources: AccountSources,
    poll: bool,
) -> Entity<AccountStore> {
    let mut accounts = vec![system];
    let registry = crate::registry::load_or_import(&registry_path, &nodeterm_dir);
    accounts.extend(registry.accounts);
    let quotas = quota_cache_path.as_deref().map(crate::quota_cache::read).unwrap_or_default();
    let store = cx.new(|cx| {
        let mut subscriptions = Vec::new();
        if let Some(sessions) = SessionStore::global(cx) {
            subscriptions.push(cx.subscribe(&sessions, |this: &mut AccountStore, sessions, event: &SessionEvent, cx| {
                // copiati fuori dal borrow dello store delle sessioni
                let found = match event {
                    SessionEvent::RateLimits { key } => sessions
                        .read(cx)
                        .get(key)
                        .and_then(|s| Some((s.account_id.clone(), s.rate_limits?))),
                    _ => None,
                };
                if let Some((id, rl)) = found {
                    this.merge_status_line(&id, &rl, cx);
                }
            }));
        }
        let poll = poll.then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(POLL_TICK).await;
                    let updated = this.update(cx, |s, cx| {
                        s.refresh_due(POLL_INTERVAL, cx);
                        // Task 20: il conto alla rovescia del reset resta giusto solo se si
                        // ridisegna almeno ogni minuto; inutile quando non c'e' nessun reset
                        // da mostrare.
                        if s.has_reset_to_show() {
                            cx.notify();
                        }
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            })
        });
        AccountStore {
            accounts,
            registry_path,
            nodeterm_dir,
            strategy: registry.scheduler,
            statuses: HashMap::default(),
            quotas,
            last_fetch: HashMap::default(),
            retry: HashMap::default(),
            quota_cache_path,
            in_flight: HashSet::default(),
            keychain_gate: Arc::default(),
            keychain_busy: Arc::default(),
            warned: HashSet::default(),
            sources,
            test_now: None,
            cache_write: None,
            cache_dirty: false,
            _poll: poll,
            _subscriptions: subscriptions,
        }
    });
    cx.set_global(GlobalAccountStore(store.clone()));
    store
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture_store, usage_json};
    use gpui::TestAppContext;
    use std::time::Duration;

    fn setup(
        cx: &mut TestAppContext,
        quotas: &[(f32, f32)],
    ) -> (Entity<AccountStore>, Arc<crate::test_support::FakeTokens>, Arc<crate::test_support::FakeFetcher>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (store, tokens, fetcher) = cx.update(|cx| {
            superzed_sessions::init_test(cx, dir.path().join("claude"), None);
            fixture_store(cx, dir.path(), quotas)
        });
        cx.run_until_parked();
        (store, tokens, fetcher, dir)
    }

    #[gpui::test]
    fn refresh_reads_tokens_and_quotas(cx: &mut TestAppContext) {
        let (store, _t, fetcher, _dir) = setup(cx, &[(30., 50.), (10., 90.)]);
        store.read_with(cx, |s, _| {
            assert_eq!(s.accounts().iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["system", "a1", "a2"]);
            assert_eq!(s.status("system"), AccountStatus::NoToken);
            assert_eq!(s.status("a1"), AccountStatus::Ok);
            assert_eq!(s.quota("a1").unwrap().session.as_ref().unwrap().used_percent, 30.);
            assert_eq!(s.quota("a2").unwrap().weekly.as_ref().unwrap().used_percent, 90.);
        });
        assert_eq!(fetcher.calls(), 2, "il sistema senza token non chiede nulla");
    }

    #[gpui::test]
    fn failures_become_statuses_and_keep_the_cache(cx: &mut TestAppContext) {
        let (store, tokens, fetcher, _dir) = setup(cx, &[(30., 50.), (10., 90.)]);
        fetcher.set("tok-a1", FetchOutcome::Unauthorized);
        fetcher.set("tok-a2", FetchOutcome::Error("HTTP 500".into()));
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            assert_eq!(s.status("a1"), AccountStatus::Unavailable);
            assert_eq!(s.status("a2"), AccountStatus::Error("HTTP 500".into()));
            assert!(s.quota("a1").is_some(), "la cache resta");
        });
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        tokens.set("a1", Some(crate::OauthToken { access_token: "tok-a1".into(), expires_at: Some(past) }));
        let before = fetcher.calls();
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        assert_eq!(store.read_with(cx, |s, _| s.status("a1")), AccountStatus::TokenExpired);
        assert_eq!(fetcher.calls(), before + 1, "con il token scaduto non si chiama l'endpoint (solo a2)");
        fetcher.set("tok-a2", FetchOutcome::Ok(serde_json::json!({"boh": 1})));
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        assert!(matches!(store.read_with(cx, |s, _| s.status("a2")), AccountStatus::Error(_)), "formato sconosciuto");
    }

    // Review Focus 5
    #[gpui::test]
    fn refresh_is_deduplicated_and_cached_for_five_minutes(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _t, fetcher) = cx.update(|cx| fixture_store(cx, dir.path(), &[(1., 1.), (2., 2.)]));
        let t0 = chrono::Utc::now();
        store.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0));
            s.refresh_due(Duration::ZERO, cx); // gia' in volo dal fixture
            s.refresh_due(MANUAL_MIN_AGE, cx);
        });
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), 2, "una richiesta per account");
        store.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0 + chrono::Duration::minutes(2)));
            s.refresh_due(MANUAL_MIN_AGE, cx);
        });
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), 2, "Aggiorna entro 5 min: cache");
        store.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0 + chrono::Duration::minutes(6)));
            s.refresh_due(MANUAL_MIN_AGE, cx);
            s.refresh_due(POLL_INTERVAL, cx); // il poll non e' ancora dovuto
        });
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), 4);
    }

    // final8 #3: dopo un reset passato la barra non deve restare a "100% left, Resets
    // now" fino al prossimo poll di 15 min; l'account torna dovuto non appena passano
    // i 5 min minimi di `MANUAL_MIN_AGE` (throttle: niente refetch a ogni tick di 60s).
    #[gpui::test]
    fn a_passed_reset_is_due_after_manual_min_age_not_immediately(cx: &mut TestAppContext) {
        let (store, _t, fetcher, _dir) = setup(cx, &[(90., 10.)]);
        let after_setup = fetcher.calls();
        let t0 = chrono::Utc::now();
        store.update(cx, |s, _| s.set_now_for_test(Some(t0)));
        // la finestra di 5h resetta fra 30s
        status_line(cx, "a1", 90., (t0 + chrono::Duration::seconds(30)).timestamp());

        let poll_at = |cx: &mut TestAppContext, at: chrono::DateTime<Utc>| {
            store.update(cx, |s, cx| {
                s.set_now_for_test(Some(at));
                s.refresh_due(POLL_INTERVAL, cx);
            });
            cx.run_until_parked();
            fetcher.calls()
        };

        assert_eq!(
            poll_at(cx, t0 + chrono::Duration::minutes(2)),
            after_setup,
            "reset gia' passato ma non ancora i 5 min minimi: resta in cache"
        );
        assert_eq!(
            poll_at(cx, t0 + chrono::Duration::minutes(5)),
            after_setup + 1,
            "reset passato: dovuto prima dei 15 min normali del poll"
        );
    }

    // final8 #3: un reset gia' noto e passato non deve far saltare il backoff dopo un
    // errore di rete/HTTP (altrimenti un account che fallisce martella l'endpoint).
    #[gpui::test]
    fn a_passed_reset_does_not_bypass_the_retry_backoff(cx: &mut TestAppContext) {
        let (store, _t, fetcher, _dir) = setup(cx, &[(90., 10.)]);
        fetcher.set("tok-a1", FetchOutcome::Error("HTTP 500".into()));
        let t0 = chrono::Utc::now();
        store.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0));
            s.refresh_due(Duration::ZERO, cx);
        });
        cx.run_until_parked();
        // a1 e' ora in backoff (1 min) dopo l'errore transitorio; la quota in cache ha
        // gia' un reset passato.
        status_line(cx, "a1", 90., (t0 - chrono::Duration::seconds(1)).timestamp());
        let after_error = fetcher.calls();
        store.update(cx, |s, cx| {
            s.set_now_for_test(Some(t0 + chrono::Duration::seconds(30)));
            s.refresh_due(POLL_INTERVAL, cx);
        });
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), after_error, "ancora in backoff nonostante il reset passato");
    }

    // Un account appeso non blocca gli altri e non resta "in volo" per sempre.
    #[gpui::test]
    fn a_hung_account_does_not_block_the_others(cx: &mut TestAppContext) {
        let (store, _t, fetcher, _dir) = setup(cx, &[(1., 1.), (2., 2.)]);
        fetcher.hang("tok-a1", true);
        fetcher.set("tok-a2", FetchOutcome::Ok(usage_json(40., 40.)));
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), 4);
        store.read_with(cx, |s, _| {
            assert_eq!(s.quota("a2").unwrap().session.as_ref().unwrap().used_percent, 40., "a2 aggiornato");
            assert_eq!(s.status("a1"), AccountStatus::Ok, "a1 ancora in volo: stato precedente");
        });
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), 5, "a1 in volo non si richiede, a2 si'");
        cx.executor().advance_clock(FETCH_TIMEOUT + Duration::from_secs(1));
        cx.run_until_parked();
        assert!(
            matches!(store.read_with(cx, |s, _| s.status("a1")), AccountStatus::Error(e) if e.contains("timed out")),
            "scaduto il tempo, a1 esce dal volo con un errore"
        );
        fetcher.hang("tok-a1", false);
        fetcher.set("tok-a1", FetchOutcome::Ok(usage_json(70., 70.)));
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            assert_eq!(s.status("a1"), AccountStatus::Ok);
            assert_eq!(s.quota("a1").unwrap().session.as_ref().unwrap().used_percent, 70.);
        });
    }

    /// Registra se token e fetch girano sul thread principale.
    struct MainThreadProbe {
        executor: gpui::BackgroundExecutor,
        on_main: std::sync::Mutex<Vec<(&'static str, bool)>>,
    }

    impl TokenSource for MainThreadProbe {
        fn token(&self, _: &Account) -> anyhow::Result<Option<crate::OauthToken>> {
            self.on_main.lock().unwrap().push(("token", self.executor.is_main_thread()));
            Ok(Some(crate::OauthToken { access_token: "t".into(), expires_at: None }))
        }

        // sul thread dedicato `is_main_thread` del test non avrebbe senso
        fn needs_thread(&self) -> bool {
            false
        }
    }

    impl UsageFetcher for MainThreadProbe {
        fn fetch(&self, _: String) -> futures::future::BoxFuture<'static, FetchOutcome> {
            self.on_main.lock().unwrap().push(("fetch", self.executor.is_main_thread()));
            Box::pin(async { FetchOutcome::Ok(usage_json(1., 1.)) })
        }
    }

    #[gpui::test]
    fn keychain_and_http_run_off_the_main_thread(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let probe = Arc::new(MainThreadProbe { executor: cx.executor(), on_main: Default::default() });
        let store = cx.update(|cx| {
            let sources = AccountSources { tokens: probe.clone(), fetcher: probe.clone() };
            crate::init_test(cx, dir.path().join("accounts.json"), dir.path().join("nodeterm"), "sistema".into(), sources)
        });
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        assert_eq!(*probe.on_main.lock().unwrap(), [("token", false), ("fetch", false)]);
        assert_eq!(store.read_with(cx, |s, _| s.status("system")), AccountStatus::Ok);
    }

    #[gpui::test]
    fn status_line_rate_limits_update_the_account(cx: &mut TestAppContext) {
        let (store, _t, _f, _dir) = setup(cx, &[(30., 50.)]);
        let sessions = cx.update(|cx| superzed_sessions::SessionStore::global(cx).unwrap());
        sessions.update(cx, |s, cx| {
            s.register("k".into(), "a1".into(), None, superzed_sessions::Phase::New, Default::default(), cx);
            s.set_account_id("k", "a1", cx);
            s.apply(
                superzed_ipc::IpcMessage {
                    channel: superzed_ipc::Channel::Status,
                    node_key: "k".into(),
                    payload: serde_json::json!({"rate_limits": {"five_hour": {"used_percentage": 64, "resets_at": 4070908800i64}}}),
                },
                cx,
            );
        });
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            let q = s.quota("a1").unwrap();
            assert_eq!(q.session.as_ref().unwrap().used_percent, 64., "status line piu' recente del fetch");
            assert_eq!(q.weekly.as_ref().unwrap().used_percent, 50., "settimana dal fetch");
        });
    }

    #[gpui::test]
    fn low_quota_notifies_once_per_window(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let notifications = cx.update(|cx| superzed_notifications::init_test(cx, dir.path().join("notifications")));
        let (store, _t, _f) = cx.update(|cx| fixture_store(cx, dir.path(), &[(95., 10.)]));
        cx.run_until_parked();
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        let list = notifications.read_with(cx, |n, _| n.list(&Default::default()));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Quota almost used up: a1");
        assert_eq!(list[0].severity, superzed_notifications::Severity::Warning);
        assert_eq!(list[0].source, "superzed");
        assert!(list[0].summary.starts_with("5h: 5% left"), "{}", list[0].summary);
    }

    fn status_line(cx: &mut TestAppContext, account: &str, used: f32, resets_at: i64) {
        let sessions = cx.update(|cx| superzed_sessions::SessionStore::global(cx).unwrap());
        sessions.update(cx, |s, cx| {
            s.register("k".into(), account.into(), None, superzed_sessions::Phase::New, Default::default(), cx);
            s.set_account_id("k", account, cx);
            s.apply(
                superzed_ipc::IpcMessage {
                    channel: superzed_ipc::Channel::Status,
                    node_key: "k".into(),
                    payload: serde_json::json!({"rate_limits": {"five_hour": {"used_percentage": used, "resets_at": resets_at}}}),
                },
                cx,
            );
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn status_line_can_trigger_the_low_quota_notification(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let notifications = cx.update(|cx| superzed_notifications::init_test(cx, dir.path().join("notifications")));
        let (_store, _t, _f, _dir) = setup(cx, &[(30., 10.)]);
        assert!(notifications.read_with(cx, |n, _| n.list(&Default::default())).is_empty());
        status_line(cx, "a1", 96., 4070908800);
        let list = notifications.read_with(cx, |n, _| n.list(&Default::default()));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Quota almost used up: a1");
        assert!(list[0].summary.starts_with("5h: 4% left"), "{}", list[0].summary);
    }

    #[gpui::test]
    fn endpoint_and_status_line_share_the_low_quota_notification(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let notifications = cx.update(|cx| superzed_notifications::init_test(cx, dir.path().join("notifications")));
        // endpoint: reset 2099-01-01T00:00:00Z = 4070908800
        let (_store, _t, _f, _dir) = setup(cx, &[(95., 10.)]);
        assert_eq!(notifications.read_with(cx, |n, _| n.list(&Default::default())).len(), 1);
        status_line(cx, "a1", 97., 4070908800 + 20);
        assert_eq!(notifications.read_with(cx, |n, _| n.list(&Default::default())).len(), 1, "stessa finestra");
    }

    /// Token che si blocca finche' il test non lo libera (come un dialogo del Portachiavi).
    #[derive(Default)]
    struct BlockingTokens {
        calls: std::sync::atomic::AtomicUsize,
        released: std::sync::atomic::AtomicBool,
        gate: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl TokenSource for BlockingTokens {
        fn token(&self, _: &Account) -> anyhow::Result<Option<crate::OauthToken>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if !self.released.load(std::sync::atomic::Ordering::SeqCst)
                && let Some(rx) = self.gate.lock().unwrap().as_ref()
            {
                rx.recv().ok();
            }
            Ok(Some(crate::OauthToken { access_token: "t".into(), expires_at: None }))
        }
    }

    /// Esegue il ciclo finche' `done` (i thread veri del Portachiavi non sono deterministici).
    fn wait_until(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
        for _ in 0..500 {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("condizione mai raggiunta");
    }

    #[gpui::test]
    fn a_stuck_keychain_read_is_not_repeated_until_it_returns(cx: &mut TestAppContext) {
        // il Portachiavi gira su un thread vero
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tokens = Arc::new(BlockingTokens::default());
        *tokens.gate.lock().unwrap() = Some(rx);
        let fetcher = Arc::new(crate::test_support::FakeFetcher::default());
        fetcher.set("t", FetchOutcome::Ok(usage_json(1., 1.)));
        let store = cx.update(|cx| {
            let sources = AccountSources { tokens: tokens.clone(), fetcher: fetcher.clone() };
            crate::init_test(cx, dir.path().join("accounts.json"), dir.path().join("nodeterm"), "sistema".into(), sources)
        });
        let calls = |t: &BlockingTokens| t.calls.load(std::sync::atomic::Ordering::SeqCst);
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        wait_until(cx, |_| calls(&tokens) == 1);
        cx.executor().advance_clock(TOKEN_TIMEOUT + Duration::from_secs(1));
        cx.run_until_parked();
        assert!(
            matches!(store.read_with(cx, |s, _| s.status("system")), AccountStatus::Error(e) if e.contains("timed out")),
            "la lettura esce dal volo per tempo scaduto"
        );
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(50));
        cx.run_until_parked();
        assert_eq!(calls(&tokens), 1, "security ancora aperto: non se ne lancia un altro");
        assert_eq!(store.read_with(cx, |s, _| s.status("system")), AccountStatus::Error("waiting for the Keychain".into()));
        tokens.released.store(true, std::sync::atomic::Ordering::SeqCst);
        tx.send(()).unwrap();
        wait_until(cx, |cx| store.read_with(cx, |s, _| !s.keychain_busy("system")));
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        wait_until(cx, |cx| store.read_with(cx, |s, _| s.status("system") == AccountStatus::Ok));
        assert_eq!(calls(&tokens), 2);
    }

    #[gpui::test]
    fn pick_uses_quotas_and_session_loads(cx: &mut TestAppContext) {
        let (store, _t, fetcher, _dir) = setup(cx, &[(10., 10.), (20., 20.)]);
        let mut loads = collections::HashMap::default();
        let (a, all) = store.read_with(cx, |s, _| s.pick(&loads, None));
        assert_eq!((a.id.as_str(), all), ("a1", false));
        loads.insert("a1".to_string(), superzed_sessions::AccountLoad { working: 1, recent: 0 });
        assert_eq!(store.read_with(cx, |s, _| s.pick(&loads, None)).0.id, "a2");
        fetcher.set("tok-a1", FetchOutcome::Ok(usage_json(100., 10.)));
        fetcher.set("tok-a2", FetchOutcome::Ok(usage_json(99., 10.)));
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        let (a, all) = store.read_with(cx, |s, _| s.pick(&Default::default(), None));
        assert_eq!((a.id.as_str(), all), ("system", true));
    }

    #[gpui::test]
    fn pick_and_the_picker_skip_an_account_excluded_from_the_project_folder(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let acme = dir.path().join("Progetti/Acme");
        let (store, _t, _f) = cx.update(|cx| {
            superzed_sessions::init_test(cx, dir.path().join("claude"), None);
            crate::test_support::fixture_store_with(cx, dir.path(), &[(10., 10.), (20., 20.)], |registry| {
                registry.accounts[0].exclude_from = vec![acme.clone()];
            })
        });
        cx.run_until_parked();
        let loads = collections::HashMap::default();
        store.read_with(cx, |s, _| {
            assert_eq!(s.pick(&loads, None).0.id, "a1", "nessun progetto: a1 ha piu' margine");
            assert_eq!(s.pick(&loads, Some(dir.path().join("Progetti/superzed").as_path())).0.id, "a1");
            assert_eq!(s.pick(&loads, Some(acme.as_path())).0.id, "a2");
            assert_eq!(s.pick(&loads, Some(acme.join("appunti").as_path())).0.id, "a2");
            let candidates = s.candidates(&loads, Some(acme.as_path()));
            assert!(candidates[1].excluded_here && !candidates[2].excluded_here);
        });
    }

    #[gpui::test]
    fn the_registry_scheduler_key_drives_the_pick(cx: &mut TestAppContext) {
        // a1: 5h 40% left, week 90%; a2: 5h 80% left, week 30%
        let quotas = [(60., 10.), (20., 70.)];
        let dir = tempfile::tempdir().unwrap();
        let (weekly, _t, _f) = cx.update(|cx| {
            superzed_sessions::init_test(cx, dir.path().join("claude"), None);
            crate::test_support::fixture_store_with(cx, dir.path(), &quotas, |_| {})
        });
        cx.run_until_parked();
        weekly.read_with(cx, |s, _| {
            assert_eq!(s.strategy(), crate::scheduler::Strategy::Weekly);
            assert_eq!(s.pick(&Default::default(), None).0.id, "a2", "settimana nel 2099: conta il rimanente 5h");
        });
        let dir = tempfile::tempdir().unwrap();
        let (headroom, _t, _f) = cx.update(|cx| {
            crate::test_support::fixture_store_with(cx, dir.path(), &quotas, |registry| {
                registry.scheduler = crate::scheduler::Strategy::Headroom;
            })
        });
        cx.run_until_parked();
        headroom.read_with(cx, |s, _| {
            assert_eq!(s.strategy(), crate::scheduler::Strategy::Headroom);
            assert_eq!(s.pick(&Default::default(), None).0.id, "a1", "margine minimo: 40 contro 30");
        });
    }

    fn quota_cache_path(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("accounts-quota-cache.json")
    }

    #[gpui::test]
    fn cached_quotas_survive_a_restart(cx: &mut TestAppContext) {
        let (_store, _t, _f, dir) = setup(cx, &[(30., 50.), (10., 90.)]);
        assert!(!quota_cache_path(dir.path()).exists(), "la scrittura aspetta CACHE_WRITE_DELAY");
        cx.executor().advance_clock(CACHE_WRITE_DELAY);
        cx.run_until_parked();
        let saved = std::fs::read_to_string(quota_cache_path(dir.path())).expect("cache scritta dopo il fetch");
        assert!(!saved.contains("tok-"), "mai token nella cache: {saved}");
        // riavvio: a1 con il token scaduto non legge nulla, restano le quote dell'ultima volta
        let tokens = Arc::new(crate::test_support::FakeTokens::default());
        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        tokens.set("a1", Some(crate::OauthToken { access_token: "tok-a1".into(), expires_at: Some(past) }));
        let fetcher = Arc::new(crate::test_support::FakeFetcher::default());
        let store = cx.update(|cx| {
            let sources = AccountSources { tokens: tokens.clone(), fetcher: fetcher.clone() };
            crate::init_test(cx, dir.path().join("accounts.json"), dir.path().join("nodeterm"), "sistema".into(), sources)
        });
        store.read_with(cx, |s, _| {
            assert_eq!(s.quota("a1").unwrap().session.as_ref().unwrap().used_percent, 30., "caricate all'avvio");
            assert_eq!(s.quota("a2").unwrap().weekly.as_ref().unwrap().used_percent, 90.);
        });
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        assert_eq!(fetcher.calls(), 0);
        store.read_with(cx, |s, _| {
            assert_eq!(s.status("a1"), AccountStatus::TokenExpired);
            let entry = crate::status_item::status_entry(s.account("a1").unwrap(), s.quota("a1"), &s.status("a1"), s.now());
            assert_eq!(entry.text, "a1 5h 70% · week 50%");
            assert!(entry.problem.unwrap().contains("Token expired"));
            let candidates = s.candidates(&Default::default(), None);
            assert!(candidates[1].quota.is_some(), "lo scheduler usa la cache, non il 50 sconosciuto");
        });
    }

    #[gpui::test]
    fn a_broken_quota_cache_is_ignored(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(quota_cache_path(dir.path()), "{ rotto").unwrap();
        let sources = AccountSources {
            tokens: Arc::new(crate::test_support::FakeTokens::default()),
            fetcher: Arc::new(crate::test_support::FakeFetcher::default()),
        };
        let store = cx.update(|cx| {
            crate::init_test(cx, dir.path().join("accounts.json"), dir.path().join("nodeterm"), "sistema".into(), sources)
        });
        assert!(store.read_with(cx, |s, _| s.quota("system").is_none()));
    }

    #[gpui::test]
    fn status_line_quotas_are_cached_too(cx: &mut TestAppContext) {
        let (_store, _t, _f, dir) = setup(cx, &[(30., 50.)]);
        // la scrittura dopo il fetch di setup e' gia' in attesa: 64 e 65 la marcano soltanto
        status_line(cx, "a1", 64., 4070908800);
        status_line(cx, "a1", 65., 4070908800);
        cx.executor().advance_clock(CACHE_WRITE_DELAY);
        cx.run_until_parked();
        status_line(cx, "a1", 66., 4070908800);
        let session = || {
            let saved: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(quota_cache_path(dir.path())).unwrap()).unwrap();
            saved.pointer("/accounts/a1/session/used_percent").and_then(|v| v.as_f64())
        };
        assert_eq!(session(), Some(65.), "l'ultimo valore prima della scrittura");
        cx.executor().advance_clock(CACHE_WRITE_DELAY);
        cx.run_until_parked();
        assert_eq!(session(), Some(66.));
    }

    #[gpui::test]
    fn unavailable_accounts_are_not_picked(cx: &mut TestAppContext) {
        let (store, _t, fetcher, _dir) = setup(cx, &[(10., 10.), (20., 20.)]);
        fetcher.set("tok-a1", FetchOutcome::Unauthorized);
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            assert_eq!(s.status("a1"), AccountStatus::Unavailable);
            assert!(!s.candidates(&Default::default(), None)[1].usable);
            assert_eq!(s.pick(&Default::default(), None).0.id, "a2", "a1 ha quote migliori ma il login e' revocato");
        });
    }

    /// Conta quante letture del token sono aperte insieme (thread veri, come `security`).
    #[derive(Default)]
    struct ConcurrencyTokens {
        open: std::sync::atomic::AtomicUsize,
        max_open: std::sync::atomic::AtomicUsize,
    }

    impl TokenSource for ConcurrencyTokens {
        fn token(&self, account: &Account) -> anyhow::Result<Option<crate::OauthToken>> {
            use std::sync::atomic::Ordering::SeqCst;
            let open = self.open.fetch_add(1, SeqCst) + 1;
            self.max_open.fetch_max(open, SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(30));
            self.open.fetch_sub(1, SeqCst);
            Ok(Some(crate::OauthToken { access_token: format!("tok-{}", account.id), expires_at: None }))
        }
    }

    #[gpui::test]
    fn keychain_reads_run_one_account_at_a_time(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let accounts: Vec<Account> = ["a1", "a2", "a3"]
            .iter()
            .map(|id| Account { id: id.to_string(), label: id.to_string(), email: None, config_dir: Some(dir.path().join(id)), exclude_from: Vec::new() })
            .collect();
        crate::registry::write_registry(&dir.path().join("accounts.json"), Default::default(), &accounts).unwrap();
        let tokens = Arc::new(ConcurrencyTokens::default());
        let fetcher = Arc::new(crate::test_support::FakeFetcher::default());
        for id in ["system", "a1", "a2", "a3"] {
            fetcher.set(&format!("tok-{id}"), FetchOutcome::Ok(usage_json(1., 1.)));
        }
        let store = cx.update(|cx| {
            let sources = AccountSources { tokens: tokens.clone(), fetcher: fetcher.clone() };
            crate::init_test(cx, dir.path().join("accounts.json"), dir.path().join("nodeterm"), "sistema".into(), sources)
        });
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        wait_until(cx, |cx| {
            store.read_with(cx, |s, _| s.accounts().iter().all(|a| s.status(&a.id) == AccountStatus::Ok))
        });
        assert_eq!(tokens.max_open.load(std::sync::atomic::Ordering::SeqCst), 1, "un dialogo del Portachiavi alla volta");
    }

    #[gpui::test]
    fn transient_errors_retry_with_backoff(cx: &mut TestAppContext) {
        let (store, _t, fetcher, _dir) = setup(cx, &[(1., 1.)]);
        assert_eq!(fetcher.calls(), 1);
        fetcher.set("tok-a1", FetchOutcome::Error("HTTP 500".into()));
        let t0 = chrono::Utc::now() + chrono::Duration::minutes(20);
        let poll_at = |cx: &mut TestAppContext, at: chrono::DateTime<Utc>, min_age: Duration| {
            store.update(cx, |s, cx| {
                s.set_now_for_test(Some(at));
                s.refresh_due(min_age, cx);
            });
            cx.run_until_parked();
            fetcher.calls()
        };
        let secs = |s: i64| chrono::Duration::seconds(s);
        assert_eq!(poll_at(cx, t0, POLL_INTERVAL), 2, "poll dovuto: errore");
        assert_eq!(poll_at(cx, t0 + secs(30), POLL_INTERVAL), 2, "backoff di 1 min");
        assert_eq!(poll_at(cx, t0 + secs(60), POLL_INTERVAL), 3, "riprova dopo 1 min, non 15");
        assert_eq!(poll_at(cx, t0 + secs(60 + 90), POLL_INTERVAL), 3, "poi 2 min");
        assert_eq!(poll_at(cx, t0 + secs(60 + 120), POLL_INTERVAL), 4);
        assert_eq!(poll_at(cx, t0 + secs(60 + 120 + 60), POLL_INTERVAL), 4, "poi 4 min");
        assert_eq!(poll_at(cx, t0 + secs(60 + 120 + 61), MANUAL_MIN_AGE), 5, "Aggiorna dopo un errore rilegge sempre");
        fetcher.set("tok-a1", FetchOutcome::Ok(usage_json(3., 3.)));
        assert_eq!(poll_at(cx, t0 + secs(60 + 120 + 62), MANUAL_MIN_AGE), 6);
        assert_eq!(store.read_with(cx, |s, _| s.status("a1")), AccountStatus::Ok);
        assert_eq!(poll_at(cx, t0 + secs(60 + 120 + 122), MANUAL_MIN_AGE), 6, "riuscito: di nuovo la cache di 5 min");
        assert_eq!(poll_at(cx, t0 + secs(60 + 120 + 62 + 14 * 60), POLL_INTERVAL), 6, "e il poll di 15 min");
    }

    #[test]
    fn backoff_doubles_up_to_fifteen_minutes() {
        let mins: Vec<i64> = (1..=7).map(|n| retry_delay(n).num_minutes()).collect();
        assert_eq!(mins, [1, 2, 4, 8, 15, 15, 15]);
    }

    // Task 20: il tick da 60s (`POLL_TICK`) ridisegna status bar e sezione Limits solo se
    // c'e' almeno un reset da mostrare (altrimenti il conto alla rovescia non serve).
    #[gpui::test]
    fn the_minute_tick_redraws_only_when_a_reset_is_pending(cx: &mut TestAppContext) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let store = cx.update(|cx| {
            superzed_sessions::init_test(cx, dir.path().join("claude"), None);
            let registry = dir.path().join("accounts.json");
            crate::registry::write_registry(&registry, Default::default(), &[]).unwrap();
            let sources = AccountSources {
                tokens: Arc::new(crate::test_support::FakeTokens::default()),
                fetcher: Arc::new(crate::test_support::FakeFetcher::default()),
            };
            new_store(cx, registry, dir.path().join("nodeterm"), None, Account::system("sistema".into()), sources, true)
        });
        cx.run_until_parked();
        // consuma la prima lettura dovuta subito (senza dati, quindi senza reset): il tick
        // periodico che segue non deve piu' rileggere per altri 15 minuti.
        store.update(cx, |s, cx| s.refresh_due(Duration::ZERO, cx));
        cx.run_until_parked();

        let notified = Arc::new(AtomicUsize::new(0));
        let n = notified.clone();
        cx.update(|cx| cx.observe(&store, move |_, _| { n.fetch_add(1, Ordering::SeqCst); }).detach());

        cx.executor().advance_clock(POLL_TICK);
        cx.run_until_parked();
        assert_eq!(notified.load(Ordering::SeqCst), 0, "senza reset da mostrare il tick non ridisegna");

        status_line(cx, "system", 90., (Utc::now() + chrono::Duration::hours(1)).timestamp());
        notified.store(0, Ordering::SeqCst);
        cx.executor().advance_clock(POLL_TICK);
        cx.run_until_parked();
        assert!(notified.load(Ordering::SeqCst) >= 1, "con un reset da mostrare il tick ridisegna");
    }

    #[gpui::test]
    fn a_busy_keychain_keeps_the_previous_status(cx: &mut TestAppContext) {
        let (store, _t, _f, _dir) = setup(cx, &[(1., 1.)]);
        store.update(cx, |s, cx| {
            s.keychain_busy.lock().unwrap().extend(["a1".to_string(), "system".to_string()]);
            s.statuses.remove("system");
            s.refresh_due(Duration::ZERO, cx);
        });
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            assert_eq!(s.status("a1"), AccountStatus::Ok, "Aggiorna non cancella uno stato buono");
            assert_eq!(s.status("system"), AccountStatus::Error("waiting for the Keychain".into()));
        });
    }

    #[gpui::test]
    fn set_excluded_from_updates_the_pick_and_the_registry_file(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _t, _f) = cx.update(|cx| {
            superzed_sessions::init_test(cx, dir.path().join("claude"), None);
            crate::test_support::fixture_store(cx, dir.path(), &[(1., 1.), (2., 2.)])
        });
        cx.run_until_parked();
        let folder = dir.path().join("Progetti/appunti");
        std::fs::create_dir_all(&folder).unwrap();
        let changed = std::rc::Rc::new(std::cell::Cell::new(0));
        let _sub = cx.update(|cx| {
            let changed = changed.clone();
            cx.subscribe(&store, move |_, _: &AccountEvent, _| changed.set(changed.get() + 1))
        });
        store.update(cx, |s, cx| s.set_excluded_from("a1", &folder, true, cx)).unwrap();
        assert_eq!(changed.get(), 1, "Changed emesso");
        store.read_with(cx, |s, _| {
            let home = util::paths::home_dir();
            assert!(s.account("a1").unwrap().excludes_folder(&folder, home));
            let candidates = s.candidates(&HashMap::default(), Some(folder.join("src").as_path()));
            assert!(candidates.iter().find(|c| c.id == "a1").unwrap().excluded_here);
        });
        let saved = crate::registry::read_registry(&dir.path().join("accounts.json")).unwrap().unwrap();
        assert_eq!(saved.accounts[0].exclude_from, vec![folder.clone()], "salvato");
        assert!(saved.accounts[1].exclude_from.is_empty());

        store.update(cx, |s, cx| s.set_excluded_from("a1", &folder, false, cx)).unwrap();
        let saved = crate::registry::read_registry(&dir.path().join("accounts.json")).unwrap().unwrap();
        assert!(saved.accounts[0].exclude_from.is_empty(), "tolta");
        let err = store.update(cx, |s, cx| s.set_excluded_from("system", &folder, true, cx)).unwrap_err();
        assert!(err.to_string().contains("system"), "{err}");
    }

    #[gpui::test]
    fn reimport_merges_by_id_without_deleting(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (store, _t, _f) = cx.update(|cx| {
            superzed_sessions::init_test(cx, dir.path().join("claude"), None);
            crate::test_support::fixture_store_with(cx, dir.path(), &[(1., 1.), (2., 2.)], |registry| {
                registry.scheduler = crate::scheduler::Strategy::Headroom;
            })
        });
        cx.run_until_parked();
        let nt = dir.path().join("nodeterm");
        std::fs::create_dir_all(&nt).unwrap();
        std::fs::write(
            nt.join("settings.json"),
            r#"{"claudeAccounts":[{"id":"a1","label":"nuovo@x.it","pending":false},{"id":"a3","label":"a3","pending":false}]}"#,
        )
        .unwrap();
        let added = store.update(cx, |s, cx| s.reimport(cx));
        assert_eq!(added, 1);
        store.read_with(cx, |s, _| {
            assert_eq!(s.accounts().iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["system", "a1", "a2", "a3"]);
            assert_eq!(s.account("a1").unwrap().label, "nuovo");
        });
        let saved = crate::registry::read_registry(&dir.path().join("accounts.json")).unwrap().unwrap();
        assert_eq!(saved.accounts.len(), 3);
        assert_eq!(saved.scheduler, crate::scheduler::Strategy::Headroom, "il reimport conserva la strategia");
    }
}
