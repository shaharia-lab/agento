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

/** The name a share card and the structured data both call this site. */
export const SITE_NAME = 'Agento';

/** Who publishes it. Used by the structured data on the landing page. */
export const ORG = { name: 'Shaharia Lab', url: 'https://shaharialab.com' };

/**
 * The share card. Drawn by design/og-image.html and rendered to public/og.png
 * by scripts/render-og.mjs; the dimensions are repeated in the meta tags,
 * because every consumer that pre-allocates space reads them rather than the
 * file. Keep the three in step.
 */
export const OG_IMAGE = { path: '/og.png', width: 1200, height: 630 };

/**
 * Google Tag Manager, or nothing at all.
 *
 * The container id is a build-time variable rather than a literal, and there is
 * deliberately NO baked-in default: an unset variable produces a site with no
 * third-party script on it whatsoever. That matters more here than on most
 * sites, because `npm run dev`, a contributor's fork and every PR preview build
 * would otherwise report into the production container as if they were real
 * traffic — and because this site's own landing page promises the product sends
 * nothing anywhere, so a tag that turns up by accident is a claim broken by
 * accident.
 *
 * Set it in the workflow from a repository variable:
 *
 *     env:
 *       PUBLIC_GTM_ID: ${{ vars.PUBLIC_GTM_ID }}
 *
 * The `PUBLIC_` prefix is Astro's: it marks a variable as safe to reach client
 * code, which a container id is — it is visible in the page source of every
 * site that uses one.
 */
export function gtmId() {
  const id = (process.env.PUBLIC_GTM_ID ?? '').trim();
  return /^GTM-[A-Z0-9]+$/.test(id) ? id : null;
}

/**
 * GTM's own loader, verbatim from the container's install instructions bar the
 * id. It is emitted `is:inline` on both halves of the site — Astro would
 * otherwise bundle it, and a bundled tag loader is fetched after the module
 * graph rather than during head parsing, which is the one thing this snippet
 * is shaped to avoid.
 *
 * The <noscript> iframe half of GTM's install is deliberately omitted. It
 * exists to fire tags for visitors without JavaScript, and every tag worth
 * having here needs JavaScript to do anything; including it would also mean
 * injecting markup into <body>, which Starlight's head-only extension point
 * cannot do — so the two halves of the site would end up with different
 * installs, which is worse than a missing fallback that fires nothing.
 */
export function gtmSnippet(id) {
  return `(function(w,d,s,l,i){w[l]=w[l]||[];w[l].push({'gtm.start':new Date().getTime(),event:'gtm.js'});var f=d.getElementsByTagName(s)[0],j=d.createElement(s),dl=l!='dataLayer'?'&l='+l:'';j.async=true;j.src='https://www.googletagmanager.com/gtm.js?id='+i+dl;f.parentNode.insertBefore(j,f);})(window,document,'script','dataLayer','${id}');`;
}

/** Join a site-absolute path onto the base. `url('/docs/')` → `/docs/`. */
export function url(path = '/') {
  const p = path.startsWith('/') ? path : `/${path}`;
  return `${BASE}${p}`.replace(/\/{2,}/g, '/');
}

/**
 * The same, absolute. `og:image`, `twitter:image` and every `@id` in the
 * structured data must be fully qualified — a crawler and a share-card scraper
 * both resolve them out of context, where a site-relative path means nothing.
 */
export function abs(path = '/') {
  return `${SITE}${url(path)}`;
}
