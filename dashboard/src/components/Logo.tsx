import { cn } from '@/lib/utils';

/** Shakespeare's theatre masks. */
export function Logo({ className }: { className?: string }) {
  return <img src="/logo.svg" alt="" className={cn('select-none', className)} draggable={false} />;
}
