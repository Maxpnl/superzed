use editor::Editor;
use gpui::{
    Action, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, Pixels, Render, Window, px,
};
use superzed_actions::SidebarView;
use ui::{IconButton, IconName, IconSize, Label, LabelSize, Tooltip, prelude::*};
use workspace::{
    DeploySearch,
    dock::{DockPosition, Panel, PanelEvent},
};

/// Azione di ricerca per la query; `None` se la query e' vuota.
pub fn deploy_action(query: &str, case_sensitive: bool, whole_word: bool, regex: bool) -> Option<DeploySearch> {
    let query = query.trim();
    (!query.is_empty()).then(|| DeploySearch {
        query: Some(query.to_string()),
        case_sensitive: Some(case_sensitive),
        whole_word: Some(whole_word),
        regex: Some(regex),
        ..Default::default()
    })
}

/// Vista Ricerca: campo e opzioni nella sidebar; i risultati si aprono nella
/// ricerca di progetto di Zed, nell'area editor.
pub struct SearchPanel {
    query: Entity<Editor>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
}

impl SearchPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search the project…", window, cx);
            editor
        });
        Self { query, case_sensitive: false, whole_word: false, regex: false }
    }

    fn search(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.query.read(cx).text(cx);
        if let Some(action) = deploy_action(&text, self.case_sensitive, self.whole_word, self.regex) {
            window.dispatch_action(Box::new(action), cx);
        }
    }

    fn toggle(&self, id: &'static str, icon: IconName, tip: &'static str, on: bool,
              f: fn(&mut Self), cx: &mut Context<Self>) -> IconButton {
        IconButton::new(id, icon)
            .icon_size(IconSize::Small)
            .toggle_state(on)
            .tooltip(Tooltip::text(tip))
            .on_click(cx.listener(move |this, _, _, cx| {
                f(this);
                cx.notify();
            }))
    }
}

impl EventEmitter<PanelEvent> for SearchPanel {}

impl Focusable for SearchPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let border = if self.query.focus_handle(cx).is_focused(window) { colors.border_focused } else { colors.border };
        v_flex()
            .key_context("SuperzedSearch")
            .on_action(cx.listener(Self::search))
            .size_full()
            .p_2()
            .gap_2()
            .child(
                div()
                    .px_1()
                    .py_0p5()
                    .border_1()
                    .border_color(border)
                    .rounded_sm()
                    .child(self.query.clone()),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(self.toggle("superzed-search-case", IconName::CaseSensitive, "Match case", self.case_sensitive, |t| t.case_sensitive = !t.case_sensitive, cx))
                    .child(self.toggle("superzed-search-word", IconName::WholeWord, "Whole word", self.whole_word, |t| t.whole_word = !t.whole_word, cx))
                    .child(self.toggle("superzed-search-regex", IconName::Regex, "Regular expression", self.regex, |t| t.regex = !t.regex, cx)),
            )
            .child(Label::new("Enter: results in the editor area").size(LabelSize::Small).color(Color::Muted))
    }
}

impl Panel for SearchPanel {
    fn persistent_name() -> &'static str {
        "SuperzedSearchPanel"
    }
    fn panel_key() -> &'static str {
        "SuperzedSearchPanel"
    }
    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Left
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        position == DockPosition::Left
    }
    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}
    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(240.)
    }
    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::MagnifyingGlass)
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Search")
    }
    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(SidebarView(4))
    }
    fn activation_priority(&self) -> u32 {
        10
    }
}
