//! End-to-end: the real router, signed requests, and a fake upstream
//! provider.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::app::{router, App};
use crate::config::Config;
use crate::db::testing::{temp_db, TempDb};
use crate::money::Money;
use crate::nostr::{public_key, sign_nip98};
use crate::store;

const SECRET: &str = "0000000000000000000000000000000000000000000000000000000000000007";

/// A provider that streams when asked, slowly enough to hang up on.
async fn fake_upstream() -> String {
    async fn chat(headers: axum::http::HeaderMap, body: Bytes) -> axum::response::Response {
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "upstream-model");
        // Only what our schema names ever gets here.
        for field in ["models", "provider", "plugins", "route", "n", "user"] {
            assert!(request.get(field).is_none(), "{field} reached the provider");
        }
        assert!(headers.get("accept-language").is_none() && headers.get("x-session-id").is_none());
        // As OpenRouter does, when asked: what it really cost.
        let cost = if request["usage"]["include"] == true { json!(0.5) } else { Value::Null };
        if request["stream"] == true {
            assert_eq!(request["stream_options"]["include_usage"], true);
            let chunks = vec![
                json!({"id": "up-1", "object": "chat.completion.chunk", "created": 1700000000, "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}),
                json!({"id": "up-1", "object": "chat.completion.chunk", "created": 1700000000, "choices": [{"index": 0, "delta": {"content": "lo"}, "finish_reason": "stop"}]}),
                json!({"id": "up-1", "object": "chat.completion.chunk", "created": 1700000000, "choices": [], "usage": {"prompt_tokens": 1000, "completion_tokens": 2000, "total_tokens": 3000}}),
            ];
            let stream = futures_util::stream::iter(chunks.into_iter().map(|c| format!("data: {c}\n\n")).chain([": keepalive\n\n".to_string(), "data: [DONE]\n\n".to_string()]))
                .then(|line| async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok::<_, std::convert::Infallible>(Bytes::from(line))
                });
            return ([("content-type", "text/event-stream")], Body::from_stream(stream)).into_response();
        }
        axum::Json(json!({
            "id": "up-2", "object": "chat.completion", "created": 1700000000,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 1000, "completion_tokens": 1000, "total_tokens": 2000, "cost": cost,
                "prompt_tokens_details": if cost.is_null() { Value::Null } else { json!({ "cached_tokens": 600, "cache_write_tokens": 100 }) },
                "completion_tokens_details": { "reasoning_tokens": 400 },
            },
            "system_fingerprint": "dropped",
        }))
        .into_response()
    }
    async fn images(body: Bytes) -> axum::Json<Value> {
        let request: Value = serde_json::from_slice(&body).unwrap();
        if request["model"] == "gpt-image-tokens" {
            // As GPT Image 2 and later do: a quality of its own choosing, and
            // what it used.
            return axum::Json(json!({
                "created": 1, "quality": "xhigh", "size": "2048x2048", "data": [{"b64_json": "x"}],
                "usage": { "input_tokens": 1000, "input_tokens_details": { "text_tokens": 1000, "image_tokens": 0 }, "output_tokens": 10_000 },
            }));
        }
        assert_eq!(request["model"], "gpt-image-1");
        axum::Json(json!({ "created": 1, "quality": "medium", "size": "1024x1024", "data": [{"b64_json": "x"}, {"b64_json": "y"}] }))
    }
    async fn down() -> impl IntoResponse {
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({ "error": { "message": "overloaded" } })))
    }
    async fn refuses() -> impl IntoResponse {
        (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": { "message": "bad tool", "type": "invalid_request_error" } })))
    }
    async fn silent() -> impl IntoResponse {
        let stream = futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok::<_, std::convert::Infallible>(Bytes::from_static(b"data: [DONE]\n\n"))
        });
        ([("content-type", "text/event-stream")], Body::from_stream(stream))
    }
    async fn messages(headers: axum::http::HeaderMap, body: Bytes) -> axum::response::Response {
        assert_eq!(headers["x-api-key"], "test");
        assert_eq!(headers["anthropic-version"], crate::api::anthropic::VERSION);
        assert!(headers.get("authorization").is_none());
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "claude-test");
        assert_eq!(request["system"], json!([{ "type": "text", "text": "be brief", "cache_control": { "type": "ephemeral" } }]));
        assert_eq!(request["messages"], json!([{ "role": "user", "content": [{ "type": "text", "text": "hi", "cache_control": { "type": "ephemeral" } }] }]));
        assert_eq!(request["max_tokens"], 16384);
        let usage = json!({ "input_tokens": 100, "cache_read_input_tokens": 600, "cache_creation_input_tokens": 300, "output_tokens": 1000 });
        if request["stream"] == true {
            let events = [
                json!({ "type": "message_start", "message": { "id": "msg_1", "model": "claude-test", "usage": { "input_tokens": 100, "cache_read_input_tokens": 600, "cache_creation_input_tokens": 300, "output_tokens": 1 } } }),
                json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hel" } }),
                json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "lo" } }),
                json!({ "type": "content_block_stop", "index": 0 }),
                json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 1000 } }),
                json!({ "type": "message_stop" }),
            ];
            let text: String = events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect();
            return ([("content-type", "text/event-stream")], text).into_response();
        }
        axum::Json(json!({
            "id": "msg_2", "type": "message", "role": "assistant", "model": "claude-test", "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": "Hi" }],
            "usage": usage,
        }))
        .into_response()
    }
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/anthropic/v1/messages", post(messages))
        .route("/v1/images/generations", post(images))
        .route("/down/v1/chat/completions", post(down))
        .route("/down/v1/images/generations", post(down))
        .route("/refuses/v1/chat/completions", post(refuses))
        .route("/silent/v1/chat/completions", post(silent));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/v1")
}

