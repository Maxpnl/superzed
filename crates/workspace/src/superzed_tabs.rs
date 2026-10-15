// superzed: tab di progetto. L'ordine e' quello dei project_groups di MultiWorkspace
// letto al contrario: upstream inserisce i gruppi nuovi in testa, cosi' un progetto
// aperto dopo compare a destra. I gruppi sono gia' serializzati in ordine.
use crate::{MultiWorkspace, OpenMode, Workspace};
use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, IntoElement, MouseButton, SharedString,
    Window, div, px,
};
use project::ProjectGroupKey;
use superzed_actions::{
    ActivateProjectTab, CloseProjectTab, CycleProjectTabColor, NextProjectTab, PrevProjectTab, RenameProjectTab,
};
use ui::{IconButton, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*};
use util::ResultExt as _;

pub const TAB_COLORS: u8 = 8;

pub struct ProjectTab {
    pub key: Option<ProjectGroupKey>,
    pub workspace: Option<Entity<Workspace>>,
    pub label: SharedString,
    /// Percorso completo del progetto (tooltip della tab).
    pub path: Option<SharedString>,
    pub color: Option<u8>,
    pub is_active: bool,
}

/// Cartelle del gruppo, o della prima worktree visibile del workspace.
fn tab_path(key: Option<&ProjectGroupKey>, workspace: Option<&Entity<Workspace>>, cx: &App) -> Option<SharedString> {
    let from_key = key
        .map(|k| k.path_list().paths().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))
        .filter(|p| !p.is_empty());
    from_key
        .or_else(|| {
            let ws = workspace?.read(cx);
            let wt = ws.project().read(cx).visible_worktrees(cx).next()?;
            Some(wt.read(cx).abs_path().display().to_string())
        })
        .map(SharedString::from)
}

fn root_name(workspace: &Workspace, cx: &App) -> Option<SharedString> {
    workspace
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|wt| SharedString::from(wt.read(cx).root_name().to_string()))
}

fn key_name(key: &ProjectGroupKey) -> SharedString {
    key.path_list()
        .paths()
        .first()
        .and_then(|p| p.file_name())
        .map(|n| SharedString::from(n.to_string_lossy().to_string()))
        .unwrap_or_else(|| SharedString::from("untitled"))
}

fn tab_for(key: Option<ProjectGroupKey>, workspace: Option<Entity<Workspace>>, active: &Entity<Workspace>, cx: &App) -> ProjectTab {
    let state = workspace.as_ref().map(|ws| ws.read(cx).superzed_state().clone());
    let label = state
        .as_ref()
        .and_then(|s| s.tab_name.clone())
        .or_else(|| workspace.as_ref().and_then(|ws| root_name(ws.read(cx), cx)))
        .or_else(|| key.as_ref().map(key_name))
        .unwrap_or_else(|| SharedString::from("untitled"));
    ProjectTab {
        is_active: workspace.as_ref() == Some(active),
        path: tab_path(key.as_ref(), workspace.as_ref(), cx),
        color: state.and_then(|s| s.tab_color),
        key,
        workspace,
        label,
    }
}

pub fn project_tabs(mw: &MultiWorkspace, cx: &App) -> Vec<ProjectTab> {
    let active = mw.workspace().clone();
    let active_key = active.read(cx).project_group_key(cx);
    let mut tabs: Vec<ProjectTab> = mw
        .project_group_keys()
        .into_iter()
        .rev()
        .map(|key| {
            let workspace = if key == active_key {
                Some(active.clone())
            } else {
                mw.last_active_workspace_for_group(&key, cx)
                    .or_else(|| mw.workspaces_for_project_group(&key, cx).into_iter().next())
            };
            tab_for(Some(key), workspace, &active, cx)
        })
        .collect();
    // workspace attivo senza gruppo (finestra vuota): una tab in fondo
    if !tabs.iter().any(|t| t.is_active) {
        tabs.push(tab_for(None, Some(active.clone()), &active, cx));
    }
    tabs
}

pub fn active_tab_index(tabs: &[ProjectTab]) -> usize {
    tabs.iter().position(|t| t.is_active).unwrap_or(0)
}

