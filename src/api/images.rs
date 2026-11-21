//! `/v1/images/generations`, priced per image.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::Json;
use serde_json::{json, Map, Value};

use super::upstream::{self, upstream_failure};
use super::ApiError;
use crate::app::App;
use crate::config::{ImagePrice, Kind};
use crate::money::Money;
use crate::store::{self, Generation};

const QUALITIES: &[&str] = &["low", "medium", "high"];
const SIZES: &[&str] = &["1024x1024", "1024x1536", "1536x1024", "auto"];
const FORMATS: &[&str] = &["png", "jpeg", "webp"];

pub async fn generations(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, ApiError> {
    let caller = app.authenticate_inference(&method, &uri, &headers, &body).await?;
    let pubkey = caller.pubkey.clone();
    let request = super::json_object(&body)?;
    let params = Params::parse(&request)?;

    let not_found = || ApiError::invalid("model_not_found", format!("Model '{}' is not supported", params.model)).param("model");
    let model = app.config.models.get(&params.model).ok_or_else(not_found)?;
    let Kind::Image { price } = &model.kind else {
        return Err(ApiError::invalid("invalid_model_type", format!("Model '{}' cannot be used with the image generations endpoint", params.model))
            .param("model"));
    };
    let _slot = app.take_slot(&pubkey)?;

    // Until the provider says what it made, assume the dearest it could
    // have: without a quality it picks one itself.
    let n = params.n.unwrap_or(1);
    let most = per_image(price, params.quality.as_deref().unwrap_or("high"), params.size.as_deref()).checked_mul(i64::from(n)).unwrap_or(Money(i64::MAX));
    app.check_key_limit(&caller, most).await?;
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

    let mut body = Map::new();
    body.insert("prompt".into(), params.prompt.clone().into());
    for (key, value) in [("n", params.n.map(Value::from)), ("quality", params.quality.clone().map(Value::from)), ("size", params.size.clone().map(Value::from))] {
        if let Some(value) = value {
            body.insert(key.into(), value);
        }
    }
    for key in ["output_format", "output_compression"] {
        if let Some(value) = request.get(key).filter(|v| !v.is_null()) {
            body.insert(key.into(), value.clone());
        }
    }
    let (response, upstream) = upstream::route(&app, &params.model, &model.upstreams, |upstream, provider| {
        let mut body = body.clone();
        body.insert("model".into(), upstream.model.clone().into());
        for (key, value) in &upstream.params {
            body.insert(key.clone(), value.clone());
        }
        let app = &app;
        async move { upstream::post(app, provider, &upstream.label(), "images/generations", serde_json::to_vec(&body).unwrap_or_default()).await }
    })
    .await?;
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
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost,
        estimated: false,
        api_key_id: caller.api_key_id(),
        upstream: Some(upstream.label()),
        upstream_cost: None,
    };
    let id = generation.id.clone();
    app.metrics.usage(&params.model, 0, u64::from(made), cost);
    let recorded = app.db.call(move |conn| store::record_generation(conn, &generation)).await;
    let fields = json!({ "pubkey": pubkey, "api_key": caller.api_key_id(), "model": params.model, "cost": cost.to_f64(), "upstream": upstream.label(), "images": made, "quality": quality, "size": size, "id": id });
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
