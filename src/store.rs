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
    Monero,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Stripe => "stripe",
            Method::Lightning => "lightning",
            Method::Bitcoin => "bitcoin",
            Method::Monero => "monero",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "stripe" => Some(Method::Stripe),
            "lightning" => Some(Method::Lightning),
            "bitcoin" => Some(Method::Bitcoin),
            "monero" => Some(Method::Monero),
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
    /// A bitcoin payment's sats: what it asked for, and what's been seen
    /// sent to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitcoin: Option<BitcoinSats>,
    /// A monero payment's piconero: what it asked for, and what's been seen
    /// sent to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monero: Option<MoneroPiconero>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct BitcoinSats {
    /// What it asked for, at the rate when it was made.
    pub sats: u64,
    /// What's been sent, the mempool included, at the last check.
    pub received: u64,
    /// Credit its account may already spend, while what was sent confirms.
    #[serde(skip_serializing_if = "is_zero")]
    pub advanced: Money,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MoneroPiconero {
    /// What it asked for, at the rate when it was made.
    pub piconero: u64,
    /// What's been sent, the mempool included, at the last check.
    pub received: u64,
}

fn is_zero(money: &Money) -> bool {
    *money == Money::ZERO
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

/// A payment's columns, read by [`payment_row`] from [`PAYMENTS`]: as many
/// as [`PAYMENT_COLUMN_COUNT`], so a query can read more after them.
const PAYMENT_COLUMNS: &str =
    "p.id, p.pubkey, p.method, p.status, p.amount, p.fee, p.url, p.provider_ref, p.created_at, p.completed_at, p.expires_at, b.sats, b.seen, b.advanced, m.piconero, m.seen";
const PAYMENT_COLUMN_COUNT: usize = 16;

/// Payments, with a bitcoin or monero payment's address. (Only `payments`
/// has the columns queries name unqualified: `id`, `pubkey`, `method`,
/// `status`, `created_at`.)
const PAYMENTS: &str = "payments p LEFT JOIN bitcoin_addresses b ON b.payment_id = p.id LEFT JOIN monero_addresses m ON m.payment_id = p.id";

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
            Method::Monero => "Credit purchase via Monero".into(),
        },
        url: row.get(6)?,
        provider_ref: row.get(7)?,
        created_at: row.get(8)?,
        completed_at: row.get(9)?,
        expires_at: row.get(10)?,
        bitcoin: match row.get::<_, Option<i64>>(11)? {
            Some(sats) => Some(BitcoinSats { sats: sats as u64, received: row.get::<_, i64>(12)? as u64, advanced: Money(row.get(13)?) }),
            None => None,
        },
        monero: match row.get::<_, Option<i64>>(14)? {
            Some(piconero) => Some(MoneroPiconero { piconero: piconero as u64, received: row.get::<_, i64>(15)? as u64 }),
            None => None,
        },
    })
}

pub fn get_payment(conn: &Connection, id: &str) -> Result<Option<Payment>> {
    Ok(conn.query_row(&format!("SELECT {PAYMENT_COLUMNS} FROM {PAYMENTS} WHERE p.id = ?"), [id], payment_row).optional()?)
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
        "SELECT {PAYMENT_COLUMNS} FROM {PAYMENTS}
         WHERE pubkey = ?1 AND (?2 IS NULL OR (created_at, id) < (?2, ?3)) AND (?4 IS NULL OR status = ?4) AND (?5 IS NULL OR method = ?5)
         ORDER BY created_at DESC, id DESC LIMIT ?6"
    ))?;
    let rows = stmt.query_map(params![pubkey, after_time, after_id, status, method, limit as i64 + 1], payment_row)?;
    page(rows.collect::<rusqlite::Result<Vec<_>>>()?, limit)
}

/// How many payments an account has started since `since`.
pub fn payments_started(conn: &Connection, pubkey: &str, since: i64) -> Result<u32> {
    Ok(conn.query_row("SELECT COUNT(*) FROM payments WHERE pubkey = ? AND created_at >= ?", params![pubkey, since], |r| r.get(0))?)
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
    let mut stmt = conn.prepare_cached(&format!("SELECT {PAYMENT_COLUMNS} FROM {PAYMENTS} WHERE status = 'pending' AND method = 'lightning'"))?;
    let rows = stmt.query_map([], payment_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Take the next unused receive address index for an account key. Call it in
/// the transaction that records the address, so a rollback gives it back.
pub fn take_bitcoin_index(conn: &Connection, xpub: &str) -> Result<u32> {
    // An account key first seen here starts past any address already
    // recorded for it.
    let index: i64 = conn.query_row(
        "INSERT INTO bitcoin_accounts (xpub, next_idx) VALUES (?1, (SELECT COALESCE(MAX(idx) + 1, 0) FROM bitcoin_addresses WHERE xpub = ?1) + 1)
         ON CONFLICT (xpub) DO UPDATE SET next_idx = next_idx + 1
         RETURNING next_idx - 1",
        [xpub],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(index)?)
}

pub fn insert_bitcoin_address(conn: &Connection, payment_id: &str, xpub: &str, index: u32, address: &str, sats: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO bitcoin_addresses (payment_id, xpub, idx, address, sats) VALUES (?, ?, ?, ?, ?)",
        params![payment_id, xpub, index, address, i64::try_from(sats)?],
    )?;
    Ok(())
}

/// A watched bitcoin address: what it's owed, and what it had when last
/// checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BitcoinWatch {
    pub sats: u64,
    pub seen: u64,
    pub checked_at: Option<i64>,
    pub checked_height: Option<u64>,
    /// Credit advanced on it before it confirmed.
    pub advanced: Money,
}

