pub mod connection_form;
pub mod notifications_panel;
pub mod panel_ui;
pub mod search_panel;
pub mod sessions_panel;
pub mod sources_panel;

use git_ui::git_panel::GitPanel;
use gpui::{App, AppContext as _};
use notifications_panel::NotificationsPanel;
use project_panel::ProjectPanel;
use search_panel::SearchPanel;
use sessions_panel::SessionsPanel;
use sources_panel::SourcesPanel;
use superzed_actions::SidebarView;
use workspace::Workspace;

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        if let Some(window) = window {
            let weak = cx.entity().downgrade();
            let sessions = cx.new(|cx| SessionsPanel::new(weak.clone(), window, cx));
            workspace.add_panel(sessions, window, cx);
            let search = cx.new(|cx| SearchPanel::new(window, cx));
            workspace.add_panel(search, window, cx);
            let sources = cx.new(|cx| SourcesPanel::new(weak.clone(), window, cx));
            workspace.add_panel(sources, window, cx);
            let notifications = cx.new(|cx| NotificationsPanel::new(weak, cx));
            workspace.add_panel(notifications, window, cx);
        }
        workspace.register_action(|ws, action: &SidebarView, window, cx| match action.0 {
            1 => {
                ws.focus_panel::<ProjectPanel>(window, cx);
            }
            2 => {
                ws.focus_panel::<GitPanel>(window, cx);
            }
            3 => {
                ws.focus_panel::<SessionsPanel>(window, cx);
            }
            4 => {
                ws.focus_panel::<SearchPanel>(window, cx);
            }
            5 => {
                ws.focus_panel::<NotificationsPanel>(window, cx);
            }
            6 => {
                ws.focus_panel::<SourcesPanel>(window, cx);
            }
            _ => {}
        });
        // clic sui limiti della status bar: pannello Sessions con «Limits» aperta e quote
        // riaggiornate (`RefreshQuotas` usa la cache entro MANUAL_MIN_AGE)
        workspace.register_action(|ws, _: &superzed_actions::ShowAccountLimits, window, cx| {
            if let Some(panel) = ws.focus_panel::<SessionsPanel>(window, cx) {
                panel.update(cx, |p, cx| p.set_limits_open(true, cx));
            }
            window.dispatch_action(Box::new(superzed_accounts::RefreshQuotas), cx);
        });
    })
    .detach();

    // una volta per app: errori e critici avvisano (notifica di sistema o toast)
    if let Some(store) = superzed_notifications::NotificationStore::global(cx) {
        let bundled = std::env::current_exe().is_ok_and(|p| notifications_panel::is_app_bundle(&p));
        cx.subscribe(&store, move |store, event, cx| {
            if let superzed_notifications::NotificationEvent::Added(id) = event
                && let Some(n) = store.read(cx).get(id).cloned()
                && n.severity.alerts()
            {
                notifications_panel::alert(&n, bundled, cx);
            }
        })
        .detach();
    }
    // "Risolvi" sulla notifica di sistema: stesso comportamento del pulsante della card
    cx.on_system_notification_response(|response, cx| {
        if response.action_id.as_deref() != Some(notifications_panel::RESOLVE_ACTION) {
            return;
        }
        // l'azione di sistema non porta l'app in primo piano: la si attiva qui (la
        // finestra del progetto la attiva Risolvi)
        cx.activate(true);
        notifications_panel::resolve_in_any_window(&response.tag, None, cx);
    });
}

#[cfg(test)]
mod tests {
    use crate::{notifications_panel::NotificationsPanel, search_panel::SearchPanel, sessions_panel::SessionsPanel};
    use gpui::{Focusable, TestAppContext, VisualTestContext};
    use std::path::{Path, PathBuf};
    use serde_json::json;
    use workspace::{MultiWorkspace, Workspace};

