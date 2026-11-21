//! `/v1/chat/completions`: proxied to the model's provider, and charged for
//! by the usage it reports.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use super::schema::ChatRequest;
use super::ApiError;
use crate::app::App;
use crate::config::{Price, Provider, TokenPrice};
use crate::money::{decimal_from_json_number, Money};
use crate::store::{self, Generation};

pub async fn completions(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Response, ApiError> {
    let caller = app.authenticate_inference(&method, &uri, &headers, &body).await?;
    let raw = super::json_object(&body)?;
    if !raw.get("model").is_some_and(Value::is_string) {
        return Err(ApiError::invalid("missing_required_parameter", "Missing `model` property").param("model"));
    }
    let mut request: ChatRequest = serde_json::from_value(Value::Object(raw)).map_err(|error| ApiError::invalid("invalid_parameter", format!("Invalid request: {error}")))?;
    let model_id = request.model.clone();
    let not_found = || ApiError::invalid("model_not_found", format!("Model '{model_id}' is not supported")).param("model");
    let model = app.config.models.get(&model_id).ok_or_else(not_found)?;
    let Price::Tokens(price) = &model.price else {
        return Err(ApiError::invalid("invalid_model_type", format!("Model '{model_id}' cannot be used with the chat completions endpoint"))
            .param("model"));
    };
    let (provider_name, upstream_model) = model.upstream();
    let provider = app.config.providers.get(provider_name).ok_or_else(not_found)?;
    let free = model.is_free_chat();

    // Credit is checked before and charged after: a request's cost isn't
    // known until it's done, so a balance can end a little below zero.
    if !free {
        app.check_key_limit(&caller, Money::ZERO).await?;
        let who = caller.pubkey.clone();
        let balance = app.db.call(move |conn| store::balance(conn, &who)).await?;
        if !balance.is_positive() {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "insufficient_quota",
                "insufficient_quota",
                "You have run out of credits. Please add more credits to continue using the service.",
            ));
        }
    }

    let stream = request.is_stream();
    let mut request = request.upstream(upstream_model, provider.openrouter);
    if provider.openrouter {
        // https://openrouter.ai/docs/use-cases/usage-accounting
        request.insert("usage".into(), json!({ "include": true }));
        if upstream_model.starts_with("anthropic/") {
            add_cache_breakpoints(&mut request);
        }
    }

    let upstream_body = serde_json::to_vec(&request).map_err(anyhow::Error::from)?;
    let response = send(&app, provider, "chat/completions", upstream_body.clone()).await?;

    let meter = Meter {
        app: app.clone(),
        api_key_id: caller.api_key_id(),
        pubkey: caller.pubkey,
        model_id,
        upstream_model: upstream_model.to_string(),
        price: TokenPrice { input: price.input, output: price.output },
        free,
        use_reported_cost: provider.openrouter,
        prompt_bytes: upstream_body.len(),
    };

    if stream {
        Ok(stream_response(meter, response))
    } else {
        let mut completion: Value = response.json().await.map_err(|e| upstream_failure(&e.to_string()))?;
        let usage = completion.get("usage").cloned().unwrap_or(Value::Null);
        let content_bytes = completion.pointer("/choices/0/message").map_or(0, |m| m.to_string().len());
        let upstream_id = completion.get("id").and_then(Value::as_str).map(str::to_string);
        meter.check_model(completion.get("model"));
        let charge = meter.charge(&usage, content_bytes);
        meter.record(upstream_id, &charge, false).await;

        let body = json!({
            "id": completion.get("id"),
            "object": completion.get("object"),
            "choices": completion.get_mut("choices").map(Value::take),
            "created": completion.get("created"),
            "model": meter.model_id,
            "usage": charge.usage_json(),
        });
        Ok(Json(body).into_response())
    }
}

