//! Problemi Zabbix via JSON-RPC (`problem.get`, `event.get`), solo lettura; token API nel
//! Portachiavi, header `Authorization: Bearer` (Zabbix 6.4+).
use crate::item::{Filter, FilterField, FilterKind, Item, Severity, Source, SourceKind, context_block, filter_value, validate_filter};
use crate::secrets::SecretStore;
use anyhow::{anyhow, bail};
use chrono::DateTime;
use futures::AsyncReadExt as _;
use gpui::{App, Task};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Method, Request};
use serde_json::{Value, json};
use std::{rc::Rc, sync::Arc, time::Duration};

/// Indice = livello Zabbix (0 non classificato … 5 disastro); "" = tutte.
pub const SEVERITY_CHOICES: [&str; 6] = ["", "informazione", "avviso", "media", "alta", "disastro"];
pub const TOKEN_MISSING: &str = "Zabbix token missing: set it in the Sources view";
pub const RPC_TIMEOUT: Duration = Duration::from_secs(20);
/// Corpo della risposta RPC, massimo (come `runner::MAX_OUTPUT`): oltre, errore invece di un `Vec` senza limiti.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// Livello Zabbix → gravita': Not classified (0) nota, Information (1) info, Warning (2)
/// e Average (3) avviso, High (4) errore, Disaster (5) critico.
pub fn zabbix_severity(level: u8) -> Severity {
    match level {
        0 => Severity::Note,
        1 => Severity::Info,
        2 | 3 => Severity::Warning,
        4 => Severity::Error,
        _ => Severity::Critical,
    }
}

pub fn severity_label(level: u8) -> &'static str {
    match level {
        0 => "not classified",
        1 => "information",
        2 => "warning",
        3 => "average",
        4 => "high",
        _ => "disaster",
    }
}

pub fn schema() -> Vec<FilterField> {
    vec![
        FilterField { key: "severity".into(), label: "Minimum severity".into(), kind: FilterKind::Choice(SEVERITY_CHOICES.map(String::from).to_vec()), default: String::new() },
        FilterField { key: "groups".into(), label: "Groups (id)".into(), kind: FilterKind::Text, default: String::new() },
        FilterField { key: "text".into(), label: "Text".into(), kind: FilterKind::Text, default: String::new() },
    ]
}

pub fn api_url(base: &str) -> String {
    format!("{}/api_jsonrpc.php", base.trim_end_matches('/'))
}

pub fn event_url(base: &str, eventid: &str, triggerid: &str) -> String {
    format!("{}/tr_events.php?triggerid={triggerid}&eventid={eventid}", base.trim_end_matches('/'))
}

pub fn rpc_body(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params, "id": 1})
}

pub fn problem_params(filter: &Filter) -> Result<Value, String> {
    let schema = schema();
    validate_filter(filter, &schema)?;
    let mut p = json!({
        "output": "extend", "selectTags": "extend", "recent": true,
        "sortfield": ["eventid"], "sortorder": "DESC", "limit": 200
    });
    let sev = filter_value(filter, &schema, "severity");
    if let Some(min) = SEVERITY_CHOICES.iter().position(|c| *c == sev).filter(|ix| *ix > 0) {
        p["severities"] = json!((min as u8..=5).collect::<Vec<u8>>());
    }
    let groups = filter_value(filter, &schema, "groups");
    if !groups.is_empty() {
        let ids: Vec<String> = groups.split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect();
        if ids.iter().any(|g| !g.chars().all(|c| c.is_ascii_digit())) {
            return Err("Groups: only numeric ids separated by commas".into());
        }
        p["groupids"] = json!(ids);
    }
    Ok(p)
}

pub fn event_params(eventid: &str) -> Result<Value, String> {
    if eventid.is_empty() || !eventid.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid event “{eventid}”"));
    }
    Ok(json!({"eventids": [eventid], "output": "extend", "selectTags": "extend", "select_acknowledges": "extend"}))
}

pub fn parse_rpc(text: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "Zabbix: non-JSON response".to_string())?;
    if let Some(e) = v.get("error") {
        let message = e.get("message").and_then(|m| m.as_str()).unwrap_or("error");
        let data = e.get("data").and_then(|d| d.as_str()).unwrap_or("");
        return Err(format!("Zabbix: {message} {data}").trim_end().to_string());
    }
    v.get("result").cloned().ok_or_else(|| "Zabbix: response without result".to_string())
}

