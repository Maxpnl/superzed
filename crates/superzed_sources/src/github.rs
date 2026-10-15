//! GitHub in sola lettura via `gh`: board del Project (config claude-pm) unita alle issue
//! del repo per (repo, numero), divisa nei blocchi della visione: Blocker, Problemi,
//! In corso, Da fare, Fatto di recente, Stale. Le bozze della board (DraftIssue, senza
//! numero ne' repo) compaiono come righe con id `bozza:<id della voce>`.
use crate::connections::GithubConfig;
use crate::item::{Filter, FilterField, FilterKind, Item, Severity, Source, SourceKind, context_block, filter_value, validate_filter};
use crate::runner::{RunError, Runner};
use anyhow::anyhow;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use gpui::{App, AppContext as _, Task};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub const PM_CONFIG: &str = ".claude/claude-pm.local.md";
pub const DEFAULT_STALE_DAYS: u32 = 14;
pub const RECENT_DONE_DAYS: i64 = 7;
pub const ISSUE_FIELDS: &str = "number,title,url,state,body,labels,createdAt,updatedAt,closedAt";
pub const DETAIL_FIELDS: &str = "number,title,url,state,body,labels,comments,createdAt,updatedAt,closedAt";
pub const GH_MISSING: &str = "gh not found: install it with “brew install gh”, then run “gh auth login”";
pub const GH_NOT_LOGGED: &str = "gh not logged in: run “gh auth login”";
pub const GH_SCOPE: &str = "gh is missing the “project” scope: run “gh auth refresh -s project”";
pub const DRAFT_PREFIX: &str = "bozza:";
const MAX_COMMENTS: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct PmConfig {
    pub owner: String,
    pub project_number: u32,
    pub repo: String,
    pub stale_days: u32,
}

/// Frontmatter di `claude-pm.local.md`: servono `project_owner`, `project_number`, `repo`
/// (`stale_days` facoltativo); mappe annidate e commenti si saltano.
pub fn parse_pm_config(text: &str) -> Result<PmConfig, String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("claude-pm.local.md: the “---” frontmatter is missing".into());
    }
    let mut values = BTreeMap::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            values.insert(k.trim().to_string(), v.trim().trim_matches(['"', '\'']).to_string());
        }
    }
    if !closed {
        return Err("claude-pm.local.md: frontmatter not closed".into());
    }
    let get = |k: &str| values.get(k).filter(|v| !v.is_empty()).cloned();
    let missing: Vec<&str> = ["project_owner", "project_number", "repo"].into_iter().filter(|k| get(k).is_none()).collect();
    if !missing.is_empty() {
        return Err(format!("claude-pm.local.md: missing {}", missing.join(", ")));
    }
    let number = |k: &str| -> Result<Option<u32>, String> {
        get(k).map(|v| v.parse().map_err(|_| format!("claude-pm.local.md: {k} is not a number"))).transpose()
    };
    Ok(PmConfig {
        owner: get("project_owner").unwrap_or_default(),
        project_number: number("project_number")?.unwrap_or_default(),
        repo: get("repo").unwrap_or_default(),
        stale_days: number("stale_days")?.unwrap_or(DEFAULT_STALE_DAYS),
    })
}

