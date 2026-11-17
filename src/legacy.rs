//! Importing the old (Deno and Postgres) service's database.
//!
//! `scripts/export-legacy.sh` writes each table as JSON lines from one
//! consistent snapshot, numerics as strings so none of their precision is lost
//! to floats. This reads them into an empty database.
//!
//! The old service kept balances, not a ledger, so each account's history
//! starts here, with one `opening` entry for the balance it had. Payments,
//! gift cards and generations are carried over as records, keeping their ids
//! so clients' links and printed gift card codes still work, but move no
//! credit: their effects are already in the opening balances.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use rusqlite::params;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::db::Db;
use crate::money::Money;
use crate::store::{self, Kind};

#[derive(Deserialize)]
struct Account {
    pubkey: String,
    credits: String,
}

#[derive(Deserialize)]
struct Payment {
    id: String,
    pubkey: String,
    status: String,
    fee: String,
    total: String,
    credits: String,
    method: String,
    url: String,
    created_at: i64,
    completed_at: Option<i64>,
    expires_at: i64,
    lightning_verify: Option<String>,
}

#[derive(Deserialize)]
struct Giftcard {
    id: String,
    code: String,
    sender_pubkey: String,
    amount: String,
    redeemed: bool,
    redeemed_by: Option<String>,
    redeemed_at: Option<i64>,
    created_at: i64,
    cancelled: bool,
    cancelled_at: Option<i64>,
}

#[derive(Deserialize)]
struct Generation {
    id: String,
    upstream_id: Option<String>,
    pubkey: String,
    model: String,
    created_at: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    cost: String,
}

#[derive(Default, Debug)]
pub struct Report {
    pub accounts: usize,
    pub payments: usize,
    pub giftcards: usize,
    pub generations: usize,
    /// The sum of the old balances, exactly.
    pub legacy_total: Decimal,
    /// The sum of the new balances.
    pub total: Money,
    pub warnings: BTreeMap<String, usize>,
}

fn dollars(value: &str, what: &str) -> Result<Money> {
    let decimal = Decimal::from_str(value.trim()).with_context(|| format!("{what}: {value:?} isn't a number"))?;
    Money::from_dollars(decimal).with_context(|| format!("{what}: {value} is out of range"))
}

fn each<T: for<'de> Deserialize<'de>>(dir: &Path, table: &str, mut f: impl FnMut(T) -> Result<()>) -> Result<()> {
    let path = dir.join(format!("{table}.jsonl"));
    let file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row: T = serde_json::from_str(&line).with_context(|| format!("{}:{}", path.display(), n + 1))?;
        f(row).with_context(|| format!("{}:{}", path.display(), n + 1))?;
    }
    Ok(())
}

