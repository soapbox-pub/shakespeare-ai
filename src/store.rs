//! Every query: accounts and the ledger, payments, gift cards, generations.
//!
//! Each change to a balance goes through [`post`], which writes a ledger entry
//! and the cached balance together, and refuses a second entry for the same
//! thing. Status changes that release money (a payment completing, a gift card
//! being redeemed or cancelled) are a single conditional `UPDATE`, and money
//! only moves if it changed a row — so two racing requests can't both win.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::money::Money;
use crate::util::{giftcard_code, random_hex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Opening,
    Payment,
    Generation,
    GiftcardPurchase,
    GiftcardRedeem,
    GiftcardRefund,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Opening => "opening",
            Kind::Payment => "payment",
            Kind::Generation => "generation",
            Kind::GiftcardPurchase => "giftcard_purchase",
            Kind::GiftcardRedeem => "giftcard_redeem",
            Kind::GiftcardRefund => "giftcard_refund",
        }
    }
}

/// Start a transaction that takes the write lock up front, so what it reads
/// can't change before it writes.
pub fn write_tx(conn: &mut Connection) -> rusqlite::Result<Transaction<'_>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

/// Move `amount` into (or, negative, out of) an account, once per
/// `(kind, reference)`. Whether it moved.
pub fn post(tx: &Transaction, pubkey: &str, amount: Money, kind: Kind, reference: &str, now: i64) -> Result<bool> {
    let inserted = tx.execute(
        "INSERT INTO ledger (pubkey, amount, kind, ref, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT (kind, ref) DO NOTHING",
        params![pubkey, amount.nanos(), kind.as_str(), reference, now],
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO accounts (pubkey, balance) VALUES (?1, ?2) ON CONFLICT (pubkey) DO UPDATE SET balance = balance + ?2",
        params![pubkey, amount.nanos()],
    )?;
    Ok(true)
}

pub fn balance(conn: &Connection, pubkey: &str) -> Result<Money> {
    let nanos: Option<i64> = conn.query_row("SELECT balance FROM accounts WHERE pubkey = ?", [pubkey], |r| r.get(0)).optional()?;
    Ok(Money(nanos.unwrap_or(0)))
}

// Payments.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Stripe,
    Lightning,
    Bitcoin,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Stripe => "stripe",
            Method::Lightning => "lightning",
            Method::Bitcoin => "bitcoin",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "stripe" => Some(Method::Stripe),
            "lightning" => Some(Method::Lightning),
            "bitcoin" => Some(Method::Bitcoin),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Payment {
    pub id: String,
    pub status: String,
    pub amount: Money,
    pub fee: Money,
    pub total: Money,
    pub method: Method,
    pub description: String,
    pub url: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    pub expires_at: i64,
    #[serde(skip)]
    pub pubkey: String,
    #[serde(skip)]
    pub provider_ref: Option<String>,
}

pub struct NewPayment<'a> {
    pub id: &'a str,
    pub pubkey: &'a str,
    pub method: Method,
    pub amount: Money,
    pub fee: Money,
    pub url: &'a str,
    pub provider_ref: Option<&'a str>,
    pub created_at: i64,
    pub expires_at: i64,
}

pub fn new_payment_id() -> String {
    format!("pi_{}", random_hex(32))
}

pub fn insert_payment(conn: &Connection, p: &NewPayment) -> Result<()> {
    conn.execute(
        "INSERT INTO payments (id, pubkey, method, status, amount, fee, url, provider_ref, created_at, expires_at)
         VALUES (?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?)",
        params![p.id, p.pubkey, p.method.as_str(), p.amount.nanos(), p.fee.nanos(), p.url, p.provider_ref, p.created_at, p.expires_at],
    )?;
    Ok(())
}

const PAYMENT_COLUMNS: &str = "id, pubkey, method, status, amount, fee, url, provider_ref, created_at, completed_at, expires_at";

