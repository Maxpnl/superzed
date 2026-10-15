//! Frequenza di un'automazione (testo libero → `Schedule`) e calcolo puro della
//! prossima esecuzione. Niente GPUI: solo `chrono`.
use chrono::{DateTime, Datelike, Local, NaiveTime, Weekday};

/// Sotto questa soglia un intervallo (`Every`) e' rifiutato dal parser.
pub const MIN_INTERVAL_MINUTES: i64 = 5;
/// Una scadenza entro questo margine da "adesso" e' considerata dovuta (non saltata).
pub const GRACE_MINUTES: i64 = 2;

const MAX_INTERVAL_MINUTES: i64 = 30 * 24 * 60;

/// Frequenza di un'automazione: un intervallo fisso, oppure uno o piu' orari in
/// alcuni giorni della settimana (`days[0]` = lunedi').
#[derive(Clone, Debug, PartialEq)]
pub enum Schedule {
    Every(chrono::Duration),
    Daily { days: [bool; 7], times: Vec<NaiveTime> },
}

fn format_error() -> String {
    "format: 15m, 4h, 1d, 09:00, 09:00,18:00, mon-fri 09:00".to_string()
}

/// Se `s` ha la forma `<numero><m|h|d>`, prova a interpretarlo come intervallo.
fn parse_interval(s: &str) -> Option<Result<Schedule, String>> {
    let suffix = s.chars().last()?;
    if !matches!(suffix, 'm' | 'h' | 'd') {
        return None;
    }
    let digits = &s[..s.len() - suffix.len_utf8()];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // cifre tutte ASCII: l'unico errore possibile e' un numero oltre i64
    let too_big = || Some(Err("interval too large".to_string()));
    let Ok(n) = digits.parse::<i64>() else {
        return too_big();
    };
    let minutes = match suffix {
        'm' => Some(n),
        'h' => n.checked_mul(60),
        'd' => n.checked_mul(1440),
        _ => unreachable!(),
    };
    // oltre questa soglia (ben sopra il massimo di 30d) anche le `chrono::Duration`
    // andrebbero in panico
    let Some(minutes) = minutes.filter(|m| *m <= 1_000_000_000) else {
        return too_big();
    };
    if minutes < MIN_INTERVAL_MINUTES {
        return Some(Err(format!("minimum interval {MIN_INTERVAL_MINUTES}m")));
    }
    if minutes > MAX_INTERVAL_MINUTES {
        return Some(Err("maximum interval 30d".to_string()));
    }
    let duration = match suffix {
        'm' => chrono::Duration::minutes(n),
        'h' => chrono::Duration::hours(n),
        'd' => chrono::Duration::days(n),
        _ => unreachable!(),
    };
    Some(Ok(Schedule::Every(duration)))
}

/// Giorni in inglese, e in italiano per le frequenze scritte prima dell'interfaccia in inglese.
fn day_index(token: &str) -> Option<usize> {
    match token.to_lowercase().as_str() {
        "mon" | "lun" => Some(0),
        "tue" | "mar" => Some(1),
        "wed" | "mer" => Some(2),
        "thu" | "gio" => Some(3),
        "fri" | "ven" => Some(4),
        "sat" | "sab" => Some(5),
        "sun" | "dom" => Some(6),
        _ => None,
    }
}

/// Lista di giorni separata da virgole, voci `gg` o `gg-gg` (l'intervallo puo'
/// avvolgersi dalla domenica al lunedi').
fn parse_days(s: &str) -> Result<[bool; 7], String> {
    let mut days = [false; 7];
    for part in s.split(',') {
        let part = part.trim();
        if let Some((a, b)) = part.split_once('-') {
            // "lun - ven": spazi attorno al trattino
            let (a, b) = (a.trim(), b.trim());
            let ai = day_index(a).ok_or_else(|| format!("invalid day: {a}"))?;
            let bi = day_index(b).ok_or_else(|| format!("invalid day: {b}"))?;
            if ai <= bi {
                for i in ai..=bi {
                    days[i] = true;
                }
            } else {
                for i in ai..=6 {
                    days[i] = true;
                }
                for i in 0..=bi {
                    days[i] = true;
                }
            }
        } else {
            let i = day_index(part).ok_or_else(|| format!("invalid day: {part}"))?;
            days[i] = true;
        }
    }
    Ok(days)
}

