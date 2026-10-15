//! Account claude di superzed: registro importato da nodeterm, quote lette dal
//! Keychain e dall'endpoint `oauth/usage`, scheduler della chat nuova, picker e
//! status bar. Non dipende dal canvas.
pub mod credentials;
pub mod limits_tooltip;
pub mod meter;
pub mod picker;
pub mod quota;
pub mod quota_cache;
pub mod registry;
pub mod scheduler;
pub mod status_item;
pub mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod usage;

pub use credentials::{KeychainTokenSource, OauthToken, TokenSource};
pub use quota::{Limit, Quota};
pub use registry::{Account, SYSTEM_ID};
pub use scheduler::{Candidate, Strategy};
pub use status_item::QuotaStatusItem;
pub use store::{AccountEvent, AccountSources, AccountStatus, AccountStore};
pub use usage::{FetchOutcome, HttpUsageFetcher, UsageFetcher};

use collections::HashMap;
use gpui::{App, Entity, actions};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use superzed_sessions::{AccountLoad, SessionStore};

actions!(
    superzed,
    [
        /// Rilegge le quote degli account (entro 5 minuti dall'ultima lettura usa la cache).
        RefreshQuotas,
        /// Reimporta gli account claude da nodeterm (unisce per id, non toglie nulla).
        ReimportAccounts,
    ]
);

/// Carico attuale per account, dalle sessioni aperte.
pub fn current_loads(cx: &App) -> HashMap<String, AccountLoad> {
    SessionStore::global(cx)
        .map(|s| s.read(cx).account_loads(std::time::Instant::now()))
        .unwrap_or_default()
}

/// Account dello scheduler per una chat nuova nel progetto `project` (`None` senza store)
/// e se erano tutti esclusi.
pub fn pick_for_new_chat(cx: &App, project: Option<&Path>) -> Option<(Account, bool)> {
    let store = AccountStore::global(cx)?;
    Some(store.read(cx).pick(&current_loads(cx), project))
}

/// Dopo `superzed_sessions::init` (le quote seguono anche la status line delle chat).
pub fn init(cx: &mut App) {
    let home = util::paths::home_dir();
    let system = Account::system(superzed_sessions::launch::account_label(None, home));
    let sources = AccountSources {
        tokens: Arc::new(KeychainTokenSource),
        fetcher: Arc::new(HttpUsageFetcher::new(cx.http_client())),
    };
    let store = store::new_store(
        cx,
        registry::registry_path(),
        registry::nodeterm_dir(home),
        Some(quota_cache::cache_path()),
        system,
        sources,
        true,
    );
    store.update(cx, |s, cx| s.refresh_due(store::POLL_INTERVAL, cx));
    cx.on_action(|_: &RefreshQuotas, cx| {
        if let Some(s) = AccountStore::global(cx) {
            s.update(cx, |s, cx| s.refresh_due(store::MANUAL_MIN_AGE, cx));
        }
    });
    cx.on_action(|_: &ReimportAccounts, cx| {
        if let Some(s) = AccountStore::global(cx) {
            let added = s.update(cx, |s, cx| s.reimport(cx));
            log::info!("superzed: accounts reimported from nodeterm, new: {added}");
        }
    });
}

/// Store senza polling ne' fetch iniziale (i test chiamano `refresh_due`); la cache
/// delle quote sta accanto al registro (`accounts-quota-cache.json`).
pub fn init_test(
    cx: &mut App,
    registry: PathBuf,
    nodeterm_dir: PathBuf,
    system_label: String,
    sources: AccountSources,
) -> Entity<AccountStore> {
    let cache = registry.with_file_name(quota_cache::FILE_NAME);
    store::new_store(cx, registry, nodeterm_dir, Some(cache), Account::system(system_label), sources, false)
}