fn payment_row(row: &rusqlite::Row) -> rusqlite::Result<Payment> {
    let method = Method::parse(&row.get::<_, String>(2)?).unwrap_or(Method::Stripe);
    let amount = Money(row.get(4)?);
    let fee = Money(row.get(5)?);
    Ok(Payment {
        id: row.get(0)?,
        pubkey: row.get(1)?,
        method,
        status: row.get(3)?,
        amount,
        fee,
        total: Money(amount.nanos() + fee.nanos()),
        description: match method {
            Method::Stripe => "Credit purchase via Stripe".into(),
            Method::Lightning => "Credit purchase via Lightning".into(),
            Method::Bitcoin => "Credit purchase via Bitcoin".into(),
        },
        url: row.get(6)?,
        provider_ref: row.get(7)?,
        created_at: row.get(8)?,
        completed_at: row.get(9)?,
        expires_at: row.get(10)?,
    })
}

pub fn get_payment(conn: &Connection, id: &str) -> Result<Option<Payment>> {
    Ok(conn.query_row(&format!("SELECT {PAYMENT_COLUMNS} FROM payments WHERE id = ?"), [id], payment_row).optional()?)
}

pub fn list_payments(
    conn: &Connection,
    pubkey: &str,
    limit: usize,
    starting_after: Option<&str>,
    status: Option<&str>,
    method: Option<&str>,
) -> Result<(Vec<Payment>, bool)> {
    let cursor = starting_after.map(|id| cursor_of(conn, "payments", "pubkey", pubkey, id)).transpose()?.flatten();
    let (after_time, after_id) = cursor.unzip();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {PAYMENT_COLUMNS} FROM payments
         WHERE pubkey = ?1 AND (?2 IS NULL OR (created_at, id) < (?2, ?3)) AND (?4 IS NULL OR status = ?4) AND (?5 IS NULL OR method = ?5)
         ORDER BY created_at DESC, id DESC LIMIT ?6"
    ))?;
    let rows = stmt.query_map(params![pubkey, after_time, after_id, status, method, limit as i64 + 1], payment_row)?;
    page(rows.collect::<rusqlite::Result<Vec<_>>>()?, limit)
}