pub fn problem_item(v: &Value, base_url: &str) -> Option<Item> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let eventid = v.get("eventid")?.as_str()?.to_string();
    let name = s("name");
    let level: u8 = s("severity").parse().unwrap_or(0);
    let timestamp = s("clock").parse::<i64>().ok().and_then(|c| DateTime::from_timestamp(c, 0));
    let acknowledged = s("acknowledged") == "1";
    let objectid = s("objectid");
    let mut fields = vec![("Severity".to_string(), severity_label(level).to_string()), ("Event".to_string(), eventid.clone())];
    if !objectid.is_empty() {
        fields.push(("Trigger".into(), objectid.clone()));
        fields.push(("URL".into(), event_url(base_url, &eventid, &objectid)));
    }
    for tag in v.get("tags").and_then(|t| t.as_array()).into_iter().flatten() {
        if let (Some(k), Some(val)) = (tag.get("tag").and_then(|x| x.as_str()), tag.get("value").and_then(|x| x.as_str())) {
            fields.push((format!("tag {k}"), val.to_string()));
        }
    }
    let opdata = s("opdata");
    Some(Item {
        id: eventid,
        title: name.clone(),
        subtitle: format!("{} · {}", severity_label(level), if acknowledged { "acknowledged" } else { "not acknowledged" }),
        severity: Some(zabbix_severity(level)),
        timestamp,
        fields,
        body: if opdata.is_empty() { name } else { format!("{name}\n{opdata}") },
    })
}

/// Toglie eventuali credenziali dall'URL configurato: mai nella richiesta, nei link o negli
/// errori. Se l'URL non si analizza, resta com'e' (nessuna sintassi `user:pass@` da spogliare).
fn strip_userinfo(base: &str) -> String {
    match url::Url::parse(base) {
        Ok(mut url) => {
            url.set_username("").ok();
            url.set_password(None).ok();
            url.to_string()
        }
        Err(_) => base.to_string(),
    }
}

pub struct ZabbixSource {
    /// URL gia' riscritto sulla porta locale del tunnel, se c'e'; senza credenziali (`new` le spoglia).
    base_url: String,
    token_ref: Option<String>,
    http: Arc<dyn HttpClient>,
    secrets: Rc<dyn SecretStore>,
}

impl ZabbixSource {
    pub fn new(base_url: String, token_ref: Option<String>, http: Arc<dyn HttpClient>, secrets: Rc<dyn SecretStore>) -> Self {
        Self { base_url: strip_userinfo(&base_url), token_ref, http, secrets }
    }

    fn rpc(&self, method: &'static str, params: Value, cx: &App) -> Task<anyhow::Result<Value>> {
        let Some(token_ref) = self.token_ref.clone() else {
            return Task::ready(Err(anyhow!(TOKEN_MISSING)));
        };
        let token = self.secrets.read(&token_ref, cx);
        let (http, url) = (self.http.clone(), api_url(&self.base_url));
        cx.spawn(async move |_| {
            let token = token.await?.ok_or_else(|| anyhow!(TOKEN_MISSING))?;
            let request = Request::builder()
                .method(Method::POST)
                .uri(url.as_str())
                .header("Content-Type", "application/json-rpc")
                .header("Authorization", format!("Bearer {token}"))
                .timeout(RPC_TIMEOUT)
                .body(AsyncBody::from(rpc_body(method, params).to_string()))
                .map_err(|_| anyhow!("invalid Zabbix request"))?;
            // gli errori del client non contengono gli header: niente token nei messaggi
            let mut response = http.send(request).await.map_err(|e| anyhow!("Zabbix not reachable: {e}"))?;
            let status = response.status();
            let mut bytes = Vec::new();
            response
                .body_mut()
                .take(MAX_BODY as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| anyhow!("Zabbix: response interrupted: {e}"))?;
            if bytes.len() > MAX_BODY {
                bail!("Zabbix: response too large");
            }
            // byte non UTF-8: sostituiti, non un errore
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !status.is_success() {
                bail!("Zabbix HTTP {}", status.as_u16());
            }
            parse_rpc(&text).map_err(|e| anyhow!(e))
        })
    }
}

