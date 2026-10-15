pub mod age;
pub mod automation;
pub mod automation_form;
pub mod automations_layout;
pub mod automations_node;
pub mod canvas_view;
pub mod commit_close;
pub mod done_review;
pub mod kanban_layout;
pub mod kanban_node;
pub mod mail_node;
pub mod model;
pub mod node;
pub mod note_node;
pub mod persistence;
pub mod process;
pub mod prompt_picker;
pub mod registry;
pub mod schedule;
pub mod severity_style;
pub mod source_node;
pub mod source_picker;
#[cfg(test)]
pub(crate) mod test_support;
pub mod terminal_node;
pub mod text_prompt;
pub mod viewport;

pub use canvas_view::{CanvasView, ClaudePtyOverride, DefaultContainers, NodeSessionInit, SessionCard, golden_color, node_color};
pub use commit_close::{UncommittedFile, uncommitted_files};
pub use model::{DockKind, NodeFrame, NodeId, NodeKind};
pub use node::NodeContent;

use gpui::{App, AppContext as _, Context, Entity, Focusable as _, Window};
use superzed_actions::{FocusCanvas, FocusEditor, NewNote};
use workspace::Workspace;

pub fn init(cx: &mut App) {
    // le notifiche del canale `notify`: la chiave del nodo dice canvas e progetto
    cx.set_global(superzed_sessions::store::NotifyRouter(crate::registry::route_notify));
    cx.set_global(workspace::superzed_tabs::RenameTabHook(rename_project_tab));
    cx.set_global(workspace::superzed_tabs::CloseProjectTabHook(close_project_tab));
    cx.set_global(workspace::superzed_layout::CanvasHasNodes(|view, cx| {
        view.clone()
            .downcast::<CanvasView>()
            .is_ok_and(|canvas| !canvas.read(cx).model().frames().is_empty())
    }));
    cx.set_global(workspace::superzed_tabs::TabAttentionHook(|ws, cx| {
        use workspace::superzed_tabs::TabAttention;
        match canvas_for(ws.read(cx)).map(|c| c.read(cx).attention(cx)) {
            Some(superzed_sessions::Attention::NeedsInput) => TabAttention::NeedsInput,
            Some(superzed_sessions::Attention::Done) => TabAttention::Done,
            _ => TabAttention::None,
        }
    }));
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        // superzed: ogni workspace con finestra ha il suo canvas al centro
        if let Some(window) = window {
            let weak = cx.entity().downgrade();
            let canvas = cx.new(|cx| CanvasView::new(weak, window, cx));
            let focus = canvas.focus_handle(cx);
            workspace.set_superzed_canvas(canvas.into(), focus, cx);
        }
        workspace
            .register_action(|ws, _: &FocusCanvas, window, cx| ws.superzed_focus_canvas(window, cx))
            .register_action(|ws, _: &FocusEditor, window, cx| ws.superzed_focus_editor(window, cx))
            .register_action(|ws, _: &NewNote, window, cx| {
                with_canvas(ws, window, cx, |canvas, window, cx| {
                    canvas.new_note(window, cx);
                })
            })
            .register_action(|ws, _: &superzed_actions::NewTerminal, window, cx| {
                with_canvas(ws, window, cx, |c, window, cx| {
                    c.open_terminal(NodeKind::Shell, None, &serde_json::Value::Null, window, cx)
                })
            })
            .register_action(|ws, _: &superzed_actions::NewClaudeNode, window, cx| {
                with_canvas(ws, window, cx, |c, window, cx| {
                    // con il focus nel kanban (o in una sua chat) la chat nasce agganciata
                    if c.focus_in_kanban(window, cx) {
                        c.new_claude_in_kanban(window, cx)
                    } else {
                        c.open_terminal(NodeKind::Claude, None, &serde_json::Value::Null, window, cx)
                    }
                })
            })
            .register_action(|ws, _: &superzed_actions::NewClaudeNodePick, window, cx| {
                pick_account_for_new_claude(ws, window, cx)
            })
            .register_action(|ws, _: &superzed_actions::NewClaudeNodeFromPrompt, window, cx| {
                crate::prompt_picker::new_claude_from_prompt_picker(ws, window, cx)
            })
            .register_action(|ws, _: &superzed_actions::RenameProjectTab, window, cx| {
                rename_project_tab(ws, window, cx)
            })
            .register_action(|ws, _: &superzed_actions::OpenKanban, window, cx| {
                with_canvas(ws, window, cx, |c, window, cx| {
                    c.open_kanban(window, cx);
                })
            })
            .register_action(|ws, _: &crate::source_picker::NewSourcePanel, window, cx| {
                crate::source_picker::new_source_panel_from_picker(ws, window, cx)
            })
            // Task 22: con il focus in un pannello della sidebar (senza la propria azione
            // sul canvas, piu' annidata: vedi `canvas_view.rs`) FocusNextArea/FocusPreviousArea
            // arrivano qui e vanno inoltrate al canvas del progetto attivo
            .register_action(|ws, _: &superzed_actions::FocusNextArea, window, cx| {
                with_canvas(ws, window, cx, |c, window, cx| c.focus_area(true, window, cx));
            })
            .register_action(|ws, _: &superzed_actions::FocusPreviousArea, window, cx| {
                with_canvas(ws, window, cx, |c, window, cx| c.focus_area(false, window, cx));
            });
        // superzed: azioni delle tab di progetto (next/prev/activate/close)
        workspace::superzed_tabs::register_tab_actions(workspace);
    })
    .detach();
}