async fn setup() -> (Arc<App>, TempDb) {
    let upstream = fake_upstream().await;
    let root = upstream.trim_end_matches("/v1");
    let yaml = format!(
        r#"
origin: https://ai.example.com
database: unused.sqlite3
providers:
  fake:
    api: openai
    base_url: {upstream}
    api_key: test
  router:
    api: openrouter
    base_url: {upstream}
    api_key: test
  down:
    api: openai
    base_url: {root}/down/v1
    api_key: test
  refuses:
    api: openai
    base_url: {root}/refuses/v1
    api_key: test
  silent:
    api: openai
    base_url: {root}/silent/v1
    api_key: test
  claude:
    api: anthropic
    base_url: {root}/anthropic/v1
    api_key: test
routing:
  first_byte_timeout: 300ms
  failures: 2
  cooldown: 1h
models:
  fallback:
    type: chat
    hidden: true
    upstreams: [down/upstream-model, fake/upstream-model]
    context_window: 1000
    price: {{ input: 3, output: 15 }}
  slow-fallback:
    type: chat
    hidden: true
    upstreams: [silent/upstream-model, fake/upstream-model]
    context_window: 1000
    price: {{ input: 3, output: 15 }}
  refused:
    type: chat
    hidden: true
    upstreams: [refuses/upstream-model, fake/upstream-model]
    context_window: 1000
    price: {{ input: 3, output: 15 }}
  claude:
    type: chat
    hidden: true
    upstreams: [claude/claude-test]
    context_window: 1000
    price: {{ input: 3, output: 15, cache_read: 0.3, cache_write: 3.75 }}
  cached:
    type: chat
    hidden: true
    upstreams: [router/upstream-model]
    context_window: 1000
    price: {{ input: 3, output: 15, cache_read: 0.3, cache_write: 3.75 }}
  paid:
    type: chat
    upstreams: [fake/upstream-model]
    context_window: 1000
    price: {{ input: 3, output: 15 }}
  free:
    type: chat
    upstreams: [fake/upstream-model]
    context_window: 1000
    price: {{ input: 0, output: 0 }}
  routed-free:
    type: chat
    upstreams: [router/upstream-model]
    context_window: 1000
    price: {{ input: 0, output: 0 }}
  gpt-image-1:
    type: image
    upstreams: [down/gpt-image-1, fake/gpt-image-1]
    price:
      low: {{ square: 0.011, wide: 0.016 }}
      medium: {{ square: 0.042, wide: 0.063 }}
      high: {{ square: 0.167, wide: 0.25 }}
  gpt-image-tokens:
    type: image
    hidden: true
    upstreams: [fake/gpt-image-tokens]
    price:
      low: {{ square: 0.025, wide: 0.02 }}
      medium: {{ square: 0.044, wide: 0.035 }}
      high: {{ square: 0.053, wide: 0.042 }}
      tokens: {{ text_input: 5, image_input: 8, output: 30 }}
"#
    );
    let t = temp_db();
    let app = Arc::new(App::new(Config::parse(&yaml).unwrap(), t.db.clone()).unwrap());
    (app, t)
}

fn signed(method: &str, path: &str, body: Option<Value>) -> Request<Body> {
    let bytes = body.map(|b| serde_json::to_vec(&b).unwrap());
    let auth = sign_nip98(SECRET, method, &format!("https://ai.example.com{path}"), bytes.as_deref(), crate::util::now());
    Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", auth)
        .header("content-type", "application/json")
        .body(bytes.map_or_else(Body::empty, Body::from))
        .unwrap()
}

