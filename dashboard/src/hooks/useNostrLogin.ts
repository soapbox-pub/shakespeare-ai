import { useContext } from 'react';

import { NostrLoginContext, type NostrLoginContextType } from '@/contexts/NostrLoginContext';

/** The logins, from the app's NostrLoginProvider. */
export function useNostrLogin(): NostrLoginContextType {
  const context = useContext(NostrLoginContext);
  if (!context) throw new Error('useNostrLogin must be used within a NostrLoginProvider');
  return context;
}
