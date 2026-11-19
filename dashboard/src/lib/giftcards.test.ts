import { describe, expect, it } from 'vitest';

import { codeFromHash, giftcardLink } from './giftcards';

describe('gift card links', () => {
  it('carry the code in the fragment, and give it back', () => {
    const link = new URL(giftcardLink('ABCD-EFGH-IJKL'));
    expect(link.pathname).toBe('/credits');
    expect(link.search).toBe('');
    expect(codeFromHash(link.hash)).toBe('ABCD-EFGH-IJKL');
  });

  it('ignore fragments without a code', () => {
    expect(codeFromHash('')).toBeUndefined();
    expect(codeFromHash('#redeem=')).toBeUndefined();
    expect(codeFromHash('#other=1')).toBeUndefined();
  });
});
