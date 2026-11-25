//! Buying credits: making payments, and noticing when they're paid.

pub mod bitcoin;
pub mod lightning;
pub mod rates;
pub mod stripe;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::app::App;
use crate::money::Money;
use crate::store::{self, Method, NewPayment, Payment};

/// How long a payment stays payable. Stripe allows a Checkout session at most
/// a day.
const PAYMENT_LIFETIME: i64 = 24 * 60 * 60;

/// The most any request to a payment provider, Esplora or an exchange rate
/// API may take, start to finish. The client's own timeout is between bytes,
/// and long, for streaming completions; a poll stuck that long would hold up
/// every other payment's.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// How many payments are checked on at once.
const CONCURRENT_CHECKS: usize = 8;

/// How long after a bitcoin payment expires we keep watching its address: for
/// a payment sent late, or one sent in time that's slow to confirm.
const BITCOIN_GRACE: i64 = 7 * 24 * 60 * 60;

/// Make a payment for `amount` of credits, plus the fee.
pub async fn create(app: &App, pubkey: &str, amount: Money, method: Method, redirect_url: Option<&str>) -> Result<Payment> {
    let id = store::new_payment_id();
    let created_at = crate::util::now();
    let expires_at = created_at + PAYMENT_LIFETIME;
    let fee = Money::from_dollars(amount.to_decimal() * app.config.payments.fee).unwrap_or(Money::ZERO).round_to_cents();
    let total = amount.checked_add(fee).ok_or(anyhow!("amount too large"))?;

    if method == Method::Bitcoin {
        return create_bitcoin(app, pubkey, id, amount, fee, total, created_at, expires_at).await;
    }

    let (url, provider_ref) = match method {
        Method::Bitcoin => unreachable!("handled above"),
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

/// A bitcoin payment: the account's next receive address, given out in the
/// same transaction that records it, so two payments never share one.
#[allow(clippy::too_many_arguments)]
async fn create_bitcoin(app: &App, pubkey: &str, id: String, amount: Money, fee: Money, total: Money, created_at: i64, expires_at: i64) -> Result<Payment> {
    let bitcoin = app.config.payments.bitcoin.as_ref().ok_or(anyhow!("Bitcoin isn't configured"))?;
    let rate = app.rates.btc_usd(&app.http).await?;
    let sats = usd_to_sats(total, rate).ok_or(anyhow!("amount too large"))?;
    let wallet = bitcoin.xpub.clone();
    let pubkey = pubkey.to_string();
    app.db
        .call(move |conn| {
            let tx = store::write_tx(conn)?;
            let account = wallet.id();
            let index = store::next_bitcoin_index(&tx, &account)?;
            let address = wallet.address(index)?;
            let url = bitcoin::uri(&address, sats);
            let payment =
                NewPayment { id: &id, pubkey: &pubkey, method: Method::Bitcoin, amount, fee, url: &url, provider_ref: Some(&address), created_at, expires_at };
            store::insert_payment(&tx, &payment)?;
            store::insert_bitcoin_address(&tx, &id, &account, index, &address, sats)?;
            tx.commit()?;
            store::get_payment(conn, &id)?.ok_or(anyhow!("the payment vanished"))
        })
        .await
}

/// Whole sats for a dollar amount at `rate` dollars per bitcoin, rounded up.
fn usd_to_sats(usd: Money, rate: Decimal) -> Option<u64> {
    (usd.to_decimal() / rate * Decimal::from(100_000_000)).ceil().to_u64()
}

/// [`usd_to_sats`], in millisats.
fn usd_to_msats(usd: Money, rate: Decimal) -> Option<u64> {
    usd_to_sats(usd, rate)?.checked_mul(1000)
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

/// Bring a bitcoin payment up to date, given the best block's height, and
/// credit it for whatever confirmed sats were sent, more or less than it
/// asked for: at once when it's paid in full, or once its time is up when
/// it's paid short (a top-up before then still counts), as soon as what was
/// sent has confirmed. One that's been sent nothing expires with its time.
///
/// Something unconfirmed keeps a payment pending past its time, for up to
/// [`BITCOIN_GRACE`]; after that, what's confirmed is what's credited.
async fn check_bitcoin(app: &App, config: &crate::config::Bitcoin, payment: &Payment, watch: &store::BitcoinWatch, tip: u64) -> Result<()> {
    let Some(address) = &payment.provider_ref else { return Ok(()) };
    let received = bitcoin::received(&app.http, &config.esplora(), address, tip, config.confirmations).await?;
    let now = crate::util::now();
    let id = payment.id.clone();
    let seen = received.seen;
    app.db.call(move |conn| store::record_bitcoin_check(conn, &id, seen, now, tip)).await?;

    let owed = watch.sats;
    let past_due = now > payment.expires_at;
    let settled = received.seen == received.confirmed || now > payment.expires_at + BITCOIN_GRACE;
    if received.confirmed > 0 && (received.confirmed >= owed || (past_due && settled)) {
        let (amount, fee) = (pro_rata(payment.amount, received.confirmed, owed)?, pro_rata(payment.fee, received.confirmed, owed)?);
        let (id, paid) = (payment.id.clone(), received.confirmed);
        if !amount.is_positive() {
            // Dust worth less than a nanodollar of credit.
            return Ok(());
        }
        if app.db.call(move |conn| store::complete_bitcoin_payment(conn, &id, now, amount, fee, paid)).await? {
            let log = serde_json::json!({
                "id": payment.id, "method": "bitcoin", "address": address, "sats": owed, "paid": paid,
                "credits": amount.to_f64(), "asked": payment.amount.to_f64(),
            });
            crate::log::info("payments.completed", log.clone());
            if paid != owed {
                crate::log::info(if paid > owed { "payments.bitcoin.overpaid" } else { "payments.bitcoin.underpaid" }, log);
            }
        }
        return Ok(());
    }
    if payment.status == "pending" && past_due && (received.seen == 0 || now > payment.expires_at + BITCOIN_GRACE) {
        let id = payment.id.clone();
        app.db.call(move |conn| store::expire_payment(conn, &id)).await?;
    }
    Ok(())
}

/// `money` scaled by `paid` sats of `owed`, rounded down to the nanodollar.
fn pro_rata(money: Money, paid: u64, owed: u64) -> Result<Money> {
    let scaled = i128::from(money.nanos()) * i128::from(paid) / i128::from(owed.max(1));
    Ok(Money(i64::try_from(scaled).context("paid far too much to count")?))
}

/// Whether an address is worth asking about again: only when what it says
/// could have changed, so addresses no one pays cost next to nothing.
///
/// - Never checked: yes.
/// - Sent something that isn't yet deep enough: only once there's a new
///   block, which is all that can change that. (With no confirmations
///   needed, the mempool counts, so it's checked as an open payment is.)
/// - Open and sent nothing yet: every poll, so it's credited promptly.
/// - Past its time and sent nothing: less and less often, as a late payment
///   grows less likely: every 5 minutes for an hour, hourly for a day, then
///   every 6 hours until [`BITCOIN_GRACE`] is up.
fn bitcoin_check_due(payment: &Payment, watch: &store::BitcoinWatch, now: i64, tip: u64, confirmations: u32) -> bool {
    let Some(checked_at) = watch.checked_at else { return true };
    if watch.seen > 0 && confirmations > 0 {
        return watch.checked_height != Some(tip);
    }
    if payment.status == "pending" && now <= payment.expires_at {
        return true;
    }
    let late = now - payment.expires_at;
    let every = if late < 60 * 60 {
        5 * 60
    } else if late < 24 * 60 * 60 {
        60 * 60
    } else {
        6 * 60 * 60
    };
    now - checked_at >= every
}

/// Look in on every bitcoin payment that's waiting for money and might have
/// news.
pub async fn check_bitcoin_payments(app: &App) -> Result<()> {
    let Some(config) = &app.config.payments.bitcoin else { return Ok(()) };
    let watching = app.db.call(|conn| store::bitcoin_to_watch(conn, crate::util::now(), BITCOIN_GRACE)).await?;
    if watching.is_empty() {
        return Ok(());
    }
    let tip = bitcoin::tip_height(&app.http, &config.esplora()).await.context("couldn't get the block height")?;
    let now = crate::util::now();
    let due = watching.into_iter().filter(|(payment, watch)| bitcoin_check_due(payment, watch, now, tip, config.confirmations));
    futures_util::stream::iter(due)
        .for_each_concurrent(CONCURRENT_CHECKS, |(payment, watch)| async move {
            if let Err(error) = check_bitcoin(app, config, &payment, &watch, tip).await {
                crate::log::warn("payments.bitcoin", serde_json::json!({ "id": payment.id, "error": format!("{error:#}") }));
            }
        })
        .await;
    Ok(())
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
        futures_util::stream::iter(pending)
            .for_each_concurrent(CONCURRENT_CHECKS, |payment| {
                let app = &app;
                async move {
                    if let Err(error) = check_lightning(app, &payment).await {
                        crate::log::warn("payments.lightning", serde_json::json!({ "id": payment.id, "error": format!("{error:#}") }));
                    }
                }
            })
            .await;
        if let Err(error) = check_bitcoin_payments(&app).await {
            crate::log::warn("payments.bitcoin", serde_json::json!({ "error": format!("{error:#}") }));
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

    #[test]
    fn credits_what_was_paid() {
        let ten = Money::from_dollars(Decimal::from(10)).unwrap();
        assert_eq!(pro_rata(ten, 11_000, 11_000).unwrap(), ten);
        assert_eq!(pro_rata(ten, 5_500, 11_000).unwrap(), Money(5_000_000_000));
        assert_eq!(pro_rata(ten, 22_000, 11_000).unwrap(), Money(20_000_000_000));
        // Rounded down.
        assert_eq!(pro_rata(Money(10), 1, 3).unwrap(), Money(3));
    }

    #[test]
    fn checks_addresses_only_when_they_could_have_news() {
        let payment = |status: &str| Payment {
            id: "pi_1".into(),
            status: status.into(),
            amount: Money(1),
            fee: Money(0),
            total: Money(1),
            method: Method::Bitcoin,
            description: String::new(),
            url: String::new(),
            created_at: 0,
            completed_at: None,
            expires_at: 1000,
            pubkey: String::new(),
            provider_ref: None,
        };
        let checked = |at: i64, seen: u64| store::BitcoinWatch { sats: 100, seen, checked_at: Some(at), checked_height: Some(10) };
        let (open, expired) = (payment("pending"), payment("expired"));

        assert!(bitcoin_check_due(&open, &store::BitcoinWatch::default(), 500, 10, 1));
        // Open and unpaid: every poll.
        assert!(bitcoin_check_due(&open, &checked(499, 0), 500, 10, 1));
        // Sent something: only once there's a new block, unless the mempool counts.
        assert!(!bitcoin_check_due(&open, &checked(499, 50), 500, 10, 1));
        assert!(bitcoin_check_due(&open, &checked(499, 50), 500, 11, 1));
        assert!(bitcoin_check_due(&open, &checked(499, 50), 500, 10, 0));
        // Past its time and unpaid: less and less often.
        assert!(!bitcoin_check_due(&expired, &checked(1100, 0), 1200, 10, 1));
        assert!(bitcoin_check_due(&expired, &checked(1100, 0), 1400, 10, 1));
        assert!(!bitcoin_check_due(&expired, &checked(5000, 0), 7000, 10, 1));
        assert!(bitcoin_check_due(&expired, &checked(5000, 0), 8600, 10, 1));
        assert!(!bitcoin_check_due(&expired, &checked(100_000, 0), 110_000, 10, 1));
        assert!(bitcoin_check_due(&expired, &checked(100_000, 0), 121_600, 10, 1));
    }
}