async fn json_of(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn give(app: &App, dollars: i64) {
    let pubkey = public_key(SECRET);
    app.db
        .with(|conn| {
            let tx = store::write_tx(conn)?;
            store::post(&tx, &pubkey, Money(dollars * 1_000_000_000), store::Kind::Opening, &pubkey, 0)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

fn balance(app: &App) -> Money {
    app.db.with(|conn| store::balance(conn, &public_key(SECRET))).unwrap()
}

async fn wait_for_generations(app: &App, n: i64) -> Vec<(i64, i64)> {
    for _ in 0..100 {
        let rows: Vec<(i64, i64)> = app
            .db
            .with(|conn| {
                let mut stmt = conn.prepare("SELECT cost, estimated FROM generations ORDER BY rowid")?;
                Ok(stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
            })
            .unwrap();
        if rows.len() as i64 >= n {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no generation recorded");
}

#[tokio::test]
async fn refuses_without_credit_and_charges_for_completions() {
    let (app, _t) = setup().await;
    let messages = json!([{ "role": "user", "content": "hi" }]);

    let (status, body) = json_of(router(app.clone()).oneshot(signed("POST", "/v1/chat/completions", Some(json!({ "model": "paid", "messages": messages })))).await.unwrap()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "insufficient_quota");

    // Free models need no credit, and cost nothing.
    let (status, body) = json_of(router(app.clone()).oneshot(signed("POST", "/v1/chat/completions", Some(json!({ "model": "free", "messages": messages })))).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["usage"]["cost"], 0.0);

    give(&app, 1);
    let (status, body) = json_of(router(app.clone()).oneshot(signed("POST", "/v1/chat/completions", Some(json!({ "model": "paid", "messages": messages })))).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"], "paid");
    assert!(body.get("system_fingerprint").is_none());
    // 1000 × $3/M + 1000 × $15/M.
    assert_eq!(body["usage"]["cost"], 0.018);
    assert_eq!(balance(&app), Money(1_000_000_000 - 18_000_000));

    let (_, list) = json_of(router(app.clone()).oneshot(signed("GET", "/v1/credits/generations", None)).await.unwrap()).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 2);
    let costs: Vec<f64> = list["data"].as_array().unwrap().iter().map(|g| g["cost"].as_f64().unwrap()).collect();
    assert!(costs.contains(&0.018) && costs.contains(&0.0), "{costs:?}");
}

#[tokio::test]
async fn sends_providers_only_what_the_schema_allows() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let mut request = signed(
        "POST",
        "/v1/chat/completions",
        Some(json!({
            "model": "paid",
            "messages": [{ "role": "user", "content": "hi" }],
            "models": ["anthropic/claude-opus-4"],
            "provider": { "order": ["x"] },
            "plugins": [{ "id": "web" }],
            "route": "fallback",
            "n": 8,
            "user": "u",
        })),
    );
    request.headers_mut().insert("accept-language", "fr".parse().unwrap());
    request.headers_mut().insert("x-session-id", "s".parse().unwrap());
    let (status, body) = json_of(router(app.clone()).oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A field in the schema with the wrong shape is refused.
    let bad = signed("POST", "/v1/chat/completions", Some(json!({ "model": "paid", "messages": [{ "role": "function", "content": "x" }] })));
    let (status, body) = json_of(router(app.clone()).oneshot(bad).await.unwrap()).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("invalid_parameter")));
}

#[tokio::test]
async fn charges_a_free_model_whatever_the_provider_charged() {
    let (app, _t) = setup().await;
    let request = signed("POST", "/v1/chat/completions", Some(json!({ "model": "routed-free", "messages": [{ "role": "user", "content": "hi" }] })));
    let (status, body) = json_of(router(app.clone()).oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["usage"]["cost"], 0.5);
    assert_eq!(balance(&app), Money(-500_000_000));
}

fn upstreams_used(app: &App) -> Vec<Option<String>> {
    app.db
        .with(|conn| {
            let mut stmt = conn.prepare("SELECT upstream FROM generations ORDER BY rowid")?;
            Ok(stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
        })
        .unwrap()
}

fn chat(model: &str, stream: bool) -> Request<Body> {
    signed("POST", "/v1/chat/completions", Some(json!({ "model": model, "stream": stream, "messages": [{ "role": "user", "content": "hi" }] })))
}

#[tokio::test]
async fn falls_over_to_the_next_upstream() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let (status, body) = json_of(router(app.clone()).oneshot(chat("fallback", false)).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["usage"]["cost"], 0.018);
    assert_eq!(upstreams_used(&app), [Some("fake/upstream-model".to_string())]);
    assert!(app.metrics.render().contains("shakespeare_upstream_failures_total{upstream=\"down/upstream-model\"} 1"));

    // Failing twice in a row puts it out of turn, so the next request goes
    // straight to the one that works.
    let order = |app: &App| app.health.order(&app.config.models["fallback"].upstreams).iter().map(|u| u.label()).collect::<Vec<_>>();
    assert_eq!(order(&app), ["down/upstream-model", "fake/upstream-model"]);
    router(app.clone()).oneshot(chat("fallback", false)).await.unwrap();
    assert_eq!(order(&app), ["fake/upstream-model", "down/upstream-model"]);
    router(app.clone()).oneshot(chat("fallback", false)).await.unwrap();
    assert!(app.metrics.render().contains("shakespeare_upstream_failures_total{upstream=\"down/upstream-model\"} 2"));
}

#[tokio::test]
async fn falls_over_from_a_stream_that_never_starts() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let response = router(app.clone()).oneshot(chat("slow-fallback", true)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(text.contains("\"Hel\"") && text.ends_with("data: [DONE]\n\n"), "{text}");
    wait_for_generations(&app, 1).await;
    assert_eq!(upstreams_used(&app), [Some("fake/upstream-model".to_string())]);
}

#[tokio::test]
async fn does_not_fall_over_when_the_request_is_refused() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let (status, body) = json_of(router(app.clone()).oneshot(chat("refused", false)).await.unwrap()).await;
    assert_eq!((status, &body["error"]["message"]), (StatusCode::BAD_REQUEST, &json!("bad tool")));
    assert!(upstreams_used(&app).is_empty());
}

#[tokio::test]
async fn prices_cached_input_and_records_what_it_cost_us() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let (status, body) = json_of(router(app.clone()).oneshot(chat("cached", false)).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 300 uncached × $3/M + 600 read × $0.30/M + 100 written × $3.75/M +
    // 1000 out × $15/M, whatever OpenRouter charged us.
    assert_eq!(body["usage"]["cost"], 0.016455);
    assert_eq!(body["usage"]["prompt_tokens_details"], json!({ "cached_tokens": 600, "cache_write_tokens": 100 }));
    assert_eq!(body["usage"]["completion_tokens_details"], json!({ "reasoning_tokens": 400 }));
    let row: (i64, i64, Option<i64>, String) = app
        .db
        .with(|conn| Ok(conn.query_row("SELECT cache_read_tokens, cache_write_tokens, upstream_cost, upstream FROM generations", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?))
        .unwrap();
    assert_eq!(row, (600, 100, Some(500_000_000), "router/upstream-model".into()));
}

#[tokio::test]
async fn speaks_anthropic_to_anthropic() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let messages = json!([{ "role": "system", "content": "be brief" }, { "role": "user", "content": "hi" }]);
    // 100 uncached × $3/M + 600 read × $0.30/M + 300 written × $3.75/M +
    // 1000 out × $15/M.
    let cost = 0.016605;

    let request = signed("POST", "/v1/chat/completions", Some(json!({ "model": "claude", "messages": messages })));
    let (status, body) = json_of(router(app.clone()).oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((&body["model"], &body["object"]), (&json!("claude"), &json!("chat.completion")));
    assert_eq!(body["choices"][0]["message"], json!({ "role": "assistant", "content": "Hi" }));
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["prompt_tokens"], 1000);
    assert_eq!(body["usage"]["prompt_tokens_details"], json!({ "cached_tokens": 600, "cache_write_tokens": 300 }));
    assert_eq!(body["usage"]["cost"], cost);

    let request = signed("POST", "/v1/chat/completions", Some(json!({ "model": "claude", "stream": true, "messages": messages })));
    let response = router(app.clone()).oneshot(request).await.unwrap();
    let text = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    let chunks: Vec<Value> = text.split("\n\n").filter_map(|e| e.strip_prefix("data: ")).filter(|d| *d != "[DONE]").map(|d| serde_json::from_str(d).unwrap()).collect();
    let content: String = chunks.iter().filter_map(|c| c.pointer("/choices/0/delta/content").and_then(Value::as_str)).collect();
    assert_eq!(content, "Hello");
    assert!(chunks.iter().all(|c| c["model"] == "claude"));
    assert_eq!(chunks.last().unwrap()["usage"]["cost"], cost, "{text}");
    assert!(text.ends_with("data: [DONE]\n\n"));

    let rows = wait_for_generations(&app, 2).await;
    assert_eq!(rows, [(16_605_000, 0), (16_605_000, 0)]);
    assert_eq!(balance(&app), Money(1_000_000_000 - 2 * 16_605_000));
}

#[tokio::test]
async fn streams_and_charges_by_reported_usage() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let request = signed("POST", "/v1/chat/completions", Some(json!({ "model": "paid", "stream": true, "messages": [{ "role": "user", "content": "hi" }] })));
    let response = router(app.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let text = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    let events: Vec<&str> = text.split("\n\n").filter(|e| !e.is_empty()).collect();
    assert_eq!(events.len(), 4, "{text}");
    assert_eq!(events[3], "data: [DONE]");
    let first: Value = serde_json::from_str(events[0].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(first["model"], "paid");
    assert_eq!(first["choices"][0]["delta"]["content"], "Hel");
    let last: Value = serde_json::from_str(events[2].strip_prefix("data: ").unwrap()).unwrap();
    // 1000 × $3/M + 2000 × $15/M.
    assert_eq!(last["usage"]["cost"], 0.033);

    assert_eq!(wait_for_generations(&app, 1).await, vec![(33_000_000, 0)]);
    assert_eq!(balance(&app), Money(1_000_000_000 - 33_000_000));
}

#[tokio::test]
async fn limits_the_requests_an_account_has_in_flight() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let stream = || signed("POST", "/v1/chat/completions", Some(json!({ "model": "paid", "stream": true, "messages": [{ "role": "user", "content": "hi" }] })));
    let mut running = Vec::new();
    for _ in 0..crate::app::MAX_IN_FLIGHT {
        let response = router(app.clone()).oneshot(stream()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        running.push(response);
    }
    let (status, body) = json_of(router(app.clone()).oneshot(stream()).await.unwrap()).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::TOO_MANY_REQUESTS, &json!("too_many_concurrent_requests")));
    // Images count too.
    let image = signed("POST", "/v1/images/generations", Some(json!({ "model": "gpt-image-1", "prompt": "a cat", "quality": "low" })));
    assert_eq!(router(app.clone()).oneshot(image).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);

    // Each place comes back once its generation is charged, read or not.
    drop(running);
    wait_for_generations(&app, crate::app::MAX_IN_FLIGHT as i64).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(router(app.clone()).oneshot(stream()).await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn charges_reported_usage_when_the_client_hangs_up() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let request = signed("POST", "/v1/chat/completions", Some(json!({ "model": "paid", "stream": true, "messages": [{ "role": "user", "content": "hi" }] })));
    let response = router(app.clone()).oneshot(request).await.unwrap();
    let mut body = response.into_body().into_data_stream();
    body.next().await.unwrap().unwrap();
    // Gone before the usage arrives: the provider's stream is still read to
    // its end, and charged by what it reports, not an estimate.
    drop(body);

    assert_eq!(wait_for_generations(&app, 1).await, vec![(33_000_000, 0)]);
    assert_eq!(balance(&app), Money(1_000_000_000 - 33_000_000));
    assert_eq!(holds(&app), 0);
}

fn holds(app: &App) -> i64 {
    app.db.with(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM holds", [], |r| r.get(0))?)).unwrap()
}

fn give_nanos(app: &App, nanos: i64) {
    let pubkey = public_key(SECRET);
    app.db
        .with(|conn| {
            let tx = store::write_tx(conn)?;
            store::post(&tx, &pubkey, Money(nanos), store::Kind::Opening, &pubkey, 0)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn holds_credit_for_requests_in_flight() {
    let (app, _t) = setup().await;
    // $0.10: enough to hold for one request of 4096 tokens out at $15/M
    // ($0.061), not two.
    give_nanos(&app, 100_000_000);
    let stream = |extra: Value| {
        let mut body = json!({ "model": "paid", "stream": true, "messages": [{ "role": "user", "content": "hi" }] });
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        signed("POST", "/v1/chat/completions", Some(body))
    };
    let first = router(app.clone()).oneshot(stream(json!({}))).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(holds(&app), 1);

    // The balance is still $0.10, but most of it is held.
    let (status, body) = json_of(router(app.clone()).oneshot(stream(json!({}))).await.unwrap()).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::TOO_MANY_REQUESTS, &json!("insufficient_quota")), "{body}");
    assert!(body["error"]["available"].as_f64().unwrap() < 0.04, "{body}");
    let image = signed("POST", "/v1/images/generations", Some(json!({ "model": "gpt-image-1", "prompt": "a cat" })));
    assert_eq!(router(app.clone()).oneshot(image).await.unwrap().status(), StatusCode::TOO_MANY_REQUESTS);
    // Asking for less output holds less.
    let small = router(app.clone()).oneshot(stream(json!({ "max_tokens": 100 }))).await.unwrap();
    assert_eq!(small.status(), StatusCode::OK);

    // Each charge gives its hold back.
    drop((first, small));
    wait_for_generations(&app, 2).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(holds(&app), 0);
    assert_eq!(balance(&app), Money(100_000_000 - 2 * 33_000_000));
}

#[tokio::test]
async fn gives_a_hold_back_when_the_request_fails() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let chat = signed("POST", "/v1/chat/completions", Some(json!({ "model": "refused", "messages": [{ "role": "user", "content": "hi" }] })));
    let (status, _) = json_of(router(app.clone()).oneshot(chat).await.unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for _ in 0..50 {
        if holds(&app) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(holds(&app), 0);
    assert_eq!(balance(&app), Money(1_000_000_000));
}

#[tokio::test]
async fn generates_images_priced_by_what_was_made() {
    let (app, _t) = setup().await;
    let request = || signed("POST", "/v1/images/generations", Some(json!({ "model": "gpt-image-1", "prompt": "a cat", "n": 2 })));

    // Without a quality, the most it could cost: 2 × high wide.
    give(&app, 0);
    let (status, body) = json_of(router(app.clone()).oneshot(request()).await.unwrap()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["required"], 0.5);

    give_more(&app);
    let (status, body) = json_of(router(app.clone()).oneshot(request()).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The provider made two medium squares.
    assert_eq!(body["usage"]["cost"], 0.084);
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
    assert_eq!(balance(&app), Money(1_000_000_000 - 84_000_000));
}

#[tokio::test]
async fn charges_token_priced_images_by_what_they_used() {
    let (app, _t) = setup().await;
    give_more(&app);
    let request = signed("POST", "/v1/images/generations", Some(json!({ "model": "gpt-image-tokens", "prompt": "a cat" })));
    let (status, body) = json_of(router(app.clone()).oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 1000 text tokens at $5/M and 10,000 out at $30/M: more than the table's
    // dearest image, for a quality it hasn't got.
    assert_eq!(body["usage"]["cost"], 0.305);
    assert_eq!(balance(&app), Money(1_000_000_000 - 305_000_000));
    let tokens = app
        .db
        .with(|conn| Ok(conn.query_row("SELECT prompt_tokens, completion_tokens FROM generations", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?))
        .unwrap();
    assert_eq!(tokens, (1000, 10_000));
}

fn give_more(app: &App) {
    let pubkey = public_key(SECRET);
    app.db
        .with(|conn| {
            let tx = store::write_tx(conn)?;
            store::post(&tx, &pubkey, Money(1_000_000_000), store::Kind::GiftcardRedeem, "test", 0)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn sells_and_redeems_gift_cards() {
    let (app, _t) = setup().await;
    give(&app, 12);
    let (status, body) =
        json_of(router(app.clone()).oneshot(signed("POST", "/v1/credits/giftcards", Some(json!({ "amount": 5, "quantity": 3 })))).await.unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "insufficient_credits");
    assert_eq!(body["error"]["required"], 15.0);

    let (status, body) = json_of(router(app.clone()).oneshot(signed("POST", "/v1/credits/giftcards", Some(json!({ "amount": 5 })))).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let code = body["data"][0]["code"].as_str().unwrap().to_string();
    assert_eq!(body["data"][0]["object"], "giftcard");

    let check = Request::builder().uri(format!("/v1/credits/giftcards/check/{}", code.to_lowercase())).body(Body::empty()).unwrap();
    let (status, body) = json_of(router(app.clone()).oneshot(check).await.unwrap()).await;
    assert_eq!((status, body["redeemed"].as_bool()), (StatusCode::OK, Some(false)));

    let redeem = || signed("POST", "/v1/credits/giftcards/redeem", Some(json!({ "code": code })));
    let (status, body) = json_of(router(app.clone()).oneshot(redeem()).await.unwrap()).await;
    assert_eq!((status, &body["amount"]), (StatusCode::OK, &json!(5.0)));
    let (status, body) = json_of(router(app.clone()).oneshot(redeem()).await.unwrap()).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("giftcard_already_redeemed")));

    let (_, body) = json_of(router(app.clone()).oneshot(signed("GET", "/v1/credits", None)).await.unwrap()).await;
    assert_eq!(body, json!({ "object": "credits", "amount": 12.0 }));

    // A cancelled card is refunded, and listed as cancelled.
    let (_, body) = json_of(router(app.clone()).oneshot(signed("POST", "/v1/credits/giftcards", Some(json!({ "amount": 5 })))).await.unwrap()).await;
    let id = body["data"][0]["id"].as_str().unwrap().to_string();
    let response = router(app.clone()).oneshot(signed("DELETE", &format!("/v1/credits/giftcards/{id}"), None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let (_, body) = json_of(router(app.clone()).oneshot(signed("GET", "/v1/credits/giftcards", None)).await.unwrap()).await;
    let mut statuses: Vec<&str> = body["data"].as_array().unwrap().iter().map(|c| c["status"].as_str().unwrap()).collect();
    statuses.sort();
    assert_eq!(statuses, ["cancelled", "redeemed"]);
    assert_eq!(balance(&app), Money(12_000_000_000));
}

#[tokio::test]
async fn validates_requests() {
    let (app, _t) = setup().await;
    let unsigned = Request::builder().uri("/v1/credits").body(Body::empty()).unwrap();
    assert_eq!(router(app.clone()).oneshot(unsigned).await.unwrap().status(), StatusCode::UNAUTHORIZED);

    let (status, body) = json_of(router(app.clone()).oneshot(signed("POST", "/v1/credits/add", Some(json!({ "amount": 1, "method": "stripe" })))).await.unwrap()).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("minimum_amount_not_met")));
    assert_eq!(body["error"]["minimum_amount"], 5.0);
    // Not configured here, so not supported.
    let (_, body) = json_of(router(app.clone()).oneshot(signed("POST", "/v1/credits/add", Some(json!({ "amount": 10, "method": "lightning" })))).await.unwrap()).await;
    assert_eq!(body["error"]["code"], "unsupported_method");

    let unsigned = Request::builder().uri("/v1/credits/methods").body(Body::empty()).unwrap();
    let (status, body) = json_of(router(app.clone()).oneshot(unsigned).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "object": "list", "data": [], "has_more": false, "minimum_amount": 5.0, "fee": 0.1 }));

    let (status, body) = json_of(router(app.clone()).oneshot(signed("GET", "/v1/credits/payments?limit=101", None)).await.unwrap()).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("invalid_limit")));

    let (_, body) = json_of(router(app.clone()).oneshot(Request::builder().uri("/v1/models").body(Body::empty()).unwrap()).await.unwrap()).await;
    let ids: Vec<&str> = body["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["paid", "gpt-image-1"]);
    assert_eq!(body["data"][0]["pricing"]["prompt"], "0.000003");
    // Cached input costs what input does, when no price is given.
    assert_eq!(body["data"][0]["pricing"]["input_cache_read"], "0.000003");

    let preflight = Request::builder()
        .method("OPTIONS")
        .uri("/v1/chat/completions")
        .header("origin", "https://shakespeare.diy")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "authorization,content-type")
        .body(Body::empty())
        .unwrap();
    let response = router(app.clone()).oneshot(preflight).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["content-security-policy"], "frame-ancestors 'none'");
    assert_eq!(response.headers()["access-control-allow-headers"], "authorization,content-type");
}

#[tokio::test]
async fn sells_credits_for_onchain_bitcoin() {
    use crate::payments::bitcoin::tests::{FakeEsplora, FIRST, SECOND, ZPUB};
    let esplora = FakeEsplora::default();
    *esplora.tip.lock() = 800_000;
    let base = esplora.serve().await;
    let yaml = format!(
        "origin: https://ai.example.com\ndatabase: unused.sqlite3\npayments:\n  bitcoin:\n    xpub: {ZPUB}\n    esplora: {base}\n    confirmations: 2\nproviders: {{}}\nmodels: {{}}\n"
    );
    let t = temp_db();
    let app = Arc::new(App::new(Config::parse(&yaml).unwrap(), t.db.clone()).unwrap());
    app.rates.set(rust_decimal::Decimal::from(100_000));

    // No signature needed.
    let unsigned = Request::builder().uri("/v1/credits/methods").body(Body::empty()).unwrap();
    let (_, methods) = json_of(router(app.clone()).oneshot(unsigned).await.unwrap()).await;
    assert_eq!(methods["data"], json!([{ "object": "payment_method", "id": "bitcoin", "confirmations": 2 }]));

    let add = || signed("POST", "/v1/credits/add", Some(json!({ "amount": 10, "method": "bitcoin" })));
    let (status, first) = json_of(router(app.clone()).oneshot(add()).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["method"], "bitcoin");
    assert_eq!(first["status"], "pending");
    assert_eq!(first["total"], 11.0);
    // $11 at $100,000/BTC.
    assert_eq!(first["url"], format!("bitcoin:{FIRST}?amount=0.00011&label=Shakespeare%20AI%20Credits"));
    let (_, second) = json_of(router(app.clone()).oneshot(add()).await.unwrap()).await;
    assert!(second["url"].as_str().unwrap().starts_with(&format!("bitcoin:{SECOND}?")));

    let (status, body) = json_of(
        router(app.clone())
            .oneshot(signed("POST", "/v1/credits/add", Some(json!({ "amount": 10, "method": "bitcoin", "redirect_url": "https://x.example" }))))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("invalid_parameter_combination")));

    let status_of = |id: &str| app.db.with(|conn| Ok(store::get_payment(conn, id)?.unwrap().status)).unwrap();
    let first_id = first["id"].as_str().unwrap();
    let second_id = second["id"].as_str().unwrap();

    // In the mempool, then one block deep: not yet.
    esplora.pay(FIRST, 11_000, None);
    crate::payments::check_bitcoin_payments(&app).await.unwrap();
    assert_eq!(status_of(first_id), "pending");
    esplora.txs.lock().clear();
    esplora.pay(FIRST, 11_000, Some(800_000));
    crate::payments::check_bitcoin_payments(&app).await.unwrap();
    assert_eq!(status_of(first_id), "pending");
    // Two deep: credited, once.
    *esplora.tip.lock() = 800_001;
    crate::payments::check_bitcoin_payments(&app).await.unwrap();
    crate::payments::check_bitcoin_payments(&app).await.unwrap();
    assert_eq!(status_of(first_id), "completed");
    assert_eq!(balance(&app), Money(10_000_000_000));

    // Short by a sat: never credited.
    esplora.pay(SECOND, 10_999, Some(800_000));
    crate::payments::check_bitcoin_payments(&app).await.unwrap();
    assert_eq!(status_of(second_id), "pending");
    assert_eq!(balance(&app), Money(10_000_000_000));

    let (_, list) = json_of(router(app.clone()).oneshot(signed("GET", "/v1/credits/payments?method=bitcoin", None)).await.unwrap()).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 2);
    assert_eq!(list["data"][0]["description"], "Credit purchase via Bitcoin");
}

#[tokio::test]
async fn limits_the_payments_an_account_starts() {
    use crate::payments::bitcoin::tests::{FakeEsplora, ZPUB};
    let base = FakeEsplora::default().serve().await;
    let yaml = format!("origin: https://ai.example.com\ndatabase: unused.sqlite3\npayments:\n  bitcoin:\n    xpub: {ZPUB}\n    esplora: {base}\nproviders: {{}}\nmodels: {{}}\n");
    let t = temp_db();
    let app = Arc::new(App::new(Config::parse(&yaml).unwrap(), t.db.clone()).unwrap());
    app.rates.set(rust_decimal::Decimal::from(100_000));
    let add = || signed("POST", "/v1/credits/add", Some(json!({ "amount": 10, "method": "bitcoin" })));
    for _ in 0..10 {
        assert_eq!(router(app.clone()).oneshot(add()).await.unwrap().status(), StatusCode::OK);
    }
    let (status, body) = json_of(router(app.clone()).oneshot(add()).await.unwrap()).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::TOO_MANY_REQUESTS, &json!("too_many_payments")));
}

fn with_key(method: &str, path: &str, key: &str, body: Option<Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {key}"))
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(&b).unwrap())))
        .unwrap()
}

