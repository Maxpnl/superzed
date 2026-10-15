//! Quote di un account: parsing della risposta di `oauth/usage` (formato `limits[]`
//! e legacy `five_hour`/`seven_day`), `rate_limits` della status line, rimanente e
//! avvisi di quota bassa. Puro.
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use superzed_sessions::{RateLimits, RateWindow};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Limit {
    /// Percentuale usata, 0..=100.
    pub used_percent: f32,
    pub resets_at: Option<DateTime<Utc>>,
    /// Quando e' stato letto (fetch o status line): nel merge vince il piu' recente.
    pub seen_at: DateTime<Utc>,
}

impl Limit {
    pub fn is_reset(&self, now: DateTime<Utc>) -> bool {
        self.resets_at.is_some_and(|r| r <= now)
    }

    /// Percentuale rimanente; una finestra gia' resettata e' piena.
    pub fn remaining(&self, now: DateTime<Utc>) -> f32 {
        if self.is_reset(now) { 100. } else { (100. - self.used_percent).clamp(0., 100.) }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    /// Finestra di 5 ore.
    pub session: Option<Limit>,
    /// Settimana, tutti i modelli.
    pub weekly: Option<Limit>,
    /// Settimana per modello (`weekly_scoped`): solo mostrati.
    pub scoped: Vec<(String, Limit)>,
    /// Ultimo fetch riuscito dell'endpoint.
    pub fetched_at: Option<DateTime<Utc>>,
}

fn limit_from(percent: Option<&serde_json::Value>, resets: Option<&serde_json::Value>, now: DateTime<Utc>) -> Option<Limit> {
    let used = percent?.as_f64()? as f32;
    let resets_at = resets
        .and_then(|r| r.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    Some(Limit { used_percent: used.clamp(0., 100.), resets_at, seen_at: now })
}

/// Risposta di `GET /api/oauth/usage`; `None` se non e' in nessuno dei due formati.
/// Nel legacy `utilization` e' gia' una percentuale (0..100).
pub fn parse_usage(v: &serde_json::Value, now: DateTime<Utc>) -> Option<Quota> {
    if let Some(limits) = v.get("limits").and_then(|l| l.as_array()) {
        let mut q = Quota { fetched_at: Some(now), ..Default::default() };
        for l in limits {
            let Some(limit) = limit_from(l.get("percent"), l.get("resets_at"), now) else {
                continue;
            };
            match l.get("kind").and_then(|k| k.as_str()) {
                Some("session") => q.session = Some(limit),
                Some("weekly_all") => q.weekly = Some(limit),
                Some("weekly_scoped") => {
                    let name = l.pointer("/scope/model/display_name").and_then(|n| n.as_str()).unwrap_or("model");
                    q.scoped.push((name.to_string(), limit));
                }
                _ => {}
            }
        }
        return Some(q);
    }
    let (five, seven) = (v.get("five_hour"), v.get("seven_day"));
    if five.is_none() && seven.is_none() {
        return None;
    }
    let window = |w: Option<&serde_json::Value>| w.and_then(|w| limit_from(w.get("utilization"), w.get("resets_at"), now));
    Some(Quota { session: window(five), weekly: window(seven), scoped: Vec::new(), fetched_at: Some(now) })
}

/// `rate_limits` della status line (`resets_at` in epoch s) come quota parziale.
pub fn from_status_line(rl: &RateLimits, seen: DateTime<Utc>) -> Quota {
    let limit = |w: &RateWindow| Limit {
        used_percent: w.used_percent.clamp(0., 100.),
        resets_at: w.resets_at.and_then(|s| DateTime::from_timestamp(s, 0)),
        seen_at: seen,
    };
    Quota {
        session: rl.five_hour.as_ref().map(limit),
        weekly: rl.seven_day.as_ref().map(limit),
        scoped: Vec::new(),
        fetched_at: None,
    }
}

fn newer(old: Option<Limit>, new: Option<Limit>) -> Option<Limit> {
    match (old, new) {
        (Some(o), Some(n)) => Some(if n.seen_at >= o.seen_at { n } else { o }),
        (o, n) => n.or(o),
    }
}

pub fn merge(old: Option<Quota>, new: Quota) -> Quota {
    let old = old.unwrap_or_default();
    Quota {
        session: newer(old.session, new.session),
        weekly: newer(old.weekly, new.weekly),
        scoped: if new.scoped.is_empty() { old.scoped } else { new.scoped },
        fetched_at: old.fetched_at.max(new.fetched_at),
    }
}

pub fn remaining_pct(limit: Option<&Limit>, now: DateTime<Utc>) -> Option<u8> {
    limit.map(|l| l.remaining(now).round() as u8)
}

/// Sotto questa percentuale rimanente arriva la notifica.
pub const LOW_QUOTA: f32 = 10.0;

#[derive(Clone, Debug, PartialEq)]
pub struct LowQuota {
    /// "5h" o "settimana".
    pub window: &'static str,
    pub remaining: u8,
    pub resets_at: Option<DateTime<Utc>>,
}

impl LowQuota {
    /// Una notifica per account, finestra e reset (al minuto piu' vicino: endpoint e
    /// status line danno lo stesso reset con qualche secondo di differenza).
    pub fn key(&self, account_id: &str) -> String {
        let minute = self.resets_at.map(|r| (r.timestamp() + 30).div_euclid(60)).unwrap_or(0);
        format!("{account_id}|{}|{minute}", self.window)
    }

    pub fn summary(&self) -> String {
        let reset = self
            .resets_at
            .map(|r| format!(" (reset {})", r.with_timezone(&Local).format("%d/%m %H:%M")))
            .unwrap_or_default();
        format!("{}: {}% left{reset}", self.window, self.remaining)
    }
}

pub fn low_quota_warnings(q: &Quota, now: DateTime<Utc>) -> Vec<LowQuota> {
    [("5h", q.session.as_ref()), ("week", q.weekly.as_ref())]
        .into_iter()
        .filter_map(|(window, l)| {
            let l = l?;
            let remaining = l.remaining(now);
            (remaining < LOW_QUOTA).then(|| LowQuota { window, remaining: remaining.round() as u8, resets_at: l.resets_at })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use serde_json::json;
    use super::*;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 27, h, 0, 0).unwrap()
    }

    #[test]
    fn parses_the_new_format() {
        let v = json!({"limits": [
            {"kind": "session", "percent": 37.5, "resets_at": "2026-09-27T14:00:00Z", "severity": "ok"},
            {"kind": "weekly_all", "percent": 80, "resets_at": "2026-10-01T00:00:00+02:00"},
            {"kind": "weekly_scoped", "percent": 12, "resets_at": null, "scope": {"model": {"display_name": "Opus"}}},
            {"kind": "futuro", "percent": 1}
        ]});
        let q = parse_usage(&v, t(9)).unwrap();
        assert_eq!(q.session, Some(Limit { used_percent: 37.5, resets_at: Some(t(14)), seen_at: t(9) }));
        assert_eq!(q.weekly.as_ref().unwrap().resets_at, Some(Utc.with_ymd_and_hms(2026, 9, 30, 22, 0, 0).unwrap()));
        assert_eq!(q.scoped, vec![("Opus".to_string(), Limit { used_percent: 12., resets_at: None, seen_at: t(9) })]);
        assert_eq!(q.fetched_at, Some(t(9)));
    }

    #[test]
    fn parses_the_legacy_format_and_rejects_the_unknown() {
        let v = json!({"five_hour": {"utilization": 22.0, "resets_at": "2026-09-27T14:00:00Z"}, "seven_day": null});
        let q = parse_usage(&v, t(9)).unwrap();
        assert_eq!(q.session.unwrap().used_percent, 22.);
        assert_eq!(q.weekly, None);
        assert_eq!(parse_usage(&json!({"altro": 1}), t(9)), None);
        assert_eq!(parse_usage(&json!([1]), t(9)), None);
        let clamped = parse_usage(&json!({"limits": [{"kind": "session", "percent": 140}]}), t(9)).unwrap();
        assert_eq!(clamped.session.unwrap().used_percent, 100.);
    }

    #[test]
    fn status_line_merges_by_recency() {
        let fetched = parse_usage(&json!({"limits": [
            {"kind": "session", "percent": 30, "resets_at": "2026-09-27T14:00:00Z"},
            {"kind": "weekly_all", "percent": 50, "resets_at": "2026-10-01T00:00:00Z"}
        ]}), t(9)).unwrap();
        let rl = RateLimits {
            five_hour: Some(RateWindow { used_percent: 45., resets_at: Some(t(14).timestamp()) }),
            seven_day: None,
        };
        let merged = merge(Some(fetched.clone()), from_status_line(&rl, t(10)));
        assert_eq!(merged.session.as_ref().unwrap().used_percent, 45., "status line piu' recente");
        assert_eq!(merged.weekly, fetched.weekly, "settimana: resta il fetch");
        assert_eq!(merged.fetched_at, Some(t(9)));
        let older = merge(Some(merged.clone()), from_status_line(&rl, t(8)));
        assert_eq!(older.session, merged.session, "un valore piu' vecchio non vince");
        let again = merge(Some(merged), parse_usage(&json!({"limits": [{"kind": "session", "percent": 5}]}), t(11)).unwrap());
        assert_eq!(again.session.unwrap().used_percent, 5., "il fetch dopo vince");
    }

    #[test]
    fn remaining_counts_resets() {
        let l = Limit { used_percent: 97., resets_at: Some(t(14)), seen_at: t(9) };
        assert_eq!(remaining_pct(Some(&l), t(10)), Some(3));
        assert_eq!(remaining_pct(Some(&l), t(14)), Some(100), "finestra resettata");
        assert_eq!(remaining_pct(None, t(10)), None);
        let no_reset = Limit { used_percent: 20.4, resets_at: None, seen_at: t(9) };
        assert_eq!(remaining_pct(Some(&no_reset), t(23)), Some(80));
    }

    #[test]
    fn low_quota_warnings_have_a_key_per_reset_window() {
        let q = Quota {
            session: Some(Limit { used_percent: 93., resets_at: Some(t(14)), seen_at: t(9) }),
            weekly: Some(Limit { used_percent: 50., resets_at: None, seen_at: t(9) }),
            ..Default::default()
        };
        let w = low_quota_warnings(&q, t(10));
        assert_eq!(w, vec![LowQuota { window: "5h", remaining: 7, resets_at: Some(t(14)) }]);
        assert_eq!(w[0].key("a1"), format!("a1|5h|{}", t(14).timestamp() / 60));
        // endpoint e status line danno reset che differiscono di qualche secondo
        let near = |secs: i64| LowQuota { resets_at: Some(t(14) + chrono::Duration::seconds(secs)), ..w[0].clone() };
        assert_eq!(near(-20).key("a1"), w[0].key("a1"), "stesso minuto (arrotondato)");
        assert_eq!(near(25).key("a1"), w[0].key("a1"));
        assert_ne!(near(3600).key("a1"), w[0].key("a1"), "finestra diversa");
        assert!(w[0].summary().starts_with("5h: 7% left"), "{}", w[0].summary());
        assert!(low_quota_warnings(&q, t(15)).is_empty(), "dopo il reset niente avviso");
    }
}
