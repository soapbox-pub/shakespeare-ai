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
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Stripe => "stripe",
            Method::Lightning => "lightning",
        }
    }

    pub fn parse(s: &str) -> Option<Method> {
        match s {
            "stripe" => Some(Method::Stripe),
            "lightning" => Some(Method::Lightning),
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
}

pub fn new_generation_id() -> String {
    format!("gen_{}", random_hex(32))
}

/// Record a generation and charge for it, together.
pub fn record_generation(conn: &mut Connection, g: &Generation) -> Result<()> {
    let tx = write_tx(conn)?;
    tx.execute(
        "INSERT INTO generations (id, upstream_id, pubkey, model, created_at, prompt_tokens, completion_tokens, cost, estimated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            g.id,
            g.upstream_id,
            g.pubkey,
            g.model,
            g.created_at,
            g.prompt_tokens as i64,
            g.completion_tokens as i64,
            g.cost.nanos(),
            g.estimated
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
        "SELECT id, upstream_id, pubkey, model, created_at, prompt_tokens, completion_tokens, cost, estimated FROM generations
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
        })
    })?;
    page(rows.collect::<rusqlite::Result<Vec<_>>>()?, limit)
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
