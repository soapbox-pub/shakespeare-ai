//! `/v1/keys`: API keys, for clients that can't sign each request.
//!
//! A key is a bearer token that works for inference only, and spends its
//! owner's credit, up to a limit if it has one. Keys are made, changed and
//! revoked with NIP-98 alone, so a leaked key can't make more of itself,
//! raise its own limit, or touch the account's credit any other way.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::Json;
use serde_json::{json, Map, Value};

use super::{list_response, page_limit, ApiError};
use crate::app::App;
use crate::money::{decimal_from_json_number, Money};
use crate::store::{self, ApiKey, ApiKeyChanges, ApiKeyError, ApiKeyUsage, LimitReset, NewApiKey, MAX_API_KEYS};

const MAX_NAME: usize = 100;

pub async fn create(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, &body)?;
    // A key with no name or limit needs no body at all.
    let request = if body.is_empty() { Map::new() } else { super::json_object(&body)? };
    let now = crate::util::now();
    let changes = parse_changes(&request, now)?;
    if changes.disabled.is_some() {
        return Err(ApiError::invalid("invalid_parameter", "A new key can't be disabled").param("disabled"));
    }
    let new = NewApiKey {
        name: changes.name.unwrap_or_default(),
        limit: changes.limit.flatten(),
        limit_reset: changes.limit_reset.flatten(),
        expires_at: changes.expires_at.flatten(),
    };
    let created = app.db.call(move |conn| store::create_api_key(conn, &pubkey, new, now)).await?;
    let (key, token) = created.map_err(key_error)?;
    crate::log::info("keys.create", json!({ "pubkey": key.pubkey, "id": key.id }));
    let mut out = describe(&key, ApiKeyUsage::default());
    // The only time the key is ever shown.
    out["key"] = token.into();
    Ok(Json(out))
}

pub async fn list(
    State(app): State<Arc<App>>,
    Query(query): Query<HashMap<String, String>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let limit = page_limit(query.get("limit").map(String::as_str))?;
    let after = query.get("starting_after").filter(|v| !v.is_empty()).cloned();
    let now = crate::util::now();
    let (keys, more) = app
        .db
        .call(move |conn| {
            let (keys, more) = store::list_api_keys(conn, &pubkey, limit, after.as_deref())?;
            let keys = keys.into_iter().map(|k| Ok((store::api_key_usage(conn, &k.id, now)?, k))).collect::<anyhow::Result<Vec<_>>>()?;
            Ok((keys, more))
        })
        .await?;
    let data = keys.iter().map(|(usage, key)| describe(key, *usage)).collect();
    Ok(Json(list_response(data, more, Some("/v1/keys"))))
}

pub async fn get(State(app): State<Arc<App>>, Path(id): Path<String>, method: Method, uri: Uri, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let key = app.db.call(move |conn| store::get_api_key(conn, &pubkey, &id)).await?.ok_or_else(|| key_error(ApiKeyError::NotFound))?;
    respond(&app, key).await
}

pub async fn update(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, &body)?;
    let request = super::json_object(&body)?;
    let now = crate::util::now();
    let changes = parse_changes(&request, now)?;
    let key = app.db.call(move |conn| store::update_api_key(conn, &pubkey, &id, changes, now)).await?.map_err(key_error)?;
    crate::log::info("keys.update", json!({ "pubkey": key.pubkey, "id": key.id }));
    respond(&app, key).await
}

/// Revoke a key, for good. It stays listed, with its usage, as revoked.
pub async fn revoke(State(app): State<Arc<App>>, Path(id): Path<String>, method: Method, uri: Uri, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let now = crate::util::now();
    let key = app.db.call(move |conn| store::revoke_api_key(conn, &pubkey, &id, now)).await?.ok_or_else(|| key_error(ApiKeyError::NotFound))?;
    crate::log::info("keys.revoke", json!({ "pubkey": key.pubkey, "id": key.id }));
    respond(&app, key).await
}

