import { Loader2 } from 'lucide-react';
import { type FormEvent, useState } from 'react';

import { CopyButton } from '@/components/CopyButton';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { useBalance, useCreateGiftcards, usePaymentMethods } from '@/hooks/useCredits';
import type { Giftcard } from '@/lib/api';
import { usd } from '@/lib/format';
import { giftcardLink } from '@/lib/giftcards';

/** Buy gift cards with credit from the balance. */
export function BuyGiftcardsDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const [bought, setBought] = useState<Giftcard[]>();

  const close = (open: boolean) => {
    onOpenChange(open);
    if (!open) setBought(undefined);
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[95dvh] overflow-y-auto sm:max-w-md">
        {bought ? <Bought cards={bought} onDone={() => close(false)} /> : <BuyForm onBought={setBought} />}
      </DialogContent>
    </Dialog>
  );
}

function BuyForm({ onBought }: { onBought: (cards: Giftcard[]) => void }) {
  const balance = useBalance();
  const methods = usePaymentMethods();
  const create = useCreateGiftcards();
  const [amount, setAmount] = useState('10');
  const [quantity, setQuantity] = useState('1');

  const minimum = methods.data?.minimum_amount ?? 0;
  const dollars = Number(amount);
  const count = Number(quantity);
  const amountValid = amount.trim() !== '' && Number.isFinite(dollars) && dollars >= minimum;
  const quantityValid = Number.isInteger(count) && count >= 1 && count <= 100;
  const total = amountValid && quantityValid ? dollars * count : 0;
  const affordable = balance.data === undefined || total <= balance.data.amount;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (amountValid && quantityValid) onBought(await create.mutateAsync({ amount: dollars, quantity: count }));
  };

  return (
    <form onSubmit={submit} className="min-w-0 space-y-4">
      <DialogHeader>
        <DialogTitle>Buy gift cards</DialogTitle>
        <DialogDescription>
          Paid for from your balance. Each card has a code anyone can redeem for its credits, here or in Shakespeare.
        </DialogDescription>
      </DialogHeader>

      <div className="grid grid-cols-2 gap-4">
        <div className="space-y-2">
          <Label htmlFor="giftcard-amount">Credits on each</Label>
          <div className="relative">
            <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground">$</span>
            <Input id="giftcard-amount" inputMode="decimal" className="pl-7" value={amount} onChange={(e) => setAmount(e.target.value)} aria-invalid={!amountValid} />
          </div>
        </div>
        <div className="space-y-2">
          <Label htmlFor="giftcard-quantity">How many</Label>
          <Input id="giftcard-quantity" inputMode="numeric" value={quantity} onChange={(e) => setQuantity(e.target.value)} aria-invalid={!quantityValid} />
        </div>
      </div>
      {!amountValid && <p className="text-sm text-destructive">A card is for at least {usd(minimum)}.</p>}
      {!quantityValid && <p className="text-sm text-destructive">Buy from 1 to 100 cards at a time.</p>}

      {total > 0 && (
        <p className="text-sm text-muted-foreground">
          {usd(total)} comes out of your balance
          {balance.data && <> of {usd(balance.data.amount)}</>}.
          {!affordable && <span className="text-destructive"> That's more than you have. Add credits first.</span>}
        </p>
      )}

      {create.isError && (
        <Alert variant="destructive">
          <AlertDescription>{create.error.message}</AlertDescription>
        </Alert>
      )}

      <DialogFooter>
        <Button type="submit" disabled={!amountValid || !quantityValid || !affordable || create.isPending}>
          {create.isPending && <Loader2 className="size-4 animate-spin" />}
          Buy {quantityValid && count > 1 ? `${count} cards` : 'card'}
        </Button>
      </DialogFooter>
    </form>
  );
}

function Bought({ cards, onDone }: { cards: Giftcard[]; onDone: () => void }) {
  const all = cards.map((card) => `${card.code}  ${usd(card.amount)}  ${giftcardLink(card.code)}`).join('\n');
  return (
    <div className="min-w-0 space-y-4">
      <DialogHeader>
        <DialogTitle>{cards.length === 1 ? 'Your gift card' : `Your ${cards.length} gift cards`}</DialogTitle>
        <DialogDescription>
          Share a code, or its link. They're listed on the Credits page too, where an unused card can be cancelled for a
          refund.
        </DialogDescription>
      </DialogHeader>
      <ul className="max-h-72 divide-y overflow-y-auto rounded-md border">
        {cards.map((card) => (
          <li key={card.id} className="flex items-center gap-2 py-1 pr-1 pl-3">
            <code className="flex-1 truncate text-sm">{card.code}</code>
            <span className="text-sm text-muted-foreground tabular-nums">{usd(card.amount)}</span>
            <CopyButton value={card.code} label={`Copy ${card.code}`} />
          </li>
        ))}
      </ul>
      <DialogFooter className="gap-2">
        {cards.length > 1 && (
          <Button variant="outline" onClick={() => navigator.clipboard.writeText(all)}>
            Copy all, with links
          </Button>
        )}
        <Button onClick={onDone}>Done</Button>
      </DialogFooter>
    </div>
  );
}
