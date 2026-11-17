//! Buying credits: making payments, and noticing when they're paid.

pub mod lightning;
pub mod rates;
pub mod stripe;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::app::App;
use crate::money::Money;
use crate::store::{self, Method, NewPayment, Payment};

/// How long a payment stays payable. Stripe allows a Checkout session at most
/// a day.
const PAYMENT_LIFETIME: i64 = 24 * 60 * 60;

/// Make a payment for `amount` of credits, plus the fee.
pub async fn create(app: &App, pubkey: &str, amount: Money, method: Method, redirect_url: Option<&str>) -> Result<Payment> {
    let id = store::new_payment_id();
    let created_at = crate::util::now();
    let expires_at = created_at + PAYMENT_LIFETIME;
    let fee = Money::from_dollars(amount.to_decimal() * app.config.payments.fee).unwrap_or(Money::ZERO).round_to_cents();
    let total = amount.checked_add(fee).ok_or(anyhow!("amount too large"))?;

    let (url, provider_ref) = match method {
        Method::Stripe => {
            let stripe = app.config.payments.stripe.as_ref().ok_or(anyhow!("Stripe isn't configured"))?;
            let checkout = stripe::create_checkout(
                &app.http,
                &stripe.secret_key,
                &stripe::CheckoutRequest {
                    payment_id: &id,
                    pubkey,
                    credits: amount,
                    total,
                    redirect_url: redirect_url.unwrap_or_default(),
                    expires_at,
                },
            )
            .await?;
            (checkout.url, checkout.session_id)
        }
        Method::Lightning => {
            let lightning = app.config.payments.lightning.as_ref().ok_or(anyhow!("Lightning isn't configured"))?;
            let rate = app.rates.btc_usd(&app.http).await?;
            let msats = usd_to_msats(total, rate).ok_or(anyhow!("amount too large"))?;
            let comment = format!("Shakespeare AI Credits - ${amount} + ${fee} fee");
            let invoice = lightning::request_invoice(&app.http, &lightning.address, msats, &comment)
                .await
                .context("couldn't get a Lightning invoice")?;
            (invoice.bolt11, invoice.verify)
        }
    };

    let pubkey = pubkey.to_string();
    app.db
        .call(move |conn| {
            let payment = NewPayment { id: &id, pubkey: &pubkey, method, amount, fee, url: &url, provider_ref: Some(&provider_ref), created_at, expires_at };
            store::insert_payment(conn, &payment)?;
            store::get_payment(conn, &id)?.ok_or(anyhow!("the payment vanished"))
        })
        .await
}

/// Whole sats for a dollar amount at `rate` dollars per bitcoin, rounded up,
/// in millisats.
fn usd_to_msats(usd: Money, rate: Decimal) -> Option<u64> {
    let sats = (usd.to_decimal() / rate * Decimal::from(100_000_000)).ceil();
    sats.to_u64()?.checked_mul(1000)
}

/// Bring a pending Lightning payment up to date: complete it if it's been
/// paid, or expire it if its time is up and it hasn't been.
pub async fn check_lightning(app: &App, payment: &Payment) -> Result<()> {
    if payment.status != "pending" || payment.method != Method::Lightning {
        return Ok(());
    }
    let now = crate::util::now();
    // Carried over from the old service without a verify URL: nothing to ask.
    let settled = match &payment.provider_ref {
        Some(verify) => lightning::is_settled(&app.http, verify).await?,
        None => false,
    };
    let id = payment.id.clone();
    let expired = now > payment.expires_at;
    app.db
        .call(move |conn| {
            if settled {
                if store::complete_payment(conn, &id, now)? {
                    crate::log::info("payments.completed", serde_json::json!({ "id": id, "method": "lightning" }));
                }
            } else if expired {
                store::expire_payment(conn, &id)?;
            }
            Ok(())
        })
        .await
}

/// Look in on pending payments once a minute.
pub async fn poll(app: Arc<App>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        let pending = app.db.call(|conn| {
            store::expire_stripe_payments(conn, crate::util::now())?;
            store::pending_lightning(conn)
        });
        let pending = match pending.await {
            Ok(pending) => pending,
            Err(error) => {
                crate::log::error("payments.poll", serde_json::json!({ "error": format!("{error:#}") }));
                continue;
            }
        };
        for payment in pending {
            if let Err(error) = check_lightning(&app, &payment).await {
                crate::log::warn("payments.lightning", serde_json::json!({ "id": payment.id, "error": format!("{error:#}") }));
            }
        }
    }
}

/// Act on a verified Stripe webhook event.
pub async fn handle_stripe_event(app: &App, event: serde_json::Value) -> Result<()> {
    if event.get("type").and_then(|t| t.as_str()) != Some("checkout.session.completed") {
        return Ok(());
    }
    let session = event.pointer("/data/object").ok_or(anyhow!("the event has no session"))?;
    if session.get("payment_status").and_then(|s| s.as_str()) != Some("paid") {
        return Ok(());
    }
    let Some(payment_id) = session.pointer("/metadata/payment_id").and_then(|s| s.as_str()).map(str::to_string) else {
        return Ok(());
    };
    let session_id = session.get("id").and_then(|s| s.as_str()).map(str::to_string);
    let now = crate::util::now();
    let completed = app
        .db
        .call(move |conn| {
            let Some(payment) = store::get_payment(conn, &payment_id)? else {
                return Ok(None);
            };
            // The session must be the one made for this payment, where we
            // know it; payments carried over from the old service don't say.
            if payment.method != Method::Stripe || (payment.provider_ref.is_some() && payment.provider_ref != session_id) {
                return Ok(None);
            }
            Ok(Some((payment_id.clone(), store::complete_payment(conn, &payment_id, now)?)))
        })
        .await?;
    if let Some((id, true)) = completed {
        crate::log::info("payments.completed", serde_json::json!({ "id": id, "method": "stripe" }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn converts_to_msats() {
        let ten = Money::from_dollars(Decimal::from(10)).unwrap();
        // $10 at $100,000/BTC is 10,000 sats.
        assert_eq!(usd_to_msats(ten, Decimal::from(100_000)), Some(10_000_000));
        // Rounded up to a whole sat.
        assert_eq!(usd_to_msats(ten, Decimal::from_str("99999.99").unwrap()), Some(10_001_000));
    }
}
