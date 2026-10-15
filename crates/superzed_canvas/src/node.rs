use gpui::{Action, AnyView, App, FocusHandle, SharedString};

/// Contenuto di un nodo del canvas. Il frame (posizione, titolo, colore) sta nel
/// modello; qui c'e' solo cio' che dipende dal tipo di nodo.
pub trait NodeContent {
    fn view(&self) -> AnyView;
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    /// Zoom del font del nodo: lo zoom del canvas (non c'e' zoom del solo nodo).
    fn set_font_zoom(&self, zoom: f32, cx: &mut App);
    /// Dati salvati in `NodeRecord::data`.
    fn save_data(&self, _cx: &App) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// Voci agacmeve del menu contestuale del nodo.
    fn context_menu_entries(&self) -> Vec<(SharedString, Box<dyn Action>)> {
        Vec::new()
    }
    /// Chiamato prima che il nodo venga tolto dal canvas (es. termina i processi).
    fn on_close(&self, _cx: &mut App) {}
}
