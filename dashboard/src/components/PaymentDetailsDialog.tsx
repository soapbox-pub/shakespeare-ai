import { ExternalLink, Hourglass } from 'lucide-react';
import type { ReactNode } from 'react';

import { CopyField } from '@/components/CopyField';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { isConfirming, isUnsettled, METHOD_LABELS, type Payment, PAYMENT_STATUS } from '@/lib/api';
import { dateTime, usd } from '@/lib/format';
import { cn } from '@/lib/utils';

interface PaymentDetailsDialogProps {
  payment?: Payment;
  onOpenChange: (open: boolean) => void;
  /** Pick up a payment that's still open, or watch bitcoin that's confirming. */
  onResume: (payment: Payment) => void;
}

/** Everything known about one payment, as a receipt. */
export function PaymentDetailsDialog({ payment, onOpenChange, onResume }: PaymentDetailsDialogProps) {
  return (
    <Dialog open={!!payment} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[95dvh] overflow-y-auto sm:max-w-md">
        {payment && <Details payment={payment} onResume={onResume} />}
      </DialogContent>
    </Dialog>
  );
}

function Details({ payment, onResume }: { payment: Payment; onResume: (payment: Payment) => void }) {
  const confirming = isConfirming(payment);
  const status = PAYMENT_STATUS[payment.status];
  const address = payment.method === 'bitcoin' ? bitcoinAddress(payment.url) : undefined;
  const sats = payment.bitcoin;

  return (
    <>
      <DialogHeader>
        <DialogTitle className="flex items-center gap-2">
          {METHOD_LABELS[payment.method]} payment
          {confirming ? (
            <Badge variant="secondary">
              <Hourglass className="size-3" aria-hidden /> Confirming
            </Badge>
          ) : (
            <Badge variant={status.variant}>{status.label}</Badge>
          )}
        </DialogTitle>
        <DialogDescription>
          {usd(payment.total)} for {usd(payment.amount)} of credits
        </DialogDescription>
      </DialogHeader>

      <div className="min-w-0 space-y-4">
        <dl className="space-y-1 rounded-lg bg-muted/50 p-3 text-sm">
          <Row label="Credits">{usd(payment.amount)}</Row>
          <Row label="Fee">{usd(payment.fee)}</Row>
          <Row label="Total" strong>
            {usd(payment.total)}
          </Row>
        </dl>

        <dl className="space-y-1 text-sm">
          <Row label="Method">{METHOD_LABELS[payment.method]}</Row>
          <Row label="Started">{dateTime(payment.created_at)}</Row>
          {payment.completed_at ? (
            <Row label="Paid">{dateTime(payment.completed_at)}</Row>
          ) : (
            <Row label={payment.status === 'expired' ? 'Expired' : 'Expires'}>{dateTime(payment.expires_at)}</Row>
          )}
          {sats && (
            <>
              <Row label="Sats asked for">{sats.sats.toLocaleString()}</Row>
              <Row label="Sats received">{sats.received.toLocaleString()}</Row>
            </>
          )}
          {payment.description && <Row label="Description">{payment.description}</Row>}
        </dl>

        <div className="space-y-3">
          <CopyField label="Payment ID" value={payment.id} />
          {address && <CopyField label="Address" value={address} />}
          {payment.method === 'lightning' && payment.url && <CopyField label="Invoice" value={payment.url} />}
        </div>
      </div>

      {(isUnsettled(payment) || address?.startsWith('bc1')) && (
        <DialogFooter className="gap-2">
          {address?.startsWith('bc1') && (
            <Button asChild variant="outline">
              <a href={`https://mempool.space/address/${address}`} target="_blank" rel="noreferrer">
                View on mempool.space <ExternalLink className="size-4" />
              </a>
            </Button>
          )}
          {isUnsettled(payment) && (
            <Button onClick={() => onResume(payment)}>{confirming ? 'See progress' : 'Pay now'}</Button>
          )}
        </DialogFooter>
      )}
    </>
  );
}

/** The address in a BIP21 `bitcoin:` URI. */
function bitcoinAddress(uri: string): string | undefined {
  try {
    return new URL(uri).pathname || undefined;
  } catch {
    return undefined;
  }
}

function Row({ label, children, strong }: { label: string; children: ReactNode; strong?: boolean }) {
  return (
    <div className={cn('flex justify-between gap-4', strong && 'border-t pt-1 font-medium')}>
      <dt className={cn('shrink-0', !strong && 'text-muted-foreground')}>{label}</dt>
      <dd className="min-w-0 text-right tabular-nums break-words">{children}</dd>
    </div>
  );
}
