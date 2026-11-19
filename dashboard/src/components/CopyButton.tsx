import { Check, Copy } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';

export function CopyButton({ value, label = 'Copy' }: { value: string; label?: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    await navigator.clipboard.writeText(value);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <Button type="button" variant="ghost" size="icon" className="size-8 shrink-0" onClick={copy} aria-label={label}>
      {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
    </Button>
  );
}