impl Source for ZabbixSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Zabbix
    }

    fn filter_schema(&self) -> Vec<FilterField> {
        schema()
    }

    fn list(&self, filter: &Filter, cx: &App) -> Task<anyhow::Result<Vec<Item>>> {
        let params = match problem_params(filter) {
            Ok(p) => p,
            Err(e) => return Task::ready(Err(anyhow!(e))),
        };
        let text = filter_value(filter, &schema(), "text").to_lowercase();
        let base = self.base_url.clone();
        let call = self.rpc("problem.get", params, cx);
        cx.spawn(async move |_| {
            let result = call.await?;
            Ok(result
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| problem_item(p, &base))
                .filter(|i| text.is_empty() || i.title.to_lowercase().contains(&text) || i.body.to_lowercase().contains(&text))
                .collect())
        })
    }

    fn detail(&self, id: &str, cx: &App) -> Task<anyhow::Result<Item>> {
        let params = match event_params(id) {
            Ok(p) => p,
            Err(e) => return Task::ready(Err(anyhow!(e))),
        };
        let base = self.base_url.clone();
        let call = self.rpc("event.get", params, cx);
        cx.spawn(async move |_| {
            let result = call.await?;
            let event = result.get(0).ok_or_else(|| anyhow!("event not found"))?;
            let mut item = problem_item(event, &base).ok_or_else(|| anyhow!("unreadable event"))?;
            let acks = event.get("acknowledges").and_then(|a| a.as_array()).cloned().unwrap_or_default();
            if !acks.is_empty() {
                item.body.push_str("\n\nAcknowledgements:");
                for a in acks {
                    let who = a.get("username").and_then(|u| u.as_str()).unwrap_or("?");
                    let msg = a.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    item.body.push_str(&format!("\n- {who}: {msg}"));
                }
            }
            Ok(item)
        })
    }

    fn to_context(&self, item: &Item) -> String {
        format!("Problema Zabbix {}\n{}", item.id, context_block(item))
    }

    fn open_url(&self, item: &Item) -> Option<String> {
        item.field("URL").map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemorySecrets;
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};
    use serde_json::json;

    const PROBLEMS: &str = r#"{"jsonrpc":"2.0","result":[
        {"eventid":"9001","name":"PostgreSQL: troppe connessioni","severity":"4","clock":"1790499600","acknowledged":"0","objectid":"13550","opdata":"98 su 100","tags":[{"tag":"service","value":"app-db"}]},
        {"eventid":"9002","name":"Disco /var quasi pieno","severity":"2","clock":"1790496000","acknowledged":"1","objectid":"13551","tags":[]}
    ],"id":1}"#;

    fn filter(pairs: &[(&str, &str)]) -> Filter {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // `is_config_error` (canvas) riconosce gli errori di configurazione da SOURCES_VIEW.
    #[test]
    fn token_missing_points_to_the_sources_view() {
        assert!(TOKEN_MISSING.contains(crate::item::SOURCES_VIEW));
    }

    #[test]
    fn severities() {
        let got: Vec<Severity> = (0..=5).map(zabbix_severity).collect();
        // Average (3) e High (4) si distinguono: avviso contro errore
        assert_eq!(got, [Severity::Note, Severity::Info, Severity::Warning, Severity::Warning, Severity::Error, Severity::Critical]);
        assert_eq!(severity_label(3), "average");
    }

    #[test]
    fn params_follow_the_filter() {
        let p = problem_params(&filter(&[("severity", "alta"), ("groups", " 12, 13 ")])).unwrap();
        assert_eq!(p["severities"], json!([4, 5]));
        assert_eq!(p["groupids"], json!(["12", "13"]));
        assert_eq!(p["recent"], json!(true));
        assert_eq!(p["selectTags"], json!("extend"));
        assert_eq!(p["limit"], json!(200));
        let all = problem_params(&Filter::new()).unwrap();
        assert!(all.get("severities").is_none() && all.get("groupids").is_none());
        assert!(problem_params(&filter(&[("groups", "12; DROP")])).unwrap_err().contains("Groups"));
        assert!(problem_params(&filter(&[("severity", "urgente")])).is_err());
        assert!(event_params("9001 OR 1").is_err());
        assert_eq!(rpc_body("problem.get", json!({}))["jsonrpc"], "2.0");
    }

    #[test]
    fn rpc_errors_and_items() {
        let err = parse_rpc(r#"{"jsonrpc":"2.0","error":{"code":-32602,"message":"Invalid params.","data":"Not authorized."},"id":1}"#).unwrap_err();
        assert_eq!(err, "Zabbix: Invalid params. Not authorized.");
        assert!(parse_rpc("<html>").is_err());
        let result = parse_rpc(PROBLEMS).unwrap();
        let item = problem_item(&result[0], "https://zabbix.example/").unwrap();
        assert_eq!(item.id, "9001");
        assert_eq!(item.title, "PostgreSQL: troppe connessioni");
        assert_eq!(item.subtitle, "high · not acknowledged");
        assert_eq!(item.severity, Some(Severity::Error));
        assert_eq!(item.timestamp, DateTime::from_timestamp(1790499600, 0));
        assert_eq!(item.field("tag service"), Some("app-db"));
        assert_eq!(item.field("URL"), Some("https://zabbix.example/tr_events.php?triggerid=13550&eventid=9001"));
        assert_eq!(item.body, "PostgreSQL: troppe connessioni\n98 su 100");
        assert!(problem_item(&json!({"name": "senza id"}), "https://z").is_none());
    }

    fn source(http: Arc<dyn HttpClient>, secrets: Rc<MemorySecrets>) -> ZabbixSource {
        ZabbixSource::new("https://zabbix.example".into(), Some("superzed:zb".into()), http, secrets)
    }

    // Fix round 1, punto 2
    #[test]
    fn base_url_credentials_are_stripped() {
        assert_eq!(strip_userinfo("https://user:secret@zabbix.example/"), "https://zabbix.example/");
        assert_eq!(strip_userinfo("https://zabbix.example"), "https://zabbix.example/");
        assert_eq!(strip_userinfo("non un url"), "non un url");
    }

    // Fix round 1, punto 2: le credenziali configurate non arrivano ne' alla richiesta ne' ai link
    #[gpui::test]
    async fn credentials_in_the_base_url_never_reach_the_request_or_links(cx: &mut TestAppContext) {
        let secrets = Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:zb", "tok-zb", cx)).await.unwrap();
        let http = FakeHttpClient::create(|req| async move {
            assert_eq!(req.uri().to_string(), "https://zabbix.example/api_jsonrpc.php");
            Ok(Response::builder().status(200).body(AsyncBody::from(PROBLEMS)).unwrap())
        });
        let s = ZabbixSource::new("https://user:secret@zabbix.example".into(), Some("superzed:zb".into()), http, secrets);
        let items = cx.update(|cx| s.list(&Filter::new(), cx)).await.unwrap();
        assert_eq!(s.open_url(&items[0]).as_deref(), Some("https://zabbix.example/tr_events.php?triggerid=13550&eventid=9001"));
    }

    // Fix round 1, punto 3
    #[gpui::test]
    async fn list_without_a_token_ref_fails_before_any_request(cx: &mut TestAppContext) {
        let http = FakeHttpClient::create(|_| async move { panic!("nessuna richiesta senza token_ref") });
        let s = ZabbixSource::new("https://zabbix.example".into(), None, http, Rc::new(MemorySecrets::default()));
        assert_eq!(cx.update(|cx| s.list(&Filter::new(), cx)).await.unwrap_err().to_string(), TOKEN_MISSING);
    }

    #[gpui::test]
    async fn list_posts_problem_get_with_the_bearer_token(cx: &mut TestAppContext) {
        let secrets = Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:zb", "tok-zb", cx)).await.unwrap();
        let http = FakeHttpClient::create(|req| async move {
            assert_eq!(req.uri().to_string(), "https://zabbix.example/api_jsonrpc.php");
            assert_eq!(req.headers()["Authorization"], "Bearer tok-zb");
            let mut body = String::new();
            req.into_body().read_to_string(&mut body).await.unwrap();
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["method"], "problem.get");
            Ok(Response::builder().status(200).body(AsyncBody::from(PROBLEMS)).unwrap())
        });
        let s = source(http, secrets.clone());
        let items = cx.update(|cx| s.list(&Filter::new(), cx)).await.unwrap();
        assert_eq!(items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["9001", "9002"]);
        let only_disk = cx.update(|cx| s.list(&filter(&[("text", "DISCO")]), cx)).await.unwrap();
        assert_eq!(only_disk.len(), 1);
        assert_eq!(s.open_url(&items[1]).as_deref(), Some("https://zabbix.example/tr_events.php?triggerid=13551&eventid=9002"));
        let no_token = source(FakeHttpClient::with_200_response(), Rc::new(MemorySecrets::default()));
        assert_eq!(cx.update(|cx| no_token.list(&Filter::new(), cx)).await.unwrap_err().to_string(), TOKEN_MISSING);
    }

    // Review Focus 1
    #[gpui::test]
    async fn token_never_appears_in_errors(cx: &mut TestAppContext) {
        let secrets = Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:zb", "tok-SEGRETO", cx)).await.unwrap();
        let down = FakeHttpClient::create(|_| async move { Err(anyhow::anyhow!("connessione rifiutata")) });
        let err = cx.update(|cx| source(down, secrets.clone()).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert!(err.starts_with("Zabbix not reachable") && !err.contains("SEGRETO"), "{err}");
        let bad = FakeHttpClient::create(|_| async move { Ok(Response::builder().status(502).body(AsyncBody::from("bad gateway")).unwrap()) });
        let err = cx.update(|cx| source(bad, secrets.clone()).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert_eq!(err, "Zabbix HTTP 502");
    }

    // Fix round 1, punto 1: byte non UTF-8 sostituiti (non un panico), non JSON valido dopo la sostituzione
    #[gpui::test]
    async fn invalid_utf8_body_is_replaced_not_a_panic(cx: &mut TestAppContext) {
        let secrets = Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:zb", "tok-zb", cx)).await.unwrap();
        let bad_utf8 = FakeHttpClient::create(|_| async move {
            Ok(Response::builder().status(200).body(AsyncBody::from(vec![0xff, 0xfe, b'{', b'}'])).unwrap())
        });
        let err = cx.update(|cx| source(bad_utf8, secrets.clone()).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert_eq!(err, "Zabbix: non-JSON response");
    }

    // Fix round 1, punto 1: risposta oltre il limite rifiutata invece di un `Vec` senza limiti
    #[gpui::test]
    async fn oversized_body_is_refused(cx: &mut TestAppContext) {
        let secrets = Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:zb", "tok-zb", cx)).await.unwrap();
        let huge = FakeHttpClient::create(|_| async move {
            Ok(Response::builder().status(200).body(AsyncBody::from(vec![b'x'; MAX_BODY + 1])).unwrap())
        });
        let err = cx.update(|cx| source(huge, secrets.clone()).list(&Filter::new(), cx)).await.unwrap_err().to_string();
        assert_eq!(err, "Zabbix: response too large");
    }

    #[gpui::test]
    async fn detail_reads_the_event_and_its_acknowledges(cx: &mut TestAppContext) {
        let secrets = Rc::new(MemorySecrets::default());
        cx.update(|cx| secrets.write("superzed:zb", "tok-zb", cx)).await.unwrap();
        let http = FakeHttpClient::create(|req| async move {
            let mut body = String::new();
            req.into_body().read_to_string(&mut body).await.unwrap();
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["method"], "event.get");
            assert_eq!(v["params"]["eventids"], json!(["9001"]));
            let result = json!({"jsonrpc": "2.0", "id": 1, "result": [{
                "eventid": "9001", "name": "PostgreSQL: troppe connessioni", "severity": "4", "clock": "1790499600",
                "acknowledged": "1", "objectid": "13550", "tags": [],
                "acknowledges": [{"clock": "1790500000", "message": "guardo io", "username": "alice"}]
            }]});
            Ok(Response::builder().status(200).body(AsyncBody::from(result.to_string())).unwrap())
        });
        let d = cx.update(|cx| source(http, secrets.clone()).detail("9001", cx)).await.unwrap();
        assert!(d.body.contains("Acknowledgements:\n- alice: guardo io"), "{}", d.body);
    }
}
