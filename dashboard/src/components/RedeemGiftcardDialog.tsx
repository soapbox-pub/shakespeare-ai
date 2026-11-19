import { CircleCheck, Loader2 } from 'lucide-react';
import { type FormEvent, useState } from 'react';

import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { useRedeemGiftcard } from '@/hooks/useCredits';
import { usd } from '@/lib/format';

export function RedeemGiftcardDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const redeem = useRedeemGiftcard();
  const [code, setCode] = useState('');

  const close = (open: boolean) => {
    onOpenChange(open);
    if (!open) {
      setCode('');
      redeem.reset();
    }
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (code.trim()) redeem.mutate(code.trim());
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Redeem a gift card</DialogTitle>
          <DialogDescription>Its credits are added to your balance.</DialogDescription>
        </DialogHeader>
        {redeem.isSuccess ? (
          <div className="flex flex-col items-center gap-3 py-6 text-center">
            <CircleCheck className="size-12 text-green-600" aria-hidden />
            <p className="text-lg font-medium">{usd(redeem.data.amount)} added to your balance</p>
            <Button variant="outline" onClick={() => close(false)}>Done</Button>
          </div>
        ) : (
          <form onSubmit={submit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="giftcard-code">Code</Label>
              <Input
                id="giftcard-code"
                autoFocus
                autoComplete="off"
                spellCheck={false}
                className="font-mono uppercase"
                value={code}
                onChange={(e) => setCode(e.target.value)}
              />
            </div>
            {redeem.isError && (
              <Alert variant="destructive">
                <AlertDescription>{redeem.error.message}</AlertDescription>
              </Alert>
            )}
            <Button type="submit" className="w-full" disabled={!code.trim() || redeem.isPending}>
              {redeem.isPending && <Loader2 className="size-4 animate-spin" />}
              Redeem
            </Button>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