async fn respond(app: &App, key: ApiKey) -> Result<Json<Value>, ApiError> {
    let id = key.id.clone();
    let now = crate::util::now();
    let usage = app.db.call(move |conn| store::api_key_usage(conn, &id, now)).await?;
    Ok(Json(describe(&key, usage)))
}

fn describe(key: &ApiKey, usage: ApiKeyUsage) -> Value {
    let counted = match key.limit_reset {
        None => usage.total,
        Some(LimitReset::Daily) => usage.daily,
        Some(LimitReset::Weekly) => usage.weekly,
        Some(LimitReset::Monthly) => usage.monthly,
    };
    let remaining = key.limit.map(|limit| Money(limit.nanos().saturating_sub(counted.nanos()).max(0)).to_f64());
    json!({
        "object": "api_key",
        "id": key.id,
        "name": key.name,
        "label": key.label,
        "disabled": key.disabled,
        "limit": key.limit.map(Money::to_f64),
        "limit_reset": key.limit_reset,
        "limit_remaining": remaining,
        "usage": usage.total.to_f64(),
        "usage_daily": usage.daily.to_f64(),
        "usage_weekly": usage.weekly.to_f64(),
        "usage_monthly": usage.monthly.to_f64(),
        "created_at": key.created_at,
        "updated_at": key.updated_at,
        "expires_at": key.expires_at,
        "revoked_at": key.revoked_at,
    })
}

/// The fields of a key a request sets. Absent leaves one alone; `null`
/// clears a limit, its reset or an expiry.
fn parse_changes(request: &Map<String, Value>, now: i64) -> Result<ApiKeyChanges, ApiError> {
    let bad = |param: &str, message: &str| ApiError::invalid("invalid_parameter", message).param(param);
    let mut changes = ApiKeyChanges::default();
    for (field, value) in request {
        match (field.as_str(), value) {
            ("name", Value::Null) => changes.name = Some(String::new()),
            ("name", Value::String(name)) if name.chars().count() <= MAX_NAME => changes.name = Some(name.trim().to_string()),
            ("name", _) => return Err(bad("name", &format!("name must be a string of at most {MAX_NAME} characters"))),
            ("disabled", Value::Bool(disabled)) => changes.disabled = Some(*disabled),
            ("disabled", _) => return Err(bad("disabled", "disabled must be true or false")),
            ("limit", Value::Null) => changes.limit = Some(None),
            ("limit", limit) => {
                let limit = Some(limit)
                    .filter(|l| l.is_number())
                    .and_then(decimal_from_json_number)
                    .and_then(Money::from_dollars)
                    .filter(|l| l.nanos() >= 0)
                    .ok_or_else(|| bad("limit", "limit must be a number of credits, at least 0, or null for no limit"))?;
                changes.limit = Some(Some(limit));
            }
            ("limit_reset", Value::Null) => changes.limit_reset = Some(None),
            ("limit_reset", Value::String(reset)) if LimitReset::parse(reset).is_some() => changes.limit_reset = Some(LimitReset::parse(reset)),
            ("limit_reset", _) => return Err(bad("limit_reset", "limit_reset must be one of: daily, weekly, monthly, or null")),
            ("expires_at", Value::Null) => changes.expires_at = Some(None),
            ("expires_at", at) if at.as_i64().is_some_and(|at| at > now) => changes.expires_at = Some(at.as_i64()),
            ("expires_at", _) => return Err(bad("expires_at", "expires_at must be a future Unix time in seconds, or null")),
            (other, _) => return Err(bad(other, &format!("Unknown parameter '{other}'"))),
        }
    }
    Ok(changes)
}

fn key_error(error: ApiKeyError) -> ApiError {
    match error {
        ApiKeyError::NotFound => ApiError::invalid("api_key_not_found", "No API key found with that ID").status(StatusCode::NOT_FOUND),
        ApiKeyError::Revoked => ApiError::invalid("api_key_revoked", "This API key has been revoked, and can't be changed"),
        ApiKeyError::TooMany => {
            ApiError::invalid("too_many_api_keys", format!("An account can have at most {MAX_API_KEYS} API keys. Revoke one to make another."))
        }
    }
}
