# Shakespeare AI

An OpenAI-compatible AI provider that authenticates with Nostr (NIP-98), and
bills prepaid credits bought by card (Stripe), Lightning or onchain bitcoin.
It serves `https://ai.shakespeare.diy/v1`. API keys made with NIP-98 let any
OpenAI client use it with just a base URL and a key.

This is the Rust rewrite of the Deno service (`soapbox-pub/shakespeare-ai`).
Its API is the same, so clients don't change; see "Differences" below for what
did.

## Running

```sh
cp config.example.yaml config.yaml   # then edit it
export ANTHROPIC_API_KEY=… OPENROUTER_API_KEY=… OPENAI_API_KEY=… \
       STRIPE_SECRET_KEY=… STRIPE_WEBHOOK_SECRET=…
shakespeare-ai --config config.yaml serve
```

`--config` defaults to `$SHAKESPEARE_AI_CONFIG`, then `./config.yaml`. The
database is one SQLite file, created and migrated on start.

Put it behind a reverse proxy that sets `X-Real-IP` (the gift card endpoints
rate-limit by it) and doesn't buffer `text/event-stream` responses.

Stripe's webhook goes to `https://<origin>/webhooks/stripe`, subscribed to
`checkout.session.completed`.

### Commands

- `serve`: serve the API.
- `giftcards --amount 20 --quantity 10 [--output giftcards.csv] [--base-url …]`:
  mint gift cards paid for by nobody, and write a CSV of codes and redemption
  links.
- `import-legacy DIR`: import the old service's database (see below).

## Dashboard

The site at `/` is a dashboard (`dashboard/`, React and Vite, started from
mkstack): the models and their prices for anyone, and for someone logged in
with Nostr, their balance, buying credits, redeeming gift cards, payments and
usage. It has no API of its own. It signs each request to the public API with
NIP-98, as any client does, so there are no sessions or cookies.

Any path that isn't the API's (`/v1/…`, `/webhooks/…`, `/docs`, `/terms`,
`/metrics`) is the dashboard's, falling back to its `index.html`.
`GET /v1/credits/methods` tells it the payment methods configured, the minimum
and the fee.

`nix build` compiles the dashboard into the binary (the `embedded-dashboard`
feature). Otherwise it's served from `dashboard/dist`, under the working
directory, beside the binary or in the source tree, so build it first:
`cd dashboard && npm ci && npm run build`.

## Configuration

YAML; see `config.example.yaml`, which documents every field. In short:

- **providers**: OpenAI-compatible APIs, each a `base_url` and `api_key`.
  `openrouter: true` turns on OpenRouter's usage accounting (the reported
  per-request cost becomes the charge) and Anthropic prompt caching
  breakpoints.
- **models**: what clients ask for by id. `upstream: provider/model` maps one
  to a provider, with everything after the first slash being the provider's own
  id (`openrouter/anthropic/claude-sonnet-4.6`). A chat model has a
  `context_window` and a `price` in USD per million tokens. An image model's
  `price` is per image, by quality and shape. `hidden: true` keeps a model out
  of `/v1/models` while it stays callable. Free chat models are always hidden.
- **payments**: the fee (`0.1` is 10%), the minimum purchase, Stripe's keys,
  the Lightning address, and the bitcoin account key.

Any secret may be `${ENV_VAR}`. Unknown fields are errors, so typos don't pass
silently.

## Lightning

Invoices come from a Lightning address: LUD-16 to find its LNURL-pay endpoint,
and LUD-06 to get an invoice. Payment is confirmed by polling the LUD-21
`verify` URL that comes with each invoice, so no node or provider-specific API
is involved. **The address's server must support LUD-21**; if it doesn't
return a `verify` URL, the payment is refused before it's shown. LNbits'
lnurlp extension doesn't support it. Alby, Coinos and Primal do, among others.

## Onchain bitcoin

Each payment gets a fresh address, derived from a watch-only BIP84 account key
(`zpub`, or `xpub` for the same account) on its receive chain, `…/0/i`. The
service holds no private key; the money lands in the wallet the key came
from. Addresses are native segwit (`bc1q…`), and the payment's `url` is a
BIP21 `bitcoin:` URI with the amount, for a QR code.

Payments are confirmed by polling an Esplora API (mempool.space's unless
`esplora` says otherwise; run your own for privacy) once a minute, and
credited once the sats asked for are `confirmations` blocks deep (1 by
default; 0 trusts the mempool, where a payment can still be double-spent).

- The amount in sats is fixed when the payment is made, at that moment's rate.
- A payment stays payable for a day. One that's been sent something stays
  pending while it confirms, and an address is watched for a week after its
  payment expires, so late payments still count.
- **Paying too little credits nothing**, and paying too much credits only
  what was bought. Both are logged (`payments.bitcoin.underpaid`,
  `payments.bitcoin.overpaid`), for an operator to settle by hand.
- Unpaid payments leave unused addresses, so the wallet must look further
  ahead than the usual gap limit of 20 to see everything. Raise it (Sparrow:
  Settings → Gap limit), or the wallet won't show some payments.
- Indexes are counted per account key, so changing the key starts again at 0.
- Don't use the key for anything else that hands out receive addresses, or
  the two will reuse each other's.

## API keys

