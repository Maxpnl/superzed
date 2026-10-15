//! Barre delle quote (componente condiviso fra status bar e sezione «Limits» del
//! pannello Sessions) e righe dei limiti per account: frazione rimanente, colore delle
//! soglie di `pct_color`, testo del reset e ordine degli account.
use crate::quota::{self, Limit};
use crate::{Account, AccountStatus, AccountStore};
use chrono::{DateTime, Utc};
use gpui::{App, Div, Hsla, Rems, relative};
use ui::prelude::*;

/// Frazione della barra: la percentuale rimanente in 0..=1 (senza dati: vuota).
pub fn bar_fraction(remaining: Option<u8>) -> f32 {
    remaining.map_or(0., |p| (f32::from(p) / 100.).clamp(0., 1.))
}

/// Colore del riempimento: le soglie di `pct_color` (normale verde, poi warning ed error).
pub fn bar_color(remaining: Option<u8>, cx: &App) -> Hsla {
    let status = cx.theme().status();
    match remaining.map(superzed_sessions::ui::pct_color) {
        Some(Color::Error) => status.error,
        Some(Color::Warning) => status.warning,
        _ => status.success,
    }
}

/// Opacita' del colore del testo per la traccia delle barre.
pub const TRACK_ALPHA: f32 = 0.25;

/// Traccia delle barre: il colore del testo del tema, trasparente. Contrasta sempre con i
/// fondi del pannello e della status bar, chiari o scuri (`element_background` nei temi
/// inclusi coincide con `panel_background` e la traccia spariva).
pub fn track_color(text: Hsla) -> Hsla {
    text.opacity(TRACK_ALPHA)
}

/// «NN%» col colore di `pct_color`, o «–» attenuato senza dati.
pub fn pct_label(remaining: Option<u8>) -> (String, Color) {
    match remaining {
        Some(p) => (format!("{p}%"), superzed_sessions::ui::pct_color(p)),
        None => ("–".to_string(), Color::Muted),
    }
}

/// Barra orizzontale: traccia arrotondata (`track_color`), riempita per la parte
/// rimanente.
pub fn meter(remaining: Option<u8>, width: Rems, height: Rems, cx: &App) -> Div {
    let fraction = bar_fraction(remaining);
    div()
        .flex_none()
        .w(width)
        .h(height)
        .rounded_full()
        .overflow_hidden()
        .bg(track_color(cx.theme().colors().text))
        .child(div().h_full().w(relative(fraction)).rounded_full().bg(bar_color(remaining, cx)))
}

/// La finestra piu' stretta (minimo fra quelle con dati: 5h, Week, e i limiti per modello).
pub fn tightest(values: &[Option<u8>]) -> Option<u8> {
    values.iter().flatten().copied().min()
}

/// Id stabile per gli occhielli di debug/tooltip a partire dal nome di un modello
/// (es. «Fable» -> "fable"); i caratteri non alfanumerici diventano un trattino.
pub fn slug(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect()
}

/// «in 1d 2h 35m» / «in 35m» / «in 2h 0m» / «in <1m» / «now»: minuti arrotondati per
/// eccesso; le unita' a zero si omettono solo in testa (giorni, poi ore), mai in coda.
pub fn countdown_phrase(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (resets_at - now).num_seconds();
    if seconds <= 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return "in <1m".to_string();
    }
    let minutes = (seconds + 59) / 60;
    let (days, hours, mins) = (minutes / (24 * 60), (minutes % (24 * 60)) / 60, minutes % 60);
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if days > 0 || hours > 0 {
        parts.push(format!("{hours}h"));
    }
    parts.push(format!("{mins}m"));
    format!("in {}", parts.join(" "))
}

/// Reset breve della sezione «Limits»: «1d 18h» / «2h 4m» / «35m» / «<1m» / «now».
/// Stesso arrotondamento per eccesso di `countdown_phrase`, al massimo due unita' a
/// partire dalla prima non nulla.
pub fn short_countdown(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (resets_at - now).num_seconds();
    if seconds <= 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return "<1m".to_string();
    }
    let minutes = (seconds + 59) / 60;
    let (days, hours, mins) = (minutes / (24 * 60), (minutes % (24 * 60)) / 60, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

/// «83% left» per il tooltip, o «–» senza dati.
pub fn left_label(remaining: Option<u8>) -> String {
    remaining.map_or_else(|| "–".to_string(), |p| format!("{p}% left"))
}

/// «Resets in 1d 2h 35m» (conto alla rovescia, Task 20) o «Resets now» se gia' passato;
/// niente senza reset o senza finestra.
pub fn reset_label(limit: Option<&Limit>, now: DateTime<Utc>) -> Option<String> {
    let resets_at = limit?.resets_at?;
    Some(format!("Resets {}", countdown_phrase(resets_at, now)))
}

/// Una finestra della riga dei limiti.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowMeter {
    pub remaining: Option<u8>,
    /// «Resets in 1d 2h 35m» (tooltip).
    pub reset: Option<String>,
    /// «1d 2h» (sezione «Limits», `short_countdown`).
    pub short_reset: Option<String>,
}

