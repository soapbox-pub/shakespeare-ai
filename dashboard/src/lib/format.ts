/** Dollars as people read them: to the cent, or finer when that would round a real cost to nothing. */
export function usd(amount: number): string {
  const abs = Math.abs(amount);
  const digits = abs !== 0 && abs < 0.01 ? Math.min(6, Math.max(2, -Math.floor(Math.log10(abs)) + 1)) : 2;
  return amount.toLocaleString('en-US', { style: 'currency', currency: 'USD', minimumFractionDigits: digits, maximumFractionDigits: digits });
}

/** A per-token price as dollars per million tokens. */
export function perMillion(perToken: string): string {
  const value = Number(perToken) * 1_000_000;
  return value.toLocaleString('en-US', { style: 'currency', currency: 'USD', minimumFractionDigits: 2, maximumFractionDigits: 3 });
}

/** A list price, which can be a fraction of a cent. */
export function price(amount: number): string {
  return amount.toLocaleString('en-US', { style: 'currency', currency: 'USD', minimumFractionDigits: 2, maximumFractionDigits: 3 });
}

export function tokens(n: number): string {
  return n.toLocaleString('en-US');
}

/** A Unix time, as a date and time in the reader's own zone. */
export function dateTime(unix: number): string {
  return new Date(unix * 1000).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}