/// POST to a provider, turning its refusals into errors of our own. Nothing
/// of the client's request goes with it but the body we wrote.
pub async fn send(app: &App, provider: &Provider, path: &str, body: Vec<u8>) -> Result<reqwest::Response, ApiError> {
    let url = format!("{}/{path}", provider.base_url.as_str().trim_end_matches('/'));
    let request = app.http.post(url).bearer_auth(&provider.api_key).header(header::CONTENT_TYPE, "application/json").body(body);
    let response = request.send().await.map_err(|e| upstream_failure(&e.to_string()))?;
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    crate::log::warn("upstream.error", json!({ "status": status.as_u16(), "body": text.chars().take(2000).collect::<String>() }));
    Err(upstream_error(status, &text))
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
        401 | 403 => upstream_failure("the upstream provider refused our credentials"),
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

/// Mark Anthropic prompt cache breakpoints, through OpenRouter. Anthropic
/// allows four, and caches `tools` → `system` → `messages` in that order, so:
/// the last tool (caching every tool), the system prompt, and the last two
/// conversation messages (caching nearly all, then all, of the history).
///
/// https://openrouter.ai/docs/features/prompt-caching
fn add_cache_breakpoints(request: &mut Map<String, Value>) {
    let ephemeral = json!({ "type": "ephemeral" });
    if let Some(messages) = request.get_mut("messages").and_then(Value::as_array_mut) {
        // Breakpoints go on content blocks, so string content becomes one.
        for message in messages.iter_mut() {
            if message.get("role").and_then(Value::as_str) == Some("function") {
                continue;
            }
            if let Some(text) = message.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_string) {
                message["content"] = json!([{ "type": "text", "text": text }]);
            }
        }
        let system = messages.iter().position(|m| m.get("role").and_then(Value::as_str) == Some("system"));
        let others: Vec<usize> = (0..messages.len()).filter(|i| messages[*i].get("role").and_then(Value::as_str) != Some("system")).collect();
        let marked = system.into_iter().chain(others.iter().rev().take(2).copied());
        for i in marked {
            if let Some(blocks) = messages[i].get_mut("content").and_then(Value::as_array_mut)
                && let Some(block) = blocks.iter_mut().rev().find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            {
                block["cache_control"] = ephemeral.clone();
            }
        }
    }
    if let Some(last) = request.get_mut("tools").and_then(Value::as_array_mut).and_then(|t| t.last_mut())
        && last.is_object()
    {
        last["cache_control"] = ephemeral;
    }
}

/// What one completion is charged, and by what.
struct Meter {
    app: Arc<App>,
    pubkey: String,
    api_key_id: Option<String>,
    model_id: String,
    /// The provider's id for it, to notice a provider answering with another.
    upstream_model: String,
    price: TokenPrice,
    free: bool,
    /// OpenRouter reports what each request actually cost (cache discounts
    /// and all); with it, that's the charge rather than the list price.
    use_reported_cost: bool,
    prompt_bytes: usize,
}

struct Charge {
    prompt_tokens: u64,
    completion_tokens: u64,
    cost: Money,
    estimated: bool,
}

impl Charge {
    fn usage_json(&self) -> Value {
        json!({
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.prompt_tokens + self.completion_tokens,
            "cost": self.cost.to_f64(),
        })
    }
}