/// `ctrl-n`: tab n (1-based); 9 = ultima, come nei browser.
pub fn tab_index_for(n: usize, len: usize) -> Option<usize> {
    if len == 0 || n == 0 {
        return None;
    }
    if n == 9 {
        return Some(len - 1);
    }
    (n <= len).then(|| n - 1)
}

pub fn activate_project_tab(
    mw: &mut MultiWorkspace,
    index: usize,
    window: &mut Window,
    cx: &mut Context<MultiWorkspace>,
) {
    let mut tabs = project_tabs(mw, cx);
    if index >= tabs.len() {
        return;
    }
    let tab = tabs.swap_remove(index);
    if let Some(workspace) = tab.workspace {
        mw.activate(workspace, None, window, cx);
    } else if let Some(key) = tab.key {
        // gruppo senza workspace vivo (es. dopo un restore fallito): lo si apre ora
        mw.find_or_create_local_workspace(
            key.path_list().clone(),
            Some(key),
            None,
            OpenMode::Activate,
            None,
            window,
            cx,
        )
        .detach_and_log_err(cx);
    }
}

pub fn cycle_project_tab(
    mw: &mut MultiWorkspace,
    step: isize,
    window: &mut Window,
    cx: &mut Context<MultiWorkspace>,
) {
    let tabs = project_tabs(mw, cx);
    if tabs.len() < 2 {
        return;
    }
    let len = tabs.len() as isize;
    let next = (active_tab_index(&tabs) as isize + step).rem_euclid(len) as usize;
    activate_project_tab(mw, next, window, cx);
}

/// Etichetta della tab `index`, il workspace attivo (dove si mostra il modale: e' l'unico
/// disegnato) e la chiusura del gruppo della tab (senza conferma).
fn project_tab_closer(
    mw: &Entity<MultiWorkspace>,
    index: usize,
    cx: &App,
) -> Option<(SharedString, Entity<Workspace>, Box<dyn FnOnce(&mut Window, &mut App)>)> {
    let mw_ref = mw.read(cx);
    let tab = project_tabs(mw_ref, cx).into_iter().nth(index)?;
    let key = tab.key?;
    let workspace = mw_ref.workspace().clone();
    let mw_weak = mw.downgrade();
    let close = move |window: &mut Window, cx: &mut App| {
        mw_weak
            .update(cx, |mw, cx| mw.remove_project_group(&key, window, cx).detach_and_log_err(cx))
            .ok();
    };
    Some((tab.label, workspace, Box::new(close)))
}

pub fn close_project_tab_with_confirm(
    mw: Entity<MultiWorkspace>,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((label, workspace, close_project)) = project_tab_closer(&mw, index, cx) else {
        return;
    };
    workspace.update(cx, |ws, cx| {
        crate::superzed_confirm::superzed_confirm_action(
            ws,
            format!("Close the project {label}?"),
            "Close",
            true,
            close_project,
            window,
            cx,
        )
    });
}

/// Chiusura della tab di progetto `index` da cmd-shift-w: passa dal `CloseProjectTabHook`
/// se c'e' (il canvas propone di chiudere le chat ferme), altrimenti dalla conferma.
pub fn close_project_tab_action(mw: Entity<MultiWorkspace>, index: usize, window: &mut Window, cx: &mut App) {
    let Some(hook) = cx.try_global::<CloseProjectTabHook>().map(|hook| hook.0) else {
        return close_project_tab_with_confirm(mw, index, window, cx);
    };
    let Some((label, workspace, close_project)) = project_tab_closer(&mw, index, cx) else {
        return;
    };
    hook(&workspace, CloseProjectRequest { label, close_project }, window, cx);
}

/// Esegue `f` sul MultiWorkspace del workspace, fuori dall'update corrente:
/// `activate` aggiorna anche il workspace da cui parte l'azione.
fn defer_on_multi_workspace(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&mut MultiWorkspace, &mut Window, &mut Context<MultiWorkspace>) + 'static,
) {
    let Some(mw) = workspace.multi_workspace().and_then(|m| m.upgrade()) else {
        return;
    };
    window.defer(cx, move |window, cx| mw.update(cx, |mw, cx| f(mw, window, cx)));
}

