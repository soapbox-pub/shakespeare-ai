import type { NLoginType } from '@nostrify/react/login';
import { type ReactNode, useEffect, useReducer } from 'react';

import { NostrLoginContext, nostrLoginReducer } from '@/contexts/NostrLoginContext';

function load(storageKey: string): NLoginType[] {
  try {
    const stored = globalThis.localStorage?.getItem(storageKey);
    return stored ? JSON.parse(stored) : [];
  } catch {
    return [];
  }
}

/**
 * Nostrify's NostrLoginProvider, but reading the logins as it first draws.
 * Nostrify's reads them in an effect and draws nothing until then, which
 * would leave the build's prerendered pages empty, and blank them for a
 * moment as the app replaces them.
 */
export function NostrLoginProvider({ storageKey, children }: { storageKey: string; children: ReactNode }) {
  const [logins, dispatch] = useReducer(nostrLoginReducer, storageKey, load);

  useEffect(() => {
    localStorage.setItem(storageKey, JSON.stringify(logins));
  }, [logins, storageKey]);

  const value = {
    logins,
    addLogin: (login: NLoginType) => dispatch({ type: 'login.add', login }),
    removeLogin: (id: string) => dispatch({ type: 'login.remove', id }),
    setLogin: (id: string) => dispatch({ type: 'login.set', id }),
    clearLogins: () => dispatch({ type: 'login.clear' }),
  };

  return <NostrLoginContext.Provider value={value}>{children}</NostrLoginContext.Provider>;
}
