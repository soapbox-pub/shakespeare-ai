// Write the public pages' HTML into dist/, after `vite build` and the SSR
// build of src/prerender.tsx into .prerender/. See src/prerender.tsx.
//
// The unprerendered index.html is kept as dist/200.html: the server answers
// any other path of the app's with it, so a logged-in page doesn't flash the
// home page's HTML first.

import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const dist = join(root, 'dist');
const bundle = join(root, '.prerender');

// Pages show the API's own address. The real one isn't known until they're
// served, so the HTML has the public service's, and the app puts in the
// right one as it starts.
const origin = process.env.PRERENDER_ORIGIN ?? 'https://ai.shakespeare.diy';

/** What the browser has that the pages read while drawing. */
function shim(path) {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => store.set(key, String(value)),
    removeItem: (key) => store.delete(key),
    clear: () => store.clear(),
    key: (i) => [...store.keys()][i] ?? null,
    get length() {
      return store.size;
    },
  };
  Object.defineProperty(globalThis, 'location', { value: new URL(path, origin), configurable: true, writable: true });
}

shim('/');
const { PAGES, render } = await import(pathToFileURL(join(bundle, 'prerender.js')).href);

const template = await readFile(join(dist, 'index.html'), 'utf8');
await writeFile(join(dist, '200.html'), template);

for (const [path, file] of Object.entries(PAGES)) {
  shim(path);
  const html = await render(path, template);
  await mkdir(dirname(join(dist, file)), { recursive: true });
  await writeFile(join(dist, file), html);
  console.log(`prerendered ${path} → dist/${file}`);
}

await rm(bundle, { recursive: true, force: true });
