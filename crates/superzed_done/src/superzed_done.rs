//! Pipeline Fatto → GitHub di superzed: report di una chat arrivata in Fatto, anteprima
//! delle modifiche alla board claude-pm e scrittura con `gh` dopo la conferma. Niente UI.
pub mod gh;
pub mod headless;
pub mod pm;
pub mod preview;
pub mod report;
pub mod result;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use gh::{execute, marker, marker_query};
pub use pm::{load_pm, parse_areas};
pub use preview::{
    Action, BODY_MAX_CHARS, InvalidField, IssueDraft, Kind, NewIssue, Preview, SUMMARY_KEY, Status, SummaryEntry,
    TITLE_MAX_CHARS, TouchedEntry, action_label, actions, closes, fully_written, item_label, new_key, ordinal, title_of,
    touch_label, touched_key, validate_fields, validate_preview,
};
pub use report::{
    Blocker, MAX_ENTRIES, MAX_ENTRY_CHARS, REPORTS_DIR, Report, TRANSCRIPT_SEPARATOR, TouchAction, TouchedIssue,
    chat_prompt, ensure_reports_dir, headless_prompt, parse_report, read_report, report_json,
    report_markdown, report_path, report_summary, result_path, safe_session_id, write_atomic, write_report,
};
pub use result::{FoundIssue, ItemOutcome, ItemReport, REPORT_CHANGED, ReviewSource, RunResult, review_source};

pub use headless::{
    CLAUDE_TIMEOUT, ClaudeProcessRunner, SUPERZED_HOOK_VARS, TRANSCRIPT_TAIL, claude_argv, headless_report, headless_stdin, parse_claude_output,
    transcript_tail,
};

use gpui::{App, Global};
use std::{path::PathBuf, sync::Arc};
use superzed_sources::github::PmConfig;
use superzed_sources::{ProcessRunner, Runner};

/// Esito di una scrittura: l'esito completo (anche delle scritture di prima), le voci di
/// questa e l'eventuale errore di salvataggio.
#[derive(Clone, Debug, PartialEq)]
pub struct WriteRun {
    pub result: RunResult,
    pub reports: Vec<ItemReport>,
    pub save_error: Option<String>,
}

/// Scrive su GitHub (`execute`) salvando l'esito in `path` prima della prima voce (con la
/// copia del report) e dopo ognuna: una scrittura interrotta non rifa' le voci gia' fatte.
pub async fn write_and_save(
    runner: Arc<dyn Runner>,
    cfg: PmConfig,
    report: &Report,
    actions: Vec<Action>,
    mut result: RunResult,
    path: PathBuf,
) -> WriteRun {
    result.set_snapshot(report);
    let mut save_error = result.write(&path).err();
    let known = result.known_issues();
    let reports = execute(runner, cfg, report.session_id.clone(), actions, known, |item| {
        result.record(vec![item.clone()]);
        if let Err(e) = result.write(&path) {
            log::error!("superzed done: outcome not saved: {e}");
            save_error.get_or_insert(e);
        }
    })
    .await;
    WriteRun { result, reports, save_error }
}

/// Tipo dell'azione «Rivedi» sulle notifiche della pipeline (argomento: id di sessione).
pub const REVIEW_ACTION: &str = "done_review";
/// Sorgente delle notifiche della pipeline.
pub const NOTIFICATION_SOURCE: &str = "Done → GitHub";

/// Chi esegue `gh` e `claude` (finti nei test) e, solo nei test, la cartella usata al
/// posto della root del progetto (il progetto di test e' su un FakeFs, il report su disco).
#[derive(Clone)]
pub struct DoneEnv {
    pub gh: Arc<dyn Runner>,
    pub claude: Arc<dyn Runner>,
    pub root_override: Option<PathBuf>,
}

impl Global for DoneEnv {}

impl DoneEnv {
    /// `None` senza `init`: la pipeline non parte.
    pub fn get(cx: &App) -> Option<DoneEnv> {
        cx.try_global::<DoneEnv>().cloned()
    }

