//! Token e quote finti per i test (anche di altri crate, feature `test-support`).
use crate::{Account, AccountSources, AccountStore, FetchOutcome, OauthToken, TokenSource, UsageFetcher, registry::Registry};
use collections::HashSet;
use futures::future::BoxFuture;
use gpui::{App, Entity};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Token per id di account; un account assente non ha token.
#[derive(Default)]
pub struct FakeTokens(pub Mutex<HashMap<String, OauthToken>>);

impl FakeTokens {
    pub fn set(&self, id: &str, token: Option<OauthToken>) {
        let mut map = self.0.lock().unwrap();
        match token {
            Some(t) => map.insert(id.to_string(), t),
            None => map.remove(id),
        };
    }
}

impl TokenSource for FakeTokens {
    fn token(&self, account: &Account) -> anyhow::Result<Option<OauthToken>> {
        Ok(self.0.lock().unwrap().get(&account.id).cloned())
    }

    fn needs_thread(&self) -> bool {
        false
    }
}

/// Risposte per token; conta le chiamate. Un token in `hanging` non risponde mai.
#[derive(Default)]
pub struct FakeFetcher {
    pub responses: Mutex<HashMap<String, FetchOutcome>>,
    pub hanging: Mutex<HashSet<String>>,
    pub calls: AtomicUsize,
}

impl FakeFetcher {
    pub fn set(&self, token: &str, outcome: FetchOutcome) {
        self.responses.lock().unwrap().insert(token.to_string(), outcome);
    }

    /// `true`: le richieste con questo token restano appese per sempre.
    pub fn hang(&self, token: &str, hang: bool) {
        let mut set = self.hanging.lock().unwrap();
        if hang {
            set.insert(token.to_string());
        } else {
            set.remove(token);
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl UsageFetcher for FakeFetcher {
    fn fetch(&self, token: String) -> BoxFuture<'static, FetchOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hanging.lock().unwrap().contains(&token) {
            return Box::pin(futures::future::pending());
        }
        let out = self
            .responses
            .lock()
            .unwrap()
            .get(&token)
            .cloned()
            .unwrap_or(FetchOutcome::Error("sconosciuto".into()));
        Box::pin(async move { out })
    }
}

pub fn usage_json(session: f32, weekly: f32) -> serde_json::Value {
    serde_json::json!({"limits": [
        {"kind": "session", "percent": session, "resets_at": "2099-01-01T00:00:00Z"},
        {"kind": "weekly_all", "percent": weekly, "resets_at": "2099-01-06T00:00:00Z"}
    ]})
}

/// Come `usage_json`, con uno o piu' limiti per modello (`weekly_scoped`, es. "Fable").
pub fn usage_json_scoped(session: f32, weekly: f32, scoped: &[(&str, f32)]) -> serde_json::Value {
    let mut limits = vec![
        serde_json::json!({"kind": "session", "percent": session, "resets_at": "2099-01-01T00:00:00Z"}),
        serde_json::json!({"kind": "weekly_all", "percent": weekly, "resets_at": "2099-01-06T00:00:00Z"}),
    ];
    for (name, percent) in scoped {
        limits.push(serde_json::json!({
            "kind": "weekly_scoped",
            "percent": percent,
            "resets_at": "2099-01-06T00:00:00Z",
            "scope": {"model": {"display_name": name}}
        }));
    }
    serde_json::json!({"limits": limits})
}

/// Registro con `a1..an` (config dir `dir/cfg-aN`, token `tok-aN`, quote `(5h%, settimana%)` usate);
/// il sistema (etichetta "sistema") senza token. Chiede subito un refresh: il chiamante
/// fa `run_until_parked`.
pub fn fixture_store(
    cx: &mut App,
    dir: &Path,
    quotas: &[(f32, f32)],
) -> (Entity<AccountStore>, Arc<FakeTokens>, Arc<FakeFetcher>) {
    let labels: Vec<String> = (1..=quotas.len()).map(|i| format!("a{i}")).collect();
    fixture_store_labeled(cx, dir, &labels, quotas)
}

/// Come `fixture_store`, con l'etichetta di ogni account (l'id resta `aN`).
pub fn fixture_store_labeled(
    cx: &mut App,
    dir: &Path,
    labels: &[String],
    quotas: &[(f32, f32)],
) -> (Entity<AccountStore>, Arc<FakeTokens>, Arc<FakeFetcher>) {
    fixture_store_full(cx, dir, labels, quotas, |_| {})
}

/// Come `fixture_store`, ritoccando il registro (strategia, `exclude_from`) prima di scriverlo.
pub fn fixture_store_with(
    cx: &mut App,
    dir: &Path,
    quotas: &[(f32, f32)],
    adjust: impl FnOnce(&mut Registry),
) -> (Entity<AccountStore>, Arc<FakeTokens>, Arc<FakeFetcher>) {
    let labels: Vec<String> = (1..=quotas.len()).map(|i| format!("a{i}")).collect();
    fixture_store_full(cx, dir, &labels, quotas, adjust)
}

fn fixture_store_full(
    cx: &mut App,
    dir: &Path,
    labels: &[String],
    quotas: &[(f32, f32)],
    adjust: impl FnOnce(&mut Registry),
) -> (Entity<AccountStore>, Arc<FakeTokens>, Arc<FakeFetcher>) {
    assert_eq!(labels.len(), quotas.len(), "un'etichetta per account");
    let tokens = Arc::new(FakeTokens::default());
    let fetcher = Arc::new(FakeFetcher::default());
    let mut accounts = Vec::new();
    for (i, (session, weekly)) in quotas.iter().enumerate() {
        let id = format!("a{}", i + 1);
        let cfg = dir.join(format!("cfg-{id}"));
        std::fs::create_dir_all(&cfg).unwrap();
        tokens.set(&id, Some(OauthToken { access_token: format!("tok-{id}"), expires_at: None }));
        fetcher.set(&format!("tok-{id}"), FetchOutcome::Ok(usage_json(*session, *weekly)));
        accounts.push(Account {
            id: id.clone(),
            label: labels[i].clone(),
            email: Some(format!("{id}@x.it")),
            config_dir: Some(cfg),
            exclude_from: Vec::new(),
        });
    }
    let mut file = Registry { scheduler: Default::default(), accounts };
    adjust(&mut file);
    let registry = dir.join("accounts.json");
    crate::registry::write_registry(&registry, file.scheduler, &file.accounts).unwrap();
    let sources = AccountSources { tokens: tokens.clone(), fetcher: fetcher.clone() };
    let store = crate::init_test(cx, registry, dir.join("nodeterm"), "sistema".into(), sources);
    store.update(cx, |s, cx| s.refresh_due(std::time::Duration::ZERO, cx));
    (store, tokens, fetcher)
}
