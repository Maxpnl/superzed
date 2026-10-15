//! Anteprima delle modifiche alla board (cosa propone il report, cosa tiene l'utente) e
//! le azioni che ne derivano. Qui nessun `gh`.
use crate::report::{Report, TouchAction, report_markdown};
use crate::result::RunResult;

pub const TITLE_MAX_CHARS: usize = 120;
/// Limite di GitHub (65536) con margine per il marker.
pub const BODY_MAX_CHARS: usize = 65_000;
const GITHUB_TITLE_MAX: usize = 256;
pub const SUMMARY_KEY: &str = "summary";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Task,
    Question,
    Blocker,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Task => "Task",
            Kind::Question => "Question",
            Kind::Blocker => "Blocker",
        }
    }

    /// Status con cui la issue nuova entra nella board.
    pub fn status(self) -> Status {
        match self {
            Kind::Blocker => Status::Blocked,
            Kind::Task | Kind::Question => Status::Backlog,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Backlog,
    InProgress,
    Blocked,
    Done,
}

impl Status {
    pub const ALL: [Status; 4] = [Status::Backlog, Status::InProgress, Status::Blocked, Status::Done];

    pub fn name(self) -> &'static str {
        match self {
            Status::Backlog => "Backlog",
            Status::InProgress => "In Progress",
            Status::Blocked => "Blocked",
            Status::Done => "Done",
        }
    }

    pub fn next(self) -> Status {
        let ix = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(ix + 1) % Self::ALL.len()]
    }
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
    /// Solo per `update`: lo Status da impostare.
    pub status: Status,
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
    /// Indice nel marker (`missing`, poi `problems`, poi `blockers`).
    pub item: usize,
    pub kind: Kind,
    pub title: String,
    pub body: String,
    pub area: Option<String>,
    pub waiting_on: Option<String>,
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
    /// Repo della board (`owner/nome`) per i link alle issue; `None` se non noto.
    pub repo: Option<String>,
    /// Aree della config (scelte della tendina).
    pub areas: Vec<String>,
    pub touched: Vec<TouchedEntry>,
    pub new_issues: Vec<NewIssue>,
    pub summary: Option<SummaryEntry>,
}

fn summary_comment(report: &Report) -> String {
    format!("Riepilogo della sessione superzed `{}`\n\n{}", report.session_id, report_markdown(report))
}

impl Preview {
    /// Proposta dal report; le voci gia' scritte (`previous`) restano visibili ma spente.
    pub fn from_report(report: &Report, areas: &[String], previous: &RunResult) -> Self {
        let area = report.area.clone().filter(|a| areas.contains(a));
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
                status: Status::InProgress,
                enabled: false,
                written,
                key,
            };
            entry.enabled = !written && entry.writable();
            touched.push(entry);
        }
        let texts = report
            .missing
            .iter()
            .map(|t| (Kind::Task, t.clone(), None))
            .chain(report.problems.iter().map(|t| (Kind::Question, t.clone(), None)))
            .chain(report.blockers.iter().map(|b| (Kind::Blocker, b.title.clone(), b.waiting_on.clone())));
        let new_issues = texts
            .enumerate()
            .map(|(item, (kind, text, waiting_on))| {
                let key = new_key(item);
                let written = previous.is_written(&key);
                let body = match &waiting_on {
                    Some(w) => format!("{text}\n\nIn attesa di: {w}"),
                    None => text.clone(),
                };
                NewIssue { item, kind, title: title_of(&text), body, area: area.clone(), waiting_on, enabled: !written, written, key }
            })
            .collect();
        let summary = (!touched.is_empty()).then(|| {
            let written = previous.is_written(SUMMARY_KEY);
            SummaryEntry { key: SUMMARY_KEY.into(), body: summary_comment(report), enabled: !written, written }
        });
        Preview { session_id: report.session_id.clone(), repo: None, areas: areas.to_vec(), touched, new_issues, summary }
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
pub fn fully_written(report: &Report, areas: &[String], previous: &RunResult) -> bool {
    !previous.items.is_empty() && actions(&Preview::from_report(report, areas, previous)).is_empty()
}

