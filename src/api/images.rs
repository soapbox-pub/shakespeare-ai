//! `/v1/images/generations`, priced per image.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::Json;
use serde_json::{json, Map, Value};

use super::chat::{send, upstream_failure};
use super::ApiError;
use crate::app::App;
use crate::config::{ImagePrice, Price};
use crate::money::Money;
use crate::store::{self, Generation};

const QUALITIES: &[&str] = &["low", "medium", "high"];
const SIZES: &[&str] = &["1024x1024", "1024x1536", "1536x1024", "auto"];
const FORMATS: &[&str] = &["png", "jpeg", "webp"];

pub async fn generations(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    let pubkey = app.authenticate(&method, &uri, &headers, &body)?;
    let request = super::json_object(&body)?;
    let params = Params::parse(&request)?;

    let not_found = || ApiError::invalid("model_not_found", format!("Model '{}' is not supported", params.model)).param("model");
    let model = app.config.models.get(&params.model).ok_or_else(not_found)?;
    let Price::Image(price) = &model.price else {
        return Err(ApiError::invalid("invalid_model_type", format!("Model '{}' cannot be used with the image generations endpoint", params.model))
            .param("model"));
    };
    let (provider_name, upstream_model) = model.upstream();
    let provider = app.config.providers.get(provider_name).ok_or_else(not_found)?;

    // Until the provider says what it made, assume the dearest it could
    // have: without a quality it picks one itself.
    let n = params.n.unwrap_or(1);
    let most = per_image(price, params.quality.as_deref().unwrap_or("high"), params.size.as_deref()).checked_mul(i64::from(n)).unwrap_or(Money(i64::MAX));
    let who = pubkey.clone();
    let balance = app.db.call(move |conn| store::balance(conn, &who)).await?;
    if balance < most {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "insufficient_quota",
            "insufficient_quota",
            format!("You need {} credits to generate {n} image(s), but you only have {} credits available", most.to_f64(), balance.to_f64()),
        )
        .with("required", most.to_f64())
        .with("available", balance.to_f64()));
    }

    let mut upstream = Map::new();
    upstream.insert("model".into(), upstream_model.into());
    upstream.insert("prompt".into(), params.prompt.into());
    for (key, value) in [("n", params.n.map(Value::from)), ("quality", params.quality.clone().map(Value::from)), ("size", params.size.clone().map(Value::from))] {
        if let Some(value) = value {
            upstream.insert(key.into(), value);
        }
    }
    for key in ["output_format", "output_compression"] {
        if let Some(value) = request.get(key).filter(|v| !v.is_null()) {
            upstream.insert(key.into(), value.clone());
        }
    }
    let response = send(&app, provider, "images/generations", &headers, serde_json::to_vec(&upstream).map_err(anyhow::Error::from)?).await?;
    let mut result: Value = response.json().await.map_err(|e| upstream_failure(&e.to_string()))?;

    // Charge for what was made, as the provider describes it.
    let made = result.get("data").and_then(Value::as_array).map_or(n, |d| d.len() as u32);
    let quality = result.get("quality").and_then(Value::as_str).filter(|q| QUALITIES.contains(q)).or(params.quality.as_deref()).unwrap_or("high");
    let size = result.get("size").and_then(Value::as_str).or(params.size.as_deref());
    let cost = per_image(price, quality, size).checked_mul(i64::from(made)).unwrap_or(Money(i64::MAX));

    let created_at = crate::util::now();
    let generation = Generation {
        id: format!("img_{}", crate::util::random_hex(32)),
        upstream_id: None,
        pubkey: pubkey.clone(),
        model: params.model.clone(),
        created_at,
        prompt_tokens: 0,
        // Images, counted as completion tokens, as they always have been.
        completion_tokens: u64::from(made),
        cost,
        estimated: false,
    };
    let id = generation.id.clone();
    app.metrics.usage(&params.model, 0, u64::from(made), cost);
    let recorded = app.db.call(move |conn| store::record_generation(conn, &generation)).await;
    let fields = json!({ "pubkey": pubkey, "model": params.model, "cost": cost.to_f64(), "images": made, "quality": quality, "size": size, "id": id });
    match recorded {
        Ok(()) => crate::log::info("image.generation", fields),
        Err(error) => crate::log::error("image.unrecorded", json!({ "generation": fields, "error": format!("{error:#}") })),
    }

    Ok(Json(json!({
        "created": result.get("created"),
        "data": result.get_mut("data").map(Value::take),
        "usage": { "prompt_tokens": 0, "completion_tokens": made, "total_tokens": made, "cost": cost.to_f64() },
    })))
}

/// One image's price. Anything but a square costs as a wide one, `auto`
/// included.
fn per_image(price: &ImagePrice, quality: &str, size: Option<&str>) -> Money {
    let sizes = match quality {
        "low" => &price.low,
        "medium" => &price.medium,
        _ => &price.high,
    };
    let dollars = if size == Some("1024x1024") { sizes.square } else { sizes.wide };
    Money::from_dollars(dollars).unwrap_or(Money(i64::MAX))
}

struct Params {
    model: String,
    prompt: String,
    n: Option<u32>,
    quality: Option<String>,
    size: Option<String>,
}

impl Params {
    fn parse(request: &Map<String, Value>) -> Result<Params, ApiError> {
        let bad = |param: &str, message: &str| ApiError::invalid("invalid_parameter", message).param(param);
        let string = |key: &str| -> Result<Option<String>, ApiError> {
            match request.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(bad(key, &format!("{key} must be a string"))),
            }
        };
        let one_of = |key: &str, allowed: &[&str]| -> Result<Option<String>, ApiError> {
            let value = string(key)?;
            if let Some(v) = &value
                && !allowed.contains(&v.as_str())
            {
                return Err(bad(key, &format!("{key} must be one of: {}", allowed.join(", "))));
            }
            Ok(value)
        };
        let model = string("model")?.ok_or_else(|| bad("model", "model is required"))?;
        let prompt = string("prompt")?.ok_or_else(|| bad("prompt", "prompt is required"))?;
        let n = match request.get("n") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_u64().filter(|n| (1..=10).contains(n)).ok_or_else(|| bad("n", "n must be a whole number from 1 to 10"))? as u32),
        };
        one_of("output_format", FORMATS)?;
        if let Some(c) = request.get("output_compression").filter(|v| !v.is_null())
            && !c.as_u64().is_some_and(|c| (1..=100).contains(&c))
        {
            return Err(bad("output_compression", "output_compression must be a whole number from 1 to 100"));
        }
        Ok(Params { model, prompt, n, quality: one_of("quality", QUALITIES)?, size: one_of("size", SIZES)? })
    }
}
