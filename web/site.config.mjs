/**
 * One definition of where this site lives.
 *
 * The site is published to GitHub Pages at shaharia-lab.github.io/agento, so
 * every internal URL carries a `/agento` prefix. Astro applies `base` to its
 * own routing, but NOT to hrefs inside markdown — so the docs sync script and
 * every hand-written link build their URLs through `url()` below rather than
 * spelling the prefix out. Change the base here and nothing else moves.
 */
export const SITE = 'https://shaharia-lab.github.io';
export const BASE = '/agento';

export const REPO = 'https://github.com/shaharia-lab/agento';
export const REPO_BLOB = `${REPO}/blob/main`;

/** Join a site-absolute path onto the base. `url('/docs/')` → `/agento/docs/`. */
export function url(path = '/') {
  const p = path.startsWith('/') ? path : `/${path}`;
  return `${BASE}${p}`.replace(/\/{2,}/g, '/');
}
