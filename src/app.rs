//! The service's shared state, and its routes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use parking_lot::Mutex;

use crate::api::{self, ApiError};
use crate::config::Config;
use crate::db::Db;
use crate::metrics::Metrics;
use crate::money::Money;
use crate::payments::rates::Rates;
use crate::store::{self, ApiKey, Unusable};

pub struct App {
    pub config: Config,
    pub db: Db,
    pub http: reqwest::Client,
    pub rates: Rates,
    pub metrics: Metrics,
    pub limiter: RateLimiter,
    pub dashboard: crate::dashboard::Source,
}

impl App {
    pub fn new(config: Config, db: Db) -> anyhow::Result<App> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("shakespeare-ai/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            // Between bytes, not overall: a long completion streams for many
            // minutes, but an upstream silent this long is gone.
            .read_timeout(Duration::from_secs(600))
            .build()?;
        Ok(App { config, db, http, rates: Rates::default(), metrics: Metrics::default(), limiter: RateLimiter::default(), dashboard: crate::dashboard::source() })
    }

    /// The pubkey a request's NIP-98 header was signed by. Only NIP-98 will
    /// do: API keys are for inference, not for managing an account.
    pub fn authenticate(&self, method: &Method, uri: &axum::http::Uri, headers: &HeaderMap, body: &[u8]) -> Result<String, ApiError> {
        let header = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());
        if header.is_some_and(|h| bearer_token(h).is_some()) {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "nip98_required",
                "API keys can only be used for inference. Sign this request with NIP-98.",
            ));
        }
        let path_and_query = uri.path_and_query().map_or("/", |p| p.as_str());
        let host = self.config.origin.host_str().unwrap_or_default();
        crate::nostr::verify_nip98(header, method.as_str(), path_and_query, body, host, crate::util::now())
            .map_err(|message| ApiError::new(StatusCode::UNAUTHORIZED, "authentication_error", "invalid_authorization", message))
    }

    /// Who's paying for an inference request: an API key's owner, if it
    /// carries `Authorization: Bearer <key>`, or else whoever signed it with
    /// NIP-98.
    pub async fn authenticate_inference(&self, method: &Method, uri: &axum::http::Uri, headers: &HeaderMap, body: &[u8]) -> Result<Caller, ApiError> {
        let header = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());
        let Some(token) = header.and_then(bearer_token) else {
            return Ok(Caller { pubkey: self.authenticate(method, uri, headers, body)?, api_key: None });
        };
        let error = |code: &str, message: &str| ApiError::new(StatusCode::UNAUTHORIZED, "authentication_error", code, message);
        let token = token.to_string();
        let key = self.db.call(move |conn| store::api_key_by_token(conn, &token)).await?.ok_or_else(|| error("invalid_api_key", "Invalid API key"))?;
        match key.unusable(crate::util::now()) {
            None => Ok(Caller { pubkey: key.pubkey.clone(), api_key: Some(key) }),
            Some(Unusable::Revoked) => Err(error("api_key_revoked", "This API key has been revoked")),
            Some(Unusable::Disabled) => Err(error("api_key_disabled", "This API key is disabled")),
            Some(Unusable::Expired) => Err(error("api_key_expired", "This API key has expired")),
        }
    }

    /// Refuse a request an API key's spend limit can't cover: `needed` more
    /// than it has left, or nothing left at all. NIP-98 has no limit.
    pub async fn check_key_limit(&self, caller: &Caller, needed: Money) -> Result<(), ApiError> {
        let Some(key) = &caller.api_key else { return Ok(()) };
        let Some(limit) = key.limit else { return Ok(()) };
        let (id, since) = (key.id.clone(), key.limit_since(crate::util::now()));
        let spent = self.db.call(move |conn| store::api_key_spent(conn, &id, since)).await?;
        let remaining = Money(limit.nanos().saturating_sub(spent.nanos()).max(0));
        if !remaining.is_positive() || remaining < needed {
            let period = key.limit_reset.map_or(String::new(), |r| format!(" {}", r.as_str()));
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "insufficient_quota",
                "key_limit_exceeded",
                format!("This API key has reached its{period} spending limit of {} credits", limit.to_f64()),
            )
            .with("limit", limit.to_f64())
            .with("limit_remaining", remaining.to_f64()));
        }
        Ok(())
    }
}

/// Who an inference request is from, and the key it came with, if any.
pub struct Caller {
    pub pubkey: String,
    pub api_key: Option<ApiKey>,
}

impl Caller {
    pub fn api_key_id(&self) -> Option<String> {
        self.api_key.as_ref().map(|k| k.id.clone())
    }
}

