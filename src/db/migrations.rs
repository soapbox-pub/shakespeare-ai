//! The schema, as an append-only list of migrations.
//!
//! Money is INTEGER nanodollars throughout (see `money.rs`), and times are
//! INTEGER Unix seconds.

pub const MIGRATIONS: &[(&str, &str)] = &[("001_initial", INITIAL), ("002_bitcoin", BITCOIN), ("003_api_keys", API_KEYS), ("004_upstreams", UPSTREAMS), ("005_holds", HOLDS), ("006_bitcoin_checks", BITCOIN_CHECKS), ("007_payment_cleanup", PAYMENT_CLEANUP), ("008_bitcoin_advances", BITCOIN_ADVANCES), ("009_monero", MONERO), ("010_hold_keys", HOLD_KEYS)];

const INITIAL: &str = r#"
-- Every movement of credit, ever. Rows are only ever inserted. A balance is
-- the sum of an account's entries; `accounts` caches it, updated in the same
-- transaction as each entry.
--
-- (kind, ref) is unique, so the same payment, generation or gift card can
-- never move credit twice, however many times something tries.
CREATE TABLE ledger (
    id INTEGER PRIMARY KEY,
    pubkey TEXT NOT NULL,
    amount INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN (
        'opening',            -- the balance carried over from the old service
        'payment',            -- ref: payments.id
        'generation',         -- ref: generations.id
        'giftcard_purchase',  -- ref: giftcards.id
        'giftcard_redeem',    -- ref: giftcards.id
        'giftcard_refund',    -- ref: giftcards.id
        'adjustment'          -- ref: anything unique, by an operator
    )),
    ref TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (kind, ref)
) STRICT;

CREATE INDEX ledger_pubkey ON ledger (pubkey, id);

CREATE TABLE accounts (
    pubkey TEXT PRIMARY KEY,
    balance INTEGER NOT NULL
) STRICT;

