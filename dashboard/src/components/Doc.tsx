import type { ReactNode } from 'react';

import { CopyButton } from '@/components/CopyButton';
import { cn } from '@/lib/utils';

/** Long-form text: the API docs and the terms. */
export function Prose({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cn('prose-doc', className)}>{children}</div>;
}

/** A top-level section, which a table of contents can link to. */
export function DocSection({ id, title, children }: { id?: string; title: string; children: ReactNode }) {
  return (
    <section id={id} className="scroll-mt-8 border-t pt-10 first:border-t-0 first:pt-0">
      <h2 className="mb-4 text-2xl font-semibold tracking-tight">{title}</h2>
      {children}
    </section>
  );
}

const METHOD_COLORS: Record<string, string> = {
  GET: 'bg-sky-500/12 text-sky-700 dark:text-sky-300',
  POST: 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300',
  PATCH: 'bg-amber-500/15 text-amber-700 dark:text-amber-300',
  DELETE: 'bg-red-500/12 text-red-700 dark:text-red-300',
};

/** An endpoint's method and path. */
export function Endpoint({ method, path }: { method: string; path: string }) {
  return (
    <div className="not-prose my-4 flex items-center gap-3 rounded-xl border bg-muted/50 px-3 py-2.5 font-mono text-sm">
      <span className={cn('rounded-md px-2 py-0.5 text-xs font-semibold tracking-wide', METHOD_COLORS[method])}>{method}</span>
      <span className="truncate">{path}</span>
    </div>
  );
}

/** A block of code, to copy. */
export function Code({ children, title }: { children: string; title?: string }) {
  return (
    <div className="not-prose group relative my-4 overflow-hidden rounded-xl border bg-[hsl(262_18%_10%)] text-[hsl(260_20%_88%)] dark:bg-[hsl(262_16%_6%)]">
      {title && <div className="border-b border-white/10 px-4 py-2 text-xs font-medium text-white/50">{title}</div>}
      <pre className="overflow-x-auto px-4 py-3.5 text-[0.82rem] leading-relaxed">
        <code>{children}</code>
      </pre>
      <div className="absolute top-1.5 right-1.5 text-white/60 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100 [&_button:hover]:bg-white/10 [&_button:hover]:text-white">
        <CopyButton value={children} label="Copy the code" />
      </div>
    </div>
  );
}

/** A table of parameters or codes, each row's first cell in code. */
export function Params({ head, rows }: { head: string[]; rows: ReactNode[][] }) {
  return (
    <div className="not-prose my-4 overflow-x-auto rounded-xl border">
      <table className="w-full text-sm">
        <thead className="bg-muted/60">
          <tr>
            {head.map((h) => (
              <th key={h} className="px-4 py-2.5 text-left text-xs font-medium tracking-wide text-muted-foreground uppercase">
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i} className="border-t align-top">
              {row.map((cell, j) => (
                <td key={j} className={cn('px-4 py-3', j === 0 ? 'whitespace-nowrap' : 'text-muted-foreground', head[j] === 'Type' && 'whitespace-nowrap')}>
                  {j === 0 && typeof cell === 'string' ? <code className="font-mono text-[0.82rem] text-foreground">{cell}</code> : cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Something to notice. */
export function Note({ title = 'Note', children }: { title?: string; children: ReactNode }) {
  return (
    <div className="my-4 rounded-xl border border-primary/20 bg-accent/60 px-4 py-3 text-sm [&_p]:my-0">
      <strong className="font-semibold text-accent-foreground">{title}:</strong> {children}
    </div>
  );
}

/** Links to a page's sections, beside it on wide screens. */
export function Contents({ items }: { items: { id: string; label: string }[] }) {
  return (
    <nav aria-label="On this page" className="sticky top-8 hidden w-48 shrink-0 self-start xl:block">
      <div className="mb-3 text-xs font-medium tracking-wide text-muted-foreground uppercase">On this page</div>
      <ul className="space-y-1 border-l text-sm">
        {items.map(({ id, label }) => (
          <li key={id}>
            <a href={`#${id}`} className="-ml-px block border-l border-transparent py-1 pl-4 text-muted-foreground transition-colors hover:border-primary hover:text-foreground">
              {label}
            </a>
          </li>
        ))}
      </ul>
    </nav>
  );
}
