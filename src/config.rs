//! The config file, in YAML.
//!
//! Providers are upstream APIs; models are what clients ask for by name, each
//! served by a list of upstreams (`provider/upstream-model`) tried in order.
//! Secrets are never written into the file: any secret field may be
//! `${ENV_VAR}`, read from the environment at startup.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

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
    #[serde(default)]
    pub routing: Routing,
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

/// How hard to try a model's upstreams before giving up on a request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Routing {
    /// How long connecting to a provider may take.
    #[serde(default = "default_connect_timeout", deserialize_with = "duration")]
    pub connect_timeout: Duration,
    /// How long a stream's first byte may take. Non-streaming responses come
    /// whole, however long the completion takes, so only streams have it.
    #[serde(default = "default_first_byte_timeout", deserialize_with = "duration")]
    pub first_byte_timeout: Duration,
    /// Failures in a row that take an upstream out of turn for `cooldown`.
    #[serde(default = "default_failures")]
    pub failures: u32,
    #[serde(default = "default_cooldown", deserialize_with = "duration")]
    pub cooldown: Duration,
}

impl Default for Routing {
    fn default() -> Self {
        Routing {
            connect_timeout: default_connect_timeout(),
            first_byte_timeout: default_first_byte_timeout(),
            failures: default_failures(),
            cooldown: default_cooldown(),
        }
    }
}

fn default_connect_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_first_byte_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_failures() -> u32 {
    3
}

fn default_cooldown() -> Duration {
    Duration::from_secs(30)
}

/// A duration: `500ms`, `10s`, `5m` or `1h`, or a number of seconds.
fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Seconds(u64),
        Text(String),
    }
    let text = match Raw::deserialize(deserializer)? {
        Raw::Seconds(s) => return Ok(Duration::from_secs(s)),
        Raw::Text(text) => text,
    };
    parse_duration(&text).ok_or_else(|| serde::de::Error::custom(format!("{text:?} isn't a duration, like 500ms, 10s, 5m or 1h")))
}

fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = text.split_at(split);
    let n: u64 = n.parse().ok()?;
    match unit.trim() {
        "ms" => Some(Duration::from_millis(n)),
        "s" => Some(Duration::from_secs(n)),
        "m" => Some(Duration::from_secs(n.checked_mul(60)?)),
        "h" => Some(Duration::from_secs(n.checked_mul(3600)?)),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    /// The protocol it speaks.
    pub api: Api,
    /// Its API root, if not the usual one for `api`.
    #[serde(default)]
    base_url: Option<url::Url>,
    #[serde(deserialize_with = "secret")]
    pub api_key: String,
}

/// The protocol a provider speaks, which decides how requests are written
/// and how their usage is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Api {
    /// OpenAI's chat completions and image generations.
    Openai,
    /// OpenAI's, with OpenRouter's extras: its spelling of reasoning effort,
    /// reasoning blocks handed back, usage accounting (what each request
    /// cost us), and prompt caching breakpoints for `anthropic/` models.
    Openrouter,
    /// Anthropic's Messages API, with prompt caching. Chat only.
    Anthropic,
}

impl Provider {
    pub fn base_url(&self) -> url::Url {
        let default = match self.api {
            Api::Openai => "https://api.openai.com/v1",
            Api::Openrouter => "https://openrouter.ai/api/v1",
            Api::Anthropic => "https://api.anthropic.com/v1",
        };
        self.base_url.clone().unwrap_or_else(|| url::Url::parse(default).expect("a URL"))
    }
}

/// One way to serve a model: `provider/model`, everything after the first
/// slash being the provider's own id, or the long form with `params`.
#[derive(Debug)]
pub struct Upstream {
    pub provider: String,
    pub model: String,
    /// Merged into the top level of each request body sent this way.
    pub params: serde_json::Map<String, serde_json::Value>,
}

