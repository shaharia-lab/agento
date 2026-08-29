/**
 * Which files in docs/ become pages, in what order, under what heading.
 *
 * Imported by both the sync script and astro.config.mjs, so the sidebar and
 * the generated pages cannot disagree about what exists. A doc added to docs/
 * and not listed here fails the build rather than silently never publishing.
 */
export const GROUPS = [
  { id: 'users', label: 'For users' },
  { id: 'contributors', label: 'For contributors' },
];

export const PAGES = [
  {
    file: 'README.md',
    slug: 'index',
    label: 'Overview',
    group: null,
    // The lead paragraph of docs/README.md sits under a table, so it makes a
    // poor <meta> description. Only pages whose opening line does not stand
    // alone need an override here.
    description: 'Guides for using Agento and for working on it — installation, every section of the app, troubleshooting, architecture and the release process.',
  },
  { file: 'installation.md', slug: 'installation', label: 'Installation', group: 'users' },
  { file: 'user-guide.md', slug: 'user-guide', label: 'User Guide', group: 'users' },
  { file: 'troubleshooting.md', slug: 'troubleshooting', label: 'Troubleshooting', group: 'users' },
  { file: 'architecture.md', slug: 'architecture', label: 'Architecture', group: 'contributors' },
  { file: 'development.md', slug: 'development', label: 'Development', group: 'contributors' },
  { file: 'releasing.md', slug: 'releasing', label: 'Releasing', group: 'contributors' },
];
