//! Chiusura di una chat con file non committati: quali file sono sporchi, il prompt che
//! chiede a claude di committarli e la macchina a stati che decide se chiudere.
use std::collections::BTreeSet;
use gpui::{App, Entity};
use project::Project;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use superzed_sessions::SessionState;

pub const COMMIT_PROMPT: &str = "Commit only the files you modified in this session. Do not stage or commit any other file. Use a concise commit message that follows this repository's conventions.";

/// Un file toccato dalla chat che il suo repository da' come cambiato (modificato,
/// aggiunto, cancellato, non tracciato o in conflitto).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncommittedFile {
    pub path: PathBuf,
    pub repo_root: PathBuf,
    /// Path relativo alla radice del repository, per tooltip e `git status`.
    pub display: String,
}

/// Indice della radice che contiene `path`; con repository annidati vince la piu' profonda.
pub fn repo_for(path: &Path, roots: &[Arc<Path>]) -> Option<usize> {
    roots
        .iter()
        .enumerate()
        .filter(|(_, root)| path.starts_with(root))
        .max_by_key(|(_, root)| root.components().count())
        .map(|(ix, _)| ix)
}

/// File toccati ancora sporchi secondo lo stato git in cache del progetto (lo aggiorna il
/// watcher di Zed, quindi segue anche i commit fatti a mano).
pub fn uncommitted_files(touched: &BTreeSet<PathBuf>, project: &Entity<Project>, cx: &App) -> Vec<UncommittedFile> {
    if touched.is_empty() {
        return Vec::new();
    }
    let repos: Vec<_> = project.read(cx).git_store().read(cx).repositories().values().cloned().collect();
    let roots: Vec<Arc<Path>> = repos.iter().map(|repo| repo.read(cx).work_directory_abs_path.clone()).collect();
    touched
        .iter()
        .filter_map(|path| {
            let ix = repo_for(path, &roots)?;
            let repo = repos.get(ix)?.read(cx);
            let root = roots.get(ix)?;
            let repo_path = repo.abs_path_to_repo_path(path)?;
            let entry = repo.status_for_path(&repo_path)?;
            entry.status.has_changes().then(|| UncommittedFile {
                path: path.clone(),
                repo_root: root.to_path_buf(),
                display: path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned(),
            })
        })
        .collect()
}

/// Radici dei repository git del progetto.
pub fn repository_roots(project: &Entity<Project>, cx: &App) -> Vec<Arc<Path>> {
    project
        .read(cx)
        .git_store()
        .read(cx)
        .repositories()
        .values()
        .map(|repo| repo.read(cx).work_directory_abs_path.clone())
        .collect()
}

/// File del controllo finale: quelli sporchi al click piu' quelli toccati da claude (anche
/// durante il turno del commit) dentro un repository del progetto, senza doppioni.
pub fn files_to_check(
    clicked: &[UncommittedFile],
    touched: &BTreeSet<PathBuf>,
    roots: &[Arc<Path>],
) -> Vec<UncommittedFile> {
    let mut files = clicked.to_vec();
    for path in touched {
        if files.iter().any(|file| &file.path == path) {
            continue;
        }
        let Some(root) = repo_for(path, roots).and_then(|ix| roots.get(ix)) else {
            continue;
        };
        files.push(UncommittedFile {
            path: path.clone(),
            repo_root: root.to_path_buf(),
            display: path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned(),
        });
    }
    files
}

pub fn git_status_args(repo_root: &Path, files: &[&UncommittedFile]) -> Vec<String> {
    let mut args = vec![
        "-C".to_string(),
        repo_root.to_string_lossy().into_owned(),
        "status".to_string(),
        "--porcelain".to_string(),
        "--untracked-files=all".to_string(),
        "--".to_string(),
    ];
    // `:(literal)`: nomi con `*`, `?` o `[` non vanno espansi come glob
    args.extend(files.iter().map(|file| format!(":(literal){}", file.display)));
    args
}

pub fn count_dirty(porcelain: &str) -> usize {
    porcelain.lines().filter(|line| !line.trim().is_empty()).count()
}

/// Una chat a cui e' stato chiesto di committare e poi chiudersi.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PendingCommit {
    /// claude ha cominciato il turno del commit (visto `Working` o `NeedsInput`).
    pub started: bool,
    /// Dopo lo `Stop`: il controllo git e' in corso.
    pub checking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Wait,
    Cancel,
}

