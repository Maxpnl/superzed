//! Chiusura di chat con file non committati: modale a tre bottoni, prompt di commit a
//! claude, controllo git allo `Stop` e chiusura.
use super::*;
use crate::automation::Readiness;
use crate::commit_close::{
    COMMIT_PROMPT, Decision, PendingCommit, Step, UncommittedFile, commit_close_decision, count_dirty,
    dirty_close_message, files_to_check, git_status_args, repository_roots, uncommitted_files,
};
use gpui::AnyWindowHandle;

pub(crate) struct CommitClose {
    pending: PendingCommit,
    /// File sporchi al momento del click: il controllo finale guarda questi piu' quelli
    /// toccati da claude nel frattempo (`files_to_check`).
    files: Vec<UncommittedFile>,
    /// Finestra del canvas: alla chiusura serve per spostare il focus sulla vicina.
    window: Option<AnyWindowHandle>,
    _enter: Option<Task<()>>,
    _check: Option<Task<()>>,
}

/// Divide i nodi da chiudere in puliti e sporchi (col numero di file).
pub(crate) fn split_dirty(
    ids: &[NodeId],
    uncommitted: impl Fn(NodeId) -> usize,
) -> (Vec<NodeId>, Vec<(NodeId, usize)>) {
    let mut clean = Vec::new();
    let mut dirty = Vec::new();
    for id in ids {
        match uncommitted(*id) {
            0 => clean.push(*id),
            count => dirty.push((*id, count)),
        }
    }
    (clean, dirty)
}

impl CanvasView {
    pub fn is_committing(&self, node: NodeId) -> bool {
        self.commit_closes.contains_key(&node)
    }

