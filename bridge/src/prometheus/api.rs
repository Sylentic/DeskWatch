//! Prometheus HTTP API client: instant queries only.
//!
//! `GET /api/v1/query?query=<promql>` answers
//! `{"status":"success","data":{"resultType":"vector","result":[...]}}`, one
//! entry per series with its labels and a `[timestamp, "value"]` pair.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde::Deserialize;

use crate::source::Secret;

/// Give up on a query after this long, so a hung server cannot stall polling.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One series of an instant query result.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub labels: HashMap<String, String>,
    pub value: f64,
}

impl Sample {
    /// The value of label `name`, if the series has it.
    pub fn label(&self, name: &str) -> Option<&str> {
        self.labels.get(name).map(String::as_str)
    }
}

/// The parts of the answer we read.
#[derive(Deserialize)]
struct Reply {
    status: String,
    data: Option<Data>,
    /// Set when `status` is `error`.
    error: Option<String>,
}

#[derive(Deserialize)]
struct Data {
    #[serde(rename = "resultType")]
    result_type: String,
    result: Vec<Series>,
}

#[derive(Deserialize)]
struct Series {
    metric: HashMap<String, String>,
    /// `[unix timestamp, "value"]`; the value is text so it can be `NaN` or `+Inf`.
    value: (f64, String),
}

/// Prometheus over HTTP, with an optional bearer token.
pub struct HttpApi {
    client: reqwest::Client,
    /// Server address without trailing slash.
    base_url: String,
    token: Option<Secret>,
}

impl HttpApi {
    pub fn new(base_url: &str, token: Option<Secret>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("deskwatch-bridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("cannot build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    /// Run an instant query. Series whose value is `NaN` or infinite are
    /// dropped, since the panel has no use for them.
    ///
    /// A 401 or 403 comes back as a `reqwest::Error`, so
    /// `Health::from_error` can tell a refused token from an unreachable server.
    pub async fn query(&self, promql: &str) -> Result<Vec<Sample>> {
        let mut request = self
            .client
            .get(format!("{}/api/v1/query", self.base_url))
            .query(&[("query", promql)]);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token.expose());
        }
        let response = request.send().await?;
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(response
                .error_for_status()
                .expect_err("401 and 403 are errors")
                .into());
        }
        // Prometheus explains a bad query in the body of its 400 answer.
        let body = response.text().await?;
        let reply: Reply = serde_json::from_str(&body).map_err(|_| {
            anyhow::anyhow!("Prometheus answered HTTP {status} with something other than JSON")
        })?;
        parse(reply, status)
    }
}

fn parse(reply: Reply, status: StatusCode) -> Result<Vec<Sample>> {
    if reply.status != "success" {
        bail!(
            "Prometheus query failed (HTTP {status}): {}",
            reply.error.as_deref().unwrap_or("no reason given")
        );
    }
    let data = reply.data.context("Prometheus answer has no data")?;
    if data.result_type != "vector" {
        bail!("expected a vector result, got {}", data.result_type);
    }
    Ok(data
        .result
        .into_iter()
        .filter_map(|series| {
            let value = series
                .value
                .1
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())?;
            Some(Sample {
                labels: series.metric,
                value,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Health;
    use axum::Router;
    use axum::extract::Query;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use std::net::SocketAddr;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn api(base_url: &str, token: Option<&str>) -> HttpApi {
        HttpApi::new(base_url, token.map(Secret::new)).unwrap()
    }

    #[tokio::test]
    async fn sends_query_and_token_and_parses_vector() {
        let app = Router::new().route(
            "/api/v1/query",
            get(
                |Query(q): Query<HashMap<String, String>>, headers: HeaderMap| async move {
                    assert_eq!(q["query"], "up{job=\"node\"}");
                    assert_eq!(headers["authorization"], "Bearer test-token");
                    (
                        [("content-type", "application/json")],
                        r#"{"status":"success","data":{"resultType":"vector","result":[
                            {"metric":{"instance":"a:9100"},"value":[1790000000.5,"0.25"]},
                            {"metric":{"instance":"b:9100"},"value":[1790000000.5,"NaN"]},
                            {"metric":{"instance":"c:9100"},"value":[1790000000.5,"+Inf"]}
                        ]}}"#,
                    )
                },
            ),
        );
        // A trailing slash on the address is fine.
        let base = format!("{}/", serve(app).await);
        let samples = api(&base, Some("test-token"))
            .query("up{job=\"node\"}")
            .await
            .unwrap();
        assert_eq!(samples.len(), 1, "NaN and Inf series are dropped");
        assert_eq!(samples[0].label("instance"), Some("a:9100"));
        assert_eq!(samples[0].value, 0.25);
    }

    #[tokio::test]
    async fn bad_query_reports_prometheus_message() {
        let app = Router::new().route(
            "/api/v1/query",
            get(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    r#"{"status":"error","errorType":"bad_data","error":"parse error: unexpected end of input"}"#,
                )
            }),
        );
        let err = api(&serve(app).await, None)
            .query("sum(")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("unexpected end of input"));
        assert_eq!(Health::from_error(&err), Health::Unreachable);
    }

    #[tokio::test]
    async fn refused_token_is_auth_failed() {
        let app = Router::new().route(
            "/api/v1/query",
            get(|| async { (StatusCode::UNAUTHORIZED, "nope") }),
        );
        let err = api(&serve(app).await, Some("bad"))
            .query("up")
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::AuthFailed);
    }

    #[tokio::test]
    async fn non_json_and_matrix_answers_are_errors() {
        let app = Router::new().route(
            "/api/v1/query",
            get(|| async { "<html>proxy error</html>" }),
        );
        assert!(api(&serve(app).await, None).query("up").await.is_err());

        let matrix = Reply {
            status: "success".into(),
            data: Some(Data {
                result_type: "matrix".into(),
                result: vec![],
            }),
            error: None,
        };
        assert!(parse(matrix, StatusCode::OK).is_err());
    }

    #[tokio::test]
    async fn unreachable_server() {
        // Port 1 on loopback is closed.
        let err = api("http://127.0.0.1:1", None)
            .query("up")
            .await
            .unwrap_err();
        assert_eq!(Health::from_error(&err), Health::Unreachable);
    }
}
