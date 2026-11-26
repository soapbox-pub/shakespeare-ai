import { CircleCheck, Clock, ExternalLink, Hourglass, Loader2 } from 'lucide-react';

import { CopyField } from '@/components/CopyField';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { QRCodeCanvas } from '@/components/ui/qrcode';
import { usePayment, usePaymentMethods } from '@/hooks/useCredits';
import { coinUnits, isConfirming, type Onchain, onchain, type Payment } from '@/lib/api';
import { usd } from '@/lib/format';

/** How to pay a payment, kept up to date until it's paid or expires. */
export function PaymentView({ payment: initial }: { payment: Payment }) {
  const live = usePayment(initial.id, initial.method);
  const payment = live.data ?? initial;

  if (payment.status === 'completed') {
    return (
      <div className="flex flex-col items-center gap-3 py-6 text-center">
        <CircleCheck className="size-12 text-green-600" aria-hidden />
        <p className="text-lg font-medium">{usd(payment.amount)} added to your balance</p>
        <p className="text-sm text-muted-foreground">Thank you!</p>
      </div>
    );
  }

  const chain = onchain(payment);
  // Sent something, short or not: all that's left is for it to confirm.
  if (chain && isConfirming(payment)) {
    return <Confirming payment={payment} chain={chain} />;
  }

  if (payment.status === 'expired') {
    return (
      <div className="flex flex-col items-center gap-3 py-6 text-center">
        <Clock className="size-12 text-muted-foreground" aria-hidden />
        <p className="text-lg font-medium">This payment expired</p>
        <p className="text-sm text-muted-foreground">
          {chain
            ? `If you sent ${COIN_NAMES[chain.coin]} to it anyway, it's still watched for a week, and credited once it confirms.`
            : 'Start a new one to add credits.'}
        </p>
      </div>
    );
  }

  return (
    <div className="min-w-0 space-y-4">
      <p className="text-center text-sm text-muted-foreground">
        {usd(payment.amount)} of credits, plus a {usd(payment.fee)} fee: <span className="font-medium text-foreground">{usd(payment.total)}</span>
      </p>
      {payment.method === 'lightning' && <Lightning invoice={payment.url} />}
      {chain && <Pay uri={payment.url} chain={chain} />}
      {payment.method === 'stripe' && (
        <Button asChild className="w-full">
          <a href={payment.url}>
            Continue to checkout <ExternalLink className="size-4" />
          </a>
        </Button>
      )}
      <p className="flex items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        Waiting for payment…
      </p>
    </div>
  );
}

const COIN_NAMES: Record<Onchain['coin'], string> = { BTC: 'bitcoin', XMR: 'monero' };

/** The query parameter each kind of URI gives the amount in. */
const AMOUNT_PARAMS: Record<Onchain['coin'], string> = { BTC: 'amount', XMR: 'tx_amount' };

function Lightning({ invoice }: { invoice: string }) {
  const uri = `lightning:${invoice}`;
  return (
    <div className="space-y-3">
      <a href={uri} className="mx-auto block w-fit rounded-lg bg-white p-2" aria-label="Open the invoice in your wallet">
        <QRCodeCanvas value={uri.toUpperCase()} size={240} />
      </a>
      <CopyField label="Invoice" value={invoice} />
      <Button asChild variant="outline" className="w-full">
        <a href={uri}>Open in wallet</a>
      </Button>
    </div>
  );
}

/** What an onchain payment will credit for what's been sent: its share of what it asked for. */
function creditFor(payment: Payment, chain: Onchain): number {
  return (payment.amount * chain.received) / chain.asked;
}

function Confirming({ payment, chain }: { payment: Payment; chain: Onchain }) {
  const methods = usePaymentMethods();
  const confirmations = methods.data?.data.find((m) => m.id === payment.method)?.confirmations ?? 1;
  const address = new URL(payment.url).pathname;
  const short = chain.received < chain.asked;
  const wait = chain.coin === 'XMR' ? 'usually within a few minutes' : 'usually within an hour, or longer if the fee was low';
  return (
    <div className="flex flex-col items-center gap-3 py-6 text-center">
      <Hourglass className="size-12 text-brand motion-safe:animate-pulse" aria-hidden />
      <p className="text-lg font-medium">Payment received</p>
      <p className="text-sm text-muted-foreground">
        <span className="font-medium text-foreground">{usd(creditFor(payment, chain))}</span> is on its way to your
        balance. It's added once the transaction has {confirmations === 1 ? 'a confirmation' : `${confirmations} confirmations`},{' '}
        {wait}. You can close this; it's added on its own.
      </p>
      {chain.advanced ? (
        <p className="text-sm text-muted-foreground">
          Since you've paid before, you can spend{' '}
          <span className="font-medium text-foreground">{usd(chain.advanced)}</span> of it now.
        </p>
      ) : null}
      {short && (
        <p className="text-sm text-muted-foreground">
          {coinUnits(chain.coin, chain.received)} of the {coinUnits(chain.coin, chain.asked)} asked for arrived, so it's
          credited for what was sent.
        </p>
      )}
      {address.startsWith('bc1') && (
        <Button asChild variant="outline" size="sm">
          <a href={`https://mempool.space/address/${address}`} target="_blank" rel="noreferrer">
            View on mempool.space <ExternalLink className="size-4" />
          </a>
        </Button>
      )}
    </div>
  );
}

/** Pay a bitcoin or monero payment: a QR code of its URI, and its address and amount to copy. */
function Pay({ uri, chain }: { uri: string; chain: Onchain }) {
  const parsed = new URL(uri);
  const address = parsed.pathname;
  const amount = parsed.searchParams.get(AMOUNT_PARAMS[chain.coin]);
  return (
    <div className="space-y-3">
      <a href={uri} className="mx-auto block w-fit rounded-lg bg-white p-2" aria-label="Open the payment in your wallet">
        <QRCodeCanvas value={uri} size={240} />
      </a>
      <CopyField label="Address" value={address} />
      {amount && <CopyField label={`Amount (${chain.coin})`} value={amount} />}
      <Alert>
        <AlertDescription>
          Send this amount, in one transaction. It's credited for what arrives, at this rate: paying less adds less
          credit, and paying more adds more.
        </AlertDescription>
      </Alert>
    </div>
  );
}
