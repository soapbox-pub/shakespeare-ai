//! Monero: a fresh subaddress for each payment, from a view-only wallet in
//! `monero-wallet-rpc`, which scans the chain and says what's arrived.
//!
//! The service never holds a spend key: the wallet has only the view key, so
//! it sees money arrive but can't move it. Monero hides who's paid whom, so
//! no public API could say what an address received; the wallet has to scan
//! every block with the view key, as any Monero wallet does, against a node
//! we run ourselves.
//!
//! Its RPC takes HTTP digest authentication, which `reqwest` doesn't, so
//! that's done here: a request without credentials gets a challenge, and is
//! sent again answering it.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use md5::{Digest, Md5};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Piconero in a monero.
pub const PICONERO: u64 = 1_000_000_000_000;

/// How to reach the wallet: its JSON-RPC endpoint, and the login it was
/// started with (`--rpc-login`), if any.
pub struct Rpc<'a> {
    pub http: &'a reqwest::Client,
    pub url: &'a url::Url,
    pub login: Option<(&'a str, &'a str)>,
}

#[derive(Deserialize)]
struct Response<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl Rpc<'_> {
    async fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let body = json!({ "jsonrpc": "2.0", "id": "0", "method": method, "params": params });
        let send = |authorization: Option<String>| {
            let mut request = self.http.post(self.url.clone()).timeout(super::TIMEOUT).json(&body);
            if let Some(authorization) = authorization {
                request = request.header(AUTHORIZATION, authorization);
            }
            request.send()
        };
        let mut response = send(None).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let (username, password) = self.login.ok_or(anyhow!("monero-wallet-rpc wants a login, and none is configured"))?;
            let challenges: Vec<String> = response.headers().get_all(WWW_AUTHENTICATE).iter().filter_map(|v| v.to_str().ok()).map(str::to_string).collect();
            let uri = match self.url.query() {
                Some(query) => format!("{}?{query}", self.url.path()),
                None => self.url.path().to_string(),
            };
            let cnonce = crate::util::random_hex(16);
            let authorization = digest_authorization(&challenges, username, password, "POST", &uri, &cnonce)
                .ok_or(anyhow!("monero-wallet-rpc asked for a login we can't give (no MD5 digest challenge)"))?;
            response = send(Some(authorization)).await?;
            if response.status() == StatusCode::UNAUTHORIZED {
                bail!("monero-wallet-rpc refused the login");
            }
        }
        let response: Response<T> = response.error_for_status()?.json().await.with_context(|| format!("monero-wallet-rpc's answer to {method}"))?;
        match (response.result, response.error) {
            (_, Some(error)) => bail!("monero-wallet-rpc {method}: {} ({})", error.message, error.code),
            (Some(result), None) => Ok(result),
            (None, None) => bail!("monero-wallet-rpc {method}: no result"),
        }
    }

    /// The wallet's primary address, which names it: subaddress indexes are
    /// counted per wallet.
    pub async fn primary_address(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Address {
            address: String,
        }
        let address: Address = self.call("get_address", json!({ "account_index": 0, "address_index": [0] })).await?;
        Ok(address.address)
    }

    /// Make `count` new subaddresses in the wallet's first account, labelled
    /// `label`: the wallet's next indexes, and their addresses, in order.
    /// The wallet watches every subaddress it's made.
    pub async fn create_addresses(&self, count: u32, label: &str) -> Result<Vec<(u32, String)>> {
        #[derive(Deserialize)]
        struct Created {
            address: String,
            address_index: u32,
            #[serde(default)]
            addresses: Vec<String>,
            #[serde(default)]
            address_indices: Vec<u32>,
        }
        let created: Created = self.call("create_address", json!({ "account_index": 0, "count": count.max(1), "label": label })).await?;
        if created.addresses.is_empty() {
            return Ok(vec![(created.address_index, created.address)]);
        }
        if created.addresses.len() != created.address_indices.len() {
            bail!("monero-wallet-rpc made {} addresses but gave {} indexes", created.addresses.len(), created.address_indices.len());
        }
        Ok(created.address_indices.into_iter().zip(created.addresses).collect())
    }

    /// What's been sent to each of the subaddresses at `indexes`, counting a
    /// transfer as confirmed once it's `confirmations` deep. (Zero counts the
    /// mempool.)
    pub async fn received(&self, indexes: &[u32], confirmations: u32) -> Result<HashMap<u32, Received>> {
        if indexes.is_empty() {
            return Ok(HashMap::new());
        }
        let transfers: Transfers = self.call("get_transfers", json!({ "in": true, "pool": true, "account_index": 0, "subaddr_indices": indexes })).await?;
        Ok(tally(transfers, confirmations))
    }
}

