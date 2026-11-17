//! The HTTP API: OpenAI-shaped, errors included.

pub mod chat;
pub mod credits;
pub mod giftcards;
pub mod images;
pub mod models;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

/// An OpenAI-style error: `{"error": {message, type, param, code, ...}}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Map<String, Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, kind: &str, code: &str, message: impl Into<String>) -> ApiError {
        let mut body = Map::new();
        body.insert("message".into(), Value::String(message.into()));
        body.insert("type".into(), kind.into());
        body.insert("param".into(), Value::Null);
        body.insert("code".into(), code.into());
        ApiError { status, body }
    }

    pub fn invalid(code: &str, message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, "invalid_request_error", code, message)
    }

    pub fn param(mut self, param: &str) -> ApiError {
        self.body.insert("param".into(), param.into());
        self
    }

    pub fn with(mut self, key: &str, value: impl Into<Value>) -> ApiError {
        self.body.insert(key.into(), value.into());
        self
    }

    pub fn status(mut self, status: StatusCode) -> ApiError {
        self.status = status;
        self
    }

    pub fn invalid_json() -> ApiError {
        ApiError::invalid("invalid_json", "Invalid JSON body")
    }

    pub fn internal() -> ApiError {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "api_error", "internal_error", "Internal server error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.body }))).into_response()
    }
}

/// Anything unexpected is logged and becomes a bare 500: its details are ours,
/// not the client's.
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> ApiError {
        crate::log::error("http.error", json!({ "error": format!("{error:#}") }));
        ApiError::internal()
    }
}

/// A request body that must be a JSON object.
pub fn json_object(body: &[u8]) -> Result<Map<String, Value>, ApiError> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) | Err(_) => Err(ApiError::invalid_json()),
    }
}

/// A list endpoint's `limit`: 20 unless given, and at most 100.
pub fn page_limit(raw: Option<&str>) -> Result<usize, ApiError> {
    let limit = raw.and_then(|l| l.trim().parse::<i64>().ok()).filter(|l| *l > 0).unwrap_or(20);
    if limit > 100 {
        return Err(ApiError::invalid("invalid_limit", "Maximum limit is 100").param("limit"));
    }
    Ok(limit as usize)
}

pub fn list_response(data: Vec<Value>, has_more: bool, url: Option<&str>) -> Value {
    let mut body = json!({ "object": "list", "data": data, "has_more": has_more });
    if let Some(url) = url {
        body["url"] = url.into();
    }
    body
}

/// An object with `"object": kind` first, as the API has always written them.
pub fn tagged(kind: &str, value: impl serde::Serialize) -> Value {
    let mut out = Map::new();
    out.insert("object".into(), kind.into());
    if let Ok(Value::Object(fields)) = serde_json::to_value(value) {
        out.extend(fields);
    }
    Value::Object(out)
}
