//! Reaching a model's upstreams: each in turn, until one answers.
//!
//! A request goes to the model's first upstream that isn't cooling down, and
//! on to the next if that one fails in a way another might not: it can't be
//! reached, it's slow to start a stream, it's overloaded or down, or it
//! refuses our credentials. A refusal of the request itself (a 400) would be
//! the same anywhere, so it goes straight back to the client. Once anything
//! has reached the client, there's no going back.
//!
//! An upstream that fails `routing.failures` times in a row is skipped for
//! `routing.cooldown`, unless every other one is too.

use std::collections::HashMap;
use std::future::Future;
use std::time::Instant;

use axum::http::{header, StatusCode};
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::ApiError;
use crate::app::App;
use crate::config::{Api, Provider, Routing, Upstream};

/// How each upstream has fared lately, by its label.
#[derive(Default)]
pub struct Health {
    upstreams: Mutex<HashMap<String, State>>,
}

#[derive(Default)]
struct State {
    /// Failures in a row.
    failures: u32,
    /// When it's back in turn, if it's out.
    until: Option<Instant>,
}

impl Health {
    /// `upstreams` in the order to try them: as configured, but those
    /// cooling down last.
    pub fn order<'a>(&self, upstreams: &'a [Upstream]) -> Vec<&'a Upstream> {
        let now = Instant::now();
        let states = self.upstreams.lock();
        let cooling = |u: &Upstream| states.get(&u.label()).and_then(|s| s.until).is_some_and(|until| until > now);
        let (mut ready, cooling): (Vec<_>, Vec<_>) = upstreams.iter().partition(|u| !cooling(u));
        ready.extend(cooling);
        ready
    }

    pub fn succeeded(&self, label: &str) {
        self.upstreams.lock().remove(label);
    }

    pub fn failed(&self, label: &str, routing: &Routing) {
        let mut states = self.upstreams.lock();
        let state = states.entry(label.to_string()).or_default();
        state.failures += 1;
        if state.failures >= routing.failures {
            state.failures = 0;
            state.until = Some(Instant::now() + routing.cooldown);
        }
    }
}

/// Why an attempt on one upstream failed: in a way the next might not
/// (`Retry`), or in a way that's the client's to hear (`Final`).
pub enum Failure {
    Retry(ApiError),
    Final(ApiError),
}

/// Try `upstreams` in turn with `attempt`, until one succeeds or fails for
/// good. The result comes with the upstream that gave it.
pub async fn route<'a, T, F, Fut>(app: &'a App, model: &str, upstreams: &'a [Upstream], mut attempt: F) -> Result<(T, &'a Upstream), ApiError>
where
    F: FnMut(&'a Upstream, &'a Provider) -> Fut,
    Fut: Future<Output = Result<T, Failure>>,
{
    let mut last = None;
    for upstream in app.health.order(upstreams) {
        let label = upstream.label();
        // Checked when the config was loaded.
        let Some(provider) = app.config.providers.get(&upstream.provider) else { continue };
        match attempt(upstream, provider).await {
            Ok(value) => {
                app.health.succeeded(&label);
                return Ok((value, upstream));
            }
            Err(Failure::Final(error)) => return Err(error),
            Err(Failure::Retry(error)) => {
                app.health.failed(&label, &app.config.routing);
                app.metrics.upstream_failure(&label);
                crate::log::warn("upstream.failover", json!({ "model": model, "upstream": label, "code": error.body.get("code") }));
                last = Some(error);
            }
        }
    }
    Err(last.unwrap_or_else(|| upstream_failure("the model has no upstreams")))
}

/// POST to a provider, turning its refusals into errors of our own. Nothing
/// of the client's request goes with it but the body we wrote.
pub async fn post(app: &App, provider: &Provider, label: &str, path: &str, body: Vec<u8>) -> Result<reqwest::Response, Failure> {
    let url = format!("{}/{path}", provider.base_url().as_str().trim_end_matches('/'));
    let request = app.http.post(url).header(header::CONTENT_TYPE, "application/json").body(body);
    let request = match provider.api {
        Api::Anthropic => request.header("x-api-key", &provider.api_key).header("anthropic-version", super::anthropic::VERSION),
        Api::Openai | Api::Openrouter => request.bearer_auth(&provider.api_key),
    };
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return Err(Failure::Retry(upstream_failure(&format!("{label}: {error}")))),
    };
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    crate::log::warn("upstream.error", json!({ "upstream": label, "status": status.as_u16(), "body": text.chars().take(2000).collect::<String>() }));
    let error = upstream_error(status, &text);
    Err(if retryable(status) { Failure::Retry(error) } else { Failure::Final(error) })
}