/// Bitcoin payments to look in on, with each address's state: those
/// pending, and those that expired within `grace` seconds, in case they're
/// paid late. Everyone's, or one account's.
pub fn bitcoin_to_watch(conn: &Connection, pubkey: Option<&str>, now: i64, grace: i64) -> Result<Vec<(Payment, BitcoinWatch)>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {PAYMENT_COLUMNS}, b.checked_at, b.checked_height FROM {PAYMENTS}
         WHERE p.method = 'bitcoin' AND (p.status = 'pending' OR (p.status = 'expired' AND p.expires_at > ?1)) AND (?2 IS NULL OR p.pubkey = ?2)
         ORDER BY b.xpub, b.idx"
    ))?;
    let rows = stmt.query_map(params![now - grace, pubkey], watch_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One bitcoin payment, with its address's state.
pub fn bitcoin_watch(conn: &Connection, id: &str) -> Result<Option<(Payment, BitcoinWatch)>> {
    let sql = format!("SELECT {PAYMENT_COLUMNS}, b.checked_at, b.checked_height FROM {PAYMENTS} WHERE p.id = ? AND b.payment_id IS NOT NULL");
    Ok(conn.query_row(&sql, [id], watch_row).optional()?)
}

fn watch_row(r: &rusqlite::Row) -> rusqlite::Result<(Payment, BitcoinWatch)> {
    let watch = BitcoinWatch {
        sats: r.get::<_, i64>(11)? as u64,
        seen: r.get::<_, i64>(12)? as u64,
        advanced: Money(r.get(13)?),
        checked_at: r.get(PAYMENT_COLUMN_COUNT)?,
        checked_height: r.get::<_, Option<i64>>(PAYMENT_COLUMN_COUNT + 1)?.map(|h| h as u64),
    };
    Ok((payment_row(r)?, watch))
}

/// Note what an address had at a check, and when.
pub fn record_bitcoin_check(conn: &Connection, payment_id: &str, seen: u64, now: i64, height: u64) -> Result<()> {
    conn.execute(
        "UPDATE bitcoin_addresses SET seen = ?2, checked_at = ?3, checked_height = ?4 WHERE payment_id = ?1",
        params![payment_id, i64::try_from(seen)?, now, i64::try_from(height)?],
    )?;
    Ok(())
}

/// Complete a bitcoin payment for what was actually paid: its credits and
/// fee become `amount` and `fee`, and `paid` sats are recorded. Credits move
/// once, as for any payment, and replace whatever was advanced on it.
pub fn complete_bitcoin_payment(conn: &mut Connection, id: &str, now: i64, amount: Money, fee: Money, paid: u64) -> Result<bool> {
    let tx = write_tx(conn)?;
    let pubkey: Option<String> = tx
        .query_row(
            "UPDATE payments SET status = 'completed', completed_at = ?2, amount = ?3, fee = ?4
             WHERE id = ?1 AND method = 'bitcoin' AND status IN ('pending', 'expired') RETURNING pubkey",
            params![id, now, amount.nanos(), fee.nanos()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(pubkey) = pubkey else {
        return Ok(false);
    };
    tx.execute("UPDATE bitcoin_addresses SET paid = ?2, advanced = 0 WHERE payment_id = ?1", params![id, i64::try_from(paid)?])?;
    post(&tx, &pubkey, amount, Kind::Payment, id, now)?;
    tx.commit()?;
    Ok(true)
}

/// Payments by `method` still open to be paid, by one account or by
/// everyone.
pub fn open_payments(conn: &Connection, method: Method, pubkey: Option<&str>, now: i64) -> Result<u32> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM payments WHERE method = ?3 AND status = 'pending' AND expires_at > ?1 AND (?2 IS NULL OR pubkey = ?2)",
        params![now, pubkey, method.as_str()],
        |r| r.get(0),
    )?)
}

// Monero.

/// The next subaddress index to give out for a wallet, named by its primary
/// address: 1 for one never seen, since 0 is the primary address.
pub fn next_monero_index(conn: &Connection, account: &str) -> Result<u32> {
    let next: Option<i64> = conn.query_row("SELECT next_idx FROM monero_accounts WHERE account = ?", [account], |r| r.get(0)).optional()?;
    Ok(u32::try_from(next.unwrap_or(1))?)
}

