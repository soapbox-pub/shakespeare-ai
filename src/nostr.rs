//! The little of Nostr this service needs: verifying events (NIP-01), NIP-98
//! HTTP auth, and spelling a pubkey as an npub (NIP-19).
//!
//! Adapted from npanel's `nostr/event.rs`, `auth.rs` and `nostr/nip19.rs`.

use base64::Engine;
use secp256k1::{schnorr, XOnlyPublicKey, SECP256K1};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: i64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl Event {
    /// The id NIP-01 defines: the sha256 of the event's canonical
    /// serialization.
    pub fn compute_id(pubkey: &str, created_at: i64, kind: i64, tags: &[Vec<String>], content: &str) -> String {
        let canonical = serde_json::to_string(&(0, pubkey, created_at, kind, tags, content)).unwrap_or_default();
        hex::encode(Sha256::digest(canonical.as_bytes()))
    }

    /// Whether the id is the hash of the event and the signature is its
    /// author's.
    pub fn verify(&self) -> bool {
        if !is_hex64(&self.id) || !is_hex64(&self.pubkey) || self.sig.len() != 128 {
            return false;
        }
        if Self::compute_id(&self.pubkey, self.created_at, self.kind, &self.tags, &self.content) != self.id {
            return false;
        }
        let (Ok(id), Ok(pubkey), Ok(sig)) = (hex::decode(&self.id), hex::decode(&self.pubkey), hex::decode(&self.sig)) else {
            return false;
        };
        let (Ok(pubkey), Ok(sig)) = (XOnlyPublicKey::from_slice(&pubkey), schnorr::Signature::from_slice(&sig)) else {
            return false;
        };
        SECP256K1.verify_schnorr(&sig, &id, &pubkey).is_ok()
    }

    /// The value of the first tag named `name`.
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags.iter().find(|tag| tag.first().map(String::as_str) == Some(name)).and_then(|tag| tag.get(1)).map(String::as_str)
    }

    #[cfg(test)]
    pub fn sign(secret: &str, created_at: i64, kind: i64, tags: Vec<Vec<String>>, content: &str) -> Event {
        let keypair = secp256k1::Keypair::from_seckey_str_global(secret).expect("a valid secret key");
        let pubkey = hex::encode(keypair.x_only_public_key().0.serialize());
        let id = Self::compute_id(&pubkey, created_at, kind, &tags, content);
        let digest = hex::decode(&id).expect("hex");
        let sig = SECP256K1.sign_schnorr_no_aux_rand(&digest, &keypair);
        Event { id, pubkey, created_at, kind, tags, content: content.to_string(), sig: hex::encode(sig.to_byte_array()) }
    }
}

/// Whether a string is 64 lowercase hex characters.
pub fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// NIP-98 HTTP auth event kind.
pub const NIP98_KIND: i64 = 27235;

/// How far an auth event's `created_at` may be from now, in seconds.
pub const MAX_AUTH_AGE: i64 = 60;

/// Verify an `Authorization: Nostr <base64 event>` header against a request,
/// returning the pubkey that signed it.
///
/// The signed `u` tag is compared against the public origin rather than the
/// URL this process sees, which differs behind a reverse proxy. Host, path and
/// query must match; scheme and port are tolerated.
///
/// A body is only bound to the signature by the `payload` tag, so a request
/// with one must carry it — otherwise anyone who saw the header could replay
/// it within the window with a body of their own. Every known client
/// (nostrify's `NIP98Client`, nostr-tools' `getToken` with a payload) sends it.
pub fn verify_nip98(header: Option<&str>, method: &str, path_and_query: &str, body: &[u8], host: &str, now: i64) -> Result<String, String> {
    let header = header.ok_or("Missing Nostr authorization header")?;
    let token = header.strip_prefix("Nostr ").map(str::trim).filter(|t| !t.is_empty()).ok_or("Missing Nostr authorization token")?;

    let event = base64::engine::general_purpose::STANDARD
        .decode(token)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(token))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Event>(&bytes).ok())
        .ok_or("Invalid token")?;

    if event.kind != NIP98_KIND {
        return Err(format!("Event must be kind {NIP98_KIND}"));
    }
    if !event.verify() {
        return Err("Event signature is invalid".into());
    }
    // Unsigned arithmetic: `created_at` is anything the signer likes, and the
    // difference of two i64s can overflow.
    if now.abs_diff(event.created_at) > MAX_AUTH_AGE as u64 {
        return Err("Event expired. Check your device's clock.".into());
    }
    if !event.tag("method").is_some_and(|m| m.eq_ignore_ascii_case(method)) {
        return Err("Event method does not match HTTP request method".into());
    }

    let signed = event.tag("u").and_then(|u| url::Url::parse(u).ok()).ok_or("Event URL is missing or invalid")?;
    let requested = url::Url::parse(&format!("http://request{path_and_query}")).ok();
    let same = signed.host_str().is_some_and(|h| h.eq_ignore_ascii_case(host))
        && requested.is_some_and(|r| r.path() == signed.path() && r.query().unwrap_or("") == signed.query().unwrap_or(""));
    if !same {
        return Err("Event URL does not match request URL".into());
    }

    match event.tag("payload") {
        None if !body.is_empty() => return Err("Event must carry a payload tag for a request with a body".into()),
        Some(payload) if !payload.eq_ignore_ascii_case(&hex::encode(Sha256::digest(body))) => {
            return Err("Event payload does not match request body".into());
        }
        _ => {}
    }

    Ok(event.pubkey)
}

