import { BarChart3, BookOpen, KeyRound, type LucideIcon, Monitor, Moon, Plus, Sparkles, Sun, Wallet } from 'lucide-react';
import { Suspense } from 'react';
import { Link, Outlet, useMatch, useResolvedPath } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { Logo } from '@/components/Logo';
import { Skeleton } from '@/components/ui/skeleton';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import type { Theme } from '@/contexts/AppContext';
import { useBalance } from '@/hooks/useCredits';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useTheme } from '@/hooks/useTheme';
import { usd } from '@/lib/format';
import { cn } from '@/lib/utils';

const NAV: { to: string; label: string; icon: LucideIcon; end?: boolean }[] = [
  { to: '/', label: 'Models', icon: Sparkles, end: true },
  { to: '/credits', label: 'Credits', icon: Wallet },
  { to: '/keys', label: 'API keys', icon: KeyRound },
  { to: '/usage', label: 'Usage', icon: BarChart3 },
];

const item =
  'group flex h-9 items-center gap-3 rounded-lg px-2.5 text-sm font-medium text-sidebar-foreground/75 transition-colors hover:bg-sidebar-accent/60 hover:text-sidebar-foreground max-md:justify-center max-md:px-0';

/**
 * A sidebar that's always there, on the window's tint, beside the page in a
 * rounded panel. On narrow screens the sidebar keeps only its icons.
 */
export function Layout() {
  return (
    <div className="flex h-dvh overflow-hidden bg-shell">
      <aside className="flex w-16 shrink-0 flex-col gap-1 px-2 pt-3 pb-2 md:w-64 md:px-3" aria-label="Sidebar">
        <Link to="/" className="mb-4 flex h-10 items-center gap-2.5 rounded-lg px-1.5 max-md:justify-center max-md:px-0">
          <Logo className="size-8 shrink-0 drop-shadow-sm" />
          <span className="hidden text-[1.05rem] font-semibold tracking-tight md:block">
            <span className="text-brand">Shakespeare</span> <span className="text-sidebar-foreground">AI</span>
          </span>
        </Link>

        <nav className="flex flex-col gap-0.5" aria-label="Main">
          {NAV.map((entry) => (
            <NavItem key={entry.to} {...entry} />
          ))}

          <div className="mx-2.5 my-2 h-px bg-sidebar-border" />

          <NavItem to="/docs" label="API docs" icon={BookOpen} />
        </nav>

        <div className="mt-auto flex flex-col gap-2">
          <BalanceCard />
          <ThemeSwitch />
          <LoginArea variant="sidebar" />
          <div className="hidden flex-wrap gap-x-3 gap-y-1 px-2 pt-1 text-xs text-sidebar-foreground/55 md:flex">
            <Link to="/terms" className="hover:text-sidebar-foreground">Terms</Link>
            <a href="https://shakespeare.diy" className="hover:text-sidebar-foreground">shakespeare.diy</a>
            <span>Open source, AGPLv3</span>
          </div>
        </div>
      </aside>

      <main
        id="main"
        className="my-2 mr-2 min-w-0 flex-1 overflow-y-auto rounded-2xl border bg-background shadow-panel [scrollbar-gutter:stable] md:my-3 md:mr-3"
      >
        <div className="mx-auto w-full max-w-5xl px-5 py-8 sm:px-10 sm:py-12">
          <Suspense>
            <Outlet />
          </Suspense>
        </div>
      </main>
    </div>
  );
}

function NavItem({ to, label, icon: Icon, end }: (typeof NAV)[number]) {
  const path = useResolvedPath(to);
  const active = !!useMatch({ path: path.pathname, end: !!end });

  return (
    <SidebarTip label={label}>
      <Link
        to={to}
        aria-current={active ? 'page' : undefined}
        className={cn(item, active && 'bg-sidebar-accent text-sidebar-accent-foreground shadow-xs hover:bg-sidebar-accent')}
      >
        <Icon className={cn('size-4.5 shrink-0 transition-colors', active ? 'text-primary' : 'opacity-70 group-hover:opacity-100')} aria-hidden />
        <span className="max-md:sr-only">{label}</span>
      </Link>
    </SidebarTip>
  );
}

/** On the narrow sidebar, where only icons show, a label on hover. */
function SidebarTip({ label, children }: { label: string; children: React.ReactElement }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side="right" className="md:hidden">
        {label}
      </TooltipContent>
    </Tooltip>
  );
}

function BalanceCard() {
  const { user } = useCurrentUser();
  const balance = useBalance();
  if (!user) return null;

  return (
    <Link
      to="/credits"
      className="group hidden rounded-xl border border-sidebar-border bg-background/60 bg-brand-soft p-3 shadow-xs transition-colors hover:bg-background md:block"
    >
      <div className="flex items-center justify-between text-xs font-medium text-muted-foreground">
        Balance
        <span className="flex items-center gap-0.5 text-primary opacity-0 transition-opacity group-hover:opacity-100">
          <Plus className="size-3" /> Add
        </span>
      </div>
      <div className="mt-1 text-xl font-semibold tracking-tight tabular-nums">
        {balance.isPending ? <Skeleton className="h-7 w-24" /> : balance.isError ? '—' : usd(balance.data.amount)}
      </div>
      {balance.data?.pending ? (
        <div className="text-xs text-muted-foreground tabular-nums">+{usd(balance.data.pending)} pending</div>
      ) : null}
    </Link>
  );
}

const THEMES: { value: Theme; label: string; icon: LucideIcon }[] = [
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
  { value: 'system', label: 'System', icon: Monitor },
];

function ThemeSwitch() {
  const { theme, setTheme } = useTheme();
  const current = THEMES.find((t) => t.value === theme) ?? THEMES[2];
  const next = THEMES[(THEMES.indexOf(current) + 1) % THEMES.length];

  return (
    <>
      {/* Narrow: one button that steps through the themes. */}
      <SidebarTip label={`Theme: ${current.label}`}>
        <button type="button" onClick={() => setTheme(next.value)} className={cn(item, 'md:hidden')} aria-label={`Theme: ${current.label}. Switch to ${next.label}`}>
          <current.icon className="size-4.5" aria-hidden />
        </button>
      </SidebarTip>

      {/* Wide: a segmented control. */}
      <div className="hidden rounded-lg bg-sidebar-accent/50 p-0.5 md:flex" role="radiogroup" aria-label="Theme">
        {THEMES.map(({ value, label, icon: Icon }) => (
          <button
            key={value}
            type="button"
            role="radio"
            aria-checked={theme === value}
            onClick={() => setTheme(value)}
            className={cn(
              'flex h-7 flex-1 items-center justify-center gap-1.5 rounded-md text-xs font-medium text-sidebar-foreground/60 transition-all hover:text-sidebar-foreground',
              theme === value && 'bg-background text-sidebar-foreground shadow-xs',
            )}
          >
            <Icon className="size-3.5" aria-hidden />
            {label}
          </button>
        ))}
      </div>
    </>
  );
}
