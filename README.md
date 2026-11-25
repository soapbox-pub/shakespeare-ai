# Shakespeare AI

An OpenAI-compatible AI provider that authenticates with Nostr (NIP-98), and
bills prepaid credits bought by card (Stripe), Lightning, onchain bitcoin or
Monero.
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

Put it behind a reverse proxy that doesn't buffer `text/event-stream`
responses, and that puts the client's IP in a header: set `real_ip_header` to
it (`x-real-ip`, or `x-forwarded-for`, whose last address is used). The gift
card and payment endpoints rate-limit by that IP, and the logs show it.
Unset, the IP is the address that connected, which behind a proxy is the
proxy's, so every client would share one limit.

Prometheus metrics are served on `metrics_listen`, apart from clients, if
it's set: `http://<metrics_listen>/metrics`.

Stripe's webhook goes to `https://<origin>/webhooks/stripe`, subscribed to
`checkout.session.completed`.

### Commands

- `serve`: serve the API.
- `giftcards --amount 20 --quantity 10 [--output giftcards.csv] [--base-url …]`:
  mint gift cards paid for by nobody, and write a CSV of codes and redemption
  links.
- `import-legacy DIR`: import the old service's database (see below).

An account may start 10 payments an hour, and an IP 30 (an IPv6 one counted
by its /64), since each is a Stripe session, an invoice, or a bitcoin or
monero address watched for a week. Bitcoin and monero payments are capped by
how many are open at once as well (see below), since accounts are free to
make. Pending
payments are checked on 8 at a time, and a provider that takes more than 15
seconds to answer is given up on until the next poll.

A payment that expires unpaid is deleted 30 days later, by which time nothing
can still complete it, so expired payments don't pile up. A bitcoin or monero
payment that was sent something is kept, as the record of where that money
went.

## Dashboard

The site at `/` is a dashboard (`dashboard/`, React and Vite, started from
mkstack): the models and their prices for anyone, and for someone logged in
with Nostr, their balance, buying credits, payments, API keys, gift cards
(bought, listed, cancelled and redeemed) and usage, and the API docs and
terms at `/docs` and `/terms`. A gift card's link is
`/credits#redeem=<code>`, the code in the fragment so it stays out of logs. It has no API of its own. It signs each request to the public API with
NIP-98, as any client does, so there are no sessions or cookies.

Any path that isn't the API's (`/v1/…`, `/webhooks/…`) is the dashboard's: a file, or its directory's `index.html`, or
else `200.html`, the app with no page in it.

`npm run build` prerenders the public pages (`/`, `/docs` and `/terms`, as
someone logged out sees them) into that `index.html` for each, so crawlers and
link previews that don't run JavaScript get the whole page
(`dashboard/src/prerender.tsx`). The app replaces it as it starts. The pages
show the API's address, which isn't known at build time: they're prerendered
with `https://ai.shakespeare.diy`, or `PRERENDER_ORIGIN` if it's set, and the
app puts in the real one.
`GET /v1/credits/methods` tells it the payment methods configured, the minimum
and the fee.

`nix build` compiles the dashboard into the binary (the `embedded-dashboard`
feature). Otherwise it's served from `dashboard/dist`, under the working
directory, beside the binary or in the source tree, so build it first:
`cd dashboard && npm ci && npm run build`.

## Configuration

YAML; see `config.example.yaml`, which documents every field. In short:

- **routing**: how hard to try a model's upstreams (see below).
- **providers**: upstream APIs, each an `api` (the protocol: `openai`;
  `openrouter`, OpenAI's with OpenRouter's usage accounting and Anthropic
  prompt caching breakpoints; or `anthropic`, see below), an `api_key`, and a
  `base_url` if it isn't the usual one for `api`.
- **models**: what clients ask for by id. Each has a `type`, which says
  which endpoint serves it and so which fields it has: `chat` for
  `/v1/chat/completions`, or `image` for `/v1/images/generations`. `/v1/models`
  says the same in each model's `type`. Each has a list of `upstreams`
  tried in order. An upstream is `provider/model`, everything after the first
  slash being the provider's own id (`openrouter/anthropic/claude-sonnet-4.6`),
  or `{ provider, model, params }`, whose `params` are merged into each
  request body sent that way. A chat model has a `context_window` and a
  `price` in USD per million tokens: `input`, `output`, and `cache_read` and
  `cache_write` for cached input, which default to `input`. An image model's
  `price` is per image, by quality and shape, and its upstreams must speak
  `openai`. With `price.tokens` (USD per million text input, image input and
  output tokens, as OpenAI bills GPT Image 2 and later), it's charged by the
  tokens the provider says it used, whatever quality or size it picked, and
  the per-image table is only what's shown and checked against beforehand. `hidden: true` keeps a model out of `/v1/models` while it stays
  callable. Free chat models are always hidden.