#[tokio::test]
async fn api_keys_spend_their_owners_credit_within_their_limits() {
    let (app, _t) = setup().await;
    let call = |request| {
        let app = app.clone();
        async move { json_of(router(app).oneshot(request).await.unwrap()).await }
    };
    let chat = json!({ "model": "paid", "messages": [{ "role": "user", "content": "hi" }] });

    // Made with NIP-98, with no body at all.
    let (status, created) = call(signed("POST", "/v1/keys", None)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let key = created["key"].as_str().unwrap().to_string();
    let id = created["id"].as_str().unwrap().to_string();
    assert!(key.starts_with("sk-shakespeare-") && key.len() == 15 + 64, "{key}");
    assert_eq!(created["label"], format!("sk-shakespeare-{}...{}", &key[15..19], &key[key.len() - 4..]));
    assert_eq!((&created["limit"], &created["usage"]), (&Value::Null, &json!(0.0)));

    // It spends its owner's credit.
    let (status, body) = call(with_key("POST", "/v1/chat/completions", &key, Some(chat.clone()))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::TOO_MANY_REQUESTS, &json!("insufficient_quota")));
    give(&app, 1);
    let (status, body) = call(with_key("POST", "/v1/chat/completions", &key, Some(chat.clone()))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(balance(&app), Money(1_000_000_000 - 18_000_000));
    let (_, generations) = call(signed("GET", "/v1/credits/generations", None)).await;
    assert_eq!(generations["data"][0]["api_key_id"], id.as_str());

    // But it's for inference only.
    for (method, path) in [("GET", "/v1/credits"), ("GET", "/v1/keys"), ("POST", "/v1/keys"), ("DELETE", &*format!("/v1/keys/{id}"))] {
        let (status, body) = call(with_key(method, path, &key, None)).await;
        assert_eq!((status, &body["error"]["code"]), (StatusCode::UNAUTHORIZED, &json!("nip98_required")), "{method} {path}");
    }
    let (status, body) = call(with_key("POST", "/v1/chat/completions", "sk-shakespeare-nope", Some(chat.clone()))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::UNAUTHORIZED, &json!("invalid_api_key")));

    // A limit counts what it has already spent.
    let path = format!("/v1/keys/{id}");
    let (status, body) = call(signed("PATCH", &path, Some(json!({ "name": "bot", "limit": 0.02, "limit_reset": "daily" })))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((&body["name"], &body["limit"], &body["limit_reset"]), (&json!("bot"), &json!(0.02), &json!("daily")));
    assert_eq!((&body["usage"], &body["usage_daily"], &body["limit_remaining"]), (&json!(0.018), &json!(0.018), &json!(0.002)));
    // $0.002 left: enough to start a chat, not two images.
    let (status, _) = call(with_key("POST", "/v1/chat/completions", &key, Some(chat.clone()))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(with_key("POST", "/v1/chat/completions", &key, Some(chat.clone()))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::TOO_MANY_REQUESTS, &json!("key_limit_exceeded")));
    let (status, body) = call(signed("PATCH", &path, Some(json!({ "limit": null })))).await;
    assert_eq!((status, &body["limit_remaining"]), (StatusCode::OK, &Value::Null));
    let images = json!({ "model": "gpt-image-1", "prompt": "a cat", "n": 2, "quality": "medium", "size": "1024x1024" });
    call(signed("PATCH", &path, Some(json!({ "limit": 0.1, "limit_reset": null })))).await;
    let (status, body) = call(with_key("POST", "/v1/images/generations", &key, Some(images.clone()))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::TOO_MANY_REQUESTS, &json!("key_limit_exceeded")), "{body}");
    call(signed("PATCH", &path, Some(json!({ "limit": 1 })))).await;
    let (status, body) = call(with_key("POST", "/v1/images/generations", &key, Some(images))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Free models cost nothing, so a spent limit doesn't stop them.
    call(signed("PATCH", &path, Some(json!({ "limit": 0 })))).await;
    let (status, _) = call(with_key("POST", "/v1/chat/completions", &key, Some(json!({ "model": "free", "messages": chat["messages"] })))).await;
    assert_eq!(status, StatusCode::OK);

    // Disabled, then enabled, then revoked for good.
    call(signed("PATCH", &path, Some(json!({ "limit": null, "disabled": true })))).await;
    let (status, body) = call(with_key("POST", "/v1/chat/completions", &key, Some(chat.clone()))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::UNAUTHORIZED, &json!("api_key_disabled")));
    call(signed("PATCH", &path, Some(json!({ "disabled": false })))).await;
    let (status, _) = call(with_key("POST", "/v1/chat/completions", &key, Some(chat.clone()))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(signed("DELETE", &path, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["revoked_at"].is_i64());
    let (status, body) = call(with_key("POST", "/v1/chat/completions", &key, Some(chat.clone()))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::UNAUTHORIZED, &json!("api_key_revoked")));
    let (status, body) = call(signed("PATCH", &path, Some(json!({ "disabled": false })))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::BAD_REQUEST, &json!("api_key_revoked")));

    // Listed still, with what it spent; and nobody else's to see.
    let (_, list) = call(signed("GET", "/v1/keys", None)).await;
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert!(list["data"][0]["usage"].as_f64().unwrap() > 0.1);
    assert!(list["data"][0].get("key").is_none());
    let other = "0000000000000000000000000000000000000000000000000000000000000008";
    let auth = sign_nip98(other, "GET", &format!("https://ai.example.com{path}"), None, crate::util::now());
    let request = Request::builder().uri(&path).header("authorization", auth).body(Body::empty()).unwrap();
    let (status, body) = call(request).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::NOT_FOUND, &json!("api_key_not_found")));
}