/// Record the subaddress a monero payment is paid to, and that its index is
/// taken. Indexes come from the wallet, which hands them out in order; this
/// keeps count too, so one made again from its keys, starting over, can be
/// moved on past those already given out (see [`next_monero_index`]).
pub fn insert_monero_address(conn: &Connection, payment_id: &str, account: &str, index: u32, address: &str, piconero: u64) -> Result<()> {
    conn.execute(
        "INSERT INTO monero_addresses (payment_id, account, idx, address, piconero) VALUES (?, ?, ?, ?, ?)",
        params![payment_id, account, index, address, i64::try_from(piconero)?],
    )?;
    conn.execute(
        "INSERT INTO monero_accounts (account, next_idx) VALUES (?1, ?2) ON CONFLICT (account) DO UPDATE SET next_idx = MAX(next_idx, ?2)",
        params![account, i64::from(index) + 1],
    )?;
    Ok(())
}

/// A watched monero subaddress: whose and which it is, what it's owed, and
/// what it had when last checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MoneroWatch {
    pub account: String,
    pub index: u32,
    pub piconero: u64,
    pub seen: u64,
    pub checked_at: Option<i64>,
}

const MONERO_WATCH_COLUMNS: &str = "m.account, m.idx, m.checked_at";

/// Monero payments to look in on, with each subaddress's state: those
/// pending, and those that expired within `grace` seconds, in case they're
/// paid late. Everyone's, or one account's.
pub fn monero_to_watch(conn: &Connection, pubkey: Option<&str>, now: i64, grace: i64) -> Result<Vec<(Payment, MoneroWatch)>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {PAYMENT_COLUMNS}, {MONERO_WATCH_COLUMNS} FROM {PAYMENTS}
         WHERE p.method = 'monero' AND m.payment_id IS NOT NULL
         AND (p.status = 'pending' OR (p.status = 'expired' AND p.expires_at > ?1)) AND (?2 IS NULL OR p.pubkey = ?2)
         ORDER BY m.account, m.idx"
    ))?;
    let rows = stmt.query_map(params![now - grace, pubkey], monero_watch_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// One monero payment, with its subaddress's state.
pub fn monero_watch(conn: &Connection, id: &str) -> Result<Option<(Payment, MoneroWatch)>> {
    let sql = format!("SELECT {PAYMENT_COLUMNS}, {MONERO_WATCH_COLUMNS} FROM {PAYMENTS} WHERE p.id = ? AND m.payment_id IS NOT NULL");
    Ok(conn.query_row(&sql, [id], monero_watch_row).optional()?)
}

fn monero_watch_row(r: &rusqlite::Row) -> rusqlite::Result<(Payment, MoneroWatch)> {
    let payment = payment_row(r)?;
    let amounts = payment.monero.unwrap_or_default();
    let watch = MoneroWatch {
        account: r.get(PAYMENT_COLUMN_COUNT)?,
        index: r.get(PAYMENT_COLUMN_COUNT + 1)?,
        piconero: amounts.piconero,
        seen: amounts.received,
        checked_at: r.get(PAYMENT_COLUMN_COUNT + 2)?,
    };
    Ok((payment, watch))
}

/// Note what a subaddress had at a check, and when.
pub fn record_monero_check(conn: &Connection, payment_id: &str, seen: u64, now: i64) -> Result<()> {
    conn.execute("UPDATE monero_addresses SET seen = ?2, checked_at = ?3 WHERE payment_id = ?1", params![payment_id, i64::try_from(seen)?, now])?;
    Ok(())
}

/// Complete a monero payment for what was actually paid: its credits and fee
/// become `amount` and `fee`, and `paid` piconero are recorded. Credits move
/// once, as for any payment.
pub fn complete_monero_payment(conn: &mut Connection, id: &str, now: i64, amount: Money, fee: Money, paid: u64) -> Result<bool> {
    let tx = write_tx(conn)?;
    let pubkey: Option<String> = tx
        .query_row(
            "UPDATE payments SET status = 'completed', completed_at = ?2, amount = ?3, fee = ?4
             WHERE id = ?1 AND method = 'monero' AND status IN ('pending', 'expired') RETURNING pubkey",
            params![id, now, amount.nanos(), fee.nanos()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(pubkey) = pubkey else {
        return Ok(false);
    };
    tx.execute("UPDATE monero_addresses SET paid = ?2 WHERE payment_id = ?1", params![id, i64::try_from(paid)?])?;
    post(&tx, &pubkey, amount, Kind::Payment, id, now)?;
    tx.commit()?;
    Ok(true)
}

// Advances: credit before confirmation.

/// How much may be advanced (see `config::Trust`).
#[derive(Clone, Copy, Debug)]
pub struct AdvanceLimits {
    pub max_advance: Money,
    pub ratio: rust_decimal::Decimal,
    pub max_total: Money,
}

/// What's advanced on bitcoin payments, one account's or everyone's, but
/// for the payment `except`.
fn advanced(conn: &Connection, pubkey: Option<&str>, except: &str) -> Result<Money> {
    Ok(Money(conn.query_row(
        "SELECT COALESCE(SUM(b.advanced), 0) FROM bitcoin_addresses b JOIN payments p ON p.id = b.payment_id
         WHERE b.advanced > 0 AND b.payment_id != ?2 AND (?1 IS NULL OR p.pubkey = ?1)",
        params![pubkey, except],
        |r| r.get(0),
    )?))
}

/// What an account may spend now on its payments that haven't confirmed.
pub fn advances(conn: &Connection, pubkey: &str) -> Result<Money> {
    advanced(conn, Some(pubkey), "")
}

/// Credit an account has bought that can't be taken back: completed bitcoin,
/// monero and Lightning payments. (A card payment can be charged back.)
fn final_purchases(conn: &Connection, pubkey: &str) -> Result<Money> {
    Ok(Money(conn.query_row(
        "SELECT COALESCE(SUM(amount), 0) FROM payments WHERE pubkey = ? AND status = 'completed' AND method IN ('bitcoin', 'monero', 'lightning')",
        [pubkey],
        |r| r.get(0),
    )?))
}

fn is_distrusted(conn: &Connection, pubkey: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT 1 FROM distrusted WHERE pubkey = ?", [pubkey], |_| Ok(())).optional()?.is_some())
}

/// The most an account may have advanced at once, on all its payments: a
/// share of what it's paid for before, capped, or nothing for an account
/// that's never paid, or whose advance once wasn't paid.
pub fn advance_allowance(conn: &Connection, pubkey: &str, limits: &AdvanceLimits) -> Result<Money> {
    if is_distrusted(conn, pubkey)? {
        return Ok(Money::ZERO);
    }
    let share = Money::from_dollars(final_purchases(conn, pubkey)?.to_decimal() * limits.ratio).unwrap_or(Money::ZERO);
    Ok(share.min(limits.max_advance).max(Money::ZERO))
}

/// Set what's advanced on a bitcoin payment that isn't yet credited to as
/// much of `want` as its account's trust allows, first distrusting the
/// account for good if `distrust`. What was advanced before, and now.
///
/// It's one write transaction, so payments checked at once can't each take
/// the same room: each counts every other's advance, so an account's, and
/// everyone's, never total more than the limits.
pub fn set_bitcoin_advance(conn: &mut Connection, id: &str, want: Money, distrust: bool, limits: &AdvanceLimits, now: i64) -> Result<(Money, Money)> {
    let tx = write_tx(conn)?;
    let row: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT p.pubkey, p.status, b.advanced FROM payments p JOIN bitcoin_addresses b ON b.payment_id = p.id WHERE p.id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((pubkey, status, before)) = row else {
        return Ok((Money::ZERO, Money::ZERO));
    };
    let before = Money(before);
    if status == "completed" {
        return Ok((before, before));
    }
    if distrust {
        tx.execute("INSERT INTO distrusted (pubkey, payment_id, created_at) VALUES (?, ?, ?) ON CONFLICT (pubkey) DO NOTHING", params![pubkey, id, now])?;
    }
    let mine = Money(advance_allowance(&tx, &pubkey, limits)?.nanos() - advanced(&tx, Some(&pubkey), id)?.nanos());
    let everyone = Money(limits.max_total.nanos() - advanced(&tx, None, id)?.nanos());
    let after = want.min(mine).min(everyone).max(Money::ZERO);
    if after != before {
        tx.execute("UPDATE bitcoin_addresses SET advanced = ? WHERE payment_id = ?", params![after.nanos(), id])?;
    }
    tx.commit()?;
    Ok((before, after))
}

/// Take back every advance on a payment uncredited whose watch is up
/// (`expires_at` at or before `before`), and distrust its account. The
/// payments, and what each had advanced.
pub fn lapse_bitcoin_advances(conn: &mut Connection, before: i64, now: i64) -> Result<Vec<(String, Money)>> {
    let tx = write_tx(conn)?;
    let lapsed: Vec<(String, String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT p.id, p.pubkey, b.advanced FROM bitcoin_addresses b JOIN payments p ON p.id = b.payment_id
             WHERE b.advanced > 0 AND p.status != 'completed' AND p.expires_at <= ? ORDER BY p.id",
        )?;
        let rows = stmt.query_map([before], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, pubkey, _) in &lapsed {
        tx.execute("UPDATE bitcoin_addresses SET advanced = 0 WHERE payment_id = ?", [id])?;
        tx.execute("INSERT INTO distrusted (pubkey, payment_id, created_at) VALUES (?, ?, ?) ON CONFLICT (pubkey) DO NOTHING", params![pubkey, id, now])?;
    }
    tx.commit()?;
    Ok(lapsed.into_iter().map(|(id, _, advanced)| (id, Money(advanced))).collect())
}

/// Expire every pending Stripe payment past its time; Stripe's own session
/// expires with it, and a late webhook can still complete it.
pub fn expire_stripe_payments(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute("UPDATE payments SET status = 'expired' WHERE status = 'pending' AND method = 'stripe' AND expires_at <= ?", [now])?)
}

/// Delete payments that expired before `before`, unpaid: nothing was
/// credited for them, and they're no longer watched or payable. A bitcoin or
/// monero payment that was sent something is kept, as the record of where
/// that money went. Their indexes stay taken (see [`take_bitcoin_index`] and
/// [`insert_monero_address`]).
pub fn delete_expired_payments(conn: &mut Connection, before: i64) -> Result<usize> {
    let tx = write_tx(conn)?;
    tx.execute(
        "DELETE FROM bitcoin_addresses WHERE seen = 0 AND paid IS NULL
         AND payment_id IN (SELECT id FROM payments WHERE status = 'expired' AND expires_at < ?)",
        [before],
    )?;
    tx.execute(
        "DELETE FROM monero_addresses WHERE seen = 0 AND paid IS NULL
         AND payment_id IN (SELECT id FROM payments WHERE status = 'expired' AND expires_at < ?)",
        [before],
    )?;
    let deleted = tx.execute(
        "DELETE FROM payments WHERE status = 'expired' AND expires_at < ?
         AND NOT EXISTS (SELECT 1 FROM bitcoin_addresses b WHERE b.payment_id = payments.id)
         AND NOT EXISTS (SELECT 1 FROM monero_addresses m WHERE m.payment_id = payments.id)",
        [before],
    )?;
    tx.commit()?;
    Ok(deleted)
}

// Gift cards.

#[derive(Clone, Debug, Serialize)]
pub struct Giftcard {
    pub id: String,
    pub code: String,
    pub amount: Money,
    pub redeemed: bool,
    pub created_at: i64,
    /// `active`, `redeemed` or `cancelled`: `redeemed` alone can't tell an
    /// unused card from a cancelled one.
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
        // What's held for requests in flight is spoken for: bought with,
        // it would be charged again when they finish, and a card would carry
        // credit its sender no longer has to another account.
        let available = Money(balance(&tx, sender)?.nanos().saturating_sub(held(&tx, sender)?));
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
    /// All of the input, cached or not.
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Of `prompt_tokens`, those read from the provider's cache.
    pub cache_read_tokens: u64,
    /// Of `prompt_tokens`, those written to it.
    pub cache_write_tokens: u64,
    pub cost: Money,
    pub estimated: bool,
    /// The API key it was made with, if it wasn't signed for with NIP-98.
    pub api_key_id: Option<String>,
    /// The upstream that served it, `provider/model`.
    pub upstream: Option<String>,
    /// What it cost us, if the upstream said.
    pub upstream_cost: Option<Money>,
}

pub fn new_generation_id() -> String {
    format!("gen_{}", random_hex(32))
}

/// Record a generation and charge for it, together, giving back the hold
/// that was taken for it, if one was.
pub fn record_generation(conn: &mut Connection, g: &Generation, hold: Option<&str>) -> Result<()> {
    let tx = write_tx(conn)?;
    if let Some(hold) = hold {
        tx.execute("DELETE FROM holds WHERE id = ?", [hold])?;
    }
    tx.execute(
        "INSERT INTO generations (id, upstream_id, pubkey, model, created_at, prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens,
             cost, estimated, api_key_id, upstream, upstream_cost)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            g.id,
            g.upstream_id,
            g.pubkey,
            g.model,
            g.created_at,
            g.prompt_tokens as i64,
            g.completion_tokens as i64,
            g.cache_read_tokens as i64,
            g.cache_write_tokens as i64,
            g.cost.nanos(),
            g.estimated,
            g.api_key_id,
            g.upstream,
            g.upstream_cost.map(Money::nanos)
        ],
    )?;
    if g.cost.is_positive() {
        post(&tx, &g.pubkey, -g.cost, Kind::Generation, &g.id, g.created_at)?;
    }
    tx.commit()?;
    Ok(())
}

