// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import sitemap from '@astrojs/sitemap';
import { SITE, BASE, REPO } from './site.config.mjs';
import { GROUPS, PAGES } from './docs.manifest.mjs';

/** The sidebar is derived from the manifest, so it cannot drift from the pages. */
const sidebar = [
  ...PAGES.filter((p) => p.group === null).map((p) => ({
    label: p.label,
    slug: p.slug === 'index' ? 'docs' : `docs/${p.slug}`,
  })),
  ...GROUPS.map((g) => ({
    label: g.label,
    items: PAGES.filter((p) => p.group === g.id).map((p) => ({
      label: p.label,
      slug: `docs/${p.slug}`,
    })),
  })),
];

/**
 * Code blocks are inverted relative to the page in *both* themes, so their
 * ground is a constant rather than a token. It has to be a literal here:
 * Expressive Code parses every styleOverride colour with a colour library at
 * build time, and a `var(--…)` string is not a colour it can parse — feeding
 * it one produced a stylesheet whose whole `@layer` block was discarded, so
 * every code block on the site rendered unstyled.
 */
const INK = '#0B0C07';

export default defineConfig({
  site: SITE,
  base: BASE,
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'Agento',
      customCss: [
        './src/styles/fonts.css',
        './src/styles/tokens.css',
        './src/styles/starlight.css',
      ],
      expressiveCode: {
        // One theme for both site themes: code blocks are always inverted
        // relative to the page, which is the design's own rule.
        themes: ['github-dark'],
        styleOverrides: {
          borderRadius: '2px',
          borderWidth: '1.5px',
          codeBackground: INK,
          codeFontSize: '0.82rem',
          // Expressive Code draws a title bar above shell blocks, and left at
          // its defaults that bar takes the *page* background — so every `bash`
          // block rendered as a bare light strip stacked on a dark body. The
          // frame is flattened into the code ground instead, which makes a
          // fenced block here look like the hand-written ones on the landing
          // page: one solid rectangle.
          frames: {
            frameBoxShadowCssValue: 'none',
            editorBackground: INK,
            editorTabBarBackground: INK,
            editorActiveTabBackground: INK,
            editorActiveTabBorderColor: 'transparent',
            editorActiveTabIndicatorTopColor: 'transparent',
            editorTabBarBorderBottomColor: 'transparent',
            terminalBackground: INK,
            terminalTitlebarBackground: INK,
            terminalTitlebarBorderBottomColor: 'transparent',
            terminalTitlebarDotsOpacity: '0',
          },
        },
      },
      description:
        'Cost analytics, session history and scheduled agents for Claude Code — a desktop app that reads what is already on your disk.',
      social: [{ icon: 'github', label: 'GitHub', href: REPO }],
      editLink: { baseUrl: `${REPO}/edit/main/` },
      sidebar,
      // The docs pages are generated from ../docs by scripts/sync-docs.mjs.
      // Editing them in src/content/ has no effect; the next build overwrites.
      // The 404 is a site page (src/pages/404.astro), not a docs page: landing
      // on the docs sidebar reads as "this documentation page is missing"
      // rather than "this address is wrong".
      disable404Route: true,
      lastUpdated: false,
      pagination: true,
      credits: false,
    }),
    sitemap(),
  ],
});