/// Complete a payment and credit its buyer, if it hasn't been already.
/// Whether this call did it.
///
/// An expired payment can still complete: Stripe can deliver a webhook late,
/// and an invoice can be paid in its last moment and seen after we gave up on
/// it. If the money arrived, the credits are owed.
pub fn complete_payment(conn: &mut Connection, id: &str, now: i64) -> Result<bool> {
    let tx = write_tx(conn)?;
    let row: Option<(String, i64)> = tx
        .query_row(
            "UPDATE payments SET status = 'completed', completed_at = ?2 WHERE id = ?1 AND status IN ('pending', 'expired') RETURNING pubkey, amount",
            params![id, now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((pubkey, amount)) = row else {
        return Ok(false);
    };
    post(&tx, &pubkey, Money(amount), Kind::Payment, id, now)?;
    tx.commit()?;
    Ok(true)
}

pub fn expire_payment(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("UPDATE payments SET status = 'expired' WHERE id = ? AND status = 'pending'", [id])?;
    Ok(())
}

/// Pending payments whose method we check on ourselves (Lightning).
pub fn pending_lightning(conn: &Connection) -> Result<Vec<Payment>> {
    let mut stmt = conn.prepare_cached(&format!("SELECT {PAYMENT_COLUMNS} FROM payments WHERE status = 'pending' AND method = 'lightning'"))?;
    let rows = stmt.query_map([], payment_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The next unused receive address index for an account key.
pub fn next_bitcoin_index(conn: &Connection, xpub: &str) -> Result<u32> {
    let next: i64 = conn.query_row("SELECT COALESCE(MAX(idx) + 1, 0) FROM bitcoin_addresses WHERE xpub = ?", [xpub], |r| r.get(0))?;
    Ok(u32::try_from(next)?)
}

pub fn insert_bitcoin_address(conn: &Connection, payment_id: &str, xpub: &str, index: u32, address: &str, sats: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO bitcoin_addresses (payment_id, xpub, idx, address, sats) VALUES (?, ?, ?, ?, ?)",
        params![payment_id, xpub, index, address, i64::try_from(sats)?],
    )?;
    Ok(())
}

/// Bitcoin payments to look in on, with the sats each is owed: those
/// pending, and those that expired within `grace` seconds, in case they're
/// paid late.
pub fn bitcoin_to_watch(conn: &Connection, now: i64, grace: i64) -> Result<Vec<(Payment, u64)>> {
    let columns = PAYMENT_COLUMNS.split(", ").map(|c| format!("p.{c}")).collect::<Vec<_>>().join(", ");
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {columns}, b.sats FROM payments p JOIN bitcoin_addresses b ON b.payment_id = p.id
         WHERE p.method = 'bitcoin' AND (p.status = 'pending' OR (p.status = 'expired' AND p.expires_at > ?))"
    ))?;
    let rows = stmt.query_map([now - grace], |r| Ok((payment_row(r)?, r.get::<_, i64>(11)? as u64)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Expire every pending Stripe payment past its time; Stripe's own session
/// expires with it, and a late webhook can still complete it.
pub fn expire_stripe_payments(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute("UPDATE payments SET status = 'expired' WHERE status = 'pending' AND method = 'stripe' AND expires_at <= ?", [now])?)
}

// Gift cards.

#[derive(Clone, Debug, Serialize)]
pub struct Giftcard {
    pub id: String,
    pub code: String,
    pub amount: Money,
    pub redeemed: bool,
    pub created_at: i64,
    #[serde(skip)]
    pub status: String,
}

const GIFTCARD_COLUMNS: &str = "id, code, amount, status, created_at";

fn giftcard_row(row: &rusqlite::Row) -> rusqlite::Result<Giftcard> {
    let status: String = row.get(3)?;
    Ok(Giftcard {
        id: row.get(0)?,
        code: row.get(1)?,
        amount: Money(row.get(2)?),
        redeemed: status == "redeemed",
        status,
        created_at: row.get(4)?,
    })
}

#[derive(Debug, PartialEq)]
pub enum GiftcardError {
    NotFound,
    Cancelled,
    Redeemed,
    Insufficient { required: Money, available: Money },
}

/// Make `quantity` gift cards of `amount` each. With `charge`, their sender
/// pays for them, and must be able to; without, they're minted (the
/// `giftcards` command).
pub fn create_giftcards(
    conn: &mut Connection,
    sender: &str,
    amount: Money,
    quantity: u32,
    charge: bool,
    now: i64,
) -> Result<Result<Vec<Giftcard>, GiftcardError>> {
    let tx = write_tx(conn)?;
    let required = amount.checked_mul(i64::from(quantity)).unwrap_or(Money(i64::MAX));
    if charge {
        let available = balance(&tx, sender)?;
        if available < required {
            return Ok(Err(GiftcardError::Insufficient { required, available }));
        }
    }
    let mut cards = Vec::with_capacity(quantity as usize);
    for _ in 0..quantity {
        let id = format!("gc_{}", random_hex(12));
        let code = giftcard_code();
        tx.execute(
            "INSERT INTO giftcards (id, code, sender_pubkey, amount, status, created_at) VALUES (?, ?, ?, ?, 'active', ?)",
            params![id, code, sender, amount.nanos(), now],
        )?;
        if charge {
            post(&tx, sender, -amount, Kind::GiftcardPurchase, &id, now)?;
        }
        cards.push(Giftcard { id, code, amount, redeemed: false, created_at: now, status: "active".into() });
    }
    tx.commit()?;
    Ok(Ok(cards))
}

pub fn redeem_giftcard(conn: &mut Connection, redeemer: &str, code: &str, now: i64) -> Result<Result<Money, GiftcardError>> {
    let tx = write_tx(conn)?;
    let redeemed: Option<(String, i64)> = tx
        .query_row(
            "UPDATE giftcards SET status = 'redeemed', redeemed_by = ?2, closed_at = ?3 WHERE code = ?1 AND status = 'active' RETURNING id, amount",
            params![code, redeemer, now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, amount)) = redeemed else {
        let status: Option<String> = tx.query_row("SELECT status FROM giftcards WHERE code = ?", [code], |r| r.get(0)).optional()?;
        return Ok(Err(match status.as_deref() {
            Some("cancelled") => GiftcardError::Cancelled,
            Some(_) => GiftcardError::Redeemed,
            None => GiftcardError::NotFound,
        }));
    };
    post(&tx, redeemer, Money(amount), Kind::GiftcardRedeem, &id, now)?;
    tx.commit()?;
    Ok(Ok(Money(amount)))
}

/// Cancel an unredeemed gift card and refund its sender.
pub fn cancel_giftcard(conn: &mut Connection, sender: &str, id: &str, now: i64) -> Result<Result<(), GiftcardError>> {
    let tx = write_tx(conn)?;
    let amount: Option<i64> = tx
        .query_row(
            "UPDATE giftcards SET status = 'cancelled', closed_at = ?3 WHERE id = ?1 AND sender_pubkey = ?2 AND status = 'active' RETURNING amount",
            params![id, sender, now],
            |r| r.get(0),
        )
        .optional()?;
    let Some(amount) = amount else {
        let status: Option<String> =
            tx.query_row("SELECT status FROM giftcards WHERE id = ? AND sender_pubkey = ?", [id, sender], |r| r.get(0)).optional()?;
        return Ok(Err(match status.as_deref() {
            Some("cancelled") => GiftcardError::Cancelled,
            Some(_) => GiftcardError::Redeemed,
            None => GiftcardError::NotFound,
        }));
    };
    post(&tx, sender, Money(amount), Kind::GiftcardRefund, id, now)?;
    tx.commit()?;
    Ok(Ok(()))
}

pub fn get_giftcard(conn: &Connection, sender: &str, id: &str) -> Result<Option<Giftcard>> {
    Ok(conn
        .query_row(&format!("SELECT {GIFTCARD_COLUMNS} FROM giftcards WHERE id = ? AND sender_pubkey = ?"), [id, sender], giftcard_row)
        .optional()?)
}

pub fn giftcard_by_code(conn: &Connection, code: &str) -> Result<Option<Giftcard>> {
    Ok(conn.query_row(&format!("SELECT {GIFTCARD_COLUMNS} FROM giftcards WHERE code = ?"), [code], giftcard_row).optional()?)
}

pub fn list_giftcards(conn: &Connection, sender: &str, limit: usize, after: Option<&str>) -> Result<(Vec<Giftcard>, bool)> {
    let cursor = after.map(|id| cursor_of(conn, "giftcards", "sender_pubkey", sender, id)).transpose()?.flatten();
    let (after_time, after_id) = cursor.unzip();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {GIFTCARD_COLUMNS} FROM giftcards WHERE sender_pubkey = ?1 AND (?2 IS NULL OR (created_at, id) < (?2, ?3))
         ORDER BY created_at DESC, id DESC LIMIT ?4"
    ))?;
    let rows = stmt.query_map(params![sender, after_time, after_id, limit as i64 + 1], giftcard_row)?;
    page(rows.collect::<rusqlite::Result<Vec<_>>>()?, limit)
}

// Generations.

#[derive(Clone, Debug)]
pub struct Generation {
    pub id: String,
    pub upstream_id: Option<String>,
    pub pubkey: String,
    pub model: String,
    pub created_at: i64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost: Money,
    pub estimated: bool,
    /// The API key it was made with, if it wasn't signed for with NIP-98.
    pub api_key_id: Option<String>,
}

pub fn new_generation_id() -> String {
    format!("gen_{}", random_hex(32))
}

/// Record a generation and charge for it, together.
pub fn record_generation(conn: &mut Connection, g: &Generation) -> Result<()> {
    let tx = write_tx(conn)?;
    tx.execute(
        "INSERT INTO generations (id, upstream_id, pubkey, model, created_at, prompt_tokens, completion_tokens, cost, estimated, api_key_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            g.id,
            g.upstream_id,
            g.pubkey,
            g.model,
            g.created_at,
            g.prompt_tokens as i64,
            g.completion_tokens as i64,
            g.cost.nanos(),
            g.estimated,
            g.api_key_id
        ],
    )?;
    if g.cost.is_positive() {
        post(&tx, &g.pubkey, -g.cost, Kind::Generation, &g.id, g.created_at)?;
    }
    tx.commit()?;
    Ok(())
}

