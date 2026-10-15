//! Esito della scrittura per voce (`<sessionId>.result.json` accanto al report): cosa e'
//! gia' scritto non si riscrive, cosa e' fallito si riprova.
use serde::{Deserialize, Serialize};
use serde_json::json;
use crate::report::{Report, parse_report, read_report, report_json, result_path};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundIssue {
    pub number: u64,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemOutcome {
    pub ok: bool,
    /// Link o descrizione (riuscita) o messaggio di `gh` (errore).
    pub message: String,
    /// Issue creata per una voce nuova, anche se un passo successivo e' fallito.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<FoundIssue>,
}

/// Esito di una voce appena eseguita (`gh::execute`).
#[derive(Clone, Debug, PartialEq)]
pub struct ItemReport {
    pub key: String,
    pub result: Result<String, String>,
    pub issue: Option<FoundIssue>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunResult {
    pub items: BTreeMap<String, ItemOutcome>,
    /// Copia del report salvata alla prima scrittura: da li' in poi l'anteprima (e gli
    /// indici delle voci) vengono da questa, non dal file del report.
    pub report: Option<serde_json::Value>,
}

/// Avviso nell'anteprima quando il file del report non e' piu' quello della prima scrittura.
pub const REPORT_CHANGED: &str =
    "The report file changed after the first write to GitHub: the preview uses the copy saved then";

/// Da dove viene l'anteprima di una sessione: report (copia salvata o file), esito, avviso.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewSource {
    pub report: Report,
    pub previous: RunResult,
    pub warning: Option<String>,
}

/// Report ed esito della sessione: dopo la prima scrittura vince la copia nell'esito.
pub fn review_source(root: &Path, session_id: &str) -> Result<ReviewSource, String> {
    let previous = result_path(root, session_id).map(|p| RunResult::read(&p)).unwrap_or_default();
    let file = read_report(root, session_id);
    match previous.snapshot(session_id) {
        Some(report) => {
            let warning = (file.as_ref() != Ok(&report)).then(|| REPORT_CHANGED.to_string());
            Ok(ReviewSource { report, previous, warning })
        }
        None => Ok(ReviewSource { report: file?, previous, warning: None }),
    }
}

impl RunResult {
    /// La copia del report (solo la prima: le scritture dopo non la cambiano).
    pub fn set_snapshot(&mut self, report: &Report) {
        if self.report.is_none() {
            self.report = serde_json::from_str(&report_json(report)).ok();
        }
    }

    /// La copia salvata, rivalidata come un report (`None` se assente o rovinata).
    pub fn snapshot(&self, session_id: &str) -> Option<Report> {
        parse_report(&self.report.as_ref()?.to_string(), session_id).ok()
    }

    pub fn is_written(&self, key: &str) -> bool {
        self.items.get(key).is_some_and(|o| o.ok)
    }

    /// Unisce gli esiti nuovi; l'issue gia' nota di una voce resta se il nuovo esito non ne porta.
    pub fn record(&mut self, reports: Vec<ItemReport>) {
        for r in reports {
            let issue = r.issue.or_else(|| self.items.get(&r.key).and_then(|o| o.issue.clone()));
            let (ok, message) = match r.result {
                Ok(m) => (true, m),
                Err(e) => (false, e),
            };
            self.items.insert(r.key, ItemOutcome { ok, message, issue });
        }
    }

    /// Voci fallite: (chiave, messaggio).
    pub fn failed(&self) -> Vec<(String, String)> {
        self.items.iter().filter(|(_, o)| !o.ok).map(|(k, o)| (k.clone(), o.message.clone())).collect()
    }

    /// Issue gia' create, per indice della voce nuova (`new:<n>` → issue).
    pub fn known_issues(&self) -> BTreeMap<usize, FoundIssue> {
        self.items
            .iter()
            .filter_map(|(k, o)| Some((k.strip_prefix("new:")?.parse().ok()?, o.issue.clone()?)))
            .collect()
    }

    /// File assente o illeggibile → vuoto (le issue gia' create le ritrova la ricerca dei marker).
    pub fn read(path: &Path) -> RunResult {
        let Ok(text) = std::fs::read_to_string(path) else {
            return RunResult::default();
        };
        let value = serde_json::from_str::<serde_json::Value>(&text).ok();
        let items = value.as_ref().and_then(|v| serde_json::from_value(v.get("items")?.clone()).ok());
        match items {
            Some(items) => RunResult { items, report: value.and_then(|v| v.get("report").filter(|r| r.is_object()).cloned()) },
            None => {
                log::warn!("superzed done: {} unreadable, ignored", path.display());
                RunResult::default()
            }
        }
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let mut v = json!({"version": 1, "items": &self.items});
        if let Some(report) = &self.report {
            v["report"] = report.clone();
        }
        let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        crate::report::write_atomic(path, &text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(n: u64) -> FoundIssue {
        FoundIssue { number: n, url: format!("https://github.com/acme/acme_app/issues/{n}") }
    }

    #[test]
    fn results_merge_keep_issues_and_roundtrip() {
        let mut r = RunResult::default();
        r.record(vec![
            ItemReport { key: "new:0".into(), result: Err("gh: HTTP 502".into()), issue: Some(found(42)) },
            ItemReport { key: "touched:12".into(), result: Ok("#12 closed".into()), issue: None },
        ]);
        assert!(!r.is_written("new:0") && r.is_written("touched:12") && !r.is_written("summary"));
        assert_eq!(r.known_issues(), BTreeMap::from([(0, found(42))]), "creata anche se un passo dopo e' fallito");
        assert_eq!(r.failed(), [("new:0".to_string(), "gh: HTTP 502".to_string())]);
        r.record(vec![ItemReport { key: "new:0".into(), result: Ok(found(42).url), issue: None }]);
        assert!(r.is_written("new:0"));
        assert_eq!(r.items["new:0"].issue, Some(found(42)), "l'issue nota resta");
        assert!(r.failed().is_empty());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.result.json");
        assert_eq!(RunResult::read(&path), RunResult::default(), "assente");
        r.write(&path).unwrap();
        assert_eq!(RunResult::read(&path), r);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"version\": 1"), "{text}");
        std::fs::write(&path, "rotto").unwrap();
        assert_eq!(RunResult::read(&path), RunResult::default(), "illeggibile: si riparte");
    }

    // Revisione finale 5: dalla prima scrittura l'anteprima viene dalla copia del report
    // salvata nell'esito; un report riscritto dopo non rimappa le voci.
    #[test]
    fn the_report_snapshot_wins_over_a_rewritten_file() {
        use crate::report::{parse_report, report_path, result_path, write_report};
        use crate::test_support::{REPORT_JSON, SID};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let report = parse_report(REPORT_JSON, SID).unwrap();
        write_report(root, &report).unwrap();
        let src = review_source(root, SID).unwrap();
        assert_eq!((&src.report, src.warning.as_deref(), src.previous.snapshot(SID)), (&report, None, None), "nessuna scrittura: il file");

        let mut r = RunResult::default();
        r.set_snapshot(&report);
        let mut other = report.clone();
        other.missing.clear();
        r.set_snapshot(&other);
        assert_eq!(r.snapshot(SID), Some(report.clone()), "la prima copia resta");
        r.record(vec![ItemReport { key: "new:0".into(), result: Ok("x".into()), issue: Some(found(42)) }]);
        r.write(&result_path(root, SID).unwrap()).unwrap();
        assert_eq!(RunResult::read(&result_path(root, SID).unwrap()), r, "copia e voci sopravvivono al file");

        let src = review_source(root, SID).unwrap();
        assert_eq!((&src.report, src.warning.as_deref()), (&report, None), "file uguale alla copia: nessun avviso");
        write_report(root, &other).unwrap();
        let src = review_source(root, SID).unwrap();
        assert_eq!(src.report, report, "indici della copia, non del file nuovo");
        assert_eq!(src.warning.as_deref(), Some(REPORT_CHANGED));
        std::fs::remove_file(report_path(root, SID).unwrap()).unwrap();
        assert_eq!(review_source(root, SID).unwrap().report, report, "anche senza file");
        std::fs::remove_file(result_path(root, SID).unwrap()).unwrap();
        assert!(review_source(root, SID).is_err(), "niente report, niente copia");
    }
}
