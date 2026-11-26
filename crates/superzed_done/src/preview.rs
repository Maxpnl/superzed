//! Anteprima delle modifiche a GitHub (cosa propone il report, cosa tiene l'utente) e
//! le azioni che ne derivano. Qui nessun `gh`.
use crate::report::{Report, TouchAction, report_markdown};
use crate::result::RunResult;

pub const TITLE_MAX_CHARS: usize = 120;
/// Limite di GitHub (65536) con margine per il marker.
pub const BODY_MAX_CHARS: usize = 65_000;
const GITHUB_TITLE_MAX: usize = 256;
pub const SUMMARY_KEY: &str = "summary";
/// Inizio del corpo di un blocker che ha un'attesa nota.
pub const WAITING_PREFIX: &str = "Waiting on: ";

/// Parole intere, minuscole, separate da tutto cio' che non e' alfanumerico.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase)
}

/// L'attesa e' di una macchina (CI, job, pipeline): la voce non e' un blocker per una persona.
pub fn machine_wait(waiting_on: &str) -> bool {
    const MACHINE_WORDS: [&str; 8] = ["ci", "actions", "pipeline", "job", "build", "runner", "cron", "timer"];
    words(waiting_on).any(|word| MACHINE_WORDS.contains(&word.as_str()))
}

/// L'attesa e' del proprietario del repository: la issue si assegna a chi scrive.
pub fn owner_wait(waiting_on: &str) -> bool {
    words(waiting_on).any(|word| word == "owner")
}

pub fn touched_key(number: u64) -> String {
    format!("touched:{number}")
}

pub fn new_key(item: usize) -> String {
    format!("new:{item}")
}

/// Titolo di una issue nuova: la prima riga non vuota, entro `TITLE_MAX_CHARS`.
pub fn title_of(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    if line.chars().count() <= TITLE_MAX_CHARS {
        return line.to_string();
    }
    let mut t: String = line.chars().take(TITLE_MAX_CHARS - 1).collect();
    t.push('…');
    t
}

#[derive(Clone, Debug, PartialEq)]
pub struct TouchedEntry {
    pub key: String,
    pub number: u64,
    pub action: TouchAction,
    pub note: String,
    pub enabled: bool,
    /// Gia' scritta da un'esecuzione precedente: mostrata, mai rieseguita.
    pub written: bool,
}

