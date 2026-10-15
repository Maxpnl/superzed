use crate::node::NodeContent;
use editor::Editor;
use gpui::{AnyView, App, AppContext as _, Entity, FocusHandle, Focusable, TextStyleRefinement, Window};
use settings::Settings as _;
use theme_settings::ThemeSettings;

/// Nodo nota: un editor multilinea. Serve anche a verificare il modello a nodi
/// con un tipo non terminale.
pub struct NoteNode {
    editor: Entity<Editor>,
}

impl NoteNode {
    pub fn new(text: &str, window: &mut Window, cx: &mut App) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::multi_line(window, cx);
            // gutter e scrollbar si misurano nel rem dell'interfaccia, non seguono lo zoom
            // del canvas come il font: a zoom basso coprirebbero il testo
            editor.set_show_gutter(false, cx);
            editor.disable_scrollbars_and_minimap(window, cx);
            editor.set_soft_wrap();
            editor.set_text(text, window, cx);
            editor
        });
        Self { editor }
    }

    pub fn from_data(data: &serde_json::Value, window: &mut Window, cx: &mut App) -> Self {
        let text = data.get("text").and_then(|t| t.as_str()).unwrap_or("");
        Self::new(text, window, cx)
    }

    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }
}

impl NodeContent for NoteNode {
    fn view(&self) -> AnyView {
        self.editor.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }

    fn set_font_zoom(&self, zoom: f32, cx: &mut App) {
        let base = ThemeSettings::get_global(cx).buffer_font_size(cx);
        self.editor.update(cx, |editor, cx| {
            editor.set_text_style_refinement(TextStyleRefinement {
                font_size: Some((base * zoom).into()),
                ..Default::default()
            });
            cx.notify();
        });
    }

    fn save_data(&self, cx: &App) -> serde_json::Value {
        serde_json::json!({ "text": self.editor.read(cx).text(cx) })
    }
}