pub fn register_tab_actions(workspace: &mut Workspace) {
    workspace
        .register_action(|ws, _: &NextProjectTab, window, cx| {
            defer_on_multi_workspace(ws, window, cx, |mw, window, cx| cycle_project_tab(mw, 1, window, cx))
        })
        .register_action(|ws, _: &PrevProjectTab, window, cx| {
            defer_on_multi_workspace(ws, window, cx, |mw, window, cx| cycle_project_tab(mw, -1, window, cx))
        })
        .register_action(|ws, action: &ActivateProjectTab, window, cx| {
            let n = action.0;
            defer_on_multi_workspace(ws, window, cx, move |mw, window, cx| {
                if let Some(ix) = tab_index_for(n, project_tabs(mw, cx).len()) {
                    activate_project_tab(mw, ix, window, cx);
                }
            })
        })
        .register_action(|ws, _: &CloseProjectTab, window, cx| {
            let Some(mw) = ws.multi_workspace().and_then(|m| m.upgrade()) else {
                return;
            };
            window.defer(cx, move |window, cx| {
                let ix = active_tab_index(&project_tabs(mw.read(cx), cx));
                close_project_tab_action(mw, ix, window, cx);
            });
        })
        .register_action(|ws, _: &CycleProjectTabColor, window, cx| {
            // i colori delle altre tab si leggono dal MultiWorkspace, che legge anche
            // questo workspace: fuori dall'update in corso
            let Some(mw) = ws.multi_workspace().and_then(|m| m.upgrade()) else {
                return;
            };
            let target = cx.entity();
            window.defer(cx, move |_, cx| cycle_tab_color(&mw, &target, cx));
        });
}

/// Rinomina di una tab, fornita da `superzed_canvas` (che ha il prompt di testo).
pub struct RenameTabHook(pub fn(&mut Workspace, &mut Window, &mut Context<Workspace>));
impl gpui::Global for RenameTabHook {}

/// Richiesta di chiusura di una tab (cmd-shift-w): etichetta e chiusura del gruppo,
/// da chiamare quando l'utente conferma.
pub struct CloseProjectRequest {
    pub label: SharedString,
    pub close_project: Box<dyn FnOnce(&mut Window, &mut App)>,
}

/// cmd-shift-w con un canvas: `superzed_canvas` propone di chiudere le chat ferme del
/// progetto, con «Close project» come secondario. Senza hook, la conferma di chiusura.
pub struct CloseProjectTabHook(pub fn(&Entity<Workspace>, CloseProjectRequest, &mut Window, &mut App));
impl gpui::Global for CloseProjectTabHook {}

/// Una sessione della tab aspetta l'utente o ha finito. Fornito da `superzed_canvas`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAttention {
    None,
    Done,
    NeedsInput,
}

pub struct TabAttentionHook(pub fn(&Entity<Workspace>, &App) -> TabAttention);
impl gpui::Global for TabAttentionHook {}

pub fn tab_attention(workspace: &Entity<Workspace>, cx: &App) -> TabAttention {
    cx.try_global::<TabAttentionHook>()
        .map(|hook| (hook.0)(workspace, cx))
        .unwrap_or(TabAttention::None)
}

/// Colore successivo non usato da altre tab; finiti i colori, nessun colore.
fn next_free_color(current: Option<u8>, used: &[u8]) -> Option<u8> {
    (current.map_or(0, |c| c + 1)..TAB_COLORS).find(|c| !used.contains(c))
}

fn cycle_tab_color(mw: &Entity<MultiWorkspace>, target: &Entity<Workspace>, cx: &mut App) {
    let used: Vec<u8> = project_tabs(mw.read(cx), cx)
        .into_iter()
        .filter(|t| t.workspace.as_ref() != Some(target))
        .filter_map(|t| t.color)
        .collect();
    target.update(cx, |ws, cx| {
        ws.update_superzed_state(|s| s.tab_color = next_free_color(s.tab_color, &used), cx)
    });
}

/// "Colore successivo" dal menu della tab `ix`, senza attivarla.
pub fn cycle_tab_color_at(mw: &Entity<MultiWorkspace>, ix: usize, cx: &mut App) {
    let target = project_tabs(mw.read(cx), cx).get(ix).and_then(|t| t.workspace.clone());
    if let Some(target) = target {
        cycle_tab_color(mw, &target, cx);
    }
}

