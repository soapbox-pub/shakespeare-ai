//! The schema, as an append-only list of migrations.
//!
//! Money is INTEGER nanodollars throughout (see `money.rs`), and times are
//! INTEGER Unix seconds.

pub const MIGRATIONS: &[(&str, &str)] = &[("001_initial", INITIAL)];

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