// Holds.

/// What's held for an account's requests in flight, in nanodollars.
fn held(conn: &Connection, pubkey: &str) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(SUM(amount), 0) FROM holds WHERE pubkey = ?", [pubkey], |r| r.get(0))?)
}

/// What an account can spend on inference now: its balance, plus what's
/// advanced on its bitcoin payments that haven't confirmed, less what's held
/// for its requests in flight. (Gift cards are bought from the balance less
/// what's held, without advances, so an advance can't be moved to another
/// account.)
pub fn spendable(conn: &Connection, pubkey: &str) -> Result<Money> {
    Ok(Money(balance(conn, pubkey)?.nanos().saturating_add(advances(conn, pubkey)?.nanos()).saturating_sub(held(conn, pubkey)?)))
}

/// Set `amount` aside for a request, if the account can spend that much and
/// more than nothing. The hold's id, or what the account could spend.
///
/// The check and the hold are one write transaction, so requests racing each
/// other can't all pass a check that only one of them should: each sees the
/// holds of those before it.
pub fn place_hold(conn: &mut Connection, pubkey: &str, amount: Money, now: i64) -> Result<Result<String, Money>> {
    let tx = write_tx(conn)?;
    let available = spendable(&tx, pubkey)?;
    if !available.is_positive() || available < amount {
        return Ok(Err(available));
    }
    let id = format!("hold_{}", random_hex(24));
    tx.execute("INSERT INTO holds (id, pubkey, amount, created_at) VALUES (?, ?, ?, ?)", params![id, pubkey, amount.nanos().max(0), now])?;
    tx.commit()?;
    Ok(Ok(id))
}