#[tokio::test]
async fn validates_api_keys() {
    let (app, _t) = setup().await;
    let call = |request| {
        let app = app.clone();
        async move { json_of(router(app).oneshot(request).await.unwrap()).await }
    };
    for (body, param) in [
        (json!({ "limit": -1 }), "limit"),
        (json!({ "limit": "5" }), "limit"),
        (json!({ "limit_reset": "hourly" }), "limit_reset"),
        (json!({ "expires_at": 1 }), "expires_at"),
        (json!({ "name": "x".repeat(101) }), "name"),
        (json!({ "disabled": true }), "disabled"),
        (json!({ "scopes": [] }), "scopes"),
    ] {
        let (status, error) = call(signed("POST", "/v1/keys", Some(body.clone()))).await;
        assert_eq!((status, &error["error"]["param"]), (StatusCode::BAD_REQUEST, &json!(param)), "{body}");
    }

    let soon = crate::util::now() + 3600;
    let (status, created) = call(signed("POST", "/v1/keys", Some(json!({ "name": "ci", "expires_at": soon })))).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!((&created["name"], &created["expires_at"]), (&json!("ci"), &json!(soon)));
    // Past its expiry, it's refused.
    app.db.with(|conn| Ok(conn.execute("UPDATE api_keys SET expires_at = 1", [])?)).unwrap();
    let chat = json!({ "model": "free", "messages": [] });
    let (status, body) = call(with_key("POST", "/v1/chat/completions", created["key"].as_str().unwrap(), Some(chat))).await;
    assert_eq!((status, &body["error"]["code"]), (StatusCode::UNAUTHORIZED, &json!("api_key_expired")));
}