/// What's been sent to a subaddress, in piconero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Received {
    /// Everything that counts, the mempool included.
    pub seen: u64,
    /// In blocks at least the required depth.
    pub confirmed: u64,
    /// Sent locked until some later block or time, which isn't counted at
    /// all: the money couldn't be spent until then. Wallets don't send it,
    /// but anyone can.
    pub locked: u64,
}

#[derive(Default, Deserialize)]
struct Transfers {
    #[serde(default, rename = "in")]
    confirmed: Vec<Transfer>,
    #[serde(default)]
    pool: Vec<Transfer>,
}

#[derive(Deserialize)]
struct Transfer {
    amount: u64,
    #[serde(default)]
    confirmations: u64,
    #[serde(default)]
    unlock_time: u64,
    /// The wallet has seen another transaction spend the same money, so this
    /// one may never confirm.
    #[serde(default)]
    double_spend_seen: bool,
    subaddr_index: SubaddrIndex,
}

#[derive(Deserialize)]
struct SubaddrIndex {
    major: u32,
    minor: u32,
}

fn tally(transfers: Transfers, confirmations: u32) -> HashMap<u32, Received> {
    let mut received: HashMap<u32, Received> = HashMap::new();
    let mined = transfers.confirmed.into_iter().map(|t| (t, true));
    let pool = transfers.pool.into_iter().map(|t| (t, false));
    for (transfer, in_block) in mined.chain(pool) {
        if transfer.subaddr_index.major != 0 {
            continue;
        }
        let entry = received.entry(transfer.subaddr_index.minor).or_default();
        if transfer.unlock_time != 0 {
            entry.locked += transfer.amount;
            continue;
        }
        if !in_block && transfer.double_spend_seen {
            continue;
        }
        entry.seen += transfer.amount;
        let depth = if in_block { transfer.confirmations } else { 0 };
        if depth >= u64::from(confirmations) {
            entry.confirmed += transfer.amount;
        }
    }
    received
}

/// A `monero:` URI for paying `piconero` to `address`, which wallets read
/// from a QR code with the amount filled in.
pub fn uri(address: &str, piconero: u64) -> String {
    format!("monero:{address}?tx_amount={}&tx_description=Shakespeare%20AI%20Credits", xmr(piconero))
}

/// Piconero as monero, without trailing zeros.
pub fn xmr(piconero: u64) -> Decimal {
    (Decimal::from(piconero) / Decimal::from(PICONERO)).normalize()
}

/// The `Authorization` header answering an HTTP digest challenge (RFC 7616,
/// MD5 with `qop=auth`, as monero-wallet-rpc offers), or `None` if none of
/// `challenges` is one.
fn digest_authorization(challenges: &[String], username: &str, password: &str, method: &str, uri: &str, cnonce: &str) -> Option<String> {
    let params = challenges.iter().filter_map(|c| parse_challenge(c)).find(|p| {
        let algorithm = p.get("algorithm").map(|a| a.to_ascii_uppercase());
        let qop_auth = p.get("qop").is_some_and(|q| q.split(',').any(|q| q.trim() == "auth"));
        matches!(algorithm.as_deref(), None | Some("MD5")) && qop_auth
    })?;
    let (realm, nonce) = (params.get("realm")?, params.get("nonce")?);
    let hex = |s: String| hex::encode(Md5::digest(s.as_bytes()));
    let ha1 = hex(format!("{username}:{realm}:{password}"));
    let ha2 = hex(format!("{method}:{uri}"));
    let nc = "00000001";
    let response = hex(format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}"));
    let mut header = format!(
        "Digest username=\"{username}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", algorithm=MD5, response=\"{response}\", qop=auth, nc={nc}, cnonce=\"{cnonce}\""
    );
    if let Some(opaque) = params.get("opaque") {
        header.push_str(&format!(", opaque=\"{opaque}\""));
    }
    Some(header)
}