/// The token in `Bearer <token>`, if that's what the header is. An empty one
/// is no key at all: the OpenAI SDK sends `Bearer ` when given none.
fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, token) = header.split_once(' ')?;
    Some(token.trim()).filter(|t| scheme.eq_ignore_ascii_case("bearer") && !t.is_empty())
}

/// Chat requests carry whole conversations, images included.
const MAX_BODY: usize = 64 * 1024 * 1024;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route("/v1/models", get(api::models::list))
        .route("/v1/chat/completions", post(api::chat::completions))
        .route("/v1/images/generations", post(api::images::generations))
        .route("/v1/credits", get(api::credits::balance))
        .route("/v1/credits/add", post(api::credits::add))
        .route("/v1/credits/methods", get(api::credits::methods))
        .route("/v1/credits/payments", get(api::credits::list_payments))
        .route("/v1/credits/payments/{payment_id}", get(api::credits::get_payment))
        .route("/v1/credits/giftcards", post(api::giftcards::create).get(api::giftcards::list))
        .route("/v1/credits/giftcards/redeem", post(api::giftcards::redeem))
        .route("/v1/credits/giftcards/check/{code}", get(api::giftcards::check))
        .route("/v1/credits/giftcards/{giftcard_id}", get(api::giftcards::get).delete(api::giftcards::cancel))
        .route("/v1/credits/generations", get(api::credits::list_generations))
        .route("/v1/keys", post(api::keys::create).get(api::keys::list))
        .route("/v1/keys/{key_id}", get(api::keys::get).patch(api::keys::update).delete(api::keys::revoke))
        .route("/webhooks/stripe", post(api::credits::stripe_webhook))
        .fallback(fallback)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(app.clone(), observe))
        .layer(middleware::from_fn(cors))
        .with_state(app)
}

/// The API's own paths are JSON errors; anything else is the dashboard's, a
/// file or one of its routes.
async fn fallback(State(app): State<Arc<App>>, method: Method, uri: axum::http::Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    let api = path == "/v1" || path.starts_with("/v1/") || path.starts_with("/webhooks/");
    if api || !matches!(method, Method::GET | Method::HEAD) {
        return ApiError::new(StatusCode::NOT_FOUND, "invalid_request_error", "not_found", "Not found").into_response();
    }
    let path = path.to_string();
    tokio::task::spawn_blocking(move || app.dashboard.serve(&path, &headers)).await.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn metrics(State(app): State<Arc<App>>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")], app.metrics.render())
}

/// Anyone may call this API from a browser: it's authenticated per request,
/// never by cookie.
async fn cors(request: Request, next: Next) -> Response {
    let allow_origin = HeaderValue::from_static("*");
    if request.method() == Method::OPTIONS && request.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD) {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let headers = response.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin);
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET,HEAD,PUT,POST,DELETE,PATCH"));
        if let Some(requested) = request.headers().get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
            headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone());
        }
        headers.insert(header::VARY, HeaderValue::from_static("Access-Control-Request-Headers"));
        return response;
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin);
    response
}

/// Log each request and its response, and count responses by route.
async fn observe(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let pathname = request.uri().path().to_string();
    let route = request.extensions().get::<MatchedPath>().map_or_else(|| "unmatched".to_string(), |p| p.as_str().to_string());
    let ip = request.headers().get("x-real-ip").and_then(|v| v.to_str().ok()).map(str::to_string);
    let start = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    app.metrics.response(method.as_str(), &route, status);
    let level = if status >= 500 { crate::log::error } else { crate::log::info };
    level(
        "http.response",
        serde_json::json!({
            "method": method.as_str(),
            "pathname": pathname,
            "status": status,
            "duration": start.elapsed().as_secs_f64(),
            "ip": ip,
        }),
    );
    response
}

/// Fixed hourly windows per client IP, for the endpoints that guess at gift
/// card codes. Only behind a proxy that sets `X-Real-IP`; without one there's
/// no telling clients apart.
/// (bucket, IP) → when its window started, and requests in it.
type Windows = HashMap<(&'static str, String), (Instant, u32)>;

#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<Windows>,
}

const WINDOW: Duration = Duration::from_secs(60 * 60);

impl RateLimiter {
    pub fn check(&self, bucket: &'static str, headers: &HeaderMap, limit: u32) -> Result<(), ApiError> {
        let Some(ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) else {
            return Ok(());
        };
        let now = Instant::now();
        let mut windows = self.windows.lock();
        if windows.len() > 100_000 {
            windows.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
        }
        let entry = windows.entry((bucket, ip.to_string())).or_insert((now, 0));
        if now.duration_since(entry.0) >= WINDOW {
            *entry = (now, 0);
        }
        entry.1 += 1;
        if entry.1 > limit {
            return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limit_error", "rate_limited", "Too many requests, please try again later."));
        }
        Ok(())
    }
}