- **payments**: the fee (`0.1` is 10%), the minimum purchase, Stripe's keys,
  the Lightning address, the bitcoin account key, and the Monero wallet's
  RPC.

Any secret may be `${ENV_VAR}`. Unknown fields are errors, so typos don't pass
silently.

### Upstreams

A request goes to the model's first upstream, and on to the next if that one
fails in a way another might not: it can't be reached, it's overloaded or down
(408, 429, 5xx), it refuses our credentials or credit (401–403), or it hasn't
got the model (404). A refusal of the request itself (any other 4xx) would be
the same anywhere, so it goes back to the client. A stream is committed to once
its first byte arrives, which must be within `routing.first_byte_timeout`;
after that nothing falls over, since the client has started reading. A
non-streaming response comes whole, however long it takes, so only streams
have that timeout.

An upstream that fails `routing.failures` times in a row goes to the back of
the line for `routing.cooldown`. That's kept in memory, so it starts again on
restart. Failures are counted in `shakespeare_upstream_failures_total`.

A request costs the model's price whichever upstream serves it, so clients
can know what they'll pay. Each generation records the upstream that served
it, and what it cost us when the provider says (OpenRouter does), for an
operator to see the margin by route; neither is shown to clients. The one
exception is a free model an upstream charged us for anyway, which is charged
on at that cost.

## Anthropic

A provider with `api: anthropic` is spoken to in Anthropic's Messages API
(version `2023-06-01`), so clients keep speaking OpenAI's chat API while
direct Claude requests get prompt caching: breakpoints go on the last tool,
the end of the system prompt and the last two messages, as for OpenRouter's
`anthropic/` models. Usage comes back in OpenAI's terms, with cached input in
`prompt_tokens` and broken out in `prompt_tokens_details`.

- `max_tokens` is required by Anthropic, so it's 16384 when the client
  doesn't say.
