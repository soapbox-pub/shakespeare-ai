#!/bin/sh
# Export the old (Deno and Postgres) Shakespeare AI database for
# `shakespeare-ai import-legacy`.
#
#   DATABASE_URL=postgres://… scripts/export-legacy.sh legacy-export/
#
# Writes one JSON object per line per table, all from one snapshot, with
# NUMERIC columns as strings so no precision is lost on the way. Stop the old
# service first, so nothing changes between the export and the switch.
set -eu

out=${1:?usage: export-legacy.sh OUTPUT_DIR}
: "${DATABASE_URL:?set DATABASE_URL to the old database}"
mkdir -p "$out"

psql "$DATABASE_URL" -X -q -A -t -v ON_ERROR_STOP=1 <<SQL
BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;

\o $out/accounts.jsonl
SELECT json_build_object('pubkey', pubkey, 'credits', credits::text) FROM accounts;

\o $out/payments.jsonl
SELECT json_build_object(
  'id', id, 'pubkey', pubkey, 'status', status,
  'amount', amount::text, 'fee', fee::text, 'total', total::text, 'credits', credits::text,
  'method', method, 'url', url,
  'created_at', created_at, 'completed_at', completed_at, 'expires_at', expires_at,
  'lightning_verify', lightning_verify
) FROM payments;

\o $out/giftcards.jsonl
SELECT json_build_object(
  'id', id, 'code', code, 'sender_pubkey', sender_pubkey, 'amount', amount::text,
  'redeemed', redeemed, 'redeemed_by', redeemed_by, 'redeemed_at', redeemed_at,
  'created_at', created_at, 'cancelled', cancelled, 'cancelled_at', cancelled_at
) FROM giftcards;

\o $out/generations.jsonl
SELECT json_build_object(
  'id', id, 'upstream_id', upstream_id, 'pubkey', pubkey, 'model', model,
  'created_at', created_at, 'prompt_tokens', prompt_tokens,
  'completion_tokens', completion_tokens, 'cost', cost::text
) FROM generations;

\o
COMMIT;
SQL

wc -l "$out"/*.jsonl
