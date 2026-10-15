//! Registro degli account (`~/.config/superzed/accounts.json`), importato da nodeterm
//! alla prima apertura e poi con `superzed::ReimportAccounts`. La chiave `scheduler`
//! (`weekly`, `headroom`, `session`) sceglie la strategia della chat nuova.
use crate::scheduler::Strategy;
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SYSTEM_ID: &str = superzed_sessions::SYSTEM_ACCOUNT;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    /// Etichetta corta (parte locale dell'email, o l'etichetta scelta in nodeterm).
    pub label: String,
    #[serde(default)]
    pub email: Option<String>,
    /// `None` = account di sistema (`~/.claude`).
    #[serde(default)]
    pub config_dir: Option<PathBuf>,
    /// Cartelle (e sottocartelle, `~` ammesso) in cui la chat nuova non lo sceglie da sola.
    /// Si scrive a mano nel registro; il reimport da nodeterm lo conserva.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_from: Vec<PathBuf>,
}

impl Account {
    pub fn system(label: String) -> Self {
        Self { id: SYSTEM_ID.to_string(), label, email: None, config_dir: None, exclude_from: Vec::new() }
    }

    pub fn is_system(&self) -> bool {
        self.id == SYSTEM_ID
    }

    /// `project` e' una delle cartelle di `exclude_from` o una loro sottocartella.
    pub fn excluded_from(&self, project: &Path, home: &Path) -> bool {
        let project = canonical_or_same(project);
        self.exclude_from.iter().any(|dir| project.starts_with(canonical_or_same(&expand_home(dir, home))))
    }

    /// `folder` stessa (non una sua cartella madre) e' in `exclude_from`.
    pub fn excludes_folder(&self, folder: &Path, home: &Path) -> bool {
        let folder = canonical_or_same(folder);
        self.exclude_from.iter().any(|dir| canonical_or_same(&expand_home(dir, home)) == folder)
    }

    /// Mette o toglie `folder` da `exclude_from`; scritta con `~` al posto della home,
    /// come si farebbe a mano nel registro.
    pub fn set_excluded(&mut self, folder: &Path, excluded: bool, home: &Path) {
        let target = canonical_or_same(folder);
        self.exclude_from.retain(|dir| canonical_or_same(&expand_home(dir, home)) != target);
        if excluded {
            self.exclude_from.push(contract_home(folder, home));
        }
    }
}

fn contract_home(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => Path::new("~").join(rest),
        _ => path.to_path_buf(),
    }
}

fn expand_home(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// Link simbolici risolti quando la cartella esiste, cosi' le due parti si confrontano uguali.
fn canonical_or_same(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    version: u32,
    /// Nome della strategia; assente o ignoto → `weekly` (il file non e' corrotto).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduler: Option<String>,
    accounts: Vec<Account>,
}

/// Contenuto del registro: strategia della chat nuova e account (senza il sistema).
#[derive(Clone, Debug, PartialEq)]
pub struct Registry {
    pub scheduler: Strategy,
    pub accounts: Vec<Account>,
}

pub fn nodeterm_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support/node-terminal")
}

pub fn registry_path() -> PathBuf {
    paths::config_dir().join("accounts.json")
}

/// Etichetta corta: l'email si riduce alla parte locale; vuota → email, poi id.
pub fn short_label(label: &str, email: Option<&str>, id: &str) -> String {
    let local = |s: &str| s.split('@').next().unwrap_or(s).trim().to_string();
    let label = label.trim();
    if !label.is_empty() {
        return if label.contains('@') { local(label) } else { label.to_string() };
    }
    match email.map(str::trim).filter(|e| !e.is_empty()) {
        Some(e) => local(e),
        None => id.chars().take(8).collect(),
    }
}

/// Account claude di nodeterm: saltati i `pending` e i remoti (`host`); `configDir`
/// preso com'e' (il servizio del Keychain dipende dalla stringa esatta).
pub fn import_nodeterm(settings_json: &str, nodeterm_dir: &Path) -> Vec<Account> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(settings_json) else {
        return Vec::new();
    };
    let Some(list) = v.get("claudeAccounts").and_then(|l| l.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|a| {
            let id = a.get("id")?.as_str().filter(|s| !s.is_empty())?.to_string();
            if a.get("pending").and_then(|p| p.as_bool()) == Some(true) {
                return None;
            }
            if a.get("host").is_some_and(|h| !h.is_null() && h.as_str() != Some("")) {
                return None;
            }
            let email = a.get("email").and_then(|e| e.as_str()).map(str::to_string);
            let label = short_label(a.get("label").and_then(|l| l.as_str()).unwrap_or(""), email.as_deref(), &id);
            let config_dir = match a.get("configDir").and_then(|c| c.as_str()).filter(|c| !c.is_empty()) {
                Some(dir) => PathBuf::from(dir),
                None => nodeterm_dir.join("claude-accounts").join(&id),
            };
            Some(Account { id, label, email, config_dir: Some(config_dir), exclude_from: Vec::new() })
        })
        .collect()
}

