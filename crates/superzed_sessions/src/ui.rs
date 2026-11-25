//! Icone di stato delle sessioni, usate dal titolo dei nodi e dalla vista Sessioni, e
//! colori delle percentuali rimanenti.
use crate::model::{InputKind, Session, SessionState};
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

/// Icona di stato di una sessione: come [`status_icon`], ma una chat ferma con cron o wakeup
/// in attesa ha l'orologio verde al posto del vuoto di `Idle`.
pub fn session_status_icon(session: &Session, cx: &App) -> Option<AnyElement> {
    if session.state == SessionState::Idle && !session.scheduled.is_empty() {
        return Some(Icon::new(IconName::Clock).size(IconSize::Small).color(Color::Custom(scheduled_color(cx))).into_any_element());
    }
    status_icon(&session.state, cx)
}

/// Tooltip dell'icona di stato di una sessione: [`status_tooltip`] piu' i cron in attesa
/// quando la chat e' ferma (verde, `Session::waiting_on_schedule`).
pub fn session_tooltip(session: &Session, now_ms: i64) -> Option<String> {
    let base = status_tooltip(&session.state, session.background_tasks);
    if !session.waiting_on_schedule() {
        return base;
    }
    let scheduled = scheduled_text(session.scheduled.len(), session.next_scheduled_fire(now_ms).map(|at| at - now_ms));
    Some(match base {
        Some(base) => format!("{base} · {scheduled}"),
        None => scheduled,
    })
}

/// «2 scheduled tasks, next in 5 min». `next_in_ms`: fra quanto scatta il primo, se si sa.
pub fn scheduled_text(count: usize, next_in_ms: Option<i64>) -> String {
    let tasks = if count == 1 { "1 scheduled task".to_string() } else { format!("{count} scheduled tasks") };
    let Some(next_in_ms) = next_in_ms else {
        return tasks;
    };
    let minutes = (next_in_ms + 59_999) / 60_000;
    let next = if minutes <= 1 {
        "under a minute".to_string()
    } else if minutes < 60 {
        format!("{minutes} min")
    } else if minutes < 48 * 60 {
        format!("{} h", minutes / 60)
    } else {
        format!("{} d", minutes / (24 * 60))
    };
    format!("{tasks}, next in {next}")
}

/// «2 subagents running».
pub fn subagents_text(count: usize) -> String {
    if count == 1 { "1 subagent running".to_string() } else { format!("{count} subagents running") }
}

/// Icona e conteggio, come l'indicatore dei file non committati: chi lo usa aggiunge id,
/// tooltip e selettore.
pub fn count_chip(icon: IconName, count: usize, color: Color) -> gpui::Div {
    h_flex()
        .flex_none()
        .gap_0p5()
        .child(Icon::new(icon).size(IconSize::XSmall).color(color))
        .child(Label::new(count.to_string()).size(LabelSize::Small).color(color))
}

/// Verde di una chat ferma con cron o wakeup in attesa: si risveglia da sola. Distinto dal
/// ciano del lavoro e dall'ambra dell'attesa dell'utente.
pub fn scheduled_color(cx: &App) -> Hsla {
    cx.theme().colors().terminal_ansi_green
}

/// Colore di una chat al lavoro: ciano del tema, distinto dal blu della selezione,
/// dall'ambra di "aspetta l'utente" e dal verde di "fatto".
pub fn working_color(cx: &App) -> Hsla {
    cx.theme().colors().terminal_ansi_cyan
}

/// Pillola «Working» con un punto fermo, per la vista d'insieme dei nodi al posto
/// dell'icona di stato.
pub fn working_pill(cx: &App) -> AnyElement {
    state_pill("Working", working_color(cx))
}

/// Pillola «Scheduled» verde: chat ferma con cron o wakeup in attesa, nella vista d'insieme.
pub fn scheduled_pill(cx: &App) -> AnyElement {
    state_pill("Scheduled", scheduled_color(cx))
}

fn state_pill(label: &'static str, color: Hsla) -> AnyElement {
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
        .child(Label::new(label).size(LabelSize::Small).color(Color::Custom(color)))
        .into_any_element()
}

