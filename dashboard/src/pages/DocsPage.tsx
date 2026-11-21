import { useSeoMeta } from '@unhead/react';

import { CopyButton } from '@/components/CopyButton';
import { Code, Contents, DocSection, Endpoint, Note, Params, Prose } from '@/components/Doc';
import { PageHeader } from '@/components/PageHeader';
import { useHashScroll } from '@/hooks/useHashScroll';

const SECTIONS = [
  { id: 'authentication', label: 'Authentication' },
  { id: 'models', label: 'Models' },
  { id: 'chat-completions', label: 'Chat Completions' },
  { id: 'credit-management', label: 'Credit Management' },
  { id: 'api-keys', label: 'API Keys' },
  { id: 'error-handling', label: 'Error Handling' },
];

export default function DocsPage() {
  const title = 'API documentation · Shakespeare AI';
  const description = "Shakespeare AI's OpenAI-compatible API: authentication with Nostr or API keys, models, chat completions, credits and errors.";
  // Given, not inferred: index.html's own og: tags would win.
  useSeoMeta({ title, description, ogTitle: title, ogDescription: description });
  useHashScroll();
  const base = `${location.origin}/v1`;

  return (
    <div className="flex gap-12">
      <div className="min-w-0 flex-1 space-y-10">
        <PageHeader title="API documentation" description="An OpenAI-compatible API, authenticated with Nostr." />

        <div className="grid gap-3 sm:grid-cols-2">
          <div className="rounded-xl border bg-card p-4">
            <div className="text-xs font-medium tracking-wide text-muted-foreground uppercase">Base URL</div>
            <div className="mt-1 flex items-center gap-1">
              <code className="flex-1 truncate font-mono text-sm font-medium">{base}</code>
              <CopyButton value={base} label="Copy the base URL" />
            </div>
          </div>
          <div className="rounded-xl border bg-card p-4">
            <div className="text-xs font-medium tracking-wide text-muted-foreground uppercase">Authentication</div>
            <p className="mt-1 text-sm leading-8">
              <a href="https://github.com/nostr-protocol/nips/blob/master/98.md" className="font-medium text-primary hover:underline">
                Nostr NIP-98
              </a>
              , or an API key for inference
            </p>
          </div>
        </div>

        <Prose className="space-y-12">
          <DocSection id="authentication" title="Authentication">
            <p>
              Shakespeare AI uses Nostr NIP-98 authentication: each request is signed with your Nostr private key. For
              inference (chat completions and image generations) you can instead use an <a href="#api-keys">API key</a>,
              made with NIP-98, that spends your credits.
            </p>

            <h3>Using with OpenAI SDK and an API key</h3>
            <Code>{`import OpenAI from "openai";

const openai = new OpenAI({
  baseURL: "${base}",
  apiKey: "sk-shakespeare-...",
});`}</Code>

            <h3>Using with OpenAI SDK and NIP-98</h3>
            <Code>{`import { NIP98Client } from "@nostrify/nostrify";
import OpenAI from "openai";

function createShakespeareAI(signer: NostrSigner): OpenAI {
  return new OpenAI({
    baseURL: "${base}",
    apiKey: "", // Not needed for NIP-98
    dangerouslyAllowBrowser: true,
    fetch(input, init) {
      const client = new NIP98Client({ signer });
      return client.fetch(input, init);
    },
  });
}`}</Code>
          </DocSection>

          <DocSection id="models" title="Models">
            <h3>List Available Models</h3>
            <Endpoint method="GET" path="/v1/models" />
            <p>Returns a list of available models and their specifications.</p>
            <h4>Response</h4>
            <Code>{`{
  "object": "list",
  "data": [
    {
      "id": "glm-4.6",
      "object": "model",
      "created": 1640995200,
      "owned_by": "system"
    },
    {
      "id": "claude-sonnet-4.5",
      "object": "model",
      "created": 1640995200,
      "owned_by": "system"
    }
  ]
}`}</Code>
          </DocSection>

          <DocSection id="chat-completions" title="Chat Completions">
            <h3>Create Chat Completion</h3>
            <Endpoint method="POST" path="/v1/chat/completions" />
            <p>Creates a completion for the chat message. Compatible with OpenAI's chat completions API.</p>

            <h4>Request Body</h4>
            <Params
              head={['Parameter', 'Type', 'Required', 'Description']}
              rows={[
                ['model', 'string', 'Yes', 'Model to use (glm-4.6, claude-sonnet-4.5, claude-opus-4.5)'],
                ['messages', 'array', 'Yes', 'Array of message objects'],
                ['temperature', 'number', 'No', 'Sampling temperature (0-2)'],
                ['max_tokens', 'integer', 'No', 'Maximum tokens to generate (or max_completion_tokens)'],
                ['stream', 'boolean', 'No', 'Whether to stream responses'],
                ['tools', 'array', 'No', 'Functions the model may call'],
                ['tool_choice', 'string or object', 'No', 'none, auto, required, or a function by name'],
                ['parallel_tool_calls', 'boolean', 'No', 'Whether the model may call several tools at once'],
                ['response_format', 'object', 'No', 'text, json_object, or json_schema'],
                ['reasoning_effort', 'string', 'No', 'none, minimal, low, medium, high or xhigh, for models that reason'],
                ['top_p, stop, seed, frequency_penalty, presence_penalty', '', 'No', 'As in OpenAI\'s API'],
              ]}
            />
            <p>
              Messages are <code>system</code>, <code>developer</code>, <code>user</code>, <code>assistant</code> (with{' '}
              <code>tool_calls</code>) or <code>tool</code>, with text and <code>image_url</code> parts. Any other field
              is ignored, and <code>n</code> isn't supported. An account can have 4 requests running at once.
            </p>

            <h4>Example Request</h4>
            <Code>{`{
  "model": "glm-4.6",
  "messages": [
    {
      "role": "user",
      "content": "Hello, how are you?"
    }
  ],
  "temperature": 0.7,
  "max_tokens": 150
}`}</Code>

            <h4>Example Response</h4>
            <Code>{`{
  "id": "chatcmpl-abc123",
  "object": "chat.completion",
  "created": 1640995200,
  "model": "glm-4.6",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": "Hello! I'm doing well, thank you for asking..."
      },
      "finish_reason": "stop"
    }
  ],
  "usage": {
    "prompt_tokens": 10,
    "completion_tokens": 20,
    "total_tokens": 30,
    "prompt_tokens_details": {
      "cached_tokens": 0,
      "cache_write_tokens": 0
    },
    "cost": 0.00033
  }
}`}</Code>
            <p>
              <code>usage.cost</code> is what the request was charged, in credits. Of <code>prompt_tokens</code>, those
              read from or written to the provider's prompt cache are in <code>prompt_tokens_details</code>, and are charged
              at the model's <code>input_cache_read</code> and <code>input_cache_write</code> prices from{' '}
              <code>/v1/models</code>. A request costs the same whichever provider serves it.
            </p>
          </DocSection>

          <DocSection id="credit-management" title="Credit Management">
            <Note>
              Credit management endpoints require NIP-98 authentication, except for listing payment methods. Premium models
              require credits, while free models (like Tybalt) don't consume credits.
            </Note>

            <h3>Get Credit Balance</h3>
            <Endpoint method="GET" path="/v1/credits" />
            <p>Retrieves the user's current credit balance.</p>
            <h4>Response</h4>
            <Code>{`{
  "object": "credits",
  "amount": 10.50
}`}</Code>

            <h3>List Payment Methods</h3>
            <Endpoint method="GET" path="/v1/credits/methods" />
            <p>
              Lists the payment methods this server accepts, with the minimum purchase in dollars and the fee charged on top
              (<code>0.1</code> is 10%). It needs no authentication. Only <code>stripe</code> takes a{' '}
              <code>redirect_url</code>; <code>bitcoin</code> says how many confirmations a payment needs before it's
              credited.
            </p>
            <h4>Response</h4>
            <Code>{`{
  "object": "list",
  "data": [
    { "object": "payment_method", "id": "stripe", "requires_redirect_url": true },
    { "object": "payment_method", "id": "lightning" },
    { "object": "payment_method", "id": "bitcoin", "confirmations": 1 }
  ],
  "has_more": false,
  "minimum_amount": 5,
  "fee": 0.1
}`}</Code>

            <h3>Add Credits</h3>
            <Endpoint method="POST" path="/v1/credits/add" />
            <p>Initiates a payment to add credits to the user's account. Supports Stripe, Lightning Network and onchain Bitcoin payments.</p>
            <h4>Request Body (Stripe)</h4>
            <Code>{`{
  "amount": 10.00,
  "method": "stripe",
  "redirect_url": "https://yourapp.com/payment/complete"
}`}</Code>
            <h4>Request Body (Lightning)</h4>
            <Code>{`{
  "amount": 10.00,
  "method": "lightning"
}`}</Code>
            <h4>Request Body (Bitcoin)</h4>
            <Code>{`{
  "amount": 10.00,
  "method": "bitcoin"
}`}</Code>
            <p>
              A Lightning payment's <code>url</code> is a bolt11 invoice. A Bitcoin payment's is a BIP21 URI with a fresh
              address and the amount, such as <code>bitcoin:bc1q...?amount=0.00011</code>; it completes once the transaction
              confirms.
            </p>
            <h4>Response</h4>
            <Code>{`{
  "object": "payment",
  "id": "pi_abc123",
  "status": "pending",
  "amount": 10.00,
  "fee": 1.00,
  "total": 11.00,
  "method": "stripe",
  "description": "Credit purchase via Stripe",
  "url": "https://checkout.stripe.com/pay/cs_...",
  "created_at": 1640995200,
  "expires_at": 1640999800
}`}</Code>

            <h3>Check Payment Status</h3>
            <Endpoint method="GET" path="/v1/credits/payments/{payment_id}" />
            <p>
              Checks the status of a payment. Payments are completed automatically once paid: by Stripe's webhook, or by
              checking the Lightning invoice or the Bitcoin address.
            </p>

            <h3>List Payments</h3>
            <Endpoint method="GET" path="/v1/credits/payments" />
            <p>Retrieves a paginated list of payments for the user.</p>
            <h4>Query Parameters</h4>
            <Params
              head={['Parameter', 'Type', 'Description']}
              rows={[
                ['limit', 'integer', 'Maximum number of payments to return (default: 20, max: 100)'],
                ['starting_after', 'string', 'Payment ID for pagination'],
                ['status', 'string', 'Filter by status: pending, completed, failed, expired'],
                ['method', 'string', 'Filter by method: stripe, lightning, bitcoin'],
              ]}
            />
          </DocSection>

          <DocSection id="api-keys" title="API Keys">
            <Note>
              API keys work only for <code>/v1/chat/completions</code> and <code>/v1/images/generations</code>, sent as{' '}
              <code>Authorization: Bearer &lt;key&gt;</code>. Managing keys, and everything else, requires NIP-98, so a
              leaked key can only spend up to its limit.
            </Note>
            <p>
              A key spends the credits of the Nostr account that made it. Its generations appear in{' '}
              <code>/v1/credits/generations</code> with its <code>api_key_id</code>. An account can have up to 100 keys
              that haven't been revoked.
            </p>

            <h3>Create API Key</h3>
            <Endpoint method="POST" path="/v1/keys" />
            <p>
              Creates a key. Every field is optional, and the body may be empty. The response's <code>key</code> is shown
              only this once: store it safely.
            </p>
            <h4>Request Body</h4>
            <Params
              head={['Parameter', 'Type', 'Description']}
              rows={[
                ['name', 'string', 'A name for the key, up to 100 characters'],
                ['limit', 'number | null', 'The most credits the key may spend (default: no limit)'],
                [
                  'limit_reset',
                  'string | null',
                  'When the limit starts again: daily, weekly or monthly (at midnight UTC, Mondays, or the 1st). Without one, the limit is for the key\'s lifetime.',
                ],
                ['expires_at', 'integer | null', 'When the key stops working, in Unix seconds'],
              ]}
            />
            <h4>Example Response</h4>
            <Code>{`{
  "object": "api_key",
  "id": "key_3f9a1c0b7e2d4a6f8b1c2d3e",
  "name": "My bot",
  "label": "sk-shakespeare-1a2b...9f8e",
  "disabled": false,
  "limit": 5.0,
  "limit_reset": "monthly",
  "limit_remaining": 5.0,
  "usage": 0.0,
  "usage_daily": 0.0,
  "usage_weekly": 0.0,
  "usage_monthly": 0.0,
  "created_at": 1795100645,
  "updated_at": 1795100645,
  "expires_at": null,
  "revoked_at": null,
  "key": "sk-shakespeare-1a2b(56 more hex characters)9f8e"
}`}</Code>

            <h3>List API Keys</h3>
            <Endpoint method="GET" path="/v1/keys" />
            <p>
              Lists your keys, newest first, revoked ones included, with <code>limit</code> and <code>starting_after</code>{' '}
              for pagination as for payments. Each is as above, without <code>key</code>.
            </p>

            <h3>Get API Key</h3>
            <Endpoint method="GET" path="/v1/keys/{key_id}" />

            <h3>Update API Key</h3>
            <Endpoint method="PATCH" path="/v1/keys/{key_id}" />
            <p>
              Changes any of <code>name</code>, <code>limit</code>, <code>limit_reset</code>, <code>expires_at</code> and{' '}
              <code>disabled</code> (true or false). Fields left out stay as they are; <code>null</code> clears a limit, its
              reset or an expiry.
            </p>

            <h3>Revoke API Key</h3>
            <Endpoint method="DELETE" path="/v1/keys/{key_id}" />
            <p>
              Revokes a key permanently. It stays in the list, with <code>revoked_at</code> set, and can't be changed or
              used again.
            </p>

            <h3>API Key Error Codes</h3>
            <Params
              head={['Code', 'Status', 'Description']}
              rows={[
                ['invalid_api_key', '401', "The key doesn't exist"],
                ['api_key_disabled', '401', "The key can't be used now"],
                ['api_key_expired', '401', "The key can't be used now"],
                ['api_key_revoked', '401', "The key can't be used now"],
                ['key_limit_exceeded', '429', 'The key has reached its spending limit'],
                ['nip98_required', '401', 'An API key was used for something other than inference'],
                ['api_key_not_found', '404', 'No such key of yours'],
                ['too_many_api_keys', '', 'Revoke a key to make another'],
              ]}
            />
          </DocSection>

          <DocSection id="error-handling" title="Error Handling">
            <p>Shakespeare AI uses standard HTTP status codes and returns errors in OpenAI-compatible format.</p>

            <h3>Common Error Codes</h3>
            <Params
              head={['HTTP Status', 'Error Code', 'Description']}
              rows={[
                ['401', 'unauthorized', 'Invalid or missing NIP-98 authentication'],
                ['400', 'invalid_request_error', 'Invalid request format or parameters'],
                ['429', 'insufficient_quota', 'Insufficient credits for premium models'],
                ['429', 'rate_limit_error', 'Too many requests running at once, or payments started this hour'],
                ['500', 'api_error', 'Internal server or upstream provider error'],
              ]}
            />

            <h3>Error Response Format</h3>
            <Code>{`{
  "error": {
    "type": "insufficient_quota",
    "code": "insufficient_credits",
    "message": "You have insufficient credits for this request",
    "param": null
  }
}`}</Code>

            <h3>Credit Management Error Codes</h3>
            <Params
              head={['Code', 'Description']}
              rows={[
                ['minimum_amount_not_met', 'Payment amount below minimum threshold'],
                ['unsupported_method', 'Payment method not supported'],
                ['missing_required_parameter', 'Required parameter missing'],
                ['payment_not_found', 'Payment ID not found'],
                ['payment_expired', 'Payment has expired'],
              ]}
            />

            <Note title="Need help?">
              Shakespeare AI is open source. Check the <a href="https://gitlab.com/soapbox-pub/shakespeare-ai">GitLab repository</a>{' '}
              for more information or to report issues.
            </Note>
          </DocSection>
        </Prose>
      </div>

      <Contents items={SECTIONS} />
    </div>
  );
}