    async fn setup(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, tempfile::TempDir, &'static mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            terminal_view::init(cx);
            superzed_canvas::init(cx);
            superzed_notifications::init_test(cx, dir.path().to_path_buf());
            crate::init(cx);
        });
        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree("/repo", json!({ "a.txt": "" })).await;
        let project = project::Project::test(fs, ["/repo".as_ref()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        cx.run_until_parked();
        (workspace, dir, cx)
    }

    #[gpui::test]
    async fn sidebar_view_actions_focus_their_panel(cx: &mut TestAppContext) {
        let (workspace, _dir, cx) = setup(cx).await;
        workspace.update_in(cx, |ws, window, cx| window.focus(&ws.focus_handle(cx), cx));
        cx.dispatch_action(superzed_actions::SidebarView(3));
        cx.run_until_parked();
        workspace.read_with(cx, |ws, cx| {
            let dock = ws.left_dock().read(cx);
            assert!(dock.is_open());
            assert_eq!(dock.active_panel_index(), dock.panel_index_for_type::<SessionsPanel>());
        });
        cx.dispatch_action(superzed_actions::SidebarView(4));
        cx.run_until_parked();
        workspace.read_with(cx, |ws, cx| {
            let dock = ws.left_dock().read(cx);
            assert_eq!(dock.active_panel_index(), dock.panel_index_for_type::<SearchPanel>());
        });
        cx.dispatch_action(superzed_actions::SidebarView(5));
        cx.run_until_parked();
        workspace.read_with(cx, |ws, cx| {
            let dock = ws.right_dock().read(cx);
            assert!(dock.is_open());
            assert_eq!(dock.active_panel_index(), dock.panel_index_for_type::<NotificationsPanel>());
        });
        cx.dispatch_action(superzed_actions::SidebarView(6));
        cx.run_until_parked();
        workspace.read_with(cx, |ws, cx| {
            let dock = ws.left_dock().read(cx);
            assert_eq!(dock.active_panel_index(), dock.panel_index_for_type::<crate::sources_panel::SourcesPanel>());
        });
    }

    #[gpui::test]
    async fn strip_shows_left_panels_and_switches_view(cx: &mut TestAppContext) {
        let (workspace, _dir, cx) = setup(cx).await;
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SessionsPanel>(window, cx));
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let search_ix = workspace
            .read_with(cx, |ws, cx| ws.left_dock().read(cx).panel_index_for_type::<SearchPanel>())
            .unwrap();
        let selector: &'static str = Box::leak(format!("superzed-sidebar-strip-{search_ix}").into_boxed_str());
        let button = cx.debug_bounds(selector).expect("icona della ricerca nella striscia");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        workspace.read_with(cx, |ws, cx| {
            assert_eq!(ws.left_dock().read(cx).active_panel_index(), Some(search_ix));
        });
    }

    #[gpui::test]
    async fn sessions_lists_terminal_nodes(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, _dir, cx) = setup(cx).await;
        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal(superzed_canvas::NodeKind::Shell, None, &serde_json::Value::Null, window, cx);
            c.new_note(window, cx);
        });
        cx.run_until_parked();
        let panel = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        panel.read_with(cx, |p, cx| {
            let rows = p.rows(cx);
            assert_eq!(rows.len(), 1, "solo i nodi terminale, non la nota");
            assert_eq!(rows[0].1, "shell");
        });
    }

    async fn two_projects(cx: &mut TestAppContext) -> (gpui::Entity<MultiWorkspace>, &'static mut VisualTestContext) {
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            terminal_view::init(cx);
            superzed_sessions::init_test(cx, std::env::temp_dir().join("superzed-no-claude"), None);
            superzed_canvas::init(cx);
            crate::init(cx);
        });
        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree("/a", json!({ "f.txt": "" })).await;
        fs.insert_tree("/b", json!({ "f.txt": "" })).await;
        let pa = project::Project::test(fs.clone(), ["/a".as_ref()], cx).await;
        let pb = project::Project::test(fs.clone(), ["/b".as_ref()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(pa, window, cx));
        let mw = window.root(cx).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        mw.update_in(cx, |mw, window, cx| {
            mw.test_add_workspace(pb, window, cx);
        });
        cx.run_until_parked();
        (mw, cx)
    }

    fn zsh(label: &str) -> task::SpawnInTerminal {
        task::SpawnInTerminal {
            label: label.to_string(),
            command: Some("/bin/zsh".to_string()),
            args: vec!["-f".to_string(), "-i".to_string()],
            use_new_terminal: true,
            allow_concurrent_runs: true,
            hide: task::HideStrategy::Always,
            ..Default::default()
        }
    }

    #[gpui::test]
    async fn sessions_are_grouped_by_project_with_session_info(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, cx) = two_projects(cx).await;
        let workspaces: Vec<_> = mw.read_with(cx, |mw, _| mw.workspaces().cloned().collect());
        for (i, ws) in workspaces.iter().enumerate() {
            let canvas = ws.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
            let init = superzed_canvas::NodeSessionInit {
                key: format!("k{i}"),
                account: "alice".into(),
                account_id: None,
                session_id: None,
                phase: superzed_sessions::Phase::New,
                times: Default::default(),
            };
            canvas.update_in(cx, |c, window, cx| {
                c.open_terminal_with_spec(superzed_canvas::NodeKind::Claude, None, zsh("claude"), Some(init), window, cx)
            });
        }
        cx.run_until_parked();
        let store = cx.update(|_, cx| superzed_sessions::SessionStore::global(cx).unwrap());
        store.update(cx, |s, cx| {
            s.apply(
                superzed_ipc::IpcMessage {
                    channel: superzed_ipc::Channel::Status,
                    node_key: "k0".into(),
                    payload: json!({"context_window": {"remaining_percentage": 70}}),
                },
                cx,
            )
        });
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let panel = active.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        panel.read_with(cx, |p, cx| {
            let groups = p.groups(cx);
            assert_eq!(groups.len(), 2);
            assert!(groups[0].is_active, "il progetto attivo per primo");
            assert!(groups.iter().all(|g| g.rows.len() == 1));
            let with_ctx = groups.iter().flat_map(|g| &g.rows).filter(|r| r.session.as_ref().and_then(|s| s.context_left) == Some(70)).count();
            assert_eq!(with_ctx, 1);
            assert!(groups.iter().flat_map(|g| &g.rows).all(|r| r.session.as_ref().unwrap().account == "alice"));
        });
        for ws in &workspaces {
            let canvas = ws.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
            canvas.update(cx, |c, cx| {
                let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
                c.remove_nodes(&ids, cx)
            });
        }
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn clicking_a_session_of_another_project_switches_tab_and_focuses_node(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, cx) = two_projects(cx).await;
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let other = mw.read_with(cx, |mw, _| mw.workspaces().find(|w| **w != active).cloned().unwrap());
        let canvas = other.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell"), None, window, cx)
        });
        cx.run_until_parked();
        let panel = active.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        let (tab_ix, node) = panel.read_with(cx, |p, cx| {
            let g = p.groups(cx).into_iter().find(|g| !g.is_active).expect("gruppo dell'altro progetto");
            (g.tab_ix, g.rows[0].node)
        });
        cx.update(|window, cx| crate::sessions_panel::focus_session(&mw, tab_ix, other.clone(), node, window, cx));
        cx.run_until_parked();
        assert_eq!(mw.read_with(cx, |mw, _| mw.workspace().clone()), other, "tab cambiata");
        let focused = canvas.read_with(cx, |c, cx| c.node_focus_handle(node, cx).unwrap());
        cx.update(|window, _| assert!(focused.is_focused(window), "focus sul nodo"));
        canvas.update(cx, |c, cx| c.remove_nodes(&[node], cx));
        cx.run_until_parked();
    }

    // Final fix 5, punto 2: l'attivazione e' per entita' workspace, non per indice di
    // tab dell'ultimo render (che puo' non corrispondere piu' a niente).
    #[gpui::test]
    async fn focus_session_activates_by_workspace_even_with_a_stale_tab_index(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, cx) = two_projects(cx).await;
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let other = mw.read_with(cx, |mw, _| mw.workspaces().find(|w| **w != active).cloned().unwrap());
        let canvas = other.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell"), None, window, cx)
        });
        cx.run_until_parked();
        let panel = active.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        let node = panel.read_with(cx, |p, cx| {
            let g = p.groups(cx).into_iter().find(|g| !g.is_active).expect("gruppo dell'altro progetto");
            g.rows[0].node
        });
        // indice fuori range: se l'attivazione lo usasse ancora, non farebbe niente
        let stale_tab_ix = 99;
        cx.update(|window, cx| crate::sessions_panel::focus_session(&mw, stale_tab_ix, other.clone(), node, window, cx));
        cx.run_until_parked();
        assert_eq!(mw.read_with(cx, |mw, _| mw.workspace().clone()), other, "tab cambiata anche con indice stale");
        let focused = canvas.read_with(cx, |c, cx| c.node_focus_handle(node, cx).unwrap());
        cx.update(|window, _| assert!(focused.is_focused(window), "focus sul nodo"));
        canvas.update(cx, |c, cx| c.remove_nodes(&[node], cx));
        cx.run_until_parked();
    }

    // Final fix 6, punto 3: se prima del ritocco rimandato l'utente e' gia' passato a
    // un'altra tab, il focus e lo zoom non la riportano indietro ne' toccano il canvas.
    #[gpui::test]
    async fn focus_session_skips_the_touch_if_the_tab_changed_meanwhile(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, cx) = two_projects(cx).await;
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let other = mw.read_with(cx, |mw, _| mw.workspaces().find(|w| **w != active).cloned().unwrap());
        let canvas = other.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell1"), None, window, cx);
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell2"), None, window, cx);
        });
        cx.run_until_parked();
        let panel = active.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        let (tab_ix, node) = panel.read_with(cx, |p, cx| {
            let g = p.groups(cx).into_iter().find(|g| !g.is_active).expect("gruppo dell'altro progetto");
            (g.tab_ix, g.rows.last().expect("almeno un nodo").node)
        });
        let before = canvas.read_with(cx, |c, _| c.viewport());
        cx.update(|window, cx| {
            crate::sessions_panel::focus_session(&mw, tab_ix, other.clone(), node, window, cx);
            // di nuovo la tab di partenza, prima che il ritocco rimandato giri
            let back = active.clone();
            mw.update(cx, |mw, cx| mw.activate(back, None, window, cx));
        });
        cx.run_until_parked();
        assert_eq!(mw.read_with(cx, |mw, _| mw.workspace().clone()), active, "resta la tab scelta dopo");
        let focused = canvas.read_with(cx, |c, cx| c.node_focus_handle(node, cx).unwrap());
        cx.update(|window, _| assert!(!focused.is_focused(window), "niente focus sul nodo dell'altra tab"));
        assert_eq!(canvas.read_with(cx, |c, _| c.viewport()), before, "niente zoom");
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn close_session_activates_by_workspace_even_with_a_stale_tab_index(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, cx) = two_projects(cx).await;
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let other = mw.read_with(cx, |mw, _| mw.workspaces().find(|w| **w != active).cloned().unwrap());
        let canvas = other.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell"), None, window, cx)
        });
        cx.run_until_parked();
        let panel = active.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        let node = panel.read_with(cx, |p, cx| {
            let g = p.groups(cx).into_iter().find(|g| !g.is_active).expect("gruppo dell'altro progetto");
            g.rows[0].node
        });
        let stale_tab_ix = 99;
        cx.update(|window, cx| crate::sessions_panel::close_session(&mw, stale_tab_ix, other.clone(), node, window, cx));
        cx.run_until_parked();
        // la tab dev'essere gia' cambiata prima della conferma (il modale sta nel
        // workspace del canvas): con l'indice stale la vecchia implementazione non
        // attivava niente
        assert_eq!(mw.read_with(cx, |mw, _| mw.workspace().clone()), other, "tab cambiata anche con indice stale");
        canvas.update(cx, |c, cx| c.remove_nodes(&[node], cx));
        cx.run_until_parked();
    }

    // Zoom atteso ricalcolato in modo indipendente da `zoom_to_node` (stessa formula
    // `fit_rect` usata dal doppio click sul titolo), dai limiti reali del canvas
    // disegnato e dal frame del nodo: non richiama la produzione, cosi' il confronto
    // non e' tautologico.
    fn expected_zoom_to_node(
        canvas: &gpui::Entity<superzed_canvas::CanvasView>,
        node: superzed_canvas::NodeId,
        cx: &mut VisualTestContext,
    ) -> superzed_canvas::viewport::Viewport {
        let bounds = cx.debug_bounds("superzed-canvas").expect("il canvas deve essere disegnato");
        let view = gpui::size(f32::from(bounds.size.width), f32::from(bounds.size.height));
        let (origin, size, min_zoom) = canvas.read_with(cx, |c, _| {
            let f = c.model().frame(node).expect("il nodo esiste");
            (f.origin, f.size, c.min_zoom())
        });
        superzed_canvas::viewport::fit_rect(origin, size, view, superzed_canvas::viewport::FIT_MARGIN, 1.0, min_zoom)
            .expect("il rettangolo deve entrarci")
    }

    // Task 17: clic su una riga del pannello Sessions -> zoom sul nodo, stessa logica
    // del doppio click sul titolo (`zoom_to_node`, zoom minimo dinamico incluso).
    #[gpui::test]
    async fn clicking_a_session_zooms_to_its_node(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, _dir, cx) = setup(cx).await;
        let mw = workspace.read_with(cx, |ws, _| ws.multi_workspace().unwrap().upgrade().unwrap());
        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal(superzed_canvas::NodeKind::Shell, None, &serde_json::Value::Null, window, cx);
        });
        cx.run_until_parked();
        let node = canvas.read_with(cx, |c, cx| c.terminal_nodes(cx)[0].0);
        // si allontana la vista dal nodo prima del clic
        canvas.update(cx, |c, cx| c.zoom_to_rect(gpui::point(20_000., 20_000.), gpui::size(50., 50.), cx));
        cx.run_until_parked();
        let before = canvas.read_with(cx, |c, _| c.viewport());
        let panel = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        let (tab_ix, row_ws) = panel.read_with(cx, |p, cx| {
            let g = p.groups(cx).into_iter().next().expect("gruppo del progetto");
            (g.tab_ix, g.workspace)
        });
        cx.update(|window, cx| crate::sessions_panel::focus_session(&mw, tab_ix, row_ws, node, window, cx));
        cx.run_until_parked();
        let after = canvas.read_with(cx, |c, _| c.viewport());
        assert_ne!(after, before, "la vista si e' spostata sul nodo");
        let expected = expected_zoom_to_node(&canvas, node, cx);
        assert_eq!(after, expected, "stesso zoom del doppio click sul titolo (fit_rect)");
        let focused = canvas.read_with(cx, |c, cx| c.node_focus_handle(node, cx).unwrap());
        cx.update(|window, _| assert!(focused.is_focused(window), "focus sul terminale"));
    }

    // Task 17, caso multi-progetto: il canvas dell'altro progetto non e' mai stato
    // disegnato prima dell'attivazione della tab (niente limiti finche' non lo e'): lo
    // zoom deve avvenire dopo, quando il canvas ha i suoi limiti.
    #[gpui::test]
    async fn clicking_a_session_of_another_project_zooms_to_its_node_after_the_tab_activates(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, cx) = two_projects(cx).await;
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let other = mw.read_with(cx, |mw, _| mw.workspaces().find(|w| **w != active).cloned().unwrap());
        let canvas = other.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        // due nodi: il canvas non e' mai stato disegnato, quindi il primo nasce al
        // centro (0,0) per costruzione (`local_center` a canvas non disegnato) e uno
        // zoom-to-node su di lui coinciderebbe per caso con la viewport di default. Il
        // secondo viene incolonnato altrove da `placement_near`: cliccando su di lui lo
        // zoom atteso non puo' coincidere per caso con quello di partenza.
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell1"), None, window, cx);
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell2"), None, window, cx);
        });
        cx.run_until_parked();
        let panel = active.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        let (tab_ix, node) = panel.read_with(cx, |p, cx| {
            let g = p.groups(cx).into_iter().find(|g| !g.is_active).expect("gruppo dell'altro progetto");
            (g.tab_ix, g.rows.last().expect("almeno un nodo").node)
        });
        cx.update(|window, cx| crate::sessions_panel::focus_session(&mw, tab_ix, other.clone(), node, window, cx));
        cx.run_until_parked();
        assert_eq!(mw.read_with(cx, |mw, _| mw.workspace().clone()), other, "tab cambiata");
        let focused = canvas.read_with(cx, |c, cx| c.node_focus_handle(node, cx).unwrap());
        cx.update(|window, _| assert!(focused.is_focused(window), "focus sul nodo"));
        let actual = canvas.read_with(cx, |c, _| c.viewport());
        let expected = expected_zoom_to_node(&canvas, node, cx);
        assert_eq!(actual, expected, "stesso zoom del doppio click sul titolo, anche cambiando progetto");
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    #[test]
    fn deploy_action_carries_query_and_options() {
        let a = crate::search_panel::deploy_action("  todo ", true, false, true).unwrap();
        assert_eq!(a.query.as_deref(), Some("todo"));
        assert_eq!((a.case_sensitive, a.whole_word, a.regex), (Some(true), Some(false), Some(true)));
        assert!(crate::search_panel::deploy_action("   ", false, false, false).is_none());
    }

    async fn notifications_setup(cx: &mut TestAppContext) -> (gpui::Entity<MultiWorkspace>, tempfile::TempDir, &'static mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        cx.update(|cx| {
            superzed_notifications::init_test(cx, path);
            cx.set_global(superzed_canvas::ClaudePtyOverride(zsh("claude")));
            // la piattaforma di test consegna le notifiche di sistema solo con un'identita'
            cx.set_app_identity("dev.superzed.test", "superzed");
        });
        let (mw, cx) = two_projects(cx).await;
        // `two_projects` lascia attivo /b: si parte da /a, come il progetto "corrente"
        mw.update_in(cx, |mw, window, cx| workspace::superzed_tabs::activate_project_tab(mw, 0, window, cx));
        cx.run_until_parked();
        assert_eq!(
            mw.read_with(cx, |mw, cx| superzed_canvas::canvas_for(mw.workspace().read(cx)).unwrap().read(cx).project_root_path(cx)),
            Some("/a".into())
        );
        (mw, dir, cx)
    }

    fn notif(title: &str, severity: superzed_notifications::Severity, project: &str) -> superzed_notifications::NewNotification {
        superzed_notifications::NewNotification {
            severity,
            title: title.into(),
            summary: "riassunto".into(),
            details: Some("trace".into()),
            suggested_prompt: Some("guarda il pool".into()),
            project_root: Some(project.into()),
            source: "Log".into(),
        }
    }

    fn notifications(cx: &mut VisualTestContext) -> gpui::Entity<superzed_notifications::NotificationStore> {
        cx.update(|_, cx| superzed_notifications::NotificationStore::global(cx).unwrap())
    }

    #[gpui::test]
    async fn cards_are_sorted_colored_and_filtered_by_project(cx: &mut TestAppContext) {
        use superzed_notifications::Severity::*;
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let t = |h| { use chrono::TimeZone as _; chrono::Local.with_ymd_and_hms(2026, 9, 27, h, 0, 0).unwrap() };
        let store = notifications(cx);
        store.update(cx, |s, cx| {
            s.add_at(notif("info", Info, "/a"), t(12), cx);
            s.add_at(notif("err-old", Error, "/a"), t(9), cx);
            s.add_at(notif("crit", Critical, "/a"), t(8), cx);
            s.add_at(notif("err-new", Error, "/a"), t(11), cx);
            s.add_at(notif("altrove", Critical, "/b"), t(10), cx);
        });
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let panel = active.read_with(cx, |ws, cx| ws.panel::<crate::notifications_panel::NotificationsPanel>(cx).unwrap());
        let titles = |cx: &mut VisualTestContext| panel.read_with(cx, |p, cx| p.cards(cx).into_iter().map(|n| n.title).collect::<Vec<_>>());
        assert_eq!(titles(cx), ["crit", "err-new", "err-old", "info"], "solo questo progetto, per gravita' poi recenti");
        panel.update(cx, |p, cx| p.set_all_projects(true, cx));
        assert_eq!(titles(cx)[0..2], ["altrove", "crit"], "entrambe critiche: prima la piu' recente (10:00 contro 8:00)");
        // colori dal tema: accento d'errore per Critical (con il suo sfondo), nessuno per Note
        cx.update(|_, cx| {
            use ui::ActiveTheme as _;
            let (status, colors) = (cx.theme().status(), cx.theme().colors());
            let crit = crate::notifications_panel::card_style(Critical, cx);
            assert_eq!((crit.accent, crit.background), (Some(status.error), status.error_background));
            let note = crate::notifications_panel::card_style(Note, cx);
            assert_eq!((note.accent, note.background, note.icon), (None, colors.elevated_surface_background, None));
        });
        // la card e' disegnata con il suo selettore
        active.update_in(cx, |ws, window, cx| ws.focus_panel::<crate::notifications_panel::NotificationsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let crit = panel.read_with(cx, |p, cx| p.cards(cx)[0].id.clone());
        let selector: &'static str = Box::leak(format!("superzed-notification-{crit}").into_boxed_str());
        assert!(cx.debug_bounds(selector).is_some());
    }

    #[test]
    fn notifications_empty_text() {
        use crate::notifications_panel::empty_text;
        assert_eq!(empty_text(false, 0), "No notifications");
        assert_eq!(empty_text(false, 3), "No notifications in this project (3 in others)");
        assert_eq!(empty_text(true, 0), "No notifications");
    }

    #[gpui::test]
    async fn an_empty_project_filter_says_how_many_are_elsewhere(cx: &mut TestAppContext) {
        use superzed_notifications::Severity::*;
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let store = notifications(cx);
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let panel = active.read_with(cx, |ws, cx| ws.panel::<crate::notifications_panel::NotificationsPanel>(cx).unwrap());
        let message = |cx: &mut VisualTestContext| panel.read_with(cx, |p, cx| p.empty_message(cx));
        assert_eq!(message(cx).as_deref(), Some("No notifications"));
        store.update(cx, |s, cx| {
            s.add(notif("b1", Error, "/b"), cx);
            s.add(notif("b2", Info, "/b"), cx);
        });
        assert_eq!(message(cx).as_deref(), Some("No notifications in this project (2 in others)"));
        panel.update(cx, |p, cx| p.set_all_projects(true, cx));
        assert_eq!(message(cx), None, "con tutti i progetti le card ci sono");
    }

    #[gpui::test]
    async fn sessions_panel_shows_an_empty_state(cx: &mut TestAppContext) {
        let (workspace, _dir, cx) = setup(cx).await;
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SessionsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds("superzed-sessions-empty").is_some(), "«Nessuna chat aperta»");
        assert!(cx.debug_bounds("superzed-sessions-list").is_some(), "lista scorrevole");
    }

    /// Progetto con due shell rinominate «alpha» e «beta», pannello Sessioni visibile.
    async fn two_shells(
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<Workspace>,
        gpui::Entity<superzed_canvas::CanvasView>,
        [superzed_canvas::NodeId; 2],
        tempfile::TempDir,
        &'static mut VisualTestContext,
    ) {
        cx.executor().allow_parking();
        let (workspace, dir, cx) = setup(cx).await;
        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        for _ in 0..2 {
            canvas.update_in(cx, |c, window, cx| {
                c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell"), None, window, cx)
            });
            cx.run_until_parked();
        }
        let nodes = canvas.update(cx, |c, cx| {
            let n = c.terminal_nodes(cx);
            assert_eq!(n.len(), 2);
            c.set_node_title(n[0].0, "alpha".into(), cx);
            c.set_node_title(n[1].0, "beta".into(), cx);
            [n[0].0, n[1].0]
        });
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SessionsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (workspace, canvas, nodes, dir, cx)
    }

    fn row_selector(node: superzed_canvas::NodeId) -> &'static str {
        Box::leak(format!("superzed-session-0-{}", node.0).into_boxed_str())
    }

    #[gpui::test]
    async fn the_sessions_panel_has_a_header_with_the_total(cx: &mut TestAppContext) {
        let (workspace, canvas, nodes, _dir, cx) = two_shells(cx).await;
        let panel = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        assert_eq!(panel.read_with(cx, |p, cx| p.total(cx)), 2);
        assert!(cx.debug_bounds("superzed-sessions-header").is_some(), "intestazione «Sessions»");
        assert!(cx.debug_bounds("superzed-sessions-filter").is_some(), "campo filtro");
        assert!(cx.debug_bounds("superzed-sessions-group-0").is_some(), "intestazione del gruppo");
        canvas.update(cx, |c, cx| c.remove_nodes(&nodes, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn the_filter_hides_the_rows_that_do_not_match(cx: &mut TestAppContext) {
        let (workspace, canvas, [alpha, beta], _dir, cx) = two_shells(cx).await;
        let panel = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        assert!(cx.debug_bounds(row_selector(alpha)).is_some());
        assert!(cx.debug_bounds(row_selector(beta)).is_some());
        let editor = panel.read_with(cx, |p, _| p.filter_editor().clone());
        editor.update_in(cx, |e, window, cx| e.set_text("ALP", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds(row_selector(alpha)).is_some(), "alpha corrisponde");
        assert!(cx.debug_bounds(row_selector(beta)).is_none(), "beta nascosta");
        editor.update_in(cx, |e, window, cx| e.set_text("zzz", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds(row_selector(alpha)).is_none());
        assert!(cx.debug_bounds("superzed-sessions-group-0").is_none(), "gruppo senza risultati nascosto");
        assert!(cx.debug_bounds("superzed-sessions-no-match").is_some(), "«No sessions match»");
        canvas.update(cx, |c, cx| c.remove_nodes(&[alpha, beta], cx));
        cx.run_until_parked();
    }

    // final8 #4: stessa regola del pannello Sources: un gruppo collassato a mano non
    // nasconde le righe che combaciano col filtro.
    #[gpui::test]
    async fn a_collapsed_session_group_still_shows_matching_rows_while_filtering(cx: &mut TestAppContext) {
        let (workspace, canvas, [alpha, beta], _dir, cx) = two_shells(cx).await;
        let panel = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        let group = cx.debug_bounds("superzed-sessions-group-0").expect("intestazione del gruppo");
        cx.simulate_click(group.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds(row_selector(alpha)).is_none(), "gruppo collassato: righe nascoste");
        let editor = panel.read_with(cx, |p, _| p.filter_editor().clone());
        editor.update_in(cx, |e, window, cx| e.set_text("ALP", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds(row_selector(alpha)).is_some(), "con un filtro attivo il gruppo si mostra espanso anche se collassato");
        assert!(cx.debug_bounds(row_selector(beta)).is_none(), "beta non combacia col filtro");
        editor.update_in(cx, |e, window, cx| e.set_text("", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds(row_selector(alpha)).is_none(), "senza filtro il gruppo torna chiuso: lo stato non era stato toccato");
        canvas.update(cx, |c, cx| c.remove_nodes(&[alpha, beta], cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn the_close_button_asks_and_closes_the_node(cx: &mut TestAppContext) {
        let (_workspace, canvas, [alpha, beta], _dir, cx) = two_shells(cx).await;
        let close: &'static str = Box::leak(format!("superzed-session-close-0-{}", alpha.0).into_boxed_str());
        assert!(cx.debug_bounds(close).is_none(), "la ✕ compare solo al passaggio");
        let row = cx.debug_bounds(row_selector(alpha)).unwrap();
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        cx.run_until_parked();
        let button = cx.debug_bounds(close).expect("✕ al passaggio");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(canvas.read_with(cx, |c, _| c.model().frame(alpha).is_some()), "prima la conferma");
        let ok = cx.debug_bounds("superzed-confirm-ok").expect("conferma della chiusura");
        cx.simulate_click(ok.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(canvas.read_with(cx, |c, _| c.model().frame(alpha).is_none()), "nodo chiuso");
        assert!(canvas.read_with(cx, |c, _| c.model().frame(beta).is_some()), "l'altro resta");
        canvas.update(cx, |c, cx| c.remove_nodes(&[beta], cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn archive_hides_the_card_until_show_archived(cx: &mut TestAppContext) {
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("x", superzed_notifications::Severity::Warning, "/a"), cx));
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        active.update_in(cx, |ws, window, cx| ws.focus_panel::<crate::notifications_panel::NotificationsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let selector: &'static str = Box::leak(format!("superzed-notification-archive-{id}").into_boxed_str());
        let button = cx.debug_bounds(selector).expect("pulsante Archivia");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let panel = active.read_with(cx, |ws, cx| ws.panel::<crate::notifications_panel::NotificationsPanel>(cx).unwrap());
        assert!(panel.read_with(cx, |p, cx| p.cards(cx).is_empty()));
        panel.update(cx, |p, cx| p.set_show_archived(true, cx));
        assert_eq!(panel.read_with(cx, |p, cx| p.cards(cx).len()), 1);
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::Archived
        );
    }

    #[gpui::test]
    async fn resolve_opens_a_chat_in_the_kanban_of_the_notification_project(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("DB giu'", superzed_notifications::Severity::Error, "/b"), cx));
        cx.update(|window, cx| crate::notifications_panel::resolve_notification(&mw, &id, window, cx));
        cx.run_until_parked();
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let canvas = active.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        assert_eq!(canvas.read_with(cx, |c, cx| c.project_root_path(cx)), Some("/b".into()), "tab del progetto della notifica");
        let docked = canvas.read_with(cx, |c, cx| c.docked_chats(cx));
        assert_eq!(docked.len(), 1);
        assert_eq!(docked[0].1, superzed_sessions::Phase::New);
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains("Risolvi questo problema segnalato da «Log» (errore): DB giu'"), "{cmd}");
        assert!(cmd.contains("guarda il pool"));
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::Taken
        );
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn resolve_for_a_closed_project_shows_a_toast(cx: &mut TestAppContext) {
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("x", superzed_notifications::Severity::Error, "/chiuso"), cx));
        cx.update(|window, cx| crate::notifications_panel::resolve_notification(&mw, &id, window, cx));
        cx.run_until_parked();
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        active.read_with(cx, |ws, _| {
            assert!(ws.has_notification(&workspace::notifications::NotificationId::named("superzed-notification-not-open".into())))
        });
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::New,
            "resta nuova"
        );
    }

    #[gpui::test]
    async fn a_notification_action_replaces_resolve_and_opens_the_review(cx: &mut TestAppContext) {
        use superzed_done::test_support::{REPORT_JSON, SID, write_pm};
        cx.executor().allow_parking();
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let root = tempfile::tempdir().unwrap();
        write_pm(root.path());
        superzed_done::write_report(root.path(), &superzed_done::parse_report(REPORT_JSON, SID).unwrap()).unwrap();
        let gh = superzed_sources::test_support::FakeRunner::new();
        cx.update(|_, cx| superzed_done::init_test(cx, gh.clone(), superzed_sources::test_support::FakeRunner::new(), Some(root.path().to_path_buf())));
        let action = superzed_notifications::NotificationAction { kind: superzed_done::REVIEW_ACTION.into(), label: "Rivedi".into(), arg: SID.into() };
        let id = notifications(cx).update(cx, |s, cx| s.add_with_action(notif("Report pronto: x", superzed_notifications::Severity::Info, "/a"), action, cx));
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        active.update_in(cx, |ws, window, cx| ws.focus_panel::<crate::notifications_panel::NotificationsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let resolve: &'static str = Box::leak(format!("superzed-notification-resolve-{id}").into_boxed_str());
        assert!(cx.debug_bounds(resolve).is_none(), "l'azione prende il posto di Risolvi");
        let selector: &'static str = Box::leak(format!("superzed-notification-action-{id}").into_boxed_str());
        let button = cx.debug_bounds(selector).expect("pulsante Rivedi");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let is_open = |cx: &mut VisualTestContext| {
            let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
            active.read_with(cx, |ws, cx| ws.active_modal::<superzed_canvas::done_review::DoneReviewModal>(cx).is_some())
        };
        assert!(is_open(cx), "anteprima nel progetto della notifica");
        cx.dispatch_action(menu::Cancel);
        cx.run_until_parked();
        assert!(!is_open(cx));
        // «Risolvi» del toast o della notifica di sistema: stessa azione, la notifica resta nuova
        cx.update(|window, cx| crate::notifications_panel::resolve_notification(&mw, &id, window, cx));
        cx.run_until_parked();
        assert!(is_open(cx));
        assert_eq!(notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state), superzed_notifications::NotificationState::New);
        assert!(gh.calls().is_empty(), "aprire l'anteprima non chiama gh");
    }

    #[gpui::test]
    async fn errors_raise_a_system_notification_when_bundled_and_a_toast_otherwise(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("DB giu'", superzed_notifications::Severity::Critical, "/b"), cx));
        let n = notifications(cx).read_with(cx, |s, _| s.get(&id).cloned().unwrap());
        cx.update(|_, cx| crate::notifications_panel::alert(&n, true, cx));
        let shown = cx.shown_system_notifications();
        let last = shown.last().expect("notifica di sistema");
        assert_eq!(last.title.as_ref(), "superzed · b");
        assert_eq!(last.body.as_ref(), "DB giu'");
        assert_eq!(last.tag.as_ref(), id.as_str());
        assert_eq!(last.actions[0].id.as_ref(), crate::notifications_panel::RESOLVE_ACTION);
        cx.update(|_, cx| crate::notifications_panel::alert(&n, false, cx));
        cx.run_until_parked();
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        active.read_with(cx, |ws, _| {
            assert!(ws.has_notification(&workspace::notifications::NotificationId::named(format!("superzed-notification-{id}").into())))
        });
        // "Risolvi" sulla notifica di sistema: stesso comportamento del pulsante
        cx.simulate_system_notification_response(gpui::SystemNotificationResponse {
            tag: id.clone().into(),
            action_id: Some(crate::notifications_panel::RESOLVE_ACTION.into()),
        });
        cx.run_until_parked();
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::Taken
        );
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let canvas = active.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn clicking_resolve_on_a_card_opens_the_chat_and_marks_it_taken(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("lento", superzed_notifications::Severity::Warning, "/a"), cx));
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        active.update_in(cx, |ws, window, cx| ws.focus_panel::<crate::notifications_panel::NotificationsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let selector: &'static str = Box::leak(format!("superzed-notification-resolve-{id}").into_boxed_str());
        let button = cx.debug_bounds(selector).expect("pulsante Risolvi");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let canvas = active.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx)).len(), 1);
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains("(avviso): lento"), "{cmd}");
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::Taken
        );
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn resolve_finds_the_project_in_another_window(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree("/c", json!({ "f.txt": "" })).await;
        let pc = project::Project::test(fs, ["/c".as_ref()], cx).await;
        let other = cx.add_window(|window, cx| MultiWorkspace::test_new(pc, window, cx));
        cx.run_until_parked();
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("altra finestra", superzed_notifications::Severity::Info, "/c"), cx));
        cx.update(|window, cx| crate::notifications_panel::resolve_notification(&mw, &id, window, cx));
        cx.run_until_parked();
        let other_ws = other.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let canvas = other_ws.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        assert_eq!(canvas.read_with(cx, |c, cx| c.docked_chats(cx)).len(), 1, "chat nel kanban della finestra di /c");
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        active.read_with(cx, |ws, _| {
            assert!(!ws.has_notification(&workspace::notifications::NotificationId::named("superzed-notification-not-open".into())))
        });
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::Taken
        );
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    // Revisione finale (5): "in lavorazione" solo quando compare la chat di quella
    // notifica, non un nodo terminale qualunque aperto nel frattempo.
    #[gpui::test]
    async fn an_unrelated_new_terminal_does_not_mark_the_notification_taken(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("x", superzed_notifications::Severity::Error, "/a"), cx));
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let canvas = active.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        cx.update(|_, cx| crate::notifications_panel::mark_taken_when_created(canvas.clone(), "chiave-della-chat".into(), id.clone(), cx));
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell"), None, window, cx)
        });
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, cx| c.terminal_nodes(cx).len()), 1);
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::New,
            "un altro nodo non conta"
        );
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    // Revisione finale (5): Risolvi su una card gia' in lavorazione porta alla sua chat
    // se c'e' ancora; se e' stata chiusa ne apre una nuova.
    #[gpui::test]
    async fn resolve_on_a_taken_card_focuses_its_chat_or_opens_a_new_one(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (mw, _dir, cx) = notifications_setup(cx).await;
        let id = notifications(cx).update(cx, |s, cx| s.add(notif("lento", superzed_notifications::Severity::Warning, "/a"), cx));
        cx.update(|window, cx| crate::notifications_panel::resolve_notification(&mw, &id, window, cx));
        cx.run_until_parked();
        let active = mw.read_with(cx, |mw, _| mw.workspace().clone());
        let canvas = active.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        let first: Vec<_> = canvas.read_with(cx, |c, cx| c.terminal_nodes(cx).into_iter().map(|(n, _)| n).collect());
        assert_eq!(first.len(), 1);
        assert_eq!(
            notifications(cx).read_with(cx, |s, _| s.get(&id).unwrap().state),
            superzed_notifications::NotificationState::Taken
        );
        // il focus altrove, poi di nuovo Risolvi: nessuna chat nuova, focus sulla sua
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Shell, None, zsh("shell"), None, window, cx)
        });
        cx.run_until_parked();
        let shell = canvas.read_with(cx, |c, cx| c.terminal_nodes(cx).into_iter().map(|(n, _)| n).find(|n| !first.contains(n)).unwrap());
        canvas.update_in(cx, |c, window, cx| c.focus_node(shell, window, cx));
        cx.update(|window, cx| crate::notifications_panel::resolve_notification(&mw, &id, window, cx));
        cx.run_until_parked();
        assert_eq!(canvas.read_with(cx, |c, cx| c.terminal_nodes(cx).len()), 2, "nessuna chat nuova");
        let focused = canvas.read_with(cx, |c, cx| c.node_focus_handle(first[0], cx).unwrap());
        cx.update(|window, _| assert!(focused.is_focused(window), "focus sulla chat della notifica"));
        // chat chiusa: Risolvi ne apre una nuova
        canvas.update(cx, |c, cx| c.remove_nodes(&first, cx));
        cx.run_until_parked();
        cx.update(|window, cx| crate::notifications_panel::resolve_notification(&mw, &id, window, cx));
        cx.run_until_parked();
        let docked = canvas.read_with(cx, |c, cx| c.docked_chats(cx));
        assert_eq!(docked.len(), 1, "chat nuova nel kanban");
        assert!(!first.contains(&docked[0].0));
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    use crate::connection_form::ConnectionFormModal;
    use crate::sources_panel::SourcesPanel;
    use std::rc::Rc;
    use superzed_sources::{
        ClickhouseConfig, ClickhouseTransport, Connection, ConnectionConfig, ConnectionKind, ConnectionStore, Forward, GithubConfig,
        HyperdxConfig, MemorySecrets, TunnelConfig, TunnelState,
    };

    /// Connessioni finte e tunnel che fallisce subito (`boom` su stderr), prima del workspace.
    async fn sources_setup(
        cx: &mut TestAppContext,
        file: Option<&str>,
    ) -> (gpui::Entity<Workspace>, gpui::Entity<ConnectionStore>, Rc<MemorySecrets>, (tempfile::TempDir, tempfile::TempDir), &'static mut VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        if let Some(text) = file {
            std::fs::write(&path, text).unwrap();
        }
        let secrets = Rc::new(MemorySecrets::default());
        let deps = superzed_sources::test_support::deps_with(
            superzed_sources::test_support::FakeRunner::new(),
            http_client::FakeHttpClient::with_404_response(),
            secrets.clone(),
            superzed_sources::test_support::FixedTunnelCommand::new("/bin/sh", &["-c", "echo boom >&2; exit 1"]),
        );
        let store = cx.update(|cx| superzed_sources::init_test(cx, path, deps));
        let (workspace, notifications_dir, cx) = setup(cx).await;
        (workspace, store, secrets, (dir, notifications_dir), cx)
    }

    fn github(id: &str) -> Connection {
        Connection { id: id.into(), name: "acme-app".into(), config: ConnectionConfig::Github(GithubConfig { owner: Some("acme".into()), project_number: Some(3), repo: Some("acme/acme_app".into()), stale_days: None }) }
    }

    fn tunnel(id: &str) -> Connection {
        Connection { id: id.into(), name: "box".into(), config: ConnectionConfig::SshTunnel(TunnelConfig { host: "root@203.0.113.10".into(), forwards: vec![Forward { local_port: 0, remote_host: "localhost".into(), remote_port: 8123 }], keep_open: false, idle_minutes: 10 }) }
    }

    fn clickhouse_ssh(id: &str) -> Connection {
        Connection { id: id.into(), name: "ch".into(), config: ConnectionConfig::Clickhouse(ClickhouseConfig { transport: ClickhouseTransport::SshExec { host: "root@203.0.113.10".into(), command: None }, user: None, password_ref: None, database: "default".into() }) }
    }

    fn hyperdx(id: &str, ch: &str) -> Connection {
        Connection { id: id.into(), name: "log app".into(), config: ConnectionConfig::Hyperdx(HyperdxConfig { clickhouse: ch.into(), ui_url: None, namespace: Some("acme-app".into()), environment: Some("prod".into()) }) }
    }

    fn sources_panel(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) -> gpui::Entity<SourcesPanel> {
        workspace.read_with(cx, |ws, cx| ws.panel::<SourcesPanel>(cx).unwrap())
    }

    fn show_sources(workspace: &gpui::Entity<Workspace>, cx: &mut VisualTestContext) {
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SourcesPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn sources_view_lists_connections_and_tunnel_states(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(github("gh"), vec![], cx).unwrap();
            s.upsert(tunnel("t"), vec![], cx).unwrap();
            s.upsert(clickhouse_ssh("ch"), vec![], cx).unwrap();
            s.upsert(hyperdx("hx", "ch"), vec![], cx).unwrap();
        });
        let panel = sources_panel(&workspace, cx);
        let rows = panel.read_with(cx, |p, cx| p.rows(cx));
        assert_eq!(
            rows.iter().map(|r| (r.id.as_str(), r.kind, r.used)).collect::<Vec<_>>(),
            [("gh", ConnectionKind::Github, Some(false)), ("t", ConnectionKind::SshTunnel, None), ("ch", ConnectionKind::Clickhouse, None), ("hx", ConnectionKind::Hyperdx, Some(false))]
        );
        assert_eq!(rows[0].detail, "repo acme/acme_app · project acme/3");
        assert_eq!(rows[1].detail, "host root@203.0.113.10 · ports 0:localhost:8123");
        assert_eq!(rows[2].detail, "ssh root@203.0.113.10");
        assert_eq!(rows[3].detail, "clickhouse ch · scope acme-app/prod");
        assert_eq!((rows[1].tunnel_status, rows[1].error.clone()), (Some(crate::sources_panel::TunnelStatus::Closed), None));
        assert_eq!((rows[0].tunnel_status, rows[0].error.clone()), (None, None));
        panel.update(cx, |p, cx| p.open_tunnel("t", cx));
        assert_eq!(panel.read_with(cx, |p, cx| p.rows(cx)[1].tunnel_status), Some(crate::sources_panel::TunnelStatus::Opening));
        wait_tunnel_failed(&store, "t", cx);
        let error = panel.read_with(cx, |p, cx| p.rows(cx)[1].error.clone()).expect("errore sotto la riga");
        assert!(error.summary.starts_with("boom. Retrying in"), "{error:?}");
        assert_eq!(panel.read_with(cx, |p, cx| p.rows(cx)[1].tunnel_status), Some(crate::sources_panel::TunnelStatus::Failed));
        show_sources(&workspace, cx);
        assert!(cx.debug_bounds("superzed-source-connection-t").is_some());
        hover_source("t", cx);
        assert!(cx.debug_bounds("superzed-source-tunnel-close-t").is_some());
        assert!(cx.debug_bounds("superzed-sources-add").is_some());
        panel.update(cx, |p, cx| p.close_tunnel("t", cx));
        assert_eq!(panel.read_with(cx, |p, cx| (p.rows(cx)[1].tunnel_status, p.rows(cx)[1].error.clone())), (Some(crate::sources_panel::TunnelStatus::Closed), None));
    }

    /// Aspetta che il tunnel aperto (che fallisce subito) vada in errore.
    fn wait_tunnel_failed(store: &gpui::Entity<ConnectionStore>, id: &str, cx: &mut VisualTestContext) {
        let tunnels = store.read_with(cx, |s, _| s.tunnels().clone());
        for _ in 0..100 {
            tunnels.update(cx, |t, cx| t.poll(cx));
            if matches!(tunnels.read_with(cx, |t, _| t.state(id)), TunnelState::Failed { .. }) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("il tunnel {id} non e' andato in errore");
    }

    fn source_selector(what: &str, id: &str) -> &'static str {
        Box::leak(format!("superzed-source-{what}-{id}").into_boxed_str())
    }

    /// Mouse sopra la riga della connessione.
    fn hover_source(id: &str, cx: &mut VisualTestContext) {
        let row = cx.debug_bounds(source_selector("connection", id)).expect("riga");
        cx.simulate_mouse_move(row.center(), None, gpui::Modifiers::default());
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn the_sources_panel_has_a_header_with_the_total_and_groups_by_kind(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(tunnel("t"), vec![], cx).unwrap();
            s.upsert(github("gh"), vec![], cx).unwrap();
            s.upsert(clickhouse_ssh("ch"), vec![], cx).unwrap();
        });
        let panel = sources_panel(&workspace, cx);
        assert_eq!(panel.read_with(cx, |p, cx| p.total(cx)), 3);
        show_sources(&workspace, cx);
        assert!(cx.debug_bounds("superzed-sources-header").is_some(), "intestazione «Sources»");
        assert!(cx.debug_bounds("superzed-sources-filter").is_some(), "campo filtro");
        let github = cx.debug_bounds("superzed-sources-group-github").expect("gruppo GitHub");
        let tunnels = cx.debug_bounds("superzed-sources-group-ssh_tunnel").expect("gruppo SSH tunnel");
        assert!(cx.debug_bounds("superzed-sources-group-clickhouse").is_some());
        assert!(cx.debug_bounds("superzed-sources-group-zabbix").is_none(), "gruppo vuoto nascosto");
        assert!(github.top() < tunnels.top(), "GitHub prima dei tunnel anche se creato dopo");
        // pill di stato del tunnel, nessuna pill per GitHub
        assert!(cx.debug_bounds("superzed-source-status-t").is_some());
        assert!(cx.debug_bounds("superzed-source-status-gh").is_none());
        // il disclosure chiude il gruppo
        cx.simulate_click(github.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("superzed-source-connection-gh").is_none(), "gruppo chiuso");
        assert!(cx.debug_bounds("superzed-source-connection-t").is_some(), "gli altri restano");
    }

    #[gpui::test]
    async fn the_sources_filter_hides_rows_and_empty_groups(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(github("gh"), vec![], cx).unwrap();
            s.upsert(tunnel("t"), vec![], cx).unwrap();
        });
        let panel = sources_panel(&workspace, cx);
        show_sources(&workspace, cx);
        let editor = panel.read_with(cx, |p, _| p.filter_editor().clone());
        editor.update_in(cx, |e, window, cx| e.set_text("203.0", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds("superzed-source-connection-t").is_some(), "l'host corrisponde");
        assert!(cx.debug_bounds("superzed-source-connection-gh").is_none());
        assert!(cx.debug_bounds("superzed-sources-group-github").is_none(), "gruppo senza risultati nascosto");
        editor.update_in(cx, |e, window, cx| e.set_text("zzz", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds("superzed-sources-no-match").is_some(), "«No sources match»");
        assert!(cx.debug_bounds("superzed-sources-empty").is_none(), "non e' lo stato vuoto");
    }

    fn github_named(id: &str, name: &str) -> Connection {
        Connection {
            id: id.into(),
            name: name.into(),
            config: ConnectionConfig::Github(GithubConfig { owner: Some("acme".into()), project_number: Some(3), repo: Some("acme/acme_app".into()), stale_days: None }),
        }
    }

    // final8 #4: un gruppo collassato a mano non deve nascondere le righe che
    // combaciano col filtro; il filtro cancellato lascia il gruppo com'era.
    #[gpui::test]
    async fn a_collapsed_group_still_shows_matching_rows_while_filtering(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(github_named("gh1", "alpha-repo"), vec![], cx).unwrap();
            s.upsert(github_named("gh2", "beta-repo"), vec![], cx).unwrap();
        });
        let panel = sources_panel(&workspace, cx);
        show_sources(&workspace, cx);
        let github = cx.debug_bounds("superzed-sources-group-github").expect("gruppo GitHub");
        cx.simulate_click(github.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("superzed-source-connection-gh1").is_none(), "gruppo collassato: righe nascoste");
        let editor = panel.read_with(cx, |p, _| p.filter_editor().clone());
        editor.update_in(cx, |e, window, cx| e.set_text("alpha", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("superzed-source-connection-gh1").is_some(),
            "con un filtro attivo il gruppo si mostra espanso anche se collassato"
        );
        assert!(cx.debug_bounds("superzed-source-connection-gh2").is_none(), "l'altra riga del gruppo non combacia col filtro");
        editor.update_in(cx, |e, window, cx| e.set_text("", window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("superzed-source-connection-gh1").is_none(),
            "senza filtro il gruppo torna chiuso: lo stato collassato non era stato toccato"
        );
    }

    #[gpui::test]
    async fn a_failed_tunnel_shows_its_error_under_the_row(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| s.upsert(tunnel("t"), vec![], cx).unwrap());
        store.update(cx, |s, cx| s.hold_tunnel("t", cx));
        wait_tunnel_failed(&store, "t", cx);
        show_sources(&workspace, cx);
        let detail = cx.debug_bounds("superzed-source-detail-t").expect("host/porte");
        let error = cx.debug_bounds("superzed-source-error-t").expect("errore");
        assert!(error.top() >= detail.bottom(), "l'errore sta sotto il dettaglio");
        assert!(cx.debug_bounds("superzed-source-status-t").is_some(), "pill «Failed»");
        // chiuso: niente piu' errore
        store.update(cx, |s, cx| s.close_tunnel("t", cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds("superzed-source-error-t").is_none());
    }

    #[gpui::test]
    async fn source_actions_appear_on_hover_and_delete_asks_first(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(github("gh"), vec![], cx).unwrap();
            s.upsert(tunnel("t"), vec![], cx).unwrap();
        });
        show_sources(&workspace, cx);
        assert!(cx.debug_bounds(source_selector("edit", "gh")).is_none(), "azioni solo al passaggio");
        hover_source("t", cx);
        assert!(cx.debug_bounds(source_selector("tunnel-open", "t")).is_some(), "tunnel chiuso: «Open the tunnel»");
        assert!(cx.debug_bounds(source_selector("add-panel", "t")).is_none(), "un tunnel non si apre come pannello");
        assert!(cx.debug_bounds(source_selector("edit", "gh")).is_none(), "solo la riga sotto il mouse");
        hover_source("gh", cx);
        assert!(cx.debug_bounds(source_selector("edit", "gh")).is_some());
        assert!(cx.debug_bounds(source_selector("add-panel", "gh")).is_some(), "fonte da pannello");
        let delete = cx.debug_bounds(source_selector("delete", "gh")).expect("elimina");
        cx.simulate_click(delete.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(store.read_with(cx, |s, _| s.get("gh").is_some()), "prima la conferma");
        let ok = cx.debug_bounds("superzed-confirm-ok").expect("conferma dell'eliminazione");
        cx.simulate_click(ok.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(store.read_with(cx, |s, _| s.get("gh").is_none()), "eliminata");
    }

    #[gpui::test]
    async fn the_add_panel_action_opens_the_source_on_the_canvas(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| s.upsert(github("gh"), vec![], cx).unwrap());
        show_sources(&workspace, cx);
        hover_source("gh", cx);
        let add = cx.debug_bounds(source_selector("add-panel", "gh")).expect("aggiungi al canvas");
        cx.simulate_click(add.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        let sources: Vec<_> = canvas.read_with(cx, |c, _| {
            c.model().frames().iter().filter(|f| f.kind == superzed_canvas::NodeKind::Source).map(|f| f.id).collect()
        });
        assert_eq!(sources.len(), 1, "un pannello della fonte sul canvas");
        canvas.update(cx, |c, cx| c.remove_nodes(&sources, cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn the_empty_sources_panel_offers_to_add_one(cx: &mut TestAppContext) {
        let (workspace, _store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        show_sources(&workspace, cx);
        assert!(cx.debug_bounds("superzed-sources-empty").is_some(), "«No sources yet»");
        assert!(cx.debug_bounds("superzed-sources-empty-add").is_some(), "bottone «Add source»");
        assert!(cx.debug_bounds("superzed-sources-no-match").is_none());
    }

    // Bottone apri/chiudi e colore dipendono dallo stato del tunnel, non dal testo mostrato.
    #[test]
    fn tunnel_buttons_and_color_follow_the_state_not_the_text() {
        use crate::sources_panel::{TunnelStatus, status_color, tunnel_status};
        let failed = TunnelState::Failed { stderr: "boom".into(), retry_at: std::time::Instant::now() };
        assert_eq!(tunnel_status(&TunnelState::Closed), TunnelStatus::Closed);
        assert_eq!(tunnel_status(&TunnelState::Opening), TunnelStatus::Opening);
        assert_eq!(tunnel_status(&TunnelState::Open), TunnelStatus::Open);
        assert_eq!(tunnel_status(&failed), TunnelStatus::Failed);
        assert_eq!(status_color(TunnelStatus::Failed), ui::Color::Error);
        assert_eq!(status_color(TunnelStatus::Open), ui::Color::Success);
        assert_eq!(status_color(TunnelStatus::Opening), ui::Color::Warning);
        assert_eq!(status_color(TunnelStatus::Closed), ui::Color::Muted);
    }

    #[gpui::test]
    async fn used_by_this_project_goes_to_the_canvas(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| s.upsert(github("gh"), vec![], cx).unwrap());
        let panel = sources_panel(&workspace, cx);
        panel.update(cx, |p, cx| p.set_used("gh", true, cx));
        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        assert_eq!(canvas.read_with(cx, |c, _| c.project_sources().to_vec()), ["gh"]);
        assert_eq!(panel.read_with(cx, |p, cx| p.rows(cx)[0].used), Some(true));
        show_sources(&workspace, cx);
        let check = cx.debug_bounds("superzed-source-used-gh").expect("spunta");
        cx.simulate_click(check.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(canvas.read_with(cx, |c, _| c.project_sources().is_empty()), "click: tolta");
    }

    // Review Focus 1
    #[gpui::test]
    async fn the_form_creates_and_edits_a_connection_keeping_the_password_in_the_keychain(cx: &mut TestAppContext) {
        let (workspace, store, secrets, _dirs, cx) = sources_setup(cx, None).await;
        let panel = sources_panel(&workspace, cx);
        panel.update_in(cx, |p, window, cx| p.open_form(ConnectionKind::Clickhouse, None, window, cx));
        cx.run_until_parked();
        let modal = workspace.read_with(cx, |ws, cx| ws.active_modal::<ConnectionFormModal>(cx)).expect("form aperto");
        modal.update(cx, |m, cx| m.save(cx));
        assert!(modal.read_with(cx, |m, _| m.error().map(str::to_string)).unwrap().contains("URL"), "senza URL ne' host resta aperto");
        modal.update_in(cx, |m, window, cx| {
            m.set_field("name", "CH prod", window, cx);
            m.set_field("url", "http://127.0.0.1:8123", window, cx);
            m.set_field("user", "reader", window, cx);
            m.set_field("password", "SEGRETO", window, cx);
        });
        assert_eq!(modal.read_with(cx, |m, _| m.error().map(str::to_string)), None, "modificare un campo toglie l'errore");
        modal.update(cx, |m, cx| m.save(cx));
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |ws, cx| ws.active_modal::<ConnectionFormModal>(cx)).is_none(), "salvato e chiuso");
        let conn = store.read_with(cx, |s, _| s.get("ch-prod").cloned()).expect("id dal nome");
        assert_eq!(conn.secret_ref(), Some("superzed:ch-prod"));
        assert_eq!(secrets.get("superzed:ch-prod").as_deref(), Some("SEGRETO"));
        let json = std::fs::read_to_string(store.read_with(cx, |s, _| s.path().to_path_buf())).unwrap();
        assert!(!json.contains("SEGRETO"), "{json}");
        // modifica: il form non rilegge la password; lasciata vuota resta quella del Portachiavi
        panel.update_in(cx, |p, window, cx| p.open_form(ConnectionKind::Clickhouse, Some("ch-prod".into()), window, cx));
        cx.run_until_parked();
        let modal = workspace.read_with(cx, |ws, cx| ws.active_modal::<ConnectionFormModal>(cx)).expect("form di modifica");
        modal.update_in(cx, |m, window, cx| m.set_field("name", "CH produzione", window, cx));
        modal.update(cx, |m, cx| m.save(cx));
        cx.run_until_parked();
        let conn = store.read_with(cx, |s, _| s.get("ch-prod").cloned()).unwrap();
        assert_eq!((conn.name.as_str(), conn.secret_ref()), ("CH produzione", Some("superzed:ch-prod")));
        assert_eq!(secrets.get("superzed:ch-prod").as_deref(), Some("SEGRETO"));
    }

    #[gpui::test]
    async fn delete_asks_and_refuses_referenced_connections(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(clickhouse_ssh("ch"), vec![], cx).unwrap();
            s.upsert(hyperdx("hx", "ch"), vec![], cx).unwrap();
        });
        let panel = sources_panel(&workspace, cx);
        panel.update_in(cx, |p, window, cx| p.delete("ch", window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        workspace.read_with(cx, |ws, _| {
            assert!(ws.has_notification(&workspace::notifications::NotificationId::named("superzed-sources-error".into())))
        });
        assert!(store.read_with(cx, |s, _| s.get("ch").is_some()), "usata da hx: resta");
        panel.update_in(cx, |p, window, cx| p.delete("hx", window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("n");
        cx.run_until_parked();
        assert!(store.read_with(cx, |s, _| s.get("hx").is_some()), "n annulla");
        panel.update_in(cx, |p, window, cx| p.delete("hx", window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("y");
        cx.run_until_parked();
        assert!(store.read_with(cx, |s, _| s.get("hx").is_none()));
    }

    #[gpui::test]
    async fn delete_confirmation_names_the_connection_and_its_use_here(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(github("gh"), vec![], cx).unwrap();
            s.upsert(tunnel("t"), vec![], cx).unwrap();
        });
        let panel = sources_panel(&workspace, cx);
        assert_eq!(panel.read_with(cx, |p, cx| p.delete_message("t", cx)), "Delete the connection “box”?");
        panel.update(cx, |p, cx| p.set_used("gh", true, cx));
        assert_eq!(
            panel.read_with(cx, |p, cx| p.delete_message("gh", cx)),
            "Delete the connection “acme-app”? The current project uses it: its panels will be left without a source."
        );
    }

    #[gpui::test]
    async fn the_view_follows_the_canvas_and_counts_down_while_a_tunnel_fails(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        store.update(cx, |s, cx| {
            s.upsert(github("gh"), vec![], cx).unwrap();
            s.upsert(tunnel("t"), vec![], cx).unwrap();
        });
        let panel = sources_panel(&workspace, cx);
        show_sources(&workspace, cx);
        let renders = Rc::new(std::cell::Cell::new(0));
        let count = renders.clone();
        cx.update(|_, cx| cx.observe(&panel, move |_, _| count.set(count.get() + 1)).detach());
        // spunta cambiata dal canvas (non dalla vista): la vista si ridisegna subito
        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        canvas.update(cx, |c, cx| c.set_project_source("gh", true, cx));
        cx.run_until_parked();
        assert!(renders.get() > 0, "la vista osserva project_sources del canvas");
        // tunnel in errore: il conto alla rovescia si aggiorna ogni secondo
        assert!(!panel.read_with(cx, |p, _| p.countdown_active()));
        panel.update(cx, |p, cx| p.open_tunnel("t", cx));
        let tunnels = store.read_with(cx, |s, _| s.tunnels().clone());
        for _ in 0..100 {
            tunnels.update(cx, |t, cx| t.poll(cx));
            if matches!(tunnels.read_with(cx, |t, _| t.state("t")), TunnelState::Failed { .. }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(panel.read_with(cx, |p, _| p.countdown_active()), "timer attivo con un tunnel in errore");
        let before = renders.get();
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(renders.get() > before, "ridisegnata dopo un secondo");
        // nessun tunnel in errore: il timer si ferma
        panel.update(cx, |p, cx| p.close_tunnel("t", cx));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(!panel.read_with(cx, |p, _| p.countdown_active()));
        // configurazione non valida: niente conto alla rovescia promesso
        let bad = superzed_sources::TunnelConfig { host: "-x".into(), forwards: vec![Forward { local_port: 0, remote_host: "localhost".into(), remote_port: 1 }], keep_open: false, idle_minutes: 10 };
        tunnels.update(cx, |t, cx| t.hold("bad", &bad, cx));
        let text = crate::sources_panel::tunnel_error(&tunnels.read_with(cx, |t, _| t.state("bad")), true, std::time::Instant::now()).expect("errore").summary;
        assert!(text.contains("Invalid tunnel configuration") && !text.contains("Retrying"), "{text}");
    }

    #[gpui::test]
    async fn the_form_warns_about_plain_http_with_a_secret(cx: &mut TestAppContext) {
        let (workspace, _store, _secrets, _dirs, cx) = sources_setup(cx, None).await;
        let panel = sources_panel(&workspace, cx);
        panel.update_in(cx, |p, window, cx| p.open_form(ConnectionKind::Zabbix, None, window, cx));
        cx.run_until_parked();
        let modal = workspace.read_with(cx, |ws, cx| ws.active_modal::<ConnectionFormModal>(cx)).expect("form aperto");
        modal.update_in(cx, |m, window, cx| {
            m.set_field("url", "http://zabbix.example", window, cx);
            m.set_field("token", "TOKEN", window, cx);
        });
        assert!(modal.read_with(cx, |m, cx| m.warning(cx)).unwrap().contains("plain text"));
        modal.update_in(cx, |m, window, cx| m.set_field("url", "https://zabbix.example", window, cx));
        assert_eq!(modal.read_with(cx, |m, cx| m.warning(cx)), None);
    }

    // Review Focus 2
    #[gpui::test]
    async fn a_corrupt_file_shows_a_banner(cx: &mut TestAppContext) {
        let (workspace, store, _secrets, _dirs, cx) = sources_setup(cx, Some("{ rotto")).await;
        let panel = sources_panel(&workspace, cx);
        let banner = panel.read_with(cx, |p, cx| p.banner(cx)).expect("avviso");
        assert!(banner.contains("not readable"), "{banner}");
        show_sources(&workspace, cx);
        assert!(cx.debug_bounds("superzed-sources-banner").is_some());
        // corretto a mano: "Ricarica il file" toglie l'avviso
        let path = store.read_with(cx, |s, _| s.path().to_path_buf());
        std::fs::write(&path, r#"{"version": 1, "connections": []}"#).unwrap();
        let reload = cx.debug_bounds("superzed-sources-reload").expect("pulsante ricarica");
        cx.simulate_click(reload.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(panel.read_with(cx, |p, cx| p.banner(cx)), None);
        assert!(cx.debug_bounds("superzed-sources-banner").is_none());
    }

    #[test]
    fn bundle_detection() {
        assert!(crate::notifications_panel::is_app_bundle(std::path::Path::new("/Applications/superzed.app/Contents/MacOS/zed")));
        assert!(!crate::notifications_panel::is_app_bundle(std::path::Path::new("/Users/u/Progetti/superzed/target/spike/zed")));
    }

    // Task 18: sezione «Limits» in fondo al pannello Sessions.
    fn limits_selector(what: &str) -> &'static str {
        Box::leak(format!("superzed-limits-{what}").into_boxed_str())
    }

    async fn sessions_with_accounts(cx: &mut TestAppContext) -> (gpui::Entity<Workspace>, tempfile::TempDir, &'static mut VisualTestContext) {
        let (workspace, dir, cx) = setup(cx).await;
        // a1 largo, a2 stretto (week 12%): a2 va per primo
        cx.update(|_, cx| {
            superzed_accounts::test_support::fixture_store(cx, dir.path(), &[(37., 20.), (10., 88.)]);
        });
        cx.run_until_parked();
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SessionsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        (workspace, dir, cx)
    }

    #[gpui::test]
    async fn the_limits_section_shows_every_account_with_two_bars_and_collapses(cx: &mut TestAppContext) {
        let (workspace, _dir, cx) = sessions_with_accounts(cx).await;
        let panel = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        assert!(panel.read_with(cx, |p, _| p.limits_open()), "aperta di default");
        let section = cx.debug_bounds("superzed-limits").expect("sezione Limits");
        let list = cx.debug_bounds("superzed-sessions-list").expect("lista");
        assert!(section.top() >= list.bottom(), "fuori dalla lista, sotto");
        assert!(cx.debug_bounds(limits_selector("header")).is_some());
        for id in ["a1", "a2", "system"] {
            assert!(cx.debug_bounds(limits_selector(&format!("row-{id}"))).is_some(), "riga {id}");
        }
        // Task 23: una riga per finestra: nome | barra | percentuale | reset breve
        for id in ["a1", "a2"] {
            let five = cx.debug_bounds(limits_selector(&format!("bar-{id}-5h"))).expect("barra 5h");
            let week = cx.debug_bounds(limits_selector(&format!("bar-{id}-week"))).expect("barra Week");
            assert!(five.bottom() <= week.top(), "5h sopra Week, una riga ciascuna");
            assert_eq!(five.size.width, week.size.width, "colonne fisse: barre uguali");
            let name = cx.debug_bounds(limits_selector(&format!("name-{id}"))).expect("nome");
            assert!(name.bottom() <= five.top(), "il nome sopra le finestre");
            for key in ["5h", "week"] {
                let bar = cx.debug_bounds(limits_selector(&format!("bar-{id}-{key}"))).unwrap();
                let label = cx.debug_bounds(limits_selector(&format!("label-{id}-{key}"))).expect("nome finestra");
                let pct = cx.debug_bounds(limits_selector(&format!("pct-{id}-{key}"))).expect("percentuale");
                let reset = cx.debug_bounds(limits_selector(&format!("reset-{id}-{key}"))).expect("reset breve");
                assert!(label.right() <= bar.left() && bar.right() <= pct.left() && pct.right() <= reset.left(), "in fila {id} {key}");
                for other in [label, pct, reset] {
                    assert!(other.top() < bar.bottom() && other.bottom() > bar.top(), "sulla stessa riga {id} {key}");
                }
                assert!(bar.size.width > name.size.width / 4., "la barra prende lo spazio restante");
            }
        }
        assert!(cx.debug_bounds(limits_selector("login-needed-system")).is_some(), "sistema senza token: «Login needed»");
        let (a1, a2) = (
            cx.debug_bounds(limits_selector("row-a1")).unwrap(),
            cx.debug_bounds(limits_selector("row-a2")).unwrap(),
        );
        assert!(a2.top() < a1.top(), "prima l'account piu' stretto");
        let header = cx.debug_bounds(limits_selector("header")).unwrap();
        cx.simulate_click(header.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert!(!panel.read_with(cx, |p, _| p.limits_open()), "richiusa");
        assert!(cx.debug_bounds(limits_selector("row-a1")).is_none(), "righe nascoste");
        assert!(cx.debug_bounds(limits_selector("header")).is_some(), "l'intestazione resta");
    }

    // Task 19: un limite per modello (`weekly_scoped`, oggi "Fable") diventa un blocco in
    // piu' nella riga dell'account, affiancato a 5h e Week.
    // Account sloggato (nessun token nel Keychain): la riga di Limits dice «Login needed»
    // senza barre (le quote in cache direbbero «100% now»); «Log in» apre una chat su
    // quell'account, dove claude chiede il login (il picker di cmd-shift-n lo esclude).
    #[gpui::test]
    async fn a_logged_out_account_shows_login_needed_and_log_in_opens_a_chat_on_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let (workspace, dir, cx) = setup(cx).await;
        cx.update(|_, cx| {
            let (_store, tokens, _fetcher) = superzed_accounts::test_support::fixture_store(cx, dir.path(), &[(37., 20.), (10., 88.)]);
            tokens.set("a1", None);
            cx.set_global(superzed_canvas::ClaudePtyOverride(zsh("claude")));
        });
        cx.run_until_parked();
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SessionsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds(limits_selector("login-needed-a1")).is_some(), "«Login needed»");
        assert!(cx.debug_bounds(limits_selector("bar-a1-5h")).is_none(), "niente barre");
        assert!(cx.debug_bounds(limits_selector("no-data-a1")).is_none());
        assert!(cx.debug_bounds(limits_selector("bar-a2-5h")).is_some(), "a2 e' loggato");
        let a1 = cx.debug_bounds(limits_selector("row-a1")).unwrap();
        let a2 = cx.debug_bounds(limits_selector("row-a2")).unwrap();
        assert!(a1.top() > a2.top(), "lo sloggato in fondo");
        let button = cx.debug_bounds(limits_selector("login-a1")).expect("pulsante Log in");
        cx.simulate_click(button.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        assert_eq!(canvas.read_with(cx, |c, cx| c.terminal_nodes(cx).len()), 1, "una chat nuova");
        let cmd = canvas.read_with(cx, |c, _| c.last_claude_command().unwrap().to_string());
        assert!(cmd.contains("CLAUDE_CONFIG_DIR=") && cmd.contains("cfg-a1"), "{cmd}");
        canvas.update(cx, |c, cx| {
            let ids: Vec<_> = c.terminal_nodes(cx).into_iter().map(|(id, _)| id).collect();
            c.remove_nodes(&ids, cx)
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn the_limits_section_shows_the_fable_block_for_an_account_with_a_model_limit(cx: &mut TestAppContext) {
        let (workspace, dir, cx) = setup(cx).await;
        cx.update(|_, cx| {
            let (_store, _tokens, fetcher) = superzed_accounts::test_support::fixture_store(cx, dir.path(), &[(37., 20.)]);
            fetcher.set(
                "tok-a1",
                superzed_accounts::FetchOutcome::Ok(superzed_accounts::test_support::usage_json_scoped(37., 20., &[("Fable", 91.)])),
            );
        });
        cx.run_until_parked();
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SessionsPanel>(window, cx));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let five = cx.debug_bounds(limits_selector("bar-a1-5h")).expect("barra 5h");
        let week = cx.debug_bounds(limits_selector("bar-a1-week")).expect("barra Week");
        let fable = cx.debug_bounds(limits_selector("bar-a1-fable")).expect("barra Fable");
        assert_eq!(five.size.width, fable.size.width, "colonne della stessa larghezza");
        assert!(week.left() >= five.right() || week.top() >= five.bottom(), "affiancati o a capo");
        // il modello non compare per un account senza limiti per modello
        assert!(cx.debug_bounds(limits_selector("bar-system-fable")).is_none());
    }

    // Final fix 6, punto 4: aprire i limiti chiede anche l'aggiornamento delle quote
    // (`RefreshQuotas`, gia' limitato da MANUAL_MIN_AGE nello store).
    #[gpui::test]
    async fn show_account_limits_also_refreshes_the_quotas(cx: &mut TestAppContext) {
        let (_workspace, _dir, cx) = sessions_with_accounts(cx).await;
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = count.clone();
        cx.update(|_, cx| { cx.on_action(move |_: &superzed_accounts::RefreshQuotas, _| seen.set(seen.get() + 1)); });
        cx.dispatch_action(superzed_actions::ShowAccountLimits);
        cx.run_until_parked();
        assert_eq!(count.get(), 1, "un RefreshQuotas per ShowAccountLimits");
    }

    #[gpui::test]
    async fn clicking_the_status_bar_limits_opens_sessions_with_limits_open(cx: &mut TestAppContext) {
        let (workspace, _dir, cx) = sessions_with_accounts(cx).await;
        let panel = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap());
        panel.update(cx, |p, cx| p.set_limits_open(false, cx));
        cx.dispatch_action(superzed_actions::SidebarView(4));
        cx.run_until_parked();
        let item = cx.update(|_, cx| gpui::AppContext::new(cx, |cx| superzed_accounts::QuotaStatusItem::new(cx)));
        workspace.update_in(cx, |ws, window, cx| {
            ws.status_bar().update(cx, |sb, cx| sb.add_right_item(item, window, cx))
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let quota = cx.debug_bounds("superzed-quota").expect("limiti nella status bar");
        cx.simulate_click(quota.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        workspace.read_with(cx, |ws, cx| {
            let dock = ws.left_dock().read(cx);
            assert!(dock.is_open());
            assert_eq!(dock.active_panel_index(), dock.panel_index_for_type::<SessionsPanel>());
        });
        assert!(panel.read_with(cx, |p, _| p.limits_open()), "Limits aperta");
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        assert!(cx.debug_bounds(limits_selector("row-a1")).is_some());
    }

    // Tasto destro sul nome di un account in «Limits»: «Exclude from <progetto>» mette (e
    // poi toglie) la cartella del progetto aperto in `exclude_from`, salvato nel registro.
    #[gpui::test]
    async fn right_click_on_a_limits_account_toggles_its_exclusion_from_the_open_project(cx: &mut TestAppContext) {
        let (_workspace, dir, cx) = sessions_with_accounts(cx).await;
        let store = cx.update(|_, cx| superzed_accounts::AccountStore::global(cx).unwrap());
        let registry = dir.path().join("accounts.json");
        let home = util::paths::home_dir();
        // il menu si apre solo con l'elemento sotto il mouse: prima lo spostamento
        let right_click = |cx: &mut VisualTestContext, selector: &str| {
            let target = cx.debug_bounds(limits_selector(selector)).expect("riga da cliccare");
            cx.simulate_mouse_move(target.center(), None, gpui::Modifiers::default());
            cx.simulate_mouse_down(target.center(), gpui::MouseButton::Right, gpui::Modifiers::default());
            cx.simulate_mouse_up(target.center(), gpui::MouseButton::Right, gpui::Modifiers::default());
            cx.run_until_parked();
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
        };
        let choose_exclude = |cx: &mut VisualTestContext| {
            right_click(cx, "name-a1");
            let entry = cx.debug_bounds("MENU_ITEM-Exclude from repo").expect("voce «Exclude from repo»");
            cx.simulate_click(entry.center(), gpui::Modifiers::default());
            cx.run_until_parked();
        };

        choose_exclude(cx);
        store.read_with(cx, |s, _| assert!(s.account("a1").unwrap().excludes_folder(Path::new("/repo"), home), "escluso da /repo"));
        let saved = superzed_accounts::registry::read_registry(&registry).unwrap().unwrap();
        assert_eq!(saved.accounts[0].exclude_from, vec![PathBuf::from("/repo")], "scritto nel registro");

        choose_exclude(cx);
        store.read_with(cx, |s, _| assert!(!s.account("a1").unwrap().excludes_folder(Path::new("/repo"), home), "riammesso"));
        let saved = superzed_accounts::registry::read_registry(&registry).unwrap().unwrap();
        assert!(saved.accounts[0].exclude_from.is_empty());

        // l'account di sistema non ha il menu
        right_click(cx, "name-system");
        assert!(cx.debug_bounds("MENU_ITEM-Exclude from repo").is_none(), "nessun menu per il sistema");
    }

    // Task 22: con il focus in un pannello della sidebar cmd-alt-destra/sinistra non
    // facevano niente (nessun binding, e comunque nessun gestore che raggiungesse il
    // canvas). Il keymap vero (assets/keymaps/default-macos.json) lega ora i quattro
    // contesti dei pannelli, e `superzed_canvas::init` inoltra al canvas del progetto
    // attivo (`with_canvas`, come le altre azioni superzed del Workspace).

    fn bind_real_keymap(cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            let keymap = include_str!("../../../assets/keymaps/default-macos.json");
            let bindings = match settings::KeymapFile::load(keymap, cx) {
                settings::KeymapFileLoadResult::Success { key_bindings }
                | settings::KeymapFileLoadResult::SomeFailedToLoad { key_bindings, .. } => key_bindings,
                settings::KeymapFileLoadResult::JsonParseFailure { error } => panic!("keymap: {error}"),
            };
            cx.bind_keys(bindings);
        });
    }

    /// Canvas con due aree: una chat agganciata al kanban (dove si mette il focus,
    /// cosi' e' quella l'area "corrente" del canvas) e un nodo libero (l'altra area
    /// esistente); niente contenitore delle Automazioni (`DefaultContainers(false)`),
    /// cosi' il ciclo ha solo due aree fra cui muoversi, senza ambiguita'.
    async fn workspace_with_two_canvas_areas(
        cx: &mut TestAppContext,
    ) -> (
        gpui::Entity<Workspace>,
        gpui::Entity<superzed_canvas::CanvasView>,
        superzed_canvas::NodeId,
        superzed_canvas::NodeId,
        &'static mut VisualTestContext,
    ) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            let store = settings::SettingsStore::test(cx);
            cx.set_global(store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            terminal_view::init(cx);
            cx.set_global(superzed_canvas::DefaultContainers(false));
            superzed_canvas::init(cx);
            crate::init(cx);
        });
        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree("/repo", json!({ "a.txt": "" })).await;
        let project = project::Project::test(fs, ["/repo".as_ref()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
        cx.run_until_parked();
        bind_real_keymap(cx);

        let canvas = workspace.read_with(cx, |ws, _| superzed_canvas::canvas_for(ws).unwrap());
        // `dock` richiede una chat claude e il contenitore del kanban gia' sul canvas
        // (con `DefaultContainers(false)` non nasce da solo): niente vero `claude`, il
        // comando e' un semplice zsh come nelle altre chat fittizie di questo modulo
        canvas.update_in(cx, |c, window, cx| {
            c.ensure_container(superzed_canvas::DockKind::Kanban, cx);
            c.open_terminal_with_spec(superzed_canvas::NodeKind::Claude, None, zsh("chat"), None, window, cx)
        });
        cx.run_until_parked();
        let kanban_chat = canvas.read_with(cx, |c, cx| c.terminal_nodes(cx)[0].0);
        canvas.update(cx, |c, cx| c.dock(kanban_chat, superzed_sessions::Phase::New, cx));
        canvas.update_in(cx, |c, window, cx| {
            c.open_terminal(superzed_canvas::NodeKind::Shell, None, &serde_json::Value::Null, window, cx)
        });
        cx.run_until_parked();
        let free_node = canvas.read_with(cx, |c, cx| {
            c.terminal_nodes(cx).into_iter().map(|(id, _)| id).find(|id| *id != kanban_chat).unwrap()
        });
        // l'area corrente del canvas, prima di passare il focus al pannello: la chat del kanban
        canvas.update_in(cx, |c, window, cx| c.focus_node(kanban_chat, window, cx));
        cx.run_until_parked();
        (workspace, canvas, kanban_chat, free_node, cx)
    }

    #[gpui::test]
    async fn cmd_alt_right_from_the_sessions_panel_switches_the_active_canvas_area(cx: &mut TestAppContext) {
        let (workspace, canvas, kanban_chat, free_node, cx) = workspace_with_two_canvas_areas(cx).await;
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<SessionsPanel>(window, cx));
        cx.run_until_parked();
        let sessions_focus = workspace.read_with(cx, |ws, cx| ws.panel::<SessionsPanel>(cx).unwrap().read(cx).focus_handle(cx));
        cx.update(|window, _| assert!(sessions_focus.is_focused(window), "focus nel pannello Sessions"));

        cx.simulate_keystrokes("cmd-alt-right");
        cx.run_until_parked();

        let free_focus = canvas.read_with(cx, |c, cx| c.node_focus_handle(free_node, cx).unwrap());
        cx.update(|window, _| assert!(free_focus.is_focused(window), "area successiva rispetto al kanban: il nodo libero"));
        let kanban_focus = canvas.read_with(cx, |c, cx| c.node_focus_handle(kanban_chat, cx).unwrap());
        cx.update(|window, _| assert!(!kanban_focus.is_focused(window), "non e' rimasto sulla chat del kanban"));
    }

    #[gpui::test]
    async fn cmd_alt_left_from_the_sources_panel_switches_the_active_canvas_area(cx: &mut TestAppContext) {
        let (workspace, canvas, _kanban_chat, free_node, cx) = workspace_with_two_canvas_areas(cx).await;
        workspace.update_in(cx, |ws, window, cx| ws.focus_panel::<crate::sources_panel::SourcesPanel>(window, cx));
        cx.run_until_parked();
        let sources_focus =
            workspace.read_with(cx, |ws, cx| ws.panel::<crate::sources_panel::SourcesPanel>(cx).unwrap().read(cx).focus_handle(cx));
        cx.update(|window, _| assert!(sources_focus.is_focused(window), "focus nel pannello Fonti"));

        cx.simulate_keystrokes("cmd-alt-left");
        cx.run_until_parked();

        let free_focus = canvas.read_with(cx, |c, cx| c.node_focus_handle(free_node, cx).unwrap());
        cx.update(|window, _| assert!(free_focus.is_focused(window), "area precedente rispetto al kanban: il nodo libero"));
    }
}