impl TouchedEntry {
    /// Un commento senza nota non ha niente da scrivere: spento e non si accende.
    pub fn writable(&self) -> bool {
        !(self.action == TouchAction::Comment && self.note.trim().is_empty())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewIssue {
    pub key: String,
    /// Indice nel marker (`missing`, poi `problems`, poi `blockers`): solo i blocker diventano issue,
    /// ma gli indici restano quelli di prima, cosi' i marker gia' scritti valgono ancora.
    pub item: usize,
    pub title: String,
    pub body: String,
    pub waiting_on: Option<String>,
    pub assign_me: bool,
    pub enabled: bool,
    pub written: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SummaryEntry {
    pub key: String,
    pub body: String,
    pub enabled: bool,
    pub written: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub session_id: String,
    /// Repo delle issue (`owner/nome`) per i link alle issue; `None` se non noto.
    pub repo: Option<String>,
    pub touched: Vec<TouchedEntry>,
    pub new_issues: Vec<NewIssue>,
    pub summary: Option<SummaryEntry>,
}

fn summary_comment(report: &Report) -> String {
    format!("Riepilogo della sessione superzed `{}`\n\n{}", report.session_id, report_markdown(report))
}

impl Preview {
    /// Proposta dal report; le voci gia' scritte (`previous`) restano visibili ma spente.
    pub fn from_report(report: &Report, previous: &RunResult) -> Self {
        let mut touched: Vec<TouchedEntry> = Vec::new();
        for t in &report.issues_touched {
            // una voce per issue: vince la prima
            if touched.iter().any(|e| e.number == t.number) {
                continue;
            }
            let key = touched_key(t.number);
            let written = previous.is_written(&key);
            let mut entry = TouchedEntry {
                number: t.number,
                action: t.action,
                note: t.note.clone().unwrap_or_default(),
                enabled: false,
                written,
                key,
            };
            entry.enabled = !written && entry.writable();
            touched.push(entry);
        }
        let first_blocker = report.missing.len() + report.problems.len();
        let new_issues = report
            .blockers
            .iter()
            .enumerate()
            .map(|(offset, blocker)| {
                let item = first_blocker + offset;
                let key = new_key(item);
                let written = previous.is_written(&key);
                let body = match &blocker.waiting_on {
                    Some(waiting_on) => format!("{WAITING_PREFIX}{waiting_on}\n\n{}", blocker.title),
                    None => blocker.title.clone(),
                };
                let machine = blocker.waiting_on.as_deref().is_some_and(machine_wait);
                NewIssue {
                    item,
                    title: title_of(&blocker.title),
                    body,
                    assign_me: blocker.waiting_on.as_deref().is_some_and(owner_wait),
                    waiting_on: blocker.waiting_on.clone(),
                    enabled: !written && !machine,
                    written,
                    key,
                }
            })
            .collect();
        let summary = (!touched.is_empty()).then(|| {
            let written = previous.is_written(SUMMARY_KEY);
            SummaryEntry { key: SUMMARY_KEY.into(), body: summary_comment(report), enabled: !written, written }
        });
        Preview { session_id: report.session_id.clone(), repo: None, touched, new_issues, summary }
    }

    /// Indirizzo della issue su GitHub, se il repo è noto.
    pub fn issue_url(&self, number: u64) -> Option<String> {
        let repo = self.repo.as_deref().map(str::trim).filter(|r| !r.is_empty())?;
        Some(crate::gh::issue_url(repo, number))
    }

    /// Issue del commento di riepilogo: la prima issue toccata spuntata (o gia' scritta).
    pub fn summary_target(&self) -> Option<u64> {
        self.touched.iter().find(|t| t.enabled || t.written).map(|t| t.number)
    }
}

/// Tutto quello che il report propone (con le spunte di partenza) e' gia' su GitHub.
pub fn fully_written(report: &Report, previous: &RunResult) -> bool {
    !previous.items.is_empty() && actions(&Preview::from_report(report, previous)).is_empty()
}

#[derive(Clone, Debug, PartialEq)]
pub struct IssueDraft {
    pub key: String,
    pub item: usize,
    pub title: String,
    /// Corpo senza marker (lo aggiunge `gh::issue_body`).
    pub body: String,
    pub assign_me: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Issue nuova (o gia' creata da questa sessione: aggiornata).
    Upsert(IssueDraft),
    Comment { key: String, number: u64, body: String },
    Close { key: String, number: u64, comment: Option<String> },
}

impl Action {
    pub fn key(&self) -> &str {
        match self {
            Action::Upsert(d) => &d.key,
            Action::Comment { key, .. } | Action::Close { key, .. } => key,
        }
    }
}

/// Azioni delle voci spuntate e non ancora scritte, nell'ordine di esecuzione: issue
/// nuove, issue toccate, riepilogo.
pub fn actions(p: &Preview) -> Vec<Action> {
    let mut out = Vec::new();
    for n in p.new_issues.iter().filter(|n| n.enabled && !n.written) {
        out.push(Action::Upsert(IssueDraft {
            key: n.key.clone(),
            item: n.item,
            title: n.title.trim().to_string(),
            body: n.body.clone(),
            assign_me: n.assign_me,
        }));
    }
    for t in p.touched.iter().filter(|t| t.enabled && !t.written && t.writable()) {
        let note = Some(t.note.trim().to_string()).filter(|s| !s.is_empty());
        match t.action {
            TouchAction::Close => out.push(Action::Close { key: t.key.clone(), number: t.number, comment: note }),
            TouchAction::Comment => {
                if let Some(body) = note {
                    out.push(Action::Comment { key: t.key.clone(), number: t.number, body });
                }
            }
        }
    }
    if let (Some(s), Some(number)) = (p.summary.as_ref().filter(|s| s.enabled && !s.written && !s.body.trim().is_empty()), p.summary_target()) {
        out.push(Action::Comment { key: s.key.clone(), number, body: s.body.clone() });
    }
    out
}

/// Quante chiusure di issue ci sono fra le azioni.
pub fn closes(actions: &[Action]) -> usize {
    actions.iter().filter(|a| matches!(a, Action::Close { .. })).count()
}

/// Cosa succede alla issue toccata, per l'interfaccia.
pub fn touch_label(action: TouchAction) -> &'static str {
    match action {
        TouchAction::Close => "close",
        TouchAction::Comment => "comment",
    }
}

/// Titolo lungo accorciato per i messaggi.
const LABEL_TITLE_CHARS: usize = 40;

fn new_issue_label(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return "New issue without a title".into();
    }
    let short = if title.chars().count() > LABEL_TITLE_CHARS {
        let mut t: String = title.chars().take(LABEL_TITLE_CHARS - 1).collect();
        t.push('…');
        t
    } else {
        title.to_string()
    };
    format!("New issue “{short}”")
}

fn touched_label(number: u64, action: TouchAction) -> String {
    format!("#{number} ({})", touch_label(action))
}

/// Etichetta leggibile di un'azione (al posto della sua chiave interna).
pub fn action_label(a: &Action) -> String {
    match a {
        Action::Upsert(d) => new_issue_label(&d.title),
        Action::Close { number, .. } => touched_label(*number, TouchAction::Close),
        Action::Comment { key, .. } if key == SUMMARY_KEY => "Summary".into(),
        Action::Comment { number, .. } => touched_label(*number, TouchAction::Comment),
    }
}

/// Etichetta leggibile della voce `key` nelle notifiche: dalle azioni appena scritte (con
/// i testi modificati), se no dal report; chiave sconosciuta: la chiave.
pub fn item_label(key: &str, actions: &[Action], report: &Report) -> String {
    if let Some(a) = actions.iter().find(|a| a.key() == key) {
        return action_label(a);
    }
    if key == SUMMARY_KEY {
        return "Summary".into();
    }
    let p = Preview::from_report(report, &RunResult::default());
    if let Some(n) = p.new_issues.iter().find(|n| n.key == key) {
        return new_issue_label(&n.title);
    }
    if let Some(t) = p.touched.iter().find(|t| t.key == key) {
        return touched_label(t.number, t.action);
    }
    key.to_string()
}

/// Posizione nell'elenco a parole: «the second».
pub fn ordinal(n: usize) -> String {
    const WORDS: [&str; 10] = ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth"];
    match n.checked_sub(1).and_then(|i| WORDS.get(i)) {
        Some(w) => format!("the {w}"),
        None => format!("number {n}"),
    }
}

/// Il campo che non passa i controlli; gli indici sono quelli di `Preview::new_issues`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidField {
    Title(usize),
    Body(usize),
    Summary,
}

/// Controlli sulle voci spuntate prima di scrivere (titoli vuoti o troppo lunghi, corpi
/// enormi): il primo campo in errore e il messaggio.
pub fn validate_fields(p: &Preview) -> Result<(), (InvalidField, String)> {
    for (ix, n) in p.new_issues.iter().enumerate().filter(|(_, n)| n.enabled && !n.written) {
        if n.title.trim().is_empty() {
            return Err((InvalidField::Title(ix), format!("New issue without a title ({})", ordinal(ix + 1))));
        }
        let label = new_issue_label(&n.title);
        if n.title.trim().chars().count() > GITHUB_TITLE_MAX {
            return Err((InvalidField::Title(ix), format!("{label}: the title is longer than {GITHUB_TITLE_MAX} characters")));
        }
        if n.body.chars().count() > BODY_MAX_CHARS {
            return Err((InvalidField::Body(ix), format!("{label}: the description is longer than {BODY_MAX_CHARS} characters")));
        }
    }
    if let Some(s) = p.summary.as_ref().filter(|s| s.enabled && !s.written)
        && s.body.chars().count() > BODY_MAX_CHARS
    {
        return Err((InvalidField::Summary, format!("The summary is longer than {BODY_MAX_CHARS} characters")));
    }
    Ok(())
}

/// Come `validate_fields`, solo il messaggio.
pub fn validate_preview(p: &Preview) -> Result<(), String> {
    validate_fields(p).map_err(|(_, message)| message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Blocker, parse_report};
    use crate::result::{FoundIssue, ItemReport, RunResult};
    use crate::test_support::{REPORT_JSON, SID};

    fn report() -> Report {
        parse_report(REPORT_JSON, SID).unwrap()
    }

    fn keys(actions: &[Action]) -> Vec<&str> {
        actions.iter().map(Action::key).collect()
    }

    #[test]
    fn preview_from_the_report() {
        let p = Preview::from_report(&report(), &RunResult::default());
        assert_eq!(p.session_id, SID);
        let touched: Vec<(u64, TouchAction, &str, bool)> =
            p.touched.iter().map(|t| (t.number, t.action, t.note.as_str(), t.enabled)).collect();
        assert_eq!(touched, [(12, TouchAction::Close, "Import completato", true), (8, TouchAction::Comment, "Serve l'accesso allo staging", true)]);
        let new: Vec<(usize, &str)> = p.new_issues.iter().map(|n| (n.item, n.title.as_str())).collect();
        assert_eq!(new, [(2, "Policy di annullamento")], "solo i blocker, con l'indice che avevano");
        assert_eq!(p.new_issues.iter().map(|n| n.key.as_str()).collect::<Vec<_>>(), ["new:2"]);
        assert!(p.new_issues.iter().all(|n| n.enabled && !n.written && !n.assign_me));
        assert_eq!(p.new_issues[0].body, "Waiting on: design\n\nPolicy di annullamento");
        assert_eq!(p.new_issues[0].waiting_on.as_deref(), Some("design"));
        let summary = p.summary.as_ref().expect("riepilogo sull'issue principale");
        assert_eq!(p.summary_target(), Some(12));
        assert!(summary.body.starts_with(&format!("Riepilogo della sessione superzed `{SID}`\n\n## Fatto\n")), "{}", summary.body);
    }

    #[test]
    fn duplicates_and_empty_comments() {
        let mut r = report();
        r.issues_touched.push(crate::report::TouchedIssue { number: 12, action: TouchAction::Comment, note: Some("doppione".into()) });
        r.issues_touched.push(crate::report::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let p = Preview::from_report(&r, &RunResult::default());
        assert_eq!(p.touched.iter().map(|t| t.number).collect::<Vec<_>>(), [12, 8, 30], "una voce per issue");
        assert!(!p.touched[2].enabled, "commento senza nota: spento");
        assert!(!keys(&actions(&p)).contains(&"touched:30"));
        let none = Preview::from_report(&Report { session_id: SID.into(), ..Default::default() }, &RunResult::default());
        assert!(none.summary.is_none() && actions(&none).is_empty(), "nessuna issue toccata: nessun riepilogo");
    }

    #[test]
    fn actions_follow_the_checks_and_the_edits() {
        let mut p = Preview::from_report(&report(), &RunResult::default());
        p.new_issues[0].title = "  Policy  ".into();
        p.new_issues[0].assign_me = true;
        let a = actions(&p);
        assert_eq!(keys(&a), ["new:2", "touched:12", "touched:8", "summary"]);
        assert_eq!(
            a[0],
            Action::Upsert(IssueDraft {
                key: "new:2".into(),
                item: 2,
                title: "Policy".into(),
                body: "Waiting on: design\n\nPolicy di annullamento".into(),
                assign_me: true,
            })
        );
        assert_eq!(a[1], Action::Close { key: "touched:12".into(), number: 12, comment: Some("Import completato".into()) });
        assert_eq!(a[2], Action::Comment { key: "touched:8".into(), number: 8, body: "Serve l'accesso allo staging".into() });
        assert!(matches!(&a[3], Action::Comment { key, number: 12, .. } if key == SUMMARY_KEY));
        p.new_issues[0].enabled = false;
        p.summary.as_mut().unwrap().enabled = false;
        let a = actions(&p);
        assert_eq!(keys(&a), ["touched:12", "touched:8"]);
    }

    #[test]
    fn written_entries_are_never_redone() {
        let mut previous = RunResult::default();
        previous.record(vec![
            ItemReport { key: "new:2".into(), result: Ok("https://github.com/acme/acme_app/issues/42".into()), issue: Some(FoundIssue { number: 42, url: "https://github.com/acme/acme_app/issues/42".into() }) },
            ItemReport { key: "touched:12".into(), result: Ok("#12 closed".into()), issue: None },
            ItemReport { key: "touched:8".into(), result: Err("gh: HTTP 502".into()), issue: None },
        ]);
        let p = Preview::from_report(&report(), &previous);
        assert!(p.new_issues[0].written && !p.new_issues[0].enabled);
        assert!(!p.touched[1].written && p.touched[1].enabled, "fallita: si riprova");
        assert!(p.touched[0].written);
        let mut forced = p;
        forced.new_issues[0].enabled = true; // anche riaccesa a mano, una voce scritta non si riscrive
        assert_eq!(keys(&actions(&forced)), ["touched:8", "summary"]);
    }

    #[test]
    fn validation_of_the_edits() {
        let mut r = report();
        r.blockers.push(Blocker { title: "Seconda attesa".into(), waiting_on: None });
        let mut p = Preview::from_report(&r, &RunResult::default());
        assert_eq!(validate_preview(&p), Ok(()));
        p.new_issues[1].title = "   ".into();
        assert_eq!(validate_preview(&p), Err("New issue without a title (the second)".into()));
        assert_eq!(validate_fields(&p).unwrap_err().0, InvalidField::Title(1));
        p.new_issues[1].enabled = false;
        assert_eq!(validate_preview(&p), Ok(()), "spenta: non conta");
        p.new_issues[0].title = "x".repeat(257);
        assert_eq!(
            validate_preview(&p),
            Err(format!("New issue “{}…”: the title is longer than 256 characters", "x".repeat(39))),
            "titolo lungo accorciato nel messaggio"
        );
        p.new_issues[0].title = "Policy".into();
        p.new_issues[0].body = "x".repeat(BODY_MAX_CHARS + 1);
        assert_eq!(validate_preview(&p), Err("New issue “Policy”: the description is longer than 65000 characters".into()));
        assert_eq!(validate_fields(&p).unwrap_err().0, InvalidField::Body(0));
        p.new_issues[0].body = "ok".into();
        p.summary.as_mut().unwrap().body = "x".repeat(BODY_MAX_CHARS + 1);
        assert_eq!(validate_preview(&p), Err("The summary is longer than 65000 characters".into()));
        assert_eq!(validate_fields(&p).unwrap_err().0, InvalidField::Summary);
    }

    #[test]
    fn ordinals_for_untitled_issues() {
        assert_eq!([1, 2, 3, 10, 11].map(ordinal), ["the first", "the second", "the third", "the tenth", "number 11"]);
    }

    #[test]
    fn closes_are_counted() {
        let mut p = Preview::from_report(&report(), &RunResult::default());
        assert_eq!(closes(&actions(&p)), 1);
        p.touched[0].enabled = false;
        assert_eq!(closes(&actions(&p)), 0);
    }

    #[test]
    fn an_edited_comment_is_what_gets_written() {
        let mut p = Preview::from_report(&report(), &RunResult::default());
        p.touched[0].note = "  Import completato e verificato  ".into();
        p.touched[1].note = String::new();
        let a = actions(&p);
        assert_eq!(a[1], Action::Close { key: "touched:12".into(), number: 12, comment: Some("Import completato e verificato".into()) });
        assert!(!keys(&a).contains(&"touched:8"), "commento svuotato: niente da scrivere");
        // un commento puro svuotato non si scrive; riempito si scrive col testo nuovo
        let mut r = report();
        r.issues_touched.push(crate::report::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let mut p = Preview::from_report(&r, &RunResult::default());
        p.touched[2].note = "Aggiunto il test".into();
        p.touched[2].enabled = true;
        assert!(actions(&p).contains(&Action::Comment { key: "touched:30".into(), number: 30, body: "Aggiunto il test".into() }));
    }

    #[test]
    fn issue_links_need_the_repo() {
        let mut p = Preview::from_report(&report(), &RunResult::default());
        assert_eq!(p.issue_url(12), None);
        p.repo = Some("acme/acme_app".into());
        assert_eq!(p.issue_url(12).as_deref(), Some("https://github.com/acme/acme_app/issues/12"));
        p.repo = Some("  ".into());
        assert_eq!(p.issue_url(12), None);
    }

    #[test]
    fn failure_labels_are_readable() {
        let r = report();
        let mut p = Preview::from_report(&r, &RunResult::default());
        p.new_issues[0].title = "Test su Safari".into();
        let a = actions(&p);
        assert_eq!(item_label("new:2", &a, &r), "New issue “Test su Safari”", "il titolo modificato");
        assert_eq!(item_label("touched:12", &a, &r), "#12 (close)");
        assert_eq!(item_label("touched:8", &a, &r), "#8 (comment)");
        assert_eq!(item_label(SUMMARY_KEY, &a, &r), "Summary");
        // voci non scritte da questa scrittura (fallite prima e poi spente): dal report
        assert_eq!(item_label("new:2", &[], &r), "New issue “Policy di annullamento”");
        assert_eq!(item_label("touched:12", &[], &r), "#12 (close)");
        assert_eq!(item_label(SUMMARY_KEY, &[], &r), "Summary");
        assert_eq!(item_label("boh:1", &[], &r), "boh:1");
        let untitled = Action::Upsert(IssueDraft { title: String::new(), ..match &a[0] { Action::Upsert(d) => d.clone(), _ => unreachable!() } });
        assert_eq!(action_label(&untitled), "New issue without a title");
        assert_eq!(action_label(&Action::Comment { key: "touched:30".into(), number: 30, body: "x".into() }), "#30 (comment)");
    }

    // Revisione finale 6: il riepilogo va sulla prima issue toccata spuntata.
    #[test]
    fn the_summary_goes_to_the_first_checked_touched_issue() {
        let mut p = Preview::from_report(&report(), &RunResult::default());
        p.touched[0].enabled = false;
        assert_eq!(p.summary_target(), Some(8));
        assert!(matches!(actions(&p).last(), Some(Action::Comment { key, number: 8, .. }) if key == SUMMARY_KEY));
        p.touched[1].enabled = false;
        assert_eq!(p.summary_target(), None);
        assert!(!keys(&actions(&p)).contains(&SUMMARY_KEY), "nessuna issue spuntata: nessun riepilogo");
        let mut previous = RunResult::default();
        previous.record(vec![ItemReport { key: "touched:12".into(), result: Ok("#12 closed".into()), issue: None }]);
        let p = Preview::from_report(&report(), &previous);
        assert_eq!(p.summary_target(), Some(12), "gia' scritta: resta l'issue principale");
    }

    // Revisione finale 6: un commento senza nota parte spento e non si puo' accendere.
    #[test]
    fn a_comment_without_a_note_is_not_writable() {
        let mut r = report();
        r.issues_touched.push(crate::report::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let p = Preview::from_report(&r, &RunResult::default());
        assert!(!p.touched[2].writable() && !p.touched[2].enabled);
        assert!(p.touched[0].writable() && p.touched[1].writable());
    }

    // Revisione finale 6: rientro in Fatto con tutto scritto.
    #[test]
    fn fully_written_needs_every_default_entry_written() {
        let r = report();
        assert!(!fully_written(&r, &RunResult::default()), "niente scritto");
        let mut previous = RunResult::default();
        let ok = |k: &str| ItemReport { key: k.into(), result: Ok("ok".into()), issue: None };
        previous.record(["new:2", "touched:12", "touched:8"].map(ok).to_vec());
        assert!(!fully_written(&r, &previous), "manca il riepilogo");
        previous.record(vec![ok(SUMMARY_KEY)]);
        assert!(fully_written(&r, &previous));
        let empty = Report { session_id: SID.into(), ..Default::default() };
        assert!(!fully_written(&empty, &RunResult::default()), "report vuoto: niente da dire");
    }

    #[test]
    fn titles_are_the_first_line_and_bounded() {
        assert_eq!(title_of("\n  Prima riga \nseconda"), "Prima riga");
        let t = title_of(&"x".repeat(200));
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn only_blockers_become_issues_and_keep_their_marker_index() {
        let report = parse_report(
            r#"{"missing":["m0","m1"],"problems":["p0"],"blockers":[
            {"title":"Staging API key for the mailer","waiting_on":"Alice (owner)"},
            {"title":"e2e still queued","waiting_on":"GitHub Actions"},
            {"title":"Firewall rule for the reporting database","waiting_on":"Ops team"},
            {"title":"No one named"}]}"#,
            "s-1",
        )
        .unwrap();
        let preview = Preview::from_report(&report, &RunResult::default());
        let seen: Vec<_> = preview.new_issues.iter().map(|n| (n.item, n.title.as_str(), n.assign_me, n.enabled)).collect();
        // gli indici restano quelli di prima (missing, problems, blockers): i marker gia' scritti valgono ancora
        assert_eq!(seen, [(3, "Staging API key for the mailer", true, true), (4, "e2e still queued", false, false), (5, "Firewall rule for the reporting database", false, true), (6, "No one named", false, true)]);
        assert_eq!(preview.new_issues[0].body, "Waiting on: Alice (owner)\n\nStaging API key for the mailer");
        assert_eq!(preview.new_issues[3].body, "No one named");
    }

    #[test]
    fn machine_and_owner_waits() {
        for w in ["GitHub Actions", "CI", "the nightly job", "build pipeline", "runner"] {
            assert!(machine_wait(w), "{w}");
        }
        for w in ["Alice (owner)", "decisione dell'owner", "Ops team", "Legal"] {
            assert!(!machine_wait(w), "{w}");
        }
        for w in ["Alice (owner)", "via dell'owner", "Owner"] {
            assert!(owner_wait(w), "{w}");
        }
        for w in ["Ops team", "ownership review"] {
            assert!(!owner_wait(w), "{w}");
        }
    }

    #[test]
    fn update_from_an_old_report_is_a_comment() {
        let report = parse_report(r#"{"issues_touched":[{"number":8,"action":"update","note":"n"}]}"#, "s-1").unwrap();
        assert_eq!(report.issues_touched[0].action, TouchAction::Comment);
    }
}
