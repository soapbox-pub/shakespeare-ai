use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::app::App;
use crate::config::{Model, Price};

/// What `/v1/models` has always said for `created`.
const DEFAULT_CREATED: i64 = 1757894400;

pub async fn list(State(app): State<Arc<App>>) -> Json<Value> {
    let data: Vec<Value> =
        app.config.models.iter().filter(|(_, m)| !m.hidden && !m.is_free_chat()).map(|(id, model)| describe(id, model)).collect();
    Json(json!({ "object": "list", "data": data }))
}

fn describe(id: &str, model: &Model) -> Value {
    let mut out = json!({
        "id": id,
        "name": model.name.as_deref().unwrap_or(id),
        "description": model.description,
        "object": "model",
        "owned_by": "system",
        "created": model.created.unwrap_or(DEFAULT_CREATED),
    });
    match &model.price {
        Price::Tokens(price) => {
            let per_token = |p: Decimal| (p / Decimal::from(1_000_000)).normalize().to_string();
            out["type"] = "chat".into();
            out["context_window"] = model.context_window.into();
            out["pricing"] = json!({ "prompt": per_token(price.input), "completion": per_token(price.output) });
        }
        Price::Image(price) => {
            let s = |p: Decimal| p.normalize().to_string();
            out["type"] = "image".into();
            out["pricing"] = json!({
                "low_1024": s(price.low.square),
                "low_1024_wide": s(price.low.wide),
                "medium_1024": s(price.medium.square),
                "medium_1024_wide": s(price.medium.wide),
                "high_1024": s(price.high.square),
                "high_1024_wide": s(price.high.wide),
            });
        }
    }
    out
}