/// Riga dei limiti di un account.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitsRow {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub five: WindowMeter,
    pub week: WindowMeter,
    /// Limiti per modello (`weekly_scoped`), nell'ordine ricevuto da `oauth/usage`.
    pub scoped: Vec<(String, WindowMeter)>,
    /// Sloggato (`AccountStatus::needs_login`): le quote in cache non si mostrano (una
    /// finestra gia' resettata direbbe «100% now»), al loro posto la richiesta di login.
    pub login_needed: bool,
}

impl LimitsRow {
    pub fn has_data(&self) -> bool {
        self.five.remaining.is_some() || self.week.remaining.is_some() || self.scoped.iter().any(|(_, w)| w.remaining.is_some())
    }

    /// Le finestre in ordine (5h, Week, poi i limiti per modello): id stabile per i
    /// selettori («5h», «week», slug del modello), nome mostrato e valori.
    pub fn windows(&self) -> Vec<(String, String, &WindowMeter)> {
        let mut windows = vec![("5h".to_string(), "5h".to_string(), &self.five), ("week".to_string(), "Week".to_string(), &self.week)];
        windows.extend(self.scoped.iter().map(|(name, w)| (slug(name), name.clone(), w)));
        windows
    }

    pub fn tightest(&self) -> Option<u8> {
        let mut values = vec![self.five.remaining, self.week.remaining];
        values.extend(self.scoped.iter().map(|(_, w)| w.remaining));
        tightest(&values)
    }
}

fn window(limit: Option<&Limit>, now: DateTime<Utc>) -> WindowMeter {
    WindowMeter {
        remaining: quota::remaining_pct(limit, now),
        reset: reset_label(limit, now),
        short_reset: limit.and_then(|l| l.resets_at).map(|r| short_countdown(r, now)),
    }
}

pub fn limits_row(account: &Account, quota: Option<&crate::Quota>, status: &AccountStatus, now: DateTime<Utc>) -> LimitsRow {
    LimitsRow {
        id: account.id.clone(),
        label: account.label.clone(),
        email: account.email.clone(),
        five: window(quota.and_then(|q| q.session.as_ref()), now),
        week: window(quota.and_then(|q| q.weekly.as_ref()), now),
        scoped: quota
            .map(|q| q.scoped.iter().map(|(name, limit)| (name.clone(), window(Some(limit), now))).collect())
            .unwrap_or_default(),
        login_needed: status.needs_login(),
    }
}

/// Prima l'account con la percentuale rimanente minima; quelli senza dati in fondo e
/// quelli sloggati dopo ancora (ordine stabile: a parita' resta quello del registro).
pub fn sort_rows(rows: &mut [LimitsRow]) {
    rows.sort_by_key(|r| (r.login_needed, r.tightest().map_or(u16::MAX, u16::from)));
}

