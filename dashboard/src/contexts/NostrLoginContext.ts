import type { NLoginType } from '@nostrify/react/login';
import { createContext } from 'react';

/** The same as Nostrify's own login context, which it doesn't export. */
export interface NostrLoginContextType {
  /** The logins, the current one first. */
  logins: readonly NLoginType[];
  addLogin: (login: NLoginType) => void;
  removeLogin: (loginId: string) => void;
  /** Make a login the current one, by moving it first. */
  setLogin: (loginId: string) => void;
  clearLogins: () => void;
}

export const NostrLoginContext = createContext<NostrLoginContextType | undefined>(undefined);

export type NostrLoginAction =
  | { type: 'login.add'; login: NLoginType; set?: boolean }
  | { type: 'login.remove'; id: string }
  | { type: 'login.set'; id: string }
  | { type: 'login.clear' };

/** As Nostrify's. */
export function nostrLoginReducer(state: readonly NLoginType[], action: NostrLoginAction): readonly NLoginType[] {
  switch (action.type) {
    case 'login.add': {
      const filtered = state.filter((login) => login.id !== action.login.id);
      return action.set ? [action.login, ...filtered] : [...filtered, action.login];
    }
    case 'login.remove':
      return state.filter((login) => login.id !== action.id);
    case 'login.set': {
      const login = state.find((l) => l.id === action.id);
      return login ? [login, ...state.filter((l) => l.id !== action.id)] : state;
    }
    case 'login.clear':
      return [];
  }
}