/// "Rinomina" dal menu della tab `ix`: la attiva (il prompt compare li') e chiama il
/// prompt direttamente sul suo workspace. Un'azione dispatchata qui andrebbe persa:
/// la frame disegnata non contiene ancora il workspace appena attivato.
pub fn rename_project_tab_at(mw: &Entity<MultiWorkspace>, ix: usize, window: &mut Window, cx: &mut App) {
    let mw = mw.clone();
    window.defer(cx, move |window, cx| {
        let Some(RenameTabHook(rename)) = cx.try_global::<RenameTabHook>().map(|h| RenameTabHook(h.0)) else {
            return;
        };
        mw.update(cx, |mw, cx| activate_project_tab(mw, ix, window, cx));
        let workspace = mw.read(cx).workspace().clone();
        window.focus(&workspace.focus_handle(cx), cx);
        workspace.update(cx, |ws, cx| rename(ws, window, cx));
    });
}

/// Sposta la tab `from` in posizione `to` (indici a schermo). A schermo l'ordine e'
/// quello dei gruppi al contrario: spostare a destra = gruppo verso l'indice 0 ("up").
pub fn move_project_tab(
    mw: &mut MultiWorkspace,
    from: usize,
    to: usize,
    cx: &mut Context<MultiWorkspace>,
) {
    let tabs = project_tabs(mw, cx);
    let (Some(tab), true) = (tabs.get(from), to < tabs.len()) else {
        return;
    };
    let Some(key) = tab.key.clone() else {
        return;
    };
    for _ in 0..from.abs_diff(to) {
        let moved = if to > from {
            mw.move_project_group_up(&key, cx)
        } else {
            mw.move_project_group_down(&key, cx)
        };
        if !moved {
            break;
        }
    }
}

#[derive(Clone)]
pub struct DraggedProjectTab {
    pub ix: usize,
    pub label: SharedString,
}

impl gpui::Render for DraggedProjectTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(cx.theme().colors().tab_active_background)
            .child(Label::new(self.label.clone()).size(LabelSize::Small))
    }
}

fn tab_color(ix: Option<u8>, cx: &App) -> Option<gpui::Hsla> {
    ix.map(|i| cx.theme().players().color_for_participant(u32::from(i % TAB_COLORS)).cursor)
}

