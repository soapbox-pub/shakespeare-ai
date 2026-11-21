//! Lightning invoices from a Lightning address, confirmed by LUD-21.
//!
//! The address (LUD-16) points at an LNURL-pay endpoint (LUD-06), which makes
//! an invoice for an amount and, with LUD-21, a `verify` URL that says whether
//! it's been paid. No node, and nothing particular to one provider.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayRequest {
    callback: String,
    min_sendable: u64,
    max_sendable: u64,
    #[serde(default)]
    comment_allowed: usize,
}

#[derive(Deserialize)]
struct InvoiceResponse {
    pr: Option<String>,
    verify: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct VerifyResponse {
    status: Option<String>,
    #[serde(default)]
    settled: bool,
    reason: Option<String>,
}

#[derive(Debug)]
pub struct Invoice {
    pub bolt11: String,
    pub verify: String,
}

/// Ask a Lightning address for an invoice of `msats`.
pub async fn request_invoice(http: &reqwest::Client, address: &str, msats: u64, comment: &str) -> Result<Invoice> {
    let url = crate::config::lightning_address_url(address).ok_or(anyhow!("invalid Lightning address"))?;
    request_invoice_from(http, url, msats, comment).await
}

/// Ask an LNURL-pay endpoint for an invoice of `msats`.
async fn request_invoice_from(http: &reqwest::Client, url: url::Url, msats: u64, comment: &str) -> Result<Invoice> {
    let pay: PayRequest =
        http.get(url).timeout(super::TIMEOUT).send().await?.error_for_status()?.json().await.context("the Lightning address didn't return LNURL-pay metadata")?;
    if msats < pay.min_sendable || msats > pay.max_sendable {
        bail!("the Lightning address accepts {} to {} sats; this invoice would be {}", pay.min_sendable / 1000, pay.max_sendable / 1000, msats / 1000);
    }

    let mut callback = url::Url::parse(&pay.callback).context("the LNURL-pay callback isn't a URL")?;
    callback.query_pairs_mut().append_pair("amount", &msats.to_string());
    // Some servers refuse any comment outright when they allow none.
    if pay.comment_allowed > 0 {
        let comment: String = comment.chars().take(pay.comment_allowed).collect();
        callback.query_pairs_mut().append_pair("comment", &comment);
    }

    let invoice: InvoiceResponse = http.get(callback).timeout(super::TIMEOUT).send().await?.json().await.context("the LNURL-pay callback didn't return an invoice")?;
    let Some(bolt11) = invoice.pr.filter(|pr| !pr.is_empty()) else {
        bail!("no invoice: {}", invoice.reason.unwrap_or_else(|| "no reason given".into()));
    };
    // Without it we could take the money and never notice; refuse up front.
    let Some(verify) = invoice.verify.filter(|v| url::Url::parse(v).is_ok_and(|u| matches!(u.scheme(), "https" | "http"))) else {
        bail!("the Lightning address doesn't support payment verification (LUD-21)");
    };
    Ok(Invoice { bolt11, verify })
}

/// Whether the invoice behind a LUD-21 verify URL has been paid.
pub async fn is_settled(http: &reqwest::Client, verify: &str) -> Result<bool> {
    let response: VerifyResponse = http.get(verify).timeout(super::TIMEOUT).send().await?.error_for_status()?.json().await?;
    if response.status.as_deref() == Some("ERROR") {
        bail!("verify failed: {}", response.reason.unwrap_or_default());
    }
    Ok(response.settled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Query, State};
    use axum::routing::get;
    use axum::Json;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn gets_and_verifies_an_invoice() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let paid = Arc::new(AtomicBool::new(false));
        let meta = {
            let base = base.clone();
            move || async move { Json(json!({ "tag": "payRequest", "callback": format!("{base}/cb?x=1"), "minSendable": 1000, "maxSendable": 100_000_000_000u64, "commentAllowed": 5, "metadata": "[]" })) }
        };
        let callback = {
            let base = base.clone();
            move |Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q["x"], "1");
                assert_eq!(q["comment"], "Shake");
                Json(json!({ "pr": format!("lnbc{}", q["amount"]), "verify": format!("{base}/verify"), "routes": [] }))
            }
        };
        let verify = |State(paid): State<Arc<AtomicBool>>| async move {
            Json(json!({ "status": "OK", "settled": paid.load(Ordering::SeqCst), "preimage": null, "pr": "lnbc" }))
        };
        let app = axum::Router::new().route("/meta", get(meta)).route("/cb", get(callback)).route("/verify", get(verify)).with_state(paid.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let http = reqwest::Client::new();
        let url = url::Url::parse(&format!("{base}/meta")).unwrap();
        let invoice = request_invoice_from(&http, url.clone(), 21_000, "Shakespeare").await.unwrap();
        assert_eq!(invoice.bolt11, "lnbc21000");
        assert!(!is_settled(&http, &invoice.verify).await.unwrap());
        paid.store(true, Ordering::SeqCst);
        assert!(is_settled(&http, &invoice.verify).await.unwrap());

        assert!(request_invoice_from(&http, url, 1, "x").await.unwrap_err().to_string().contains("accepts"));
    }
}