    pub fn node_uncommitted_files(&self, node: NodeId, cx: &App) -> Vec<UncommittedFile> {
        let Some(key) = self.session_key(node) else {
            return Vec::new();
        };
        let Some(store) = superzed_sessions::SessionStore::global(cx) else {
            return Vec::new();
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        let Some(session) = store.read(cx).get(key) else {
            return Vec::new();
        };
        uncommitted_files(&session.touched_files, workspace.read(cx).project(), cx)
    }

    pub(super) fn confirm_close_dirty(
        &mut self,
        clean: Vec<NodeId>,
        dirty: Vec<(NodeId, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let titles: Vec<(String, usize)> = dirty
            .iter()
            .map(|(id, count)| {
                let title = self.model.frame(*id).map(|f| self.node_title(f, cx)).unwrap_or_default();
                (title, *count)
            })
            .collect();
        let message = dirty_close_message(&titles);
        let busy = dirty.iter().any(|(id, _)| self.chat_readiness(*id, cx) != Readiness::Ready);
        let all: Vec<NodeId> = clean.iter().copied().chain(dirty.iter().map(|(id, _)| *id)).collect();
        let landing = self.landing_after_close(&all, window, cx);
        // «Commit & close» chiude subito i puliti: se fra loro c'e' il nodo corrente il focus
        // va spostato ora, quando il modale ha gia' preso il focus e il nodo non si vede piu'
        let clean_landing = self
            .nav_current(window, cx)
            .filter(|current| clean.contains(current))
            .map(|_| self.landing_after_close(&clean, window, cx));
        let dirty_ids: Vec<NodeId> = dirty.iter().map(|(id, _)| *id).collect();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let canvas = cx.entity().downgrade();
        let canvas_for_commit = canvas.clone();
        workspace.update(cx, |ws, cx| {
            workspace::superzed_confirm::superzed_confirm_with_secondary(
                ws,
                message,
                "Close anyway",
                true,
                move |window, cx| {
                    canvas
                        .update(cx, |c, cx| {
                            c.remove_nodes(&all, cx);
                            c.land_after_close(landing, window, cx);
                        })
                        .ok();
                },
                workspace::superzed_confirm::SecondaryAction {
                    label: "Commit & close".into(),
                    disabled_reason: busy.then(|| "Wait for the chat to finish".into()),
                    on_click: Box::new(move |window, cx| {
                        canvas_for_commit
                            .update(cx, |c, cx| {
                                c.remove_nodes(&clean, cx);
                                if let Some(landing) = clean_landing {
                                    c.land_after_close(landing, window, cx);
                                }
                                for id in dirty_ids {
                                    c.start_commit_close(id, window.window_handle(), cx);
                                }
                            })
                            .ok();
                    }),
                },
                window,
                cx,
            )
        });
    }

    /// Dopo una chiusura il focus va alla vicina (inquadrata a zoom 1) o al canvas.
    pub(super) fn land_after_close(&mut self, landing: Option<NodeId>, window: &mut Window, cx: &mut Context<Self>) {
        match landing.filter(|id| self.model.frame(*id).is_some()) {
            Some(next) => {
                self.land_on(next, window, cx);
                self.zoom_to_node(next, cx);
            }
            None => self.focus_handle.focus(window, cx),
        }
    }

    fn start_commit_close(&mut self, node: NodeId, window: AnyWindowHandle, cx: &mut Context<Self>) {
        // fra il modale e il click la chat puo' essere ripartita
        if self.chat_readiness(node, cx) != Readiness::Ready {
            self.warn("The chat is busy: commit not requested".to_string(), cx);
            return;
        }
        let files = self.node_uncommitted_files(node, cx);
        // ctrl-u: il prompt del commit non va incollato in coda a una bozza non inviata
        if let Some((_, terminal)) = self.terminals.get(&node) {
            terminal.update(cx, |t, _| t.input("\x15".as_bytes()));
        }
        let enter = self.type_prompt(node, COMMIT_PROMPT, cx);
        self.commit_closes.insert(
            node,
            CommitClose { pending: PendingCommit::default(), files, window: Some(window), _enter: enter, _check: None },
        );
        cx.notify();
    }

    pub(super) fn commit_close_sessions_changed(&mut self, cx: &mut Context<Self>) {
        if self.commit_closes.is_empty() {
            return;
        }
        let nodes: Vec<NodeId> = self.commit_closes.keys().copied().collect();
        let mut changed = false;
        let mut idle = Vec::new();
        for node in nodes {
            let state = self.chat_session_state(node, cx);
            let Some(entry) = self.commit_closes.get_mut(&node) else {
                continue;
            };
            match entry.pending.on_state(state.as_ref()) {
                Step::Wait => {}
                Step::Cancel if state == Some(superzed_sessions::SessionState::Idle) => idle.push(node),
                Step::Cancel => {
                    self.commit_closes.remove(&node);
                    changed = true;
                }
            }
        }
        // `Idle` puo' essere lo `Stop` di una chat col focus il cui `Stopped` non e' ancora
        // arrivato: GPUI fonde i notify dello store ancora in coda, quindi questo observe puo'
        // girare prima dell'emit. Il defer va in coda dopo l'emit: se a quel punto il
        // controllo git non e' partito, era davvero un'interruzione (Esc/ctrl-c).
        if !idle.is_empty() {
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                this.update(cx, |c, cx| c.commit_close_interrupted(&idle, cx)).ok();
            });
        }
        if changed {
            cx.notify();
        }
    }

