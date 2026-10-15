//! Log HyperDX (ClickStack) letti da ClickHouse: SQL con valori escapati come letterali,
//! sempre `readonly`, `LIMIT 200`, `JSONEachRow`; trasporto HTTP (anche via tunnel) o
//! `ssh <host> clickhouse-client` con la query su stdin (8123 non pubblicata sull'host).
//! Nessun riferimento a `TimestampTime`: non tutte le tabelle `otel_logs` ce l'hanno (alcune
//! partizionano solo su `toDate(Timestamp)`) e una colonna mancante romperebbe ogni query.
//! I log che arrivano da journald hanno spesso `ServiceName` vuoto: il servizio e'
//! allora l'unita' systemd (`LogAttributes['_SYSTEMD_UNIT']`) e il titolo il suo `MESSAGE`.
use crate::connections::safe_ident;
use crate::item::{Filter, FilterField, FilterKind, Item, SOURCES_VIEW, Severity, Source, SourceKind, context_block, filter_value, parse_window, validate_filter};
use crate::runner::{RunError, Runner};
use crate::secrets::SecretStore;
use anyhow::{anyhow, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use futures::AsyncReadExt as _;
use gpui::{App, AppContext as _, Task};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Method, Request};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const ROW_LIMIT: usize = 200;
/// Orario sempre in UTC (senza `session_timezone`, che un utente readonly non puo' cambiare).
pub const COLUMNS: &str = "toString(Timestamp, 'UTC') AS TimestampUtc, ServiceName, SeverityText, Body, TraceId, SpanId, LogAttributes, ResourceAttributes['host.name'] AS Host";
/// Attributo journald con l'unita' systemd, al posto di `ServiceName` quando e' vuoto.
pub const SYSTEMD_UNIT: &str = "_SYSTEMD_UNIT";
const UNIT_FIELD: &str = "systemd unit";
/// Campi della riga usati anche dalla logica (link a HyperDX).
pub const SERVICE_FIELD: &str = "Service";
pub const SEVERITY_FIELD: &str = "Severity";
/// Sola lettura e tempo massimo applicati dal trasporto (riga di comando qui, parametri
/// dell'URL via HTTP), non da una clausola `SETTINGS` che una sessione readonly rifiuterebbe.
pub const DEFAULT_SSH_COMMAND: &str = "docker compose -f /opt/hyperdx/docker-compose.yml exec -T ch-server clickhouse-client --max_execution_time=30 --readonly=1 -f JSONEachRow";
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Secondi di esecuzione lato server (`max_execution_time`), mai dalla configurazione.
pub const MAX_EXECUTION_SECS: u64 = 30;
/// Finestra della traccia attorno alla riga cliccata.
pub const TRACE_MARGIN_SECS: i64 = 3600;
const MAX_ATTRIBUTES: usize = 20;
const LINK_MARGIN_MS: i64 = 15 * 60 * 1000;
const BAD_LINE_CHARS: usize = 200;

/// Letterale stringa ClickHouse.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Testo cercato con `ILIKE`: `%` e `_` sono letterali (poi `quote`).
pub fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            c => out.push(c),
        }
    }
    out
}

pub fn severity_values(choice: &str) -> Option<&'static [&'static str]> {
    match choice {
        "fatal" => Some(&["fatal", "critical"]),
        "error" => Some(&["error"]),
        "warn" => Some(&["warn", "warning"]),
        "info" => Some(&["info", "information"]),
        "debug" => Some(&["debug", "trace"]),
        _ => None,
    }
}