#[derive(Clone, Debug, PartialEq)]
pub struct IssueDraft {
    pub key: String,
    pub item: usize,
    pub title: String,
    /// Corpo senza marker (lo aggiunge `gh::issue_body`).
    pub body: String,
    pub kind: Kind,
    pub status: Status,
    pub area: Option<String>,
    pub waiting_on: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Issue nuova (o gia' creata da questa sessione: aggiornata).
    Upsert(IssueDraft),
    SetStatus { key: String, number: u64, status: Status, comment: Option<String> },
    Comment { key: String, number: u64, body: String },
    Close { key: String, number: u64, comment: Option<String> },
}

impl Action {
    pub fn key(&self) -> &str {
        match self {
            Action::Upsert(d) => &d.key,
            Action::SetStatus { key, .. } | Action::Comment { key, .. } | Action::Close { key, .. } => key,
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
            kind: n.kind,
            status: n.kind.status(),
            area: n.area.clone(),
            waiting_on: n.waiting_on.clone(),
        }));
    }
    for t in p.touched.iter().filter(|t| t.enabled && !t.written && t.writable()) {
        let note = Some(t.note.trim().to_string()).filter(|s| !s.is_empty());
        match t.action {
            TouchAction::Close => out.push(Action::Close { key: t.key.clone(), number: t.number, comment: note }),
            TouchAction::Update => {
                out.push(Action::SetStatus { key: t.key.clone(), number: t.number, status: t.status, comment: note })
            }
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
        TouchAction::Update => "update the Status",
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
        Action::SetStatus { number, .. } => touched_label(*number, TouchAction::Update),
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
    let p = Preview::from_report(report, &[], &RunResult::default());
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
    Area(usize),
    Summary,
}

/// Controlli sulle voci spuntate prima di scrivere (titoli vuoti o troppo lunghi, corpi enormi,
/// aree che non sono fra quelle della config): il primo campo in errore e il messaggio.
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
        if let Some(area) = n.area.as_ref().filter(|a| !p.areas.contains(a)) {
            return Err((InvalidField::Area(ix), format!("{label}: the area “{area}” is not one of the configured areas")));
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
    use crate::report::parse_report;
    use crate::result::{FoundIssue, ItemReport, RunResult};
    use crate::test_support::{REPORT_JSON, SID};

    fn report() -> Report {
        parse_report(REPORT_JSON, SID).unwrap()
    }

    fn areas() -> Vec<String> {
        vec!["frontend".into(), "backend".into()]
    }

    fn keys(actions: &[Action]) -> Vec<&str> {
        actions.iter().map(Action::key).collect()
    }

    #[test]
    fn preview_from_the_report() {
        let p = Preview::from_report(&report(), &areas(), &RunResult::default());
        assert_eq!(p.session_id, SID);
        let touched: Vec<(u64, TouchAction, &str, bool)> =
            p.touched.iter().map(|t| (t.number, t.action, t.note.as_str(), t.enabled)).collect();
        assert_eq!(touched, [(12, TouchAction::Close, "Import completato", true), (8, TouchAction::Update, "Serve l'accesso allo staging", true)]);
        assert_eq!(p.touched[1].status, Status::InProgress);
        let new: Vec<(usize, Kind, &str)> = p.new_issues.iter().map(|n| (n.item, n.kind, n.title.as_str())).collect();
        assert_eq!(
            new,
            [
                (0, Kind::Task, "Test dell'export PDF su Safari"),
                (1, Kind::Question, "Quale formato per la nota di credito?"),
                (2, Kind::Blocker, "Policy di annullamento"),
            ]
        );
        assert_eq!(p.new_issues.iter().map(|n| n.key.as_str()).collect::<Vec<_>>(), ["new:0", "new:1", "new:2"]);
        assert!(p.new_issues.iter().all(|n| n.enabled && !n.written && n.area.as_deref() == Some("frontend")));
        assert_eq!(p.new_issues[2].body, "Policy di annullamento\n\nIn attesa di: design");
        assert_eq!(p.new_issues[2].waiting_on.as_deref(), Some("design"));
        let summary = p.summary.as_ref().expect("riepilogo sull'issue principale");
        assert_eq!(p.summary_target(), Some(12));
        assert!(summary.body.starts_with(&format!("Riepilogo della sessione superzed `{SID}`\n\n## Fatto\n")), "{}", summary.body);
    }

    #[test]
    fn unknown_area_duplicates_and_empty_comments() {
        let mut r = report();
        r.area = Some("boh".into());
        r.issues_touched.push(crate::report::TouchedIssue { number: 12, action: TouchAction::Comment, note: Some("doppione".into()) });
        r.issues_touched.push(crate::report::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let p = Preview::from_report(&r, &areas(), &RunResult::default());
        assert!(p.new_issues.iter().all(|n| n.area.is_none()), "area fuori dalla config: nessuna");
        assert_eq!(p.touched.iter().map(|t| t.number).collect::<Vec<_>>(), [12, 8, 30], "una voce per issue");
        assert!(!p.touched[2].enabled, "commento senza nota: spento");
        assert!(!keys(&actions(&p)).contains(&"touched:30"));
        let none = Preview::from_report(&Report { session_id: SID.into(), ..Default::default() }, &areas(), &RunResult::default());
        assert!(none.summary.is_none() && actions(&none).is_empty(), "nessuna issue toccata: nessun riepilogo");
    }

    #[test]
    fn actions_follow_the_checks_and_the_edits() {
        let mut p = Preview::from_report(&report(), &areas(), &RunResult::default());
        p.new_issues[1].enabled = false;
        p.new_issues[0].title = "  Test Safari  ".into();
        p.touched[1].status = Status::Blocked;
        let a = actions(&p);
        assert_eq!(keys(&a), ["new:0", "new:2", "touched:12", "touched:8", "summary"]);
        assert_eq!(
            a[0],
            Action::Upsert(IssueDraft {
                key: "new:0".into(),
                item: 0,
                title: "Test Safari".into(),
                body: "Test dell'export PDF su Safari".into(),
                kind: Kind::Task,
                status: Status::Backlog,
                area: Some("frontend".into()),
                waiting_on: None,
            })
        );
        match &a[1] {
            Action::Upsert(d) => assert_eq!((d.kind, d.status, d.waiting_on.as_deref(), d.item), (Kind::Blocker, Status::Blocked, Some("design"), 2)),
            other => panic!("{other:?}"),
        }
        assert_eq!(a[2], Action::Close { key: "touched:12".into(), number: 12, comment: Some("Import completato".into()) });
        assert_eq!(
            a[3],
            Action::SetStatus { key: "touched:8".into(), number: 8, status: Status::Blocked, comment: Some("Serve l'accesso allo staging".into()) }
        );
        assert!(matches!(&a[4], Action::Comment { key, number: 12, .. } if key == SUMMARY_KEY));
        p.summary.as_mut().unwrap().enabled = false;
        assert!(!keys(&actions(&p)).contains(&"summary"));
    }

    #[test]
    fn written_entries_are_never_redone() {
        let mut previous = RunResult::default();
        previous.record(vec![
            ItemReport { key: "new:0".into(), result: Ok("https://github.com/acme/acme_app/issues/42".into()), issue: Some(FoundIssue { number: 42, url: "https://github.com/acme/acme_app/issues/42".into() }) },
            ItemReport { key: "touched:12".into(), result: Ok("#12 closed".into()), issue: None },
            ItemReport { key: "new:1".into(), result: Err("gh: HTTP 502".into()), issue: None },
        ]);
        let p = Preview::from_report(&report(), &areas(), &previous);
        assert!(p.new_issues[0].written && !p.new_issues[0].enabled);
        assert!(!p.new_issues[1].written && p.new_issues[1].enabled, "fallita: si riprova");
        assert!(p.touched[0].written);
        let mut forced = p;
        forced.new_issues[0].enabled = true; // anche riaccesa a mano, una voce scritta non si riscrive
        assert_eq!(keys(&actions(&forced)), ["new:1", "new:2", "touched:8", "summary"]);
    }

    #[test]
    fn validation_of_the_edits() {
        let mut p = Preview::from_report(&report(), &areas(), &RunResult::default());
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
        p.new_issues[0].title = "Test Safari".into();
        p.new_issues[2].body = "x".repeat(BODY_MAX_CHARS + 1);
        assert_eq!(validate_preview(&p), Err("New issue “Policy di annullamento”: the description is longer than 65000 characters".into()));
        assert_eq!(validate_fields(&p).unwrap_err().0, InvalidField::Body(2));
        p.new_issues[2].body = "ok".into();
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
        let mut p = Preview::from_report(&report(), &areas(), &RunResult::default());
        assert_eq!(closes(&actions(&p)), 1);
        p.touched[0].enabled = false;
        assert_eq!(closes(&actions(&p)), 0);
    }

    #[test]
    fn an_edited_comment_is_what_gets_written() {
        let mut p = Preview::from_report(&report(), &areas(), &RunResult::default());
        p.touched[0].note = "  Import completato e verificato  ".into();
        p.touched[1].note = String::new();
        let a = actions(&p);
        assert_eq!(a[3], Action::Close { key: "touched:12".into(), number: 12, comment: Some("Import completato e verificato".into()) });
        assert_eq!(a[4], Action::SetStatus { key: "touched:8".into(), number: 8, status: Status::InProgress, comment: None });
        // un commento puro svuotato non si scrive; riempito si scrive col testo nuovo
        let mut r = report();
        r.issues_touched.push(crate::report::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let mut p = Preview::from_report(&r, &areas(), &RunResult::default());
        p.touched[2].note = "Aggiunto il test".into();
        p.touched[2].enabled = true;
        assert!(actions(&p).contains(&Action::Comment { key: "touched:30".into(), number: 30, body: "Aggiunto il test".into() }));
    }

    #[test]
    fn issue_links_need_the_repo() {
        let mut p = Preview::from_report(&report(), &areas(), &RunResult::default());
        assert_eq!(p.issue_url(12), None);
        p.repo = Some("acme/acme_app".into());
        assert_eq!(p.issue_url(12).as_deref(), Some("https://github.com/acme/acme_app/issues/12"));
        p.repo = Some("  ".into());
        assert_eq!(p.issue_url(12), None);
    }

    #[test]
    fn failure_labels_are_readable() {
        let r = report();
        let mut p = Preview::from_report(&r, &areas(), &RunResult::default());
        p.new_issues[0].title = "Test su Safari".into();
        let a = actions(&p);
        assert_eq!(item_label("new:0", &a, &r), "New issue “Test su Safari”", "il titolo modificato");
        assert_eq!(item_label("touched:12", &a, &r), "#12 (close)");
        assert_eq!(item_label("touched:8", &a, &r), "#8 (update the Status)");
        assert_eq!(item_label(SUMMARY_KEY, &a, &r), "Summary");
        // voci non scritte da questa scrittura (fallite prima e poi spente): dal report
        assert_eq!(item_label("new:1", &[], &r), "New issue “Quale formato per la nota di credito?”");
        assert_eq!(item_label("touched:12", &[], &r), "#12 (close)");
        assert_eq!(item_label(SUMMARY_KEY, &[], &r), "Summary");
        assert_eq!(item_label("boh:1", &[], &r), "boh:1");
        let untitled = Action::Upsert(IssueDraft { title: String::new(), ..match &a[0] { Action::Upsert(d) => d.clone(), _ => unreachable!() } });
        assert_eq!(action_label(&untitled), "New issue without a title");
        assert_eq!(action_label(&Action::Comment { key: "touched:30".into(), number: 30, body: "x".into() }), "#30 (comment)");
    }

    #[test]
    fn the_area_must_be_one_of_the_config() {
        let mut p = Preview::from_report(&report(), &areas(), &RunResult::default());
        p.new_issues[0].area = Some("backend".into());
        p.new_issues[1].area = None;
        assert_eq!(validate_preview(&p), Ok(()), "un'area della config o nessuna");
        p.new_issues[2].area = Some("marketing".into());
        assert_eq!(
            validate_preview(&p),
            Err("New issue “Policy di annullamento”: the area “marketing” is not one of the configured areas".into())
        );
        assert_eq!(validate_fields(&p).unwrap_err().0, InvalidField::Area(2));
        p.new_issues[2].enabled = false;
        assert_eq!(validate_preview(&p), Ok(()), "spenta: non conta");
    }

    // Revisione finale 6: il riepilogo va sulla prima issue toccata spuntata.
    #[test]
    fn the_summary_goes_to_the_first_checked_touched_issue() {
        let mut p = Preview::from_report(&report(), &areas(), &RunResult::default());
        p.touched[0].enabled = false;
        assert_eq!(p.summary_target(), Some(8));
        assert!(matches!(actions(&p).last(), Some(Action::Comment { key, number: 8, .. }) if key == SUMMARY_KEY));
        p.touched[1].enabled = false;
        assert_eq!(p.summary_target(), None);
        assert!(!keys(&actions(&p)).contains(&SUMMARY_KEY), "nessuna issue spuntata: nessun riepilogo");
        let mut previous = RunResult::default();
        previous.record(vec![ItemReport { key: "touched:12".into(), result: Ok("#12 closed".into()), issue: None }]);
        let p = Preview::from_report(&report(), &areas(), &previous);
        assert_eq!(p.summary_target(), Some(12), "gia' scritta: resta l'issue principale");
    }

    // Revisione finale 6: un commento senza nota parte spento e non si puo' accendere.
    #[test]
    fn a_comment_without_a_note_is_not_writable() {
        let mut r = report();
        r.issues_touched.push(crate::report::TouchedIssue { number: 30, action: TouchAction::Comment, note: None });
        let p = Preview::from_report(&r, &areas(), &RunResult::default());
        assert!(!p.touched[2].writable() && !p.touched[2].enabled);
        assert!(p.touched[0].writable() && p.touched[1].writable());
    }

    // Revisione finale 6: rientro in Fatto con tutto scritto.
    #[test]
    fn fully_written_needs_every_default_entry_written() {
        let r = report();
        assert!(!fully_written(&r, &areas(), &RunResult::default()), "niente scritto");
        let mut previous = RunResult::default();
        let ok = |k: &str| ItemReport { key: k.into(), result: Ok("ok".into()), issue: None };
        previous.record(["new:0", "new:1", "new:2", "touched:12", "touched:8"].map(ok).to_vec());
        assert!(!fully_written(&r, &areas(), &previous), "manca il riepilogo");
        previous.record(vec![ok(SUMMARY_KEY)]);
        assert!(fully_written(&r, &areas(), &previous));
        let empty = Report { session_id: SID.into(), ..Default::default() };
        assert!(!fully_written(&empty, &areas(), &RunResult::default()), "report vuoto: niente da dire");
    }

    #[test]
    fn titles_are_the_first_line_and_bounded() {
        assert_eq!(title_of("\n  Prima riga \nseconda"), "Prima riga");
        let t = title_of(&"x".repeat(200));
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        assert!(t.ends_with('…'));
        assert_eq!(Status::InProgress.next(), Status::Blocked);
        assert_eq!(Status::Done.next(), Status::Backlog);
        assert_eq!(Status::ALL.map(Status::name), ["Backlog", "In Progress", "Blocked", "Done"]);
    }
}