/// About four bytes to a token, near enough for a charge that would otherwise
/// be nothing.
fn estimate_tokens(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

impl Meter {
    /// The charge for a reported `usage`, or an estimate from the bytes sent
    /// and received if there's none.
    fn charge(&self, usage: &Value, completion_bytes: usize) -> Charge {
        let tokens = |name: &str| usage.get(name).and_then(Value::as_u64);
        let (prompt_tokens, completion_tokens, estimated) = match (tokens("prompt_tokens"), tokens("completion_tokens")) {
            (Some(p), Some(c)) => (p, c, false),
            _ => (estimate_tokens(self.prompt_bytes), estimate_tokens(completion_bytes), true),
        };
        let reported = usage
            .get("cost")
            .filter(|_| self.use_reported_cost && !estimated)
            .and_then(decimal_from_json_number)
            .and_then(Money::from_dollars)
            .filter(|c| c.nanos() >= 0);
        let cost = match reported {
            // A free model the provider charged us for anyway (another model
            // answered, somehow) is charged on: it was never free.
            Some(cost) if self.free && cost.is_positive() => {
                crate::log::warn("chat.free_model_charged", json!({ "model": self.model_id, "pubkey": self.pubkey, "cost": cost.to_f64() }));
                cost
            }
            _ if self.free => Money::ZERO,
            Some(cost) => cost,
            None => self.price.cost(prompt_tokens, completion_tokens),
        };
        Charge { prompt_tokens, completion_tokens, cost, estimated }
    }

    /// Log OpenRouter answering with a model other than the one asked for,
    /// which only its routing options could make it do. (Other providers
    /// may answer with a dated id for an alias.)
    fn check_model(&self, model: Option<&Value>) {
        if self.use_reported_cost
            && let Some(model) = model.and_then(Value::as_str)
            && !model.is_empty()
            && model != self.upstream_model
        {
            crate::log::warn("chat.model_mismatch", json!({ "model": self.model_id, "requested": self.upstream_model, "answered": model, "pubkey": self.pubkey }));
        }
    }

    async fn record(&self, upstream_id: Option<String>, charge: &Charge, stream: bool) {
        let generation = Generation {
            id: store::new_generation_id(),
            upstream_id: upstream_id.clone(),
            pubkey: self.pubkey.clone(),
            model: self.model_id.clone(),
            // Our clock, not the provider's: it orders the history.
            created_at: crate::util::now(),
            prompt_tokens: charge.prompt_tokens,
            completion_tokens: charge.completion_tokens,
            cost: charge.cost,
            estimated: charge.estimated,
            api_key_id: self.api_key_id.clone(),
        };
        self.app.metrics.usage(&self.model_id, charge.prompt_tokens, charge.completion_tokens, charge.cost);
        let fields = json!({
            "pubkey": self.pubkey,
            "api_key": self.api_key_id,
            "model": self.model_id,
            "stream": stream,
            "cost": charge.cost.to_f64(),
            "prompt_tokens": charge.prompt_tokens,
            "completion_tokens": charge.completion_tokens,
            "estimated": charge.estimated,
            "id": upstream_id,
        });
        let result = self.app.db.call(move |conn| store::record_generation(conn, &generation)).await;
        match result {
            Ok(()) => crate::log::info("chat.completion", fields),
            // The completion was delivered; losing its charge is ours to
            // notice and fix by hand.
            Err(error) => crate::log::error("chat.unrecorded", json!({ "completion": fields, "error": format!("{error:#}") })),
        }
    }
}

/// Relay the provider's event stream to the client, rewritten to name our
/// model and carry our cost, and charge once it ends.
///
/// The relaying runs in a task of its own, so that if the client goes away
/// the generation is still charged for: by its reported usage if that has
/// arrived, or else by an estimate of what was sent and received.
fn stream_response(meter: Meter, upstream: reqwest::Response) -> Response {
    let (tx, mut rx) = mpsc::channel::<Bytes>(64);
    tokio::spawn(async move {
        let mut events = upstream.bytes_stream();
        let mut parser = SseParser::default();
        let mut state = StreamState::default();
        let mut client_gone = false;

        'read: while let Some(chunk) = events.next().await {
            let Ok(chunk) = chunk else { break };
            for data in parser.push(&chunk) {
                if data == "[DONE]" {
                    break 'read;
                }
                let Some(out) = state.rewrite(&meter, &data) else { continue };
                if tx.send(out).await.is_err() {
                    client_gone = true;
                    break 'read;
                }
            }
        }
        if !client_gone {
            let _ = tx.send(Bytes::from_static(b"data: [DONE]\n\n")).await;
        }
        drop(tx);

        let charge = state.charge.take().unwrap_or_else(|| meter.charge(&Value::Null, state.completion_bytes));
        meter.record(state.upstream_id, &charge, true).await;
    });

    let body = Body::from_stream(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|b| b.map(Ok::<_, std::convert::Infallible>))));
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap_or_else(|_| ApiError::internal().into_response())
}