pub fn list_generations(conn: &Connection, pubkey: &str, limit: usize, after: Option<&str>) -> Result<(Vec<Generation>, bool)> {
    let cursor = after.map(|id| cursor_of(conn, "generations", "pubkey", pubkey, id)).transpose()?.flatten();
    let (after_time, after_id) = cursor.unzip();
    let mut stmt = conn.prepare_cached(
        "SELECT id, upstream_id, pubkey, model, created_at, prompt_tokens, completion_tokens, cost, estimated, api_key_id FROM generations
         WHERE pubkey = ?1 AND (?2 IS NULL OR (created_at, id) < (?2, ?3)) ORDER BY created_at DESC, id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![pubkey, after_time, after_id, limit as i64 + 1], |r| {
        Ok(Generation {
            id: r.get(0)?,
            upstream_id: r.get(1)?,
            pubkey: r.get(2)?,
            model: r.get(3)?,
            created_at: r.get(4)?,
            prompt_tokens: r.get::<_, i64>(5)? as u64,
            completion_tokens: r.get::<_, i64>(6)? as u64,
            cost: Money(r.get(7)?),
            estimated: r.get(8)?,
            api_key_id: r.get(9)?,
        })
    })?;
    page(rows.collect::<rusqlite::Result<Vec<_>>>()?, limit)
}

