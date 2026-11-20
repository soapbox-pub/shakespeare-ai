import { useEffect } from 'react';

/**
 * Scroll to the element the URL's fragment names once the page has drawn it.
 * The browser tries before a lazily loaded page exists, and misses.
 */
export function useHashScroll() {
  useEffect(() => {
    const id = decodeURIComponent(location.hash.slice(1));
    if (id) document.getElementById(id)?.scrollIntoView();
  }, []);
}