/// Testo del tooltip dell'icona di stato (`Idle` non ha icona). `background_tasks`:
/// lavoro in background che claude sta aspettando (vedi `Session::background_tasks`).
pub fn status_tooltip(state: &SessionState, background_tasks: usize) -> Option<String> {
    Some(match state {
        SessionState::Working if background_tasks == 1 => "Claude is waiting for 1 background task".into(),
        SessionState::Working if background_tasks > 1 => format!("Claude is waiting for {background_tasks} background tasks"),
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
    use super::{context_label, pct_color, scheduled_text, session_tooltip, status_icon, status_tooltip, subagents_text};
    use crate::model::{InputKind, ScheduledTask, Session, SessionState};
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
        assert_eq!(status_tooltip(&SessionState::Working, 0).as_deref(), Some("Claude is working"));
        assert_eq!(
            status_tooltip(&SessionState::Working, 2).as_deref(),
            Some("Claude is waiting for 2 background tasks")
        );
        assert_eq!(
            status_tooltip(&SessionState::NeedsInput(InputKind::Other), 0).as_deref(),
            Some("Claude is waiting for a reply")
        );
        assert_eq!(
            status_tooltip(&SessionState::NeedsInput(InputKind::Question), 0).as_deref(),
            Some("Claude asked you a question")
        );
        assert_eq!(
            status_tooltip(&SessionState::NeedsInput(InputKind::Permission), 0).as_deref(),
            Some("Claude is asking for permission")
        );
        assert_eq!(status_tooltip(&SessionState::Ended, 0).as_deref(), Some("Session ended"));
        assert_eq!(status_tooltip(&SessionState::Starting, 0).as_deref(), Some("Starting…"));
        assert_eq!(status_tooltip(&SessionState::Failed("boom".into()), 0).as_deref(), Some("boom"));
        assert_eq!(status_tooltip(&SessionState::Idle, 0), None);
    }

    #[gpui::test]
    fn ended_and_starting_have_an_icon(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert!(status_icon(&SessionState::Ended, cx).is_some(), "Ended: icona");
            assert!(status_icon(&SessionState::Starting, cx).is_some(), "Starting: icona");
            assert!(status_icon(&SessionState::Idle, cx).is_none(), "Idle: nessuna icona");
        });
    }

    #[test]
    fn scheduled_and_subagent_texts() {
        assert_eq!(scheduled_text(1, None), "1 scheduled task");
        assert_eq!(scheduled_text(2, Some(30_000)), "2 scheduled tasks, next in under a minute");
        assert_eq!(scheduled_text(2, Some(4 * 60_000 + 1)), "2 scheduled tasks, next in 5 min");
        assert_eq!(scheduled_text(1, Some(3 * 3_600_000)), "1 scheduled task, next in 3 h");
        assert_eq!(scheduled_text(1, Some(3 * 86_400_000)), "1 scheduled task, next in 3 d");
        assert_eq!(subagents_text(1), "1 subagent running");
        assert_eq!(subagents_text(3), "3 subagents running");
    }

    #[test]
    fn session_tooltip_adds_the_schedule_only_when_the_chat_is_still() {
        let mut session = Session::new("a".into(), None);
        session.scheduled.push(ScheduledTask { id: "c1".into(), next_fire_ms: Some(10 * 60_000), expires_ms: i64::MAX });
        session.set_state(SessionState::Idle);
        assert_eq!(session_tooltip(&session, 0).as_deref(), Some("1 scheduled task, next in 10 min"));
        session.set_state(SessionState::Done);
        assert_eq!(session_tooltip(&session, 0).as_deref(), Some("Claude is done · 1 scheduled task, next in 10 min"));
        session.set_state(SessionState::Working);
        assert_eq!(session_tooltip(&session, 0).as_deref(), Some("Claude is working"));
        session.scheduled.clear();
        session.set_state(SessionState::Idle);
        assert_eq!(session_tooltip(&session, 0), None);
    }
}
