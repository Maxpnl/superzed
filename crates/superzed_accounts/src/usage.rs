//! Quote dall'endpoint `oauth/usage` (nei test: `test_support::FakeFetcher` o `FakeHttpClient`).
use futures::{AsyncReadExt as _, future::BoxFuture};
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Method, Request, StatusCode};
use std::{sync::Arc, time::Duration};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const USAGE_BETA: &str = "oauth-2025-04-20";
pub const USAGE_TIMEOUT: Duration = Duration::from_secs(8);
/// Oltre questa dimensione la risposta si scarta.
pub const MAX_BODY: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum FetchOutcome {
    Ok(serde_json::Value),
    /// 401/403: quote non disponibili per questo account.
    Unauthorized,
    /// Messaggio senza il token.
    Error(String),
}

pub trait UsageFetcher: Send + Sync + 'static {
    fn fetch(&self, token: String) -> BoxFuture<'static, FetchOutcome>;
}

pub struct HttpUsageFetcher {
    client: Arc<dyn HttpClient>,
}

impl HttpUsageFetcher {
    pub fn new(client: Arc<dyn HttpClient>) -> Self {
        Self { client }
    }
}

impl UsageFetcher for HttpUsageFetcher {
    fn fetch(&self, token: String) -> BoxFuture<'static, FetchOutcome> {
        let client = self.client.clone();
        Box::pin(async move {
            let request = Request::builder()
                .method(Method::GET)
                .uri(USAGE_URL)
                .header("authorization", format!("Bearer {token}"))
                .header("anthropic-beta", USAGE_BETA)
                .timeout(USAGE_TIMEOUT)
                .body(AsyncBody::empty());
            let request = match request {
                Ok(r) => r,
                Err(_) => return FetchOutcome::Error("invalid request".into()),
            };
            let response = match client.send(request).await {
                Ok(r) => r,
                // l'errore del client non contiene gli header: niente token
                Err(e) => return FetchOutcome::Error(format!("network: {e}")),
            };
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return FetchOutcome::Unauthorized;
            }
            if !status.is_success() {
                return FetchOutcome::Error(format!("HTTP {}", status.as_u16()));
            }
            // un byte oltre il limite basta per sapere che e' troppo grande
            let mut body = Vec::new();
            if response.into_body().take(MAX_BODY as u64 + 1).read_to_end(&mut body).await.is_err() {
                return FetchOutcome::Error("unreadable response".into());
            }
            if body.len() > MAX_BODY {
                return FetchOutcome::Error("response too large".into());
            }
            match serde_json::from_slice(&body) {
                Ok(v) => FetchOutcome::Ok(v),
                Err(_) => FetchOutcome::Error("non-JSON response".into()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_client::{FakeHttpClient, Response};

    fn fetcher(status: u16, body: &'static str) -> HttpUsageFetcher {
        HttpUsageFetcher::new(FakeHttpClient::create(move |req| async move {
            assert_eq!(req.uri().to_string(), USAGE_URL);
            assert_eq!(req.headers()["authorization"], "Bearer tok-1");
            assert_eq!(req.headers()["anthropic-beta"], USAGE_BETA);
            Ok(Response::builder().status(status).body(AsyncBody::from(body)).unwrap())
        }))
    }

    #[test]
    fn fetch_maps_statuses() {
        let block = |f: HttpUsageFetcher| futures::executor::block_on(f.fetch("tok-1".into()));
        assert_eq!(block(fetcher(200, r#"{"limits":[]}"#)), FetchOutcome::Ok(serde_json::json!({"limits": []})));
        assert_eq!(block(fetcher(401, "")), FetchOutcome::Unauthorized);
        assert_eq!(block(fetcher(403, "")), FetchOutcome::Unauthorized);
        assert_eq!(block(fetcher(500, "boom")), FetchOutcome::Error("HTTP 500".into()));
        assert_eq!(block(fetcher(200, "<html>")), FetchOutcome::Error("non-JSON response".into()));
    }

    #[test]
    fn body_is_capped_at_one_mib() {
        let big = |len: usize| {
            HttpUsageFetcher::new(FakeHttpClient::create(move |_| async move {
                let body = format!("\"{}\"", "x".repeat(len - 2));
                Ok(Response::builder().status(200).body(AsyncBody::from(body)).unwrap())
            }))
        };
        let block = |f: HttpUsageFetcher| futures::executor::block_on(f.fetch("tok".into()));
        assert!(matches!(block(big(MAX_BODY)), FetchOutcome::Ok(_)), "esattamente 1 MiB passa");
        assert_eq!(block(big(MAX_BODY + 1)), FetchOutcome::Error("response too large".into()));
    }

    // Review Focus 3
    #[test]
    fn token_never_appears_in_debug_or_errors() {
        let t = crate::credentials::OauthToken { access_token: "sk-ant-SEGRETO".into(), expires_at: None };
        assert!(!format!("{t:?}").contains("SEGRETO"));
        let failing = HttpUsageFetcher::new(FakeHttpClient::create(|_| async move { Err(anyhow::anyhow!("connessione rifiutata")) }));
        match futures::executor::block_on(failing.fetch("sk-ant-SEGRETO".into())) {
            FetchOutcome::Error(e) => assert!(!e.contains("SEGRETO"), "{e}"),
            other => panic!("{other:?}"),
        }
    }
}