/// Give back a hold without charging anything. Releasing one already gone
/// does nothing.
pub fn release_hold(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM holds WHERE id = ?", [id])?;
    Ok(())
}

/// Forget every hold: at startup, when no request is in flight.
pub fn clear_holds(conn: &Connection) -> Result<usize> {
    Ok(conn.execute("DELETE FROM holds", [])?)
}

pub fn list_generations(conn: &Connection, pubkey: &str, limit: usize, after: Option<&str>) -> Result<(Vec<Generation>, bool)> {
    let cursor = after.map(|id| cursor_of(conn, "generations", "pubkey", pubkey, id)).transpose()?.flatten();
    let (after_time, after_id) = cursor.unzip();
    let mut stmt = conn.prepare_cached(
        "SELECT id, upstream_id, pubkey, model, created_at, prompt_tokens, completion_tokens, cost, estimated, api_key_id,
             cache_read_tokens, cache_write_tokens, upstream, upstream_cost FROM generations
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
            cache_read_tokens: r.get::<_, i64>(10)? as u64,
            cache_write_tokens: r.get::<_, i64>(11)? as u64,
            upstream: r.get(12)?,
            upstream_cost: r.get::<_, Option<i64>>(13)?.map(Money),
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
                insert_bitcoin_address(conn, id, "xpub1", take_bitcoin_index(conn, "xpub1")?, &address, 1000)?;
            }
            assert_eq!(take_bitcoin_index(conn, "xpub2")?, 0);
            // The same index can't be given out twice.
            assert!(insert_bitcoin_address(conn, "pi_1", "xpub1", 0, "bc1qother", 1).is_err());

            expire_payment(conn, "pi_2")?;
            expire_payment(conn, "pi_3")?;
            let ids = |now| -> Result<Vec<String>> { Ok(bitcoin_to_watch(conn, None, now, 50)?.into_iter().map(|(p, _)| p.id).collect()) };
            assert_eq!(ids(100)?, ["pi_1", "pi_2"]);
            assert_eq!(ids(200)?, ["pi_1"]);
            assert_eq!(bitcoin_to_watch(conn, None, 0, 50)?[0].1, BitcoinWatch { sats: 1000, ..BitcoinWatch::default() });
            record_bitcoin_check(conn, "pi_1", 400, 7, 800_000)?;
            let watch = &bitcoin_to_watch(conn, None, 0, 50)?[0].1;
            assert_eq!((watch.seen, watch.checked_at, watch.checked_height), (400, Some(7), Some(800_000)));
            assert_eq!(get_payment(conn, "pi_1")?.unwrap().bitcoin, Some(BitcoinSats { sats: 1000, received: 400, advanced: Money::ZERO }));
            assert_eq!(bitcoin_watch(conn, "pi_1")?.unwrap().1.seen, 400);
            assert!(bitcoin_to_watch(conn, Some("nobody"), 0, 50)?.is_empty());
            assert_eq!(open_payments(conn, Method::Bitcoin, None, 0)?, 1);
            assert_eq!(open_payments(conn, Method::Bitcoin, Some("nobody"), 0)?, 0);
            assert_eq!(open_payments(conn, Method::Bitcoin, None, 100)?, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn watches_monero_payments_and_never_gives_a_subaddress_out_twice() {
        let t = temp_db();
        t.db.with(|conn| {
            let open = |conn: &mut Connection, id: &str, expires_at: i64, index: u32| -> Result<()> {
                let address = format!("8sub{index}");
                let payment = NewPayment {
                    id,
                    pubkey: ALICE,
                    method: Method::Monero,
                    amount: DOLLAR,
                    fee: Money::ZERO,
                    url: "monero:x",
                    provider_ref: Some(&address),
                    created_at: 1,
                    expires_at,
                };
                insert_payment(conn, &payment)?;
                insert_monero_address(conn, id, "4wallet", index, &address, 1000)
            };
            // Index 0 is the primary address.
            assert_eq!(next_monero_index(conn, "4wallet")?, 1);
            open(conn, "pi_1", 100, 1)?;
            open(conn, "pi_2", 100, 5)?;
            assert_eq!(next_monero_index(conn, "4wallet")?, 6);
            // An earlier index doesn't move the count back.
            open(conn, "pi_3", 10, 3)?;
            assert_eq!(next_monero_index(conn, "4wallet")?, 6);
            assert_eq!(next_monero_index(conn, "4other")?, 1);
            // Nor can one be given out twice.
            assert!(open(conn, "pi_4", 100, 5).is_err());
            conn.execute("DELETE FROM payments WHERE id = 'pi_4'", [])?;

            expire_payment(conn, "pi_2")?;
            expire_payment(conn, "pi_3")?;
            let ids = |now| -> Result<Vec<String>> { Ok(monero_to_watch(conn, None, now, 50)?.into_iter().map(|(p, _)| p.id).collect()) };
            assert_eq!(ids(100)?, ["pi_1", "pi_2"]);
            assert_eq!(ids(200)?, ["pi_1"]);
            let first = MoneroWatch { account: "4wallet".into(), index: 1, piconero: 1000, ..MoneroWatch::default() };
            assert_eq!(monero_to_watch(conn, None, 0, 50)?[0].1, first);
            record_monero_check(conn, "pi_1", 400, 7)?;
            assert_eq!(monero_watch(conn, "pi_1")?.unwrap().1, MoneroWatch { seen: 400, checked_at: Some(7), ..first });
            assert_eq!(get_payment(conn, "pi_1")?.unwrap().monero, Some(MoneroPiconero { piconero: 1000, received: 400 }));
            assert_eq!(get_payment(conn, "pi_1")?.unwrap().bitcoin, None);
            assert!(monero_to_watch(conn, Some("nobody"), 0, 50)?.is_empty());
            assert_eq!(open_payments(conn, Method::Monero, None, 0)?, 1);
            assert_eq!(open_payments(conn, Method::Bitcoin, None, 0)?, 0);

            // Credited once, for what was paid; then it counts as final.
            assert!(complete_monero_payment(conn, "pi_1", 8, Money(500_000_000), Money::ZERO, 500)?);
            assert!(!complete_monero_payment(conn, "pi_1", 9, Money(500_000_000), Money::ZERO, 500)?);
            assert_eq!(balance(conn, ALICE)?, Money(500_000_000));
            assert_eq!(final_purchases(conn, ALICE)?, Money(500_000_000));

            // Long expired and never sent anything: deleted, its index still taken.
            assert_eq!(delete_expired_payments(conn, 1000)?, 2);
            assert!(get_payment(conn, "pi_2")?.is_none());
            assert_eq!(next_monero_index(conn, "4wallet")?, 6);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn advances_only_what_trust_allows() {
        let t = temp_db();
        t.db.with(|conn| {
            let bought = |conn: &mut Connection, id: &str, pubkey: &str, method: Method, dollars: i64| -> Result<()> {
                let payment = NewPayment {
                    id,
                    pubkey,
                    method,
                    amount: DOLLAR.checked_mul(dollars).unwrap(),
                    fee: Money::ZERO,
                    url: "x",
                    provider_ref: None,
                    created_at: 1,
                    expires_at: 100,
                };
                insert_payment(conn, &payment)?;
                complete_payment(conn, id, 2)?;
                Ok(())
            };
            let open = |conn: &mut Connection, id: &str, pubkey: &str| -> Result<()> {
                let address = format!("bc1q{id}");
                let payment = NewPayment {
                    id,
                    pubkey,
                    method: Method::Bitcoin,
                    amount: DOLLAR.checked_mul(10).unwrap(),
                    fee: Money::ZERO,
                    url: "bitcoin:x",
                    provider_ref: Some(&address),
                    created_at: 1,
                    expires_at: 100,
                };
                insert_payment(conn, &payment)?;
                insert_bitcoin_address(conn, id, "xpub1", take_bitcoin_index(conn, "xpub1")?, &address, 1000)?;
                Ok(())
            };
            let limits = AdvanceLimits { max_advance: DOLLAR.checked_mul(50).unwrap(), ratio: rust_decimal::Decimal::new(5, 1), max_total: DOLLAR.checked_mul(20).unwrap() };
            let ten = DOLLAR.checked_mul(10).unwrap();
            let dollars = |n: i64| DOLLAR.checked_mul(n).unwrap();

            // Never paid: nothing.
            open(conn, "pi_a1", ALICE)?;
            open(conn, "pi_a2", ALICE)?;
            assert_eq!(set_bitcoin_advance(conn, "pi_a1", ten, false, &limits, 3)?, (Money::ZERO, Money::ZERO));
            // A card payment can be charged back, so it doesn't count.
            bought(conn, "pi_card", ALICE, Method::Stripe, 100)?;
            assert_eq!(advance_allowance(conn, ALICE, &limits)?, Money::ZERO);
            // Half of what's been paid by Lightning or bitcoin, across every
            // open payment.
            bought(conn, "pi_ln", ALICE, Method::Lightning, 30)?;
            assert_eq!(advance_allowance(conn, ALICE, &limits)?, dollars(15));
            assert_eq!(set_bitcoin_advance(conn, "pi_a1", ten, false, &limits, 3)?, (Money::ZERO, ten));
            assert_eq!(set_bitcoin_advance(conn, "pi_a2", ten, false, &limits, 3)?, (Money::ZERO, dollars(5)));
            assert_eq!(advances(conn, ALICE)?, dollars(15));
            assert_eq!(spendable(conn, ALICE)?, dollars(145));
            // Asking again for the same is no more.
            assert_eq!(set_bitcoin_advance(conn, "pi_a1", ten, false, &limits, 3)?, (ten, ten));

            // Never more than `max_total`, everyone's together.
            bought(conn, "pi_bob", BOB, Method::Lightning, 1000)?;
            assert_eq!(advance_allowance(conn, BOB, &limits)?, dollars(50));
            open(conn, "pi_b1", BOB)?;
            assert_eq!(set_bitcoin_advance(conn, "pi_b1", ten, false, &limits, 3)?, (Money::ZERO, dollars(5)));

            // Credited: the advance gives way to the credit.
            assert!(complete_bitcoin_payment(conn, "pi_a1", 4, ten, Money::ZERO, 1000)?);
            assert_eq!(advances(conn, ALICE)?, dollars(5));
            assert_eq!(get_payment(conn, "pi_a1")?.unwrap().bitcoin.unwrap().advanced, Money::ZERO);
            assert_eq!(set_bitcoin_advance(conn, "pi_a1", ten, false, &limits, 5)?, (Money::ZERO, Money::ZERO));
            assert_eq!(spendable(conn, ALICE)?, dollars(145));

            // Taken back from a payment no longer watched, for good.
            assert_eq!(lapse_bitcoin_advances(conn, 99, 6)?, []);
            assert_eq!(lapse_bitcoin_advances(conn, 100, 6)?, [("pi_a2".to_string(), dollars(5)), ("pi_b1".to_string(), dollars(5))]);
            assert_eq!(advances(conn, ALICE)?, Money::ZERO);
            open(conn, "pi_a3", ALICE)?;
            assert_eq!(set_bitcoin_advance(conn, "pi_a3", ten, false, &limits, 7)?, (Money::ZERO, Money::ZERO));
            assert_eq!(advance_allowance(conn, BOB, &limits)?, Money::ZERO);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn distrusts_an_account_whose_advance_went_away() {
        let t = temp_db();
        t.db.with(|conn| {
            pay(conn, "pi_ln", DOLLAR.checked_mul(100).unwrap());
            complete_payment(conn, "pi_ln", 1)?;
            let payment =
                NewPayment { id: "pi_btc", pubkey: ALICE, method: Method::Bitcoin, amount: DOLLAR, fee: Money::ZERO, url: "x", provider_ref: Some("bc1q"), created_at: 1, expires_at: 100 };
            insert_payment(conn, &payment)?;
            insert_bitcoin_address(conn, "pi_btc", "xpub1", 0, "bc1q", 1000)?;
            let limits = AdvanceLimits { max_advance: DOLLAR.checked_mul(50).unwrap(), ratio: rust_decimal::Decimal::ONE, max_total: DOLLAR.checked_mul(500).unwrap() };
            assert_eq!(set_bitcoin_advance(conn, "pi_btc", DOLLAR, false, &limits, 2)?, (Money::ZERO, DOLLAR));
            assert_eq!(set_bitcoin_advance(conn, "pi_btc", DOLLAR, true, &limits, 3)?, (DOLLAR, Money::ZERO));
            assert_eq!(advance_allowance(conn, ALICE, &limits)?, Money::ZERO);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn deletes_long_expired_unpaid_payments() {
        let t = temp_db();
        t.db.with(|conn| {
            pay(conn, "pi_paid", DOLLAR);
            complete_payment(conn, "pi_paid", 1)?;
            pay(conn, "pi_open", DOLLAR);
            pay(conn, "pi_expired", DOLLAR);
            expire_payment(conn, "pi_expired")?;
            for (i, id) in ["pi_btc_sent", "pi_btc"].into_iter().enumerate() {
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
                    expires_at: 2,
                };
                insert_payment(conn, &payment)?;
                insert_bitcoin_address(conn, id, "xpub1", take_bitcoin_index(conn, "xpub1")?, &address, 1000)?;
                expire_payment(conn, id)?;
            }
            record_bitcoin_check(conn, "pi_btc_sent", 400, 3, 800_000)?;

            // Not yet long enough past.
            assert_eq!(delete_expired_payments(conn, 2)?, 0);
            assert_eq!(delete_expired_payments(conn, 3)?, 2);
            assert!(get_payment(conn, "pi_expired")?.is_none());
            assert!(get_payment(conn, "pi_btc")?.is_none());
            for id in ["pi_paid", "pi_open", "pi_btc_sent"] {
                assert!(get_payment(conn, id)?.is_some(), "{id}");
            }
            assert_eq!(balance(conn, ALICE)?, DOLLAR);
            // The deleted payment had the highest index, and it's never given out again.
            assert_eq!(take_bitcoin_index(conn, "xpub1")?, 2);
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
    fn buys_gift_cards_only_with_credit_not_held() {
        let t = temp_db();
        t.db.with(|conn| {
            pay(conn, "pi_1", DOLLAR.checked_mul(10).unwrap());
            complete_payment(conn, "pi_1", 1)?;
            let five = DOLLAR.checked_mul(5).unwrap();
            // A request in flight holds $6 of the $10: what it will be charged
            // can't be given away on a card before it is.
            let hold = place_hold(conn, ALICE, DOLLAR.checked_mul(6).unwrap(), 2)?.unwrap();
            assert_eq!(
                create_giftcards(conn, ALICE, five, 1, true, 2)?.unwrap_err(),
                GiftcardError::Insufficient { required: five, available: DOLLAR.checked_mul(4).unwrap() }
            );
            release_hold(conn, &hold)?;
            assert_eq!(create_giftcards(conn, ALICE, five, 1, true, 3)?.unwrap().len(), 1);
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
                        cache_read_tokens: 1,
                        cache_write_tokens: 0,
                        cost: Money(1),
                        estimated: false,
                        api_key_id: None,
                        upstream: Some("p/m".into()),
                        upstream_cost: Some(Money(2)),
                    },
                    None,
                )?;
            }
            let (first, more) = list_generations(conn, ALICE, 2, None)?;
            assert!(more);
            assert_eq!((first[0].upstream.as_deref(), first[0].upstream_cost, first[0].cache_read_tokens), (Some("p/m"), Some(Money(2)), 1));
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
