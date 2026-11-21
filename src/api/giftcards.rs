//! Gift cards: credit bought for someone else, redeemed with a code.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

use super::{list_response, page_limit, tagged, ApiError};
use crate::app::{App, ClientIp};
use crate::money::{decimal_from_json_number, Money};
use crate::store::{self, GiftcardError};

/// Per IP per hour: generous, for a conference full of people on one network.
const REDEEM_LIMIT: u32 = 1000;
const CHECK_LIMIT: u32 = 2000;

/// Codes are spelled in capitals with dashes; be forgiving of how they were
/// typed or pasted.
fn normalize_code(code: &str) -> String {
    code.trim().to_ascii_uppercase()
}

pub async fn create(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, &body)?;
    let request = super::json_object(&body)?;
    let minimum = app.config.payments.minimum();

    let amount = request
        .get("amount")
        .filter(|a| a.is_number())
        .and_then(decimal_from_json_number)
        .and_then(Money::from_dollars)
        .ok_or_else(|| ApiError::invalid("invalid_parameter", "amount must be a number").param("amount"))?;
    if amount < minimum {
        return Err(ApiError::invalid("minimum_amount_not_met", format!("Minimum giftcard amount is ${minimum}"))
            .param("amount")
            .with("minimum_amount", minimum.to_f64()));
    }
    let quantity = match request.get("quantity") {
        None | Some(Value::Null) => Some(1),
        Some(q) => q.as_u64().filter(|q| (1..=100).contains(q)).map(|q| q as u32),
    }
    .ok_or_else(|| ApiError::invalid("invalid_quantity", "Quantity must be between 1 and 100").param("quantity"))?;

    let now = crate::util::now();
    let result = app.db.call(move |conn| store::create_giftcards(conn, &pubkey, amount, quantity, true, now)).await?;
    match result {
        Ok(cards) => Ok(Json(json!({ "object": "list", "data": cards.into_iter().map(|c| tagged("giftcard", c)).collect::<Vec<_>>() }))),
        Err(GiftcardError::Insufficient { required, available }) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "insufficient_quota",
            "insufficient_credits",
            format!(
                "You need {required} credits to create {quantity} giftcard{} of {amount} each, but you only have {available} credits available",
                if quantity > 1 { "s" } else { "" }
            ),
        )
        .with("required", required.to_f64())
        .with("available", available.to_f64())),
        Err(_) => Err(ApiError::internal()),
    }
}

pub async fn redeem(State(app): State<Arc<App>>, Extension(ip): Extension<ClientIp>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    app.limiter.check("redeem", &ip, REDEEM_LIMIT)?;
    let pubkey = app.authenticate(&method, &uri, &headers, &body)?;
    let request = super::json_object(&body)?;
    let code = request
        .get("code")
        .and_then(Value::as_str)
        .map(normalize_code)
        .ok_or_else(|| ApiError::invalid("missing_required_parameter", "Missing or invalid 'code' parameter").param("code"))?;

    let now = crate::util::now();
    let attempt = code.clone();
    let result = app.db.call(move |conn| store::redeem_giftcard(conn, &pubkey, &attempt, now)).await?;
    match result {
        Ok(amount) => Ok(Json(json!({ "object": "giftcard_redemption", "amount": amount.to_f64() }))),
        Err(GiftcardError::NotFound) => {
            Err(ApiError::invalid("giftcard_not_found", format!("No giftcard found with code '{code}'")).param("code").status(StatusCode::NOT_FOUND))
        }
        Err(GiftcardError::Cancelled) => Err(ApiError::invalid("giftcard_cancelled", "This giftcard has been cancelled").param("code")),
        Err(GiftcardError::Redeemed) => Err(ApiError::invalid("giftcard_already_redeemed", "This giftcard has already been fully redeemed").param("code")),
        Err(GiftcardError::Insufficient { .. }) => Err(ApiError::internal()),
    }
}

/// Anyone holding a code may look it up, without signing in.
pub async fn check(State(app): State<Arc<App>>, Extension(ip): Extension<ClientIp>, Path(code): Path<String>) -> Result<Json<Value>, ApiError> {
    app.limiter.check("check", &ip, CHECK_LIMIT)?;
    let code = normalize_code(&code);
    let lookup = code.clone();
    let card = app.db.call(move |conn| store::giftcard_by_code(conn, &lookup)).await?;
    match card.filter(|c| c.status != "cancelled") {
        Some(card) => Ok(Json(tagged("giftcard", card))),
        None => Err(ApiError::invalid("giftcard_not_found", format!("No giftcard found with code '{code}'")).param("code").status(StatusCode::NOT_FOUND)),
    }
}

pub async fn get(State(app): State<Arc<App>>, Path(id): Path<String>, method: Method, uri: Uri, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let lookup = id.clone();
    let card = app.db.call(move |conn| store::get_giftcard(conn, &pubkey, &lookup)).await?;
    card.map(|c| Json(tagged("giftcard", c)))
        .ok_or_else(|| ApiError::invalid("giftcard_not_found", format!("No giftcard found with ID '{id}'")).status(StatusCode::NOT_FOUND))
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
    let after = query.get("min_id").filter(|v| !v.is_empty()).cloned();
    let (cards, more) = app.db.call(move |conn| store::list_giftcards(conn, &pubkey, limit, after.as_deref())).await?;
    Ok(Json(list_response(cards.into_iter().map(|c| tagged("giftcard", c)).collect(), more, None)))
}

/// Cancel an unredeemed card and refund its sender.
pub async fn cancel(State(app): State<Arc<App>>, Path(id): Path<String>, method: Method, uri: Uri, headers: HeaderMap) -> Result<Response, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let now = crate::util::now();
    let target = id.clone();
    let result = app.db.call(move |conn| store::cancel_giftcard(conn, &pubkey, &target, now)).await?;
    match result {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(GiftcardError::NotFound) => Err(ApiError::invalid("giftcard_not_found", format!("No giftcard found with ID '{id}'")).status(StatusCode::NOT_FOUND)),
        Err(GiftcardError::Cancelled) => Err(ApiError::invalid("giftcard_already_cancelled", "This giftcard has already been cancelled")),
        Err(GiftcardError::Redeemed) => Err(ApiError::invalid("giftcard_already_redeemed", "Cannot cancel a fully redeemed giftcard")),
        Err(GiftcardError::Insufficient { .. }) => Err(ApiError::internal()),
    }
}
