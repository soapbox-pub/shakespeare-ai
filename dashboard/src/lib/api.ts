import type { NostrSigner } from '@nostrify/nostrify';

/** NIP-98 HTTP auth event kind. */
const NIP98_KIND = 27235;

export type PaymentMethod = 'stripe' | 'lightning' | 'bitcoin';

/** What credits can be bought with here, and on what terms. */
export interface PaymentMethods {
  data: { id: PaymentMethod }[];
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
}

export interface Generation {
  id: string;
  model: string;
  created: number;
  usage: { prompt_tokens: number; completion_tokens: number; total_tokens: number };
  cost: number;
  /** Tokens counted from bytes, because the client hung up before the provider reported them. */
  estimated: boolean;
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
    return (await response.json()) as T;
  }

  balance(): Promise<{ amount: number }> {
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
}

function page(startingAfter: string | undefined): string {
  const params = new URLSearchParams({ limit: '50' });
  if (startingAfter) params.set('starting_after', startingAfter);
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