pub fn severity_of(text: &str) -> Severity {
    let t = text.trim().to_lowercase();
    match t.as_str() {
        "fatal" | "critical" | "crit" | "emerg" | "emergency" | "alert" | "panic" => Severity::Critical,
        "info" | "information" | "informational" | "notice" => Severity::Info,
        _ if t.starts_with("err") => Severity::Error,
        _ if t.starts_with("warn") => Severity::Warning,
        _ => Severity::Note,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogQuery {
    pub database: String,
    pub namespace: Option<String>,
    pub environment: Option<String>,
}

pub fn hyperdx_schema() -> Vec<FilterField> {
    let severities = ["", "fatal", "error", "warn", "info", "debug"].map(String::from).to_vec();
    vec![
        FilterField { key: "service".into(), label: "Service".into(), kind: FilterKind::Text, default: String::new() },
        FilterField { key: "severity".into(), label: "Severity".into(), kind: FilterKind::Choice(severities), default: String::new() },
        FilterField { key: "window".into(), label: "Window".into(), kind: FilterKind::Duration, default: "1h".into() },
        FilterField { key: "text".into(), label: "Text".into(), kind: FilterKind::Text, default: String::new() },
    ]
}

fn scope_conditions(q: &LogQuery) -> Vec<String> {
    let mut conds = Vec::new();
    if let Some(ns) = q.namespace.as_deref().filter(|s| !s.is_empty()) {
        conds.push(format!("ResourceAttributes['service.namespace'] = {}", quote(ns)));
    }
    if let Some(env) = q.environment.as_deref().filter(|s| !s.is_empty()) {
        conds.push(format!("ResourceAttributes['deployment.environment'] = {}", quote(env)));
    }
    conds
}

fn select_sql(database: &str, conds: &[String], order: &str) -> String {
    format!(
        "SELECT {COLUMNS}\nFROM {database}.otel_logs\nWHERE {}\nORDER BY Timestamp {order}\nLIMIT {ROW_LIMIT}\nFORMAT JSONEachRow",
        conds.join("\n  AND ")
    )
}

pub fn build_logs_sql(q: &LogQuery, filter: &Filter) -> Result<String, String> {
    let schema = hyperdx_schema();
    validate_filter(filter, &schema)?;
    if !safe_ident(&q.database) {
        return Err(format!("invalid database “{}”", q.database));
    }
    let secs = parse_window(&filter_value(filter, &schema, "window"))?;
    let mut conds = vec![format!("Timestamp >= now() - INTERVAL {secs} SECOND")];
    conds.extend(scope_conditions(q));
    let service = filter_value(filter, &schema, "service");
    if !service.is_empty() {
        // `store-manager` trova anche l'unita' journald `store-manager.service`
        conds.push(format!(
            "(ServiceName = {} OR LogAttributes['{SYSTEMD_UNIT}'] IN ({}, {}))",
            quote(&service),
            quote(&service),
            quote(&format!("{service}.service"))
        ));
    }
    if let Some(values) = severity_values(&filter_value(filter, &schema, "severity")) {
        let list: Vec<String> = values.iter().map(|v| quote(v)).collect();
        conds.push(format!("lower(SeverityText) IN ({})", list.join(", ")));
    }
    let text = filter_value(filter, &schema, "text");
    if !text.is_empty() {
        conds.push(format!("Body ILIKE {}", quote(&format!("%{}%", like_escape(&text)))));
    }
    Ok(select_sql(&q.database, &conds, "DESC"))
}

/// I log della traccia entro ±1 h dalla riga (`around`): niente scansione di tutta la tabella.
pub fn build_trace_sql(q: &LogQuery, trace_id: &str, around: DateTime<Utc>) -> Result<String, String> {
    if !safe_ident(&q.database) {
        return Err(format!("invalid database “{}”", q.database));
    }
    let secs = around.timestamp();
    let mut conds = vec![
        format!("Timestamp >= toDateTime({})", secs - TRACE_MARGIN_SECS),
        format!("Timestamp <= toDateTime({})", secs + TRACE_MARGIN_SECS + 1),
        format!("TraceId = {}", quote(trace_id)),
    ];
    conds.extend(scope_conditions(q));
    Ok(select_sql(&q.database, &conds, "ASC"))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogRow {
    pub timestamp: String,
    pub service: String,
    /// `service` viene da `_SYSTEMD_UNIT` (riga journald senza `ServiceName`).
    pub systemd_unit: bool,
    pub host: String,
    pub severity_text: String,
    pub body: String,
    pub trace_id: String,
    pub span_id: String,
    pub log_attributes: BTreeMap<String, String>,
}

pub fn parse_rows(text: &str) -> Result<Vec<LogRow>, String> {
    let mut rows = Vec::new();
    for (ix, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        // ClickHouse accoda l'eccezione a una risposta 200 gia' iniziata: mostrarne l'inizio
        let v: Value = serde_json::from_str(line).map_err(|_| {
            let start: String = line.trim().chars().take(BAD_LINE_CHARS).collect();
            format!("ClickHouse response: line {} is not JSON: {start}", ix + 1)
        })?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let log_attributes: BTreeMap<String, String> = v
            .get("LogAttributes")
            .and_then(|a| a.as_object())
            .map(|m| m.iter().map(|(k, val)| (k.clone(), val.as_str().map(str::to_string).unwrap_or_else(|| val.to_string()))).collect())
            .unwrap_or_default();
        let service = s("ServiceName");
        let unit = if service.is_empty() { log_attributes.get(SYSTEMD_UNIT).cloned().filter(|u| !u.is_empty()) } else { None };
        rows.push(LogRow {
            timestamp: s("TimestampUtc"),
            systemd_unit: unit.is_some(),
            service: unit.unwrap_or(service),
            host: s("Host"),
            severity_text: s("SeverityText"),
            body: s("Body"),
            trace_id: s("TraceId"),
            span_id: s("SpanId"),
            log_attributes,
        });
    }
    Ok(rows)
}

fn parse_ch_time(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f").ok().map(|n| n.and_utc())
}

/// Testo della riga: per journald Body e' il JSON dell'evento e il testo sta in MESSAGE.
fn row_text(row: &LogRow) -> &str {
    match row.log_attributes.get("MESSAGE") {
        Some(m) if !m.trim().is_empty() && row.body.trim_start().starts_with('{') => m,
        _ => &row.body,
    }
}

pub fn row_item(ix: usize, row: &LogRow) -> Item {
    let first_line: String = row_text(row).lines().next().unwrap_or("").chars().take(160).collect();
    let mut fields = vec![(SERVICE_FIELD.to_string(), row.service.clone()), (SEVERITY_FIELD.to_string(), row.severity_text.clone())];
    if row.systemd_unit {
        fields.push((UNIT_FIELD.into(), row.service.clone()));
    }
    if !row.host.is_empty() {
        fields.push(("Host".into(), row.host.clone()));
    }
    if !row.trace_id.is_empty() {
        fields.push(("TraceId".into(), row.trace_id.clone()));
    }
    if !row.span_id.is_empty() {
        fields.push(("SpanId".into(), row.span_id.clone()));
    }
    for (k, v) in row.log_attributes.iter().take(MAX_ATTRIBUTES) {
        fields.push((format!("attr {k}"), v.clone()));
    }
    Item {
        id: format!("{ix}|{}|{}", row.timestamp, row.trace_id),
        title: if first_line.is_empty() { "(empty)".into() } else { first_line },
        subtitle: [row.service.as_str(), row.host.as_str(), row.severity_text.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
        severity: Some(severity_of(&row.severity_text)),
        timestamp: parse_ch_time(&row.timestamp),
        fields,
        body: row.body.clone(),
    }
}

/// Ricerca equivalente nell'interfaccia di HyperDX: la traccia (o il servizio) ±15 minuti.
pub fn hyperdx_url(ui_url: &str, item: &Item) -> Option<String> {
    let mut url = url::Url::parse(ui_url).ok()?;
    let lucene = match item.field("TraceId") {
        Some(t) => format!("TraceId:\"{}\"", t.replace('"', "\\\"")),
        None => {
            let service = item.field(SERVICE_FIELD).filter(|s| !s.is_empty())?.replace('"', "\\\"");
            match item.field(UNIT_FIELD) {
                Some(_) => format!("LogAttributes.{SYSTEMD_UNIT}:\"{service}\""),
                None => format!("ServiceName:\"{service}\""),
            }
        }
    };
    let ts = item.timestamp?.timestamp_millis();
    // HyperDX servito sotto un percorso (`/app`): la ricerca sta sotto quello
    let path = format!("{}/search", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut()
        .append_pair("where", &lucene)
        .append_pair("whereLanguage", "lucene")
        .append_pair("from", &(ts - LINK_MARGIN_MS).to_string())
        .append_pair("to", &(ts + LINK_MARGIN_MS).to_string())
        .append_pair("isLive", "false");
    Some(url.to_string())
}

/// Un comando ssh personalizzato deve aprire clickhouse-client in sola lettura.
pub fn check_ssh_command(command: &str) -> Result<(), String> {
    let mut readonly = command.split_whitespace().filter(|t| t.starts_with("--readonly"));
    match (readonly.next(), readonly.next()) {
        (Some("--readonly=1"), None) => Ok(()),
        _ => Err("the SSH command must contain --readonly=1".into()),
    }
}

/// `ssh` senza prompt (BatchMode): l'host non puo' essere un'opzione (`--`).
pub fn ssh_exec_args(host: &str, command: &str) -> Vec<String> {
    ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--", host, command].map(String::from).to_vec()
}

fn first_line(text: &str, max: usize) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(max).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChTransport {
    /// URL gia' riscritto sulla porta locale del tunnel, se c'e'.
    Http { url: String, user: Option<String>, password_ref: Option<String>, database: String },
    SshExec { host: String, command: String },
}

pub struct ClickhouseClient {
    pub transport: ChTransport,
    runner: Arc<dyn Runner>,
    http: Arc<dyn HttpClient>,
    secrets: Rc<dyn SecretStore>,
    /// HTTP: l'utente e' in un profilo readonly=1 e rifiuta `max_execution_time` nell'URL;
    /// scoperto al primo rifiuto e condiviso per la connessione (vedi `ConnectionStore`).
    readonly_fallback: Arc<AtomicBool>,
}

impl ClickhouseClient {
    pub fn new(transport: ChTransport, runner: Arc<dyn Runner>, http: Arc<dyn HttpClient>, secrets: Rc<dyn SecretStore>) -> Self {
        Self { transport, runner, http, secrets, readonly_fallback: Arc::default() }
    }

    pub fn with_readonly_fallback(mut self, flag: Arc<AtomicBool>) -> Self {
        self.readonly_fallback = flag;
        self
    }

    /// Esegue la query e restituisce il testo `JSONEachRow`.
    pub fn query(&self, sql: String, cx: &App) -> Task<anyhow::Result<String>> {
        match &self.transport {
            ChTransport::SshExec { host, command } => {
                if let Err(e) = check_ssh_command(command) {
                    return Task::ready(Err(anyhow!("ClickHouse via ssh {host}: {e}")));
                }
                let run = self.runner.run("ssh", &ssh_exec_args(host, command), Some(sql));
                let host = host.clone();
                cx.background_spawn(async move {
                    let out = run.await.map_err(|e| match e {
                        RunError::NotFound(_) => anyhow!("ssh not found"),
                        RunError::Failed(e) => anyhow!("ssh {host}: {e}"),
                    })?;
                    if !out.success {
                        bail!("ClickHouse via ssh {host}: {}", first_line(&out.stderr, 300));
                    }
                    Ok(out.stdout)
                })
            }
            ChTransport::Http { url, user, password_ref, database } => {
                let password = password_ref.as_deref().map(|r| self.secrets.read(r, cx));
                let (http, url, user, database) = (self.http.clone(), url.clone(), user.clone(), database.clone());
                let fallback = self.readonly_fallback.clone();
                cx.spawn(async move |_| {
                    let password = match password {
                        Some(task) => Some(task.await?.ok_or_else(|| anyhow!("ClickHouse password missing from the Keychain: set it again in {SOURCES_VIEW}"))?),
                        None => None,
                    };
                    let max_time = !fallback.load(Ordering::Relaxed);
                    let first = http_query(http.clone(), &url, &database, user.as_deref(), password.clone(), sql.clone(), max_time).await;
                    match first {
                        // utente readonly=1: il limite di tempo non si puo' passare; resta readonly=1
                        Err(e) if max_time && rejects_max_execution_time(&e.to_string()) => {
                            fallback.store(true, Ordering::Relaxed);
                            http_query(http, &url, &database, user.as_deref(), password, sql, false).await
                        }
                        other => other,
                    }
                })
            }
        }
    }
}

/// Errore di un utente in un profilo readonly=1 che non puo' cambiare `max_execution_time`
/// (`Code: 164 … Cannot modify 'max_execution_time' setting in readonly mode. (READONLY)`).
fn rejects_max_execution_time(message: &str) -> bool {
    message.contains("Cannot modify 'max_execution_time'") || (message.contains("Code: 164") && message.contains("max_execution_time"))
}

async fn http_query(
    http: Arc<dyn HttpClient>,
    base_url: &str,
    database: &str,
    user: Option<&str>,
    password: Option<String>,
    sql: String,
    max_time: bool,
) -> anyhow::Result<String> {
    let mut url = url::Url::parse(base_url).map_err(|_| anyhow!("invalid ClickHouse URL"))?;
    // credenziali solo negli header: mai nell'URL (ne' nei messaggi d'errore)
    url.set_username("").ok();
    url.set_password(None).ok();
    url.set_path("/");
    url.set_query(None);
    // impostazioni della sessione HTTP, fisse: mai dalla configurazione
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("database", database);
        if max_time {
            pairs.append_pair("max_execution_time", &MAX_EXECUTION_SECS.to_string());
        }
        pairs.append_pair("readonly", "1");
    }
    let mut builder = Request::builder().method(Method::POST).uri(url.as_str()).timeout(QUERY_TIMEOUT);
    if let Some(u) = user {
        builder = builder.header("X-ClickHouse-User", u);
    }
    if let Some(p) = password {
        builder = builder.header("X-ClickHouse-Key", p);
    }
    let request = builder.body(AsyncBody::from(sql)).map_err(|_| anyhow!("invalid ClickHouse request"))?;
    // gli errori del client non contengono gli header: niente password nei messaggi
    let mut response = http.send(request).await.map_err(|e| anyhow!("ClickHouse not reachable: {e}"))?;
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| anyhow!("ClickHouse HTTP {}: response interrupted: {e}", status.as_u16()))?;
    // log con byte non UTF-8: sostituiti, non una risposta vuota
    let body = String::from_utf8_lossy(&bytes).into_owned();
    if !status.is_success() {
        bail!("ClickHouse HTTP {}: {}", status.as_u16(), first_line(&body, 300));
    }
    Ok(body)
}

pub struct HyperdxSource {
    query: LogQuery,
    ui_url: Option<String>,
    client: ClickhouseClient,
    /// Righe dell'ultimo elenco per id: il dettaglio le conserva se la traccia non le contiene.
    listed: Rc<RefCell<BTreeMap<String, LogRow>>>,
}

impl HyperdxSource {
    pub fn new(query: LogQuery, ui_url: Option<String>, client: ClickhouseClient) -> Self {
        Self { query, ui_url, client, listed: Rc::default() }
    }

    fn scope_label(&self) -> String {
        let parts: Vec<&str> = [self.query.namespace.as_deref(), self.query.environment.as_deref()].into_iter().flatten().collect();
        if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(" · ")) }
    }
}

impl Source for HyperdxSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Hyperdx
    }

    fn filter_schema(&self) -> Vec<FilterField> {
        hyperdx_schema()
    }

    fn list(&self, filter: &Filter, cx: &App) -> Task<anyhow::Result<Vec<Item>>> {
        let sql = match build_logs_sql(&self.query, filter) {
            Ok(sql) => sql,
            Err(e) => return Task::ready(Err(anyhow!(e))),
        };
        let query = self.client.query(sql, cx);
        let listed = self.listed.clone();
        cx.spawn(async move |_| {
            let text = query.await?;
            let rows = parse_rows(&text).map_err(|e| anyhow!(e))?;
            let items: Vec<Item> = rows.iter().enumerate().map(|(ix, r)| row_item(ix, r)).collect();
            *listed.borrow_mut() = items.iter().map(|i| i.id.clone()).zip(rows).collect();
            Ok(items)
        })
    }

    /// Tutti i log della stessa traccia, attorno alla riga.
    fn detail(&self, id: &str, cx: &App) -> Task<anyhow::Result<Item>> {
        let mut parts = id.splitn(3, '|');
        let (ix, ts, trace) = (
            parts.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(0),
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
        );
        if trace.is_empty() {
            return Task::ready(Err(anyhow!("line without TraceId: no details")));
        }
        let Some(around) = parse_ch_time(&ts) else {
            return Task::ready(Err(anyhow!("line without a valid time: no details")));
        };
        let sql = match build_trace_sql(&self.query, &trace, around) {
            Ok(sql) => sql,
            Err(e) => return Task::ready(Err(anyhow!(e))),
        };
        let listed = self.listed.borrow().get(id).cloned();
        let query = self.client.query(sql, cx);
        cx.spawn(async move |_| {
            let rows = parse_rows(&query.await?).map_err(|e| anyhow!(e))?;
            let found = rows.iter().find(|r| r.timestamp == ts && r.trace_id == trace);
            let mut item = match (found, listed) {
                (Some(base), _) => row_item(ix, base),
                // la riga cliccata resta quella di partenza, con la traccia sotto
                (None, Some(original)) => {
                    let mut item = row_item(ix, &original);
                    item.body.push_str("\n\n(line not found among the trace logs read)");
                    item
                }
                (None, None) if rows.is_empty() => bail!("trace {trace} empty around {ts}"),
                (None, None) => {
                    let mut item = row_item(ix, &LogRow { timestamp: ts.clone(), trace_id: trace.clone(), ..LogRow::default() });
                    item.title = format!("Trace {trace}");
                    item.severity = None;
                    item.body = "(line not found among the trace logs read)".into();
                    item
                }
            };
            item.body.push_str(&format!("\n\nLogs of the same trace ({}):\n", rows.len()));
            for r in &rows {
                let line: String = row_text(r).lines().next().unwrap_or("").chars().take(200).collect();
                item.body.push_str(&format!("{} {} {}: {line}\n", r.timestamp, r.service, r.severity_text));
            }
            Ok(item)
        })
    }

    fn to_context(&self, item: &Item) -> String {
        format!("Log HyperDX{}\n{}", self.scope_label(), context_block(item))
    }

    fn open_url(&self, item: &Item) -> Option<String> {
        hyperdx_url(self.ui_url.as_deref()?, item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use crate::test_support::{FakeRunner, fail, ok};
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};

    const JOURNALD_ROW: &str = r#"{"TimestampUtc":"2026-09-27 09:40:28.341103000","ServiceName":"","SeverityText":"fatal","Body":"{\"MESSAGE\":\"ops-alerts.service: Deactivated successfully.\",\"PRIORITY\":\"6\"}","TraceId":"","SpanId":"","LogAttributes":{"MESSAGE":"ops-alerts.service: Deactivated successfully.","PRIORITY":"6","_SYSTEMD_UNIT":"init.scope"},"Host":"acme-app-test"}"#;

    pub(crate) const ROWS: &str = concat!(
        r#"{"TimestampUtc":"2026-09-27 10:00:00.123456789","ServiceName":"web-api","SeverityText":"ERROR","Body":"timeout contacting the database\nat db.rs:42","TraceId":"abc123","SpanId":"s1","LogAttributes":{"http.status":"504"},"ResourceAttributes":{"service.namespace":"acme-app"}}"#, "\n",
        r#"{"TimestampUtc":"2026-09-27 09:59:00.000000000","ServiceName":"web-front","SeverityText":"warn","Body":"slow render","TraceId":"","SpanId":"","LogAttributes":{},"ResourceAttributes":{}}"#, "\n",
        r#"{"TimestampUtc":"2026-09-27 09:58:00.000000000","ServiceName":"web-api","SeverityText":"Information","Body":"started","TraceId":"","SpanId":"","LogAttributes":{},"ResourceAttributes":{}}"#, "\n",
    );

    const TRACE_ROWS: &str = concat!(
        r#"{"TimestampUtc":"2026-09-27 09:59:59.900000000","ServiceName":"web-front","SeverityText":"INFO","Body":"POST /checkout","TraceId":"abc123","SpanId":"s0","LogAttributes":{},"ResourceAttributes":{}}"#, "\n",
        r#"{"TimestampUtc":"2026-09-27 10:00:00.123456789","ServiceName":"web-api","SeverityText":"ERROR","Body":"timeout contacting the database\nat db.rs:42","TraceId":"abc123","SpanId":"s1","LogAttributes":{},"ResourceAttributes":{}}"#, "\n",
    );

    fn q() -> LogQuery {
        LogQuery { database: "default".into(), namespace: Some("acme-app".into()), environment: Some("prod".into()) }
    }

    fn filter(pairs: &[(&str, &str)]) -> Filter {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn golden_sql() {
        let sql = build_logs_sql(&q(), &filter(&[("service", "web-api"), ("severity", "error"), ("window", "1h"), ("text", "timeout")])).unwrap();
        assert_eq!(
            sql,
            "SELECT toString(Timestamp, 'UTC') AS TimestampUtc, ServiceName, SeverityText, Body, TraceId, SpanId, LogAttributes, ResourceAttributes['host.name'] AS Host\n\
             FROM default.otel_logs\n\
             WHERE Timestamp >= now() - INTERVAL 3600 SECOND\n  \
             AND ResourceAttributes['service.namespace'] = 'acme-app'\n  \
             AND ResourceAttributes['deployment.environment'] = 'prod'\n  \
             AND (ServiceName = 'web-api' OR LogAttributes['_SYSTEMD_UNIT'] IN ('web-api', 'web-api.service'))\n  \
             AND lower(SeverityText) IN ('error')\n  \
             AND Body ILIKE '%timeout%'\n\
             ORDER BY Timestamp DESC\n\
             LIMIT 200\n\
             FORMAT JSONEachRow"
        );
        let plain = build_logs_sql(&LogQuery { database: "logs_2".into(), namespace: None, environment: None }, &Filter::new()).unwrap();
        assert!(plain.contains("FROM logs_2.otel_logs\nWHERE Timestamp >= now() - INTERVAL 3600 SECOND\nORDER BY"), "{plain}");
        let warn = build_logs_sql(&q(), &filter(&[("severity", "warn")])).unwrap();
        assert!(warn.contains("lower(SeverityText) IN ('warn', 'warning')"));
    }

    #[test]
    fn hostile_input_stays_inside_the_literal() {
        let sql = build_logs_sql(&q(), &filter(&[("service", "x' OR 1=1 --"), ("text", "50%_\\'")])).unwrap();
        assert!(sql.contains(r"(ServiceName = 'x\' OR 1=1 --' OR LogAttributes['_SYSTEMD_UNIT'] IN ('x\' OR 1=1 --', 'x\' OR 1=1 --.service'))"), "{sql}");
        assert!(sql.contains(r"Body ILIKE '%50\\%\\_\\\\\'%'"), "{sql}");
        let sql = build_logs_sql(&q(), &filter(&[("text", "a\nb\0")])).unwrap();
        assert!(sql.contains(r"Body ILIKE '%a\nb\0%'"), "{sql}");
        assert_eq!(quote("it's"), r"'it\'s'");
        assert!(build_logs_sql(&q(), &filter(&[("window", "1 OR 1")])).is_err());
        assert!(build_logs_sql(&q(), &filter(&[("severity", "error') OR ('1")])).is_err());
        let bad_db = LogQuery { database: "default; DROP".into(), namespace: None, environment: None };
        assert!(build_logs_sql(&bad_db, &Filter::new()).unwrap_err().contains("database"));
        let around = "2026-09-27T10:00:00.5Z".parse().unwrap();
        let trace = build_trace_sql(&q(), "abc'--", around).unwrap();
        assert!(
            trace.contains("WHERE Timestamp >= toDateTime(1790499600)\n  AND Timestamp <= toDateTime(1790506801)\n  AND TraceId = 'abc\\'--'\n  AND ResourceAttributes['service.namespace'] = 'acme-app'")
                && trace.ends_with("ORDER BY Timestamp ASC\nLIMIT 200\nFORMAT JSONEachRow"),
            "{trace}"
        );
        assert!(build_trace_sql(&bad_db, "abc", around).is_err());
    }

    #[test]
    fn rows_become_items() {
        let rows = parse_rows(ROWS).unwrap();
        assert_eq!(rows.len(), 3);
        let items: Vec<Item> = rows.iter().enumerate().map(|(ix, r)| row_item(ix, r)).collect();
        assert_eq!(items[0].id, "0|2026-09-27 10:00:00.123456789|abc123");
        assert_eq!(items[0].title, "timeout contacting the database");
        assert_eq!(items[0].subtitle, "web-api · ERROR");
        assert_eq!(items[0].field("TraceId"), Some("abc123"));
        assert_eq!(items[0].field("attr http.status"), Some("504"));
        assert_eq!(items[0].timestamp, "2026-09-27T10:00:00.123456789Z".parse().ok());
        assert_eq!(items.iter().map(|i| i.severity).collect::<Vec<_>>(), [Some(Severity::Error), Some(Severity::Warning), Some(Severity::Info)]);
        let err = parse_rows("{\"a\":1}\nCode: 241. DB::Exception: Memory limit exceeded\n").unwrap_err();
        assert_eq!(err, "ClickHouse response: line 2 is not JSON: Code: 241. DB::Exception: Memory limit exceeded");
        let long = format!("x{}", "y".repeat(500));
        assert_eq!(parse_rows(&long).unwrap_err().chars().count(), "ClickHouse response: line 1 is not JSON: ".chars().count() + 200);
        assert!(parse_rows("\n").unwrap().is_empty());
    }

    #[test]
    fn journald_rows_use_the_systemd_unit_and_message() {
        let row = &parse_rows(JOURNALD_ROW).unwrap()[0];
        assert_eq!((row.service.as_str(), row.systemd_unit, row.host.as_str()), ("init.scope", true, "acme-app-test"));
        let item = row_item(0, row);
        assert_eq!(item.title, "ops-alerts.service: Deactivated successfully.");
        assert_eq!(item.subtitle, "init.scope · acme-app-test · fatal");
        assert_eq!(item.field("systemd unit"), Some("init.scope"));
        assert!(item.body.starts_with("{\"MESSAGE\""), "il corpo resta l'evento intero");
        let link = hyperdx_url("https://hyperdx.example", &item).unwrap();
        assert!(link.starts_with("https://hyperdx.example/search?where=LogAttributes._SYSTEMD_UNIT%3A%22init.scope%22"), "{link}");
        let plain = &parse_rows(ROWS).unwrap()[1];
        assert!(!plain.systemd_unit);
        assert_eq!(row_item(1, plain).subtitle, "web-front · warn");
        let named = &parse_rows(r#"{"ServiceName":"store-manager","Body":"{\"a\":1}","LogAttributes":{"_SYSTEMD_UNIT":"store-manager.service","MESSAGE":" "}}"#).unwrap()[0];
        assert_eq!((named.service.as_str(), named.systemd_unit), ("store-manager", false), "ServiceName vince sull'unita'");
        assert_eq!(row_item(0, named).title, "{\"a\":1}", "MESSAGE vuoto: resta il corpo");
        let nothing = row_item(0, &parse_rows(r#"{"TimestampUtc":"2026-09-27 09:40:28.0","Body":"x"}"#).unwrap()[0]);
        assert_eq!(hyperdx_url("https://hyperdx.example", &nothing), None, "senza traccia ne' servizio nessun link");
    }

    #[test]
    fn severity_mapping() {
        for (text, sev) in [("FATAL", Severity::Critical), ("critical", Severity::Critical), ("Error", Severity::Error), ("err", Severity::Error), ("WARN", Severity::Warning), ("warning", Severity::Warning), ("info", Severity::Info), ("Information", Severity::Info), ("debug", Severity::Note), ("", Severity::Note)] {
            assert_eq!(severity_of(text), sev, "{text}");
        }
    }

    #[test]
    fn hyperdx_links_search_the_trace_around_the_row() {
        let item = row_item(0, &parse_rows(ROWS).unwrap()[0]);
        let link = hyperdx_url("https://hyperdx.example/app/", &item).unwrap();
        assert!(link.starts_with("https://hyperdx.example/app/search?where=TraceId%3A%22abc123%22&whereLanguage=lucene&from="), "{link}");
        assert!(link.ends_with("&isLive=false"), "{link}");
        let url = url::Url::parse(&link).unwrap();
        let pair = |k: &str| url.query_pairs().find(|(n, _)| n == k).unwrap().1.parse::<i64>().unwrap();
        assert_eq!(pair("to") - pair("from"), 30 * 60 * 1000);
        let no_trace = row_item(1, &parse_rows(ROWS).unwrap()[1]);
        let link = hyperdx_url("https://hyperdx.example", &no_trace).unwrap();
        assert!(link.starts_with("https://hyperdx.example/search?where=ServiceName%3A%22web-front%22"), "{link}");
        assert_eq!(hyperdx_url("non un url", &item), None);
    }

    fn ssh_source(runner: std::sync::Arc<FakeRunner>) -> HyperdxSource {
        let client = ClickhouseClient::new(
            ChTransport::SshExec { host: "root@203.0.113.10".into(), command: DEFAULT_SSH_COMMAND.into() },
            runner,
            FakeHttpClient::with_404_response(),
            std::rc::Rc::new(MemorySecrets::default()),
        );
        HyperdxSource::new(q(), Some("https://hyperdx.example".into()), client)
    }

    #[gpui::test]
    async fn ssh_exec_sends_the_query_on_stdin(cx: &mut TestAppContext) {
        let runner = FakeRunner::new();
        runner.on("ssh", ok(ROWS));
        let source = ssh_source(runner.clone());
        let items = cx.update(|cx| source.list(&filter(&[("severity", "error")]), cx)).await.unwrap();
        assert_eq!(items.len(), 3);
        let call = &runner.calls()[0];
        assert_eq!(call.program, "ssh");
        assert_eq!(call.args, ssh_exec_args("root@203.0.113.10", DEFAULT_SSH_COMMAND));
        assert_eq!(call.args[..5], ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--"]);
        let stdin = call.stdin.clone().unwrap();
        assert!(stdin.starts_with("SELECT toString(Timestamp, 'UTC')") && stdin.ends_with("LIMIT 200\nFORMAT JSONEachRow") && !stdin.contains("SETTINGS"), "{stdin}");
        runner.on("ssh", fail(255, "root@203.0.113.10: Permission denied (publickey).\n"));
        let err = cx.update(|cx| source.list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert_eq!(err, "ClickHouse via ssh root@203.0.113.10: root@203.0.113.10: Permission denied (publickey).");
    }

    #[gpui::test]
    async fn detail_reads_the_whole_trace(cx: &mut TestAppContext) {
        let runner = FakeRunner::new();
        runner.on("ssh", ok(TRACE_ROWS));
        let source = ssh_source(runner.clone());
        let d = cx.update(|cx| source.detail("0|2026-09-27 10:00:00.123456789|abc123", cx)).await.unwrap();
        assert_eq!(d.title, "timeout contacting the database");
        assert!(d.body.contains("Logs of the same trace (2):\n2026-09-27 09:59:59.900000000 web-front INFO: POST /checkout\n"), "{}", d.body);
        let stdin = runner.calls()[0].stdin.clone().unwrap();
        assert!(stdin.contains("TraceId = 'abc123'") && stdin.contains("ORDER BY Timestamp ASC"));
        assert!(stdin.contains("Timestamp >= toDateTime(1790499600)"), "{stdin}");
        assert!(cx.update(|cx| source.detail("1|2026-09-27 09:59:00.000000000|", cx)).await.is_err(), "senza TraceId");
        let context = source.to_context(&d);
        assert!(context.starts_with("Log HyperDX (acme-app · prod)\ntimeout contacting the database"), "{context}");
    }

    #[gpui::test]
    async fn detail_keeps_the_clicked_row_when_the_trace_misses_it(cx: &mut TestAppContext) {
        let runner = FakeRunner::new();
        runner.on("ssh", ok(ROWS));
        let source = ssh_source(runner.clone());
        let items = cx.update(|cx| source.list(&Filter::new(), cx)).await.unwrap();
        // la traccia letta non contiene la riga cliccata (solo quella a 09:59:59.9)
        runner.on("ssh", ok(TRACE_ROWS.lines().next().unwrap()));
        let d = cx.update(|cx| source.detail(&items[0].id, cx)).await.unwrap();
        assert_eq!(d.id, items[0].id);
        assert_eq!(d.title, "timeout contacting the database");
        assert!(d.body.starts_with("timeout contacting the database\nat db.rs:42\n\n(line not found among the trace logs read)"), "{}", d.body);
        assert!(d.body.contains("Logs of the same trace (1):\n2026-09-27 09:59:59.900000000 web-front INFO: POST /checkout\n"), "{}", d.body);
        // senza elenco precedente: una riga minima, non un errore
        let fresh = ssh_source(runner.clone());
        let d = cx.update(|cx| fresh.detail("0|2026-09-27 10:00:00.123456789|abc123", cx)).await.unwrap();
        assert_eq!(d.title, "Trace abc123");
        assert!(d.body.starts_with("(line not found"), "{}", d.body);
        runner.on("ssh", ok(""));
        assert!(cx.update(|cx| fresh.detail("0|2026-09-27 10:00:00.123456789|abc123", cx)).await.is_err());
        assert!(cx.update(|cx| fresh.detail("0|non un orario|abc123", cx)).await.is_err());
    }

    fn http_source(http: std::sync::Arc<dyn http_client::HttpClient>, secrets: std::rc::Rc<MemorySecrets>) -> HyperdxSource {
        let client = ClickhouseClient::new(
            ChTransport::Http { url: "http://127.0.0.1:51234".into(), user: Some("reader".into()), password_ref: Some("superzed:ch".into()), database: "default".into() },
            FakeRunner::new(),
            http,
            secrets,
        );
        HyperdxSource::new(q(), None, client)
    }

    #[gpui::test]
    async fn http_posts_the_query_with_user_and_password_headers(cx: &mut TestAppContext) {
        let secrets = std::rc::Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:ch", "SEGRETO", cx)).await.unwrap();
        let http = FakeHttpClient::create(|req| async move {
            assert_eq!(req.method(), http_client::Method::POST);
            assert_eq!(req.uri().to_string(), "http://127.0.0.1:51234/?database=default&max_execution_time=30&readonly=1");
            assert_eq!(req.headers()["X-ClickHouse-User"], "reader");
            assert_eq!(req.headers()["X-ClickHouse-Key"], "SEGRETO");
            let mut body = String::new();
            req.into_body().read_to_string(&mut body).await.unwrap();
            assert!(body.starts_with("SELECT toString(Timestamp, 'UTC')"));
            Ok(Response::builder().status(200).body(AsyncBody::from(ROWS)).unwrap())
        });
        let items = cx.update(|cx| http_source(http, secrets.clone()).list(&Filter::new(), cx)).await.unwrap();
        assert_eq!(items.len(), 3);
        let missing = std::rc::Rc::new(MemorySecrets::default());
        let err = cx.update(|cx| http_source(FakeHttpClient::with_200_response(), missing).list(&Filter::new(), cx)).await.unwrap_err();
        assert!(err.to_string().contains("Keychain"), "{err}");
    }

    #[gpui::test]
    async fn http_body_invalid_utf8_is_replaced_not_dropped(cx: &mut TestAppContext) {
        let secrets = std::rc::Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:ch", "SEGRETO", cx)).await.unwrap();
        let http = FakeHttpClient::create(|_| async move {
            let mut bytes = ROWS.as_bytes().to_vec();
            let at = bytes.windows(7).position(|w| w == b"started").unwrap();
            bytes[at] = 0xff;
            Ok(Response::builder().status(200).body(AsyncBody::from(bytes)).unwrap())
        });
        let items = cx.update(|cx| http_source(http, secrets).list(&Filter::new(), cx)).await.unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].title, "\u{fffd}tarted");
    }

    #[gpui::test]
    async fn http_url_credentials_never_leave_the_config(cx: &mut TestAppContext) {
        let http = FakeHttpClient::create(|req| async move {
            assert_eq!(req.uri().to_string(), "http://127.0.0.1:51234/?database=default&max_execution_time=30&readonly=1");
            Ok(Response::builder().status(500).body(AsyncBody::from("Code: 60. DB::Exception: boom")).unwrap())
        });
        let client = ClickhouseClient::new(
            ChTransport::Http { url: "http://utente:PASSURL@127.0.0.1:51234/x?y=1".into(), user: None, password_ref: None, database: "default".into() },
            FakeRunner::new(),
            http,
            std::rc::Rc::new(MemorySecrets::default()),
        );
        let err = cx.update(|cx| HyperdxSource::new(q(), None, client).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert_eq!(err, "ClickHouse HTTP 500: Code: 60. DB::Exception: boom");
    }

    #[gpui::test]
    async fn a_readonly_user_retries_without_max_execution_time_and_remembers_it(cx: &mut TestAppContext) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = seen.clone();
        let http = FakeHttpClient::create(move |req| {
            let uri = req.uri().to_string();
            log.lock().unwrap().push(uri.clone());
            async move {
                if uri.contains("max_execution_time") {
                    let body = "Code: 164. DB::Exception: Cannot modify 'max_execution_time' setting in readonly mode. (READONLY)";
                    return Ok(Response::builder().status(500).body(AsyncBody::from(body)).unwrap());
                }
                Ok(Response::builder().status(200).body(AsyncBody::from(ROWS)).unwrap())
            }
        });
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client = |http: Arc<dyn HttpClient>| {
            ClickhouseClient::new(
                ChTransport::Http { url: "http://127.0.0.1:51234".into(), user: None, password_ref: None, database: "default".into() },
                FakeRunner::new(),
                http,
                std::rc::Rc::new(MemorySecrets::default()),
            )
            .with_readonly_fallback(flag.clone())
        };
        let items = cx.update(|cx| HyperdxSource::new(q(), None, client(http.clone())).list(&Filter::new(), cx)).await.unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(
            seen.lock().unwrap().clone(),
            ["http://127.0.0.1:51234/?database=default&max_execution_time=30&readonly=1", "http://127.0.0.1:51234/?database=default&readonly=1"]
        );
        // stessa connessione, client nuovo: niente primo tentativo destinato a fallire
        cx.update(|cx| HyperdxSource::new(q(), None, client(http.clone())).list(&Filter::new(), cx)).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 3);
        assert_eq!(seen.lock().unwrap()[2], "http://127.0.0.1:51234/?database=default&readonly=1");
        // altri errori: nessun nuovo tentativo
        let other = Arc::new(std::sync::Mutex::new(0));
        let count = other.clone();
        let failing = FakeHttpClient::create(move |_| {
            *count.lock().unwrap() += 1;
            async move { Ok(Response::builder().status(500).body(AsyncBody::from("Code: 60. DB::Exception: boom")).unwrap()) }
        });
        flag.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(cx.update(|cx| HyperdxSource::new(q(), None, client(failing)).list(&Filter::new(), cx)).await.is_err());
        assert_eq!(*other.lock().unwrap(), 1);
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn ssh_command_must_be_readonly() {
        assert_eq!(check_ssh_command(DEFAULT_SSH_COMMAND), Ok(()));
        assert!(DEFAULT_SSH_COMMAND.contains("--readonly=1 ") && DEFAULT_SSH_COMMAND.contains("--max_execution_time=30"));
        assert_eq!(check_ssh_command("clickhouse-client --readonly=1"), Ok(()));
        for bad in ["clickhouse-client", "clickhouse-client --readonly=0", "clickhouse-client --readonly=2", "clickhouse-client --readonly=1 --readonly=0", "clickhouse-client --readonly 1", "x--readonly=1"] {
            assert_eq!(check_ssh_command(bad), Err("the SSH command must contain --readonly=1".into()), "{bad}");
        }
    }

    #[gpui::test]
    async fn ssh_exec_refuses_a_command_without_readonly(cx: &mut TestAppContext) {
        let runner = FakeRunner::new();
        runner.on("ssh", ok(ROWS));
        let client = ClickhouseClient::new(
            ChTransport::SshExec { host: "box".into(), command: "clickhouse-client -f JSONEachRow".into() },
            runner.clone(),
            FakeHttpClient::with_404_response(),
            std::rc::Rc::new(MemorySecrets::default()),
        );
        let err = cx.update(|cx| HyperdxSource::new(q(), None, client).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert_eq!(err, "ClickHouse via ssh box: the SSH command must contain --readonly=1");
        assert!(runner.calls().is_empty(), "nessun ssh lanciato");
    }

    // Review Focus 1
    #[gpui::test]
    async fn http_errors_never_contain_the_password(cx: &mut TestAppContext) {
        let secrets = std::rc::Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:ch", "SEGRETO", cx)).await.unwrap();
        let refused = FakeHttpClient::create(|_| async move { Err(anyhow::anyhow!("connessione rifiutata")) });
        let err = cx.update(|cx| http_source(refused, secrets.clone()).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert!(err.starts_with("ClickHouse not reachable") && !err.contains("SEGRETO"), "{err}");
        let denied = FakeHttpClient::create(|_| async move {
            Ok(Response::builder().status(516).body(AsyncBody::from("Code: 516. DB::Exception: reader: Authentication failed")).unwrap())
        });
        let err = cx.update(|cx| http_source(denied, secrets.clone()).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert!(err.starts_with("ClickHouse HTTP 516: Code: 516.") && !err.contains("SEGRETO"), "{err}");
    }
}
