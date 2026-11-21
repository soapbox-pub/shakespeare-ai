//! The config file, in YAML.
//!
//! Providers are upstream OpenAI-compatible APIs; models are what clients ask
//! for by name, each mapped to `provider/upstream-model`. Secrets are never
//! written into the file: any secret field may be `${ENV_VAR}`, read from the
//! environment at startup.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use indexmap::IndexMap;
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer};

use crate::money::{deserialize_decimal, Money};
use crate::payments::bitcoin::Wallet;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Where to serve Prometheus metrics, apart from clients. Unset, they
    /// aren't served.
    #[serde(default)]
    pub metrics_listen: Option<SocketAddr>,
    /// The header a reverse proxy puts the client's IP in, such as
    /// `x-real-ip` or `x-forwarded-for` (whose last address is the one the
    /// proxy saw). Unset, it's the address that connected. Rate limits and
    /// logs go by it.
    #[serde(default, deserialize_with = "header_name")]
    pub real_ip_header: Option<axum::http::HeaderName>,
    /// The public URL clients reach this service at; NIP-98 events are
    /// signed for it.
    pub origin: url::Url,
    pub database: PathBuf,
    #[serde(default)]
    pub payments: Payments,
    pub providers: IndexMap<String, Provider>,
    pub models: IndexMap<String, Model>,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 8000))
}

