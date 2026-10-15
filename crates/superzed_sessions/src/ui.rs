//! Icone di stato delle sessioni, usate dal titolo dei nodi e dalla vista Sessioni, e
//! colori delle percentuali rimanenti.
use crate::model::{InputKind, SessionState};
use gpui::{AnyElement, App, Hsla};
use ui::{Color, Icon, IconName, IconSize, prelude::*};

pub fn status_icon(state: &SessionState, cx: &App) -> Option<AnyElement> {
    Some(match state {
        SessionState::NeedsInput(_) => Icon::new(IconName::BellRing).size(IconSize::Medium).color(Color::Warning).into_any_element(),
        SessionState::Working => {
            Icon::new(IconName::LoadCircle).size(IconSize::Small).color(Color::Custom(working_color(cx))).into_any_element()
        }
        SessionState::Done => Icon::new(IconName::Check).size(IconSize::Small).color(Color::Success).into_any_element(),
        SessionState::Failed(_) => Icon::new(IconName::Warning).size(IconSize::Small).color(Color::Error).into_any_element(),
        SessionState::Starting => Icon::new(IconName::LoadCircle).size(IconSize::Small).color(Color::Muted).into_any_element(),
        SessionState::Ended => Icon::new(IconName::Stop).size(IconSize::Small).color(Color::Disabled).into_any_element(),
        SessionState::Idle => return None,
    })
}

/// Colore di una chat al lavoro: ciano del tema, distinto dal blu della selezione,
/// dall'ambra di "aspetta l'utente" e dal verde di "fatto".
pub fn working_color(cx: &App) -> Hsla {
    cx.theme().colors().terminal_ansi_cyan
}

/// Pillola «Working» con un punto fermo, per la vista d'insieme dei nodi al posto
/// dell'icona di stato.
pub fn working_pill(cx: &App) -> AnyElement {
    let color = working_color(cx);
    let dot = div().flex_none().size(rems(0.5)).rounded_full().bg(color);
    h_flex()
        .flex_none()
        .gap(rems(0.375))
        .px(rems(0.5))
        .py(rems(0.125))
        .rounded_full()
        .border_1()
        .border_color(color.opacity(0.35))
        .bg(color.opacity(0.12))
        .child(dot)
        .child(Label::new("Working").size(LabelSize::Small).color(Color::Custom(color)))
        .into_any_element()
}

/// Testo del tooltip dell'icona di stato (`Idle` non ha icona).
pub fn status_tooltip(state: &SessionState) -> Option<String> {
    Some(match state {
        SessionState::NeedsInput(InputKind::Question) => "Claude asked you a question".into(),
        SessionState::NeedsInput(InputKind::Permission) => "Claude is asking for permission".into(),
        SessionState::NeedsInput(InputKind::Other) => "Claude is waiting for a reply".into(),
        SessionState::Working => "Claude is working".into(),
        SessionState::Done => "Claude is done".into(),
        SessionState::Failed(err) => err.clone(),
        SessionState::Starting => "Starting…".into(),
        SessionState::Ended => "Session ended".into(),
        SessionState::Idle => return None,
    })
}

/// Colore di una percentuale rimanente (contesto, quote): `Error` sotto il 10%,
/// `Warning` sotto il 20%, altrimenti `Default` (chi la mostra attenuata la mappa lui).
pub fn pct_color(left: u8) -> Color {
    if left < 10 {
        Color::Error
    } else if left < 20 {
        Color::Warning
    } else {
        Color::Default
    }
}

/// `ctx NN%`, con le soglie di [`pct_color`]; attenuato quando non e' in allerta.
pub fn context_label(left: Option<u8>) -> Option<(String, Color)> {
    let left = left?;
    let color = match pct_color(left) {
        Color::Default => Color::Muted,
        alert => alert,
    };
    Some((format!("ctx {left}%"), color))
}

#[cfg(test)]
mod tests {
    use super::{context_label, pct_color, status_icon, status_tooltip};
    use crate::model::{InputKind, SessionState};
    use ui::Color;

    #[test]
    fn pct_color_thresholds() {
        assert_eq!(pct_color(0), Color::Error);
        assert_eq!(pct_color(9), Color::Error);
        assert_eq!(pct_color(10), Color::Warning);
        assert_eq!(pct_color(19), Color::Warning);
        assert_eq!(pct_color(20), Color::Default);
        assert_eq!(pct_color(100), Color::Default);
        // il contesto usa le stesse soglie, attenuato quando non e' in allerta
        assert_eq!(context_label(Some(9)).map(|(_, c)| c), Some(Color::Error));
        assert_eq!(context_label(Some(15)).map(|(_, c)| c), Some(Color::Warning));
        assert_eq!(context_label(Some(50)).map(|(_, c)| c), Some(Color::Muted));
    }

    #[test]
    fn status_tooltips() {
        assert_eq!(status_tooltip(&SessionState::Working).as_deref(), Some("Claude is working"));
        assert_eq!(
            status_tooltip(&SessionState::NeedsInput(InputKind::Other)).as_deref(),
            Some("Claude is waiting for a reply")
        );
        assert_eq!(
            status_tooltip(&SessionState::NeedsInput(InputKind::Question)).as_deref(),
            Some("Claude asked you a question")
        );
        assert_eq!(
            status_tooltip(&SessionState::NeedsInput(InputKind::Permission)).as_deref(),
            Some("Claude is asking for permission")
        );
        assert_eq!(status_tooltip(&SessionState::Ended).as_deref(), Some("Session ended"));
        assert_eq!(status_tooltip(&SessionState::Starting).as_deref(), Some("Starting…"));
        assert_eq!(status_tooltip(&SessionState::Failed("boom".into())).as_deref(), Some("boom"));
        assert_eq!(status_tooltip(&SessionState::Idle), None);
    }

    #[gpui::test]
    fn ended_and_starting_have_an_icon(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(status_icon(&SessionState::Ended, cx).is_some(), "Ended: icona");
            assert!(status_icon(&SessionState::Starting, cx).is_some(), "Starting: icona");
            assert!(status_icon(&SessionState::Idle, cx).is_none(), "Idle: nessuna icona");
        });
    }
}
