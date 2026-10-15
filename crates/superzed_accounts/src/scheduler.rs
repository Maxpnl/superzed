//! Scelta dell'account per una chat nuova, secondo la strategia del registro
//! (`"scheduler"` in accounts.json): `weekly` (default) non spreca la settimana,
//! `headroom` prende il margine minimo, `session` sfrutta le 5h in scadenza. Puro.
use crate::quota::{Limit, Quota};
use chrono::{DateTime, Utc};
use superzed_sessions::AccountLoad;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strategy {
    /// Quota settimanale rimanente per vicinanza al reset (x2), piu' il rimanente 5h.
    #[default]
    Weekly,
    /// Margine minimo tra 5h e settimana: lo scheduler originale.
    Headroom,
    /// Vicinanza al reset delle 5h (x1,5), piu' il rimanente 5h.
    Session,
}

impl Strategy {
    pub fn name(self) -> &'static str {
        match self {
            Strategy::Weekly => "weekly",
            Strategy::Headroom => "headroom",
            Strategy::Session => "session",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "weekly" => Some(Strategy::Weekly),
            "headroom" => Some(Strategy::Headroom),
            "session" => Some(Strategy::Session),
            _ => None,
        }
    }
}

/// Un limite a questa percentuale (non ancora resettato) esclude l'account.
pub const EXCLUDE_AT: f32 = 99.0;
/// Rimanente di un limite sconosciuto ("medio").
pub const UNKNOWN_REMAINING: f32 = 50.0;
/// Punti di punteggio tolti per ogni punto di carico.
pub const LOAD_WEIGHT: f32 = 10.0;
/// `weekly`: punti per ogni punto di pressione settimanale, il fattore principale.
pub const WEEKLY_WEIGHT: f32 = 2.0;
/// `session`: punti per ogni punto di urgenza del reset delle 5h.
pub const SESSION_WEIGHT: f32 = 1.5;
/// Sotto questo rimanente 5h pressione e urgenza non contano: l'account non si spreme.
pub const LOW_SESSION: f32 = 20.0;
/// Durata della finestra settimanale: a questa distanza dal reset la pressione e' zero.
pub const WEEK_HOURS: f32 = 7.0 * 24.0;
/// Durata della finestra di sessione: a questa distanza dal reset l'urgenza e' zero.
pub const SESSION_HOURS: f32 = 5.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub quota: Option<Quota>,
    pub load: AccountLoad,
    /// `false`: nessun token nel Keychain o login revocato (401/403): la chat chiederebbe il login.
    pub usable: bool,
    /// Escluso dalla scelta automatica nella cartella del progetto (`exclude_from`); a mano va bene.
    pub excluded_here: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub id: String,
    /// Quota settimanale che andrebbe persa: rimanente per vicinanza al reset, 0..=100
    /// (calcolata per ogni strategia, conta solo in `weekly`).
    pub pressure: f32,
    /// Rimanente delle 5h, 0..=100.
    pub session: f32,
    pub load: u32,
    pub score: f32,
    pub excluded: bool,
    pub excluded_here: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub id: String,
    /// Tutti esclusi: si usa il sistema (il chiamante avvisa).
    pub all_excluded: bool,
}

pub fn load_points(l: AccountLoad) -> u32 {
    3 * l.working + l.recent
}

fn remaining(l: Option<&Limit>, now: DateTime<Utc>) -> f32 {
    l.map(|l| l.remaining(now)).unwrap_or(UNKNOWN_REMAINING)
}

fn at_limit(l: Option<&Limit>, now: DateTime<Utc>) -> bool {
    l.is_some_and(|l| !l.is_reset(now) && l.used_percent >= EXCLUDE_AT)
}

/// Quanto e' vicino il reset, 0 a `window_hours` di distanza e 1 al reset; 0 per una
/// finestra sconosciuta o gia' resettata.
fn closeness(limit: Option<&Limit>, window_hours: f32, now: DateTime<Utc>) -> f32 {
    let Some(resets_at) = limit.and_then(|l| l.resets_at).filter(|r| *r > now) else {
        return 0.;
    };
    let hours_left = (resets_at - now).num_seconds() as f32 / 3600.;
    (1. - hours_left / window_hours).clamp(0., 1.)
}