/// Un orario `HH:MM` (0-23, 0-59).
fn parse_time_token(token: &str) -> Result<NaiveTime, String> {
    let invalid = || format!("invalid time: {token}");
    let (h, m) = token.split_once(':').ok_or_else(invalid)?;
    let h: u32 = h.parse().map_err(|_| invalid())?;
    let m: u32 = m.parse().map_err(|_| invalid())?;
    if h > 23 || m > 59 {
        return Err(invalid());
    }
    NaiveTime::from_hms_opt(h, m, 0).ok_or_else(invalid)
}

/// Interpreta il testo libero della frequenza di un'automazione.
pub fn parse(s: &str) -> Result<Schedule, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty schedule".to_string());
    }
    if let Some(result) = parse_interval(s) {
        return result;
    }
    if !s.contains(':') {
        return Err(format_error());
    }
    // i giorni sono tutto cio' che precede il primo orario (possono contenere spazi,
    // "lun - ven"); gli orari il resto ("09:00, 18:00")
    let times_start = s
        .char_indices()
        .filter(|(i, c)| !c.is_whitespace() && (*i == 0 || s[..*i].ends_with(char::is_whitespace)))
        .map(|(i, _)| i)
        .find(|i| s[*i..].split(char::is_whitespace).next().is_some_and(|w| w.contains(':')))
        .unwrap_or(0);
    let (days_part, times_part) = match s[..times_start].trim() {
        "" => (None, s[times_start..].trim()),
        d => (Some(d), s[times_start..].trim()),
    };
    let days = match days_part {
        Some(d) => parse_days(d)?,
        None => [true; 7],
    };
    let mut times = Vec::new();
    for token in times_part.split(',') {
        times.push(parse_time_token(token.trim())?);
    }
    if times.is_empty() {
        return Err(format_error());
    }
    times.sort();
    times.dedup();
    Ok(Schedule::Daily { days, times })
}