pub fn import(db: &Db, dir: &Path) -> Result<Report> {
    db.with(|conn| {
        let rows: i64 = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM ledger) + (SELECT COUNT(*) FROM payments) + (SELECT COUNT(*) FROM giftcards) + (SELECT COUNT(*) FROM generations)",
            [],
            |r| r.get(0),
        )?;
        if rows > 0 {
            bail!("the database isn't empty; import into a new one");
        }

        let mut report = Report::default();
        let now = crate::util::now();
        let tx = store::write_tx(conn)?;
        let warn = |report: &mut Report, what: &str| *report.warnings.entry(what.to_string()).or_default() += 1;

        each(dir, "accounts", |a: Account| {
            let exact = Decimal::from_str(a.credits.trim()).with_context(|| format!("credits {:?}", a.credits))?;
            let balance = dollars(&a.credits, "credits")?;
            report.legacy_total += exact;
            report.accounts += 1;
            if balance != Money::ZERO {
                store::post(&tx, &a.pubkey, balance, Kind::Opening, &a.pubkey, now)?;
            }
            if balance.nanos() < 0 {
                warn(&mut report, "accounts with a negative balance");
            }
            Ok(())
        })?;

        each(dir, "payments", |p: Payment| {
            let amount = dollars(&p.credits, "credits")?;
            let fee = dollars(&p.fee, "fee")?;
            if amount.checked_add(fee) != Some(dollars(&p.total, "total")?) {
                warn(&mut report, "payments whose total wasn't credits + fee (kept credits and fee)");
            }
            if !matches!(p.status.as_str(), "pending" | "completed" | "failed" | "expired") {
                bail!("payment {} has unknown status {:?}", p.id, p.status);
            }
            if store::Method::parse(&p.method).is_none() {
                bail!("payment {} has unknown method {:?}", p.id, p.method);
            }
            // Only a LUD-21 verify URL is worth keeping; Stripe sessions
            // weren't recorded.
            let provider_ref = p.lightning_verify.filter(|v| p.method == "lightning" && v.starts_with("https://"));
            let completed_at = p.completed_at.or_else(|| (p.status == "completed").then_some(p.created_at));
            tx.execute(
                "INSERT INTO payments (id, pubkey, method, status, amount, fee, url, provider_ref, created_at, completed_at, expires_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![p.id, p.pubkey, p.method, p.status, amount.nanos(), fee.nanos(), p.url, provider_ref, p.created_at, completed_at, p.expires_at],
            )?;
            report.payments += 1;
            Ok(())
        })?;

        each(dir, "giftcards", |g: Giftcard| {
            let amount = dollars(&g.amount, "amount")?;
            // Both flags can be set where the old service raced; the redeemer
            // was credited, so it was redeemed.
            let (status, closed_at, redeemed_by) = if g.redeemed {
                if g.cancelled {
                    warn(&mut report, "gift cards both redeemed and cancelled (kept as redeemed)");
                }
                let by = g.redeemed_by.unwrap_or_else(|| {
                    warn(&mut report, "redeemed gift cards with no redeemer");
                    String::new()
                });
                ("redeemed", Some(g.redeemed_at.unwrap_or(g.created_at)), Some(by))
            } else if g.cancelled {
                ("cancelled", Some(g.cancelled_at.unwrap_or(g.created_at)), None)
            } else {
                ("active", None, None)
            };
            tx.execute(
                "INSERT INTO giftcards (id, code, sender_pubkey, amount, status, redeemed_by, created_at, closed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![g.id, g.code, g.sender_pubkey, amount.nanos(), status, redeemed_by, g.created_at, closed_at],
            )?;
            report.giftcards += 1;
            Ok(())
        })?;

        {
            let mut insert = tx.prepare(
                "INSERT INTO generations (id, upstream_id, pubkey, model, created_at, prompt_tokens, completion_tokens, cost) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )?;
            each(dir, "generations", |g: Generation| {
                let cost = dollars(&g.cost, "cost")?;
                insert.execute(params![g.id, g.upstream_id, g.pubkey, g.model, g.created_at, g.prompt_tokens, g.completion_tokens, cost.nanos()])?;
                report.generations += 1;
                Ok(())
            })?;
        }

        // The cached balances and the ledger agree, and both match what the
        // old service held, to within rounding each to the nanodollar.
        let (ledger, balances): (i64, i64) = tx.query_row(
            "SELECT COALESCE((SELECT SUM(amount) FROM ledger), 0), COALESCE((SELECT SUM(balance) FROM accounts), 0)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if ledger != balances {
            bail!("the ledger ({ledger}) and the balances ({balances}) disagree");
        }
        report.total = Money(balances);
        let drift = (report.total.to_decimal() - report.legacy_total).abs();
        let tolerance = Decimal::new(5, 10) * Decimal::from(report.accounts.max(1) as i64);
        if drift > tolerance {
            bail!("the new balances total {} but the old ones totalled {}", report.total.to_decimal(), report.legacy_total);
        }

        tx.commit()?;
        Ok(report)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testing::temp_db;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn imports_an_export() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, lines: &[String]| std::fs::write(dir.path().join(format!("{name}.jsonl")), lines.join("\n")).unwrap();
        write(
            "accounts",
            &[format!(r#"{{"pubkey":"{A}","credits":"10.123456789012"}}"#), format!(r#"{{"pubkey":"{B}","credits":"-0.002475"}}"#)],
        );
        write("payments", &[format!(
            r#"{{"id":"pi_1","pubkey":"{A}","status":"completed","amount":"10","fee":"1.00","total":"11","credits":"10","method":"lightning","url":"lnbc1","created_at":1,"completed_at":2,"expires_at":3,"lightning_verify":null}}"#
        )]);
        write("giftcards", &[
            format!(r#"{{"id":"gc_1","code":"AAAA-BBBB-CCCC-DDDD","sender_pubkey":"{A}","amount":"5","redeemed":true,"redeemed_by":"{B}","redeemed_at":5,"created_at":4,"cancelled":false,"cancelled_at":null}}"#),
            format!(r#"{{"id":"gc_2","code":"EEEE-BBBB-CCCC-DDDD","sender_pubkey":"{A}","amount":"5","redeemed":false,"redeemed_by":null,"redeemed_at":null,"created_at":4,"cancelled":false,"cancelled_at":null}}"#),
        ]);
        write("generations", &[format!(
            r#"{{"id":"gen_1","upstream_id":"chatcmpl-1","pubkey":"{B}","model":"glm-4.6","created_at":6,"prompt_tokens":150,"completion_tokens":300,"cost":"0.002475"}}"#
        )]);

        let t = temp_db();
        let report = import(&t.db, dir.path()).unwrap();
        assert_eq!((report.accounts, report.payments, report.giftcards, report.generations), (2, 1, 2, 1));
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        t.db.with(|conn| {
            assert_eq!(store::balance(conn, A)?, Money(10_123_456_789));
            assert_eq!(store::balance(conn, B)?, Money(-2_475_000));
            // Carried-over gift cards still redeem, by their old codes.
            assert_eq!(store::redeem_giftcard(conn, B, "EEEE-BBBB-CCCC-DDDD", 10)?, Ok(Money(5_000_000_000)));
            assert!(store::redeem_giftcard(conn, B, "AAAA-BBBB-CCCC-DDDD", 10)?.is_err());
            Ok(())
        })
        .unwrap();

        // Only ever into an empty database.
        assert!(import(&t.db, dir.path()).is_err());
    }
}