#[tokio::test]
async fn refuses_a_replayed_authorization() {
    let (app, _t) = setup().await;
    give(&app, 20);
    let body = serde_json::to_vec(&json!({ "amount": 5 })).unwrap();
    let auth = sign_nip98(SECRET, "POST", "https://ai.example.com/v1/credits/giftcards", Some(&body), crate::util::now());
    let request = || {
        Request::builder().method("POST").uri("/v1/credits/giftcards").header("authorization", &auth).body(Body::from(body.clone())).unwrap()
    };
    assert_eq!(router(app.clone()).oneshot(request()).await.unwrap().status(), StatusCode::OK);
    let (status, error) = json_of(router(app.clone()).oneshot(request()).await.unwrap()).await;
    assert_eq!((status, &error["error"]["code"]), (StatusCode::UNAUTHORIZED, &json!("invalid_authorization")));
    assert_eq!(balance(&app), Money(15_000_000_000));

    // A read may be sent again: it shows nothing new.
    let auth = sign_nip98(SECRET, "GET", "https://ai.example.com/v1/credits", None, crate::util::now());
    for _ in 0..2 {
        let request = Request::builder().uri("/v1/credits").header("authorization", &auth).body(Body::empty()).unwrap();
        assert_eq!(router(app.clone()).oneshot(request).await.unwrap().status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn serves_metrics_apart_from_clients() {
    let (app, _t) = setup().await;
    let request = |method: &str, path: &str| Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
    router(app.clone()).oneshot(request("BREW", "/v1/models")).await.unwrap();
    router(app.clone()).oneshot(request("GET", "/v1/models")).await.unwrap();

    let response = router(app.clone()).oneshot(request("GET", "/metrics")).await.unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&body).contains("shakespeare_"));

    let response = crate::app::metrics_router(app.clone()).oneshot(request("GET", "/metrics")).await.unwrap();
    let text = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(text.contains(r#"method="GET",path="/v1/models",status="200""#), "{text}");
    assert!(text.contains(r#"method="OTHER""#) && !text.contains("BREW"), "{text}");
}

#[tokio::test]
async fn leaves_unknown_api_paths_to_the_api() {
    let (app, _t) = setup().await;
    for (method, path) in [("GET", "/v1/nope"), ("GET", "/v1"), ("POST", "/credits"), ("GET", "/webhooks/nope")] {
        let request = Request::builder().method(method).uri(path).body(Body::empty()).unwrap();
        let (status, body) = json_of(router(app.clone()).oneshot(request).await.unwrap()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(body["error"]["code"], "not_found");
    }
}