// API keys.

/// How often a key's spend limit starts again: at midnight UTC, on Mondays,
/// or on the 1st of the month.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LimitReset {
    Daily,
    Weekly,
    Monthly,
}

impl LimitReset {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitReset::Daily => "daily",
            LimitReset::Weekly => "weekly",
            LimitReset::Monthly => "monthly",
        }
    }

    pub fn parse(s: &str) -> Option<LimitReset> {
        match s {
            "daily" => Some(LimitReset::Daily),
            "weekly" => Some(LimitReset::Weekly),
            "monthly" => Some(LimitReset::Monthly),
            _ => None,
        }
    }

    /// When the period containing `now` began.
    pub fn period_start(self, now: i64) -> i64 {
        match self {
            LimitReset::Daily => crate::util::day_start(now),
            LimitReset::Weekly => crate::util::week_start(now),
            LimitReset::Monthly => crate::util::month_start(now),
        }
    }
}

/// What every API key starts with; the rest is random.
pub const API_KEY_PREFIX: &str = "sk-shakespeare-";

/// The most keys one account may have that aren't revoked.
pub const MAX_API_KEYS: i64 = 100;

#[derive(Clone, Debug)]
pub struct ApiKey {
    pub id: String,
    pub pubkey: String,
    pub label: String,
    pub name: String,
    pub disabled: bool,
    pub limit: Option<Money>,
    pub limit_reset: Option<LimitReset>,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl ApiKey {
    /// Why it can't be used now, if it can't.
    pub fn unusable(&self, now: i64) -> Option<Unusable> {
        if self.revoked_at.is_some() {
            Some(Unusable::Revoked)
        } else if self.disabled {
            Some(Unusable::Disabled)
        } else if self.expires_at.is_some_and(|at| at <= now) {
            Some(Unusable::Expired)
        } else {
            None
        }
    }

    /// When the spend that counts against its limit began: the start of the
    /// current period, or forever.
    pub fn limit_since(&self, now: i64) -> i64 {
        self.limit_reset.map_or(i64::MIN, |reset| reset.period_start(now))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Unusable {
    Revoked,
    Disabled,
    Expired,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApiKeyError {
    NotFound,
    Revoked,
    TooMany,
}

/// A key's spend: ever, and since the start of today, this week and this
/// month (UTC).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApiKeyUsage {
    pub total: Money,
    pub daily: Money,
    pub weekly: Money,
    pub monthly: Money,
}

const API_KEY_COLUMNS: &str = "id, pubkey, label, name, disabled, spend_limit, limit_reset, created_at, updated_at, expires_at, revoked_at";

fn api_key_row(row: &rusqlite::Row) -> rusqlite::Result<ApiKey> {
    Ok(ApiKey {
        id: row.get(0)?,
        pubkey: row.get(1)?,
        label: row.get(2)?,
        name: row.get(3)?,
        disabled: row.get(4)?,
        limit: row.get::<_, Option<i64>>(5)?.map(Money),
        limit_reset: row.get::<_, Option<String>>(6)?.as_deref().and_then(LimitReset::parse),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        expires_at: row.get(9)?,
        revoked_at: row.get(10)?,
    })
}

pub fn hash_api_key(key: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub struct NewApiKey {
    pub name: String,
    pub limit: Option<Money>,
    pub limit_reset: Option<LimitReset>,
    pub expires_at: Option<i64>,
}

/// Make a key for `pubkey`: the key as stored, and the secret itself, which
/// is never seen again.
pub fn create_api_key(conn: &mut Connection, pubkey: &str, new: NewApiKey, now: i64) -> Result<Result<(ApiKey, String), ApiKeyError>> {
    let tx = write_tx(conn)?;
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM api_keys WHERE pubkey = ? AND revoked_at IS NULL", [pubkey], |r| r.get(0))?;
    if count >= MAX_API_KEYS {
        return Ok(Err(ApiKeyError::TooMany));
    }
    let secret = random_hex(64);
    let token = format!("{API_KEY_PREFIX}{secret}");
    let key = ApiKey {
        id: format!("key_{}", random_hex(24)),
        pubkey: pubkey.to_string(),
        label: format!("{API_KEY_PREFIX}{}...{}", &secret[..4], &secret[secret.len() - 4..]),
        name: new.name,
        disabled: false,
        limit: new.limit,
        limit_reset: new.limit_reset,
        created_at: now,
        updated_at: now,
        expires_at: new.expires_at,
        revoked_at: None,
    };
    tx.execute(
        "INSERT INTO api_keys (id, pubkey, hash, label, name, spend_limit, limit_reset, created_at, updated_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            key.id,
            key.pubkey,
            hash_api_key(&token),
            key.label,
            key.name,
            key.limit.map(Money::nanos),
            key.limit_reset.map(LimitReset::as_str),
            now,
            now,
            key.expires_at
        ],
    )?;
    tx.commit()?;
    Ok(Ok((key, token)))
}

/// The key a bearer token is, whatever its state.
pub fn api_key_by_token(conn: &Connection, token: &str) -> Result<Option<ApiKey>> {
    if !token.starts_with(API_KEY_PREFIX) {
        return Ok(None);
    }
    let sql = format!("SELECT {API_KEY_COLUMNS} FROM api_keys WHERE hash = ?");
    Ok(conn.query_row(&sql, [hash_api_key(token)], api_key_row).optional()?)
}

pub fn get_api_key(conn: &Connection, pubkey: &str, id: &str) -> Result<Option<ApiKey>> {
    let sql = format!("SELECT {API_KEY_COLUMNS} FROM api_keys WHERE id = ? AND pubkey = ?");
    Ok(conn.query_row(&sql, [id, pubkey], api_key_row).optional()?)
}

pub fn list_api_keys(conn: &Connection, pubkey: &str, limit: usize, after: Option<&str>) -> Result<(Vec<ApiKey>, bool)> {
    let cursor = after.map(|id| cursor_of(conn, "api_keys", "pubkey", pubkey, id)).transpose()?.flatten();
    let (after_time, after_id) = cursor.unzip();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {API_KEY_COLUMNS} FROM api_keys WHERE pubkey = ?1 AND (?2 IS NULL OR (created_at, id) < (?2, ?3))
         ORDER BY created_at DESC, id DESC LIMIT ?4"
    ))?;
    let rows = stmt.query_map(params![pubkey, after_time, after_id, limit as i64 + 1], api_key_row)?;
    page(rows.collect::<rusqlite::Result<Vec<_>>>()?, limit)
}

/// Changes to a key; `None` leaves a field as it is.
#[derive(Default)]
pub struct ApiKeyChanges {
    pub name: Option<String>,
    pub disabled: Option<bool>,
    pub limit: Option<Option<Money>>,
    pub limit_reset: Option<Option<LimitReset>>,
    pub expires_at: Option<Option<i64>>,
}

pub fn update_api_key(conn: &mut Connection, pubkey: &str, id: &str, changes: ApiKeyChanges, now: i64) -> Result<Result<ApiKey, ApiKeyError>> {
    let tx = write_tx(conn)?;
    let Some(mut key) = get_api_key(&tx, pubkey, id)? else {
        return Ok(Err(ApiKeyError::NotFound));
    };
    if key.revoked_at.is_some() {
        return Ok(Err(ApiKeyError::Revoked));
    }
    if let Some(name) = changes.name {
        key.name = name;
    }
    if let Some(disabled) = changes.disabled {
        key.disabled = disabled;
    }
    if let Some(limit) = changes.limit {
        key.limit = limit;
    }
    if let Some(reset) = changes.limit_reset {
        key.limit_reset = reset;
    }
    if let Some(expires_at) = changes.expires_at {
        key.expires_at = expires_at;
    }
    key.updated_at = now;
    tx.execute(
        "UPDATE api_keys SET name = ?, disabled = ?, spend_limit = ?, limit_reset = ?, expires_at = ?, updated_at = ? WHERE id = ?",
        params![key.name, key.disabled, key.limit.map(Money::nanos), key.limit_reset.map(LimitReset::as_str), key.expires_at, now, key.id],
    )?;
    tx.commit()?;
    Ok(Ok(key))
}

/// Revoke a key for good. Revoking one already revoked changes nothing.
pub fn revoke_api_key(conn: &Connection, pubkey: &str, id: &str, now: i64) -> Result<Option<ApiKey>> {
    conn.execute("UPDATE api_keys SET revoked_at = ?1, updated_at = ?1 WHERE id = ?2 AND pubkey = ?3 AND revoked_at IS NULL", params![now, id, pubkey])?;
    get_api_key(conn, pubkey, id)
}

/// What a key has spent since `since`.
pub fn api_key_spent(conn: &Connection, id: &str, since: i64) -> Result<Money> {
    let nanos: i64 =
        conn.query_row("SELECT COALESCE(SUM(cost), 0) FROM generations WHERE api_key_id = ? AND created_at >= ?", params![id, since], |r| r.get(0))?;
    Ok(Money(nanos))
}

pub fn api_key_usage(conn: &Connection, id: &str, now: i64) -> Result<ApiKeyUsage> {
    let since = [LimitReset::Daily, LimitReset::Weekly, LimitReset::Monthly].map(|r| r.period_start(now));
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(cost), 0),
                COALESCE(SUM(CASE WHEN created_at >= ?2 THEN cost END), 0),
                COALESCE(SUM(CASE WHEN created_at >= ?3 THEN cost END), 0),
                COALESCE(SUM(CASE WHEN created_at >= ?4 THEN cost END), 0)
         FROM generations WHERE api_key_id = ?1",
        params![id, since[0], since[1], since[2]],
        |r| Ok(ApiKeyUsage { total: Money(r.get(0)?), daily: Money(r.get(1)?), weekly: Money(r.get(2)?), monthly: Money(r.get(3)?) }),
    )?)
}

