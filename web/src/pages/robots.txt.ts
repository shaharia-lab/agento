/**
 * /robots.txt — and, more to the point, the one line that tells a crawler the
 * sitemap exists.
 *
 * @astrojs/sitemap has always emitted /sitemap-index.xml here, and nothing
 * pointed at it: the file was reachable only by a crawler guessing the
 * conventional name, or by somebody submitting it in Search Console by hand.
 *
 * It is generated rather than dropped in public/ so the absolute `Sitemap:`
 * URL — which must be absolute, per the sitemaps protocol — comes from SITE in
 * site.config.mjs. A static file would be the only place in this site that
 * spells the domain out, and it would go stale in silence, since a wrong
 * Sitemap line is not an error anywhere: crawlers just ignore it.
 */
import type { APIRoute } from 'astro';
import { SITE } from '../../site.config.mjs';

/**
 * Everything here is public documentation of an open-source project, so there
 * is nothing to hide from an indexer and the allow is unconditional. The
 * generated Pagefind search index is excluded because it is a build artifact
 * of the docs, not a page: it is megabytes of fragments that render as nothing
 * and would dilute what an indexer sees of the real pages.
 */
const body = `User-agent: *
Allow: /
Disallow: /pagefind/

Sitemap: ${SITE}/sitemap-index.xml
`;

export const GET: APIRoute = () =>
  new Response(body, {
    headers: { 'Content-Type': 'text/plain; charset=utf-8' },
  });