    fn commit_close_interrupted(&mut self, nodes: &[NodeId], cx: &mut Context<Self>) {
        let mut changed = false;
        for node in nodes {
            let checking = self.commit_closes.get(node).is_some_and(|entry| entry.pending.checking);
            if !checking
                && self.chat_session_state(*node, cx) == Some(superzed_sessions::SessionState::Idle)
                && self.commit_closes.remove(node).is_some()
            {
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    pub(super) fn commit_close_stopped(&mut self, node: NodeId, failed: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.commit_closes.get(&node) else {
            return;
        };
        // lo `Stop` di un turno che non e' quello del commit (il prompt non e' partito):
        // non dice nulla sui file. `started` arriva dall'observe dello store, che ha gia'
        // visto `Working` perche' UserPromptSubmit e `Stop` sono messaggi ipc distinti,
        // ciascuno col suo ciclo di effetti
        if !entry.pending.started {
            return;
        }
        if failed {
            self.commit_closes.remove(&node);
            cx.notify();
            return;
        }
        let touched = self
            .session_key(node)
            .zip(superzed_sessions::SessionStore::global(cx))
            .and_then(|(key, store)| store.read(cx).get(key).map(|session| session.touched_files.clone()))
            .unwrap_or_default();
        let roots = self
            .workspace
            .upgrade()
            .map(|workspace| repository_roots(workspace.read(cx).project(), cx))
            .unwrap_or_default();
        let files = files_to_check(&entry.files, &touched, &roots);
        let window = entry.window;
        if let Some(entry) = self.commit_closes.get_mut(&node) {
            entry.pending.checking = true;
        }
        let runner = self.git_runner.clone();
        let mut by_root: HashMap<PathBuf, Vec<&UncommittedFile>> = HashMap::default();
        for file in &files {
            by_root.entry(file.repo_root.clone()).or_default().push(file);
        }
        let checks: Vec<_> = by_root
            .iter()
            .map(|(root, files)| runner.run("git", &git_status_args(root, files), None))
            .collect();
        let check = cx.spawn(async move |this, cx| {
            let mut dirty = Some(0usize);
            for check in checks {
                match check.await {
                    Ok(output) if output.success => {
                        dirty = dirty.map(|count| count + count_dirty(&output.stdout));
                    }
                    Ok(output) => {
                        log::warn!("superzed: git status before closing failed: {}", output.stderr.trim());
                        dirty = None;
                    }
                    Err(error) => {
                        log::warn!("superzed: git status before closing failed: {error}");
                        dirty = None;
                    }
                }
            }
            match window {
                Some(window) => cx
                    .update_window(window, |_, window, cx| {
                        this.update(cx, |c, cx| c.finish_commit_close(node, dirty, Some(window), cx))
                    })
                    .and_then(|result| result)
                    .log_err(),
                None => this.update(cx, |c, cx| c.finish_commit_close(node, dirty, None, cx)).log_err(),
            };
        });
        if let Some(entry) = self.commit_closes.get_mut(&node) {
            entry._check = Some(check);
        }
    }

    fn finish_commit_close(
        &mut self,
        node: NodeId,
        dirty: Option<usize>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.commit_closes.remove(&node).is_none() {
            return;
        }
        if commit_close_decision(false, dirty) == Decision::Keep {
            cx.notify();
            return;
        }
        let Some(window) = window else {
            self.remove_nodes(&[node], cx);
            return;
        };
        // la chiusura arriva a turno finito: il focus si sposta solo se era sulla chat chiusa
        // (o non c'era piu'), non se l'utente nel frattempo e' andato altrove
        let node_focused = self.contents.get(&node).is_some_and(|c| c.focus_handle(cx).contains_focused(window, cx));
        let landing = (node_focused || window.focused(cx).is_none())
            .then(|| self.landing_after_close(&[node], window, cx));
        self.remove_nodes(&[node], cx);
        if let Some(landing) = landing {
            self.land_after_close(landing, window, cx);
        }
    }

    /// Solo test: una chiusura con commit gia' chiesta, senza scrivere nella chat.
    #[cfg(test)]
    pub(crate) fn insert_commit_close_for_test(
        &mut self,
        node: NodeId,
        files: Vec<UncommittedFile>,
        window: Option<AnyWindowHandle>,
    ) {
        self.commit_closes.insert(
            node,
            CommitClose { pending: PendingCommit::default(), files, window, _enter: None, _check: None },
        );
    }

    /// Solo test: (iniziato, controllo git in corso) della chiusura con commit del nodo.
    #[cfg(test)]
    pub(crate) fn commit_close_progress(&self, node: NodeId) -> Option<(bool, bool)> {
        self.commit_closes.get(&node).map(|entry| (entry.pending.started, entry.pending.checking))
    }
}
