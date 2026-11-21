//! Stripe Checkout, over its REST API, and its webhook signatures.

use anyhow::{anyhow, bail, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::money::Money;

/// The API version the old service's SDK pinned; webhook payloads are shaped
/// by it too.
const API_VERSION: &str = "2025-12-15.clover";

/// How old a webhook's signed timestamp may be, as Stripe's own libraries
/// allow.
const WEBHOOK_TOLERANCE: i64 = 300;

pub struct Checkout {
    pub session_id: String,
    pub url: String,
}

pub struct CheckoutRequest<'a> {
    pub payment_id: &'a str,
    pub pubkey: &'a str,
    pub credits: Money,
    pub total: Money,
    pub redirect_url: &'a str,
    pub expires_at: i64,
}

pub async fn create_checkout(http: &reqwest::Client, secret_key: &str, r: &CheckoutRequest<'_>) -> Result<Checkout> {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("mode", "payment")
        .append_pair("payment_method_types[0]", "card")
        .append_pair("line_items[0][quantity]", "1")
        .append_pair("line_items[0][price_data][currency]", "usd")
        .append_pair("line_items[0][price_data][unit_amount]", &r.total.to_cents_ceil().to_string())
        .append_pair("line_items[0][price_data][product_data][name]", "Shakespeare AI Credits")
        .append_pair("line_items[0][price_data][product_data][description]", &format!("{} in AI credits", r.credits))
        .append_pair("success_url", r.redirect_url)
        .append_pair("cancel_url", r.redirect_url)
        .append_pair("expires_at", &r.expires_at.to_string())
        .append_pair("metadata[payment_id]", r.payment_id)
        .append_pair("metadata[pubkey]", r.pubkey)
        .append_pair("metadata[credits]", &r.credits.to_decimal().normalize().to_string())
        .append_pair("metadata[app]", "shakespeare")
        .append_pair("metadata[user]", &crate::nostr::npub_encode(r.pubkey))
        .finish();

    let response = http
        .post("https://api.stripe.com/v1/checkout/sessions")
        .bearer_auth(secret_key)
        .header("Stripe-Version", API_VERSION)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form)
        .timeout(super::TIMEOUT)
        .send()
        .await?;
    let status = response.status();
    let json: serde_json::Value = response.json().await?;
    if !status.is_success() {
        let message = json.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("unknown error");
        bail!("Stripe refused the checkout session ({status}): {message}");
    }
    let field = |name: &str| json.get(name).and_then(|v| v.as_str()).map(str::to_string).ok_or(anyhow!("Stripe's response has no {name}"));
    Ok(Checkout { session_id: field("id")?, url: field("url")? })
}

/// Check a webhook's `Stripe-Signature` and parse its event.
pub fn verify_webhook(secret: &str, payload: &[u8], header: &str, now: i64) -> Result<serde_json::Value> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
            Some(("v1", sig)) => signatures.extend(hex::decode(sig).ok()),
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(anyhow!("the signature has no timestamp"))?;
    if now.abs_diff(timestamp) > WEBHOOK_TOLERANCE as u64 {
        bail!("the signature's timestamp is too far from now");
    }

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(payload);
    let expected = mac.finalize().into_bytes();
    if !signatures.iter().any(|sig| bool::from(sig.as_slice().ct_eq(expected.as_slice()))) {
        bail!("no signature matches");
    }
    Ok(serde_json::from_slice(payload)?)
}

#[cfg(test)]
pub fn sign_webhook(secret: &str, payload: &[u8], timestamp: i64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{timestamp}.").as_bytes());
    mac.update(payload);
    format!("t={timestamp},v1={}", hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_webhooks() {
        let payload = br#"{"type":"checkout.session.completed"}"#;
        let header = sign_webhook("whsec_x", payload, 1000);
        assert!(verify_webhook("whsec_x", payload, &header, 1100).is_ok());
        assert!(verify_webhook("whsec_y", payload, &header, 1100).is_err());
        assert!(verify_webhook("whsec_x", b"{}", &header, 1100).is_err());
        assert!(verify_webhook("whsec_x", payload, &header, 2000).is_err());
        assert!(verify_webhook("whsec_x", payload, "v1=00", 1000).is_err());
    }
}
