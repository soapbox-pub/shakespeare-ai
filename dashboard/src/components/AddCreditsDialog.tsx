import { Bitcoin, CreditCard, Loader2, Zap } from 'lucide-react';
import { type ComponentType, type FormEvent, useState } from 'react';

import { MoneroIcon } from '@/components/MoneroIcon';
import { PaymentView } from '@/components/PaymentView';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group';
import { useAddCredits, usePaymentMethods } from '@/hooks/useCredits';
import type { Payment, PaymentMethod } from '@/lib/api';
import { usd } from '@/lib/format';
import { cn } from '@/lib/utils';

const PRESETS = [5, 10, 25, 50, 100];

const METHODS: Record<PaymentMethod, { label: string; icon: ComponentType<{ className?: string }> }> = {
  stripe: { label: 'Card', icon: CreditCard },
  lightning: { label: 'Lightning', icon: Zap },
  bitcoin: { label: 'Bitcoin', icon: Bitcoin },
  monero: { label: 'Monero', icon: MoneroIcon },
};

interface AddCreditsDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** A payment already started, to pick up where it was left. */
  payment?: Payment;
}

export function AddCreditsDialog({ open, onOpenChange, payment }: AddCreditsDialogProps) {
  const [started, setStarted] = useState<Payment>();
  const shown = payment ?? started;

  const close = (open: boolean) => {
    onOpenChange(open);
    if (!open) setStarted(undefined);
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[95dvh] overflow-y-auto sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{shown ? `Pay with ${METHODS[shown.method].label}` : 'Add credits'}</DialogTitle>
          <DialogDescription>
            {shown
              ? shown.method === 'bitcoin' || shown.method === 'monero'
                ? 'Credits are added once the payment confirms.'
                : 'Credits are added as soon as the payment arrives.'
              : 'Credits pay for what you use, at the prices listed for each model.'}
          </DialogDescription>
        </DialogHeader>
        {shown ? <PaymentView payment={shown} /> : <AddCreditsForm onStarted={setStarted} />}
      </DialogContent>
    </Dialog>
  );
}

function AddCreditsForm({ onStarted }: { onStarted: (payment: Payment) => void }) {
  const options = usePaymentMethods();
  const add = useAddCredits();
  const [amount, setAmount] = useState('10');
  const [method, setMethod] = useState<PaymentMethod>();

  if (options.isPending) {
    return <Loader2 className="mx-auto my-8 size-6 animate-spin text-muted-foreground" />;
  }
  if (options.isError) {
    return <p className="text-sm text-muted-foreground">Couldn't load the ways to pay. Close this and try again.</p>;
  }
  const { minimum_amount: minimum, fee } = options.data;
  const methods = options.data.data.map((m) => m.id).filter((id) => id in METHODS);
  if (methods.length === 0) {
    return <p className="text-sm text-muted-foreground">This server isn't taking payments at the moment.</p>;
  }

  const chosen = method ?? methods[0];
  const dollars = Number(amount);
  const valid = amount.trim() !== '' && Number.isFinite(dollars) && dollars >= minimum;
  const feeAmount = valid ? Math.round(dollars * fee * 100) / 100 : 0;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!valid) return;
    const payment = await add.mutateAsync({
      amount: dollars,
      method: chosen,
      // Stripe sends the buyer back here, paid or not.
      redirect_url: chosen === 'stripe' ? `${location.origin}/credits` : undefined,
    });
    if (payment.method === 'stripe') {
      location.assign(payment.url);
    } else {
      onStarted(payment);
    }
  };

  return (
    <form onSubmit={submit} className="min-w-0 space-y-5">
      <div className="space-y-2">
        <Label htmlFor="amount">Amount</Label>
        <div className="flex flex-wrap gap-2">
          {PRESETS.filter((preset) => preset >= minimum).map((preset) => (
            <Button
              key={preset}
              type="button"
              variant={dollars === preset ? 'default' : 'outline'}
              size="sm"
              onClick={() => setAmount(String(preset))}
            >
              ${preset}
            </Button>
          ))}
        </div>
        <div className="relative">
          <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground">$</span>
          <Input
            id="amount"
            inputMode="decimal"
            className="pl-7"
            value={amount}
            onChange={(e) => setAmount(e.target.value)}
            aria-invalid={!valid}
          />
        </div>
        {!valid && <p className="text-sm text-destructive">The least you can add is {usd(minimum)}.</p>}
      </div>

      <div className="space-y-2">
        <Label>Pay with</Label>
        <ToggleGroup
          type="single"
          variant="outline"
          className="w-full"
          value={chosen}
          onValueChange={(value) => value && setMethod(value as PaymentMethod)}
        >
          {methods.map((m) => {
            const { label, icon: Icon } = METHODS[m];
            return (
              <ToggleGroupItem key={m} value={m} className={cn('flex-1 gap-2')}>
                <Icon className="size-4" /> {label}
              </ToggleGroupItem>
            );
          })}
        </ToggleGroup>
      </div>

      {valid && (
        <dl className="space-y-1 rounded-lg bg-muted/50 p-3 text-sm">
          <Row label="Credits" value={usd(dollars)} />
          <Row label={`Fee (${Math.round(fee * 1000) / 10}%)`} value={usd(feeAmount)} />
          <Row label="Total" value={usd(dollars + feeAmount)} strong />
        </dl>
      )}

      {add.isError && (
        <Alert variant="destructive">
          <AlertDescription>{add.error.message}</AlertDescription>
        </Alert>
      )}

      <Button type="submit" className="w-full" disabled={!valid || add.isPending}>
        {add.isPending && <Loader2 className="size-4 animate-spin" />}
        {chosen === 'stripe' ? 'Continue to checkout' : 'Continue'}
      </Button>
    </form>
  );
}

function Row({ label, value, strong }: { label: string; value: string; strong?: boolean }) {
  return (
    <div className={cn('flex justify-between', strong && 'border-t pt-1 font-medium')}>
      <dt className={cn(!strong && 'text-muted-foreground')}>{label}</dt>
      <dd className="tabular-nums">{value}</dd>
    </div>
  );
}
