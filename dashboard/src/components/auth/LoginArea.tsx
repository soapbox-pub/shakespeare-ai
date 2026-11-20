import { LogIn } from 'lucide-react';
import { useState } from 'react';
import { Button } from '@/components/ui/button.tsx';
import AuthDialog from './AuthDialog';
import { QuickLoginDialog } from './QuickLoginDialog';
import { useLoggedInAccounts } from '@/hooks/useLoggedInAccounts';
import { AccountSwitcher } from './AccountSwitcher';
import { cn } from '@/lib/utils';

/** Minimal shape of the NIP-07 provider injected at `window.nostr`. */
interface Nip07Provider {
  getPublicKey(): Promise<string>;
}

function getNip07Provider(): Nip07Provider | undefined {
  if (typeof window === 'undefined' || !('nostr' in window)) return undefined;
  const provider = (window as { nostr?: unknown }).nostr;
  if (
    provider &&
    typeof (provider as Nip07Provider).getPublicKey === 'function'
  ) {
    return provider as Nip07Provider;
  }
  return undefined;
}

export interface LoginAreaProps {
  className?: string;
  /** `sidebar` fills the sidebar's width, and shrinks to an icon on its narrow form. */
  variant?: 'default' | 'sidebar';
}

export function LoginArea({ className, variant = 'default' }: LoginAreaProps) {
  const { currentUser } = useLoggedInAccounts();
  const [authDialogOpen, setAuthDialogOpen] = useState(false);
  const [quickLoginPubkey, setQuickLoginPubkey] = useState<string | null>(null);
  const sidebar = variant === 'sidebar';

  const handleJoin = async () => {
    const provider = getNip07Provider();
    if (provider) {
      try {
        const pubkey = await provider.getPublicKey();
        if (pubkey) {
          setQuickLoginPubkey(pubkey);
          return;
        }
      } catch {
        // Extension declined or errored — fall through to the full dialog.
      }
    }
    setAuthDialogOpen(true);
  };

  return (
    <div className={cn('inline-flex items-center justify-center', sidebar && 'flex w-full', className)}>
      {currentUser ? (
        <AccountSwitcher onAddAccountClick={() => setAuthDialogOpen(true)} variant={variant} />
      ) : (
        <Button
          onClick={handleJoin}
          variant="brand"
          size={sidebar ? 'default' : 'lg'}
          className={cn('rounded-full px-6 animate-scale-in', sidebar && 'w-full rounded-lg max-md:size-10 max-md:px-0')}
          aria-label="Log in"
        >
          {sidebar && <LogIn className="md:hidden" />}
          <span className={cn('truncate', sidebar && 'max-md:hidden')}>Log in</span>
        </Button>
      )}

      {quickLoginPubkey && (
        <QuickLoginDialog
          isOpen={quickLoginPubkey !== null}
          pubkey={quickLoginPubkey}
          onClose={() => setQuickLoginPubkey(null)}
          onOtherLogin={() => {
            setQuickLoginPubkey(null);
            setAuthDialogOpen(true);
          }}
        />
      )}

      <AuthDialog
        isOpen={authDialogOpen}
        onClose={() => setAuthDialogOpen(false)}
      />
    </div>
  );
}