CREATE TABLE payments (
    id TEXT PRIMARY KEY,
    pubkey TEXT NOT NULL,
    method TEXT NOT NULL CHECK (method IN ('stripe', 'lightning')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed', 'failed', 'expired')),
    -- Credits bought, and the fee charged on top; the total is their sum.
    amount INTEGER NOT NULL CHECK (amount > 0),
    fee INTEGER NOT NULL CHECK (fee >= 0),
    -- Where to pay: a Stripe Checkout URL, or a bolt11 invoice.
    url TEXT NOT NULL,
    -- How to check on it: the Stripe Checkout session id, or the LUD-21
    -- verify URL. NULL for payments carried over without one.
    provider_ref TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    expires_at INTEGER NOT NULL
) STRICT;

CREATE INDEX payments_pubkey ON payments (pubkey, created_at DESC, id DESC);
CREATE INDEX payments_pending ON payments (expires_at) WHERE status = 'pending';

CREATE TABLE giftcards (
    id TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    -- Who paid for it. Cards made with the `giftcards` command have a random
    -- pubkey nobody holds.
    sender_pubkey TEXT NOT NULL,
    amount INTEGER NOT NULL CHECK (amount > 0),
    status TEXT NOT NULL CHECK (status IN ('active', 'redeemed', 'cancelled')),
    redeemed_by TEXT,
    created_at INTEGER NOT NULL,
    -- When it was redeemed or cancelled.
    closed_at INTEGER,
    CHECK ((status = 'active') = (closed_at IS NULL)),
    CHECK ((status = 'redeemed') = (redeemed_by IS NOT NULL))
) STRICT;

CREATE INDEX giftcards_sender ON giftcards (sender_pubkey, created_at DESC, id DESC);

CREATE TABLE generations (
    id TEXT PRIMARY KEY,
    -- The upstream provider's id for the completion, if it gave one.
    upstream_id TEXT,
    pubkey TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL,
    cost INTEGER NOT NULL,
    -- 1 when the client went away before the upstream reported usage, and the
    -- tokens (and so the cost) are an estimate.
    estimated INTEGER NOT NULL DEFAULT 0 CHECK (estimated IN (0, 1))
) STRICT;

CREATE INDEX generations_pubkey ON generations (pubkey, created_at DESC, id DESC);
"#;

/// Onchain bitcoin payments. SQLite can't change a CHECK constraint in place,
/// so `payments` is rebuilt to allow the new method.
const BITCOIN: &str = r#"
CREATE TABLE payments_new (
    id TEXT PRIMARY KEY,
    pubkey TEXT NOT NULL,
    method TEXT NOT NULL CHECK (method IN ('stripe', 'lightning', 'bitcoin')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed', 'failed', 'expired')),
    -- Credits bought, and the fee charged on top; the total is their sum.
    amount INTEGER NOT NULL CHECK (amount > 0),
    fee INTEGER NOT NULL CHECK (fee >= 0),
    -- Where to pay: a Stripe Checkout URL, a bolt11 invoice, or a BIP21
    -- bitcoin: URI.
    url TEXT NOT NULL,
    -- How to check on it: the Stripe Checkout session id, the LUD-21 verify
    -- URL, or the bitcoin address. NULL for payments carried over without
    -- one.
    provider_ref TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    expires_at INTEGER NOT NULL
) STRICT;

INSERT INTO payments_new SELECT id, pubkey, method, status, amount, fee, url, provider_ref, created_at, completed_at, expires_at FROM payments;
DROP TABLE payments;
ALTER TABLE payments_new RENAME TO payments;

CREATE INDEX payments_pubkey ON payments (pubkey, created_at DESC, id DESC);
CREATE INDEX payments_pending ON payments (expires_at) WHERE status = 'pending';

-- The address each bitcoin payment is paid to: the account key's receive
-- address at `idx`, never given out twice.
CREATE TABLE bitcoin_addresses (
    payment_id TEXT PRIMARY KEY REFERENCES payments (id),
    -- The account key, as an xpub or tpub.
    xpub TEXT NOT NULL,
    idx INTEGER NOT NULL CHECK (idx >= 0),
    address TEXT NOT NULL UNIQUE,
    -- What's owed, at the rate when the payment was made.
    sats INTEGER NOT NULL CHECK (sats > 0),
    UNIQUE (xpub, idx)
) STRICT;
"#;

/// API keys: bearer tokens for inference, made and managed with NIP-98, that
/// spend their owner's credit.
const API_KEYS: &str = r#"
CREATE TABLE api_keys (
    id TEXT PRIMARY KEY,
    -- Whose credit it spends.
    pubkey TEXT NOT NULL,
    -- The sha256 of the key, in hex. The key itself is shown once, when it's
    -- made, and never stored.
    hash TEXT NOT NULL UNIQUE,
    -- Enough of the key to tell it apart: `sk-shakespeare-1a2b…9z8y`.
    label TEXT NOT NULL,
    name TEXT NOT NULL,
    disabled INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
    -- The most it may spend, per `limit_reset` period or, without one, ever.
    -- NULL for no limit.
    spend_limit INTEGER CHECK (spend_limit >= 0),
    limit_reset TEXT CHECK (limit_reset IN ('daily', 'weekly', 'monthly')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER,
    -- Revoked keys stay, so their generations still name them, but never work
    -- again.
    revoked_at INTEGER
) STRICT;

CREATE INDEX api_keys_pubkey ON api_keys (pubkey, created_at DESC, id DESC);

-- The key a generation was made with, if any. A key's spend is the sum of
-- its generations' costs, which the index covers.
ALTER TABLE generations ADD COLUMN api_key_id TEXT REFERENCES api_keys (id);
CREATE INDEX generations_api_key ON generations (api_key_id, created_at, cost) WHERE api_key_id IS NOT NULL;
"#;

/// Which upstream served each generation and what it cost us, and the cached
/// input it was charged for apart.
const UPSTREAMS: &str = r#"
-- `provider/model` as the config names it. NULL for generations from before
-- models had more than one.
ALTER TABLE generations ADD COLUMN upstream TEXT;
-- What the upstream says it charged us, if it says. Never shown to clients.
ALTER TABLE generations ADD COLUMN upstream_cost INTEGER;
-- Of prompt_tokens, those read from and written to the provider's cache.
ALTER TABLE generations ADD COLUMN cache_read_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE generations ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0;
"#;

/// Credit set aside for inference requests in flight.
const HOLDS: &str = r#"
-- What each request in flight might cost, taken out of what its account can
-- spend until the request is charged. Not money moved: the ledger and
-- balances don't see them. A hold belongs to a request in this process, so
-- any left at startup are from one that died, and are cleared.
CREATE TABLE holds (
    id TEXT PRIMARY KEY,
    pubkey TEXT NOT NULL,
    amount INTEGER NOT NULL CHECK (amount >= 0),
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX holds_pubkey ON holds (pubkey, amount);
"#;

/// What each bitcoin address had received when last looked at, so it's only
/// looked at again when the answer could have changed, and what was credited.
const BITCOIN_CHECKS: &str = r#"
-- Sats sent to the address, the mempool included, at the last check.
ALTER TABLE bitcoin_addresses ADD COLUMN seen INTEGER NOT NULL DEFAULT 0;
-- When it was last checked, and the best block's height then. NULL: never.
ALTER TABLE bitcoin_addresses ADD COLUMN checked_at INTEGER;
ALTER TABLE bitcoin_addresses ADD COLUMN checked_height INTEGER;
-- The confirmed sats the payment was credited for, more or less than `sats`.
ALTER TABLE bitcoin_addresses ADD COLUMN paid INTEGER;

CREATE INDEX payments_open ON payments (method, status, expires_at, pubkey);
"#;

/// Expired payments are deleted once they're long past, so bitcoin indexes
/// are counted apart from the addresses that used them.
const PAYMENT_CLEANUP: &str = r#"
-- The next receive address index for each account key. Never goes down, so
-- an address whose payment was deleted is never given out again.
CREATE TABLE bitcoin_accounts (
    xpub TEXT PRIMARY KEY,
    next_idx INTEGER NOT NULL CHECK (next_idx >= 0)
) STRICT;

INSERT INTO bitcoin_accounts (xpub, next_idx) SELECT xpub, MAX(idx) + 1 FROM bitcoin_addresses GROUP BY xpub;

CREATE INDEX payments_expired ON payments (expires_at) WHERE status = 'expired';
"#;

/// Credit given before a bitcoin payment confirms, to accounts that have
/// paid before, and the accounts that lost that trust.
const BITCOIN_ADVANCES: &str = r#"
-- Credit its account may spend now, on sats sent that haven't confirmed. Not
-- money moved: the ledger and balances don't see it until the payment is
-- credited, which sets it back to 0. If the payment never confirms, it's
-- taken back, and whatever was spent of it is left owed, a negative balance.
ALTER TABLE bitcoin_addresses ADD COLUMN advanced INTEGER NOT NULL DEFAULT 0 CHECK (advanced >= 0);

CREATE INDEX bitcoin_advances ON bitcoin_addresses (payment_id) WHERE advanced > 0;

-- Accounts whose advanced payment never confirmed, and are never advanced
-- anything again.
CREATE TABLE distrusted (
    pubkey TEXT PRIMARY KEY,
    -- The payment that wasn't paid. It may since have been deleted.
    payment_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;
"#;

/// Monero payments. `payments` is rebuilt to allow the new method, as for
/// bitcoin; migrations run with foreign keys off for that, so the tables
/// that reference it keep doing so.
const MONERO: &str = r#"
CREATE TABLE payments_new (
    id TEXT PRIMARY KEY,
    pubkey TEXT NOT NULL,
    method TEXT NOT NULL CHECK (method IN ('stripe', 'lightning', 'bitcoin', 'monero')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed', 'failed', 'expired')),
    -- Credits bought, and the fee charged on top; the total is their sum.
    amount INTEGER NOT NULL CHECK (amount > 0),
    fee INTEGER NOT NULL CHECK (fee >= 0),
    -- Where to pay: a Stripe Checkout URL, a bolt11 invoice, a BIP21
    -- bitcoin: URI, or a monero: URI.
    url TEXT NOT NULL,
    -- How to check on it: the Stripe Checkout session id, the LUD-21 verify
    -- URL, or the bitcoin or monero address. NULL for payments carried over
    -- without one.
    provider_ref TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    expires_at INTEGER NOT NULL
) STRICT;

INSERT INTO payments_new SELECT id, pubkey, method, status, amount, fee, url, provider_ref, created_at, completed_at, expires_at FROM payments;
DROP TABLE payments;
ALTER TABLE payments_new RENAME TO payments;

CREATE INDEX payments_pubkey ON payments (pubkey, created_at DESC, id DESC);
CREATE INDEX payments_pending ON payments (expires_at) WHERE status = 'pending';
CREATE INDEX payments_open ON payments (method, status, expires_at, pubkey);
CREATE INDEX payments_expired ON payments (expires_at) WHERE status = 'expired';

-- The subaddress each monero payment is paid to: the wallet's, at `idx` in
-- its first account, never given out twice.
CREATE TABLE monero_addresses (
    payment_id TEXT PRIMARY KEY REFERENCES payments (id),
    -- The wallet's primary address, which names it.
    account TEXT NOT NULL,
    -- 0 is the primary address itself, never given out.
    idx INTEGER NOT NULL CHECK (idx > 0),
    address TEXT NOT NULL UNIQUE,
    -- What's owed, at the rate when the payment was made.
    piconero INTEGER NOT NULL CHECK (piconero > 0),
    -- Piconero sent to it, the mempool included, at the last check, and when
    -- that was. NULL: never.
    seen INTEGER NOT NULL DEFAULT 0,
    checked_at INTEGER,
    -- The confirmed piconero the payment was credited for.
    paid INTEGER,
    UNIQUE (account, idx)
) STRICT;

-- The next subaddress index for each wallet. Never goes down, so an address
-- whose payment was deleted is never given out again.
CREATE TABLE monero_accounts (
    account TEXT PRIMARY KEY,
    next_idx INTEGER NOT NULL CHECK (next_idx > 0)
) STRICT;
"#;

/// The API key each hold is for, so a key's requests in flight count
/// against its spend limit as they do against its owner's credit.
const HOLD_KEYS: &str = r#"
ALTER TABLE holds ADD COLUMN api_key_id TEXT;
CREATE INDEX holds_api_key ON holds (api_key_id, amount) WHERE api_key_id IS NOT NULL;
"#;