- `reasoning_effort` asks Claude 4.6 and later for adaptive thinking at that
  effort (`minimal` is `low`, and Claude 4.6's `xhigh` is `high`), and
  earlier models for a thinking budget (1024 to 32768 tokens), left out when
  `max_tokens` has no room for one or a tool call is forced. Claude 5 and
  later think anyway, and are asked to show it. With thinking asked for,
  `temperature` and `top_p` are left out, as Anthropic refuses them.
- Thinking comes back as OpenRouter spells it: `reasoning` for the text, and
  `reasoning_details` for the signed blocks, which clients hand back (tool
  use needs them) as they would to OpenRouter. Streams send each thinking
  block's detail whole, once it's signed.
- `response_format` with a `json_schema` becomes `output_config.format`;
  `json_object` has no equivalent and is left out, as are `seed` and the
  penalties.

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

Payments are confirmed by polling an Esplora API: ours,
`https://btc.soapbox.pub/api`, on our own node, unless `esplora` says
otherwise. (Its answers decide what's credited, so point it only at a server
you trust. Public ones such as mempool.space's rate-limit.) A transaction
counts once it's `confirmations` blocks deep (1 by default; 0 trusts the
mempool, where a payment can still be double-spent).

- The amount in sats is fixed when the payment is made, at that moment's rate.
- **What's paid is what's credited.** A payment is credited in proportion to
  the confirmed sats sent, at its fixed rate, and its fee likewise: paying
  twice the amount credits twice the credits. Paid in full or more, it's
  credited at once; paid short, once its time is up, so a top-up before then
  still counts. Its `amount` and `fee` then say what was credited, and
  `payments.bitcoin.overpaid` or `.underpaid` is logged.
- A payment stays payable for a day. One that's been sent something stays
  pending while it confirms, and an address is watched for a week after its
  payment expires, so late payments still count. Money sent after a payment
  is credited, or after that week, lands in the wallet uncredited.
- An address is only asked about when the answer could have changed: every
  minute while it's open and unpaid; once a block when it's been sent
  something that isn't deep enough yet; and, past its time with nothing
  sent, every 5 minutes for an hour, hourly for a day, then every 6 hours.
  Asking for a payment (`GET /v1/credits/payments/{id}`) also checks its
  address then, at most every 10 seconds, so the dashboard sees money reach
  the mempool at once.
- What's been sent shows before it's credited: a payment's `bitcoin` has the
  `sats` it asks for and the sats `received`, the mempool included, and the
  balance's `pending` is what's on its way, valued as it will be credited.
- **Accounts that have paid before needn't wait.** What's been sent to a
  payment and will very likely confirm (in a block, or in the mempool paying
  at least Esplora's fee estimate for 6 blocks) is advanced: its account may
  spend it at once, as its payment's `bitcoin.advanced` and the balance's
  `advanced`. An account's advances, on all its open payments together, are
  at most `trust.ratio` (0.5) of the credit it has ever bought by bitcoin or
  Lightning (not card, which can be charged back), and never more than
  `trust.max_advance` ($50); everyone's together, never more than
  `trust.max_total` ($500). Each is set in one write transaction counting
  every other, so payments checked at once can't overshoot. A new account
  always waits for its confirmation, and new pubkeys earn nothing.
- An advance isn't a ledger entry: like a hold, it only changes what can be
  spent on inference (gift cards are bought from the balance less what's
  held, without advances, so it can't be moved to another account). When the payment is credited, the
  credit takes its place. An address with an advance is checked every
  minute; if what was sent to it shrinks (a transaction replaced or dropped),
  or it's still unconfirmed when its week of watching is up, the advance is
  taken back, what was spent of it is left owed as a negative balance, and
  the account is in `distrusted` and never advanced anything again
  (`payments.bitcoin.advance_failed`). So the most anyone can take is half of
  what they've already paid, up to $50, once per account. `max_advance: 0`
  turns advances off.
- An account may have 3 bitcoin payments open at once, and everyone
  together `max_pending` (200 by default); past that, a new one is refused
  with a 503 until some are paid or expire. That bounds how fast addresses
  can be used up: no more than `max_pending` a day.
- Unpaid payments leave unused addresses, so the wallet must look further
  ahead than the usual gap limit of 20 to see everything. Raise it (Sparrow:
  Settings → Gap limit) well past `max_pending`, or the wallet won't show
  some payments. (The service still credits them.)
- Indexes are counted per account key, so changing the key starts again at 0.
  An index stays used after its payment is deleted, so no address is given
  out twice.
- Don't use the key for anything else that hands out receive addresses, or
  the two will reuse each other's.

## Monero

Each payment gets a fresh subaddress from a view-only wallet open in
`monero-wallet-rpc` (`wallet_rpc`), in its first account: `create_address`,
labelled with the payment's id. The wallet has the view key, not the spend
key, so it sees money arrive and can't move it; the money is spent from the
wallet the key came from. The payment's `url` is a `monero:` URI with
`tx_amount`, for a QR code.

Monero hides who's paid whom, so there's no public API to ask what an address
received, as Esplora answers for bitcoin: only a wallet holding the view key
can tell, by scanning every block. So `monero-wallet-rpc` runs against our own
`monerod`, and its answers decide what's credited; never point it at a node
you don't trust, which could show it a payment that isn't in the chain. Its
`--rpc-login` is given as `username` and `password`, and answered with HTTP
digest auth.

It's credited as bitcoin is: the piconero asked for are fixed when the
payment is made (at Kraken's rate, else CoinGecko's, rounded up to a
hundred-millionth of a monero), what's paid is what's credited, a payment
stays payable for a day and is watched for a week after, an account may have
3 open at once and everyone together `max_pending`, and a transaction counts
once it's `confirmations` deep (1 by default; 0 counts the mempool). Where it
differs:

- The wallet has already scanned every block, so each poll is one
  `get_transfers` for every watched subaddress, rather than a schedule of
  checks. Asking for a payment checks its subaddress then, at most every 10
  seconds, so the dashboard sees money reach the mempool at once.
- Nothing is advanced before confirmation: a block comes every two minutes.
  But a completed monero payment counts as paid before, toward an account's
  bitcoin advances, as Lightning does: it can't be charged back.
- Money sent locked until a later block or time (`unlock_time`), which
  wallets don't do but anyone can, isn't counted: it couldn't be spent. It's
  logged as `payments.monero.locked`. Nor is a mempool transaction the wallet
  has seen double-spent.
- Indexes come from the wallet, which hands them out in order, and are
  counted per wallet (by its primary address) here too. A wallet made again
  from its keys starts over at 1, so if it gives an index already given out,
  it's moved on past them. Index 0, the primary address, is never given out.
  Payments to a wallet that's since been replaced in the config aren't
  checked (`payments.monero` logs how many).
- The wallet the money's spent from sees only the subaddresses it looks
  ahead to (Feather and the GUI: 200 by default). Raise its lookahead well
  past `max_pending` too.

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

A limit is checked before a request and charged after it, so a key can end
a little past its limit, and concurrent requests can each pass the check. The
owner's balance still pays: a limit is the owner's own cap, not credit.
Images are checked against the most they could cost. An account can have 100
keys that aren't revoked.

A balance is held, not just checked. Before calling a provider, a request
sets aside what it's likely to cost: a chat its prompt (estimated from all
the JSON it's written in, keys and numbers too, and about 1,600 tokens an
image) plus `max_tokens`, at most 4,096, of
output; an image request the most it could cost. The check and the hold are
one write transaction against the balance less every other hold, so
concurrent requests can't all spend the same credit. Without enough to hold,
a request is a 429, `insufficient_quota`, with `required` and `available`.
The hold is given back in the transaction that charges the request, or when
the request fails, and holds left by a server that stopped are cleared when
it starts.

A chat can still cost more than its hold, by output past 4,096 tokens, so a
balance can end a little below zero. An account (all its keys and NIP-98
together) may have only 4 inference requests in flight; a fifth is a 429,
`too_many_concurrent_requests`. With `n` gone, that bounds how far below zero
anyone can go: four requests, each past its hold by as much as its provider's
own output limit allows.

## Storage

SQLite, schema in `src/db/migrations.rs`. Money is integer nanodollars.

- `ledger` records every movement of credit and is only ever appended to.
  `UNIQUE (kind, ref)` means a payment, generation or gift card can move
  credit at most once.
- `accounts` caches each balance, updated in the same transaction as each
  ledger entry.
- `payments`, `giftcards` and `generations` are the records behind the
  entries. A generation keeps its tokens (with the cached input among them),
  its cost, the upstream that served it and, if known, what that cost us. `bitcoin_addresses` holds each bitcoin payment's address, its
  derivation index and the sats it's owed; `monero_addresses` each monero
  payment's subaddress, its index and the piconero it's owed.
- `api_keys` holds each key's hash, owner, limit and state. A key's spend is
  the sum of its generations' costs, so there's no counter to drift.
- `holds` is the credit set aside for requests in flight. Holds aren't
  ledger entries: they move no money, they only limit what can be spent.
  Nor are advances, `bitcoin_addresses.advanced`: credit spendable before a
  payment confirms. `distrusted` lists accounts that are never advanced
  anything again.

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
- **A signed request can't be sent twice.** A NIP-98 event that authorized a
  POST, PATCH or DELETE is refused if it comes again, so a header seen in a
  log or a proxy can't buy another gift card with the same body. (Reads may
  repeat; they show nothing new.) Signers must use fresh randomness in each
  signature, as nostr-tools and nostrify do.
- **Chat requests are parsed, not passed through.** A request is read into
  the part of OpenAI's chat API that providers share (`messages` with text
  and image parts and tool calls, `tools`, `tool_choice`, `max_tokens`,
  `max_completion_tokens`, `temperature`, `top_p`, `stop`, `seed`, the
  penalties, `parallel_tool_calls`, `response_format`) plus
  `reasoning_effort`, and the provider's request is written from that.
  Anything else is ignored, so a client can't reach a provider's own options
  (OpenRouter's `models` fallbacks could run any model on a free one), and
  `n` is gone. A field in the schema with the wrong shape is a 400. No client
  header is passed on, `Accept-Language` included. A session (`session_id`,
  or the `x-session-id` header Shakespeare sends) reaches a provider only as
  its hash with the caller's pubkey: OpenRouter's `session_id`, for sticky
  routing to one provider's prompt cache, or OpenAI's `prompt_cache_key`, so
  no two accounts share a key or see each other's session names. If
  OpenRouter reports a cost for a free model anyway, it's charged.
- **Interrupted streams are charged for.** If a client hangs up, or stops
  reading, the provider's stream is still read to its end and charged by the
  usage it reports, as the provider charges us. Only a stream that ends with
  no usage (a provider error mid-stream) is estimated, from bytes sent and
  received (about four per token), and marked `estimated`. These used to be
  free.
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
