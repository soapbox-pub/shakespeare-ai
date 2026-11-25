//! Onchain bitcoin: a fresh address for each payment, derived from a
//! watch-only account key, and an Esplora API to see what's arrived there.
//!
//! The service never holds a private key. Addresses are native segwit
//! (P2WPKH, BIP84), on the account's receive chain (`…/0/i`), so the wallet
//! the key came from sees the money arrive. Esplora is the API mempool.space
//! and blockstream.info serve; by default we ask our own, btc.soapbox.pub,
//! whose answers we trust and which doesn't rate-limit us.

use std::fmt;

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::bip32::{ChildNumber, Xpub};
use bitcoin::secp256k1::{Secp256k1, VerifyOnly};
use bitcoin::{Address, Network, NetworkKind};
use rust_decimal::Decimal;
use serde::Deserialize;

/// SLIP-132 version bytes: the `zpub`/`vpub` spellings of a BIP84 account
/// key, and the `ypub`/`upub` of a BIP49 one, which we don't do.
const XPUB: [u8; 4] = [0x04, 0x88, 0xb2, 0x1e];
const TPUB: [u8; 4] = [0x04, 0x35, 0x87, 0xcf];
const ZPUB: [u8; 4] = [0x04, 0xb2, 0x47, 0x46];
const VPUB: [u8; 4] = [0x04, 0x5f, 0x1c, 0xf6];
const YPUB: [u8; 4] = [0x04, 0x9d, 0x7c, 0xb2];
const UPUB: [u8; 4] = [0x04, 0x4a, 0x52, 0x62];

/// A watch-only BIP84 account, to derive receive addresses from.
#[derive(Clone)]
pub struct Wallet {
    xpub: Xpub,
    secp: Secp256k1<VerifyOnly>,
}

impl Wallet {
    /// Parse an account's extended public key: `zpub` or `xpub` on mainnet,
    /// `vpub` or `tpub` on testnet and signet.
    pub fn parse(key: &str) -> Result<Wallet> {
        let mut data = bitcoin::base58::decode_check(key.trim()).map_err(|_| anyhow!("not an extended public key"))?;
        if data.len() != 78 {
            bail!("not an extended public key");
        }
        let version: [u8; 4] = data[..4].try_into().expect("78 bytes");
        match version {
            XPUB | TPUB => {}
            ZPUB => data[..4].copy_from_slice(&XPUB),
            VPUB => data[..4].copy_from_slice(&TPUB),
            YPUB | UPUB => bail!("that's a P2SH-wrapped segwit (BIP49) key; use a native segwit (BIP84) account's zpub"),
            _ => bail!("not an extended public key (a private key, or an unknown kind?)"),
        }
        let xpub = Xpub::decode(&data).context("not an extended public key")?;
        Ok(Wallet { xpub, secp: Secp256k1::verification_only() })
    }

    pub fn network(&self) -> Network {
        match self.xpub.network {
            NetworkKind::Main => Network::Bitcoin,
            // tb1 addresses, which testnet and signet share.
            NetworkKind::Test => Network::Testnet,
        }
    }

    /// The key, spelled as an `xpub` or `tpub` however it was written, so
    /// address indexes are counted per account.
    pub fn id(&self) -> String {
        self.xpub.to_string()
    }

    /// The receive address at `index`.
    pub fn address(&self, index: u32) -> Result<String> {
        let path = [ChildNumber::from_normal_idx(0)?, ChildNumber::from_normal_idx(index)?];
        let key = self.xpub.derive_pub(&self.secp, &path)?;
        Ok(Address::p2wpkh(&key.to_pub(), self.network()).to_string())
    }
}

impl fmt::Debug for Wallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Wallet").field(&self.xpub.to_string()).finish()
    }
}

/// A BIP21 URI for paying `sats` to `address`, which wallets read from a QR
/// code with the amount filled in.
pub fn uri(address: &str, sats: u64) -> String {
    let btc = (Decimal::from(sats) / Decimal::from(100_000_000)).normalize();
    format!("bitcoin:{address}?amount={btc}&label=Shakespeare%20AI%20Credits")
}

#[derive(Deserialize)]
struct Tx {
    status: TxStatus,
    vout: Vec<TxOut>,
    /// Sats, and weight units. Missing, a transaction counts as paying no
    /// fee, which only means nothing is advanced on it.
    #[serde(default)]
    fee: u64,
    #[serde(default)]
    weight: u64,
}

#[derive(Deserialize)]
struct TxStatus {
    confirmed: bool,
    block_height: Option<u64>,
}

#[derive(Deserialize)]
struct TxOut {
    scriptpubkey_address: Option<String>,
    value: u64,
}

/// What's been sent to an address, in sats.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Received {
    /// Everything, the mempool included.
    pub seen: u64,
    /// In blocks at least the required depth.
    pub confirmed: u64,
    /// In blocks, however deep.
    pub mined: u64,
    /// Each transaction still in the mempool.
    pub mempool: Vec<Unconfirmed>,
}

