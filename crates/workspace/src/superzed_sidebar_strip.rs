// superzed: striscia di icone in cima al dock sinistro (le viste della sidebar)
use super::Dock;
use gpui::{AnyElement, Context, IntoElement, SharedString, Window};
use ui::{CountBadge, IconButton, IconSize, Tooltip, prelude::*};

impl Dock {
    pub(super) fn superzed_render_strip(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_panel_index;
        let buttons = self
            .panel_entries
            .iter()
            .enumerate()
            .filter_map(|(ix, entry)| {
                let icon = entry.panel.icon(window, cx)?;
                let tooltip = SharedString::from(entry.panel.icon_tooltip(window, cx).unwrap_or_default());
                let action = entry.panel.toggle_action(window, cx);
                // badge come nel dock di upstream (es. notifiche nuove)
                let count = entry.panel.icon_label(window, cx).and_then(|l| l.parse::<usize>().ok());
                Some(
                    div()
                        .relative()
                        .debug_selector(move || format!("superzed-sidebar-strip-{ix}"))
                        .child(
                            IconButton::new(("superzed-sidebar-view", ix), icon)
                                .icon_size(IconSize::Small)
                                .toggle_state(active == Some(ix))
                                .tooltip(move |_, cx| Tooltip::for_action(tooltip.clone(), &*action, cx))
                                .on_click(cx.listener(move |dock, _, window, cx| {
                                    dock.activate_panel(ix, window, cx);
                                    dock.set_open(true, window, cx);
                                    if let Some(entry) = dock.panel_entries.get(ix) {
                                        let focus = entry.panel.panel_focus_handle(cx);
                                        window.focus(&focus, cx);
                                    }
                                })),
                        )
                        .when_some(count, |d, count| d.child(CountBadge::new(count))),
                )
            })
            .collect::<Vec<_>>();
        h_flex()
            .flex_none()
            .w_full()
            .h(rems(1.875))
            .px_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .children(buttons)
            .into_any_element()
    }
}
