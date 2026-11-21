// The build's prerender: the app's public pages drawn to HTML, so crawlers
// and link previews that don't run JavaScript see them. scripts/prerender.mjs
// builds this for Node and calls `render` for each path.
//
// They're drawn as for someone logged out, with nothing fetched, and the
// browser's app replaces them as it starts (createRoot, not hydration, so a
// logged-in visitor's page needn't match this one).

import './lib/polyfills.ts';

import { createHead, transformHtmlTemplate } from '@unhead/react/server';
import { prerenderToNodeStream } from 'react-dom/static';
import { StaticRouter } from 'react-router-dom';
import { InferSeoMetaPlugin } from 'unhead/plugins';

import { AppProviders } from './App';
import { AppRoutes } from './AppRouter';

/** The paths to prerender, and the file each is written to under `dist/`. */
export const PAGES: Record<string, string> = {
  '/': 'index.html',
  '/docs': 'docs/index.html',
  '/terms': 'terms/index.html',
};

/** `template` (the built index.html) with `path`'s page and head in it. */
export async function render(path: string, template: string): Promise<string> {
  const head = createHead({ plugins: [InferSeoMetaPlugin({ twitterCard: 'summary' })] });
  const { prelude } = await prerenderToNodeStream(
    <AppProviders head={head}>
      <StaticRouter location={path}>
        <AppRoutes />
      </StaticRouter>
    </AppProviders>,
    {
      // Without this React moves large Suspense boundaries' HTML to the end
      // and reveals it with an inline script, which a crawler without
      // JavaScript (and the page's CSP) wouldn't run.
      progressiveChunkSize: Number.POSITIVE_INFINITY,
      // React would draw an error's Suspense fallback and leave the rest to
      // the browser; for a prerender that's a broken build.
      onError(error) {
        throw error;
      },
    },
  );

  let body = '';
  for await (const chunk of prelude) body += chunk;

  const root = '<div id="root"></div>';
  if (!template.includes(root)) throw new Error(`index.html has no ${root}`);
  return transformHtmlTemplate(head, template.replace(root, `<div id="root">${body}</div>`));
}
