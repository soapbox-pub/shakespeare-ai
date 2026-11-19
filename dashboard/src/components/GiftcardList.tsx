import { Link2, MoreHorizontal } from 'lucide-react';
import { useState } from 'react';

import { CopyButton } from '@/components/CopyButton';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useCancelGiftcard, useGiftcards } from '@/hooks/useCredits';
import { useToast } from '@/hooks/useToast';
import type { Giftcard } from '@/lib/api';
import { date, usd } from '@/lib/format';
import { giftcardLink } from '@/lib/giftcards';
import { cn } from '@/lib/utils';

const STATUS: Record<Giftcard['status'], { label: string; variant: 'default' | 'secondary' | 'outline' }> = {
  active: { label: 'Unused', variant: 'secondary' },
  redeemed: { label: 'Redeemed', variant: 'default' },
  cancelled: { label: 'Cancelled', variant: 'outline' },
};

/** The gift cards this account has bought. */
export function GiftcardList({ onBuy }: { onBuy: () => void }) {
  const giftcards = useGiftcards();
  const [cancelling, setCancelling] = useState<Giftcard>();
  const rows = giftcards.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <section className="space-y-4">
      <h2 className="text-xl font-semibold tracking-tight">Gift cards</h2>
      {giftcards.isPending ? (
        <Skeleton className="h-24 rounded-xl" />
      ) : giftcards.isError ? (
        <p className="text-sm text-destructive">{giftcards.error.message}</p>
      ) : rows.length === 0 ? (
        <p className="text-muted-foreground">
          You haven't bought any.{' '}
          <Button variant="link" className="h-auto p-0" onClick={onBuy}>
            Buy credits for someone else
          </Button>
          .
        </p>
      ) : (
        <div className="rounded-xl border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Code</TableHead>
                <TableHead className="hidden text-right sm:table-cell">Credits</TableHead>
                <TableHead className="hidden sm:table-cell">Bought</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((card) => (
                <TableRow key={card.id} className={cn(card.status === 'cancelled' && 'text-muted-foreground')}>
                  <TableCell>
                    <div className="flex items-center gap-1">
                      <code className={cn('text-sm', card.status === 'cancelled' && 'line-through')}>{card.code}</code>
                      {card.status === 'active' && <CopyButton value={card.code} label={`Copy ${card.code}`} />}
                    </div>
                    <div className="text-xs text-muted-foreground tabular-nums sm:hidden">{usd(card.amount)}</div>
                  </TableCell>
                  <TableCell className="hidden text-right tabular-nums sm:table-cell">{usd(card.amount)}</TableCell>
                  <TableCell className="hidden whitespace-nowrap sm:table-cell">{date(card.created_at)}</TableCell>
                  <TableCell>
                    <Badge variant={STATUS[card.status].variant}>{STATUS[card.status].label}</Badge>
                  </TableCell>
                  <TableCell>{card.status === 'active' && <CardActions card={card} onCancel={() => setCancelling(card)} />}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      {giftcards.hasNextPage && (
        <Button variant="outline" onClick={() => giftcards.fetchNextPage()} disabled={giftcards.isFetchingNextPage}>
          Show more
        </Button>
      )}
      <CancelDialog card={cancelling} onClose={() => setCancelling(undefined)} />
    </section>
  );
}

function CardActions({ card, onCancel }: { card: Giftcard; onCancel: () => void }) {
  const { toast } = useToast();
  const copyLink = async () => {
    await navigator.clipboard.writeText(giftcardLink(card.code));
    toast({ title: 'Link copied', description: 'Whoever opens it can log in and redeem the card.' });
  };

  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="size-8" aria-label={`Actions for ${card.code}`}>
          <MoreHorizontal className="size-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onClick={copyLink}>
          <Link2 className="size-4" /> Copy link
        </DropdownMenuItem>
        <DropdownMenuItem onClick={onCancel} className="text-destructive focus:text-destructive">
          Cancel and refund
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function CancelDialog({ card, onClose }: { card?: Giftcard; onClose: () => void }) {
  const cancel = useCancelGiftcard();
  const { toast } = useToast();

  const confirm = () => {
    if (!card) return;
    cancel.mutate(card.id, {
      onSuccess: onClose,
      onError: (error) => toast({ title: "Couldn't cancel the card", description: error.message, variant: 'destructive' }),
    });
  };

  return (
    <AlertDialog open={!!card} onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Cancel this gift card?</AlertDialogTitle>
          <AlertDialogDescription>
            {card && `Its ${usd(card.amount)} goes back to your balance, and the code ${card.code} stops working.`}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Keep it</AlertDialogCancel>
          <AlertDialogAction onClick={(e) => { e.preventDefault(); confirm(); }} disabled={cancel.isPending} className="bg-destructive text-white hover:bg-destructive/90">
            Cancel card
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
