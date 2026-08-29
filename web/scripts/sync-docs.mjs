/**
 * docs/ is the single source. This copies it into the site.
 *
 * The markdown in `docs/` is written to be read on GitHub — relative `*.md`
 * links, no frontmatter, an inline table of contents at the top of each file.
 * None of that is wrong; it is just not what Starlight consumes. So rather
 * than reshaping the source (which would degrade it for everyone reading the
 * repository, and break README.md and CLAUDE.md's own pointers), this rewrites
 * a copy at build time:
 *
 *   - the H1 becomes the frontmatter `title`, and is removed from the body,
 *     because Starlight renders the title itself;
 *   - the lead paragraph becomes `description` (used for <meta> and search);
 *   - the inline TOC list is dropped, because Starlight renders one;
 *   - `installation.md#updates` becomes `/docs/installation/#updates`;
 *   - a link that leaves docs/ (`../CLAUDE.md`) becomes a GitHub blob URL,
 *     since those files have no page on this site;
 *   - screenshots are copied to public/ and their links repointed.
 *
 * The output is generated and gitignored. Never edit it.
 */
import { mkdir, readdir, readFile, writeFile, rm, cp } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { BASE, REPO_BLOB } from '../site.config.mjs';
import { PAGES } from '../docs.manifest.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(here, '../../docs');
const OUT = resolve(here, '../src/content/docs/docs');
const SHOTS_OUT = resolve(here, '../public/screenshots');

const bySourceFile = new Map(PAGES.map((p) => [p.file, p]));

/** `installation.md` -> `/docs/installation/`; unknown files -> null. */
function hrefFor(target) {
  const [path, hash] = target.split('#');
  const page = bySourceFile.get(path);
  if (!page) return null;
  const route = page.slug === 'index' ? `${BASE}/docs/` : `${BASE}/docs/${page.slug}/`;
  return hash ? `${route}#${hash}` : route;
}

function rewriteLinks(body) {
  return body.replace(/\]\(([^)\s]+)(\s+"[^"]*")?\)/g, (whole, target, title = '') => {
    if (/^(https?:|mailto:|#)/.test(target)) return whole;

    if (target.startsWith('screenshots/')) return `](${BASE}/${target}${title})`;

    const inDocs = hrefFor(target);
    if (inDocs) return `](${inDocs}${title})`;

    // Anything else points outside docs/ — CLAUDE.md, README.md, parity/,
    // source files. Real destinations, just not pages on this site.
    const repoPath = target.replace(/^\.\//, '').replace(/^\.\.\//, '');
    return `](${REPO_BLOB}/${repoPath}${title})`;
  });
}

/** Strip the leading bullet TOC; Starlight renders its own. */
function dropInlineToc(body) {
  const lines = body.split('\n');
  const start = lines.findIndex((l) => /^[-*] \[.+\]\(#.+\)$/.test(l.trim()));
  if (start === -1) return body;
  let end = start;
  while (
    end < lines.length &&
    (/^[-*] \[.+\]\(#.+\)$/.test(lines[end].trim()) || lines[end].trim() === '')
  ) {
    end++;
  }
  const entries = lines.slice(start, end).filter((l) => l.trim()).length;
  if (entries < 3) return body;
  return [...lines.slice(0, start), ...lines.slice(end)].join('\n');
}

function plain(md) {
  return md
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/[`*_]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}

/** First whole sentence if it fits, otherwise a clean word boundary. */
function summarise(text, max = 165) {
  if (text.length <= max) return text;
  const sentence = text.slice(0, max).match(/^.*?[.!?](?=\s|$)/);
  if (sentence && sentence[0].length > 60) return sentence[0];
  return text.slice(0, max).replace(/\s+\S*$/, '') + '\u2026';
}

function yaml(value) {
  return `"${value.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
}

function transform(raw, page) {
  const lines = raw.split('\n');
  const h1 = lines.findIndex((l) => l.startsWith('# '));
  const title = h1 === -1 ? page.label : lines[h1].slice(2).trim();
  let body = h1 === -1 ? raw : lines.slice(h1 + 1).join('\n');

  body = dropInlineToc(body);

  const lead = body
    .split('\n\n')
    .map((b) => b.trim())
    .find(
      (b) =>
        b &&
        !b.startsWith('#') &&
        !b.startsWith('---') &&
        !b.startsWith('|') &&
        !b.startsWith('>')
    );
  const description = page.description ?? (lead ? summarise(plain(lead)) : '');

  const front = [
    '---',
    `title: ${yaml(title)}`,
    description ? `description: ${yaml(description)}` : null,
    `editUrl: ${yaml(`${REPO_BLOB}/docs/${page.file}`)}`,
    '---',
    '',
  ]
    .filter(Boolean)
    .join('\n');

  return front + rewriteLinks(body).replace(/^\n+/, '\n');
}

async function main() {
  await rm(OUT, { recursive: true, force: true });
  await mkdir(OUT, { recursive: true });

  const present = new Set(await readdir(SRC));
  let written = 0;

  for (const page of PAGES) {
    if (!present.has(page.file)) {
      throw new Error(
        `docs/${page.file} is listed in sync-docs.mjs but does not exist. ` +
          `Update PAGES if the file was renamed or removed.`
      );
    }
    const raw = await readFile(join(SRC, page.file), 'utf8');
    await writeFile(join(OUT, `${page.slug}.md`), transform(raw, page), 'utf8');
    written++;
  }

  // A doc added to docs/ and not listed here would silently never publish.
  const unlisted = [...present].filter((f) => f.endsWith('.md') && !bySourceFile.has(f));
  if (unlisted.length) {
    throw new Error(
      `docs/ contains ${unlisted.join(', ')}, which the site does not publish. ` +
        `Add ${unlisted.length > 1 ? 'them' : 'it'} to PAGES in web/scripts/sync-docs.mjs.`
    );
  }

  await rm(SHOTS_OUT, { recursive: true, force: true });
  await cp(join(SRC, 'screenshots'), SHOTS_OUT, { recursive: true });
  const shots = await readdir(join(SHOTS_OUT, 'light'));

  console.log(`sync-docs: ${written} pages, ${shots.length} screenshots`);
}

await main();
