/**
 * One definition of where this site lives.
 *
 * The site is published to GitHub Pages at its own domain, myagento.app, so
 * pages sit at the root and internal URLs carry no path prefix. Astro applies
 * `base` to its own routing, but NOT to hrefs inside markdown — so the docs
 * sync script and every hand-written link build their URLs through `url()`
 * below rather than spelling any prefix out. Change the base here and nothing
 * else moves.
 *
 * BASE is the EMPTY STRING, not '/'. It is concatenated directly in a few
 * places that do not go through `url()` — `${BASE}/docs/` in
 * scripts/sync-docs.mjs, and `href.slice(BASE.length)` /
 * `href.startsWith(BASE + '/')` in scripts/check-links.mjs. With '/' those
 * become `//docs/`, an off-by-one slice, and a `'//'` prefix test that matches
 * nothing, so the docs links break and the link checker silently stops
 * checking. Astro wants '/' for the site root, and astro.config.mjs converts
 * it there.
 *
 * The domain and the GitHub Pages custom-domain setting are managed in
 * shaharia-lab/infrastructure (terraform/cloudflare + terraform/github-shaharia-lab),
 * not in this repository; there is deliberately no CNAME file here.
 */
export const SITE = 'https://myagento.app';
export const BASE = '';

export const REPO = 'https://github.com/shaharia-lab/agento';
export const REPO_BLOB = `${REPO}/blob/main`;

/** Join a site-absolute path onto the base. `url('/docs/')` → `/docs/`. */
export function url(path = '/') {
  const p = path.startsWith('/') ? path : `/${path}`;
  return `${BASE}${p}`.replace(/\/{2,}/g, '/');
}