pub fn render_project_tabs(
    mw: &Entity<MultiWorkspace>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let tabs = project_tabs(mw.read(cx), cx);
    let colors = cx.theme().colors();
    let items = tabs.into_iter().enumerate().map(|(ix, tab)| {
        let activate = mw.downgrade();
        let close_mw = mw.clone();
        let middle_mw = mw.clone();
        let drop_mw = mw.downgrade();
        let dot = tab_color(tab.color, cx);
        let attention = tab.workspace.as_ref().map_or(TabAttention::None, |ws| tab_attention(ws, cx));
        let tab_element = div()
            .id(("superzed-project-tab", ix))
            .debug_selector(move || format!("superzed-project-tab-{ix}"))
            .group("superzed-tab")
            .h_full()
            .flex_none()
            .px_2()
            .gap_1()
            .flex()
            .items_center()
            .cursor_pointer()
            .rounded_sm()
            .when(tab.is_active, |d| d.bg(colors.tab_active_background))
            .when(!tab.is_active, |d| d.hover(|d| d.bg(colors.ghost_element_hover)))
            .when_some(tab.path.clone(), |d, path| d.tooltip(Tooltip::text(path)))
            // il mouse down sulle tab non deve far partire il drag della finestra
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_drag(
                DraggedProjectTab { ix, label: tab.label.clone() },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            )
            .drag_over::<DraggedProjectTab>(move |d, dragged, _, cx| {
                let d = d.bg(cx.theme().colors().drop_target_background);
                let d = if ix < dragged.ix {
                    d.border_l_2()
                } else if ix > dragged.ix {
                    d.border_r_2()
                } else {
                    d
                };
                d.border_color(cx.theme().colors().drop_target_border)
            })
            .on_drop({
                let mw = drop_mw;
                move |dragged: &DraggedProjectTab, _, cx| {
                    mw.update(cx, |mw, cx| move_project_tab(mw, dragged.ix, ix, cx)).ok();
                }
            })
            .on_click(move |_, window, cx| {
                activate
                    .update(cx, |mw, cx| activate_project_tab(mw, ix, window, cx))
                    .ok();
            })
            .on_mouse_up(MouseButton::Middle, move |_, window, cx| {
                close_project_tab_with_confirm(middle_mw.clone(), ix, window, cx)
            })
            .when(attention == TabAttention::NeedsInput, |d| {
                d.child(
                    div()
                        .debug_selector(move || format!("superzed-tab-attention-{ix}"))
                        .child(Icon::new(IconName::BellRing).size(IconSize::XSmall).color(Color::Warning)),
                )
            })
            .when(attention == TabAttention::Done, |d| {
                d.child(
                    div()
                        .debug_selector(move || format!("superzed-tab-attention-{ix}"))
                        .size(px(6.))
                        .rounded_full()
                        .bg(cx.theme().status().success),
                )
            })
            .when_some(dot, |d, color| d.child(div().size(px(8.)).rounded_full().bg(color)))
            .child(
                div().max_w(rems(14.)).min_w_0().overflow_hidden().child(
                    Label::new(tab.label.clone())
                        .size(if tab.is_active { LabelSize::Default } else { LabelSize::Small })
                        .color(if tab.is_active { Color::Default } else { Color::Muted })
                        .truncate(),
                ),
            )
            .child(
                IconButton::new(("superzed-tab-close", ix), IconName::Close)
                    .icon_size(IconSize::XSmall)
                    // sempre visibile sulla tab attiva, sulle altre al passaggio del mouse
                    .when(!tab.is_active, |b| b.visible_on_hover("superzed-tab"))
                    .tooltip(Tooltip::for_action_title("Close project", &CloseProjectTab))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        close_project_tab_with_confirm(close_mw.clone(), ix, window, cx)
                    }),
            )
            .into_any_element();
        let rename_mw = mw.clone();
        let color_mw = mw.clone();
        let menu_close_mw = mw.clone();
        ui::right_click_menu(("superzed-project-tab-menu", ix))
            .trigger(move |_, _, _| tab_element)
            .menu(move |window, cx| {
                let (rename_mw, color_mw, close_mw) =
                    (rename_mw.clone(), color_mw.clone(), menu_close_mw.clone());
                ui::ContextMenu::build(window, cx, move |menu, _, _| {
                    // le azioni solo per mostrarne la scorciatoia: il clic agisce sulla tab `ix`
                    menu.entry("Rename", Some(Box::new(RenameProjectTab)), move |window, cx| {
                        rename_project_tab_at(&rename_mw, ix, window, cx)
                    })
                    .entry("Next color", Some(Box::new(CycleProjectTabColor)), move |_, cx| {
                        cycle_tab_color_at(&color_mw, ix, cx)
                    })
                    .separator()
                    .entry("Close", Some(Box::new(CloseProjectTab)), move |window, cx| {
                        close_project_tab_with_confirm(close_mw.clone(), ix, window, cx)
                    })
                })
            })
            .into_any_element()
    });
    // le tab scorrono in orizzontale invece di spingere fuori il resto della barra
    h_flex()
        .id("superzed-project-tabs")
        .h_full()
        .min_w_0()
        .flex_shrink(1.)
        .overflow_x_scroll()
        .gap_px()
        .children(items)
        .into_any_element()
}

