import { CircleCheck, Clock, ExternalLink, Loader2 } from 'lucide-react';

import { CopyButton } from '@/components/CopyButton';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { QRCodeCanvas } from '@/components/ui/qrcode';
import { usePayment } from '@/hooks/useCredits';
import type { Payment } from '@/lib/api';
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
      {payment.method === 'bitcoin' && <Bitcoin uri={payment.url} />}
      {payment.method === 'stripe' && (
        <Button asChild className="w-full">
          <a href={payment.url}>
            Continue to checkout <ExternalLink className="size-4" />
          </a>
        </Button>
      )}
      <p className="flex items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        {payment.method === 'bitcoin' ? 'Waiting for the payment to confirm…' : 'Waiting for payment…'}
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
      <Field label="Invoice" value={invoice} />
      <Button asChild variant="outline" className="w-full">
        <a href={uri}>Open in wallet</a>
      </Button>
    </div>
  );
}

function Bitcoin({ uri }: { uri: string }) {
  const parsed = new URL(uri);
  const address = parsed.pathname;
  const amount = parsed.searchParams.get('amount');
  return (
    <div className="space-y-3">
      <a href={uri} className="mx-auto block w-fit rounded-lg bg-white p-2" aria-label="Open the payment in your wallet">
        <QRCodeCanvas value={uri} size={240} />
      </a>
      <Field label="Address" value={address} />
      {amount && <Field label="Amount (BTC)" value={amount} />}
      <Alert>
        <AlertDescription>
          Send exactly this amount, in one transaction. Paying less adds no credits, and paying more adds only what
          you bought.
        </AlertDescription>
      </Alert>
    </div>
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="flex items-center gap-1 rounded-md border bg-muted/40 py-0.5 pr-0.5 pl-3">
        <code className="flex-1 truncate text-sm">{value}</code>
        <CopyButton value={value} label={`Copy the ${label.toLowerCase()}`} />
      </div>
    </div>
  );
}