fn weekday_name(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

impl Schedule {
    /// Prima occorrenza strettamente successiva a `t`.
    pub fn next_after(&self, t: DateTime<Local>) -> DateTime<Local> {
        match self {
            Schedule::Every(d) => t + *d,
            Schedule::Daily { days, times } => {
                for offset in 0..=7i64 {
                    let date = t.date_naive() + chrono::Duration::days(offset);
                    let weekday_idx = date.weekday().num_days_from_monday() as usize;
                    if !days[weekday_idx] {
                        continue;
                    }
                    for time in times {
                        // None = buco del cambio d'ora: si salta quell'orario.
                        if let Some(candidate) = date.and_time(*time).and_local_timezone(Local).earliest() {
                            if candidate > t {
                                return candidate;
                            }
                        }
                    }
                }
                // Nessun giorno attivo (non dovrebbe capitare con una `Schedule` valida).
                t
            }
        }
    }
}

/// Prossima esecuzione dovuta, tenendo conto dell'ultima esecuzione (se c'e'), di
/// quando l'automazione e' stata creata/riattivata (`anchor`) e se recuperare le
/// scadenze perse mentre l'app era chiusa (`catch_up`).
pub fn next_run(schedule: &Schedule, last_run: Option<DateTime<Local>>, anchor: DateTime<Local>, catch_up: bool, now: DateTime<Local>) -> DateTime<Local> {
    let candidate = match (schedule, last_run) {
        // intervallo mai eseguito: con catch_up subito, altrimenti ancora + intervallo
        (Schedule::Every(_), None) if catch_up => return anchor,
        (_, Some(last)) => schedule.next_after(last),
        (_, None) => schedule.next_after(anchor),
    };
    let grace = now - chrono::Duration::minutes(GRACE_MINUTES);
    if catch_up || candidate >= grace {
        return candidate;
    }
    // senza catch_up le scadenze perse si saltano: la prima non ancora passata (o nel margine)
    let mut next = candidate;
    while next < grace {
        let after = schedule.next_after(next);
        // una frequenza che non avanza (intervallo non positivo, nessun giorno) non
        // deve far girare il ciclo all'infinito: la scadenza resta quella trovata
        if after <= next {
            return candidate;
        }
        next = after;
    }
    next
}

/// Etichetta leggibile della prossima esecuzione, relativa a `now`.
pub fn describe_next(next: DateTime<Local>, now: DateTime<Local>) -> String {
    let next_date = next.date_naive();
    let now_date = now.date_naive();
    if next_date == now_date {
        format!("today {}", next.format("%H:%M"))
    } else if next_date == now_date + chrono::Duration::days(1) {
        format!("tomorrow {}", next.format("%H:%M"))
    } else {
        format!("{} {}", weekday_name(next_date.weekday()), next.format("%d/%m %H:%M"))
    }
}

/// Quando e' avvenuta un'esecuzione passata: «today 09:00», «yesterday 18:30», altrimenti
/// giorno della settimana e data (come `describe_next`).
pub fn describe_past(at: DateTime<Local>, now: DateTime<Local>) -> String {
    let (date, today) = (at.date_naive(), now.date_naive());
    if date == today {
        format!("today {}", at.format("%H:%M"))
    } else if date + chrono::Duration::days(1) == today {
        format!("yesterday {}", at.format("%H:%M"))
    } else {
        format!("{} {}", weekday_name(date.weekday()), at.format("%d/%m %H:%M"))
    }
}

/// Anteprima testuale per il form: `parse` + `describe_next` sulla prossima occorrenza.
pub fn preview(s: &str, now: DateTime<Local>) -> Result<String, String> {
    let schedule = parse(s)?;
    Ok(format!("next: {}", describe_next(schedule.next_after(now), now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    fn t(d: u32, h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, d, h, m, 0).unwrap()
    }

    #[test]
    fn intervals() {
        assert_eq!(parse("15m"), Ok(Schedule::Every(chrono::Duration::minutes(15))));
        assert_eq!(parse(" 4h "), Ok(Schedule::Every(chrono::Duration::hours(4))));
        assert_eq!(parse("1d"), Ok(Schedule::Every(chrono::Duration::days(1))));
        assert!(parse("4m").unwrap_err().contains("5m"));
        assert!(parse("0h").is_err());
        assert!(parse("").unwrap_err().contains("empty"));
        assert!(parse("abc").unwrap_err().contains("format"));
    }

    // Revisione finale: un numero enorme non fa andare in overflow (panico nel profilo
    // spike a ogni tasto del form), e' solo un intervallo troppo grande.
    #[test]
    fn huge_intervals_are_an_error_not_an_overflow() {
        for s in ["1000000000000000000h", "1000000000000000000d", "9223372036854775807m", "99999999999999999999999h"] {
            assert_eq!(parse(s), Err("interval too large".to_string()), "{s}");
        }
        assert!(preview("1000000000000000000h", t(28, 9, 0)).is_err());
        assert_eq!(parse("31d"), Err("maximum interval 30d".to_string()));
    }

    #[test]
    fn daily_times_and_days() {
        let Ok(Schedule::Daily { days, times }) = parse("09:00") else { panic!() };
        assert_eq!(days, [true; 7]);
        assert_eq!(times, vec![NaiveTime::from_hms_opt(9, 0, 0).unwrap()]);
        let Ok(Schedule::Daily { times, .. }) = parse("18:00,09:00") else { panic!() };
        assert_eq!(times.len(), 2, "ordinati");
        assert!(times[0] < times[1]);
        let Ok(Schedule::Daily { days, .. }) = parse("lun-ven 09:00") else { panic!() };
        assert_eq!(days, [true, true, true, true, true, false, false]);
        let Ok(Schedule::Daily { days, .. }) = parse("Sab 10:30") else { panic!() };
        assert_eq!(days, [false, false, false, false, false, true, false]);
        let Ok(Schedule::Daily { days, .. }) = parse("ven-lun 08:00") else { panic!() };
        assert_eq!(days, [true, false, false, false, true, true, true], "a cavallo della settimana");
        let Ok(Schedule::Daily { days, .. }) = parse("lun,mer 08:00") else { panic!() };
        assert_eq!(days, [true, false, true, false, false, false, false]);
        assert!(parse("25:00").unwrap_err().contains("invalid time"));
        assert!(parse("lun-xyz 09:00").unwrap_err().contains("invalid day"));
    }

    #[test]
    fn next_after_is_strictly_later() {
        let s = parse("lun-ven 09:00").unwrap();
        assert_eq!(s.next_after(t(27, 10, 0)), t(28, 9, 0), "domenica → lunedi'");
        assert_eq!(s.next_after(t(28, 8, 0)), t(28, 9, 0));
        let s = parse("09:00,18:00").unwrap();
        assert_eq!(s.next_after(t(28, 9, 0)), t(28, 18, 0), "strettamente dopo");
        assert_eq!(parse("4h").unwrap().next_after(t(28, 9, 0)), t(28, 13, 0));
    }

    #[test]
    fn next_run_rules() {
        let every = parse("4h").unwrap();
        let (anchor, now) = (t(28, 9, 0), t(28, 9, 30));
        assert_eq!(next_run(&every, None, anchor, true, now), anchor, "mai eseguita + catch_up: subito");
        assert_eq!(next_run(&every, None, anchor, false, now), t(28, 13, 0), "mai eseguita: ancora + intervallo");
        assert_eq!(next_run(&every, Some(t(28, 8, 0)), anchor, true, now), t(28, 12, 0));
        // app chiusa 10 ore: con catch_up una sola esecuzione subito, senza si salta avanti
        let late = t(28, 18, 0);
        assert_eq!(next_run(&every, Some(t(28, 8, 0)), anchor, true, late), t(28, 12, 0));
        assert_eq!(next_run(&every, Some(t(28, 8, 0)), anchor, false, late), t(28, 20, 0));
        let daily = parse("09:00").unwrap();
        assert_eq!(next_run(&daily, None, t(27, 10, 0), true, t(27, 10, 0)), t(28, 9, 0), "orari: mai subito alla creazione");
        assert_eq!(next_run(&daily, Some(t(26, 9, 0)), anchor, true, t(28, 10, 0)), t(27, 9, 0), "saltata ieri: una volta");
        assert_eq!(next_run(&daily, Some(t(26, 9, 0)), anchor, false, t(28, 10, 0)), t(29, 9, 0));
        assert_eq!(next_run(&daily, Some(t(27, 9, 0)), anchor, false, t(28, 9, 1)), t(28, 9, 0), "entro il margine: tocca adesso");
    }

    #[test]
    fn next_run_labels() {
        let now = t(28, 10, 0);
        assert_eq!(describe_next(t(28, 14, 0), now), "today 14:00");
        assert_eq!(describe_next(t(29, 9, 0), now), "tomorrow 09:00");
        assert_eq!(describe_next(t(30, 9, 5), now), "Wed 30/09 09:05");
        assert_eq!(preview("lun-ven 09:00", t(27, 10, 0)), Ok("next: tomorrow 09:00".into()));
        assert!(preview("boh", now).is_err());
    }

    // Revisione del Task 9: spazi attorno al trattino di un intervallo di giorni.
    #[test]
    fn day_ranges_accept_spaces_around_the_dash() {
        assert_eq!(parse("lun - ven 09:00"), parse("lun-ven 09:00"));
        // giorni in inglese, e in italiano per le frequenze gia' salvate
        assert_eq!(parse("mon-fri 09:00"), parse("lun-ven 09:00"));
        assert_eq!(parse("Sat,sun 10:00"), parse("sab,dom 10:00"));
        assert_eq!(parse("tue,thu 08:00"), parse("mar,gio 08:00"));
        assert_eq!(parse("sab -dom 10:00"), parse("sab-dom 10:00"));
        assert!(parse("lun - ven 09:00").is_ok());
        assert_eq!(parse("lun-ven 09:00, 18:00"), parse("lun-ven 09:00,18:00"), "spazio dopo la virgola degli orari");
    }

    // Revisione del Task 9: senza catch_up il salto delle scadenze perse non gira a vuoto
    // su un intervallo non positivo (non producibile dal parser, ma possibile a mano).
    #[test]
    fn next_run_without_catch_up_terminates_on_a_non_positive_interval() {
        let (last, now) = (t(28, 8, 0), t(28, 18, 0));
        for d in [chrono::Duration::zero(), chrono::Duration::minutes(-5)] {
            let next = next_run(&Schedule::Every(d), Some(last), t(28, 7, 0), false, now);
            assert!(next <= now, "{d:?}: {next:?}");
        }
        let no_days = Schedule::Daily { days: [false; 7], times: vec![NaiveTime::from_hms_opt(9, 0, 0).unwrap()] };
        next_run(&no_days, Some(last), t(28, 7, 0), false, now);
    }

    #[test]
    fn past_runs_read_today_yesterday_or_the_date() {
        let now = t(28, 12, 0);
        assert_eq!(describe_past(t(28, 9, 0), now), "today 09:00");
        assert_eq!(describe_past(t(27, 18, 30), now), "yesterday 18:30");
        assert_eq!(describe_past(t(25, 7, 5), now), "Fri 25/09 07:05");
    }
}
