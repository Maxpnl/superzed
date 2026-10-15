use crate::CanvasView;
use crate::node::NodeContent;
use fs::FakeFs;
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, Render,
    TestAppContext, VisualTestContext, Window, div,
};
use std::{cell::Cell, rc::Rc, sync::Arc};
use ui::prelude::*;
use workspace::{MultiWorkspace, Workspace};

pub fn init_test(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let store = settings::SettingsStore::test(cx);
        cx.set_global(store);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        editor::init(cx);
        terminal_view::init(cx);
        superzed_sessions::init_test(cx, std::env::temp_dir().join("superzed-no-claude"), None);
        superzed_notifications::init_test(
            cx,
            std::env::temp_dir().join(format!("superzed-notifications-{}", uuid::Uuid::new_v4())),
        );
        // mai gh o claude veri: ogni comando fallisce ("comando non previsto") finche' un
        // test non mette i suoi finti con `superzed_done::init_test`
        superzed_done::init_test(
            cx,
            superzed_sources::test_support::FakeRunner::new(),
            superzed_sources::test_support::FakeRunner::new(),
            None,
        );
        crate::init(cx);
        // i test partono da un canvas vuoto; `workspace_with_repo_opts` sceglie
        cx.set_global(crate::DefaultContainers(false));
    });
}

/// Workspace vero su FakeFs con root `/repo`, canvas agganciato da `crate::init`; senza
/// i contenitori di default (canvas vuoto).
pub async fn workspace_with_repo(
    cx: &mut TestAppContext,
    tree: serde_json::Value,
) -> (Entity<Workspace>, Entity<CanvasView>, Arc<FakeFs>, &'static mut VisualTestContext) {
    workspace_with_repo_opts(cx, tree, false).await
}

/// Come `workspace_with_repo`, scegliendo se al caricamento si creano kanban e Automazioni.
pub async fn workspace_with_repo_opts(
    cx: &mut TestAppContext,
    tree: serde_json::Value,
    default_containers: bool,
) -> (Entity<Workspace>, Entity<CanvasView>, Arc<FakeFs>, &'static mut VisualTestContext) {
    init_test(cx);
    cx.update(|cx| cx.set_global(crate::DefaultContainers(default_containers)));
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree("/repo", tree).await;
    let project = project::Project::test(fs.clone(), ["/repo".as_ref()], cx).await;
    let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
    let workspace = window.read_with(cx, |mw, _| mw.workspace().clone()).unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx).into_mut();
    cx.run_until_parked();
    let canvas = workspace.read_with(cx, |ws, _| crate::canvas_for(ws).expect("canvas"));
    (workspace, canvas, fs, cx)
}

/// Contenuto finto che registra l'ultimo zoom ricevuto.
pub struct Probe {
    focus: FocusHandle,
}

impl Focusable for Probe {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.focus)
    }
}

pub struct RecordingContent {
    pub view: Entity<Probe>,
    pub zoom: Rc<Cell<f32>>,
    pub closed: Rc<Cell<bool>>,
}

impl RecordingContent {
    pub fn new(cx: &mut App) -> Self {
        Self {
            view: cx.new(|cx| Probe { focus: cx.focus_handle() }),
            zoom: Rc::new(Cell::new(0.)),
            closed: Rc::new(Cell::new(false)),
        }
    }
}

impl NodeContent for RecordingContent {
    fn view(&self) -> AnyView {
        self.view.clone().into()
    }
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.focus_handle(cx)
    }
    fn set_font_zoom(&self, zoom: f32, _: &mut App) {
        self.zoom.set(zoom);
    }
    fn on_close(&self, _: &mut App) {
        self.closed.set(true);
    }
}
