//! Eta' di una chat nella vista d'insieme: etichetta relativa («today 14:32», «3 weeks
//! ago») e dissolvenza a scalini per riconoscere a colpo d'occhio le chat vecchie.
use chrono::{DateTime, Local, TimeZone as _};

/// Giorni di calendario tra `then` e `now` (0 = oggi); il futuro conta come oggi.
fn days_between(then: i64, now: DateTime<Local>) -> (i64, DateTime<Local>) {
    let then = Local.timestamp_opt(then, 0).single().unwrap_or(now);
    let days = now.date_naive().signed_duration_since(then.date_naive()).num_days().max(0);
    (days, then)
}

/// «today 14:32», «yesterday 09:10», «2 days ago 18:05» (fino a 3 giorni con l'ora), poi
/// «5 days ago», «1 week ago», «3 weeks ago», «2 months ago», «1 year ago».
pub fn age_label(then: i64, now: DateTime<Local>) -> String {
    let (days, then) = days_between(then, now);
    let time = then.format("%H:%M");
    let plural = |n: i64, unit: &str| if n == 1 { format!("1 {unit} ago") } else { format!("{n} {unit}s ago") };
    match days {
        0 => format!("today {time}"),
        1 => format!("yesterday {time}"),
        2..=3 => format!("{days} days ago {time}"),
        4..=6 => plural(days, "day"),
        7..=30 => plural(days / 7, "week"),
        31..=364 => plural(days / 30, "month"),
        _ => plural(days / 365, "year"),
    }
}

/// Quanto e' fresca una chat: dall'ultima risposta (o dalla nascita se non ha mai
/// risposto). Una chat al lavoro o in attesa dell'utente e' sempre fresca.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Entro 3 giorni.
    Fresh,
    /// Da 4 a 14 giorni.
    Aging,
    /// Oltre 2 settimane.
    Stale,
}

impl Freshness {
    pub fn of(then: i64, now: DateTime<Local>) -> Self {
        match days_between(then, now).0 {
            0..=3 => Self::Fresh,
            4..=14 => Self::Aging,
            _ => Self::Stale,
        }
    }

    /// Opacita' del contenuto della vista d'insieme: come un foglio che ingiallisce.
    pub fn opacity(self) -> f32 {
        match self {
            Self::Fresh => 1.0,
            Self::Aging => 0.55,
            Self::Stale => 0.3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap()
    }

    #[test]
    fn labels_follow_calendar_days_with_the_time_up_to_three_days() {
        let now = at(2026, 10, 12, 15, 0);
        let label = |t: DateTime<Local>| age_label(t.timestamp(), now);
        assert_eq!(label(at(2026, 10, 12, 14, 32)), "today 14:32");
        assert_eq!(label(at(2026, 10, 12, 0, 5)), "today 00:05", "stesso giorno di calendario, non ultime 24 ore");
        assert_eq!(label(at(2026, 10, 11, 23, 50)), "yesterday 23:50");
        assert_eq!(label(at(2026, 10, 10, 9, 10)), "2 days ago 09:10");
        assert_eq!(label(at(2026, 10, 9, 18, 5)), "3 days ago 18:05");
        assert_eq!(label(at(2026, 10, 8, 18, 5)), "4 days ago");
        assert_eq!(label(at(2026, 10, 6, 1, 0)), "6 days ago");
        assert_eq!(label(at(2026, 10, 5, 1, 0)), "1 week ago");
        assert_eq!(label(now - Duration::days(13)), "1 week ago");
        assert_eq!(label(now - Duration::days(21)), "3 weeks ago");
        assert_eq!(label(now - Duration::days(30)), "4 weeks ago");
        assert_eq!(label(now - Duration::days(31)), "1 month ago");
        assert_eq!(label(now - Duration::days(100)), "3 months ago");
        assert_eq!(label(now - Duration::days(364)), "12 months ago");
        assert_eq!(label(now - Duration::days(365)), "1 year ago");
        assert_eq!(label(now - Duration::days(800)), "2 years ago");
        assert_eq!(label(now + Duration::hours(2)), "today 17:00", "orologio avanti: oggi");
    }

    #[test]
    fn freshness_steps_at_three_days_and_two_weeks() {
        let now = at(2026, 10, 12, 15, 0);
        let of = |d: i64| Freshness::of((now - Duration::days(d)).timestamp(), now);
        assert_eq!(of(0), Freshness::Fresh);
        assert_eq!(of(3), Freshness::Fresh);
        assert_eq!(of(4), Freshness::Aging);
        assert_eq!(of(14), Freshness::Aging);
        assert_eq!(of(15), Freshness::Stale);
        assert_eq!(of(400), Freshness::Stale);
        assert!(Freshness::Fresh.opacity() > Freshness::Aging.opacity());
        assert!(Freshness::Aging.opacity() > Freshness::Stale.opacity());
        assert_eq!(Freshness::Fresh.opacity(), 1.0);
    }
}