/// A `Digest` challenge's parameters, `key=value` or `key="value"`, keys
/// lowercased.
fn parse_challenge(challenge: &str) -> Option<HashMap<String, String>> {
    let (scheme, rest) = challenge.trim().split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("digest") {
        return None;
    }
    let mut params = HashMap::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace() || *c == ',').is_some() {}
        let key: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ',')).collect();
        if key.trim().is_empty() {
            break;
        }
        if chars.next() != Some('=') {
            return None;
        }
        let value: String = if chars.next_if_eq(&'"').is_some() {
            let mut value = String::new();
            loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => value.push(chars.next()?),
                    c => value.push(c),
                }
            }
            value
        } else {
            std::iter::from_fn(|| chars.next_if(|c| *c != ',')).collect::<String>().trim().to_string()
        };
        params.insert(key.trim().to_ascii_lowercase(), value);
    }
    Some(params)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use axum::Json;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode as Status};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use parking_lot::Mutex;
    use std::sync::Arc;

    pub const PRIMARY: &str = "48x88Cgudqh9iGxEjLLrvZc3gqpvnom9E3kCQuQcrrbWcJc5qeYSPYgDtA7Y5Am9G4CkHMdCm6ubuDfdtXAL1RwgMX88Vpz";

    /// The address a [`FakeWallet`] gives subaddress `index`.
    pub fn subaddress(index: u32) -> String {
        format!("8fake{index:05}")
    }

    #[test]
    fn answers_digest_challenges() {
        // RFC 2617's example, section 3.5.
        let challenge =
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#;
        let header = digest_authorization(&[challenge.into()], "Mufasa", "Circle Of Life", "GET", "/dir/index.html", "0a4f113b").unwrap();
        assert!(header.contains(r#"response="6629fae49393a05397450978507c4ef1""#), "{header}");
        assert!(header.contains(r#"opaque="5ccc069c403ebaf9f0171e9517f40e41""#));

        // monero-wallet-rpc's pair: MD5, and MD5-sess, which we don't do.
        let sess = r#"Digest qop="auth",algorithm=MD5-sess,realm="monero-rpc",nonce="n1",stale=false"#.to_string();
        let md5 = r#"Digest qop="auth",algorithm=MD5,realm="monero-rpc",nonce="n2",stale=false"#.to_string();
        let header = digest_authorization(&[sess.clone(), md5], "u", "p", "POST", "/json_rpc", "c").unwrap();
        assert!(header.contains(r#"nonce="n2""#) && header.contains(r#"realm="monero-rpc""#));
        assert_eq!(digest_authorization(&[sess], "u", "p", "POST", "/json_rpc", "c"), None);
        assert_eq!(digest_authorization(&["Basic realm=\"x\"".into()], "u", "p", "POST", "/", "c"), None);
    }

    #[test]
    fn makes_monero_uris() {
        assert_eq!(uri("8abc", 21_560_000_000), "monero:8abc?tx_amount=0.02156&tx_description=Shakespeare%20AI%20Credits");
        assert_eq!(uri("8abc", PICONERO), "monero:8abc?tx_amount=1&tx_description=Shakespeare%20AI%20Credits");
    }

    #[test]
    fn counts_only_money_that_can_be_spent() {
        let transfer = |minor: u32, amount: u64, confirmations: u64| json!({ "amount": amount, "confirmations": confirmations, "unlock_time": 0, "subaddr_index": { "major": 0, "minor": minor } });
        let transfers: Transfers = serde_json::from_value(json!({
            "in": [
                transfer(1, 100, 3),
                transfer(1, 20, 1),
                transfer(2, 7, 9),
                // Locked for a while: not counted.
                { "amount": 1000, "confirmations": 9, "unlock_time": 4_000_000, "subaddr_index": { "major": 0, "minor": 2 } },
                // Another account's: not ours to count.
                { "amount": 5, "confirmations": 9, "unlock_time": 0, "subaddr_index": { "major": 1, "minor": 1 } },
            ],
            "pool": [
                transfer(1, 3, 0),
                // Double-spent in the mempool: not counted.
                { "amount": 50, "double_spend_seen": true, "subaddr_index": { "major": 0, "minor": 2 } },
            ],
        }))
        .unwrap();
        let received = tally(transfers, 2);
        assert_eq!(received[&1], Received { seen: 123, confirmed: 100, locked: 0 });
        assert_eq!(received[&2], Received { seen: 7, confirmed: 7, locked: 1000 });
        // An empty wallet answers {}.
        assert!(tally(serde_json::from_value(json!({})).unwrap(), 1).is_empty());
    }

    /// A fake monero-wallet-rpc: a wallet whose subaddresses are made in
    /// order, and transfers to them. It asks for a digest login if `login` is
    /// set.
    #[derive(Clone, Default)]
    pub struct FakeWallet {
        pub next: Arc<Mutex<u32>>,
        pub transfers: Arc<Mutex<Vec<Value>>>,
        pub login: Arc<Mutex<Option<(String, String)>>>,
    }

    impl FakeWallet {
        /// Send `piconero` to subaddress `index`, `confirmations` deep, or in
        /// the mempool if `None`.
        pub fn pay(&self, index: u32, piconero: u64, confirmations: Option<u64>) {
            let kind = if confirmations.is_some() { "in" } else { "pool" };
            self.transfers.lock().push(json!({
                "type": kind, "amount": piconero, "confirmations": confirmations.unwrap_or(0), "unlock_time": 0,
                "subaddr_index": { "major": 0, "minor": index },
            }));
        }

        pub async fn serve(&self) -> url::Url {
            async fn rpc(State(fake): State<FakeWallet>, headers: HeaderMap, Json(request): Json<Value>) -> axum::response::Response {
                let login = fake.login.lock().clone();
                if let Some((username, password)) = login {
                    let challenge = r#"Digest qop="auth",algorithm=MD5,realm="monero-rpc",nonce="abc",stale=false"#.to_string();
                    let given = headers.get("authorization").and_then(|h| h.to_str().ok()).unwrap_or_default();
                    let cnonce = parse_challenge(given).and_then(|p| p.get("cnonce").cloned()).unwrap_or_default();
                    let expected = digest_authorization(std::slice::from_ref(&challenge), &username, &password, "POST", "/json_rpc", &cnonce);
                    if expected.as_deref() != Some(given) {
                        return (Status::UNAUTHORIZED, [("www-authenticate", challenge)]).into_response();
                    }
                }
                let params = &request["params"];
                let result = match request["method"].as_str().unwrap_or_default() {
                    "get_address" => json!({ "address": PRIMARY }),
                    "create_address" => {
                        let count = params["count"].as_u64().unwrap_or(1) as u32;
                        let mut next = fake.next.lock();
                        let first = (*next).max(1);
                        *next = first + count;
                        let indices: Vec<u32> = (first..first + count).collect();
                        let addresses: Vec<String> = indices.iter().map(|i| subaddress(*i)).collect();
                        json!({ "address": addresses[0], "address_index": first, "addresses": addresses, "address_indices": indices })
                    }
                    "get_transfers" => {
                        let wanted: Vec<u64> = params["subaddr_indices"].as_array().unwrap().iter().filter_map(Value::as_u64).collect();
                        let mut result = json!({});
                        for transfer in fake.transfers.lock().iter().filter(|t| wanted.contains(&t["subaddr_index"]["minor"].as_u64().unwrap())) {
                            let kind = transfer["type"].as_str().unwrap();
                            result.as_object_mut().unwrap().entry(kind).or_insert(json!([])).as_array_mut().unwrap().push(transfer.clone());
                        }
                        result
                    }
                    other => {
                        return Json(json!({ "id": "0", "jsonrpc": "2.0", "error": { "code": -32601, "message": format!("no method {other}") } }))
                            .into_response();
                    }
                };
                Json(json!({ "id": "0", "jsonrpc": "2.0", "result": result })).into_response()
            }
            let app = axum::Router::new().route("/json_rpc", post(rpc)).with_state(self.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/json_rpc", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            url::Url::parse(&base).unwrap()
        }
    }

    #[tokio::test]
    async fn talks_to_the_wallet() {
        let fake = FakeWallet::default();
        *fake.login.lock() = Some(("shakespeare".into(), "hunter2".into()));
        let url = fake.serve().await;
        let http = reqwest::Client::new();
        let rpc = Rpc { http: &http, url: &url, login: Some(("shakespeare", "hunter2")) };
        assert_eq!(rpc.primary_address().await.unwrap(), PRIMARY);
        assert_eq!(rpc.create_addresses(1, "pi_1").await.unwrap(), vec![(1, subaddress(1))]);
        assert_eq!(rpc.create_addresses(2, "pi_2").await.unwrap(), vec![(2, subaddress(2)), (3, subaddress(3))]);

        fake.pay(1, 500, Some(1));
        fake.pay(1, 250, None);
        fake.pay(3, 9, Some(10));
        let received = rpc.received(&[1, 2], 1).await.unwrap();
        assert_eq!(received.get(&1), Some(&Received { seen: 750, confirmed: 500, locked: 0 }));
        assert_eq!(received.get(&2), None);
        assert!(rpc.received(&[], 1).await.unwrap().is_empty());

        let wrong = Rpc { login: Some(("shakespeare", "nope")), ..rpc };
        assert!(wrong.primary_address().await.unwrap_err().to_string().contains("refused the login"));
        let none = Rpc { login: None, ..rpc };
        assert!(none.primary_address().await.unwrap_err().to_string().contains("wants a login"));
    }
}
