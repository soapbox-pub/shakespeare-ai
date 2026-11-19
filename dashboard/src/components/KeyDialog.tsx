import { Loader2, TriangleAlert } from 'lucide-react';
import { type FormEvent, useState } from 'react';

import { CopyButton } from '@/components/CopyButton';
import { CopyField } from '@/components/CopyField';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { useModels } from '@/hooks/useCredits';
import { useCreateKey, useUpdateKey } from '@/hooks/useKeys';
import type { ApiKey, ApiKeyChanges, LimitReset } from '@/lib/api';
import { date } from '@/lib/format';

const DAY = 24 * 60 * 60;
const EXPIRIES: { value: string; label: string; days?: number }[] = [
  { value: 'never', label: 'Never' },
  { value: '7', label: 'In 7 days', days: 7 },
  { value: '30', label: 'In 30 days', days: 30 },
  { value: '90', label: 'In 90 days', days: 90 },
  { value: '365', label: 'In a year', days: 365 },
];

interface KeyDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The key to change; without one, the dialog makes a new key. */
  apiKey?: ApiKey;
}

/** Make a key, or change one. A new key is shown once, here, and never again. */
export function KeyDialog({ open, onOpenChange, apiKey }: KeyDialogProps) {
  const [created, setCreated] = useState<string>();

  const close = (open: boolean) => {
    onOpenChange(open);
    if (!open) setCreated(undefined);
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[95dvh] overflow-y-auto sm:max-w-lg">
        {created ? (
          <CreatedKey token={created} onDone={() => close(false)} />
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>{apiKey ? 'Edit key' : 'Create an API key'}</DialogTitle>
              <DialogDescription>
                A key spends your credits on chat and image requests, and can do nothing else. A limit caps what it
                can spend.
              </DialogDescription>
            </DialogHeader>
            {/* Remounted per key, so the form starts from that key's values. */}
            <KeyForm key={apiKey?.id ?? 'new'} apiKey={apiKey} onSaved={(token) => (token ? setCreated(token) : close(false))} />
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function KeyForm({ apiKey, onSaved }: { apiKey?: ApiKey; onSaved: (token?: string) => void }) {
  const create = useCreateKey();
  const update = useUpdateKey();
  const saving = create.isPending || update.isPending;
  const error = create.error ?? update.error;

  const [name, setName] = useState(apiKey?.name ?? '');
  const [limit, setLimit] = useState(apiKey?.limit != null ? String(apiKey.limit) : '');
  const [reset, setReset] = useState<LimitReset | 'never'>(apiKey?.limit_reset ?? 'never');
  const [expiry, setExpiry] = useState(apiKey?.expires_at ? 'keep' : 'never');

  const limitValue = limit.trim() === '' ? null : Number(limit);
  const limitValid = limitValue === null || (Number.isFinite(limitValue) && limitValue >= 0);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!limitValid) return;
    const changes: ApiKeyChanges = {
      name: name.trim(),
      limit: limitValue,
      limit_reset: limitValue === null || reset === 'never' ? null : reset,
    };
    const days = EXPIRIES.find((e) => e.value === expiry)?.days;
    if (expiry === 'never') changes.expires_at = null;
    else if (days) changes.expires_at = Math.floor(Date.now() / 1000) + days * DAY;

    if (apiKey) {
      await update.mutateAsync({ id: apiKey.id, changes });
      onSaved();
    } else {
      onSaved((await create.mutateAsync(changes)).key);
    }
  };

  return (
    <form onSubmit={submit} className="min-w-0 space-y-4">
      <div className="space-y-2">
        <Label htmlFor="key-name">Name</Label>
        <Input id="key-name" placeholder="What it's for" maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
      </div>

      <div className="grid gap-4 sm:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="key-limit">Spending limit</Label>
          <div className="relative">
            <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground">$</span>
            <Input
              id="key-limit"
              inputMode="decimal"
              placeholder="No limit"
              className="pl-7"
              value={limit}
              onChange={(e) => setLimit(e.target.value)}
              aria-invalid={!limitValid}
            />
          </div>
          {!limitValid && <p className="text-sm text-destructive">A limit is a number of dollars, or empty for none.</p>}
        </div>
        <div className="space-y-2">
          <Label htmlFor="key-reset">Limit resets</Label>
          <Select value={reset} onValueChange={(value) => setReset(value as LimitReset | 'never')} disabled={limitValue === null}>
            <SelectTrigger id="key-reset" className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="never">Never: a lifetime limit</SelectItem>
              <SelectItem value="daily">Daily</SelectItem>
              <SelectItem value="weekly">Weekly, on Mondays</SelectItem>
              <SelectItem value="monthly">Monthly, on the 1st</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </div>

      <div className="space-y-2">
        <Label htmlFor="key-expiry">Expires</Label>
        <Select value={expiry} onValueChange={setExpiry}>
          <SelectTrigger id="key-expiry" className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {apiKey?.expires_at && <SelectItem value="keep">{date(apiKey.expires_at)}, as it is</SelectItem>}
            {EXPIRIES.map((e) => (
              <SelectItem key={e.value} value={e.value}>{e.label}</SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertDescription>{error.message}</AlertDescription>
        </Alert>
      )}

      <DialogFooter>
        <Button type="submit" disabled={!limitValid || saving}>
          {saving && <Loader2 className="size-4 animate-spin" />}
          {apiKey ? 'Save' : 'Create key'}
        </Button>
      </DialogFooter>
    </form>
  );
}

function CreatedKey({ token, onDone }: { token: string; onDone: () => void }) {
  const models = useModels();
  const model = models.data?.find((m) => m.type === 'chat')?.id ?? 'MODEL';
  const endpoint = `${location.origin}/v1`;
  const example = [
    `curl ${endpoint}/chat/completions \\`,
    `  -H "Authorization: Bearer ${token}" \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -d '{"model": "${model}", "messages": [{"role": "user", "content": "Hello!"}]}'`,
  ].join('\n');

  return (
    <div className="min-w-0 space-y-4">
      <DialogHeader>
        <DialogTitle>Your new key</DialogTitle>
        <DialogDescription>Use it with any OpenAI-compatible client: give it this server's endpoint and the key.</DialogDescription>
      </DialogHeader>
      <Alert>
        <TriangleAlert className="size-4" />
        <AlertDescription>Copy it now. It's shown only this once, and can't be found again.</AlertDescription>
      </Alert>
      <CopyField label="Key" value={token} />
      <CopyField label="Endpoint" value={endpoint} />
      <div className="space-y-1">
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">Try it</span>
          <CopyButton value={example} label="Copy the example" />
        </div>
        <pre className="overflow-x-auto rounded-md border bg-muted/40 p-3 text-xs">{example}</pre>
      </div>
      <DialogFooter>
        <Button onClick={onDone}>Done</Button>
      </DialogFooter>
    </div>
  );
}
