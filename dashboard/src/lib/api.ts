import type { NostrSigner } from '@nostrify/nostrify';

/** NIP-98 HTTP auth event kind. */
const NIP98_KIND = 27235;

export type PaymentMethod = 'stripe' | 'lightning' | 'bitcoin';

/** What credits can be bought with here, and on what terms. */
export interface PaymentMethods {
  /** `confirmations`, for bitcoin: how deep a payment must be to be credited. */
  data: { id: PaymentMethod; confirmations?: number }[];
  minimum_amount: number;
  /** A fraction: 0.1 is 10%. */
  fee: number;
}

export interface Payment {
  id: string;
  status: 'pending' | 'completed' | 'expired';
  amount: number;
  fee: number;
  total: number;
  method: PaymentMethod;
  description: string;
  /** Stripe's checkout page, a BOLT11 invoice, or a BIP21 `bitcoin:` URI. */
  url: string;
  created_at: number;
  completed_at?: number;
  expires_at: number;
  /** A bitcoin payment's sats: what it asks for, and what's been sent so far, the mempool included. */
  bitcoin?: { sats: number; received: number };
}

/** The balance, and credit on its way: bitcoin that's been sent but isn't confirmed yet. */
export interface Credits {
  amount: number;
  pending: number;
}

/** Whether a bitcoin payment has been sent money that's waiting to confirm. */
export function isConfirming(payment: Payment): boolean {
  return payment.method === 'bitcoin' && payment.status !== 'completed' && (payment.bitcoin?.received ?? 0) > 0;
}

/** Whether a payment can still change: one that's open, or sent bitcoin that's still confirming. */
export function isUnsettled(payment: Payment): boolean {
  return payment.status === 'pending' || isConfirming(payment);
}

export const PAYMENT_STATUS: Record<Payment['status'], { label: string; variant: 'default' | 'secondary' | 'outline' }> = {
  completed: { label: 'Paid', variant: 'default' },
  pending: { label: 'Pending', variant: 'secondary' },
  expired: { label: 'Expired', variant: 'outline' },
};

export const METHOD_LABELS: Record<PaymentMethod, string> = { stripe: 'Card', lightning: 'Lightning', bitcoin: 'Bitcoin' };

export interface Generation {
  id: string;
  model: string;
  created: number;
  usage: { prompt_tokens: number; completion_tokens: number; total_tokens: number };
  cost: number;
  /** Tokens counted from bytes, because the client hung up before the provider reported them. */
  estimated: boolean;
  /** The API key that made it, or null when the request was signed with NIP-98. */
  api_key_id: string | null;
}

export type LimitReset = 'daily' | 'weekly' | 'monthly';

export interface ApiKey {
  id: string;
  name: string;
  /** The key's first and last few characters, to recognize it by. */
  label: string;
  disabled: boolean;
  /** Credits it may spend, per `limit_reset` period or ever; null for no limit. */
  limit: number | null;
  limit_reset: LimitReset | null;
  limit_remaining: number | null;
  usage: number;
  usage_daily: number;
  usage_weekly: number;
  usage_monthly: number;
  created_at: number;
  updated_at: number;
  expires_at: number | null;
  revoked_at: number | null;
}

/** What a key can be made or changed with. Absent leaves a field alone; null clears it. */
export interface ApiKeyChanges {
  name?: string;
  disabled?: boolean;
  limit?: number | null;
  limit_reset?: LimitReset | null;
  expires_at?: number | null;
}

export interface Giftcard {
  id: string;
  code: string;
  amount: number;
  status: 'active' | 'redeemed' | 'cancelled';
  created_at: number;
}

export interface Model {
  id: string;
  name: string;
  description?: string | null;
  type: 'chat' | 'image';
  context_window?: number;
  /** Chat: USD per token. Image: USD per image, by quality and shape. */
  pricing: Record<string, string>;
}

