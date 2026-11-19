/**
 * A link that opens the Credits page ready to redeem a card. The code is in
 * the fragment, so it never reaches a server's logs.
 */
export function giftcardLink(code: string): string {
  return `${location.origin}/credits#redeem=${encodeURIComponent(code)}`;
}

/** The code a link from {@link giftcardLink} carries, if this is one. */
export function codeFromHash(hash: string): string | undefined {
  return new URLSearchParams(hash.replace(/^#/, '')).get('redeem')?.trim() || undefined;
}
