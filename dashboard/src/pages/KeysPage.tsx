import { useSeoMeta } from '@unhead/react';
import { MoreHorizontal, Plus } from 'lucide-react';
import { useState } from 'react';

import { KeyDialog } from '@/components/KeyDialog';
import { RequireLogin } from '@/components/RequireLogin';
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
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from '@/components/ui/dropdown-menu';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useToast } from '@/hooks/useToast';
import { useKeys, useRevokeKey, useUpdateKey } from '@/hooks/useKeys';
import type { ApiKey } from '@/lib/api';
import { date, usd } from '@/lib/format';
import { cn } from '@/lib/utils';

export default function KeysPage() {
  useSeoMeta({ title: 'API keys · Shakespeare AI' });
  return (
    <RequireLogin what="your API keys">
      <Keys />
    </RequireLogin>
  );
}

function Keys() {
  const keys = useKeys();
  const [creating, setCreating] = useState(false);
  const [editing, setEditing] = useState<ApiKey>();
  const [revoking, setRevoking] = useState<ApiKey>();

  const all = keys.data?.pages.flatMap((page) => page.data) ?? [];
  const live = all.filter((key) => !key.revoked_at);
  const revoked = all.filter((key) => key.revoked_at);

  return (
    <div className="space-y-8">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="max-w-2xl space-y-1">
          <h1 className="text-3xl font-semibold tracking-tight">API keys</h1>
          <p className="text-muted-foreground">
            For clients that can't sign each request with Nostr. A key spends your credits on chat and image requests
            only, up to its limit, and can't touch your account in any other way.
          </p>
        </div>
        <Button onClick={() => setCreating(true)}>
          <Plus className="size-4" /> Create key
        </Button>
      </div>

      {keys.isPending ? (
        <Skeleton className="h-40 rounded-xl" />
      ) : keys.isError ? (
        <p className="text-sm text-destructive">{keys.error.message}</p>
      ) : live.length === 0 ? (
        <p className="text-muted-foreground">No keys yet. Nostr clients don't need one; anything else does.</p>
      ) : (
        <KeyTable keys={live} onEdit={setEditing} onRevoke={setRevoking} />
      )}

      {revoked.length > 0 && (
        <section className="space-y-3">
          <h2 className="text-lg font-semibold tracking-tight">Revoked</h2>
          <KeyTable keys={revoked} />
        </section>
      )}

      {keys.hasNextPage && (
        <Button variant="outline" onClick={() => keys.fetchNextPage()} disabled={keys.isFetchingNextPage}>
          Show more
        </Button>
      )}

      <KeyDialog open={creating} onOpenChange={setCreating} />
      <KeyDialog open={!!editing} onOpenChange={(open) => !open && setEditing(undefined)} apiKey={editing} />
      <RevokeDialog apiKey={revoking} onClose={() => setRevoking(undefined)} />
    </div>
  );
}

type Status = { label: string; variant: 'default' | 'secondary' | 'outline' | 'destructive' };

function status(key: ApiKey): Status {
  if (key.revoked_at) return { label: 'Revoked', variant: 'outline' };
  if (key.expires_at && key.expires_at * 1000 <= Date.now()) return { label: 'Expired', variant: 'outline' };
  if (key.disabled) return { label: 'Disabled', variant: 'secondary' };
  if (key.limit_remaining === 0) return { label: 'Limit reached', variant: 'destructive' };
  return { label: 'Active', variant: 'default' };
}

const PER: Record<string, string> = { daily: 'a day', weekly: 'a week', monthly: 'a month' };

