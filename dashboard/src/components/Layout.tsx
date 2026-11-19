import { Feather } from 'lucide-react';
import { Suspense } from 'react';
import { Link, NavLink, Outlet } from 'react-router-dom';

import { LoginArea } from '@/components/auth/LoginArea';
import { cn } from '@/lib/utils';

const NAV = [
  { to: '/', label: 'Models', end: true },
  { to: '/credits', label: 'Credits', end: false },
  { to: '/usage', label: 'Usage', end: false },
];

export function Layout() {
  return (
    <div className="flex min-h-screen flex-col">
      <header className="sticky top-0 z-30 border-b bg-background/80 backdrop-blur">
        <div className="mx-auto flex h-14 max-w-5xl items-center gap-3 px-4 sm:gap-6 sm:px-6">
          <Link to="/" className="flex items-center gap-2 font-semibold tracking-tight">
            <Feather className="size-5" aria-hidden />
            <span className="hidden sm:inline">Shakespeare AI</span>
          </Link>
          <nav className="flex items-center gap-1 text-sm" aria-label="Main">
            {NAV.map((item) => (
              <NavLink
                key={item.to}
                to={item.to}
                end={item.end}
                className={({ isActive }) =>
                  cn(
                    'rounded-md px-2 py-1.5 transition-colors hover:text-foreground sm:px-3',
                    isActive ? 'bg-accent text-foreground' : 'text-muted-foreground',
                  )
                }
              >
                {item.label}
              </NavLink>
            ))}
            <a href="/docs" className="hidden rounded-md px-3 py-1.5 text-muted-foreground transition-colors hover:text-foreground sm:block">
              Docs
            </a>
          </nav>
          <LoginArea className="ml-auto shrink-0" />
        </div>
      </header>

      <main className="mx-auto w-full max-w-5xl flex-1 px-4 py-8 sm:px-6 sm:py-12">
        <Suspense>
          <Outlet />
        </Suspense>
      </main>

      <footer className="border-t">
        <div className="mx-auto flex max-w-5xl flex-wrap gap-x-6 gap-y-2 px-4 py-6 text-sm text-muted-foreground sm:px-6">
          <a href="/docs" className="hover:text-foreground">API documentation</a>
          <a href="/terms" className="hover:text-foreground">Terms of service</a>
          <a href="https://shakespeare.diy" className="hover:text-foreground">shakespeare.diy</a>
          <span className="sm:ml-auto">Open source under the AGPLv3</span>
        </div>
      </footer>
    </div>
  );
}