/// `cmd-shift-n`: picker degli account; la chat nasce nel kanban se il focus e' li'.
fn pick_account_for_new_claude(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(canvas) = canvas_for(ws) else {
        return;
    };
    let in_kanban = canvas.read(cx).focus_in_kanban(window, cx);
    let Some(store) = superzed_accounts::AccountStore::global(cx) else {
        with_canvas(ws, window, cx, move |c, window, cx| {
            c.new_claude_with_account(superzed_accounts::SYSTEM_ID.to_string(), in_kanban, window, cx)
        });
        return;
    };
    let loads = superzed_accounts::current_loads(cx);
    // dal workspace, non dal canvas: il workspace e' gia' in update (doppio borrow)
    let project = ws.project().read(cx).visible_worktrees(cx).next().map(|tree| tree.read(cx).abs_path().to_path_buf());
    let (rows, selected) = superzed_accounts::picker::picker_rows(store.read(cx), &loads, project.as_deref());
    superzed_accounts::picker::open_account_picker(
        ws,
        rows,
        selected,
        move |id, window, cx| {
            // a fine update: il canvas rilegge il workspace
            window.defer(cx, move |window, cx| {
                canvas.update(cx, |c, cx| c.new_claude_with_account(id, in_kanban, window, cx));
            });
        },
        window,
        cx,
    );
}

/// Prompt di rinomina della tab del workspace (azione e menu contestuale della tab).
fn rename_project_tab(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let current = ws
        .superzed_state()
        .tab_name
        .clone()
        .map(|n| n.to_string())
        .unwrap_or_default();
    let weak = cx.entity().downgrade();
    crate::text_prompt::text_prompt(
        ws,
        "Tab name (empty = folder name)",
        &current,
        move |name, _, cx| {
            weak.update(cx, |ws, cx| {
                ws.update_superzed_state(
                    |s| s.tab_name = (!name.is_empty()).then(|| name.into()),
                    cx,
                )
            })
            .ok();
        },
        window,
        cx,
    );
}

/// cmd-shift-w: con il canvas propone di chiudere le chat ferme (`close_idle_chats_with_confirm`);
/// senza, la conferma di chiusura del progetto di sempre.
fn close_project_tab(
    workspace: &Entity<Workspace>,
    request: workspace::superzed_tabs::CloseProjectRequest,
    window: &mut Window,
    cx: &mut App,
) {
    let workspace::superzed_tabs::CloseProjectRequest { label, close_project } = request;
    match canvas_for(workspace.read(cx)) {
        Some(canvas) => canvas.update(cx, |canvas, cx| {
            canvas.close_idle_chats_with_confirm(label, close_project, window, cx)
        }),
        None => workspace.update(cx, |ws, cx| {
            workspace::superzed_confirm::superzed_confirm_action(
                ws,
                format!("Close the project {label}?"),
                "Close",
                true,
                close_project,
                window,
                cx,
            )
        }),
    }
}

pub fn canvas_for(workspace: &Workspace) -> Option<Entity<CanvasView>> {
    workspace.superzed_canvas()?.downcast::<CanvasView>().ok()
}

/// Esegue `f` sul canvas del workspace, se c'e'. Rimandato a fine update: il
/// chiamante e' un'azione del Workspace, e il canvas rilegge il Workspace
/// (root del progetto, database id), che sarebbe ancora in update.
pub fn with_canvas(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    f: impl FnOnce(&mut CanvasView, &mut Window, &mut Context<CanvasView>) + 'static,
) {
    if let Some(canvas) = canvas_for(workspace) {
        window.defer(cx, move |window, cx| {
            canvas.update(cx, |canvas, cx| f(canvas, window, cx));
        });
    }
}