fn header_name<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<axum::http::HeaderName>, D::Error> {
    let raw = Option::<String>::deserialize(deserializer)?;
    raw.filter(|name| !name.trim().is_empty())
        .map(|name| axum::http::HeaderName::from_bytes(name.trim().to_ascii_lowercase().as_bytes()).map_err(|_| serde::de::Error::custom(format!("{name:?} isn't a header name"))))
        .transpose()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payments {
    /// Charged on top of the credits bought: 0.1 is 10%.
    #[serde(default = "default_fee", deserialize_with = "deserialize_decimal")]
    pub fee: Decimal,
    /// The fewest dollars of credit one payment or gift card may be for.
    #[serde(default = "default_minimum", deserialize_with = "deserialize_decimal")]
    pub minimum: Decimal,
    #[serde(default)]
    pub stripe: Option<Stripe>,
    #[serde(default)]
    pub lightning: Option<Lightning>,
    #[serde(default)]
    pub bitcoin: Option<Bitcoin>,
}

impl Default for Payments {
    fn default() -> Self {
        Payments { fee: default_fee(), minimum: default_minimum(), stripe: None, lightning: None, bitcoin: None }
    }
}

fn default_fee() -> Decimal {
    Decimal::new(1, 1)
}

fn default_minimum() -> Decimal {
    Decimal::from(5)
}

impl Payments {
    pub fn minimum(&self) -> Money {
        Money::from_dollars(self.minimum).unwrap_or(Money::ZERO)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stripe {
    #[serde(deserialize_with = "secret")]
    pub secret_key: String,
    #[serde(deserialize_with = "secret")]
    pub webhook_secret: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lightning {
    /// A Lightning address (LUD-16) whose LNURL-pay server returns a LUD-21
    /// `verify` URL with each invoice, which is how payment is confirmed.
    pub address: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bitcoin {
    /// A native segwit (BIP84) account's extended public key, `zpub` or
    /// `xpub` (`vpub` or `tpub` on testnet). Each payment gets the next
    /// receive address.
    #[serde(deserialize_with = "wallet")]
    pub xpub: Wallet,
    /// An Esplora API root. mempool.space's, for a mainnet key, unless given;
    /// a testnet key must say which testnet.
    #[serde(default)]
    esplora: Option<url::Url>,
    /// How deep a payment's transaction must be before it's credited. Zero
    /// credits it from the mempool, where it can still be double-spent.
    #[serde(default = "default_confirmations")]
    pub confirmations: u32,
}

fn default_confirmations() -> u32 {
    1
}

impl Bitcoin {
    pub fn esplora(&self) -> url::Url {
        self.esplora.clone().unwrap_or_else(|| url::Url::parse("https://mempool.space/api").expect("a URL"))
    }
}

fn wallet<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Wallet, D::Error> {
    let raw = String::deserialize(deserializer)?;
    Wallet::parse(&raw).map_err(|e| serde::de::Error::custom(format!("{e:#}")))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    /// An OpenAI-compatible API root, such as `https://api.openai.com/v1`.
    pub base_url: url::Url,
    #[serde(deserialize_with = "secret")]
    pub api_key: String,
    /// OpenRouter extras: per-request usage accounting, and Anthropic prompt
    /// caching breakpoints for `anthropic/` models.
    #[serde(default)]
    pub openrouter: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    /// `provider/model`: everything after the first slash is the provider's
    /// own model id, slashes and all.
    pub upstream: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: String,
    /// Kept out of `/v1/models`, but still callable by id. Free chat models
    /// are always hidden.
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Shown in `/v1/models` as `created`.
    #[serde(default)]
    pub created: Option<i64>,
    pub price: Price,
}

/// How a model is priced, which is also what kind of model it is.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Price {
    /// USD per million tokens.
    Tokens(TokenPrice),
    /// USD per image, by quality and shape.
    Image(ImagePrice),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenPrice {
    #[serde(deserialize_with = "deserialize_decimal")]
    pub input: Decimal,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub output: Decimal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagePrice {
    pub low: ImageSizes,
    pub medium: ImageSizes,
    pub high: ImageSizes,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageSizes {
    /// 1024x1024.
    #[serde(deserialize_with = "deserialize_decimal")]
    pub square: Decimal,
    /// 1024x1536 or 1536x1024.
    #[serde(deserialize_with = "deserialize_decimal")]
    pub wide: Decimal,
}

impl Model {
    /// The provider's name and the model's id there.
    pub fn upstream(&self) -> (&str, &str) {
        self.upstream.split_once('/').unwrap_or((&self.upstream, ""))
    }

    pub fn is_free_chat(&self) -> bool {
        matches!(&self.price, Price::Tokens(p) if p.input.is_zero() && p.output.is_zero())
    }
}

impl TokenPrice {
    /// What `input` and `output` tokens cost.
    pub fn cost(&self, input: u64, output: u64) -> Money {
        let million = Decimal::from(1_000_000);
        let dollars = self.input * Decimal::from(input) / million + self.output * Decimal::from(output) / million;
        Money::from_dollars(dollars).unwrap_or(Money(i64::MAX))
    }
}

/// A secret: given literally, or as `${VAR}` to read it from the environment.
fn secret<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let raw = String::deserialize(deserializer)?;
    resolve_secret(&raw).map_err(serde::de::Error::custom)
}

fn resolve_secret(raw: &str) -> Result<String, String> {
    let Some(name) = raw.strip_prefix("${").and_then(|r| r.strip_suffix('}')) else {
        return Ok(raw.to_string());
    };
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("environment variable {name} is not set")),
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut config = Config::parse(&text).with_context(|| format!("in {}", path.display()))?;
        // A relative database path is relative to the config file, not to
        // wherever the service happened to be started.
        if config.database.is_relative()
            && let Some(dir) = path.parent()
        {
            config.database = dir.join(&config.database);
        }
        Ok(config)
    }

    pub fn parse(text: &str) -> Result<Config> {
        let config: Config = serde_saphyr::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.origin.host_str().is_none() {
            bail!("origin must be a URL with a host");
        }
        if self.payments.fee < Decimal::ZERO || self.payments.fee > Decimal::ONE {
            bail!("payments.fee must be between 0 and 1 (0.1 is 10%)");
        }
        if self.payments.minimum <= Decimal::ZERO {
            bail!("payments.minimum must be more than 0");
        }
        if let Some(lightning) = &self.payments.lightning
            && lightning_address_url(&lightning.address).is_none()
        {
            bail!("payments.lightning.address must look like name@domain");
        }
        if let Some(bitcoin) = &self.payments.bitcoin
            && bitcoin.esplora.is_none()
            && bitcoin.xpub.network() != ::bitcoin::Network::Bitcoin
        {
            bail!("payments.bitcoin.esplora must be set for a testnet key, such as https://mempool.space/testnet4/api");
        }
        for (id, model) in &self.models {
            let (provider, upstream) = model.upstream();
            if upstream.is_empty() {
                bail!("model {id}: upstream must be provider/model, like openrouter/anthropic/claude-sonnet-4.6");
            }
            if !self.providers.contains_key(provider) {
                bail!("model {id}: no provider named {provider}");
            }
            match &model.price {
                Price::Tokens(price) => {
                    if model.context_window.is_none() {
                        bail!("model {id}: chat models need a context_window");
                    }
                    if price.input < Decimal::ZERO || price.output < Decimal::ZERO {
                        bail!("model {id}: prices can't be negative");
                    }
                }
                Price::Image(_) => {}
            }
        }
        Ok(())
    }
}

/// Where a Lightning address's LNURL-pay metadata lives (LUD-16).
pub fn lightning_address_url(address: &str) -> Option<url::Url> {
    let (name, domain) = address.trim().split_once('@')?;
    if name.is_empty() || domain.is_empty() || domain.contains('/') {
        return None;
    }
    url::Url::from_str(&format!("https://{domain}/.well-known/lnurlp/{name}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const EXAMPLE: &str = include_str!("../config.example.yaml");

    #[test]
    fn parses_the_example() {
        // SAFETY: tests that read these variables only ever set them to the
        // same values.
        unsafe {
            for var in ["ANTHROPIC_API_KEY", "OPENROUTER_API_KEY", "OPENAI_API_KEY", "STRIPE_SECRET_KEY", "STRIPE_WEBHOOK_SECRET"] {
                std::env::set_var(var, "x");
            }
        }
        let config = Config::parse(EXAMPLE).unwrap();
        let sonnet = &config.models["claude-sonnet-4.6"];
        assert_eq!(sonnet.upstream(), ("anthropic", "claude-sonnet-4-6"));
        let Price::Tokens(price) = &sonnet.price else { panic!() };
        assert_eq!(price.cost(1_000_000, 0), Money(3_000_000_000));
        assert_eq!(price.cost(1, 1), Money(18_000));
        assert!(matches!(config.models["gpt-image-1"].price, Price::Image(_)));
        let gemma = &config.models["gemma-4-26b"];
        assert_eq!(gemma.upstream(), ("openrouter", "google/gemma-4-26b-a4b-it"));
        assert!(gemma.is_free_chat());
        assert_eq!(config.payments.bitcoin.unwrap().xpub.address(0).unwrap(), crate::payments::bitcoin::tests::FIRST);
    }

    #[test]
    fn rejects_mistakes() {
        let base = "origin: https://ai.example.com\ndatabase: x.db\nproviders:\n  p:\n    base_url: https://p.example.com/v1\n    api_key: k\n";
        let err = |models: &str| Config::parse(&format!("{base}models:\n{models}")).unwrap_err().to_string();
        assert!(err("  m:\n    upstream: q/x\n    context_window: 1\n    price: {input: 1, output: 1}\n").contains("no provider named q"));
        assert!(err("  m:\n    upstream: p\n    context_window: 1\n    price: {input: 1, output: 1}\n").contains("provider/model"));
        assert!(err("  m:\n    upstream: p/x\n    price: {input: 1, output: 1}\n").contains("context_window"));
        assert!(Config::parse(&format!("{base}models: {{}}\ntypo: 1\n")).is_err());

        let bitcoin = |section: &str| Config::parse(&format!("{base}models: {{}}\npayments:\n  bitcoin:\n{section}"));
        assert!(bitcoin("    xpub: nope\n").unwrap_err().to_string().contains("extended public key"));
        let zpub = crate::payments::bitcoin::tests::ZPUB;
        let config = bitcoin(&format!("    xpub: {zpub}\n")).unwrap();
        let b = config.payments.bitcoin.unwrap();
        assert_eq!((b.esplora().as_str(), b.confirmations), ("https://mempool.space/api", 1));
        // A testnet key must say which testnet.
        let mut tpub = ::bitcoin::base58::decode_check(zpub).unwrap();
        tpub[..4].copy_from_slice(&[0x04, 0x35, 0x87, 0xcf]);
        let tpub = ::bitcoin::base58::encode_check(&tpub);
        assert!(bitcoin(&format!("    xpub: {tpub}\n")).unwrap_err().to_string().contains("esplora"));
    }

    #[test]
    fn resolves_lightning_addresses() {
        assert_eq!(lightning_address_url("alex@coinos.io").unwrap().as_str(), "https://coinos.io/.well-known/lnurlp/alex");
        assert!(lightning_address_url("nope").is_none());
    }
}
