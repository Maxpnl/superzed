// superzed: layout canvas | editor dentro il center del Workspace
use crate::Workspace;
use gpui::{
    AnyElement, AnyView, App, Context, DragMoveEvent, Empty, FocusHandle, Focusable, IntoElement,
    Pixels, Render, SharedString, Window, deferred, div, px,
};
use ui::prelude::*;

pub const MIN_CANVAS_WIDTH: Pixels = px(200.);
pub const MIN_EDITOR_WIDTH: Pixels = px(240.);
pub const DEFAULT_EDITOR_WIDTH: Pixels = px(640.);

/// Valori per progetto che vivono sul Workspace e vengono salvati in
/// `.superzed/project.json` dal crate superzed_canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct SuperzedProjectState {
    pub editor_width: Pixels,
    pub tab_name: Option<SharedString>,
    pub tab_color: Option<u8>,
    pub editor_font_size: Option<Pixels>,
}

impl Default for SuperzedProjectState {
    fn default() -> Self {
        Self {
            editor_width: DEFAULT_EDITOR_WIDTH,
            tab_name: None,
            tab_color: None,
            editor_font_size: None,
        }
    }
}

struct CanvasSlot {
    view: AnyView,
    focus: FocusHandle,
}

/// Il canvas (in `superzed_canvas`, che `workspace` non vede) ha nodi? Impostato da
/// `superzed_canvas::init`; senza, nessun nodo.
pub struct CanvasHasNodes(pub fn(&AnyView, &App) -> bool);
impl gpui::Global for CanvasHasNodes {}

/// Unico campo superzed dentro `Workspace`.
#[derive(Default)]
pub struct SuperzedWorkspace {
    canvas: Option<CanvasSlot>,
    state: SuperzedProjectState,
}

/// Larghezza effettiva dell'area editor: rispetta i minimi di editor e canvas
/// e non supera mai lo spazio disponibile.
pub fn clamp_editor_width(requested: Pixels, available: Pixels) -> Pixels {
    let max_editor = (available - MIN_CANVAS_WIDTH).max(px(0.));
    let lower = MIN_EDITOR_WIDTH.min(max_editor);
    requested.clamp(lower, max_editor.max(lower)).min(available)
}

impl Workspace {
    pub fn set_superzed_canvas(&mut self, view: AnyView, focus: FocusHandle, cx: &mut Context<Self>) {
        self.superzed.canvas = Some(CanvasSlot { view, focus });
        cx.notify();
    }

    pub fn superzed_canvas(&self) -> Option<AnyView> {
        self.superzed.canvas.as_ref().map(|slot| slot.view.clone())
    }

    pub fn superzed_canvas_has_nodes(&self, cx: &App) -> bool {
        match (&self.superzed.canvas, cx.try_global::<CanvasHasNodes>()) {
            (Some(slot), Some(hook)) => (hook.0)(&slot.view, cx),
            _ => false,
        }
    }

    pub fn superzed_state(&self) -> &SuperzedProjectState {
        &self.superzed.state
    }

    pub fn update_superzed_state(
        &mut self,
        f: impl FnOnce(&mut SuperzedProjectState),
        cx: &mut Context<Self>,
    ) {
        let before = self.superzed.state.clone();
        f(&mut self.superzed.state);
        if self.superzed.state != before {
            cx.notify();
        }
    }

    pub fn superzed_has_center_items(&self, cx: &App) -> bool {
        self.panes().iter().any(|pane| pane.read(cx).items_len() > 0)
    }

    pub fn superzed_focus_canvas(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(slot) = &self.superzed.canvas {
            window.focus(&slot.focus, cx);
        }
    }

    /// `cmd-e`: focus sull'editor, o file finder se non ci sono file aperti.
    pub fn superzed_focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.superzed_has_center_items(cx) {
            let focus = self.active_pane().focus_handle(cx);
            window.focus(&focus, cx);
        } else {
            window.dispatch_action(Box::new(crate::ToggleFileFinder::default()), cx);
        }
    }

    // superzed: chiamato quando il center guadagna o perde un item
    pub(crate) fn superzed_on_center_items_changed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
        if self.superzed.canvas.is_none() || self.superzed_has_center_items(cx) {
            return;
        }
        // l'area editor sparisce: se il focus era li' (o non c'e' piu'), va al canvas
        let pane_focus = self.active_pane().focus_handle(cx);
        if window.focused(cx).is_none() || pane_focus.contains_focused(window, cx) {
            self.superzed_focus_canvas(window, cx);
        }
    }
}

#[derive(Clone)]
pub struct DraggedEditorEdge;

