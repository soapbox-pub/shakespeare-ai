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
    async fn chat(body: Bytes) -> axum::response::Response {
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "upstream-model");
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
            "usage": {"prompt_tokens": 1000, "completion_tokens": 1000, "total_tokens": 2000},
            "system_fingerprint": "dropped",
        }))
        .into_response()
    }
    async fn images(body: Bytes) -> axum::Json<Value> {
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "gpt-image-1");
        axum::Json(json!({ "created": 1, "quality": "medium", "size": "1024x1024", "data": [{"b64_json": "x"}, {"b64_json": "y"}] }))
    }
    let app = Router::new().route("/v1/chat/completions", post(chat)).route("/v1/images/generations", post(images));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/v1")
}

async fn setup() -> (Arc<App>, TempDb) {
    let upstream = fake_upstream().await;
    let yaml = format!(
        r#"
origin: https://ai.example.com
database: unused.sqlite3
providers:
  fake:
    base_url: {upstream}
    api_key: test
models:
  paid:
    upstream: fake/upstream-model
    context_window: 1000
    price: {{ input: 3, output: 15 }}
  free:
    upstream: fake/upstream-model
    context_window: 1000
    price: {{ input: 0, output: 0 }}
  gpt-image-1:
    upstream: fake/gpt-image-1
    price:
      low: {{ square: 0.011, wide: 0.016 }}
      medium: {{ square: 0.042, wide: 0.063 }}
      high: {{ square: 0.167, wide: 0.25 }}
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
async fn charges_an_estimate_when_the_client_hangs_up() {
    let (app, _t) = setup().await;
    give(&app, 1);
    let request = signed("POST", "/v1/chat/completions", Some(json!({ "model": "paid", "stream": true, "messages": [{ "role": "user", "content": "hi" }] })));
    let response = router(app.clone()).oneshot(request).await.unwrap();
    let mut body = response.into_body().into_data_stream();
    body.next().await.unwrap().unwrap();
    drop(body);

    let rows = wait_for_generations(&app, 1).await;
    assert_eq!(rows.len(), 1);
    let (cost, estimated) = rows[0];
    assert_eq!(estimated, 1);
    assert!(cost > 0);
    assert_eq!(balance(&app), Money(1_000_000_000 - cost));
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