export interface List<T> {
  data: T[];
  has_more: boolean;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

/** Requests anyone may make. */
export async function publicRequest<T>(path: string): Promise<T> {
  const response = await fetch(path);
  if (!response.ok) throw await apiError(response);
  return (await response.json()) as T;
}

/**
 * Client for this server's API, signed in as one account.
 *
 * There are no sessions: every request carries a freshly signed NIP-98 event
 * whose `u`, `method` and `payload` tags bind it to exactly that request, as
 * the server requires.
 */
export class ApiClient {
  constructor(private readonly signer: NostrSigner) {}

  async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const url = new URL(path, location.origin);
    const payload = body === undefined ? undefined : JSON.stringify(body);

    const tags = [
      ['method', method],
      ['u', url.toString()],
    ];
    if (payload !== undefined) tags.push(['payload', await sha256Hex(payload)]);

    const event = await this.signer.signEvent({
      kind: NIP98_KIND,
      created_at: Math.floor(Date.now() / 1000),
      tags,
      content: '',
    });

    const headers: Record<string, string> = { authorization: `Nostr ${base64(JSON.stringify(event))}` };
    if (payload !== undefined) headers['content-type'] = 'application/json';

    const response = await fetch(url, { method, headers, body: payload });
    if (!response.ok) throw await apiError(response);
    if (response.status === 204) return undefined as T;
    return (await response.json()) as T;
  }

  balance(): Promise<Credits> {
    return this.request('GET', '/v1/credits');
  }

  addCredits(input: { amount: number; method: PaymentMethod; redirect_url?: string }): Promise<Payment> {
    return this.request('POST', '/v1/credits/add', input);
  }

  payment(id: string): Promise<Payment> {
    return this.request('GET', `/v1/credits/payments/${encodeURIComponent(id)}`);
  }

  payments(startingAfter?: string): Promise<List<Payment>> {
    return this.request('GET', `/v1/credits/payments${page(startingAfter)}`);
  }

  generations(startingAfter?: string): Promise<List<Generation>> {
    return this.request('GET', `/v1/credits/generations${page(startingAfter)}`);
  }

  redeemGiftcard(code: string): Promise<{ amount: number }> {
    return this.request('POST', '/v1/credits/giftcards/redeem', { code });
  }

  giftcards(after?: string): Promise<List<Giftcard>> {
    // This list's cursor has always been called `min_id`.
    return this.request('GET', `/v1/credits/giftcards${page(after, 'min_id')}`);
  }

  /** Paid for from the balance. */
  createGiftcards(input: { amount: number; quantity: number }): Promise<{ data: Giftcard[] }> {
    return this.request('POST', '/v1/credits/giftcards', input);
  }

  /** Cancel an unredeemed card, refunding it to the balance. */
  cancelGiftcard(id: string): Promise<void> {
    return this.request('DELETE', `/v1/credits/giftcards/${encodeURIComponent(id)}`);
  }

  keys(after?: string): Promise<List<ApiKey>> {
    return this.request('GET', `/v1/keys${page(after, 'starting_after', 100)}`);
  }

  /** The response's `key` is the only time the key itself is ever shown. */
  createKey(input: ApiKeyChanges): Promise<ApiKey & { key: string }> {
    return this.request('POST', '/v1/keys', input);
  }

  updateKey(id: string, changes: ApiKeyChanges): Promise<ApiKey> {
    return this.request('PATCH', `/v1/keys/${encodeURIComponent(id)}`, changes);
  }

  revokeKey(id: string): Promise<ApiKey> {
    return this.request('DELETE', `/v1/keys/${encodeURIComponent(id)}`);
  }
}

function page(after: string | undefined, cursor = 'starting_after', limit = 50): string {
  const params = new URLSearchParams({ limit: String(limit) });
  if (after) params.set(cursor, after);
  return `?${params}`;
}

/** The API's errors are OpenAI's shape: `{"error": {"message", "code", ...}}`. */
async function apiError(response: Response): Promise<ApiError> {
  try {
    const body = (await response.json()) as { error?: { message?: string; code?: string } };
    if (body.error?.message) return new ApiError(body.error.message, response.status, body.error.code);
  } catch {
    // Not JSON: a proxy's error page, most likely.
  }
  return new ApiError(`The server answered ${response.status} ${response.statusText}. Try again in a moment.`, response.status);
}

async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('');
}

function base64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}