#[cfg(test)]
pub fn sign_nip98(secret: &str, method: &str, url: &str, body: Option<&[u8]>, now: i64) -> String {
    let mut tags = vec![vec!["u".to_string(), url.to_string()], vec!["method".to_string(), method.to_string()]];
    if let Some(body) = body {
        tags.push(vec!["payload".into(), hex::encode(Sha256::digest(body))]);
    }
    let event = Event::sign(secret, now, NIP98_KIND, tags, "");
    format!("Nostr {}", base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&event).unwrap()))
}

#[cfg(test)]
pub fn public_key(secret: &str) -> String {
    let keypair = secp256k1::Keypair::from_seckey_str_global(secret).expect("a valid secret key");
    hex::encode(keypair.x_only_public_key().0.serialize())
}

const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const GENERATOR: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];

/// A pubkey's npub. Every pubkey here was checked where it entered, so
/// anything else gets an empty string — plainly wrong — rather than a
/// valid-looking npub of nothing.
pub fn npub_encode(pubkey_hex: &str) -> String {
    let mut key = [0u8; 32];
    if hex::decode_to_slice(pubkey_hex, &mut key).is_err() {
        return String::new();
    }
    bech32_encode("npub", &key)
}

fn polymod(values: impl IntoIterator<Item = u8>) -> u32 {
    let mut chk: u32 = 1;
    for value in values {
        let top = chk >> 25;
        chk = ((chk & 0x1ffffff) << 5) ^ u32::from(value);
        for (i, generator) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= generator;
            }
        }
    }
    chk
}

fn bech32_encode(hrp: &str, data: &[u8]) -> String {
    let mut words = Vec::with_capacity(data.len() * 8 / 5 + 1);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &byte in data {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            words.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        words.push(((acc << (5 - bits)) & 31) as u8);
    }

    let mut values: Vec<u8> = hrp.bytes().map(|b| b >> 5).collect();
    values.push(0);
    values.extend(hrp.bytes().map(|b| b & 31));
    values.extend_from_slice(&words);
    values.extend_from_slice(&[0; 6]);
    let checksum = polymod(values) ^ 1;

    let mut out = format!("{hrp}1");
    out.extend(words.iter().map(|w| CHARSET[*w as usize] as char));
    out.extend((0..6).map(|i| CHARSET[((checksum >> (5 * (5 - i))) & 31) as usize] as char));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0000000000000000000000000000000000000000000000000000000000000003";
    const NOW: i64 = 1_800_000_000;

    fn check(header: &str, method: &str, path: &str, body: &[u8]) -> Result<String, String> {
        verify_nip98(Some(header), method, path, body, "ai.example.com", NOW)
    }

    #[test]
    fn verifies_nip98() {
        let header = sign_nip98(SECRET, "GET", "https://ai.example.com/v1/credits?x=1", None, NOW);
        assert_eq!(check(&header, "GET", "/v1/credits?x=1", b"").unwrap(), public_key(SECRET));
        assert!(check(&header, "POST", "/v1/credits?x=1", b"").unwrap_err().contains("method"));
        assert!(check(&header, "GET", "/v1/credits", b"").unwrap_err().contains("URL"));
        assert!(check(&header, "GET", "/v1/credits?x=1", b"{}").unwrap_err().contains("payload tag"));

        let with_body = sign_nip98(SECRET, "POST", "http://ai.example.com:8080/v1/credits/add", Some(b"{}"), NOW);
        assert!(check(&with_body, "POST", "/v1/credits/add", b"{}").is_ok());
        assert!(check(&with_body, "POST", "/v1/credits/add", b"{ }").unwrap_err().contains("does not match"));

        let old = sign_nip98(SECRET, "GET", "https://ai.example.com/v1/credits", None, NOW - 61);
        assert!(check(&old, "GET", "/v1/credits", b"").unwrap_err().contains("expired"));
        let ancient = sign_nip98(SECRET, "GET", "https://ai.example.com/v1/credits", None, i64::MIN);
        assert!(check(&ancient, "GET", "/v1/credits", b"").unwrap_err().contains("expired"));

        let other = sign_nip98(SECRET, "GET", "https://evil.example.com/v1/credits", None, NOW);
        assert!(check(&other, "GET", "/v1/credits", b"").is_err());
        assert!(verify_nip98(None, "GET", "/", b"", "x", NOW).unwrap_err().starts_with("Missing"));
        assert!(check("Nostr !!!", "GET", "/", b"").unwrap_err().contains("Invalid"));
    }

    #[test]
    fn encodes_npub() {
        // NIP-19's own example.
        assert_eq!(
            npub_encode("3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"),
            "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6"
        );
    }
}