impl PendingCommit {
    /// Stato della sessione a ogni cambiamento dello store.
    pub fn on_state(&mut self, state: Option<&SessionState>) -> Step {
        match state {
            None | Some(SessionState::Ended) => Step::Cancel,
            // una chat `Failed` resta tale finche' il prompt del commit non arriva: prima
            // dell'avvio e' lo stato del turno precedente, non un fallimento del commit
            Some(SessionState::Failed(_)) if self.started => Step::Cancel,
            Some(SessionState::Working | SessionState::NeedsInput(_)) => {
                self.started = true;
                Step::Wait
            }
            // Esc/ctrl-c non mandano hook: `Idle` dopo l'avvio e prima dello `Stop` e' un'
            // interruzione. Dopo lo `Stop` invece `Idle` e' normale (chat col focus).
            Some(SessionState::Idle) if self.started && !self.checking => Step::Cancel,
            Some(_) => Step::Wait,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Close,
    Keep,
}

/// `uncommitted`: file ancora sporchi secondo `git status`, `None` se il controllo e' fallito.
pub fn commit_close_decision(failed: bool, uncommitted: Option<usize>) -> Decision {
    if !failed && uncommitted == Some(0) { Decision::Close } else { Decision::Keep }
}

pub fn dirty_close_message(dirty: &[(String, usize)]) -> String {
    match dirty {
        [(title, 1)] => format!("“{title}” has 1 uncommitted file."),
        [(title, count)] => format!("“{title}” has {count} uncommitted files."),
        _ => format!("{} chats have uncommitted files.", dirty.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(paths: &[&str]) -> Vec<Arc<Path>> {
        paths.iter().map(|p| Arc::from(Path::new(p))).collect()
    }

    #[test]
    fn repo_for_prefers_the_deepest_root() {
        let roots = roots(&["/p", "/p/vendor/lib", "/other"]);
        assert_eq!(repo_for(Path::new("/p/src/a.rs"), &roots), Some(0));
        assert_eq!(repo_for(Path::new("/p/vendor/lib/x.rs"), &roots), Some(1));
        assert_eq!(repo_for(Path::new("/elsewhere/a.rs"), &roots), None);
        assert_eq!(repo_for(Path::new("/pp/a.rs"), &roots), None, "prefisso di componenti, non di stringa");
    }

    #[test]
    fn git_status_args_and_dirty_count() {
        let file = |rel: &str| UncommittedFile {
            path: Path::new("/p").join(rel),
            repo_root: PathBuf::from("/p"),
            display: rel.to_string(),
        };
        let (a, b) = (file("a.rs"), file("src/b.rs"));
        assert_eq!(
            git_status_args(Path::new("/p"), &[&a, &b]),
            vec!["-C", "/p", "status", "--porcelain", "--untracked-files=all", "--", ":(literal)a.rs", ":(literal)src/b.rs"]
        );
        assert_eq!(count_dirty(""), 0);
        assert_eq!(count_dirty("\n"), 0);
        assert_eq!(count_dirty(" M a.rs\n?? src/b.rs\n"), 2);
    }

    #[test]
    fn files_to_check_adds_touched_files_inside_a_repo_once() {
        let roots = roots(&["/p", "/p/vendor/lib"]);
        let clicked = vec![UncommittedFile {
            path: PathBuf::from("/p/a.rs"),
            repo_root: PathBuf::from("/p"),
            display: "a.rs".into(),
        }];
        let touched: BTreeSet<PathBuf> =
            ["/p/a.rs", "/p/src/b.rs", "/p/vendor/lib/c.rs", "/elsewhere/d.rs"].iter().map(PathBuf::from).collect();
        let files = files_to_check(&clicked, &touched, &roots);
        let summary: Vec<(&str, &str)> = files
            .iter()
            .map(|file| (file.repo_root.to_str().unwrap_or_default(), file.display.as_str()))
            .collect();
        assert_eq!(summary, vec![("/p", "a.rs"), ("/p", "src/b.rs"), ("/p/vendor/lib", "c.rs")]);
    }

    #[test]
    fn pending_commit_waits_for_the_turn_and_cancels_on_interrupt_or_exit() {
        let mut pending = PendingCommit::default();
        assert_eq!(pending.on_state(Some(&SessionState::Done)), Step::Wait, "prompt non ancora arrivato");
        assert_eq!(pending.on_state(Some(&SessionState::Idle)), Step::Wait, "idem, chat col focus");
        assert_eq!(pending.on_state(Some(&SessionState::Working)), Step::Wait);
        assert!(pending.started);
        assert_eq!(
            pending.on_state(Some(&SessionState::NeedsInput(superzed_sessions::InputKind::Permission))),
            Step::Wait,
            "permesso per git commit: si aspetta"
        );
        assert_eq!(pending.on_state(Some(&SessionState::Idle)), Step::Cancel, "Esc dopo l'avvio");
        let mut pending = PendingCommit::default();
        assert_eq!(pending.on_state(Some(&SessionState::Ended)), Step::Cancel);
        assert_eq!(pending.on_state(None), Step::Cancel, "sessione sparita");
        let mut pending = PendingCommit::default();
        assert_eq!(pending.on_state(Some(&SessionState::Failed("x".into()))), Step::Wait, "turno precedente fallito");
        let mut pending = PendingCommit { started: true, checking: false };
        assert_eq!(pending.on_state(Some(&SessionState::Failed("x".into()))), Step::Cancel);
    }

    #[test]
    fn checking_ignores_idle() {
        let mut pending = PendingCommit { started: true, checking: true };
        assert_eq!(pending.on_state(Some(&SessionState::Idle)), Step::Wait, "Stop con il focus mentre git controlla");
        assert_eq!(pending.on_state(Some(&SessionState::Ended)), Step::Cancel);
    }

    #[test]
    fn decision() {
        assert_eq!(commit_close_decision(false, Some(0)), Decision::Close);
        assert_eq!(commit_close_decision(false, Some(2)), Decision::Keep);
        assert_eq!(commit_close_decision(false, None), Decision::Keep, "git fallito: non si chiude");
        assert_eq!(commit_close_decision(true, Some(0)), Decision::Keep);
    }

    #[test]
    fn dirty_close_message_names_one_chat_or_counts_many() {
        assert_eq!(dirty_close_message(&[("Atlas".into(), 1)]), "“Atlas” has 1 uncommitted file.");
        assert_eq!(dirty_close_message(&[("Atlas".into(), 3)]), "“Atlas” has 3 uncommitted files.");
        assert_eq!(dirty_close_message(&[("a".into(), 1), ("b".into(), 2)]), "2 chats have uncommitted files.");
    }
}
