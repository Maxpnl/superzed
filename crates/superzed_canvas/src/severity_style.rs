//! Stile per gravita', uguale per le card delle notifiche e le righe delle fonti: sfondo
//! neutro, testo nei colori normali, barra verticale a sinistra nel colore d'accento del
//! tema; Critical ha anche lo sfondo d'errore e l'icona. Colori solo dal tema.
use gpui::{App, Div, Hsla, px};
use superzed_notifications::Severity;
use theme::StatusColors;
use ui::{ActiveTheme as _, Icon, IconName, IconSize, prelude::*};

/// Larghezza della barra d'accento a sinistra.
pub const ACCENT_BAR_WIDTH: f32 = 3.;

/// Accento della gravita' dal tema; `Note` (e nessuna gravita') non ne ha: bordo normale.
pub fn severity_accent(severity: Option<Severity>, status: &StatusColors) -> Option<Hsla> {
    match severity? {
        Severity::Critical | Severity::Error => Some(status.error),
        Severity::Warning => Some(status.warning),
        Severity::Info => Some(status.info),
        Severity::Note => None,
    }
}

/// Come disegnare una card o una riga di quella gravita'.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeverityStyle {
    pub accent: Option<Hsla>,
    pub background: Hsla,
    /// Icona nel colore d'accento (solo Critical).
    pub icon: Option<(IconName, Hsla)>,
}

impl SeverityStyle {
    /// `base`: lo sfondo neutro di chi disegna (`element_background` per le righe,
    /// `elevated_surface_background` per le card). Critical lo sostituisce con
    /// `error_background`, cosi' si distingue da Error.
    pub fn resolve(severity: Option<Severity>, base: Hsla, status: &StatusColors) -> Self {
        let accent = severity_accent(severity, status);
        let critical = severity == Some(Severity::Critical);
        Self {
            accent,
            background: if critical { status.error_background } else { base },
            icon: critical.then_some((IconName::XCircle, status.error)),
        }
    }

    pub fn new(severity: Option<Severity>, base: Hsla, cx: &App) -> Self {
        Self::resolve(severity, base, cx.theme().status())
    }

    /// Contenitore orizzontale con sfondo, bordo e (se c'e' un accento) la barra a
    /// sinistra; chi chiama aggiunge il contenuto come figlio (`flex_1`).
    pub fn frame(&self, cx: &App) -> Div {
        h_flex()
            .items_stretch()
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .bg(self.background)
            .when_some(self.accent, |d, accent| {
                d.child(div().flex_none().w(px(ACCENT_BAR_WIDTH)).bg(accent))
            })
    }

    /// Icona della gravita' (solo Critical), da mettere accanto al titolo.
    pub fn icon_element(&self) -> Option<Icon> {
        self.icon.map(|(name, color)| Icon::new(name).size(IconSize::Small).color(Color::Custom(color)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_come_from_the_theme_and_note_has_none() {
        let status = StatusColors::dark();
        assert_eq!(severity_accent(Some(Severity::Note), &status), None);
        assert_eq!(severity_accent(None, &status), None);
        assert_eq!(severity_accent(Some(Severity::Info), &status), Some(status.info));
        assert_eq!(severity_accent(Some(Severity::Warning), &status), Some(status.warning));
        assert_eq!(severity_accent(Some(Severity::Error), &status), Some(status.error));
        assert_eq!(severity_accent(Some(Severity::Critical), &status), Some(status.error));
    }

    #[test]
    fn critical_differs_from_error() {
        let mut status = StatusColors::dark();
        status.error_background = status.error.opacity(0.2);
        let base = gpui::black();
        let error = SeverityStyle::resolve(Some(Severity::Error), base, &status);
        let critical = SeverityStyle::resolve(Some(Severity::Critical), base, &status);
        assert_ne!(error, critical);
        assert_eq!((error.background, error.icon), (base, None), "Error: sfondo neutro, niente icona");
        assert_eq!(critical.background, status.error_background);
        assert_eq!(critical.icon, Some((IconName::XCircle, status.error)));
        let note = SeverityStyle::resolve(Some(Severity::Note), base, &status);
        assert_eq!(note, SeverityStyle { accent: None, background: base, icon: None });
        assert_eq!(SeverityStyle::resolve(None, base, &status), note, "senza gravita' come Note");
    }
}