/// Righe dei limiti di tutti gli account dello store, ordinate.
pub fn limits_rows(store: &AccountStore) -> Vec<LimitsRow> {
    let now = store.now();
    let mut rows: Vec<LimitsRow> = store.accounts().iter().map(|a| limits_row(a, store.quota(&a.id), &store.status(&a.id), now)).collect();
    sort_rows(&mut rows);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Quota;
    use chrono::{Local, TimeZone as _};

    fn local(d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Local.with_ymd_and_hms(2026, 9, d, h, m, 0).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn the_bar_fills_for_the_remaining_fraction() {
        assert_eq!(bar_fraction(Some(63)), 0.63);
        assert_eq!(bar_fraction(Some(0)), 0.);
        assert_eq!(bar_fraction(Some(100)), 1.);
        assert_eq!(bar_fraction(Some(250)), 1., "clamp");
        assert_eq!(bar_fraction(None), 0., "senza dati: vuota");
    }

    // Final fix 6: la traccia si vede sui fondi del pannello e della status bar di tutti i
    // temi inclusi (`element_background` coincideva con `panel_background`).
    #[test]
    fn the_track_contrasts_with_panel_and_status_bar_in_every_bundled_theme() {
        let hex = |style: &serde_json::Value, key: &str| -> Hsla {
            let s = style[key].as_str().unwrap_or_else(|| panic!("{key}"));
            gpui::Rgba::try_from(s).unwrap().into()
        };
        let files = [
            include_str!("../../../assets/themes/one/one.json"),
            include_str!("../../../assets/themes/ayu/ayu.json"),
            include_str!("../../../assets/themes/gruvbox/gruvbox.json"),
        ];
        let mut seen = 0;
        for file in files {
            let family: serde_json::Value = serde_json::from_str(file).unwrap();
            for theme in family["themes"].as_array().unwrap() {
                let (name, style) = (theme["name"].as_str().unwrap(), &theme["style"]);
                let track = track_color(hex(style, "text"));
                for bg_key in ["panel.background", "status_bar.background"] {
                    let bg = hex(style, bg_key);
                    let ratio = ui::utils::calculate_contrast_ratio(bg.blend(track), bg);
                    assert!(ratio >= 1.35, "{name} {bg_key}: contrasto {ratio}");
                }
                seen += 1;
            }
        }
        assert!(seen >= 11, "temi letti: {seen}");
    }

    #[test]
    fn the_percentage_label_uses_the_thresholds_or_a_dash() {
        assert_eq!(pct_label(Some(63)), ("63%".to_string(), Color::Default));
        assert_eq!(pct_label(Some(19)), ("19%".to_string(), Color::Warning));
        assert_eq!(pct_label(Some(9)), ("9%".to_string(), Color::Error));
        assert_eq!(pct_label(None), ("–".to_string(), Color::Muted));
    }

    // Task 20: il reset come conto alla rovescia («reset in 1 giorno 2 ore 35 minuti»),
    // arrotondato per eccesso al minuto; le unita' a zero si omettono solo in testa.
    #[test]
    fn the_reset_label_is_a_countdown_rounded_up_to_the_minute() {
        use chrono::Duration as D;
        let now = local(27, 10, 0);
        let limit = |r: Option<DateTime<Utc>>| Limit { used_percent: 37., resets_at: r, seen_at: now };
        let at = |delta: D| now + delta;
        assert_eq!(
            reset_label(Some(&limit(Some(at(D::days(1) + D::hours(2) + D::minutes(35))))), now).as_deref(),
            Some("Resets in 1d 2h 35m")
        );
        assert_eq!(reset_label(Some(&limit(Some(at(D::minutes(35))))), now).as_deref(), Some("Resets in 35m"), "35 minuti: niente giorni/ore");
        assert_eq!(reset_label(Some(&limit(Some(at(D::hours(2))))), now).as_deref(), Some("Resets in 2h 0m"), "2 ore esatte");
        assert_eq!(reset_label(Some(&limit(Some(at(D::seconds(59))))), now).as_deref(), Some("Resets in <1m"), "meno di un minuto");
        assert_eq!(reset_label(Some(&limit(Some(at(D::seconds(-30))))), now).as_deref(), Some("Resets now"), "gia' passato");
        assert_eq!(
            reset_label(Some(&limit(Some(at(D::days(7) + D::hours(3))))), now).as_deref(),
            Some("Resets in 7d 3h 0m")
        );
        assert_eq!(
            reset_label(Some(&limit(Some(at(D::hours(2) + D::minutes(34) + D::seconds(10))))), now).as_deref(),
            Some("Resets in 2h 35m"),
            "arrotondamento per eccesso"
        );
        assert_eq!(reset_label(Some(&limit(None)), now), None, "reset assente");
        assert_eq!(reset_label(None, now), None, "finestra assente");
    }

    // Task 23: reset breve della sezione Limits, al massimo due unita', stesso
    // arrotondamento per eccesso di `countdown_phrase`.
    #[test]
    fn the_short_reset_keeps_at_most_two_units() {
        use chrono::Duration as D;
        let now = local(27, 10, 0);
        assert_eq!(short_countdown(now + D::days(1) + D::hours(18) + D::minutes(34), now), "1d 18h");
        assert_eq!(short_countdown(now + D::hours(2) + D::minutes(4), now), "2h 4m");
        assert_eq!(short_countdown(now + D::minutes(35), now), "35m");
        assert_eq!(short_countdown(now + D::hours(2), now), "2h 0m");
        assert_eq!(short_countdown(now + D::days(7), now), "7d 0h");
        assert_eq!(short_countdown(now + D::hours(2) + D::minutes(34) + D::seconds(10), now), "2h 35m", "per eccesso");
        assert_eq!(short_countdown(now + D::seconds(59), now), "<1m");
        assert_eq!(short_countdown(now - D::seconds(30), now), "now", "gia' passato");
        assert_eq!(short_countdown(now, now), "now");
    }

    #[test]
    fn a_window_carries_the_full_and_the_short_reset() {
        use chrono::Duration as D;
        let now = local(27, 10, 0);
        let limit = Limit { used_percent: 17., resets_at: Some(now + D::days(1) + D::hours(18) + D::minutes(34)), seen_at: now };
        let w = window(Some(&limit), now);
        assert_eq!(w.reset.as_deref(), Some("Resets in 1d 18h 34m"));
        assert_eq!(w.short_reset.as_deref(), Some("1d 18h"));
        assert_eq!(window(None, now).short_reset, None);
    }

    #[test]
    fn a_row_lists_its_windows_in_order_with_key_and_name() {
        let now = local(27, 10, 0);
        let acc = Account { id: "a1".into(), label: "a1".into(), email: None, config_dir: None, exclude_from: Vec::new() };
        let l = |u: f32| Limit { used_percent: u, resets_at: None, seen_at: now };
        let quota = Quota { session: Some(l(37.)), weekly: Some(l(20.)), scoped: vec![("Claude Fable".to_string(), l(9.))], ..Default::default() };
        let row = limits_row(&acc, Some(&quota), &AccountStatus::Ok, now);
        let windows: Vec<_> = row.windows().into_iter().map(|(key, name, w)| (key, name, w.remaining)).collect();
        assert_eq!(
            windows,
            [
                ("5h".to_string(), "5h".to_string(), Some(63)),
                ("week".to_string(), "Week".to_string(), Some(80)),
                ("claude-fable".to_string(), "Claude Fable".to_string(), Some(91)),
            ]
        );
    }

    #[test]
    fn the_status_bar_shows_the_tighter_window() {
        assert_eq!(tightest(&[Some(63), Some(80)]), Some(63));
        assert_eq!(tightest(&[Some(90), Some(12)]), Some(12));
        assert_eq!(tightest(&[None, Some(40)]), Some(40));
        assert_eq!(tightest(&[None, None]), None);
        // Task 19: un limite per modello (es. Fable) piu' stretto di 5h/Week vince.
        assert_eq!(tightest(&[Some(63), Some(80), Some(5)]), Some(5));
    }

    #[test]
    fn rows_put_the_lowest_remaining_first_and_no_data_last() {
        let now = local(27, 10, 0);
        let acc = |id: &str| Account { id: id.into(), label: id.into(), email: None, config_dir: None, exclude_from: Vec::new() };
        let q = |session: f32, weekly: f32| Quota {
            session: Some(Limit { used_percent: session, resets_at: None, seen_at: now }),
            weekly: Some(Limit { used_percent: weekly, resets_at: None, seen_at: now }),
            ..Default::default()
        };
        let mut rows = vec![
            limits_row(&acc("none"), None, &AccountStatus::Ok, now),
            limits_row(&acc("mid"), Some(&q(50., 10.)), &AccountStatus::Ok, now),
            limits_row(&acc("low"), Some(&q(10., 95.)), &AccountStatus::Ok, now),
            limits_row(&acc("high"), Some(&q(5., 5.)), &AccountStatus::Ok, now),
        ];
        sort_rows(&mut rows);
        let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["low", "mid", "high", "none"]);
        assert_eq!((rows[0].five.remaining, rows[0].week.remaining), (Some(90), Some(5)));
        assert!(!rows[3].has_data());
    }

    // Task 19: i limiti per modello (`weekly_scoped`, oggi "Fable") diventano finestre in
    // piu' nella riga, con la stessa forma di 5h/Week (rimanente, reset).
    #[test]
    fn scoped_limits_become_extra_windows_with_a_label() {
        let now = local(27, 10, 0);
        let acc = Account { id: "a1".into(), label: "a1".into(), email: None, config_dir: None, exclude_from: Vec::new() };
        let quota = Quota {
            session: Some(Limit { used_percent: 37., resets_at: None, seen_at: now }),
            weekly: Some(Limit { used_percent: 20., resets_at: None, seen_at: now }),
            scoped: vec![("Fable".to_string(), Limit { used_percent: 9., resets_at: Some(local(28, 9, 0)), seen_at: now })],
            ..Default::default()
        };
        let row = limits_row(&acc, Some(&quota), &AccountStatus::Ok, now);
        assert_eq!(row.scoped.len(), 1);
        let (name, window) = &row.scoped[0];
        assert_eq!(name, "Fable");
        assert_eq!(window.remaining, Some(91));
        assert_eq!(window.reset.as_deref(), Some("Resets in 23h 0m"), "27/09 10:00 -> 28/09 09:00");
        // il limite per modello (91% rimanente, quindi non il piu' stretto qui) non
        // cambia gli altri campi
        assert_eq!(row.five.remaining, Some(63));
        assert_eq!(row.week.remaining, Some(80));
        assert!(row.has_data());

        // senza dati per il modello: nessuna finestra extra
        let no_scoped = limits_row(&acc, Some(&Quota { scoped: Vec::new(), ..quota }), &AccountStatus::Ok, now);
        assert!(no_scoped.scoped.is_empty());
    }

    // Task 19: un limite per modello piu' stretto di 5h/Week vince nella riga e nell'ordine.
    #[test]
    fn a_tighter_model_limit_wins_the_row_and_the_order() {
        let now = local(27, 10, 0);
        let acc = |id: &str| Account { id: id.into(), label: id.into(), email: None, config_dir: None, exclude_from: Vec::new() };
        let wide = Quota {
            session: Some(Limit { used_percent: 10., resets_at: None, seen_at: now }),
            weekly: Some(Limit { used_percent: 10., resets_at: None, seen_at: now }),
            ..Default::default()
        };
        let tight_model = Quota {
            session: Some(Limit { used_percent: 10., resets_at: None, seen_at: now }),
            weekly: Some(Limit { used_percent: 10., resets_at: None, seen_at: now }),
            scoped: vec![("Fable".to_string(), Limit { used_percent: 97., resets_at: None, seen_at: now })],
            ..Default::default()
        };
        let mut rows = vec![limits_row(&acc("wide"), Some(&wide), &AccountStatus::Ok, now), limits_row(&acc("tight"), Some(&tight_model), &AccountStatus::Ok, now)];
        assert_eq!(rows[1].tightest(), Some(3), "Fable al 3% rimanente e' la finestra piu' stretta");
        sort_rows(&mut rows);
        let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["tight", "wide"], "il limite per modello piu' stretto passa avanti");
    }

    // Account sloggato (nessun token nel Keychain o login revocato): la riga chiede il
    // login al posto delle quote in cache (gia' resettate direbbero «100% now») e va in
    // fondo; un token scaduto resta usabile.
    #[test]
    fn a_logged_out_account_needs_login_and_goes_last() {
        use chrono::Duration as D;
        let now = local(27, 10, 0);
        let acc = |id: &str| Account { id: id.into(), label: id.into(), email: None, config_dir: None, exclude_from: Vec::new() };
        let stale = Quota {
            session: Some(Limit { used_percent: 90., resets_at: Some(now - D::hours(1)), seen_at: now - D::hours(6) }),
            weekly: Some(Limit { used_percent: 90., resets_at: Some(now - D::hours(1)), seen_at: now - D::hours(6) }),
            ..Default::default()
        };
        let live = Quota { session: Some(Limit { used_percent: 10., resets_at: None, seen_at: now }), ..Default::default() };
        let mut rows = vec![
            limits_row(&acc("out"), Some(&stale), &AccountStatus::NoToken, now),
            limits_row(&acc("revoked"), None, &AccountStatus::Unavailable, now),
            limits_row(&acc("in"), Some(&live), &AccountStatus::Ok, now),
            limits_row(&acc("expired"), Some(&live), &AccountStatus::TokenExpired, now),
        ];
        assert!(rows[0].login_needed && rows[1].login_needed);
        assert!(!rows[2].login_needed && !rows[3].login_needed, "token scaduto: l'account resta usabile");
        assert_eq!(rows[0].five.remaining, Some(100), "le quote in cache restano nella riga");
        assert_eq!(rows[0].five.short_reset.as_deref(), Some("now"));
        sort_rows(&mut rows);
        let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["in", "expired", "out", "revoked"], "gli sloggati in fondo, anche al 100%");
    }

    #[test]
    fn slug_lowercases_and_turns_non_alphanumeric_into_dashes() {
        assert_eq!(slug("Fable"), "fable");
        assert_eq!(slug("Claude Opus 4.5"), "claude-opus-4-5");
    }
}
