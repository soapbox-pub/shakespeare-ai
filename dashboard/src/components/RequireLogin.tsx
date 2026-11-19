import { KeyRound } from 'lucide-react';
import type { ReactNode } from 'react';

import { LoginArea } from '@/components/auth/LoginArea';
import { useCurrentUser } from '@/hooks/useCurrentUser';

/** Its children for a signed-in account; for anyone else, a way to sign in. */
export function RequireLogin({ children, what }: { children: ReactNode; what: string }) {
  const { user } = useCurrentUser();
  if (user) return <>{children}</>;

  return (
    <div className="mx-auto flex max-w-md flex-col items-center gap-4 py-16 text-center">
      <div className="rounded-full bg-muted p-3">
        <KeyRound className="size-6 text-muted-foreground" aria-hidden />
      </div>
      <h1 className="text-2xl font-semibold tracking-tight">Log in to see {what}</h1>
      <p className="text-muted-foreground">
        Your account is your Nostr key. There's no email or password, and the same key signs your API requests.
      </p>
      <LoginArea />
    </div>
  );
}
