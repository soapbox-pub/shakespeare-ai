import { CopyButton } from '@/components/CopyButton';

/** A labelled value too long to read, to copy. */
export function CopyField({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="flex items-center gap-1 rounded-md border bg-muted/40 py-0.5 pr-0.5 pl-3">
        <code className="flex-1 truncate text-sm">{value}</code>
        <CopyButton value={value} label={`Copy the ${label.toLowerCase()}`} />
      </div>
    </div>
  );
}
