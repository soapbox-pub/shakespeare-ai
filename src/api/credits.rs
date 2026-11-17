//! Credits: the balance, buying more, and what's been spent.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::Json;
use serde_json::{json, Value};

use super::{list_response, page_limit, tagged, ApiError};
use crate::app::App;
use crate::money::{decimal_from_json_number, Money};
use crate::store::{self, Method as PaymentMethod};

pub async fn balance(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let amount = app.db.call(move |conn| store::balance(conn, &pubkey)).await?;
    Ok(Json(json!({ "object": "credits", "amount": amount.to_f64() })))
}

pub async fn add(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, &body)?;
    let request = super::json_object(&body)?;
    let minimum = app.config.payments.minimum();
    let too_small = || {
        ApiError::invalid("minimum_amount_not_met", format!("Minimum credit amount is ${minimum}"))
            .param("amount")
            .with("minimum_amount", minimum.to_f64())
    };

    let amount = request
        .get("amount")
        .filter(|a| a.is_number())
        .and_then(decimal_from_json_number)
        .and_then(Money::from_dollars)
        .ok_or_else(|| ApiError::invalid("invalid_parameter", "amount must be a number").param("amount"))?;
    if amount < minimum {
        return Err(too_small());
    }

    let method_name = request.get("method").and_then(Value::as_str).unwrap_or_default();
    let unsupported = || ApiError::invalid("unsupported_method", format!("Payment method '{method_name}' is not supported")).param("method");
    let payment_method = PaymentMethod::parse(method_name).ok_or_else(unsupported)?;
    let configured = match payment_method {
        PaymentMethod::Stripe => app.config.payments.stripe.is_some(),
        PaymentMethod::Lightning => app.config.payments.lightning.is_some(),
    };
    if !configured {
        return Err(unsupported());
    }

    let redirect_url = match request.get("redirect_url") {
        None | Some(Value::Null) => None,
        Some(Value::String(url)) if url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) => Some(url.clone()),
        Some(_) => return Err(ApiError::invalid("invalid_url", "Invalid URL").param("redirect_url")),
    };
    match (payment_method, &redirect_url) {
        (PaymentMethod::Lightning, Some(_)) => {
            return Err(ApiError::invalid("invalid_parameter_combination", "redirect_url can only be used with stripe payment method").param("redirect_url"));
        }
        (PaymentMethod::Stripe, None) => {
            return Err(ApiError::invalid("missing_required_parameter", "redirect_url is required for stripe payment method").param("redirect_url"));
        }
        _ => {}
    }

    let payment = crate::payments::create(&app, &pubkey, amount, payment_method, redirect_url.as_deref()).await.map_err(|error| {
        crate::log::error("payments.create", json!({ "method": method_name, "error": format!("{error:#}") }));
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "api_error",
            "payment_provider_error",
            format!("Couldn't start a {method_name} payment. Try again, or use another method."),
        )
    })?;
    Ok(Json(tagged("payment", payment)))
}

pub async fn get_payment(State(app): State<Arc<App>>, Path(id): Path<String>, method: Method, uri: Uri, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let not_found = || ApiError::invalid("payment_not_found", format!("No payment found with ID '{id}'")).status(StatusCode::NOT_FOUND);
    let lookup = |app: &Arc<App>, id: String| {
        let db = app.db.clone();
        async move { db.call(move |conn| store::get_payment(conn, &id)).await }
    };
    let payment = lookup(&app, id.clone()).await?.filter(|p| p.pubkey == pubkey).ok_or_else(not_found)?;

    // Asking about a pending Lightning payment checks on it now, rather than
    // waiting for the next poll.
    if payment.status == "pending" && payment.method == PaymentMethod::Lightning {
        if let Err(error) = crate::payments::check_lightning(&app, &payment).await {
            crate::log::warn("payments.lightning", json!({ "id": payment.id, "error": format!("{error:#}") }));
        }
        let payment = lookup(&app, id.clone()).await?.ok_or_else(not_found)?;
        return Ok(Json(tagged("payment", payment)));
    }
    Ok(Json(tagged("payment", payment)))
}

pub async fn list_payments(
    State(app): State<Arc<App>>,
    Query(query): Query<HashMap<String, String>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let limit = page_limit(query.get("limit").map(String::as_str))?;
    let nonempty = |key: &str| query.get(key).filter(|v| !v.is_empty()).cloned();
    let (after, status, payment_method) = (nonempty("starting_after"), nonempty("status"), nonempty("method"));
    let (payments, more) = app
        .db
        .call(move |conn| store::list_payments(conn, &pubkey, limit, after.as_deref(), status.as_deref(), payment_method.as_deref()))
        .await?;
    let data = payments.into_iter().map(|p| tagged("payment", p)).collect();
    Ok(Json(list_response(data, more, Some("/v1/credits/payments"))))
}

pub async fn list_generations(
    State(app): State<Arc<App>>,
    Query(query): Query<HashMap<String, String>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, b"")?;
    let limit = page_limit(query.get("limit").map(String::as_str))?;
    let after = query.get("starting_after").filter(|v| !v.is_empty()).cloned();
    let (generations, more) = app.db.call(move |conn| store::list_generations(conn, &pubkey, limit, after.as_deref())).await?;
    let data = generations
        .into_iter()
        .map(|g| {
            json!({
                "object": "generation",
                "id": g.id,
                "model": g.model,
                "created": g.created_at,
                "usage": {
                    "prompt_tokens": g.prompt_tokens,
                    "completion_tokens": g.completion_tokens,
                    "total_tokens": g.prompt_tokens + g.completion_tokens,
                },
                "cost": g.cost.to_f64(),
            })
        })
        .collect();
    Ok(Json(list_response(data, more, Some("/v1/credits/generations"))))
}

pub async fn stripe_webhook(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    let Some(signature) = headers.get("stripe-signature").and_then(|s| s.to_str().ok()) else {
        return Err(ApiError::invalid("missing_signature", "Missing Stripe signature header"));
    };
    let Some(stripe) = &app.config.payments.stripe else {
        return Err(ApiError::invalid("webhook_processing_failed", "Stripe isn't configured"));
    };
    let event = crate::payments::stripe::verify_webhook(&stripe.webhook_secret, &body, signature, crate::util::now()).map_err(|error| {
        crate::log::warn("stripe.webhook", json!({ "error": format!("{error:#}") }));
        ApiError::new(StatusCode::BAD_REQUEST, "webhook_error", "webhook_processing_failed", "Failed to process webhook")
    })?;
    // A failure here is a 500, so Stripe tries again later.
    crate::payments::handle_stripe_event(&app, event).await?;
    Ok(Json(json!({ "received": true })))
}