/// A transaction in the mempool: what it sends the address, and the fee
/// it pays for its weight.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Unconfirmed {
    pub sats: u64,
    pub fee: u64,
    pub weight: u64,
}

impl Unconfirmed {
    /// Whether it pays at least `rate` sats a vbyte (four weight units).
    pub fn pays(&self, rate: f64) -> bool {
        self.weight > 0 && self.fee as f64 * 4.0 >= rate * self.weight as f64
    }
}

impl Received {
    /// What's in a block, or in the mempool at `rate` or better: what will
    /// very likely confirm soon. Without a rate, only what's in a block.
    pub fn likely(&self, rate: Option<f64>) -> u64 {
        let mempool: u64 = rate.map_or(0, |rate| self.mempool.iter().filter(|tx| tx.pays(rate)).map(|tx| tx.sats).sum());
        self.mined + mempool
    }
}

fn endpoint(esplora: &url::Url, path: &str) -> String {
    format!("{}/{path}", esplora.as_str().trim_end_matches('/'))
}

/// The height of the best block.
pub async fn tip_height(http: &reqwest::Client, esplora: &url::Url) -> Result<u64> {
    let text = http.get(endpoint(esplora, "blocks/tip/height")).timeout(super::TIMEOUT).send().await?.error_for_status()?.text().await?;
    text.trim().parse().context("Esplora's tip height isn't a number")
}

/// What's been sent to `address`, counting a transaction as confirmed once
/// it's `confirmations` deep below `tip`. (Zero counts the mempool.)
///
/// Esplora lists the mempool and the 25 newest confirmed transactions; a
/// payment address sees one or two.
pub async fn received(http: &reqwest::Client, esplora: &url::Url, address: &str, tip: u64, confirmations: u32) -> Result<Received> {
    let txs: Vec<Tx> = http.get(endpoint(esplora, &format!("address/{address}/txs"))).timeout(super::TIMEOUT).send().await?.error_for_status()?.json().await?;
    let mut received = Received::default();
    for tx in txs {
        let value: u64 = tx.vout.iter().filter(|o| o.scriptpubkey_address.as_deref() == Some(address)).map(|o| o.value).sum();
        let depth = match (tx.status.confirmed, tx.status.block_height) {
            (true, Some(height)) => tip.saturating_sub(height) + 1,
            _ => 0,
        };
        if value == 0 {
            continue;
        }
        received.seen += value;
        if depth >= u64::from(confirmations) {
            received.confirmed += value;
        }
        if depth > 0 {
            received.mined += value;
        } else {
            received.mempool.push(Unconfirmed { sats: value, fee: tx.fee, weight: tx.weight });
        }
    }
    Ok(received)
}

/// The blocks a transaction we advance credit on should confirm within, by
/// its fee: one paying less could sit in the mempool until it's dropped,
/// and be double-spent then.
const FEE_TARGET: &str = "6";

