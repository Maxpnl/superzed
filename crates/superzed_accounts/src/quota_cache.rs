//! Ultime quote note per account, su disco: un account inattivo (token scaduto, niente
//! fetch) mostra ancora i suoi valori dopo un riavvio e lo scheduler non lo tratta come
//! sconosciuto. Solo percentuali e reset, mai token.
use crate::quota::Quota;
use collections::HashMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "accounts-quota-cache.json";

/// `<data_dir>/accounts-quota-cache.json`.
pub fn cache_path() -> PathBuf {
    paths::data_dir().join(FILE_NAME)
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, Quota>,
}

/// Tollerante: file assente, illeggibile o di un'altra versione → nessuna quota.
pub fn read(path: &Path) -> HashMap<String, Quota> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::default();
    };
    match serde_json::from_str::<CacheFile>(&text) {
        Ok(file) if file.version == 1 => file.accounts,
        Ok(_) => HashMap::default(),
        Err(e) => {
            log::warn!("superzed: quota cache ignored ({}): {e}", path.display());
            HashMap::default()
        }
    }
}

/// Scrittura atomica (file temporaneo e rename).
pub fn write(path: &Path, quotas: &HashMap<String, Quota>) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = CacheFile { version: 1, accounts: quotas.clone() };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&file)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::Limit;
    use chrono::{TimeZone as _, Utc};

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join(FILE_NAME);
        assert!(read(&path).is_empty(), "assente");
        let t = Utc.with_ymd_and_hms(2026, 9, 27, 9, 0, 0).unwrap();
        let limit = Limit { used_percent: 42., resets_at: Some(t), seen_at: t };
        let quota = Quota {
            session: Some(limit.clone()),
            weekly: None,
            scoped: vec![("Opus".into(), limit)],
            fetched_at: Some(t),
        };
        let mut map = HashMap::default();
        map.insert("a1".to_string(), quota);
        write(&path, &map).unwrap();
        assert_eq!(read(&path), map);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "[1,2").unwrap();
        assert!(read(&path).is_empty(), "rotto");
        std::fs::write(&path, r#"{"version": 9, "accounts": {}}"#).unwrap();
        assert!(read(&path).is_empty(), "versione sconosciuta");
    }
}
