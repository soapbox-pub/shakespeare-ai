import { useSeoMeta } from '@unhead/react';
import { Gift, Plus, ShoppingBag } from 'lucide-react';
import { useEffect, useState } from 'react';

import { AddCreditsDialog } from '@/components/AddCreditsDialog';
import { BuyGiftcardsDialog } from '@/components/BuyGiftcardsDialog';
import { GiftcardList } from '@/components/GiftcardList';
import { RedeemGiftcardDialog } from '@/components/RedeemGiftcardDialog';
import { RequireLogin } from '@/components/RequireLogin';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useBalance, usePayments } from '@/hooks/useCredits';
import type { Payment } from '@/lib/api';
import { dateTime, usd } from '@/lib/format';
import { codeFromHash } from '@/lib/giftcards';

export default function CreditsPage() {
  useSeoMeta({ title: 'Credits · Shakespeare AI' });
  return (
    <RequireLogin what="your credits">
      <Credits />
    </RequireLogin>
  );
}

function Credits() {
  const balance = useBalance();
  const [adding, setAdding] = useState(false);
  // A gift card's link opens the page ready to redeem it, whether it's
  // opened fresh or pasted into a tab that's already here.
  const [linkedCode, setLinkedCode] = useState(() => codeFromHash(location.hash));
  const [redeeming, setRedeeming] = useState(!!linkedCode);

  useEffect(() => {
    const follow = () => {
      const code = codeFromHash(location.hash);
      if (!code) return;
      setLinkedCode(code);
      setRedeeming(true);
    };
    addEventListener('hashchange', follow);
    return () => removeEventListener('hashchange', follow);
  }, []);
  const [buying, setBuying] = useState(false);
  const [resuming, setResuming] = useState<Payment>();

  const closeRedeem = (open: boolean) => {
    setRedeeming(open);
    // Leave no code behind to reopen the dialog on a refresh.
    if (!open && linkedCode) history.replaceState(null, '', location.pathname + location.search);
  };

  return (
    <div className="space-y-10">
      <h1 className="text-3xl font-semibold tracking-tight">Credits</h1>

      <Card>
        <CardHeader>
          <CardDescription>Balance</CardDescription>
          <CardTitle className="text-4xl font-semibold tabular-nums">
            {balance.isPending ? <Skeleton className="h-10 w-40" /> : balance.isError ? '—' : usd(balance.data.amount)}
          </CardTitle>
          {balance.isError && <p className="text-sm text-destructive">{balance.error.message}</p>}
        </CardHeader>
        <CardContent className="flex flex-wrap gap-2">
          <Button onClick={() => setAdding(true)}>
            <Plus className="size-4" /> Add credits
          </Button>
          <Button variant="outline" onClick={() => setRedeeming(true)}>
            <Gift className="size-4" /> Redeem a gift card
          </Button>
          <Button variant="outline" onClick={() => setBuying(true)}>
            <ShoppingBag className="size-4" /> Buy gift cards
          </Button>
        </CardContent>
      </Card>

      <PaymentHistory onResume={setResuming} />

      <GiftcardList onBuy={() => setBuying(true)} />

      <AddCreditsDialog open={adding} onOpenChange={setAdding} />
      <AddCreditsDialog open={!!resuming} onOpenChange={(open) => !open && setResuming(undefined)} payment={resuming} />
      <RedeemGiftcardDialog key={linkedCode} open={redeeming} onOpenChange={closeRedeem} initialCode={linkedCode} />
      <BuyGiftcardsDialog open={buying} onOpenChange={setBuying} />
    </div>
  );
}

const STATUS: Record<Payment['status'], { label: string; variant: 'default' | 'secondary' | 'outline' }> = {
  completed: { label: 'Paid', variant: 'default' },
  pending: { label: 'Pending', variant: 'secondary' },
  expired: { label: 'Expired', variant: 'outline' },
};

const METHOD_LABELS: Record<Payment['method'], string> = { stripe: 'Card', lightning: 'Lightning', bitcoin: 'Bitcoin' };

function PaymentHistory({ onResume }: { onResume: (payment: Payment) => void }) {
  const payments = usePayments();
  const rows = payments.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <section className="space-y-4">
      <h2 className="text-xl font-semibold tracking-tight">Payments</h2>
      {payments.isPending ? (
        <Skeleton className="h-32 rounded-xl" />
      ) : payments.isError ? (
        <p className="text-sm text-destructive">{payments.error.message}</p>
      ) : rows.length === 0 ? (
        <p className="text-muted-foreground">No payments yet.</p>
      ) : (
        <div className="rounded-xl border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Date</TableHead>
                <TableHead className="hidden sm:table-cell">Method</TableHead>
                <TableHead className="hidden text-right sm:table-cell">Credits</TableHead>
                <TableHead className="text-right">Paid</TableHead>
                <TableHead>Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((payment) => (
                <TableRow key={payment.id}>
                  <TableCell className="whitespace-nowrap">{dateTime(payment.created_at)}</TableCell>
                  <TableCell className="hidden sm:table-cell">{METHOD_LABELS[payment.method]}</TableCell>
                  <TableCell className="hidden text-right tabular-nums sm:table-cell">{usd(payment.amount)}</TableCell>
                  <TableCell className="text-right tabular-nums">{usd(payment.total)}</TableCell>
                  <TableCell>
                    {payment.status === 'pending' ? (
                      <Button variant="link" size="sm" className="h-auto p-0" onClick={() => onResume(payment)}>
                        Pay now
                      </Button>
                    ) : (
                      <Badge variant={STATUS[payment.status].variant}>{STATUS[payment.status].label}</Badge>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      {payments.hasNextPage && (
        <Button variant="outline" onClick={() => payments.fetchNextPage()} disabled={payments.isFetchingNextPage}>
          Show more
        </Button>
      )}
    </section>
  );
}