/// Una settimana sconosciuta, gia' resettata o appena iniziata non ha nulla da perdere.
fn weekly_pressure(weekly: Option<&Limit>, now: DateTime<Utc>) -> f32 {
    remaining(weekly, now) * closeness(weekly, WEEK_HOURS, now)
}

pub fn score_all(strategy: Strategy, candidates: &[Candidate], now: DateTime<Utc>) -> Vec<Scored> {
    candidates
        .iter()
        .map(|c| {
            let (session, weekly) = match &c.quota {
                Some(q) => (q.session.as_ref(), q.weekly.as_ref()),
                None => (None, None),
            };
            let session_remaining = remaining(session, now);
            let dry = session_remaining < LOW_SESSION;
            let pressure = if dry { 0. } else { weekly_pressure(weekly, now) };
            let load = load_points(c.load);
            let base = match strategy {
                Strategy::Weekly => WEEKLY_WEIGHT * pressure + session_remaining,
                Strategy::Headroom => session_remaining.min(remaining(weekly, now)),
                Strategy::Session => {
                    let urgency = if dry { 0. } else { 100. * closeness(session, SESSION_HOURS, now) };
                    SESSION_WEIGHT * urgency + session_remaining
                }
            };
            Scored {
                id: c.id.clone(),
                pressure,
                session: session_remaining,
                load,
                score: base - LOAD_WEIGHT * load as f32,
                excluded: !c.usable || at_limit(session, now) || at_limit(weekly, now),
                excluded_here: c.excluded_here,
            }
        })
        .collect()
}