pub fn read_nodeterm(nodeterm_dir: &Path) -> Vec<Account> {
    std::fs::read_to_string(nodeterm_dir.join("settings.json"))
        .map(|text| import_nodeterm(&text, nodeterm_dir))
        .unwrap_or_default()
}

/// `Ok(None)`: file mancante. `Err`: file illeggibile o JSON non valido.
pub fn read_registry(path: &Path) -> anyhow::Result<Option<Registry>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let file: RegistryFile = serde_json::from_str(&text).with_context(|| format!("{} is not valid", path.display()))?;
    let scheduler = match file.scheduler.as_deref() {
        None => Strategy::default(),
        Some(name) => Strategy::parse(name).unwrap_or_else(|| {
            log::warn!("superzed: unknown scheduler {name:?} in {}: using {}", path.display(), Strategy::default().name());
            Strategy::default()
        }),
    };
    let accounts = file.accounts.into_iter().filter(|a| !a.is_system() && !a.id.is_empty()).collect();
    Ok(Some(Registry { scheduler, accounts }))
}

/// Scrittura atomica (file temporaneo accanto + rename).
pub fn write_registry(path: &Path, scheduler: Strategy, accounts: &[Account]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = RegistryFile { version: 1, scheduler: Some(scheduler.name().to_string()), accounts: accounts.to_vec() };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Unisce per id: gli esistenti prendono i campi importati (ma tengono le proprie
/// esclusioni, che nodeterm non conosce), i nuovi vanno in coda, nessuno viene tolto.
pub fn merge_import(existing: &[Account], imported: Vec<Account>) -> (Vec<Account>, usize) {
    let mut merged = existing.to_vec();
    let mut added = 0;
    for account in imported {
        match merged.iter_mut().find(|a| a.id == account.id) {
            Some(slot) => *slot = Account { exclude_from: std::mem::take(&mut slot.exclude_from), ..account },
            None => {
                merged.push(account);
                added += 1;
            }
        }
    }
    (merged, added)
}

/// Registro esistente; al primo avvio (file mancante) import da nodeterm e scrittura.
/// Un registro corrotto non si riscrive: si usano gli account di nodeterm solo in memoria.
pub fn load_or_import(registry: &Path, nodeterm_dir: &Path) -> Registry {
    match read_registry(registry) {
        Ok(Some(file)) => file,
        Ok(None) => {
            let accounts = read_nodeterm(nodeterm_dir);
            let scheduler = Strategy::default();
            if !accounts.is_empty()
                && let Err(e) = write_registry(registry, scheduler, &accounts)
            {
                log::warn!("superzed: account registry not written: {e:#}");
            }
            Registry { scheduler, accounts }
        }
        Err(e) => {
            log::warn!("superzed: {e:#}: using the nodeterm accounts without rewriting the registry");
            Registry { scheduler: Strategy::default(), accounts: read_nodeterm(nodeterm_dir) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SETTINGS: &str = r#"{
        "usagePercentMode": "remaining",
        "claudeAccounts": [
            {"id": "ff87", "label": "j.doe@example.com", "pending": false, "createdAt": 1, "email": "j.doe@example.com"},
            {"id": "pend", "label": "nuovo@x.it", "pending": true, "createdAt": 2, "email": "nuovo@x.it"},
            {"id": "remo", "label": "remoto", "pending": false, "createdAt": 3, "email": "r@x.it", "host": "srv"},
            {"id": "own", "label": "Lavoro", "pending": false, "createdAt": 4, "email": "m@x.it", "configDir": "/Users/u/cfg dir/"},
            {"id": "", "label": "senza id"}
        ]
    }"#;

    #[test]
    fn import_skips_pending_and_remote_and_keeps_config_dir_verbatim() {
        let got = import_nodeterm(SETTINGS, Path::new("/nt"));
        assert_eq!(
            got,
            vec![
                Account {
                    id: "ff87".into(),
                    label: "j.doe".into(),
                    email: Some("j.doe@example.com".into()),
                    config_dir: Some("/nt/claude-accounts/ff87".into()),
                    exclude_from: Vec::new(),
                },
                Account {
                    id: "own".into(),
                    label: "Lavoro".into(),
                    email: Some("m@x.it".into()),
                    config_dir: Some("/Users/u/cfg dir/".into()),
                    exclude_from: Vec::new(),
                },
            ]
        );
        assert_eq!(nodeterm_dir(Path::new("/Users/u")), PathBuf::from("/Users/u/Library/Application Support/node-terminal"));
    }

    #[test]
    fn short_labels() {
        assert_eq!(short_label("j.doe@example.com", None, "id"), "j.doe");
        assert_eq!(short_label("Lavoro", Some("m@x.it"), "id"), "Lavoro");
        assert_eq!(short_label("  ", Some("m@x.it"), "id"), "m");
        assert_eq!(short_label("", None, "abcdef0123"), "abcdef01");
    }

    // Review Focus 1
    #[test]
    fn missing_or_malformed_nodeterm_gives_no_accounts() {
        assert!(import_nodeterm("non json", Path::new("/nt")).is_empty());
        assert!(import_nodeterm(r#"{"claudeAccounts": 3}"#, Path::new("/nt")).is_empty());
        assert!(read_nodeterm(Path::new("/nessuna/cartella")).is_empty());
    }

    #[test]
    fn first_start_imports_and_writes_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let nt = dir.path().join("nt");
        std::fs::create_dir_all(&nt).unwrap();
        std::fs::write(nt.join("settings.json"), SETTINGS).unwrap();
        let reg = dir.path().join("config/accounts.json");
        let list = load_or_import(&reg, &nt);
        assert_eq!(list.accounts.len(), 2);
        assert_eq!(read_registry(&reg).unwrap().unwrap().accounts, list.accounts, "scritto al primo avvio");
        // poi il registro vince su nodeterm
        std::fs::write(nt.join("settings.json"), r#"{"claudeAccounts": []}"#).unwrap();
        assert_eq!(load_or_import(&reg, &nt), list);
    }

    #[test]
    fn nothing_to_import_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("accounts.json");
        assert!(load_or_import(&reg, &dir.path().join("nt")).accounts.is_empty());
        assert!(!reg.exists(), "si riprova al prossimo avvio");
    }

    // Review Focus 1
    #[test]
    fn corrupt_registry_is_not_overwritten_and_nodeterm_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let nt = dir.path().join("nt");
        std::fs::create_dir_all(&nt).unwrap();
        std::fs::write(nt.join("settings.json"), SETTINGS).unwrap();
        let reg = dir.path().join("accounts.json");
        std::fs::write(&reg, "{ rotto").unwrap();
        assert!(read_registry(&reg).is_err());
        assert_eq!(load_or_import(&reg, &nt).accounts.len(), 2);
        assert_eq!(std::fs::read_to_string(&reg).unwrap(), "{ rotto", "il file dell'utente resta com'e'");
    }

    #[test]
    fn merge_updates_by_id_adds_new_and_never_deletes() {
        let old = vec![
            Account { id: "a".into(), label: "vecchio".into(), email: None, config_dir: Some("/a".into()), exclude_from: Vec::new() },
            Account { id: "b".into(), label: "b".into(), email: None, config_dir: Some("/b".into()), exclude_from: Vec::new() },
        ];
        let imported = vec![
            Account { id: "a".into(), label: "nuovo".into(), email: Some("a@x".into()), config_dir: Some("/a2".into()), exclude_from: Vec::new() },
            Account { id: "c".into(), label: "c".into(), email: None, config_dir: Some("/c".into()), exclude_from: Vec::new() },
        ];
        let (merged, added) = merge_import(&old, imported);
        assert_eq!(merged.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(merged[0].label, "nuovo");
        assert_eq!(merged[0].config_dir.as_deref(), Some(Path::new("/a2")));
        assert_eq!(added, 1);
    }

    #[test]
    fn registry_never_holds_the_system_account() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("accounts.json");
        std::fs::write(&reg, r#"{"version":1,"accounts":[{"id":"system","label":"x"},{"id":"a","label":"a","config_dir":"/a"}]}"#).unwrap();
        let list = read_registry(&reg).unwrap().unwrap().accounts;
        assert_eq!(list.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["a"]);
        assert!(Account::system("sistema".into()).is_system());
    }

    fn with_exclusions(id: &str, exclude_from: &[&str]) -> Account {
        Account {
            id: id.into(),
            label: id.into(),
            email: None,
            config_dir: Some(format!("/cfg/{id}").into()),
            exclude_from: exclude_from.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn exclude_from_round_trips_and_is_omitted_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("accounts.json");
        let accounts = vec![with_exclusions("a", &["~/Progetti/Acme", "/abs/x"]), with_exclusions("b", &[])];
        write_registry(&reg, Strategy::Weekly, &accounts).unwrap();
        let text = std::fs::read_to_string(&reg).unwrap();
        assert_eq!(text.matches("exclude_from").count(), 1, "solo chi ce l'ha lo scrive: {text}");
        assert_eq!(read_registry(&reg).unwrap().unwrap().accounts, accounts);
    }

    #[test]
    fn set_excluded_adds_the_folder_with_the_tilde_and_removes_it_however_it_was_written() {
        let home = Path::new("/Users/u");
        let mut a = with_exclusions("a", &[]);
        a.set_excluded(Path::new("/Users/u/Progetti/appunti"), true, home);
        assert_eq!(a.exclude_from, vec![PathBuf::from("~/Progetti/appunti")]);
        a.set_excluded(Path::new("/Users/u/Progetti/appunti"), true, home);
        assert_eq!(a.exclude_from.len(), 1, "non si duplica");
        a.set_excluded(Path::new("/srv/x"), true, home);
        assert_eq!(a.exclude_from, vec![PathBuf::from("~/Progetti/appunti"), PathBuf::from("/srv/x")], "fuori dalla home resta assoluta");
        assert!(a.excludes_folder(Path::new("/Users/u/Progetti/appunti"), home));
        assert!(!a.excludes_folder(Path::new("/Users/u/Progetti/appunti/src"), home), "solo la cartella stessa");

        let mut b = with_exclusions("b", &["/Users/u/Progetti/appunti", "~/Progetti/appunti", "/abs/x"]);
        b.set_excluded(Path::new("/Users/u/Progetti/appunti"), false, home);
        assert_eq!(b.exclude_from, vec![PathBuf::from("/abs/x")]);
    }

    #[test]
    fn excluded_from_matches_the_folder_and_its_subfolders_expanding_the_tilde() {
        let home = Path::new("/Users/u");
        let a = with_exclusions("a", &["~/Progetti/Acme", "/abs/x"]);
        assert!(a.excluded_from(Path::new("/Users/u/Progetti/Acme"), home));
        assert!(a.excluded_from(Path::new("/Users/u/Progetti/Acme/appunti/src"), home));
        assert!(a.excluded_from(Path::new("/abs/x"), home));
        assert!(!a.excluded_from(Path::new("/Users/u/Progetti/AcmeX"), home), "prefisso di nome, non di cartella");
        assert!(!a.excluded_from(Path::new("/Users/u/Progetti"), home));
        assert!(!a.excluded_from(Path::new("/Users/u/Progetti/superzed"), home));
        assert!(!with_exclusions("b", &[]).excluded_from(Path::new("/Users/u/Progetti/Acme"), home));
    }

    #[test]
    fn reimport_keeps_the_exclusions_of_an_existing_account() {
        let old = vec![with_exclusions("a", &["~/Progetti/Acme"])];
        let imported = vec![
            Account { id: "a".into(), label: "nuovo".into(), email: Some("a@x".into()), config_dir: Some("/a2".into()), exclude_from: Vec::new() },
            with_exclusions("c", &[]),
        ];
        let (merged, added) = merge_import(&old, imported);
        assert_eq!(added, 1);
        assert_eq!(merged[0].label, "nuovo");
        assert_eq!(merged[0].exclude_from, vec![PathBuf::from("~/Progetti/Acme")]);
        assert!(merged[1].exclude_from.is_empty());
    }

    #[test]
    fn the_scheduler_key_round_trips_defaults_to_weekly_and_tolerates_unknown_values() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("accounts.json");
        write_registry(&reg, Strategy::Headroom, &[with_exclusions("a", &[])]).unwrap();
        let read = read_registry(&reg).unwrap().unwrap();
        assert_eq!((read.scheduler, read.accounts.len()), (Strategy::Headroom, 1));
        std::fs::write(&reg, r#"{"version":1,"accounts":[]}"#).unwrap();
        assert_eq!(read_registry(&reg).unwrap().unwrap().scheduler, Strategy::Weekly);
        std::fs::write(&reg, r#"{"version":1,"scheduler":"boh","accounts":[]}"#).unwrap();
        assert_eq!(read_registry(&reg).unwrap().unwrap().scheduler, Strategy::Weekly, "valore ignoto: default, non registro corrotto");
    }
}