pub fn resolve_config(config: &GithubConfig, project_root: Option<&Path>) -> Result<PmConfig, String> {
    if !config.from_project() {
        return Ok(PmConfig {
            owner: config.owner.clone().unwrap_or_default(),
            project_number: config.project_number.unwrap_or_default(),
            repo: config.repo.clone().unwrap_or_default(),
            stale_days: config.stale_days.unwrap_or(DEFAULT_STALE_DAYS),
        });
    }
    let root = project_root.ok_or_else(|| "the connection reads claude-pm.local.md, but the project has no folder".to_string())?;
    let path = root.join(PM_CONFIG);
    let text = std::fs::read_to_string(&path).map_err(|_| format!("claude-pm configuration missing: {}", path.display()))?;
    let mut cfg = parse_pm_config(&text)?;
    if let Some(days) = config.stale_days {
        cfg.stale_days = days;
    }
    Ok(cfg)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Block {
    Blocker,
    Problems,
    InProgress,
    Todo,
    RecentlyDone,
    Stale,
}

impl Block {
    pub const ALL: [Block; 6] = [Block::Blocker, Block::Problems, Block::InProgress, Block::Todo, Block::RecentlyDone, Block::Stale];

    /// Valore del filtro "Blocco".
    pub fn key(self) -> &'static str {
        match self {
            Block::Blocker => "blocker",
            Block::Problems => "problemi",
            Block::InProgress => "in corso",
            Block::Todo => "da fare",
            Block::RecentlyDone => "fatto di recente",
            Block::Stale => "stale",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Block::Blocker => "Blocker",
            Block::Problems => "Problems",
            Block::InProgress => "In progress",
            Block::Todo => "To do",
            Block::RecentlyDone => "Recently done",
            Block::Stale => "Stale",
        }
    }

    /// Solo Blocker (errore) e Problemi (domanda o bug: avviso) sono gravita'; gli stati
    /// di lavoro sono neutri (lo stato resta scritto nel sottotitolo della riga).
    pub fn severity(self) -> Option<Severity> {
        match self {
            Block::Blocker => Some(Severity::Error),
            Block::Problems => Some(Severity::Warning),
            Block::InProgress | Block::Todo | Block::RecentlyDone | Block::Stale => Some(Severity::Note),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoardEntry {
    /// Id della voce della board per le bozze (DraftIssue); `None` per le issue.
    pub draft_id: Option<String>,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: Option<String>,
    pub open: bool,
    pub body: String,
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub area: Option<String>,
    pub waiting_on: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
}

fn norm_key(k: &str) -> String {
    k.chars().filter(|c| !matches!(c, ' ' | '_')).collect::<String>().to_lowercase()
}

/// Campo custom della board, tollerante alla forma della chiave usata da gh.
fn board_field(item: &Value, name: &str) -> Option<String> {
    let target = norm_key(name);
    let obj = item.as_object()?;
    let (_, v) = obj.iter().find(|(k, _)| norm_key(k) == target)?;
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

fn time_of(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    v.get(key)?.as_str()?.parse().ok()
}

fn labels_of(v: &Value) -> Vec<String> {
    v.get("labels")
        .and_then(|l| l.as_array())
        .map(|a| a.iter().filter_map(|l| l.as_str().or_else(|| l.get("name")?.as_str()).map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn merge_board(project_json: &str, issues_json: &str, repo: &str) -> Result<Vec<BoardEntry>, String> {
    let project: Value = serde_json::from_str(project_json).map_err(|_| "gh project item-list: non-JSON output".to_string())?;
    let issues: Value = serde_json::from_str(issues_json).map_err(|_| "gh issue list: non-JSON output".to_string())?;
    let mut entries: Vec<BoardEntry> = Vec::new();
    for item in project.get("items").and_then(|i| i.as_array()).into_iter().flatten() {
        let content = item.get("content").cloned().unwrap_or(Value::Null);
        match content.get("type").and_then(|t| t.as_str()) {
            Some("Issue") => {}
            Some("DraftIssue") => {
                let Some(id) = str_of(item, "id").or_else(|| str_of(&content, "id")) else {
                    continue;
                };
                entries.push(BoardEntry {
                    draft_id: Some(id),
                    repo: repo.to_string(),
                    title: str_of(&content, "title").or_else(|| str_of(item, "title")).unwrap_or_default(),
                    open: true,
                    body: str_of(&content, "body").unwrap_or_default(),
                    labels: labels_of(item),
                    status: board_field(item, "status"),
                    kind: board_field(item, "kind"),
                    area: board_field(item, "area"),
                    waiting_on: board_field(item, "waiting on"),
                    ..BoardEntry::default()
                });
                continue;
            }
            _ => continue,
        }
        let Some(number) = content.get("number").and_then(|n| n.as_u64()) else {
            continue;
        };
        let entry_repo = content
            .get("repository")
            .or_else(|| item.get("repository"))
            .and_then(|r| r.as_str())
            .map(|r| r.trim_start_matches("https://github.com/").trim_end_matches('/').to_string())
            .unwrap_or_else(|| repo.to_string());
        entries.push(BoardEntry {
            draft_id: None,
            repo: entry_repo,
            number,
            title: str_of(&content, "title").or_else(|| str_of(item, "title")).unwrap_or_default(),
            url: str_of(&content, "url"),
            open: true,
            body: str_of(&content, "body").unwrap_or_default(),
            labels: labels_of(item),
            status: board_field(item, "status"),
            kind: board_field(item, "kind"),
            area: board_field(item, "area"),
            waiting_on: board_field(item, "waiting on"),
            updated_at: None,
            closed_at: None,
        });
    }
    for issue in issues.as_array().into_iter().flatten() {
        let Some(number) = issue.get("number").and_then(|n| n.as_u64()) else {
            continue;
        };
        let ix = match entries.iter().position(|e| e.draft_id.is_none() && e.repo == repo && e.number == number) {
            Some(ix) => ix,
            None => {
                entries.push(BoardEntry { repo: repo.to_string(), number, ..BoardEntry::default() });
                entries.len() - 1
            }
        };
        let e = &mut entries[ix];
        if let Some(t) = str_of(issue, "title") {
            e.title = t;
        }
        e.url = str_of(issue, "url").or(e.url.take());
        e.body = str_of(issue, "body").unwrap_or_default();
        e.open = str_of(issue, "state").as_deref() != Some("CLOSED");
        for l in labels_of(issue) {
            if !e.labels.contains(&l) {
                e.labels.push(l);
            }
        }
        e.updated_at = time_of(issue, "updatedAt");
        e.closed_at = time_of(issue, "closedAt");
    }
    for e in &mut entries {
        if e.title.is_empty() {
            e.title = match e.draft_id {
                Some(_) => "(untitled draft)".to_string(),
                None => format!("#{}", e.number),
            };
        }
    }
    Ok(entries)
}

pub fn blocks(e: &BoardEntry, now: DateTime<Utc>, stale_days: u32) -> Vec<Block> {
    let lower = |s: &Option<String>| s.as_deref().unwrap_or("").to_lowercase();
    let (status, kind) = (lower(&e.status), lower(&e.kind));
    // una bozza in Done non ha date: non compare nemmeno in "Fatto di recente"
    if !e.open || status == "done" {
        return match e.closed_at.or(e.updated_at) {
            Some(t) if now - t <= ChronoDuration::days(RECENT_DONE_DAYS) => vec![Block::RecentlyDone],
            _ => Vec::new(),
        };
    }
    let mut out = Vec::new();
    let blocked = kind == "blocker" || status == "blocked";
    if blocked {
        out.push(Block::Blocker);
    }
    if kind == "question" || e.labels.iter().any(|l| l.eq_ignore_ascii_case("bug")) {
        out.push(Block::Problems);
    }
    if status == "in progress" || status == "in review" {
        out.push(Block::InProgress);
    } else if !blocked {
        out.push(Block::Todo);
    }
    if e.updated_at.is_some_and(|t| now - t > ChronoDuration::days(i64::from(stale_days))) {
        out.push(Block::Stale);
    }
    out
}

pub fn schema() -> Vec<FilterField> {
    let mut options = vec!["tutti".to_string()];
    options.extend(Block::ALL.iter().map(|b| b.key().to_string()));
    vec![
        FilterField { key: "block".into(), label: "Block".into(), kind: FilterKind::Choice(options), default: "tutti".into() },
        FilterField { key: "area".into(), label: "Area".into(), kind: FilterKind::Text, default: String::new() },
        FilterField { key: "text".into(), label: "Text".into(), kind: FilterKind::Text, default: String::new() },
    ]
}

fn entry_item(e: &BoardEntry, blocks: &[Block]) -> Item {
    let first = blocks.first().copied();
    let (id, reference) = match &e.draft_id {
        Some(id) => (format!("{DRAFT_PREFIX}{id}"), "draft".to_string()),
        None => (format!("{}#{}", e.repo, e.number), format!("#{}", e.number)),
    };
    let subtitle: Vec<String> = [first.map(|b| b.label().to_string()), e.area.clone(), Some(reference)].into_iter().flatten().collect();
    let mut fields = vec![("Repo".to_string(), e.repo.clone())];
    if e.draft_id.is_some() {
        fields.push(("Item".into(), "Board draft (not an issue yet)".into()));
    }
    for (k, v) in [("Status", &e.status), ("Kind", &e.kind), ("Area", &e.area), ("Waiting on", &e.waiting_on)] {
        if let Some(v) = v {
            fields.push((k.to_string(), v.clone()));
        }
    }
    if !e.labels.is_empty() {
        fields.push(("Labels".into(), e.labels.join(", ")));
    }
    fields.push(("Blocks".into(), blocks.iter().map(|b| b.label()).collect::<Vec<_>>().join(", ")));
    if let Some(u) = &e.url {
        fields.push(("URL".into(), u.clone()));
    }
    Item {
        id,
        title: e.title.clone(),
        subtitle: subtitle.join(" · "),
        severity: first.and_then(Block::severity),
        timestamp: e.updated_at.or(e.closed_at),
        fields,
        body: e.body.clone(),
    }
}

/// Righe filtrate, per blocco (il primo di ogni elemento) e poi dalla piu' recente.
pub fn board_items(entries: &[BoardEntry], filter: &Filter, now: DateTime<Utc>, stale_days: u32) -> Vec<Item> {
    let schema = schema();
    let block = filter_value(filter, &schema, "block");
    let area = filter_value(filter, &schema, "area").to_lowercase();
    let text = filter_value(filter, &schema, "text").to_lowercase();
    let mut picked: Vec<(Block, Option<DateTime<Utc>>, Item)> = entries
        .iter()
        .filter_map(|e| {
            let bs = blocks(e, now, stale_days);
            let first = *bs.first()?;
            if block != "tutti" && !bs.iter().any(|b| b.key() == block) {
                return None;
            }
            if !area.is_empty() && !e.area.as_deref().unwrap_or("").to_lowercase().contains(&area) {
                return None;
            }
            if !text.is_empty() && !e.title.to_lowercase().contains(&text) && !e.body.to_lowercase().contains(&text) {
                return None;
            }
            Some((first, e.updated_at, entry_item(e, &bs)))
        })
        .collect();
    picked.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    picked.into_iter().map(|(_, _, item)| item).collect()
}

/// Senza la riga `Token scopes` (token a grana fine) non si blocca: parlera' item-list.
pub fn check_scopes(auth_output: &str) -> Result<(), String> {
    let Some(line) = auth_output.lines().find(|l| l.contains("Token scopes:")) else {
        return Ok(());
    };
    let scopes = line.split_once("Token scopes:").map(|(_, s)| s).unwrap_or("");
    let has = scopes.split(',').map(|s| s.trim().trim_matches(['\'', '"'])).any(|s| s == "project" || s == "read:project");
    if has { Ok(()) } else { Err(GH_SCOPE.into()) }
}

pub fn classify_gh_error(stderr: &str, cfg: &PmConfig) -> String {
    let s = stderr.to_lowercase();
    if s.contains("gh auth login") || s.contains("not logged") {
        return GH_NOT_LOGGED.into();
    }
    if s.contains("missing required scopes") || s.contains("read:project") || s.contains("insufficient_scopes") {
        return GH_SCOPE.into();
    }
    if s.contains("could not resolve to a projectv2") || s.contains("could not resolve to an organization") || s.contains("could not resolve to a user") {
        return format!("Project {} of “{}” not found", cfg.project_number, cfg.owner);
    }
    if s.contains("could not resolve to a repository") {
        return format!("repository “{}” not found", cfg.repo);
    }
    let first = stderr.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("unknown error");
    format!("gh: {}", first.chars().take(300).collect::<String>())
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| p.to_string()).collect()
}

pub fn item_list_args(cfg: &PmConfig) -> Vec<String> {
    strings(&["project", "item-list", &cfg.project_number.to_string(), "--owner", &cfg.owner, "--format", "json", "--limit", "500"])
}

pub fn issue_list_args(cfg: &PmConfig) -> Vec<String> {
    strings(&["issue", "list", "--repo", &cfg.repo, "--state", "all", "--limit", "500", "--json", ISSUE_FIELDS])
}

pub fn issue_view_args(repo: &str, number: u64) -> Vec<String> {
    strings(&["issue", "view", &number.to_string(), "--repo", repo, "--json", DETAIL_FIELDS])
}

/// `gh …`: stdout se riesce, altrimenti un errore leggibile.
async fn gh(runner: &Arc<dyn Runner>, args: Vec<String>, cfg: &PmConfig) -> Result<String, String> {
    match runner.run("gh", &args, None).await {
        Err(RunError::NotFound(_)) => Err(GH_MISSING.into()),
        Err(RunError::Failed(e)) => Err(format!("gh: {e}")),
        Ok(out) if out.success => Ok(out.stdout),
        Ok(out) => Err(classify_gh_error(&format!("{}\n{}", out.stderr, out.stdout), cfg)),
    }
}

pub async fn fetch_board(runner: Arc<dyn Runner>, cfg: PmConfig) -> Result<Vec<BoardEntry>, String> {
    // gli scope possono stare su stdout o su stderr a seconda della versione di gh
    let auth = match runner.run("gh", &strings(&["auth", "status"]), None).await {
        Err(RunError::NotFound(_)) => return Err(GH_MISSING.into()),
        Err(RunError::Failed(e)) => return Err(format!("gh: {e}")),
        Ok(out) if !out.success => return Err(classify_gh_error(&format!("{}\n{}", out.stderr, out.stdout), &cfg)),
        Ok(out) => format!("{}\n{}", out.stdout, out.stderr),
    };
    check_scopes(&auth)?;
    let items = gh(&runner, item_list_args(&cfg), &cfg).await?;
    let issues = gh(&runner, issue_list_args(&cfg), &cfg).await?;
    merge_board(&items, &issues, &cfg.repo)
}

/// `gh issue view`: body e gli ultimi commenti.
pub fn parse_issue_detail(json: &str, repo: &str) -> Result<Item, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "gh issue view: non-JSON output".to_string())?;
    let number = v.get("number").and_then(|n| n.as_u64()).ok_or("gh issue view: number missing")?;
    let mut body = str_of(&v, "body").unwrap_or_default();
    let comments = v.get("comments").and_then(|c| c.as_array()).cloned().unwrap_or_default();
    if !comments.is_empty() {
        body.push_str("\n\nComments:");
        let skip = comments.len().saturating_sub(MAX_COMMENTS);
        for c in comments.iter().skip(skip) {
            let who = c.get("author").and_then(|a| a.get("login")).and_then(|l| l.as_str()).unwrap_or("?");
            let when = str_of(c, "createdAt").map(|d| d.chars().take(10).collect::<String>()).unwrap_or_default();
            let text: String = str_of(c, "body").unwrap_or_default().chars().take(1000).collect();
            body.push_str(&format!("\n- {who} ({when}): {text}"));
        }
    }
    let mut fields = Vec::new();
    let labels = labels_of(&v);
    if !labels.is_empty() {
        fields.push(("Labels".to_string(), labels.join(", ")));
    }
    if let Some(u) = str_of(&v, "url") {
        fields.push(("URL".to_string(), u));
    }
    Ok(Item {
        id: format!("{repo}#{number}"),
        title: str_of(&v, "title").unwrap_or_else(|| format!("#{number}")),
        subtitle: format!("#{number}"),
        severity: None,
        timestamp: time_of(&v, "updatedAt"),
        fields,
        body,
    })
}

pub struct GithubSource {
    cfg: PmConfig,
    runner: Arc<dyn Runner>,
    now: Option<DateTime<Utc>>,
}

impl GithubSource {
    pub fn new(cfg: PmConfig, runner: Arc<dyn Runner>) -> Self {
        Self { cfg, runner, now: None }
    }

    /// Solo test: l'ora usata per "Fatto di recente" e "Stale".
    pub fn with_now(mut self, now: DateTime<Utc>) -> Self {
        self.now = Some(now);
        self
    }
}

impl Source for GithubSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Github
    }

    fn filter_schema(&self) -> Vec<FilterField> {
        schema()
    }

    fn list(&self, filter: &Filter, cx: &App) -> Task<anyhow::Result<Vec<Item>>> {
        if let Err(e) = validate_filter(filter, &schema()) {
            return Task::ready(Err(anyhow!(e)));
        }
        let (runner, cfg, filter) = (self.runner.clone(), self.cfg.clone(), filter.clone());
        let now = self.now.unwrap_or_else(Utc::now);
        cx.background_spawn(async move {
            let stale_days = cfg.stale_days;
            let entries = fetch_board(runner, cfg).await.map_err(|e| anyhow!(e))?;
            Ok(board_items(&entries, &filter, now, stale_days))
        })
    }

    fn detail(&self, id: &str, cx: &App) -> Task<anyhow::Result<Item>> {
        // una bozza non ha issue da leggere: chi chiama usa la riga
        if id.starts_with(DRAFT_PREFIX) {
            return Task::ready(Err(anyhow!("“{id}” is a board draft: no details")));
        }
        let parsed = id.rsplit_once('#').and_then(|(r, n)| Some((r.to_string(), n.parse::<u64>().ok()?)));
        let Some((repo, number)) = parsed else {
            return Task::ready(Err(anyhow!("invalid id “{id}”")));
        };
        let (runner, cfg) = (self.runner.clone(), self.cfg.clone());
        cx.background_spawn(async move {
            let json = gh(&runner, issue_view_args(&repo, number), &cfg).await.map_err(|e| anyhow!(e))?;
            parse_issue_detail(&json, &repo).map_err(|e| anyhow!(e))
        })
    }

    fn to_context(&self, item: &Item) -> String {
        if item.id.starts_with(DRAFT_PREFIX) {
            format!("Bozza della board GitHub {} (non ancora un'issue)\n{}", item.id, context_block(item))
        } else {
            format!("Issue GitHub {}\n{}", item.id, context_block(item))
        }
    }

    fn open_url(&self, item: &Item) -> Option<String> {
        item.field("URL").map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{AUTH_NO_SCOPE, fail, github_runner, ok};
    use gpui::TestAppContext;

    fn cfg() -> PmConfig {
        PmConfig { owner: "acme".into(), project_number: 3, repo: "acme/acme_app".into(), stale_days: 14 }
    }

    fn now() -> DateTime<Utc> {
        "2026-09-27T12:00:00Z".parse().unwrap()
    }

    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn pm_config_from_the_frontmatter() {
        let text = "---\nproject_number: 3\nproject_owner: acme\nrepo: \"acme/acme_app\"\nareas:\n  frontend: [\"app/**\"]\nquestion_assignee: Maxpnl\n---\n\nNote.\n";
        assert_eq!(parse_pm_config(text), Ok(PmConfig { owner: "acme".into(), project_number: 3, repo: "acme/acme_app".into(), stale_days: 14 }));
        assert!(parse_pm_config("---\nrepo: a/b\n---\n").unwrap_err().contains("project_owner"));
        assert!(parse_pm_config("niente frontmatter").is_err());
        assert!(parse_pm_config("---\nproject_owner: g\nproject_number: tre\nrepo: a/b\n---\n").unwrap_err().contains("number"));
    }

    #[test]
    fn config_from_the_project_or_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let from_project = GithubConfig::default();
        assert!(resolve_config(&from_project, Some(dir.path())).unwrap_err().contains("missing"));
        assert!(resolve_config(&from_project, None).is_err());
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(dir.path().join(PM_CONFIG), "---\nproject_owner: acme\nproject_number: 3\nrepo: acme/acme_app\nstale_days: 7\n---\n").unwrap();
        assert_eq!(resolve_config(&from_project, Some(dir.path())).unwrap().stale_days, 7);
        let explicit = GithubConfig { owner: Some("o".into()), project_number: Some(1), repo: Some("o/r".into()), stale_days: None };
        assert_eq!(resolve_config(&explicit, None), Ok(PmConfig { owner: "o".into(), project_number: 1, repo: "o/r".into(), stale_days: 14 }));
    }

    #[test]
    fn board_and_issues_are_merged_by_repo_and_number() {
        let entries = merge_board(crate::test_support::PROJECT_ITEMS, crate::test_support::ISSUES, "acme/acme_app").unwrap();
        let keys: Vec<(String, u64)> = entries.iter().map(|e| (e.repo.clone(), e.number)).collect();
        assert_eq!(keys.len(), 9, "due bozze, #8 aggiunta anche se non e' sulla board: {keys:?}");
        let draft = entries.iter().find(|e| e.draft_id.as_deref() == Some("PVTI_6")).unwrap();
        assert_eq!((draft.title.as_str(), draft.body.as_str(), draft.area.as_deref(), draft.number), ("Gestione dei webhook", "Da capire con design.", Some("frontend"), 0));
        let e2 = entries.iter().find(|e| e.number == 2).unwrap();
        assert_eq!((e2.title.as_str(), e2.status.as_deref(), e2.waiting_on.as_deref()), ("Annullamento fattura", Some("Blocked"), Some("design")));
        let e9 = entries.iter().find(|e| e.number == 9).unwrap();
        assert_eq!((e9.repo.as_str(), e9.title.as_str(), e9.open), ("acme/infra", "Ruolo ClickHouse", true));
        let e4 = entries.iter().find(|e| e.number == 4).unwrap();
        assert!(!e4.open);
        assert!(merge_board("non json", "[]", "o/r").is_err());
        let drafts = r#"{"items":[
 {"content":{"id":"DI_1","type":"DraftIssue"},"status":"Backlog"},
 {"content":{"type":"DraftIssue","title":"senza id"}},
 {"id":"PVTI_9","content":{"type":"DraftIssue","title":"fatta"},"status":"Done"}
]}"#;
        let entries = merge_board(drafts, "[]", "o/r").unwrap();
        let got: Vec<(Option<&str>, &str)> = entries.iter().map(|e| (e.draft_id.as_deref(), e.title.as_str())).collect();
        assert_eq!(got, [(Some("DI_1"), "(untitled draft)"), (Some("PVTI_9"), "fatta")], "id della voce o del contenuto; senza id si salta");
        assert!(blocks(&entries[1], now(), 14).is_empty(), "bozza in Done: nessun blocco");
    }

    #[test]
    fn blocks_follow_the_vision() {
        let entries = merge_board(crate::test_support::PROJECT_ITEMS, crate::test_support::ISSUES, "acme/acme_app").unwrap();
        let of = |n: u64| blocks(entries.iter().find(|e| e.number == n).unwrap(), now(), 14);
        assert_eq!(of(1), [Block::InProgress]);
        assert_eq!(of(2), [Block::Blocker]);
        assert_eq!(of(3), [Block::Problems, Block::Todo]);
        assert_eq!(of(4), [Block::RecentlyDone]);
        assert_eq!(of(5), [Block::Todo, Block::Stale]);
        assert_eq!(of(8), [Block::Problems, Block::Todo], "label bug");
        let draft = |id: &str| blocks(entries.iter().find(|e| e.draft_id.as_deref() == Some(id)).unwrap(), now(), 14);
        assert_eq!(draft("PVTI_6"), [Block::Todo], "le bozze non diventano mai stale: non hanno data");
        assert_eq!(draft("PVTI_8"), [Block::InProgress], "In review conta come in corso");
        let later = "2026-10-10T12:00:00Z".parse().unwrap();
        assert!(blocks(entries.iter().find(|e| e.number == 4).unwrap(), later, 14).is_empty(), "fatto da piu' di 7 giorni: sparisce");
    }

    #[gpui::test]
    async fn list_is_in_block_order_and_filters(cx: &mut TestAppContext) {
        let runner = github_runner();
        let source = GithubSource::new(cfg(), runner.clone()).with_now(now());
        let items = cx.update(|cx| source.list(&Filter::new(), cx)).await.unwrap();
        assert_eq!(
            ids(&items),
            [
                "acme/acme_app#2",
                "acme/acme_app#8",
                "acme/acme_app#3",
                "acme/acme_app#1",
                "acme/infra#9",
                "bozza:PVTI_8",
                "acme/acme_app#5",
                "bozza:PVTI_6",
                "acme/acme_app#4"
            ]
        );
        assert_eq!(items[0].subtitle, "Blocker · frontend · #2");
        assert_eq!(items[0].severity, Some(Severity::Error));
        assert_eq!(items[1].severity, Some(Severity::Warning));
        // gli stati di lavoro non sono gravita': neutri (lo stato resta nel sottotitolo)
        assert_eq!(items[8].severity, Some(Severity::Note));
        assert!(items[3].subtitle.starts_with("In progress"), "{}", items[3].subtitle);
        assert_eq!(items[3].severity, Some(Severity::Note));
        assert_eq!(items[7].subtitle, "To do · frontend · draft");
        assert_eq!(items[7].body, "Da capire con design.");
        assert_eq!(source.open_url(&items[7]), None);
        assert_eq!(items[7].field("Repo"), Some("acme/acme_app"));
        assert!(source.to_context(&items[7]).starts_with("Bozza della board GitHub bozza:PVTI_6 (non ancora un'issue)"));
        let before = runner.count("gh");
        assert!(cx.update(|cx| source.detail("bozza:PVTI_6", cx)).await.is_err());
        assert_eq!(runner.count("gh"), before, "il dettaglio di una bozza non chiama gh");
        assert_eq!(items[0].field("Waiting on"), Some("design"));
        assert_eq!(source.open_url(&items[0]).as_deref(), Some("https://github.com/acme/acme_app/issues/2"));
        assert_eq!(runner.count("gh issue list --repo acme/acme_app --state all --limit 500 --json number,title,url,state,body,labels,createdAt,updatedAt,closedAt"), 1);
        let filter = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Filter>();
        let stale = cx.update(|cx| source.list(&filter(&[("block", "stale")]), cx)).await.unwrap();
        assert_eq!(ids(&stale), ["acme/acme_app#5"]);
        let text = cx.update(|cx| source.list(&filter(&[("text", "FATTURA")]), cx)).await.unwrap();
        assert_eq!(ids(&text), ["acme/acme_app#2", "acme/acme_app#8"]);
        let area = cx.update(|cx| source.list(&filter(&[("area", "back")]), cx)).await.unwrap();
        assert_eq!(ids(&area), ["acme/acme_app#3"]);
        let drafts = cx.update(|cx| source.list(&filter(&[("text", "webhook")]), cx)).await.unwrap();
        assert_eq!(ids(&drafts), ["bozza:PVTI_6"]);
        assert!(cx.update(|cx| source.list(&filter(&[("block", "boh")]), cx)).await.is_err());
    }

    #[gpui::test]
    async fn gh_errors_are_explicit(cx: &mut TestAppContext) {
        let err = |runner: std::sync::Arc<crate::test_support::FakeRunner>, cx: &mut TestAppContext| {
            let source = GithubSource::new(cfg(), runner).with_now(now());
            cx.update(|cx| source.list(&Filter::new(), cx))
        };
        let r = crate::test_support::FakeRunner::new();
        r.on("gh", Err(RunError::NotFound("gh".into())));
        assert_eq!(err(r, cx).await.unwrap_err().to_string(), GH_MISSING);
        let r = crate::test_support::FakeRunner::new();
        r.on("gh auth status", fail(1, "You are not logged into any GitHub hosts. To log in, run: gh auth login"));
        assert_eq!(err(r, cx).await.unwrap_err().to_string(), GH_NOT_LOGGED);
        let r = github_runner();
        r.on("gh auth status", ok(AUTH_NO_SCOPE));
        assert_eq!(err(r, cx).await.unwrap_err().to_string(), GH_SCOPE);
        let r = github_runner();
        r.on("gh project item-list 3 --owner acme", fail(1, "GraphQL: Could not resolve to a ProjectV2 with the number 3. (organization.projectV2)"));
        assert_eq!(err(r, cx).await.unwrap_err().to_string(), "Project 3 of “acme” not found");
        let r = github_runner();
        r.on("gh issue list --repo acme/acme_app", fail(1, "HTTP 502: bad gateway\nriprova"));
        assert_eq!(err(r, cx).await.unwrap_err().to_string(), "gh: HTTP 502: bad gateway");
    }

    #[gpui::test]
    async fn detail_adds_the_comments(cx: &mut TestAppContext) {
        let source = GithubSource::new(cfg(), github_runner()).with_now(now());
        let d = cx.update(|cx| source.detail("acme/acme_app#8", cx)).await.unwrap();
        assert_eq!(d.id, "acme/acme_app#8");
        assert!(d.body.starts_with("Stack in allegato."));
        assert!(d.body.contains("- Maxpnl (2026-09-26): Succede solo su Safari"), "{}", d.body);
        assert!(cx.update(|cx| source.detail("senza-numero", cx)).await.is_err());
        let context = source.to_context(&d);
        assert!(context.starts_with("Issue GitHub acme/acme_app#8\nCrash all'export della fattura"), "{context}");
    }
}