impl Upstream {
    /// `provider/model`, as it's written in the short form.
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

impl<'de> Deserialize<'de> for Upstream {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Long {
            provider: String,
            model: String,
            #[serde(default)]
            params: serde_json::Map<String, serde_json::Value>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Short(String),
            Long(Long),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Short(text) => match text.split_once('/') {
                Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
                    Ok(Upstream { provider: provider.into(), model: model.into(), params: Default::default() })
                }
                _ => Err(serde::de::Error::custom(format!("upstream {text:?} must be provider/model, like openrouter/anthropic/claude-sonnet-4.6"))),
            },
            Raw::Long(long) => Ok(Upstream { provider: long.provider, model: long.model, params: long.params }),
        }
    }
}

/// Request fields that `params` can't set: they're the request itself.
const RESERVED_PARAMS: &[&str] = &["model", "messages", "prompt", "n", "stream", "stream_options"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    /// Tried in order, skipping any that's cooling down after failures.
    pub upstreams: Vec<Upstream>,
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

/// USD per million tokens. Cached input is priced on its own: reading it
/// back, and writing it (where providers charge extra for that). Either
/// defaults to `input`, so a missing price never undercharges.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenPrice {
    #[serde(deserialize_with = "deserialize_decimal")]
    pub input: Decimal,
    #[serde(deserialize_with = "deserialize_decimal")]
    pub output: Decimal,
    #[serde(default, deserialize_with = "optional_decimal")]
    cache_read: Option<Decimal>,
    #[serde(default, deserialize_with = "optional_decimal")]
    cache_write: Option<Decimal>,
}

fn optional_decimal<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Decimal>, D::Error> {
    deserialize_decimal(deserializer).map(Some)
}

/// What one completion used, in tokens. `input` is all of it, cached or not,
/// as OpenAI counts `prompt_tokens`; `output` includes reasoning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
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
    pub fn is_free_chat(&self) -> bool {
        matches!(&self.price, Price::Tokens(p) if p.is_free())
    }
}

impl TokenPrice {
    pub fn cache_read(&self) -> Decimal {
        self.cache_read.unwrap_or(self.input)
    }

    pub fn cache_write(&self) -> Decimal {
        self.cache_write.unwrap_or(self.input)
    }

    pub fn is_free(&self) -> bool {
        [self.input, self.output, self.cache_read(), self.cache_write()].iter().all(Decimal::is_zero)
    }