`POST /v1/keys` makes a key (`sk-shakespeare-…`) for whoever signed the
request. It works as `Authorization: Bearer <key>` on the inference endpoints,
`/v1/chat/completions` and `/v1/images/generations`, and nowhere else, so an
OpenAI SDK needs only `baseURL` and `apiKey`. What it spends comes from its
owner's balance, and its generations are listed with the owner's, marked with
its `api_key_id`.

Keys are managed with NIP-98 only: a key can't list, make or change keys, or
touch credit, so a leaked one can do no more than spend up to its limit.

- `POST /v1/keys`: `name`, `limit` (credits), `limit_reset` (`daily`,
  `weekly`, `monthly` or `null` for a lifetime limit) and `expires_at` (Unix
  seconds), all optional. The response's `key` is the only time the key is
  shown; only its sha256 is stored.
- `GET /v1/keys`, `GET /v1/keys/{id}`: each key with its `label`, limit,
  `limit_remaining`, and `usage` ever, today, this week and this month.
  Periods start at midnight UTC, on Mondays and on the 1st.
- `PATCH /v1/keys/{id}`: any of the above, plus `disabled`. `null` clears a
  limit, its reset or an expiry.
- `DELETE /v1/keys/{id}`: revokes it, for good. It stays listed, with
  `revoked_at`.

Like a balance, a limit is checked before a request and charged after it, so
a key can end a little past its limit, and concurrent requests can each pass
the check. Images are checked against the most they could cost. An account can
have 100 keys that aren't revoked.

## Storage

SQLite, schema in `src/db/migrations.rs`. Money is integer nanodollars.

- `ledger` records every movement of credit and is only ever appended to.
  `UNIQUE (kind, ref)` means a payment, generation or gift card can move
  credit at most once.
- `accounts` caches each balance, updated in the same transaction as each
  ledger entry.
- `payments`, `giftcards` and `generations` are the records behind the
  entries. `bitcoin_addresses` holds each bitcoin payment's address, its
  derivation index and the sats it's owed.
- `api_keys` holds each key's hash, owner, limit and state. A key's spend is
  the sum of its generations' costs, so there's no counter to drift.

Anything that releases money (a payment completing, a gift card being
redeemed or cancelled) is one conditional `UPDATE … RETURNING`, and credit
moves only if it changed a row. That closes the old service's races, where a
Lightning poll and a status check, or two redemptions of one code, could each
add the credits.

## Moving from the old service

1. Stop the old service, so nothing changes from here on. Pending Lightning
   invoices from LNbits can't be checked by the new service: wait for them to
   be paid or expire first.
2. Export: `DATABASE_URL=postgres://… scripts/export-legacy.sh legacy-export/`.
   This takes one consistent snapshot, with numbers as strings so no precision
   is lost.
3. Import into a new database: `shakespeare-ai -c config.yaml import-legacy legacy-export/`.
   It refuses a database that isn't empty. It prints counts, the total of all
   balances before and after (equal to within a nanodollar each), and
   anything odd it found.
4. Point the proxy at the new service.

Each account's ledger starts with one `opening` entry for the balance it had.
Payments, gift cards and generations come over as records, keeping their ids,
so payment links and printed gift card codes keep working. They move no credit
of their own, since their effects are already in the opening balances.

## Differences from the old service

- **Request bodies must be signed.** A NIP-98 event for a request with a body
  must carry a `payload` tag. Every known client (nostrify's `NIP98Client`,
  the opencode plugin) sends one. Without it, a seen header could be replayed
  with a different body.
- **Interrupted streams are charged for.** If a client hangs up before the
  provider reports usage, tokens are estimated from bytes sent and received
  (about four per token), and the generation is marked `estimated`. These used
  to be free.
- **Image cost follows what was made.** The charge uses the quality and size
  the provider reports. Without a `quality`, the balance check assumes high
  quality, because that's what the provider may pick. It used to charge low.
- **Provider errors are passed on.** They come with the provider's status and
  message. A provider rejecting our own key is a 502, and a provider rate
  limit is a 503, so neither looks like the client running out of credit.
- **Late payments still count.** A Stripe webhook arriving after a payment
  expired, or an invoice seen as paid after its deadline, still credits.
- **Gift card codes are case-insensitive.**
- **Pagination is stable.** Rows created in the same second are no longer
  skipped.
- **Dropped:**
  - LNbits payment checks.
  - Counting tokens before the request (the provider's own context-length
    error comes back instead).
  - The GLM `<think>` filter.
  - Saving each generation's full request and response as JSON files.

## Development

```sh
nix develop        # or have cargo, rustc, node and a C compiler
cargo clippy --all-targets
cargo test
(cd dashboard && npm ci && npm test)
```

For the dashboard with hot reload, run the server with `listen:
127.0.0.1:8000` and `origin: http://localhost:8000`, and `npm run dev` in
`dashboard/`, which proxies the API to it. NIP-98 events are signed for the
page's URL, and only their host is checked against `origin`, so open the
dashboard at `localhost:8080`.

Changing the dashboard's dependencies means refreshing `npmDepsHash` in
`flake.nix`: `prefetch-npm-deps dashboard/package-lock.json`.

## License

AGPL-3.0-or-later. See `LICENSE`.
