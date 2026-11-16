//! Handoff di una chat su un altro account (`cmd-shift-h`): picker degli account, poi
//! claude scrive il documento di handoff e allo `Stop` la chat si chiude e al suo posto ne
//! nasce una sull'account scelto che lo legge. Con l'account della chat al limite claude
//! non puo' scrivere nulla: si copia il transcript nell'account scelto e la stessa
//! conversazione riparte da li' (`--resume`).
use super::*;
use crate::automation::Readiness;
use crate::commit_close::{PendingCommit, Step};
use gpui::AnyWindowHandle;
use std::path::Path;

pub(crate) struct Handoff {
    pending: PendingCommit,
    account_id: String,
    /// Documento che claude deve scrivere (path assoluto).
    path: PathBuf,
    window: AnyWindowHandle,
    _enter: Option<Task<()>>,
    _check: Option<Task<()>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    Kanban,
    At(Point<f32>),
}

/// Cartella dei documenti di handoff: quella di superpowers se il progetto ce l'ha,
/// altrimenti `.superzed/handoffs`.
fn handoff_dir(root: &Path, is_dir: impl Fn(&Path) -> bool) -> PathBuf {
    let superpowers = root.join("docs").join("superpowers");
    if is_dir(&superpowers) {
        superpowers
    } else {
        root.join(".superzed").join("handoffs")
    }
}

fn title_slug(title: &str) -> String {
    let mut slug = String::new();
    for word in title.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        if slug.len() + word.len() > 40 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_lowercase());
    }
    if slug.is_empty() { "chat".to_string() } else { slug }
}

/// `HANDOFF-<data>-<titolo>.md` nella cartella, con un numero se il nome e' gia' preso.
pub(crate) fn handoff_path(dir: &Path, date: &str, title: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let base = format!("HANDOFF-{date}-{}", title_slug(title));
    let mut path = dir.join(format!("{base}.md"));
    let mut n = 2;
    while exists(&path) {
        path = dir.join(format!("{base}-{n}.md"));
        n += 1;
    }
    path
}

pub(crate) fn handoff_prompt(path: &Path) -> String {
    format!(
        "This chat is being handed off to a new session that starts from scratch. Write a handoff document at `{}` \
         so that it can continue the work without this conversation: the goal, the current state, the decisions taken \
         and why, the files involved, what is left to do as concrete next steps, and anything that would be easy to \
         get wrong. Write only that file: do not commit it and do not change anything else.",
        path.display()
    )
}

pub(crate) fn resume_prompt(path: &Path) -> String {
    format!("Read the handoff at `{}` and continue the work from there.", path.display())
}

/// Dove va copiato il transcript nella config dir di un altro account: stessa cartella di
/// progetto sotto `projects/`, cosi' `--resume` lo trova.
pub(crate) fn transcript_target(transcript: &Path, config_dir: &Path) -> Option<PathBuf> {
    let file = transcript.file_name()?;
    let project = transcript.parent()?.file_name()?;
    Some(config_dir.join("projects").join(project).join(file))
}

impl CanvasView {
    pub fn is_handing_off(&self, node: NodeId) -> bool {
        self.handoffs.contains_key(&node)
    }