function KeyTable({ keys, onEdit, onRevoke }: { keys: ApiKey[]; onEdit?: (key: ApiKey) => void; onRevoke?: (key: ApiKey) => void }) {
  return (
    <div className="rounded-xl border">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Key</TableHead>
            <TableHead className="hidden text-right sm:table-cell">This month</TableHead>
            <TableHead className="hidden text-right sm:table-cell">Total</TableHead>
            <TableHead className="hidden md:table-cell">Limit</TableHead>
            <TableHead className="hidden md:table-cell">Expires</TableHead>
            <TableHead>Status</TableHead>
            {onEdit && <TableHead className="w-0" />}
          </TableRow>
        </TableHeader>
        <TableBody>
          {keys.map((key) => {
            const { label, variant } = status(key);
            return (
              <TableRow key={key.id} className={cn(key.revoked_at && 'text-muted-foreground')}>
                <TableCell>
                  <div className="font-medium">{key.name || 'Unnamed key'}</div>
                  <code className="text-xs text-muted-foreground">{key.label}</code>
                  <div className="text-xs text-muted-foreground tabular-nums sm:hidden">{usd(key.usage_monthly)} this month</div>
                </TableCell>
                <TableCell className="hidden text-right tabular-nums sm:table-cell">{usd(key.usage_monthly)}</TableCell>
                <TableCell className="hidden text-right tabular-nums sm:table-cell">{usd(key.usage)}</TableCell>
                <TableCell className="hidden md:table-cell">
                  {key.limit === null ? (
                    <span className="text-muted-foreground">None</span>
                  ) : (
                    <>
                      <div className="tabular-nums">
                        {usd(key.limit)} {key.limit_reset ? PER[key.limit_reset] : 'in all'}
                      </div>
                      {!key.revoked_at && key.limit_remaining !== null && (
                        <div className="text-xs text-muted-foreground tabular-nums">{usd(key.limit_remaining)} left</div>
                      )}
                    </>
                  )}
                </TableCell>
                <TableCell className="hidden whitespace-nowrap md:table-cell">
                  {key.revoked_at ? `Revoked ${date(key.revoked_at)}` : key.expires_at ? date(key.expires_at) : <span className="text-muted-foreground">Never</span>}
                </TableCell>
                <TableCell>
                  <Badge variant={variant}>{label}</Badge>
                </TableCell>
                {onEdit && onRevoke && (
                  <TableCell>
                    <KeyActions apiKey={key} onEdit={() => onEdit(key)} onRevoke={() => onRevoke(key)} />
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </div>
  );
}

function KeyActions({ apiKey, onEdit, onRevoke }: { apiKey: ApiKey; onEdit: () => void; onRevoke: () => void }) {
  const update = useUpdateKey();
  const { toast } = useToast();

  const toggle = () =>
    update.mutate(
      { id: apiKey.id, changes: { disabled: !apiKey.disabled } },
      { onError: (error) => toast({ title: "Couldn't change the key", description: error.message, variant: 'destructive' }) },
    );

  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="size-8" aria-label={`Actions for ${apiKey.name || apiKey.label}`}>
          <MoreHorizontal className="size-4" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem onClick={onEdit}>Edit</DropdownMenuItem>
        <DropdownMenuItem onClick={toggle} disabled={update.isPending}>
          {apiKey.disabled ? 'Enable' : 'Disable'}
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={onRevoke} className="text-destructive focus:text-destructive">
          Revoke
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function RevokeDialog({ apiKey, onClose }: { apiKey?: ApiKey; onClose: () => void }) {
  const revoke = useRevokeKey();
  const { toast } = useToast();

  const confirm = () => {
    if (!apiKey) return;
    revoke.mutate(apiKey.id, {
      onSuccess: onClose,
      onError: (error) => toast({ title: "Couldn't revoke the key", description: error.message, variant: 'destructive' }),
    });
  };

  return (
    <AlertDialog open={!!apiKey} onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Revoke {apiKey?.name ? `“${apiKey.name}”` : 'this key'}?</AlertDialogTitle>
          <AlertDialogDescription>
            Anything using it stops working at once, for good. To stop it for now, disable it instead.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={(e) => { e.preventDefault(); confirm(); }} disabled={revoke.isPending} className="bg-destructive text-white hover:bg-destructive/90">
            Revoke
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
