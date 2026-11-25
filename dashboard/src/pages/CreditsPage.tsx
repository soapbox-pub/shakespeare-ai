import { useSeoMeta } from '@unhead/react';
import { Gift, Hourglass, Plus, Receipt, ShoppingBag } from 'lucide-react';
import { useEffect, useState } from 'react';

import { AddCreditsDialog } from '@/components/AddCreditsDialog';
import { BuyGiftcardsDialog } from '@/components/BuyGiftcardsDialog';
import { EmptyState } from '@/components/EmptyState';
import { GiftcardList } from '@/components/GiftcardList';
import { PageHeader, SectionHeader } from '@/components/PageHeader';
import { RedeemGiftcardDialog } from '@/components/RedeemGiftcardDialog';
import { RequireLogin } from '@/components/RequireLogin';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useBalance, usePayments } from '@/hooks/useCredits';
import { isConfirming, type Payment } from '@/lib/api';
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
      <PageHeader title="Credits" description="What your requests spend. Add more whenever you like; they don't expire." />

      <section className="flex flex-wrap items-end justify-between gap-6 rounded-3xl border bg-brand-soft p-6 sm:p-8">
        <div className="space-y-1">
          <div className="text-sm font-medium text-muted-foreground">Balance</div>
          <div className="text-5xl font-bold tracking-tight tabular-nums">
            {balance.isPending ? (
              <Skeleton className="h-12 w-48" />
            ) : balance.isError ? (
              '—'
            ) : (
              <span className="text-brand">{usd(balance.data.amount)}</span>
            )}
          </div>
          {balance.data?.pending ? (
            <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
              <Hourglass className="size-4 text-brand" aria-hidden />
              <span>
                <span className="font-medium text-foreground tabular-nums">+{usd(balance.data.pending)}</span> pending, added
                once your bitcoin confirms
              </span>
            </p>
          ) : null}
          {balance.isError && <p className="text-sm text-destructive">{balance.error.message}</p>}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="brand" size="lg" className="rounded-full px-5" onClick={() => setAdding(true)}>
            <Plus /> Add credits
          </Button>
          <Button variant="outline" size="lg" className="rounded-full bg-background/70 px-5" onClick={() => setRedeeming(true)}>
            <Gift /> Redeem
          </Button>
          <Button variant="outline" size="lg" className="rounded-full bg-background/70 px-5" onClick={() => setBuying(true)}>
            <ShoppingBag /> Buy gift cards
          </Button>
        </div>
      </section>

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
      <SectionHeader title="Payments" />
      {payments.isPending ? (
        <Skeleton className="h-32 rounded-xl" />
      ) : payments.isError ? (
        <p className="text-sm text-destructive">{payments.error.message}</p>
      ) : rows.length === 0 ? (
        <EmptyState icon={Receipt}>No payments yet. Credits you add show up here.</EmptyState>
      ) : (
        <div className="overflow-hidden rounded-xl border bg-card shadow-xs">
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
                    {isConfirming(payment) ? (
                      <button
                        type="button"
                        className="rounded-md focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
                        onClick={() => onResume(payment)}
                      >
                        <Badge variant="secondary">
                          <Hourglass className="size-3" aria-hidden /> Confirming
                        </Badge>
                      </button>
                    ) : payment.status === 'pending' ? (
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
