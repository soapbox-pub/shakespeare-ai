//! Exchange rates in dollars: bitcoin's from Coinbase, else CoinGecko;
//! monero's from Kraken, else CoinGecko. Each is cached for five minutes, and
//! stale rather than missing when both its sources are down, but not for
//! long: an invoice priced at last week's rate could be for far too little.

use std::future::Future;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rust_decimal::Decimal;

const FRESH_FOR: Duration = Duration::from_secs(5 * 60);
/// How old a rate may be and still be used when neither source answers.
const USABLE_FOR: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub struct Rates {
    btc_usd: Mutex<Option<(Decimal, Instant)>>,
    xmr_usd: Mutex<Option<(Decimal, Instant)>>,
}

impl Rates {
    /// Dollars per bitcoin.
    pub async fn btc_usd(&self, http: &reqwest::Client) -> Result<Decimal> {
        cached(&self.btc_usd, fetch_btc(http)).await
    }

    /// Dollars per monero.
    pub async fn xmr_usd(&self, http: &reqwest::Client) -> Result<Decimal> {
        cached(&self.xmr_usd, fetch_xmr(http)).await
    }
}

async fn cached(cache: &Mutex<Option<(Decimal, Instant)>>, fetch: impl Future<Output = Result<Decimal>>) -> Result<Decimal> {
    let held = *cache.lock();
    if let Some((rate, at)) = held
        && at.elapsed() < FRESH_FOR
    {
        return Ok(rate);
    }
    match fetch.await {
        Ok(rate) => {
            *cache.lock() = Some((rate, Instant::now()));
            Ok(rate)
        }
        Err(error) => held.filter(|(_, at)| at.elapsed() < USABLE_FOR).map(|(rate, _)| rate).ok_or(error),
    }
}

#[cfg(test)]
impl Rates {
    /// Fix the bitcoin rate, so tests never ask the internet.
    pub fn set(&self, rate: Decimal) {
        *self.btc_usd.lock() = Some((rate, Instant::now()));
    }

    /// Fix the monero rate.
    pub fn set_xmr(&self, rate: Decimal) {
        *self.xmr_usd.lock() = Some((rate, Instant::now()));
    }
}

async fn get_json(http: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    Ok(http.get(url).timeout(super::TIMEOUT).send().await?.error_for_status()?.json().await?)
}

/// The first source's rate, else the second's, else both their errors.
async fn first_of(pair: &str, (first, a): (&str, impl Future<Output = Result<Decimal>>), (second, b): (&str, impl Future<Output = Result<Decimal>>)) -> Result<Decimal> {
    let error = match a.await {
        Ok(rate) if rate > Decimal::ZERO => return Ok(rate),
        Ok(_) => anyhow!("{first} gave a nonsense rate"),
        Err(error) => error,
    };
    match b.await {
        Ok(rate) if rate > Decimal::ZERO => Ok(rate),
        Ok(_) => Err(anyhow!("{second} gave a nonsense rate")),
        Err(fallback) => Err(anyhow!("couldn't get a {pair} rate: {error}; {fallback}")),
    }
}

async fn fetch_btc(http: &reqwest::Client) -> Result<Decimal> {
    let coinbase = async {
        let json = get_json(http, "https://api.coinbase.com/v2/exchange-rates?currency=BTC").await?;
        json.pointer("/data/rates/USD").and_then(|v| v.as_str()).and_then(|s| Decimal::from_str(s).ok()).ok_or(anyhow!("no USD rate from Coinbase"))
    };
    first_of("BTC/USD", ("Coinbase", coinbase), ("CoinGecko", coingecko(http, "bitcoin"))).await
}

/// Coinbase doesn't trade monero, so Kraken: the last trade's price.
async fn fetch_xmr(http: &reqwest::Client) -> Result<Decimal> {
    let kraken = async {
        let json = get_json(http, "https://api.kraken.com/0/public/Ticker?pair=XMRUSD").await?;
        kraken_last(&json).ok_or(anyhow!("no XMR/USD rate from Kraken"))
    };
    first_of("XMR/USD", ("Kraken", kraken), ("CoinGecko", coingecko(http, "monero"))).await
}

/// The last trade's price in a Kraken ticker, under whatever it names the
/// pair (`XXMRZUSD`), if it reports no errors.
fn kraken_last(json: &serde_json::Value) -> Option<Decimal> {
    if json.get("error").and_then(|e| e.as_array()).is_some_and(|e| !e.is_empty()) {
        return None;
    }
    let ticker = json.get("result")?.as_object()?.values().next()?;
    Decimal::from_str(ticker.pointer("/c/0")?.as_str()?).ok()
}

async fn coingecko(http: &reqwest::Client, id: &str) -> Result<Decimal> {
    let json = get_json(http, &format!("https://api.coingecko.com/api/v3/simple/price?ids={id}&vs_currencies=usd")).await?;
    json.pointer(&format!("/{id}/usd")).and_then(crate::money::decimal_from_json_number).ok_or(anyhow!("no USD rate from CoinGecko"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_krakens_ticker() {
        let json = serde_json::json!({ "error": [], "result": { "XXMRZUSD": { "a": ["511.45", "2", "2.000"], "c": ["511.01000000", "0.016"] } } });
        assert_eq!(kraken_last(&json), Some(Decimal::from_str("511.01").unwrap()));
        assert_eq!(kraken_last(&serde_json::json!({ "error": ["EQuery:Unknown asset pair"] })), None);
        assert_eq!(kraken_last(&serde_json::json!({ "error": [], "result": {} })), None);
    }
}