    pub fn root(&self, project_root: Option<PathBuf>) -> Option<PathBuf> {
        self.root_override.clone().or(project_root)
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(DoneEnv { gh: Arc::new(ProcessRunner), claude: Arc::new(ClaudeProcessRunner), root_override: None });
}

pub fn init_test(cx: &mut App, gh: Arc<dyn Runner>, claude: Arc<dyn Runner>, root_override: Option<PathBuf>) {
    cx.set_global(DoneEnv { gh, claude, root_override });
}

#[cfg(test)]
mod tests {
    use super::*;
    use superzed_sources::test_support::FakeRunner;

    #[gpui::test]
    fn init_test_installs_the_fakes_and_the_root(cx: &mut gpui::TestAppContext) {
        assert!(cx.update(|cx| DoneEnv::get(cx)).is_none(), "senza init niente pipeline");
        cx.update(|cx| init_test(cx, FakeRunner::new(), FakeRunner::new(), Some("/tmp/finto".into())));
        let env = cx.update(|cx| DoneEnv::get(cx)).unwrap();
        assert_eq!(env.root(Some("/repo".into())), Some("/tmp/finto".into()));
        cx.update(|cx| init(cx));
        let env = cx.update(|cx| DoneEnv::get(cx)).unwrap();
        assert_eq!(env.root(Some("/repo".into())), Some("/repo".into()));
        assert_eq!(env.root(None), None);
    }

    /// Runner che, quando parte la chiusura, fotografa l'esito salvato su disco.
    struct Spy {
        inner: Arc<FakeRunner>,
        path: PathBuf,
        seen: std::sync::Mutex<Vec<RunResult>>,
    }

    impl Runner for Spy {
        fn run(
            &self,
            program: &str,
            args: &[String],
            stdin: Option<String>,
        ) -> futures::future::BoxFuture<'static, Result<superzed_sources::RunOutput, superzed_sources::RunError>> {
            if args.first().map(String::as_str) == Some("issue") && args.get(1).map(String::as_str) == Some("close") {
                self.seen.lock().unwrap().push(RunResult::read(&self.path));
            }
            self.inner.run(program, args, stdin)
        }
    }

    // Revisione finale 1 e 5: esito salvato dopo ogni voce, con la copia del report dalla
    // prima scrittura; una scrittura interrotta non rifa' le voci gia' fatte.
    #[test]
    fn the_result_is_saved_after_every_item() {
        use crate::test_support::{REPORT_JSON, SID, write_runner};
        let dir = tempfile::tempdir().unwrap();
        let report = parse_report(REPORT_JSON, SID).unwrap();
        let path = result_path(dir.path(), SID).unwrap();
        ensure_reports_dir(dir.path()).unwrap();
        let preview = Preview::from_report(&report, &["frontend".into()], &RunResult::default());
        let spy = Arc::new(Spy { inner: write_runner(), path: path.clone(), seen: Default::default() });
        let cfg = superzed_sources::github::PmConfig { owner: "acme".into(), project_number: 3, repo: "acme/acme_app".into(), stale_days: 14 };
        let run = futures::executor::block_on(write_and_save(spy.clone(), cfg, &report, actions(&preview), RunResult::default(), path.clone()));
        let at_close = spy.seen.lock().unwrap()[0].clone();
        assert!(["new:0", "new:1", "new:2"].iter().all(|k| at_close.is_written(k)), "{at_close:?}");
        assert!(!at_close.is_written("touched:12"));
        assert_eq!(at_close.snapshot(SID), Some(report.clone()), "copia del report salvata prima della prima voce");
        // interrotta qui: la riprova rifa' solo il resto
        let retry = Preview::from_report(&report, &["frontend".into()], &at_close);
        assert_eq!(actions(&retry).iter().map(Action::key).collect::<Vec<_>>(), ["touched:12", "touched:8", "summary"]);
        assert_eq!((run.reports.len(), run.save_error.clone()), (6, None));
        assert_eq!(RunResult::read(&path), run.result);
        assert!(run.result.failed().is_empty() && run.result.items.len() == 6);
    }
}