    /// What `tokens` cost. Cached input is part of `input`, so it's taken
    /// out of what's charged at the full price.
    pub fn cost(&self, tokens: Tokens) -> Money {
        let uncached = tokens.input.saturating_sub(tokens.cache_read).saturating_sub(tokens.cache_write);
        let million = Decimal::from(1_000_000);
        let dollars = [(self.input, uncached), (self.cache_read(), tokens.cache_read), (self.cache_write(), tokens.cache_write), (self.output, tokens.output)]
            .into_iter()
            .map(|(price, n)| price * Decimal::from(n))
            .sum::<Decimal>()
            / million;
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
        if self.routing.failures == 0 {
            bail!("routing.failures must be at least 1");
        }
        for (id, model) in &self.models {
            if model.upstreams.is_empty() {
                bail!("model {id}: upstreams can't be empty");
            }
            for upstream in &model.upstreams {
                let Some(provider) = self.providers.get(&upstream.provider) else {
                    bail!("model {id}: no provider named {}", upstream.provider);
                };
                if let Some(key) = upstream.params.keys().find(|k| RESERVED_PARAMS.contains(&k.as_str())) {
                    bail!("model {id}: params can't set {key}");
                }
                if matches!(model.price, Price::Image(_)) && provider.api != Api::Openai {
                    bail!("model {id}: image models need providers with api: openai, and {} isn't one", upstream.provider);
                }
            }
            match &model.price {
                Price::Tokens(price) => {
                    if model.context_window.is_none() {
                        bail!("model {id}: chat models need a context_window");
                    }
                    if [price.input, price.output, price.cache_read(), price.cache_write()].iter().any(|p| *p < Decimal::ZERO) {
                        bail!("model {id}: prices can't be negative");
                    }
                    if price.cache_read() > price.input {
                        bail!("model {id}: cache_read costs more than input, which is surely a mistake");
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
        let labels: Vec<String> = sonnet.upstreams.iter().map(Upstream::label).collect();
        assert_eq!(labels, ["anthropic/claude-sonnet-4-6", "openrouter/anthropic/claude-sonnet-4.6"]);
        let Price::Tokens(price) = &sonnet.price else { panic!() };
        assert_eq!(price.cost(Tokens { input: 1_000_000, ..Tokens::default() }), Money(3_000_000_000));
        assert_eq!(price.cost(Tokens { input: 1, output: 1, ..Tokens::default() }), Money(18_000));
        // A million in: half read from the cache, a quarter written to it.
        let cached = Tokens { input: 1_000_000, cache_read: 500_000, cache_write: 250_000, output: 0 };
        assert_eq!(price.cost(cached), Money(750_000_000 + 150_000_000 + 937_500_000));
        assert!(matches!(config.models["gpt-image-1"].price, Price::Image(_)));
        let gemma = &config.models["gemma-4-26b"];
        assert_eq!(gemma.upstreams[0].label(), "openrouter/google/gemma-4-26b-a4b-it");
        assert!(gemma.is_free_chat());
        assert_eq!(config.providers["openrouter"].base_url().as_str(), "https://openrouter.ai/api/v1");
        assert_eq!(config.routing.first_byte_timeout, Duration::from_secs(60));
        assert_eq!(config.payments.bitcoin.unwrap().xpub.address(0).unwrap(), crate::payments::bitcoin::tests::FIRST);
    }

    #[test]
    fn rejects_mistakes() {
        let base = "origin: https://ai.example.com\ndatabase: x.db\nproviders:\n  p:\n    api: openai\n    base_url: https://p.example.com/v1\n    api_key: k\n  r:\n    api: openrouter\n    api_key: k\n";
        let err = |models: &str| Config::parse(&format!("{base}models:\n{models}")).unwrap_err().to_string();
        let chat = "    context_window: 1\n    price: {input: 1, output: 1}\n";
        assert!(err(&format!("  m:\n    upstreams: [q/x]\n{chat}")).contains("no provider named q"));
        assert!(err(&format!("  m:\n    upstreams: [p]\n{chat}")).contains("provider/model"));
        assert!(err(&format!("  m:\n    upstreams: []\n{chat}")).contains("can't be empty"));
        assert!(err("  m:\n    upstreams: [p/x]\n    price: {input: 1, output: 1}\n").contains("context_window"));
        assert!(err(&format!("  m:\n    upstreams: [{{provider: p, model: x, params: {{stream: false}}}}]\n{chat}")).contains("can't set stream"));
        assert!(err("  m:\n    upstreams: [p/x]\n    context_window: 1\n    price: {input: 1, output: 1, cache_read: 2}\n").contains("cache_read"));
        let image = "    price: {low: {square: 1, wide: 1}, medium: {square: 1, wide: 1}, high: {square: 1, wide: 1}}\n";
        assert!(err(&format!("  m:\n    upstreams: [r/x]\n{image}")).contains("api: openai"));
        assert!(Config::parse(&format!("{base}models: {{}}\ntypo: 1\n")).is_err());
        assert!(Config::parse(&format!("{base}models: {{}}\nrouting: {{cooldown: soon}}\n")).unwrap_err().to_string().contains("duration"));

        let config = Config::parse(&format!("{base}models:\n  m:\n    upstreams: [{{provider: r, model: a/b, params: {{provider: {{order: [x]}}}}}}, p/c]\n{chat}routing: {{cooldown: 2m, connect_timeout: 5}}\n")).unwrap();
        let m = &config.models["m"];
        assert_eq!((m.upstreams[0].label(), m.upstreams[1].label()), ("r/a/b".into(), "p/c".into()));
        assert_eq!(m.upstreams[0].params["provider"]["order"][0], "x");
        assert_eq!((config.routing.cooldown, config.routing.connect_timeout), (Duration::from_secs(120), Duration::from_secs(5)));
        let Price::Tokens(price) = &m.price else { panic!() };
        assert_eq!((price.cache_read(), price.cache_write()), (Decimal::ONE, Decimal::ONE));

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
