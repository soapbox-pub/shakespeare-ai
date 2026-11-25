mod api;
mod app;
mod config;
mod dashboard;
mod db;
#[cfg(test)]
mod e2e;
mod legacy;
mod log;
mod metrics;
mod money;
mod nostr;
mod payments;
mod store;
mod util;

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;

use crate::config::Config;
use crate::db::Db;
use crate::money::Money;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// The config file.
    #[arg(long, short, env = "SHAKESPEARE_AI_CONFIG", default_value = "config.yaml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the API.
    Serve,
    /// Mint gift cards, paid for by nobody, and write them to a CSV file.
    Giftcards {
        /// Dollars of credit on each card.
        #[arg(long)]
        amount: String,
        /// How many cards to make.
        #[arg(long)]
        quantity: u32,
        #[arg(long, default_value = "giftcards.csv")]
        output: PathBuf,
        /// The API root redemption links point Shakespeare at.
        #[arg(long, default_value = "https://ai.shakespeare.diy/v1")]
        base_url: String,
    },
    /// Import the old service's database, exported by
    /// scripts/export-legacy.sh, into a new, empty one.
    ImportLegacy {
        /// The directory the export wrote.
        dir: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    let db = Db::open(&config.database)?;

    match cli.command {
        Command::Serve => serve(config, db),
        Command::Giftcards { amount, quantity, output, base_url } => giftcards(&db, &amount, quantity, &output, &base_url),
        Command::ImportLegacy { dir } => {
            let report = legacy::import(&db, &dir)?;
            println!("Imported into {}:", config.database.display());
            println!("  {} accounts, {} payments, {} gift cards, {} generations", report.accounts, report.payments, report.giftcards, report.generations);
            println!("  balances total ${} (the old database's totalled ${})", report.total.to_decimal().normalize(), report.legacy_total.normalize());
            for (warning, count) in &report.warnings {
                println!("  warning: {count} {warning}");
            }
            Ok(())
        }
    }
}

fn serve(config: Config, db: Db) -> Result<()> {
    // Holds belong to requests in flight; any here now are from a server
    // that stopped mid-request, and would keep their credit out of reach.
    let cleared = db.with(|conn| store::clear_holds(conn))?;
    if cleared > 0 {
        log::warn("holds.cleared", serde_json::json!({ "holds": cleared }));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        let (listen, metrics_listen) = (config.listen, config.metrics_listen);
        let app = Arc::new(app::App::new(config, db)?);
        tokio::spawn(payments::poll(app.clone()));

        // Metrics are for the operator, on an address of their own that the
        // proxy doesn't serve.
        if let Some(address) = metrics_listen {
            let listener = tokio::net::TcpListener::bind(address).await.with_context(|| format!("listening on {address} for metrics"))?;
            log::info("metrics.listening", serde_json::json!({ "address": address.to_string() }));
            let router = app::metrics_router(app.clone());
            tokio::spawn(async move { axum::serve(listener, router).await });
        }

        let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("listening on {listen}"))?;
        log::info("server.listening", serde_json::json!({ "address": listen.to_string() }));
        let service = app::router(app).into_make_service_with_connect_info::<std::net::SocketAddr>();
        axum::serve(listener, service).with_graceful_shutdown(shutdown()).await?;
        Ok(())
    })
}

async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
    }
}

fn giftcards(db: &Db, amount: &str, quantity: u32, output: &Path, base_url: &str) -> Result<()> {
    let amount = Decimal::from_str(amount.trim()).ok().and_then(Money::from_dollars).filter(|a| a.is_positive());
    let Some(amount) = amount else { bail!("--amount must be a positive number of dollars") };
    if quantity == 0 {
        bail!("--quantity must be at least 1");
    }

    // A sender nobody holds the key to, so nobody can cancel them.
    let sender = util::random_hex(64);
    let now = util::now();
    let cards = db.with(|conn| store::create_giftcards(conn, &sender, amount, quantity, false, now))?.map_err(|e| anyhow::anyhow!("{e:?}"))?;

    let base: String = url::form_urlencoded::byte_serialize(base_url.as_bytes()).collect();
    let link = |code: &str| format!("https://shakespeare.diy/giftcard#baseURL={base}&code={code}");
    let mut csv = String::from("Code,Amount,Status,Created Date,Redemption URL");
    for card in &cards {
        csv.push_str(&format!("\n\"{}\",\"{}\",\"Active\",\"{}\",\"{}\"", card.code, card.amount, date(card.created_at), link(&card.code)));
    }
    std::fs::write(output, csv).with_context(|| format!("writing {}", output.display()))?;

    for (i, card) in cards.iter().enumerate() {
        println!("{}. {} - {} - {}", i + 1, card.code, card.amount, link(&card.code));
    }
    let total = amount.checked_mul(i64::from(quantity)).unwrap_or(Money(i64::MAX));
    println!("\nMade {quantity} gift cards worth ${total} in all, written to {}.", output.display());
    Ok(())
}

/// A Unix time as "Oct 29, 2025", in UTC.
fn date(unix: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {day}, {year}", MONTHS[(month - 1) as usize])
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_dates() {
        assert_eq!(super::date(1_761_696_000), "Oct 29, 2025");
        assert_eq!(super::date(0), "Jan 1, 1970");
    }
}