impl Render for DraggedEditorEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub(crate) fn render_canvas_and_editor(
    ws: &Workspace,
    editor: AnyElement,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(slot) = ws.superzed.canvas.as_ref() else {
        return editor;
    };
    let canvas = slot.view.clone();
    let has_items = ws.superzed_has_center_items(cx);
    // Approssimazione: larghezza dell'intero workspace (dock inclusi). E' innocua
    // perche' l'area editor e' restringibile; il clamp vero avviene nel drag.
    let editor_width = clamp_editor_width(ws.superzed.state.editor_width, ws.bounds.size.width);
    let weak = ws.weak_handle();

    h_flex()
        .id("superzed-canvas-editor")
        .debug_selector(|| "superzed-row".into())
        .size_full()
        .on_drag_move::<DraggedEditorEdge>(move |e: &DragMoveEvent<DraggedEditorEdge>, _, cx| {
            // `e.bounds` e' la riga canvas|editor: l'editor e' ancorato al lato destro
            let requested = e.bounds.right() - e.event.position.x;
            let width = clamp_editor_width(requested, e.bounds.size.width);
            weak.update(cx, |ws, cx| ws.update_superzed_state(|s| s.editor_width = width, cx))
                .ok();
        })
        .child(
            div()
                .debug_selector(|| "superzed-canvas-area".into())
                .flex_1()
                .h_full()
                .min_w(MIN_CANVAS_WIDTH)
                .overflow_hidden()
                .child(canvas),
        )
        .child(if has_items {
            div()
                .debug_selector(|| "superzed-editor-area".into())
                .relative()
                .h_full()
                // la larghezza salvata e' la base: se il center e' piu' stretto
                // l'editor si restringe e il canvas tiene i suoi MIN_CANVAS_WIDTH
                .w(editor_width)
                .flex_shrink(1.)
                .min_w(px(0.))
                .border_l_1()
                .border_color(cx.theme().colors().border)
                .child(editor)
                .child(deferred(
                    div()
                        .id("superzed-editor-resize-handle")
                        .absolute()
                        .left(px(-3.))
                        .top(px(0.))
                        .h_full()
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(DraggedEditorEdge, |d, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| d.clone())
                        })
                        .occlude(),
                ))
                .into_any_element()
        } else {
            // Nessun item: l'area editor "non esiste" visivamente (il canvas
            // prende tutta la riga), ma il pane resta nell'albero, invisibile
            // e a larghezza zero, cosi' il suo focus handle resta tracciato
            // e cmd-j/cmd-e/cmd-n restano raggiungibili dalla tastiera anche
            // quando il focus logico e' ancora sul pane (vedi Workspace::focus_handle
            // upstream, che punta sempre al pane attivo).
            div()
                .debug_selector(|| "superzed-editor-hidden".into())
                .h_full()
                .w(px(0.))
                .flex_shrink(1.)
                .min_w(px(0.))
                .overflow_hidden()
                .invisible()
                .child(editor)
                .into_any_element()
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dock::{DockPosition, test::TestPanel},
        item::test::TestItem,
        tests::init_test,
    };
    use gpui::{Entity, FocusHandle, TestAppContext, VisualTestContext, size};
    use project::Project;

    struct FocusableCanvas {
        focus: FocusHandle,
    }

    impl Render for FocusableCanvas {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().track_focus(&self.focus)
        }
    }

    async fn workspace_with_canvas(
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, FocusHandle, &mut VisualTestContext) {
        init_test(cx);
        let fs = fs::FakeFs::new(cx.executor());
        let project = Project::test(fs, [], cx).await;
        let (workspace, cx) =
            cx.add_window_view(|window, cx| Workspace::test_new(project, window, cx));
        let focus = workspace.update_in(cx, |ws, _, cx| {
            let focus = cx.focus_handle();
            let canvas = cx.new(|_| FocusableCanvas { focus: focus.clone() });
            ws.set_superzed_canvas(canvas.into(), focus.clone(), cx);
            focus
        });
        (workspace, focus, cx)
    }

    fn add_item(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
        let item = cx.new(|cx| TestItem::new(cx));
        workspace.update_in(cx, |ws, window, cx| {
            ws.add_item_to_active_pane(Box::new(item), None, true, window, cx)
        });
        redraw(cx);
    }

    fn redraw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_superzed_editor_area_exists_only_with_items(cx: &mut TestAppContext) {
        let (workspace, _, cx) = workspace_with_canvas(cx).await;
        redraw(cx);
        assert!(cx.debug_bounds("superzed-canvas-area").is_some());
        assert!(cx.debug_bounds("superzed-editor-area").is_none(), "nessun file: niente area editor");
        add_item(&workspace, cx);
        assert!(cx.debug_bounds("superzed-editor-area").is_some());
    }

    // Review Focus 5
    #[gpui::test]
    async fn test_superzed_closing_last_item_hides_editor_and_focuses_canvas(
        cx: &mut TestAppContext,
    ) {
        let (workspace, canvas_focus, cx) = workspace_with_canvas(cx).await;
        cx.simulate_resize(size(px(1000.), px(800.)));
        workspace.update_in(cx, |ws, window, cx| {
            let right = cx.new(|cx| TestPanel::new(DockPosition::Right, 101, cx));
            ws.add_panel(right, window, cx);
            ws.toggle_dock(DockPosition::Right, window, cx);
        });
        add_item(&workspace, cx);
        let pane = workspace.read_with(cx, |ws, _| ws.active_pane().clone());
        pane.update_in(cx, |pane, window, cx| window.focus(&pane.focus_handle(cx), cx));
        let close = pane.update_in(cx, |pane, window, cx| {
            pane.close_active_item(&crate::pane::CloseActiveItem::default(), window, cx)
        });
        close.await.unwrap();
        redraw(cx);
        assert!(cx.debug_bounds("superzed-editor-area").is_none());
        let row = cx.debug_bounds("superzed-row").unwrap();
        let canvas = cx.debug_bounds("superzed-canvas-area").unwrap();
        assert!((canvas.size.width - row.size.width).abs() < px(0.5), "il canvas prende tutta la riga");
        cx.update(|window, _| assert!(canvas_focus.is_focused(window), "focus al canvas"));
    }

    // dallo spike: center piu' stretto di editor_width + MIN_CANVAS_WIDTH
    #[gpui::test]
    async fn test_superzed_row_does_not_overflow_narrow_center(cx: &mut TestAppContext) {
        let (workspace, _, cx) = workspace_with_canvas(cx).await;
        cx.simulate_resize(size(px(1000.), px(800.)));
        workspace.update_in(cx, |ws, window, cx| {
            let left = cx.new(|cx| TestPanel::new(DockPosition::Left, 100, cx));
            ws.add_panel(left, window, cx);
            let right = cx.new(|cx| TestPanel::new(DockPosition::Right, 101, cx));
            ws.add_panel(right, window, cx);
            ws.toggle_dock(DockPosition::Left, window, cx);
            ws.toggle_dock(DockPosition::Right, window, cx);
        });
        add_item(&workspace, cx);
        let row = cx.debug_bounds("superzed-row").expect("row");
        let canvas = cx.debug_bounds("superzed-canvas-area").expect("canvas");
        let editor = cx.debug_bounds("superzed-editor-area").expect("editor");
        assert!(row.size.width < DEFAULT_EDITOR_WIDTH + MIN_CANVAS_WIDTH, "precondizione: {row:?}");
        assert!(editor.right() <= row.right() + px(0.5), "editor {editor:?} sfora {row:?}");
        assert!(canvas.size.width >= MIN_CANVAS_WIDTH - px(0.5), "canvas {canvas:?}");
    }

    #[gpui::test]
    async fn test_superzed_editor_width_comes_from_state(cx: &mut TestAppContext) {
        let (workspace, _, cx) = workspace_with_canvas(cx).await;
        cx.simulate_resize(size(px(1600.), px(800.)));
        workspace.update(cx, |ws, cx| ws.update_superzed_state(|s| s.editor_width = px(500.), cx));
        add_item(&workspace, cx);
        let editor = cx.debug_bounds("superzed-editor-area").unwrap();
        assert!((editor.size.width - px(500.)).abs() < px(1.), "editor = {editor:?}");
    }

    #[gpui::test]
    async fn test_superzed_focus_editor_focuses_pane_or_asks_file_finder(cx: &mut TestAppContext) {
        let (workspace, _, cx) = workspace_with_canvas(cx).await;
        let asked = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = asked.clone();
        // Il vero handler di ToggleFileFinder vive in file_finder::init, non
        // disponibile nei test di workspace: come altrove in questo crate
        // (vedi pane.rs), un action handler globale sta al suo posto.
        cx.update(|_, cx| {
            cx.on_action(move |_: &crate::ToggleFileFinder, _cx| flag.set(true));
        });
        workspace.update_in(cx, |ws, window, cx| ws.superzed_focus_editor(window, cx));
        cx.run_until_parked();
        assert!(asked.get(), "senza file: file finder");
        add_item(&workspace, cx);
        workspace.update_in(cx, |ws, window, cx| ws.superzed_focus_editor(window, cx));
        cx.run_until_parked();
        let pane = workspace.read_with(cx, |ws, _| ws.active_pane().clone());
        cx.update(|window, cx| assert!(pane.focus_handle(cx).contains_focused(window, cx)));
    }

    #[test]
    fn editor_width_is_kept_when_it_fits() {
        assert_eq!(clamp_editor_width(px(600.), px(1400.)), px(600.));
    }

    #[test]
    fn editor_width_leaves_min_canvas_when_window_is_narrow() {
        assert_eq!(clamp_editor_width(px(900.), px(1000.)), px(800.));
    }

    #[test]
    fn editor_width_never_below_min_editor() {
        assert_eq!(clamp_editor_width(px(10.), px(1400.)), MIN_EDITOR_WIDTH);
    }

    #[test]
    fn editor_width_never_negative_in_tiny_window() {
        let w = clamp_editor_width(px(600.), px(300.));
        assert!(w >= px(0.) && w <= px(300.));
    }
}
