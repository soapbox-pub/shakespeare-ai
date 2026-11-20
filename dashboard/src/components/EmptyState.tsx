import type { LucideIcon } from 'lucide-react';
import type { ReactNode } from 'react';

/** What a list shows before there's anything in it. */
export function EmptyState({ icon: Icon, children }: { icon: LucideIcon; children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-xl border border-dashed px-6 py-10 text-center text-sm text-muted-foreground">
      <div className="rounded-full bg-accent p-2.5 text-primary">
        <Icon className="size-5" aria-hidden />
      </div>
      <div className="max-w-sm">{children}</div>
    </div>
  );
}
