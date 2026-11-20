import type { ReactNode } from 'react';

import { LoginArea } from '@/components/auth/LoginArea';
import { Logo } from '@/components/Logo';
import { useCurrentUser } from '@/hooks/useCurrentUser';

/** Its children for a signed-in account; for anyone else, a way to sign in. */
export function RequireLogin({ children, what }: { children: ReactNode; what: string }) {
  const { user } = useCurrentUser();
  if (user) return <>{children}</>;

  return (
    <div className="mx-auto flex max-w-lg flex-col items-center gap-4 rounded-3xl border bg-brand-soft px-6 py-14 text-center sm:mt-8">
      <Logo className="size-16 drop-shadow-md" />
      <h1 className="text-2xl font-semibold tracking-tight">Log in to see {what}</h1>
      <p className="max-w-sm text-muted-foreground">
        Your account is your Nostr key. There's no email or password, and the same key signs your API requests.
      </p>
      <LoginArea className="mt-2" />
    </div>
  );
}