/// The fee rate, in sats a vbyte, Esplora expects to confirm a transaction
/// within [`FEE_TARGET`] blocks.
pub async fn fee_rate(http: &reqwest::Client, esplora: &url::Url) -> Result<f64> {
    let estimates: std::collections::HashMap<String, f64> =
        http.get(endpoint(esplora, "fee-estimates")).timeout(super::TIMEOUT).send().await?.error_for_status()?.json().await?;
    estimates.get(FEE_TARGET).copied().filter(|rate| rate.is_finite() && *rate > 0.0).ok_or(anyhow!("Esplora has no fee estimate for {FEE_TARGET} blocks"))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use axum::routing::get;
    use axum::Json;
    use parking_lot::Mutex;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// BIP84's test vector: the account m/84'/0'/0' of "abandon … about".
    pub const ZPUB: &str = "zpub6rFR7y4Q2AijBEqTUquhVz398htDFrtymD9xYYfG1m4wAcvPhXNfE3EfH1r1ADqtfSdVCToUG868RvUUkgDKf31mGDtKsAYz2oz2AGutZYs";
    pub const FIRST: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";
    pub const SECOND: &str = "bc1qnjg0jd8228aq7egyzacy8cys3knf9xvrerkf9g";

    #[test]
    fn derives_bip84_addresses() {
        let wallet = Wallet::parse(ZPUB).unwrap();
        assert_eq!(wallet.address(0).unwrap(), FIRST);
        assert_eq!(wallet.address(1).unwrap(), SECOND);
        assert!(wallet.id().starts_with("xpub"));
        // The same key spelled as an xpub is the same account.
        let same = Wallet::parse(&wallet.id()).unwrap();
        assert_eq!(same.address(0).unwrap(), FIRST);
        assert_eq!(same.id(), wallet.id());

        assert!(Wallet::parse("nope").is_err());
        let mut ypub = bitcoin::base58::decode_check(ZPUB).unwrap();
        ypub[..4].copy_from_slice(&YPUB);
        assert!(Wallet::parse(&bitcoin::base58::encode_check(&ypub)).unwrap_err().to_string().contains("BIP49"));

        let mut vpub = bitcoin::base58::decode_check(ZPUB).unwrap();
        vpub[..4].copy_from_slice(&VPUB);
        let testnet = Wallet::parse(&bitcoin::base58::encode_check(&vpub)).unwrap();
        assert_eq!(testnet.network(), Network::Testnet);
        assert!(testnet.address(0).unwrap().starts_with("tb1q"));
        assert!(testnet.id().starts_with("tpub"));
    }

    #[test]
    fn makes_bip21_uris() {
        assert_eq!(uri(FIRST, 12_345), format!("bitcoin:{FIRST}?amount=0.00012345&label=Shakespeare%20AI%20Credits"));
        assert_eq!(uri(FIRST, 100_000_000), format!("bitcoin:{FIRST}?amount=1&label=Shakespeare%20AI%20Credits"));
    }

    /// A fake Esplora: a tip height, transactions by address, and the fee
    /// rate to confirm within 6 blocks (10 sats/vB unless set).
    #[derive(Clone, Default)]
    pub struct FakeEsplora {
        pub tip: Arc<Mutex<u64>>,
        pub txs: Arc<Mutex<HashMap<String, Vec<Value>>>>,
        pub fee_rate: Arc<Mutex<Option<f64>>>,
    }

    impl FakeEsplora {
        /// Pay `sats` to `address`, in the block at `height` or, if none, the
        /// mempool, with a transaction paying no fee.
        pub fn pay(&self, address: &str, sats: u64, height: Option<u64>) {
            self.pay_at(address, sats, height, 0.0);
        }

        /// [`FakeEsplora::pay`], paying `rate` sats a vbyte.
        pub fn pay_at(&self, address: &str, sats: u64, height: Option<u64>, rate: f64) {
            let weight = 561;
            let tx = json!({
                "txid": "00",
                "status": { "confirmed": height.is_some(), "block_height": height },
                "vout": [
                    { "scriptpubkey_address": address, "value": sats },
                    { "scriptpubkey_address": "bc1qchange", "value": 99_999 },
                ],
                "fee": (rate * weight as f64 / 4.0).ceil() as u64,
                "weight": weight,
            });
            self.txs.lock().entry(address.to_string()).or_default().push(tx);
        }

        pub async fn serve(&self) -> url::Url {
            let tip = |State(fake): State<FakeEsplora>| async move { fake.tip.lock().to_string() };
            let txs = |State(fake): State<FakeEsplora>, Path(address): Path<String>| async move {
                Json(Value::Array(fake.txs.lock().get(&address).cloned().unwrap_or_default()))
            };
            let fees = |State(fake): State<FakeEsplora>| async move {
                let rate = fake.fee_rate.lock().unwrap_or(10.0);
                Json(json!({ "1": rate * 2.0, "6": rate, "144": rate / 2.0 }))
            };
            let app = axum::Router::new()
                .route("/api/blocks/tip/height", get(tip))
                .route("/api/address/{address}/txs", get(txs))
                .route("/api/fee-estimates", get(fees))
                .with_state(self.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api/", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            url::Url::parse(&base).unwrap()
        }
    }

    #[tokio::test]
    async fn counts_what_arrived() {
        let fake = FakeEsplora::default();
        *fake.tip.lock() = 100;
        let esplora = fake.serve().await;
        let http = reqwest::Client::new();
        assert_eq!(tip_height(&http, &esplora).await.unwrap(), 100);
        assert_eq!(received(&http, &esplora, FIRST, 100, 1).await.unwrap(), Received::default());

        fake.pay_at(FIRST, 1000, None, 4.0);
        fake.pay(FIRST, 2000, Some(100));
        fake.pay(FIRST, 4000, Some(98));
        let mempool = || vec![Unconfirmed { sats: 1000, fee: 561, weight: 561 }];
        assert_eq!(received(&http, &esplora, FIRST, 100, 0).await.unwrap(), Received { seen: 7000, confirmed: 7000, mined: 6000, mempool: mempool() });
        assert_eq!(received(&http, &esplora, FIRST, 100, 1).await.unwrap(), Received { seen: 7000, confirmed: 6000, mined: 6000, mempool: mempool() });
        let deep = received(&http, &esplora, FIRST, 100, 3).await.unwrap();
        assert_eq!((deep.seen, deep.confirmed), (7000, 4000));
        assert_eq!(received(&http, &esplora, SECOND, 100, 1).await.unwrap(), Received::default());

        // What will likely confirm: what's mined, and what pays enough.
        assert_eq!(deep.likely(None), 6000);
        assert_eq!(deep.likely(Some(4.0)), 7000);
        assert_eq!(deep.likely(Some(4.1)), 6000);
        assert!(!Unconfirmed { sats: 1, fee: 100, weight: 0 }.pays(1.0));

        assert_eq!(fee_rate(&http, &esplora).await.unwrap(), 10.0);
        *fake.fee_rate.lock() = Some(2.5);
        assert_eq!(fee_rate(&http, &esplora).await.unwrap(), 2.5);
    }
}
