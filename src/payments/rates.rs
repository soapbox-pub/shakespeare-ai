//! The BTC/USD exchange rate, from Coinbase, else CoinGecko, cached for five
//! minutes, and stale rather than missing when both are down.

use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rust_decimal::Decimal;

const FRESH_FOR: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
pub struct Rates {
    btc_usd: Mutex<Option<(Decimal, Instant)>>,
}

impl Rates {
    /// Dollars per bitcoin.
    pub async fn btc_usd(&self, http: &reqwest::Client) -> Result<Decimal> {
        let cached = *self.btc_usd.lock();
        if let Some((rate, at)) = cached
            && at.elapsed() < FRESH_FOR
        {
            return Ok(rate);
        }
        match fetch(http).await {
            Ok(rate) => {
                *self.btc_usd.lock() = Some((rate, Instant::now()));
                Ok(rate)
            }
            Err(error) => cached.map(|(rate, _)| rate).ok_or(error),
        }
    }
}

async fn fetch(http: &reqwest::Client) -> Result<Decimal> {
    let coinbase = async {
        let json: serde_json::Value =
            http.get("https://api.coinbase.com/v2/exchange-rates?currency=BTC").send().await?.error_for_status()?.json().await?;
        json.pointer("/data/rates/USD").and_then(|v| v.as_str()).and_then(|s| Decimal::from_str(s).ok()).ok_or(anyhow!("no USD rate from Coinbase"))
    };
    let error = match coinbase.await {
        Ok(rate) if rate > Decimal::ZERO => return Ok(rate),
        Ok(_) => anyhow!("Coinbase gave a nonsense rate"),
        Err(error) => error,
    };
    let coingecko = async {
        let json: serde_json::Value = http
            .get("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        json.pointer("/bitcoin/usd").and_then(crate::money::decimal_from_json_number).ok_or(anyhow!("no USD rate from CoinGecko"))
    };
    match coingecko.await {
        Ok(rate) if rate > Decimal::ZERO => Ok(rate),
        Ok(_) => Err(anyhow!("CoinGecko gave a nonsense rate")),
        Err(fallback) => Err(anyhow!("couldn't get a BTC/USD rate: {error}; {fallback}")),
    }
}