/// Riapre come workspace vivi tutti i gruppi ripristinati che non ne hanno uno,
/// in modalita' Add (la tab attiva resta quella di upstream). I gruppi la cui
/// cartella non esiste piu' vengono tolti.
pub async fn open_restored_tabs(
    window_handle: gpui::WindowHandle<MultiWorkspace>,
    fs: std::sync::Arc<dyn fs::Fs>,
    cx: &mut gpui::AsyncApp,
) {
    let Ok(pending) = window_handle.read_with(cx, |mw, cx| {
        mw.project_group_keys()
            .into_iter()
            .filter(|key| {
                mw.last_active_workspace_for_group(key, cx).is_none()
                    && mw.workspaces_for_project_group(key, cx).is_empty()
            })
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    for key in pending {
        let mut exists = false;
        for path in key.path_list().paths() {
            if fs.is_dir(path).await {
                exists = true;
            }
        }
        // superzed: niente `Task::map` per `Task<Result<Entity<Workspace>>>`,
        // i due rami restano separati e ognuno si aspetta per conto proprio.
        if exists {
            let task = window_handle.update(cx, |mw, window, cx| {
                mw.find_or_create_local_workspace(
                    key.path_list().clone(),
                    Some(key.clone()),
                    None,
                    OpenMode::Add,
                    None,
                    window,
                    cx,
                )
            });
            if let Ok(task) = task {
                task.await.log_err();
            }
        } else {
            let task = window_handle.update(cx, |mw, window, cx| {
                mw.remove_project_group(&key, window, cx)
            });
            if let Ok(task) = task {
                task.await.log_err();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs::FakeFs;
    use gpui::{TestAppContext, VisualTestContext};
    use project::{DisableAiSettings, Project};
    use serde_json::json;
    use settings::{Settings as _, SettingsStore};
    use util::path;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            DisableAiSettings::register(cx);
        });
    }

    /// Finestra con i progetti a, b, c aperti in quest'ordine (c attivo).
    async fn three_projects(
        cx: &mut TestAppContext,
    ) -> (Entity<MultiWorkspace>, &mut VisualTestContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        for name in ["a", "b", "c"] {
            fs.insert_tree(format!("/{name}"), json!({ "f.txt": "" })).await;
        }
        let pa = Project::test(fs.clone(), [path!("/a").as_ref()], cx).await;
        let pb = Project::test(fs.clone(), [path!("/b").as_ref()], cx).await;
        let pc = Project::test(fs.clone(), [path!("/c").as_ref()], cx).await;
        let (mw, cx) = cx.add_window_view(|window, cx| MultiWorkspace::test_new(pa, window, cx));
        mw.update_in(cx, |mw, window, cx| {
            mw.test_add_workspace(pb, window, cx);
            mw.test_add_workspace(pc, window, cx);
        });
        cx.run_until_parked();
        (mw, cx)
    }

    fn labels(mw: &Entity<MultiWorkspace>, cx: &mut VisualTestContext) -> Vec<String> {
        mw.read_with(cx, |mw, cx| project_tabs(mw, cx).iter().map(|t| t.label.to_string()).collect())
    }

    fn active(mw: &Entity<MultiWorkspace>, cx: &mut VisualTestContext) -> usize {
        mw.read_with(cx, |mw, cx| active_tab_index(&project_tabs(mw, cx)))
    }

    thread_local! {
        static HOOK_LABEL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }

    #[gpui::test]
    async fn close_project_tab_goes_through_the_hook_when_set(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        cx.update(|_, cx| {
            cx.set_global(CloseProjectTabHook(|_, request, _, _| {
                HOOK_LABEL.with(|label| *label.borrow_mut() = Some(request.label.to_string()));
            }))
        });
        let workspace = mw.read_with(cx, |mw, _| mw.workspace().clone());
        workspace.update_in(cx, |ws, window, cx| {
            register_tab_actions(ws);
            window.focus(&ws.focus_handle(cx), cx);
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.dispatch_action(CloseProjectTab);
        cx.run_until_parked();
        assert_eq!(HOOK_LABEL.with(|label| label.borrow().clone()).as_deref(), Some("c"));
        assert!(cx.debug_bounds("superzed-confirm-ok").is_none(), "nessuna conferma di chiusura del progetto");
        assert_eq!(labels(&mw, cx), vec!["a", "b", "c"]);
    }

    #[gpui::test]
    async fn new_projects_appear_on_the_right(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        assert_eq!(labels(&mw, cx), vec!["a", "b", "c"]);
        assert_eq!(active(&mw, cx), 2);
    }

    #[gpui::test]
    async fn activate_by_index_and_cycle_with_wrap(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        mw.update_in(cx, |mw, window, cx| activate_project_tab(mw, 0, window, cx));
        cx.run_until_parked();
        assert_eq!(active(&mw, cx), 0);
        mw.update_in(cx, |mw, window, cx| cycle_project_tab(mw, -1, window, cx));
        cx.run_until_parked();
        assert_eq!(active(&mw, cx), 2, "prev dalla prima va all'ultima");
        mw.update_in(cx, |mw, window, cx| cycle_project_tab(mw, 1, window, cx));
        cx.run_until_parked();
        assert_eq!(active(&mw, cx), 0, "next dall'ultima va alla prima");
    }

    #[test]
    fn tab_index_for_numbers() {
        assert_eq!(tab_index_for(1, 3), Some(0));
        assert_eq!(tab_index_for(3, 3), Some(2));
        assert_eq!(tab_index_for(4, 3), None);
        assert_eq!(tab_index_for(9, 3), Some(2), "9 = ultima tab");
        assert_eq!(tab_index_for(0, 3), None);
        assert_eq!(tab_index_for(1, 0), None);
    }

    #[gpui::test]
    async fn custom_name_and_color_come_from_project_state(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        let ws = mw.read_with(cx, |mw, _| mw.workspace().clone());
        ws.update(cx, |ws, cx| {
            ws.update_superzed_state(|s| {
                s.tab_name = Some("Web".into());
                s.tab_color = Some(4);
            }, cx)
        });
        assert_eq!(labels(&mw, cx), vec!["a", "b", "Web"]);
        mw.read_with(cx, |mw, cx| assert_eq!(project_tabs(mw, cx)[2].color, Some(4)));
    }

    #[gpui::test]
    async fn empty_window_still_has_one_tab(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (mw, cx) = cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        assert_eq!(labels(&mw, cx), vec!["untitled"]);
        assert_eq!(active(&mw, cx), 0);
    }

    #[gpui::test]
    async fn move_tab_reorders_both_ways(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        mw.update(cx, |mw, cx| move_project_tab(mw, 0, 2, cx));
        assert_eq!(labels(&mw, cx), vec!["b", "c", "a"]);
        mw.update(cx, |mw, cx| move_project_tab(mw, 2, 0, cx));
        assert_eq!(labels(&mw, cx), vec!["a", "b", "c"]);
        mw.update(cx, |mw, cx| move_project_tab(mw, 1, 1, cx));
        mw.update(cx, |mw, cx| move_project_tab(mw, 7, 0, cx));
        assert_eq!(labels(&mw, cx), vec!["a", "b", "c"], "indici fuori range ignorati");
    }

    #[gpui::test]
    async fn cycle_tab_color_action(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        let ws = mw.read_with(cx, |mw, _| mw.workspace().clone());
        ws.update_in(cx, |ws, window, cx| {
            register_tab_actions(ws);
            window.focus(&ws.focus_handle(cx), cx);
        });
        // superzed: `register_tab_actions` aggiunge un `on_action` al render del
        // Workspace; senza un redraw esplicito la frame gia' dipinta (da cui
        // `dispatch_action` legge il nodo con il focus) non lo contiene ancora.
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.dispatch_action(superzed_actions::CycleProjectTabColor);
        cx.dispatch_action(superzed_actions::CycleProjectTabColor);
        mw.read_with(cx, |mw, cx| assert_eq!(project_tabs(mw, cx)[2].color, Some(1)));
    }

    // Verifica manuale 2026-09-26: "Colore successivo" dal menu su una tab non attiva
    // attivava quella tab e non cambiava colore (azione su una frame non ancora ridisegnata).
    #[gpui::test]
    async fn color_from_menu_on_inactive_tab_colors_that_tab(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        assert_eq!(active(&mw, cx), 2);
        cx.update(|_, cx| cycle_tab_color_at(&mw, 0, cx));
        mw.read_with(cx, |mw, cx| {
            let tabs = project_tabs(mw, cx);
            assert_eq!(tabs[0].color, Some(0));
            assert_eq!(tabs[2].color, None);
        });
        assert_eq!(active(&mw, cx), 2, "la tab attiva non cambia");
    }

    #[gpui::test]
    async fn next_color_skips_colors_of_other_tabs(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        cx.update(|_, cx| cycle_tab_color_at(&mw, 1, cx));
        cx.update(|_, cx| cycle_tab_color_at(&mw, 0, cx));
        cx.update(|_, cx| cycle_tab_color_at(&mw, 0, cx));
        let colors = |cx: &mut VisualTestContext| {
            mw.read_with(cx, |mw, cx| project_tabs(mw, cx).iter().map(|t| t.color).collect::<Vec<_>>())
        };
        assert_eq!(colors(cx), vec![Some(2), Some(0), None], "a salta lo 0 di b");
        // b cambia colore: lo 0 si libera e c lo prende
        cx.update(|_, cx| cycle_tab_color_at(&mw, 1, cx));
        cx.update(|_, cx| cycle_tab_color_at(&mw, 2, cx));
        assert_eq!(colors(cx), vec![Some(2), Some(1), Some(0)]);
    }

    #[gpui::test]
    async fn rename_from_menu_runs_on_the_clicked_tab(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        cx.update(|_, cx| {
            cx.set_global(RenameTabHook(|ws, _, cx| {
                ws.update_superzed_state(|s| s.tab_name = Some("X".into()), cx)
            }))
        });
        cx.update(|window, cx| rename_project_tab_at(&mw, 0, window, cx));
        cx.run_until_parked();
        assert_eq!(labels(&mw, cx), vec!["X", "b", "c"]);
        assert_eq!(active(&mw, cx), 0, "la rinomina attiva la tab (il prompt e' li')");
    }

    #[gpui::test]
    async fn close_tab_asks_and_y_removes_the_project(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        cx.update(|window, cx| close_project_tab_with_confirm(mw.clone(), 1, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("n");
        cx.run_until_parked();
        assert_eq!(labels(&mw, cx), vec!["a", "b", "c"]);
        cx.update(|window, cx| close_project_tab_with_confirm(mw.clone(), 1, window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert_eq!(labels(&mw, cx), vec!["a", "c"]);
    }

    // Review Focus 3
    #[gpui::test]
    async fn restore_opens_every_group_and_skips_missing_project(cx: &mut TestAppContext) {
        init_test(cx);
        let fs = FakeFs::new(cx.executor());
        for name in ["a", "c"] {
            fs.insert_tree(format!("/{name}"), json!({ "f.txt": "" })).await;
        }
        // superzed: `find_or_create_local_workspace` legge `<dyn Fs>::global` per i
        // gruppi non gia' aperti (vedi multi_workspace_tests, stessa necessita').
        cx.update(|cx| <dyn fs::Fs>::set_global(fs.clone(), cx));
        let pa = Project::test(fs.clone(), [path!("/a").as_ref()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(pa, window, cx));
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        let key = |p: &str| {
            crate::SerializedProjectGroupState {
                key: ProjectGroupKey::new(None, crate::PathList::new(&[std::path::PathBuf::from(p)])),
                expanded: true,
            }
        };
        window
            .update(cx, |mw, _, cx| {
                // ordine salvato: il primo gruppo e' la tab piu' a destra
                mw.restore_project_groups(vec![key(path!("/c")), key(path!("/b")), key(path!("/a"))], cx)
            })
            .unwrap();
        let fs_dyn: std::sync::Arc<dyn fs::Fs> = fs.clone();
        let mut async_cx = cx.to_async();
        open_restored_tabs(window, fs_dyn, &mut async_cx).await;
        cx.run_until_parked();
        let (labels, live) = window
            .read_with(cx, |mw, cx| {
                let tabs = project_tabs(mw, cx);
                (
                    tabs.iter().map(|t| t.label.to_string()).collect::<Vec<_>>(),
                    tabs.iter().all(|t| t.workspace.is_some()),
                )
            })
            .unwrap();
        assert_eq!(labels, vec!["a", "c"], "b non esiste piu' e viene tolto");
        assert!(live, "ogni tab ha un workspace vivo");
    }

    #[gpui::test]
    async fn tab_attention_comes_from_the_hook(cx: &mut TestAppContext) {
        let (mw, cx) = three_projects(cx).await;
        let ws = mw.read_with(cx, |mw, _| mw.workspace().clone());
        assert_eq!(cx.update(|_, cx| tab_attention(&ws, cx)), TabAttention::None, "senza hook");
        cx.update(|_, cx| cx.set_global(TabAttentionHook(|_, _| TabAttention::NeedsInput)));
        assert_eq!(cx.update(|_, cx| tab_attention(&ws, cx)), TabAttention::NeedsInput);
    }
}
