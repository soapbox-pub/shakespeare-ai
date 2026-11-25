//! `/v1/chat/completions`: proxied to the first of the model's upstreams that
//! answers, and charged at the model's price for the usage it reports.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use super::anthropic;
use super::schema::ChatRequest;
use super::upstream::{self, upstream_failure, Failure};
use super::ApiError;
use crate::app::{App, Hold};
use crate::config::{Api, Kind, Provider, TokenPrice, Tokens, Upstream};
use crate::money::{decimal_from_json_number, Money};
use crate::store::{self, Generation};

/// The most output a chat request holds credit for, fewer if it asks for
/// less. More would refuse small balances most requests fit in; what a
/// request costs past its hold is still charged, and [`crate::app::MAX_IN_FLIGHT`]
/// bounds that.
const HELD_OUTPUT_TOKENS: u32 = 4096;

pub async fn completions(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Response, ApiError> {
    let caller = app.authenticate_inference(&method, &uri, &headers, &body).await?;
    let raw = super::json_object(&body)?;
    if !raw.get("model").is_some_and(Value::is_string) {
        return Err(ApiError::invalid("missing_required_parameter", "Missing `model` property").param("model"));
    }
    let request: ChatRequest = serde_json::from_value(Value::Object(raw)).map_err(|error| ApiError::invalid("invalid_parameter", format!("Invalid request: {error}")))?;
    let model_id = request.model.clone();
    let not_found = || ApiError::invalid("model_not_found", format!("Model '{model_id}' is not supported")).param("model");
    let model = app.config.models.get(&model_id).ok_or_else(not_found)?;
    let Kind::Chat { price, context_window } = &model.kind else {
        return Err(ApiError::invalid("invalid_model_type", format!("Model '{model_id}' cannot be used with the chat completions endpoint"))
            .param("model"));
    };
    let free = model.is_free_chat();
    let slot = app.take_slot(&caller.pubkey)?;

    // A request's cost isn't known until it's done, so what it's likely to
    // cost is held first, and the hold is given back when it's charged.
    // Requests in flight together each see the others' holds, so they can't
    // all spend the same credit.
    let hold = if free {
        Hold::none(&app)
    } else {
        app.check_key_limit(&caller, Money::ZERO).await?;
        let held = price.cost(Tokens {
            input: request.prompt_tokens_estimate().min(*context_window),
            output: u64::from(request.max_output().map_or(HELD_OUTPUT_TOKENS, |max| max.min(HELD_OUTPUT_TOKENS))),
            ..Tokens::default()
        });
        match app.hold(&caller.pubkey, held).await? {
            Ok(hold) => hold,
            Err(available) => {
                let message = if available.is_positive() {
                    format!(
                        "This request needs about {} credits, but you only have {} credits available. Please add more credits to continue using the service.",
                        held.to_f64(),
                        available.to_f64()
                    )
                } else {
                    "You have run out of credits. Please add more credits to continue using the service.".to_string()
                };
                return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "insufficient_quota", "insufficient_quota", message)
                    .with("required", held.to_f64())
                    .with("available", available.to_f64()));
            }
        }
    };

    let stream = request.is_stream();
    let session = session_key(&caller.pubkey, &request, &headers);
    let first_byte_timeout = app.config.routing.first_byte_timeout;
    let (attempt, upstream) = upstream::route(&app, &model_id, &model.upstreams, |upstream, provider| {
        let body = upstream_body(&request, upstream, provider, session.as_deref());
        let app = &app;
        async move {
            let label = upstream.label();
            let prompt_bytes = body.len();
            let path = if provider.api == Api::Anthropic { "messages" } else { "chat/completions" };
            let response = upstream::post(app, provider, &label, path, body).await?;
            if !stream {
                return Ok(Attempt { reply: Reply::Whole(response), prompt_bytes });
            }
            // A stream is committed to once its first byte arrives; until
            // then, another upstream can still have the request.
            let mut events = response.bytes_stream().boxed();
            match tokio::time::timeout(first_byte_timeout, events.next()).await {
                Ok(Some(Ok(first))) => {
                    let events = futures_util::stream::once(std::future::ready(Ok(first))).chain(events).boxed();
                    Ok(Attempt { reply: Reply::Stream(events), prompt_bytes })
                }
                Ok(Some(Err(error))) => Err(Failure::Retry(upstream_failure(&format!("{label}: {error}")))),
                Ok(None) => Err(Failure::Retry(upstream_failure(&format!("{label}: the stream ended before it began")))),
                Err(_) => Err(Failure::Retry(upstream_failure(&format!("{label}: nothing streamed in {first_byte_timeout:?}")))),
            }
        }
    })
    .await?;

    let api = app.config.providers.get(&upstream.provider).map_or(Api::Openai, |p| p.api);
    let openrouter = api == Api::Openrouter;
    let mut meter = Meter {
        app: app.clone(),
        api_key_id: caller.api_key_id(),
        pubkey: caller.pubkey,
        model_id,
        upstream: upstream.label(),
        upstream_model: upstream.model.clone(),
        price: price.clone(),
        free,
        openrouter,
        prompt_bytes: attempt.prompt_bytes,
        hold,
        _slot: slot,
    };

    match attempt.reply {
        Reply::Stream(events) => Ok(stream_response(meter, events, (api == Api::Anthropic).then(anthropic::Stream::default))),
        Reply::Whole(response) => {
            let mut completion: Value = response.json().await.map_err(|e| upstream_failure(&e.to_string()))?;
            if api == Api::Anthropic {
                completion = anthropic::completion(&completion);
            }
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
}

/// An upstream that answered, and how.
struct Attempt {
    reply: Reply,
    /// The size of the request it was sent, for an estimate.
    prompt_bytes: usize,
}

enum Reply {
    Whole(reqwest::Response),
    /// Its events, the first already arrived.
    Stream(BoxStream<'static, reqwest::Result<Bytes>>),
}

/// The key that keeps a conversation on one provider's prompt cache, from
/// the client's `session_id` or, failing that, its `x-session-id` header
/// (Shakespeare sends the header). It's hashed with the caller's pubkey, so
/// two people's sessions of the same name never share a cache, and no
/// provider sees what our users call their sessions.
pub fn session_key(pubkey: &str, request: &ChatRequest, headers: &HeaderMap) -> Option<String> {
    let header = headers.get("x-session-id").and_then(|h| h.to_str().ok());
    let session = request.session_id.as_deref().or(header).map(str::trim).filter(|s| !s.is_empty())?;
    Some(hex::encode(Sha256::digest(format!("{pubkey}:{session}"))))
}

/// The request body for one upstream: the client's request in that
/// provider's dialect, with the upstream's own `params` over it.
fn upstream_body(request: &ChatRequest, upstream: &Upstream, provider: &Provider, session: Option<&str>) -> Vec<u8> {
    let openrouter = provider.api == Api::Openrouter;
    let mut body = match provider.api {
        Api::Anthropic => anthropic::request(request, &upstream.model),
        Api::Openai | Api::Openrouter => request.upstream(&upstream.model, openrouter),
    };
    // Each provider's way to route a conversation to its cache. Anthropic
    // routes on its own and has none.
    // https://openrouter.ai/docs/guides/best-practices/prompt-caching#provider-sticky-routing
    // https://developers.openai.com/api/docs/guides/prompt-caching#prompt-cache-keys
    if let Some(session) = session {
        match provider.api {
            Api::Openrouter => _ = body.insert("session_id".into(), session.into()),
            Api::Openai => _ = body.insert("prompt_cache_key".into(), session.into()),
            Api::Anthropic => {}
        }
    }
    if openrouter {
        // https://openrouter.ai/docs/use-cases/usage-accounting
        body.insert("usage".into(), json!({ "include": true }));
        if upstream.model.starts_with("anthropic/") {
            add_cache_breakpoints(&mut body);
        }
    }
    for (key, value) in &upstream.params {
        body.insert(key.clone(), value.clone());
    }
    serde_json::to_vec(&body).unwrap_or_default()
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
    /// The upstream that served it, `provider/model`.
    upstream: String,
    /// The provider's id for it, to notice a provider answering with another.
    upstream_model: String,
    price: TokenPrice,
    free: bool,
    /// OpenRouter reports what each request cost us.
    openrouter: bool,
    prompt_bytes: usize,
    /// The credit set aside for it, given back by its charge.
    hold: Hold,
    /// Held until the generation is charged, however it ends.
    _slot: crate::app::Slot,
}

struct Charge {
    tokens: Tokens,
    /// Of the output, if the provider said.
    reasoning_tokens: Option<u64>,
    cost: Money,
    /// What it cost us, if the provider said.
    upstream_cost: Option<Money>,
    estimated: bool,
}

impl Charge {
    fn usage_json(&self) -> Value {
        let t = self.tokens;
        let mut usage = Map::new();
        usage.insert("prompt_tokens".into(), t.input.into());
        usage.insert("completion_tokens".into(), t.output.into());
        usage.insert("total_tokens".into(), (t.input + t.output).into());
        usage.insert("prompt_tokens_details".into(), json!({ "cached_tokens": t.cache_read, "cache_write_tokens": t.cache_write }));
        if let Some(reasoning) = self.reasoning_tokens {
            usage.insert("completion_tokens_details".into(), json!({ "reasoning_tokens": reasoning }));
        }
        usage.insert("cost".into(), self.cost.to_f64().into());
        Value::Object(usage)
    }
}

/// About four bytes to a token, near enough for a charge that would otherwise
/// be nothing.
fn estimate_tokens(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(4)
}

/// The tokens an OpenAI-style `usage` reports, cached input included as
/// OpenRouter reports it, if it reports any.
fn reported_tokens(usage: &Value) -> Option<Tokens> {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64);
    Some(Tokens {
        input: count("/prompt_tokens")?,
        output: count("/completion_tokens")?,
        cache_read: count("/prompt_tokens_details/cached_tokens").unwrap_or(0),
        cache_write: count("/prompt_tokens_details/cache_write_tokens").unwrap_or(0),
    })
}

impl Meter {
    /// The charge for a reported `usage`, or an estimate from the bytes sent
    /// and received if there's none. It's the model's price, whichever
    /// upstream served it.
    fn charge(&self, usage: &Value, completion_bytes: usize) -> Charge {
        let (tokens, estimated) = match reported_tokens(usage) {
            Some(tokens) => (tokens, false),
            None => (Tokens { input: estimate_tokens(self.prompt_bytes), output: estimate_tokens(completion_bytes), ..Tokens::default() }, true),
        };
        let upstream_cost = usage
            .get("cost")
            .filter(|_| self.openrouter && !estimated)
            .and_then(decimal_from_json_number)
            .and_then(Money::from_dollars)
            .filter(|c| c.nanos() >= 0);
        let cost = match upstream_cost {
            // A free model the provider charged us for anyway (another model
            // answered, somehow) is charged on: it was never free.
            Some(cost) if self.free && cost.is_positive() => {
                crate::log::warn("chat.free_model_charged", json!({ "model": self.model_id, "upstream": self.upstream, "pubkey": self.pubkey, "cost": cost.to_f64() }));
                cost
            }
            _ if self.free => Money::ZERO,
            _ => self.price.cost(tokens),
        };
        let reasoning_tokens = usage.pointer("/completion_tokens_details/reasoning_tokens").and_then(Value::as_u64).filter(|_| !estimated);
        Charge { tokens, reasoning_tokens, cost, upstream_cost, estimated }
    }

    /// Log OpenRouter answering with a model other than the one asked for,
    /// which only its routing options could make it do. (Other providers
    /// may answer with a dated id for an alias.)
    fn check_model(&self, model: Option<&Value>) {
        if self.openrouter
            && let Some(model) = model.and_then(Value::as_str)
            && !model.is_empty()
            && model != self.upstream_model
        {
            crate::log::warn("chat.model_mismatch", json!({ "model": self.model_id, "requested": self.upstream_model, "answered": model, "pubkey": self.pubkey }));
        }
    }

    async fn record(&mut self, upstream_id: Option<String>, charge: &Charge, stream: bool) {
        let t = charge.tokens;
        let generation = Generation {
            id: store::new_generation_id(),
            upstream_id: upstream_id.clone(),
            pubkey: self.pubkey.clone(),
            model: self.model_id.clone(),
            // Our clock, not the provider's: it orders the history.
            created_at: crate::util::now(),
            prompt_tokens: t.input,
            completion_tokens: t.output,
            cache_read_tokens: t.cache_read,
            cache_write_tokens: t.cache_write,
            cost: charge.cost,
            estimated: charge.estimated,
            api_key_id: self.api_key_id.clone(),
            upstream: Some(self.upstream.clone()),
            upstream_cost: charge.upstream_cost,
        };
        self.app.metrics.usage(&self.model_id, t.input, t.output, charge.cost);
        let fields = json!({
            "pubkey": self.pubkey,
            "api_key": self.api_key_id,
            "model": self.model_id,
            "upstream": self.upstream,
            "stream": stream,
            "cost": charge.cost.to_f64(),
            "upstream_cost": charge.upstream_cost.map(Money::to_f64),
            "prompt_tokens": t.input,
            "completion_tokens": t.output,
            "cache_read_tokens": t.cache_read,
            "cache_write_tokens": t.cache_write,
            "estimated": charge.estimated,
            "id": upstream_id,
        });
        let hold = self.hold.id().map(str::to_string);
        let result = self.app.db.call(move |conn| store::record_generation(conn, &generation, hold.as_deref())).await;
        match result {
            Ok(()) => {
                self.hold.settled();
                crate::log::info("chat.completion", fields);
            }
            // The completion was delivered; losing its charge is ours to
            // notice and fix by hand.
            Err(error) => crate::log::error("chat.unrecorded", json!({ "completion": fields, "error": format!("{error:#}") })),
        }
    }
}

/// Relay the provider's event stream to the client, rewritten to name our
/// model and carry our cost, and charge once it ends.
///
/// The relaying runs in a task of its own, and reads the provider's stream to
/// its end whatever the client does, so the generation is charged by the
/// usage the provider reports (what it charges us): a client can't escape
/// that by hanging up before the usage arrives, or by not reading. Only if
/// the provider never reports usage is it charged an estimate of what was
/// sent and received.
///
/// The channel to the client is unbounded so that a client that stops
/// reading can't hold up the reading of the provider's stream, and with it
/// the charge. What it buffers is one completion, at most the provider's
/// output limit.
fn stream_response(mut meter: Meter, mut events: BoxStream<'static, reqwest::Result<Bytes>>, mut anthropic: Option<anthropic::Stream>) -> Response {
    let (tx, mut rx) = mpsc::unbounded_channel::<Bytes>();
    tokio::spawn(async move {
        let mut parser = SseParser::default();
        let mut state = StreamState::default();
        let mut client_gone = false;

        'read: while let Some(chunk) = events.next().await {
            let Ok(chunk) = chunk else { break };
            let received = parser.push(&chunk);
            // Anthropic's events, rewritten as OpenAI's chunks first.
            let received = match &mut anthropic {
                Some(translator) => received.iter().flat_map(|data| translator.push(data)).collect(),
                None => received,
            };
            for data in received {
                if data == "[DONE]" {
                    break 'read;
                }
                let Some(out) = state.rewrite(&meter, &data) else { continue };
                // Gone, it's still read for its usage; just not sent.
                if !client_gone && tx.send(out).is_err() {
                    client_gone = true;
                }
            }
        }
        if !client_gone {
            let _ = tx.send(Bytes::from_static(b"data: [DONE]\n\n"));
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
    fn keys_sessions_by_caller() {
        let request = |body: Value| serde_json::from_value::<ChatRequest>(body).unwrap();
        let plain = request(json!({ "model": "m", "messages": [] }));
        let named = request(json!({ "model": "m", "messages": [], "session_id": "s" }));
        let mut headers = HeaderMap::new();
        assert_eq!(session_key("a", &plain, &headers), None);
        headers.insert("x-session-id", " ".parse().unwrap());
        assert_eq!(session_key("a", &plain, &headers), None);
        headers.insert("x-session-id", "s".parse().unwrap());
        let key = session_key("a", &plain, &headers).unwrap();
        assert_eq!(key, hex::encode(Sha256::digest("a:s")));
        assert_ne!(session_key("b", &plain, &headers), Some(key.clone()));
        // The body's wins, as it does at OpenRouter.
        headers.insert("x-session-id", "other".parse().unwrap());
        assert_eq!(session_key("a", &named, &headers), Some(key));
    }

    #[test]
    fn sends_each_provider_its_session_field() {
        let request: ChatRequest = serde_json::from_value(json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }], "session_id": "raw" })).unwrap();
        let body = |api: &str, params: Value| {
            let provider: Provider = serde_json::from_value(json!({ "api": api, "api_key": "k" })).unwrap();
            let upstream = Upstream { provider: "p".into(), model: "x".into(), params: params.as_object().unwrap().clone() };
            serde_json::from_slice::<Value>(&upstream_body(&request, &upstream, &provider, Some("key"))).unwrap()
        };
        let openrouter = body("openrouter", json!({}));
        assert_eq!((&openrouter["session_id"], openrouter.get("prompt_cache_key")), (&json!("key"), None));
        let openai = body("openai", json!({}));
        assert_eq!((openai.get("session_id"), &openai["prompt_cache_key"]), (None, &json!("key")));
        let anthropic = body("anthropic", json!({}));
        assert!(anthropic.get("session_id").is_none() && anthropic.get("prompt_cache_key").is_none());
        // An upstream's params still have the last word.
        assert_eq!(body("openai", json!({ "prompt_cache_key": null }))["prompt_cache_key"], Value::Null);
    }
}
