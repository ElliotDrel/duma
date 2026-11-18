//! Responses API client (`POST /v1/responses`) and the shared HTTP/2 client.
//!
//! One client for the app's lifetime so the connection stays warm; call [`ResponsesClient::warm`]
//! when the user starts speaking so the handshake is done before the request is ready.
//! Every call returns a [`ResponseOutcome`] with every attempt, the raw body and timing headers, even on failure.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::API_BASE;

const RETRY_DELAY: Duration = Duration::from_millis(250);
/// Response headers worth keeping: request id for support, server time vs network time, rate limits.
const KEPT_HEADERS: &[&str] = &[
    "x-request-id",
    "openai-processing-ms",
    "openai-model",
    "x-ratelimit-remaining-requests",
    "x-ratelimit-remaining-tokens",
    "retry-after",
];

#[derive(Clone)]
pub struct ResponsesClient {
    http: reqwest::Client,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseRequest {
    pub model: String,
    pub instructions: String,
    pub input: String,
    /// `none` is fastest. Only sent when set.
    pub reasoning_effort: Option<String>,
    /// `low` keeps answers short. Only sent when set.
    pub verbosity: Option<String>,
    pub service_tier: Option<String>,
    /// Whether OpenAI keeps the response server-side.
    pub store: bool,
    /// Per-attempt timeout.
    pub timeout_ms: u64,
}

impl ResponseRequest {
    pub fn body(&self) -> Value {
        let mut body = json!({
            "model": self.model,
            "instructions": self.instructions,
            "input": self.input,
            "store": self.store,
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning"] = json!({ "effort": effort });
        }
        if let Some(verbosity) = &self.verbosity {
            body["text"] = json!({ "verbosity": verbosity });
        }
        if let Some(tier) = &self.service_tier {
            body["service_tier"] = json!(tier);
        }
        body
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Attempt {
    pub http_status: Option<u16>,
    pub ms: u64,
    pub error: Option<String>,
    pub headers: Value,
    /// The raw body when it was not JSON (gateway errors).
    pub raw_body: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ResponseOutcome {
    pub text: String,
    /// Set when no usable text came back. The caller decides on a fallback.
    pub error: Option<String>,
    pub response_id: Option<String>,
    pub status: Option<String>,
    pub service_tier: Option<String>,
    pub usage: Option<Value>,
    pub latency_ms: u64,
    pub attempts: Vec<Attempt>,
    pub request: Value,
    pub response: Value,
}

impl Default for ResponsesClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsesClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(300))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(30))
            .http2_keep_alive_interval(Duration::from_secs(20))
            .http2_keep_alive_while_idle(true)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("http client");
        Self { http }
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Opens (or keeps) the pooled connection. Cheap, best-effort.
    pub async fn warm(&self, api_key: &str, model: &str) {
        let _ = self
            .http
            .get(format!("{API_BASE}/models/{model}"))
            .bearer_auth(api_key)
            .timeout(Duration::from_secs(5))
            .send()
            .await;
    }

    pub async fn create(&self, api_key: &str, request: &ResponseRequest) -> ResponseOutcome {
        let body = request.body();
        let started = Instant::now();
        let mut outcome = ResponseOutcome {
            request: body.clone(),
            ..Default::default()
        };
        for attempt_number in 1..=2 {
            let attempt_started = Instant::now();
            let mut attempt = Attempt::default();
            let sent = self
                .http
                .post(format!("{API_BASE}/responses"))
                .bearer_auth(api_key)
                .timeout(Duration::from_millis(request.timeout_ms))
                .json(&body)
                .send()
                .await;
            let retryable = match sent {
                Err(error) => {
                    attempt.error = Some(error.to_string());
                    error.is_connect() || error.is_timeout() || error.is_request()
                }
                Ok(response) => {
                    let status = response.status().as_u16();
                    attempt.http_status = Some(status);
                    attempt.headers = kept_headers(response.headers());
                    let text = response.text().await.unwrap_or_default();
                    match serde_json::from_str::<Value>(&text) {
                        Ok(json) => outcome.response = json,
                        Err(_) => attempt.raw_body = Some(text),
                    }
                    if (200..300).contains(&status) {
                        attempt.ms = attempt_started.elapsed().as_millis() as u64;
                        outcome.attempts.push(attempt);
                        break;
                    }
                    attempt.error = Some(format!(
                        "HTTP {status}: {}",
                        outcome.response["error"]["message"]
                            .as_str()
                            .unwrap_or("no error message")
                    ));
                    status == 408 || status == 409 || status == 429 || status >= 500
                }
            };
            attempt.ms = attempt_started.elapsed().as_millis() as u64;
            outcome.attempts.push(attempt);
            if !retryable || attempt_number == 2 {
                break;
            }
            tokio::time::sleep(RETRY_DELAY).await;
        }
        outcome.latency_ms = started.elapsed().as_millis() as u64;
        let json = &outcome.response;
        outcome.text = output_text(json);
        outcome.response_id = json["id"].as_str().map(str::to_string);
        outcome.status = json["status"].as_str().map(str::to_string);
        outcome.service_tier = json["service_tier"].as_str().map(str::to_string);
        outcome.usage = json.get("usage").cloned();
        let last_error = outcome.attempts.last().and_then(|a| a.error.clone());
        outcome.error = if let Some(error) = last_error {
            Some(error)
        } else if outcome.status.as_deref() != Some("completed") {
            Some(format!(
                "response status {}: {}",
                outcome.status.as_deref().unwrap_or("missing"),
                json["incomplete_details"]
            ))
        } else if outcome.text.trim().is_empty() {
            Some("response contained no text".into())
        } else {
            None
        };
        outcome
    }
}

fn kept_headers(headers: &reqwest::header::HeaderMap) -> Value {
    let mut kept = serde_json::Map::new();
    for name in KEPT_HEADERS {
        if let Some(value) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            kept.insert((*name).to_string(), json!(value));
        }
    }
    Value::Object(kept)
}

/// Concatenates every `output_text` content item.
pub fn output_text(response: &Value) -> String {
    if let Some(text) = response["output_text"].as_str() {
        return text.to_string();
    }
    let mut text = String::new();
    for item in response["output"].as_array().into_iter().flatten() {
        for content in item["content"].as_array().into_iter().flatten() {
            if content["type"] == "output_text" {
                text.push_str(content["text"].as_str().unwrap_or_default());
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_from_output_items() {
        let response = json!({ "output": [
            { "type": "reasoning", "content": [] },
            { "type": "message", "content": [ { "type": "output_text", "text": "Hello." } ] }
        ]});
        assert_eq!(output_text(&response), "Hello.");
    }

    #[test]
    fn optional_fields_only_when_set() {
        let mut request = ResponseRequest {
            model: "gpt-6-luna".into(),
            instructions: "x".into(),
            input: "y".into(),
            reasoning_effort: None,
            verbosity: None,
            service_tier: None,
            store: false,
            timeout_ms: 5000,
        };
        assert!(request.body().get("reasoning").is_none());
        assert!(request.body().get("text").is_none());
        request.reasoning_effort = Some("none".into());
        request.verbosity = Some("low".into());
        assert_eq!(request.body()["reasoning"]["effort"], "none");
        assert_eq!(request.body()["text"]["verbosity"], "low");
    }
}