/// Il punteggio piu' alto tra i non esclusi; a parita' il primo (sistema, poi registro).
pub fn pick(strategy: Strategy, candidates: &[Candidate], now: DateTime<Utc>) -> Pick {
    let mut best: Option<&Scored> = None;
    let scored = score_all(strategy, candidates, now);
    for s in scored.iter().filter(|s| !s.excluded && !s.excluded_here) {
        if best.is_none_or(|b| s.score > b.score) {
            best = Some(s);
        }
    }
    match best {
        Some(b) => Pick { id: b.id.clone(), all_excluded: false },
        None => Pick { id: crate::SYSTEM_ID.to_string(), all_excluded: true },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::Limit;
    use chrono::TimeZone as _;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 27, 10, 0, 0).unwrap()
    }

    fn limit(used: f32, resets_in_h: i64) -> Limit {
        Limit { used_percent: used, resets_at: Some(now() + chrono::Duration::hours(resets_in_h)), seen_at: now() }
    }

    /// `session`/`weekly`: (percentuale usata, ore al reset).
    fn cand(id: &str, session: Option<(f32, i64)>, weekly: Option<(f32, i64)>, working: u32, recent: u32) -> Candidate {
        Candidate {
            id: id.into(),
            quota: Some(Quota {
                session: session.map(|(u, h)| limit(u, h)),
                weekly: weekly.map(|(u, h)| limit(u, h)),
                ..Default::default()
            }),
            load: AccountLoad { working, recent },
            usable: true,
            excluded_here: false,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn weekly_pressure_is_the_remaining_quota_times_the_closeness_to_the_reset() {
        // 100% della settimana che si resetta tra 24h su 168: pressione 100 * (144/168)
        let s = score_all(Strategy::Weekly, &[cand("a1", Some((40., 2)), Some((0., 24)), 0, 0)], now());
        assert!(close(s[0].pressure, 85.7), "{}", s[0].pressure);
        assert_eq!(s[0].session, 60.);
        assert!(close(s[0].score, WEEKLY_WEIGHT * 85.7 + 60.), "{}", s[0].score);
        // meta' settimana rimasta a 3 giorni dal reset: 40 * (96/168)
        let s = score_all(Strategy::Weekly, &[cand("a1", Some((0., 2)), Some((60., 72)), 0, 0)], now());
        assert!(close(s[0].pressure, 22.9), "{}", s[0].pressure);
    }

    #[test]
    fn a_fresh_or_unknown_week_has_no_pressure() {
        let fresh = cand("a1", Some((0., 2)), Some((0., 168)), 0, 0);
        let unknown = cand("a2", Some((0., 2)), None, 0, 0);
        let mut no_quota = cand("a3", None, None, 0, 0);
        no_quota.quota = None;
        let s = score_all(Strategy::Weekly, &[fresh, unknown, no_quota], now());
        assert_eq!(s.iter().map(|s| s.pressure).collect::<Vec<_>>(), [0., 0., 0.]);
        assert_eq!(s[0].score, 100.);
        assert_eq!(s[2].session, UNKNOWN_REMAINING, "sessione sconosciuta: 50");
        assert_eq!(s[2].score, UNKNOWN_REMAINING);
    }

    #[test]
    fn a_week_past_its_reset_has_no_pressure_and_is_not_excluded() {
        let s = score_all(Strategy::Weekly, &[cand("a1", Some((0., 2)), Some((100., -1)), 0, 0)], now());
        assert_eq!(s[0].pressure, 0.);
        assert!(!s[0].excluded, "la settimana e' gia' ripartita");
        assert_eq!(s[0].score, 100.);
    }

    #[test]
    fn a_low_session_drops_the_weekly_pressure() {
        let s = score_all(Strategy::Weekly, &[cand("a1", Some((90., 2)), Some((0., 24)), 0, 0)], now());
        assert_eq!(s[0].pressure, 0., "sotto il 20% rimanente le 5h non si spremono");
        assert_eq!(s[0].score, 10.);
        let s = score_all(Strategy::Weekly, &[cand("a1", Some((80., 2)), Some((0., 24)), 0, 0)], now());
        assert!(s[0].pressure > 0., "al 20% la pressione conta");
    }

    #[test]
    fn an_expiring_week_beats_a_fresh_one_unless_its_session_is_dry() {
        let expiring = cand("a1", Some((40., 2)), Some((0., 24)), 0, 0); // 2*85.7 + 60
        let mid_week = cand("a2", Some((0., 2)), Some((60., 72)), 0, 0); // 2*22.9 + 100
        let fresh = cand("a3", Some((0., 2)), Some((0., 168)), 0, 0); // 100
        let dry = cand("a4", Some((90., 2)), Some((0., 24)), 0, 0); // 10
        let all = vec![fresh.clone(), mid_week.clone(), expiring, dry.clone()];
        assert_eq!(pick(Strategy::Weekly, &all, now()), Pick { id: "a1".into(), all_excluded: false });
        assert_eq!(pick(Strategy::Weekly, &[fresh.clone(), mid_week], now()).id, "a2", "meglio finire una settimana a meta' che intaccarne una nuova");
        assert_eq!(pick(Strategy::Weekly, &[dry, fresh], now()).id, "a3");
    }

    #[test]
    fn load_costs_ten_points_per_point() {
        let mut busy = cand("a1", Some((40., 2)), Some((0., 24)), 1, 2);
        assert_eq!(load_points(busy.load), 5);
        let s = score_all(Strategy::Weekly, std::slice::from_ref(&busy), now());
        assert!(close(s[0].score, WEEKLY_WEIGHT * 85.7 + 60. - 50.), "{}", s[0].score);
        // due chat al lavoro sull'account in scadenza: la settimana nuova lo raggiunge
        busy.load = AccountLoad { working: 4, recent: 0 }; // 231 - 120 = 111
        let fresh = cand("a2", Some((0., 2)), Some((0., 168)), 0, 0); // 100
        assert_eq!(pick(Strategy::Weekly, &[busy.clone(), fresh.clone()], now()).id, "a1");
        busy.load = AccountLoad { working: 5, recent: 0 }; // 231 - 150 = 81
        assert_eq!(pick(Strategy::Weekly, &[busy, fresh], now()).id, "a2");
    }

    #[test]
    fn ties_go_to_the_system_then_registry_order() {
        let c = vec![cand("system", Some((10., 2)), None, 0, 0), cand("a1", Some((10., 2)), None, 0, 0)];
        assert_eq!(pick(Strategy::Weekly, &c, now()).id, "system");
        let c = vec![cand("system", Some((90., 2)), None, 0, 0), cand("a1", Some((10., 2)), None, 0, 0), cand("a2", Some((10., 2)), None, 0, 0)];
        assert_eq!(pick(Strategy::Weekly, &c, now()).id, "a1");
    }

    #[test]
    fn at_limit_or_without_token_is_excluded() {
        let mut c = vec![
            cand("system", Some((99., 2)), Some((10., 48)), 0, 0),
            cand("a1", Some((10., 2)), Some((99.5, 48)), 0, 0),
            cand("a2", Some((95., 2)), Some((95., 48)), 0, 0),
        ];
        let s = score_all(Strategy::Weekly, &c, now());
        assert!(s[0].excluded && s[1].excluded && !s[2].excluded);
        assert_eq!(pick(Strategy::Weekly, &c, now()), Pick { id: "a2".into(), all_excluded: false }, "anche con poco margine");
        c[2].usable = false; // nessun token nel Keychain
        assert_eq!(pick(Strategy::Weekly, &c, now()), Pick { id: "system".into(), all_excluded: true });
    }

    // Review Focus 4
    #[test]
    fn a_session_past_its_reset_is_not_excluded() {
        let mut stale = cand("a1", Some((100., 2)), Some((10., 48)), 0, 0);
        stale.quota.as_mut().unwrap().session = Some(limit(100., -1)); // reset un'ora fa
        let c = vec![cand("system", Some((50., 2)), Some((50., 48)), 0, 0), stale];
        let s = score_all(Strategy::Weekly, &c, now());
        assert!(!s[1].excluded);
        assert_eq!(s[1].session, 100., "5h piena dopo il reset");
        assert_eq!(pick(Strategy::Weekly, &c, now()).id, "a1");
    }

    #[test]
    fn an_account_excluded_here_is_skipped_but_keeps_its_score() {
        let mut here = cand("a1", Some((0., 2)), Some((0., 24)), 0, 0);
        here.excluded_here = true;
        let other = cand("a2", Some((50., 2)), Some((0., 168)), 0, 0);
        let s = score_all(Strategy::Weekly, &[here.clone(), other.clone()], now());
        assert!(!s[0].excluded && s[0].excluded_here && s[0].score > s[1].score);
        assert_eq!(pick(Strategy::Weekly, &[here.clone(), other], now()), Pick { id: "a2".into(), all_excluded: false });
        assert_eq!(pick(Strategy::Weekly, &[here], now()), Pick { id: "system".into(), all_excluded: true });
    }

    #[test]
    fn headroom_strategy_is_the_tighter_window_minus_load() {
        let s = score_all(Strategy::Headroom, &[cand("a1", Some((20., 2)), Some((70., 48)), 1, 2)], now());
        assert_eq!(s[0].score, 30. - 50.);
        let unknown = score_all(Strategy::Headroom, &[cand("a2", None, Some((10., 48)), 0, 0)], now());
        assert_eq!(unknown[0].score, 50., "finestra sconosciuta: 50");
    }

    #[test]
    fn session_strategy_weighs_the_5h_reset_and_drops_it_when_the_session_is_dry() {
        let soon = cand("a1", Some((40., 1)), Some((0., 168)), 0, 0); // 1.5 * 80 + 60
        let later = cand("a2", Some((20., 4)), Some((0., 168)), 0, 0); // 1.5 * 20 + 80
        let dry = cand("a3", Some((90., 1)), Some((0., 168)), 0, 0); // 0 + 10
        let s = score_all(Strategy::Session, &[soon.clone(), later.clone(), dry.clone()], now());
        assert!(close(s[0].score, 180.) && close(s[1].score, 110.) && close(s[2].score, 10.), "{s:?}");
        let fresh = cand("a4", Some((0., -1)), None, 0, 0);
        assert_eq!(score_all(Strategy::Session, &[fresh], now())[0].score, 100., "gia' resettata: nessuna urgenza");
        assert_eq!(pick(Strategy::Session, &[later, soon, dry], now()).id, "a1");
    }

    #[test]
    fn the_strategies_disagree_on_the_same_candidates() {
        let expiring_week = cand("a1", Some((40., 4)), Some((0., 24)), 0, 0);
        let fresh = cand("a2", Some((0., 1)), Some((0., 168)), 0, 0);
        let both = [expiring_week, fresh];
        assert_eq!(pick(Strategy::Weekly, &both, now()).id, "a1");
        assert_eq!(pick(Strategy::Headroom, &both, now()).id, "a2");
        assert_eq!(pick(Strategy::Session, &both, now()).id, "a2");
    }

    #[test]
    fn strategy_names_round_trip_and_default_to_weekly() {
        assert_eq!(Strategy::default(), Strategy::Weekly);
        for s in [Strategy::Weekly, Strategy::Headroom, Strategy::Session] {
            assert_eq!(Strategy::parse(s.name()), Some(s));
        }
        assert_eq!(Strategy::parse("boh"), None);
    }
}