#[derive(Default)]
struct StreamState {
    upstream_id: Option<String>,
    /// Content received so far, for an estimate if usage never comes.
    completion_bytes: usize,
    charge: Option<Charge>,
}

impl StreamState {
    /// One upstream event, as the client should get it, or `None` to drop it.
    fn rewrite(&mut self, meter: &Meter, data: &str) -> Option<Bytes> {
        let Ok(mut chunk) = serde_json::from_str::<Value>(data) else { return None };
        // An error mid-stream is passed on as it came.
        if chunk.get("error").is_some() {
            return Some(Bytes::from(format!("data: {chunk}\n\n")));
        }
        if let Some(id) = chunk.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
            if self.upstream_id.is_none() {
                meter.check_model(chunk.get("model"));
            }
            self.upstream_id = Some(id.to_string());
        }
        if let Some(delta) = chunk.pointer("/choices/0/delta").and_then(Value::as_object) {
            for (key, value) in delta {
                if key != "role" {
                    self.completion_bytes += value.as_str().map_or_else(|| value.to_string().len(), str::len);
                }
            }
        }

        let usage = chunk.get("usage").filter(|u| u.is_object());
        let usage_json = match usage {
            Some(usage) => {
                // Usage is reported once, at the end, for the whole completion.
                let charge = meter.charge(usage, self.completion_bytes);
                let json = charge.usage_json();
                self.charge = Some(charge);
                json
            }
            None => json!({ "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }),
        };
        let out = json!({
            "id": chunk.get("id"),
            "model": meter.model_id,
            "choices": chunk.get_mut("choices").map(Value::take).unwrap_or_else(|| json!([])),
            "created": chunk.get("created"),
            "object": chunk.get("object"),
            "usage": usage_json,
        });
        Some(Bytes::from(format!("data: {out}\n\n")))
    }
}

/// Server-sent events, split from a byte stream that may break anywhere.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    /// The data of each event completed by `bytes`.
    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data.push(data.strip_prefix(' ').unwrap_or(data).to_string());
            }
            // Comments (": keep-alive") and other fields mean nothing here.
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_split_events() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"data: {\"a\"").is_empty());
        assert!(parser.push(b":1}\r\n").is_empty());
        assert_eq!(parser.push(b"\r\n: comment\n\ndata: [DONE]\n\n"), vec!["{\"a\":1}", "[DONE]"]);
        assert_eq!(parser.push(b"data: x\ndata: y\n\n"), vec!["x\ny"]);
    }

    #[test]
    fn marks_cache_breakpoints() {
        let mut request = json!({
            "messages": [
                { "role": "system", "content": "sys" },
                { "role": "user", "content": "one" },
                { "role": "assistant", "content": "" },
                { "role": "user", "content": [{ "type": "image_url" }, { "type": "text", "text": "two" }, { "type": "image_url" }] },
            ],
            "tools": [{ "type": "function" }, { "type": "function" }],
        })
        .as_object()
        .unwrap()
        .clone();
        add_cache_breakpoints(&mut request);
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(messages[1]["content"][0].get("cache_control").is_none());
        assert_eq!(messages[2]["content"], "");
        assert_eq!(messages[3]["content"][1]["cache_control"]["type"], "ephemeral");
        assert!(request["tools"][0].get("cache_control").is_none());
        assert_eq!(request["tools"][1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn maps_upstream_errors() {
        let e = upstream_error(StatusCode::BAD_REQUEST, r#"{"error":{"message":"bad tool","type":"invalid_request_error"}}"#);
        assert_eq!((e.status, e.body["message"].as_str()), (StatusCode::BAD_REQUEST, Some("bad tool")));
        assert_eq!(upstream_error(StatusCode::TOO_MANY_REQUESTS, "").status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(upstream_error(StatusCode::UNAUTHORIZED, "").status, StatusCode::BAD_GATEWAY);
        assert_eq!(upstream_error(StatusCode::INTERNAL_SERVER_ERROR, "oops").status, StatusCode::BAD_GATEWAY);
    }
}
