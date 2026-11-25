import { CircleCheck, Clock, ExternalLink, Hourglass, Loader2 } from 'lucide-react';

import { CopyField } from '@/components/CopyField';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { QRCodeCanvas } from '@/components/ui/qrcode';
import { usePayment, usePaymentMethods } from '@/hooks/useCredits';
import { isConfirming, type Payment } from '@/lib/api';
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

  const bitcoin = payment.bitcoin;
  // Paid in full, or paid short and out of time for a top-up: all that's
  // left is for it to confirm.
  if (bitcoin && isConfirming(payment) && (bitcoin.received >= bitcoin.sats || payment.status === 'expired')) {
    return <Confirming payment={payment} sats={bitcoin} />;
  }

  if (payment.status === 'expired') {
    return (
      <div className="flex flex-col items-center gap-3 py-6 text-center">
        <Clock className="size-12 text-muted-foreground" aria-hidden />
        <p className="text-lg font-medium">This payment expired</p>
        <p className="text-sm text-muted-foreground">
          {payment.method === 'bitcoin'
            ? "If you sent bitcoin to it anyway, it's still watched for a week, and credited once it confirms."
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
      {payment.method === 'bitcoin' && <Bitcoin uri={payment.url} sats={payment.bitcoin} />}
      {payment.method === 'stripe' && (
        <Button asChild className="w-full">
          <a href={payment.url}>
            Continue to checkout <ExternalLink className="size-4" />
          </a>
        </Button>
      )}
      <p className="flex items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        {payment.bitcoin?.received ? 'Waiting for the rest…' : 'Waiting for payment…'}
      </p>
    </div>
  );
}

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

/** What a bitcoin payment will credit for `received` sats: its share of what it asked for. */
function creditFor(payment: Payment, received: number, sats: number): number {
  return (payment.amount * received) / sats;
}

const SATS = 100_000_000;

function Confirming({ payment, sats }: { payment: Payment; sats: NonNullable<Payment['bitcoin']> }) {
  const methods = usePaymentMethods();
  const confirmations = methods.data?.data.find((m) => m.id === 'bitcoin')?.confirmations ?? 1;
  const address = new URL(payment.url).pathname;
  const short = sats.received < sats.sats;
  return (
    <div className="flex flex-col items-center gap-3 py-6 text-center">
      <Hourglass className="size-12 text-brand motion-safe:animate-pulse" aria-hidden />
      <p className="text-lg font-medium">Payment received</p>
      <p className="text-sm text-muted-foreground">
        <span className="font-medium text-foreground">{usd(creditFor(payment, sats.received, sats.sats))}</span> is on its way
        to your balance. It's added once the transaction has{' '}
        {confirmations === 1 ? 'a confirmation' : `${confirmations} confirmations`}, usually within an hour, or longer if
        the fee was low. You can close this; it's added on its own.
      </p>
      {sats.advanced ? (
        <p className="text-sm text-muted-foreground">
          Since you've paid before, you can spend{' '}
          <span className="font-medium text-foreground">{usd(sats.advanced)}</span> of it now.
        </p>
      ) : null}
      {short && (
        <p className="text-sm text-muted-foreground">
          {sats.received.toLocaleString()} of the {sats.sats.toLocaleString()} sats asked for arrived, so it's credited for
          what was sent.
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

function Bitcoin({ uri, sats }: { uri: string; sats?: Payment['bitcoin'] }) {
  const parsed = new URL(uri);
  const address = parsed.pathname;
  // Paid short: ask for the rest.
  const rest = sats && sats.received > 0 && sats.received < sats.sats ? sats.sats - sats.received : undefined;
  const amount = rest ? (rest / SATS).toFixed(8).replace(/\.?0+$/, '') : parsed.searchParams.get('amount');
  const href = rest ? uri.replace(/([?&]amount=)[^&]*/, `$1${amount}`) : uri;
  return (
    <div className="space-y-3">
      <a href={href} className="mx-auto block w-fit rounded-lg bg-white p-2" aria-label="Open the payment in your wallet">
        <QRCodeCanvas value={href} size={240} />
      </a>
      <CopyField label="Address" value={address} />
      {amount && <CopyField label={rest ? 'Still to send (BTC)' : 'Amount (BTC)'} value={amount} />}
      <Alert>
        <AlertDescription>
          {rest
            ? `${sats!.received.toLocaleString()} of ${sats!.sats.toLocaleString()} sats have arrived. Send the rest before the payment expires, or it's credited for what was sent.`
            : 'Send exactly this amount, in one transaction. Paying less adds less credit, and paying more adds more.'}
        </AlertDescription>
      </Alert>
    </div>
  );
}