/// Where a page after row `id` starts: its `(created_at, id)`. Sorting by
/// both, rather than by time alone, keeps rows made in the same second from
/// being skipped. An unknown id starts from the top, as it always has.
fn cursor_of(conn: &Connection, table: &str, owner: &str, pubkey: &str, id: &str) -> Result<Option<(i64, String)>> {
    Ok(conn
        .query_row(&format!("SELECT created_at, id FROM {table} WHERE id = ? AND {owner} = ?"), [id, pubkey], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?)
}

fn page<T>(mut rows: Vec<T>, limit: usize) -> Result<(Vec<T>, bool)> {
    let more = rows.len() > limit;
    rows.truncate(limit);
    Ok((rows, more))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::temp_db;

    const ALICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BOB: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DOLLAR: Money = Money(1_000_000_000);

    fn pay(conn: &mut Connection, id: &str, amount: Money) {
        insert_payment(
            conn,
            &NewPayment {
                id,
                pubkey: ALICE,
                method: Method::Lightning,
                amount,
                fee: Money::ZERO,
                url: "lnbc1",
                provider_ref: None,
                created_at: 1,
                expires_at: 2,
            },
        )
        .unwrap();
    }

    #[test]
    fn credits_a_payment_once() {
        let t = temp_db();
        t.db.with(|conn| {
            pay(conn, "pi_1", DOLLAR.checked_mul(10).unwrap());
            expire_payment(conn, "pi_1")?;
            assert!(complete_payment(conn, "pi_1", 5)?);
            assert!(!complete_payment(conn, "pi_1", 6)?);
            assert_eq!(balance(conn, ALICE)?, DOLLAR.checked_mul(10).unwrap());
            assert_eq!(get_payment(conn, "pi_1")?.unwrap().status, "completed");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn watches_bitcoin_payments_until_their_grace_is_up() {
        let t = temp_db();
        t.db.with(|conn| {
            for (i, (id, expires_at)) in [("pi_1", 100), ("pi_2", 100), ("pi_3", 10)].into_iter().enumerate() {
                let address = format!("bc1q{i}");
                let payment = NewPayment {
                    id,
                    pubkey: ALICE,
                    method: Method::Bitcoin,
                    amount: DOLLAR,
                    fee: Money::ZERO,
                    url: "bitcoin:x",
                    provider_ref: Some(&address),
                    created_at: 1,
                    expires_at,
                };
                insert_payment(conn, &payment)?;
                insert_bitcoin_address(conn, id, "xpub1", next_bitcoin_index(conn, "xpub1")?, &address, 1000)?;
            }
            assert_eq!(next_bitcoin_index(conn, "xpub1")?, 3);
            assert_eq!(next_bitcoin_index(conn, "xpub2")?, 0);
            // The same index can't be given out twice.
            assert!(insert_bitcoin_address(conn, "pi_1", "xpub1", 0, "bc1qother", 1).is_err());

            expire_payment(conn, "pi_2")?;
            expire_payment(conn, "pi_3")?;
            let ids = |now| -> Result<Vec<String>> { Ok(bitcoin_to_watch(conn, now, 50)?.into_iter().map(|(p, _)| p.id).collect()) };
            assert_eq!(ids(100)?, ["pi_1", "pi_2"]);
            assert_eq!(ids(200)?, ["pi_1"]);
            assert_eq!(bitcoin_to_watch(conn, 0, 50)?[0].1, 1000);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn moves_gift_card_money_once() {
        let t = temp_db();
        t.db.with(|conn| {
            pay(conn, "pi_1", DOLLAR.checked_mul(12).unwrap());
            complete_payment(conn, "pi_1", 1)?;

            let five = DOLLAR.checked_mul(5).unwrap();
            assert_eq!(
                create_giftcards(conn, ALICE, five, 3, true, 2)?.unwrap_err(),
                GiftcardError::Insufficient { required: DOLLAR.checked_mul(15).unwrap(), available: DOLLAR.checked_mul(12).unwrap() }
            );
            let cards = create_giftcards(conn, ALICE, five, 2, true, 2)?.unwrap();
            assert_eq!(balance(conn, ALICE)?, DOLLAR.checked_mul(2).unwrap());

            assert_eq!(redeem_giftcard(conn, BOB, &cards[0].code, 3)?, Ok(five));
            assert_eq!(redeem_giftcard(conn, BOB, &cards[0].code, 3)?, Err(GiftcardError::Redeemed));
            assert_eq!(cancel_giftcard(conn, ALICE, &cards[0].id, 3)?, Err(GiftcardError::Redeemed));
            assert_eq!(balance(conn, BOB)?, five);

            assert_eq!(cancel_giftcard(conn, BOB, &cards[1].id, 4)?, Err(GiftcardError::NotFound));
            assert_eq!(cancel_giftcard(conn, ALICE, &cards[1].id, 4)?, Ok(()));
            assert_eq!(cancel_giftcard(conn, ALICE, &cards[1].id, 4)?, Err(GiftcardError::Cancelled));
            assert_eq!(redeem_giftcard(conn, BOB, &cards[1].code, 4)?, Err(GiftcardError::Cancelled));
            assert_eq!(redeem_giftcard(conn, BOB, "NOPE", 4)?, Err(GiftcardError::NotFound));
            assert_eq!(balance(conn, ALICE)?, DOLLAR.checked_mul(7).unwrap());

            // The ledger and the cached balances agree.
            let (ledger, cached): (i64, i64) =
                conn.query_row("SELECT (SELECT SUM(amount) FROM ledger), (SELECT SUM(balance) FROM accounts)", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            assert_eq!(ledger, cached);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn pages_through_rows_made_in_the_same_second() {
        let t = temp_db();
        t.db.with(|conn| {
            for i in 0..5 {
                record_generation(
                    conn,
                    &Generation {
                        id: format!("gen_{i}"),
                        upstream_id: None,
                        pubkey: ALICE.into(),
                        model: "m".into(),
                        created_at: 100,
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        cost: Money(1),
                        estimated: false,
                        api_key_id: None,
                    },
                )?;
            }
            let (first, more) = list_generations(conn, ALICE, 2, None)?;
            assert!(more);
            let (second, _) = list_generations(conn, ALICE, 2, Some(&first[1].id))?;
            let (third, more) = list_generations(conn, ALICE, 2, Some(&second[1].id))?;
            assert!(!more);
            let ids: Vec<_> = first.iter().chain(&second).chain(&third).map(|g| g.id.as_str()).collect();
            assert_eq!(ids, ["gen_4", "gen_3", "gen_2", "gen_1", "gen_0"]);
            assert_eq!(balance(conn, ALICE)?, Money(-5));
            Ok(())
        })
        .unwrap();
    }
}
