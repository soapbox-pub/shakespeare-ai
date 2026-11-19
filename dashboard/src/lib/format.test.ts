import { describe, expect, it } from 'vitest';

import { perMillion, price, usd } from './format';

describe('format', () => {
  it('shows balances to the cent, and tiny costs finely enough to see', () => {
    expect(usd(12.345)).toBe('$12.35');
    expect(usd(0)).toBe('$0.00');
    expect(usd(0.000123)).toBe('$0.00012');
    expect(usd(0.0042)).toBe('$0.0042');
  });

  it('shows list prices to the tenth of a cent', () => {
    expect(price(0.011)).toBe('$0.011');
    expect(price(0.25)).toBe('$0.25');
    expect(perMillion('0.000003')).toBe('$3.00');
    expect(perMillion('0.0000025')).toBe('$2.50');
  });
});