/// Whether another upstream might answer where this one answered `status`:
/// yes if it's overloaded or down, refuses our credentials or credit, or
/// hasn't got the model (OpenRouter's 404 when none of its providers can
/// serve a request); no if it found fault with the request.
fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 402 | 403 | 404 | 408 | 429) || status.is_server_error()
}

/// A provider's error, as the client should see it. Its message is passed on
/// (it usually says what's wrong with the request), but a refusal of our own
/// credentials isn't the client's problem, and a provider's rate limit mustn't
/// look like the client running out of credit.
fn upstream_error(status: StatusCode, body: &str) -> ApiError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let error = parsed.get("error").unwrap_or(&parsed);
    let message = error.get("message").and_then(Value::as_str).filter(|m| !m.is_empty()).unwrap_or("The upstream provider returned an error");
    match status.as_u16() {
        401..=403 => upstream_failure("the upstream provider refused our credentials"),
        429 => ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "api_error", "upstream_rate_limited", "The upstream provider is busy. Try again shortly."),
        _ => {
            let kind = error.get("type").and_then(Value::as_str).unwrap_or("api_error");
            let code = error.get("code").and_then(|c| c.as_str().map(str::to_string).or_else(|| c.as_i64().map(|n| n.to_string())));
            let status = if status.is_server_error() { StatusCode::BAD_GATEWAY } else { status };
            ApiError::new(status, kind, code.as_deref().unwrap_or("upstream_error"), message)
        }
    }
}

pub fn upstream_failure(detail: &str) -> ApiError {
    crate::log::warn("upstream.failure", json!({ "detail": detail }));
    ApiError::new(StatusCode::BAD_GATEWAY, "api_error", "upstream_error", "The upstream provider could not be reached. Try again shortly.")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn maps_upstream_errors() {
        let e = upstream_error(StatusCode::BAD_REQUEST, r#"{"error":{"message":"bad tool","type":"invalid_request_error"}}"#);
        assert_eq!((e.status, e.body["message"].as_str()), (StatusCode::BAD_REQUEST, Some("bad tool")));
        assert_eq!(upstream_error(StatusCode::TOO_MANY_REQUESTS, "").status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(upstream_error(StatusCode::UNAUTHORIZED, "").status, StatusCode::BAD_GATEWAY);
        assert_eq!(upstream_error(StatusCode::INTERNAL_SERVER_ERROR, "oops").status, StatusCode::BAD_GATEWAY);
        assert!(retryable(StatusCode::SERVICE_UNAVAILABLE) && retryable(StatusCode::TOO_MANY_REQUESTS) && retryable(StatusCode::UNAUTHORIZED));
        assert!(!retryable(StatusCode::BAD_REQUEST) && !retryable(StatusCode::PAYLOAD_TOO_LARGE));
    }

    #[test]
    fn puts_failing_upstreams_last() {
        let upstream = |p: &str| Upstream { provider: p.into(), model: "m".into(), params: Default::default() };
        let upstreams = [upstream("a"), upstream("b"), upstream("c")];
        let labels = |order: Vec<&Upstream>| order.iter().map(|u| u.provider.clone()).collect::<Vec<_>>();
        let routing = Routing { failures: 2, cooldown: Duration::from_secs(60), ..Routing::default() };
        let health = Health::default();

        health.failed("a/m", &routing);
        assert_eq!(labels(health.order(&upstreams)), ["a", "b", "c"]);
        health.failed("a/m", &routing);
        assert_eq!(labels(health.order(&upstreams)), ["b", "c", "a"]);
        health.failed("b/m", &routing);
        health.succeeded("b/m");
        health.failed("b/m", &routing);
        assert_eq!(labels(health.order(&upstreams)), ["b", "c", "a"]);
        health.succeeded("a/m");
        assert_eq!(labels(health.order(&upstreams)), ["a", "b", "c"]);

        let brief = Routing { failures: 1, cooldown: Duration::ZERO, ..Routing::default() };
        health.failed("a/m", &brief);
        assert_eq!(labels(health.order(&upstreams)), ["a", "b", "c"]);
    }
}