    /// `cmd-shift-h`: picker degli account per la chat con il focus.
    pub(super) fn handoff_focused_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.target_nodes(window, cx).first().copied() else {
            return;
        };
        self.pick_handoff_account(node, window, cx);
    }

    pub(super) fn pick_handoff_account(&mut self, node: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.frame(node).is_none_or(|f| f.kind != NodeKind::Claude) || self.handoffs.contains_key(&node) {
            return;
        }
        if self.automations.contains_key(&node) {
            self.warn("Automation chats can't be handed off".to_string(), cx);
            return;
        }
        let Some(current) = self.chat_session(node, cx).map(|s| s.account_id) else {
            self.warn("The chat hasn't started yet: nothing to hand off".to_string(), cx);
            return;
        };
        let Some(store) = superzed_accounts::AccountStore::global(cx) else {
            self.start_handoff(node, superzed_accounts::SYSTEM_ID.to_string(), window, cx);
            return;
        };
        let loads = superzed_accounts::current_loads(cx);
        let project = self.project_cwd(cx);
        let (rows, selected) = superzed_accounts::picker::picker_rows(store.read(cx), &loads, project.as_deref());
        let selected = superzed_accounts::picker::prefer_other(&rows, selected, &current);
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let canvas = cx.weak_entity();
        workspace.update(cx, |ws, cx| {
            superzed_accounts::picker::open_account_picker(
                ws,
                rows,
                selected,
                move |id, window, cx| {
                    window.defer(cx, move |window, cx| {
                        canvas.update(cx, |c, cx| c.start_handoff(node, id, window, cx)).ok();
                    });
                },
                window,
                cx,
            )
        });
    }

    fn chat_session(&self, node: NodeId, cx: &App) -> Option<superzed_sessions::Session> {
        let key = self.session_key(node)?;
        let store = superzed_sessions::SessionStore::global(cx)?;
        store.read(cx).get(key).cloned()
    }

    fn account_by_id(&self, id: &str, cx: &App) -> Option<superzed_accounts::Account> {
        if id == superzed_accounts::SYSTEM_ID {
            return Some(self.system_account(cx));
        }
        superzed_accounts::AccountStore::global(cx).and_then(|s| s.read(cx).account(id).cloned())
    }

    fn account_at_limit(&self, id: &str, cx: &App) -> bool {
        superzed_accounts::AccountStore::global(cx).is_some_and(|s| s.read(cx).at_limit(id))
    }

    fn account_config_dir(&self, account: &superzed_accounts::Account, cx: &App) -> PathBuf {
        account.config_dir.clone().unwrap_or_else(|| {
            superzed_sessions::SessionStore::global(cx)
                .map(|s| s.read(cx).config_dir().to_path_buf())
                .unwrap_or_else(|| superzed_sessions::launch::system_config_dir(util::paths::home_dir()))
        })
    }

    fn start_handoff(&mut self, node: NodeId, account_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.chat_session(node, cx) else {
            return;
        };
        if self.account_at_limit(&session.account_id, cx) {
            if account_id == session.account_id {
                self.warn("This account is at its limit: choose another one for the handoff".to_string(), cx);
            } else {
                self.transfer_chat(node, &account_id, session, window, cx);
            }
            return;
        }
        if self.chat_readiness(node, cx) != Readiness::Ready {
            self.warn("The chat is busy: wait for it to finish before the handoff".to_string(), cx);
            return;
        }
        let Some(root) = self.project_cwd(cx) else {
            return;
        };
        let title = self.model.frame(node).map(|f| self.node_title(f, cx)).unwrap_or_default();
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        let dir = handoff_dir(&root, |p| p.is_dir());
        let path = handoff_path(&dir, &date, &title, |p| p.exists());
        // ctrl-u: il prompt non va incollato in coda a una bozza non inviata
        if let Some((_, terminal)) = self.terminals.get(&node) {
            terminal.update(cx, |t, _| t.input("\x15".as_bytes()));
        }
        let enter = self.type_prompt(node, &handoff_prompt(&path), cx);
        self.handoffs.insert(
            node,
            Handoff {
                pending: PendingCommit::default(),
                account_id,
                path,
                window: window.window_handle(),
                _enter: enter,
                _check: None,
            },
        );
        cx.notify();
    }

    fn placement(&self, node: NodeId) -> Option<Placement> {
        let frame = self.model.frame(node)?;
        Some(if frame.in_kanban() { Placement::Kanban } else { Placement::At(frame.origin) })
    }

    /// Chiude la chat e ne apre una sull'account al suo posto (stessa colonna del kanban o
    /// stesso punto del canvas).
    fn replace_chat(
        &mut self,
        node: NodeId,
        account: superzed_accounts::Account,
        saved: superzed_sessions::SavedSession,
        prompt: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(placement) = self.placement(node) else {
            return;
        };
        self.remove_nodes(&[node], cx);
        match placement {
            Placement::Kanban => {
                if self.kanban_node().is_none() {
                    self.open_kanban(window, cx);
                }
                self.dock_next_claude = Some(DockKind::Kanban);
            }
            Placement::At(origin) => self.next_claude_origin = Some(origin),
        }
        let key = uuid::Uuid::new_v4().to_string();
        let (spec, init) = self.claude_spec_for(&account, &saved, &key, prompt, cx);
        let spec = self.claude_pty(spec, cx);
        self.open_claude_terminal(None, saved.session_id.as_deref(), spec, init, window, cx);
        // se `open_terminal_with_spec` e' uscito prima di leggerli, non restano accesi
        self.dock_next_claude = None;
        self.next_claude_origin = None;
    }

    /// Account al limite: la conversazione passa all'altro account copiandone il transcript.
    fn transfer_chat(
        &mut self,
        node: NodeId,
        account_id: &str,
        session: superzed_sessions::Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(account) = self.account_by_id(account_id, cx) else {
            self.warn(format!("The account “{account_id}” is no longer registered"), cx);
            return;
        };
        let (Some(session_id), Some(transcript)) = (session.session_id.clone(), session.transcript.clone()) else {
            self.warn("The chat has no conversation to move yet".to_string(), cx);
            return;
        };
        let Some(target) = transcript_target(&transcript, &self.account_config_dir(&account, cx)) else {
            self.warn(format!("Unexpected transcript path: {}", transcript.display()), cx);
            return;
        };
        let saved = superzed_sessions::SavedSession {
            session_id: Some(session_id),
            phase: session.phase,
            account: None,
            times: session.times,
        };
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let copied = cx
                .background_spawn(async move {
                    if let Some(dir) = target.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::copy(&transcript, &target)?;
                    anyhow::Ok(())
                })
                .await;
            cx.update_window(window_handle, |_, window, cx| {
                this.update(cx, |c, cx| match copied {
                    Ok(()) => c.replace_chat(node, account, saved, None, window, cx),
                    Err(error) => c.warn(format!("Couldn't move the chat to the other account: {error}"), cx),
                })
            })
            .and_then(|result| result)
            .log_err();
        })
        .detach();
    }

    pub(super) fn handoff_sessions_changed(&mut self, cx: &mut Context<Self>) {
        if self.handoffs.is_empty() {
            return;
        }
        let nodes: Vec<NodeId> = self.handoffs.keys().copied().collect();
        let mut changed = false;
        let mut idle = Vec::new();
        for node in nodes {
            let state = self.chat_session_state(node, cx);
            let Some(entry) = self.handoffs.get_mut(&node) else {
                continue;
            };
            match entry.pending.on_state(state.as_ref()) {
                Step::Wait => {}
                Step::Cancel if state == Some(superzed_sessions::SessionState::Idle) => idle.push(node),
                Step::Cancel => {
                    self.handoffs.remove(&node);
                    changed = true;
                }
            }
        }
        // come in `commit_close_sessions_changed`: `Idle` puo' precedere lo `Stopped` dello
        // stesso turno, quindi l'interruzione si decide dopo l'emit
        if !idle.is_empty() {
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                this.update(cx, |c, cx| c.handoff_interrupted(&idle, cx)).ok();
            });
        }
        if changed {
            cx.notify();
        }
    }

    fn handoff_interrupted(&mut self, nodes: &[NodeId], cx: &mut Context<Self>) {
        let mut changed = false;
        for node in nodes {
            let checking = self.handoffs.get(node).is_some_and(|entry| entry.pending.checking);
            if !checking
                && self.chat_session_state(*node, cx) == Some(superzed_sessions::SessionState::Idle)
                && self.handoffs.remove(node).is_some()
            {
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    pub(super) fn handoff_stopped(&mut self, node: NodeId, failed: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.handoffs.get_mut(&node) else {
            return;
        };
        // lo `Stop` di un turno precedente al prompt di handoff
        if !entry.pending.started {
            return;
        }
        if failed {
            self.handoffs.remove(&node);
            self.warn("The chat failed while writing the handoff: nothing changed".to_string(), cx);
            cx.notify();
            return;
        }
        entry.pending.checking = true;
        let path = entry.path.clone();
        let window = entry.window;
        let check = cx.spawn(async move |this, cx| {
            let written = cx.background_spawn(async move { path.is_file() }).await;
            cx.update_window(window, |_, window, cx| {
                this.update(cx, |c, cx| c.finish_handoff(node, written, window, cx))
            })
            .and_then(|result| result)
            .log_err();
        });
        entry._check = Some(check);
    }

    fn finish_handoff(&mut self, node: NodeId, written: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.handoffs.remove(&node) else {
            return;
        };
        cx.notify();
        if !written {
            self.warn(format!("The handoff wasn't written to {}: the chat stays open", entry.path.display()), cx);
            return;
        }
        let Some(account) = self.account_by_id(&entry.account_id, cx) else {
            self.warn(
                format!("The account “{}” is no longer registered: the chat stays open", entry.account_id),
                cx,
            );
            return;
        };
        let phase = self.chat_session(node, cx).map(|s| s.phase).unwrap_or_default();
        let saved = superzed_sessions::SavedSession { phase, ..Default::default() };
        self.replace_chat(node, account, saved, Some(&resume_prompt(&entry.path)), window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoffs_go_to_superpowers_when_the_project_has_it() {
        let root = Path::new("/repo");
        assert_eq!(handoff_dir(root, |_| true), Path::new("/repo/docs/superpowers"));
        assert_eq!(handoff_dir(root, |_| false), Path::new("/repo/.superzed/handoffs"));
    }

    #[test]
    fn handoff_path_uses_date_and_title_and_skips_taken_names() {
        let dir = Path::new("/repo/docs");
        assert_eq!(
            handoff_path(dir, "2026-11-16", "Giacenze: spec & piano!", |_| false),
            Path::new("/repo/docs/HANDOFF-2026-11-16-giacenze-spec-piano.md")
        );
        let taken = Path::new("/repo/docs/HANDOFF-2026-11-16-chat.md");
        assert_eq!(
            handoff_path(dir, "2026-11-16", "  ", |p| p == taken),
            Path::new("/repo/docs/HANDOFF-2026-11-16-chat-2.md")
        );
    }

    #[test]
    fn title_slug_stays_short() {
        let slug = title_slug("una chat dal titolo davvero molto lungo che non finisce mai piu");
        assert!(slug.len() <= 40, "{slug}");
        assert!(slug.starts_with("una-chat-dal-titolo"));
    }

    #[test]
    fn transcript_goes_under_the_same_project_folder() {
        assert_eq!(
            transcript_target(Path::new("/home/.claude/projects/-repo/abc.jsonl"), Path::new("/accounts/b")),
            Some(PathBuf::from("/accounts/b/projects/-repo/abc.jsonl"))
        );
    }
}
