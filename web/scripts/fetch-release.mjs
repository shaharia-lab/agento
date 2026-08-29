/**
 * The latest release, resolved at build time.
 *
 * Nothing about a download may be hand-written. Asset filenames carry the
 * version (`Agento_1.2.0_amd64.deb`), so GitHub's `releases/latest/download/…`
 * redirect cannot address them and a link spelled out in a template rots the
 * moment a release ships. This asks the API once per build and writes
 * src/data/release.json, which the pages import.
 *
 * Two properties are deliberate:
 *
 *   - **It never fails the build.** No network, no token, a rate limit, a
 *     repository with no releases yet — each falls back to the version in
 *     src-tauri/tauri.conf.json and the `releases/latest` redirect, which is
 *     always correct even when it cannot name a file. A docs site that cannot
 *     build because GitHub is slow is worse than one showing a generic link.
 *   - **It runs at build time, not in the visitor's browser.** A client-side
 *     fetch would put a third-party request on every page view of a product
 *     whose entire claim is that nothing leaves your machine, and would show
 *     an empty download section to anyone the API rate-limits. The release
 *     workflow rebuilds this site when a release is published, so the baked
 *     answer is refreshed by the event that changes it.
 */
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const OUT = resolve(here, '../src/data/release.json');
const REPO_OUT = resolve(here, '../src/data/repo.json');
const CONF = resolve(here, '../../src-tauri/tauri.conf.json');
const REPO_API = 'https://api.github.com/repos/shaharia-lab/agento';
const API = `${REPO_API}/releases/latest`;
const RELEASES = 'https://github.com/shaharia-lab/agento/releases';

/**
 * Which download a file is. Ordered: the first pattern that matches wins, so
 * `_x64-setup.exe` is claimed by Windows before any looser `x64` rule.
 */
const PLATFORMS = [
  { id: 'macos-arm', os: 'macos', label: 'macOS', arch: 'Apple Silicon', kind: 'dmg', updates: 'in-app', test: /_aarch64\.dmg$/ },
  { id: 'macos-x64', os: 'macos', label: 'macOS', arch: 'Intel', kind: 'dmg', updates: 'in-app', test: /_x64\.dmg$/ },
  { id: 'win-x64', os: 'windows', label: 'Windows', arch: 'x64', kind: 'exe', updates: 'in-app', test: /_x64-setup\.exe$/ },
  { id: 'linux-appimage-x64', os: 'linux', label: 'Linux', arch: 'x86_64', kind: 'AppImage', updates: 'in-app', test: /_amd64\.AppImage$/ },
  { id: 'linux-appimage-arm', os: 'linux', label: 'Linux', arch: 'ARM64', kind: 'AppImage', updates: 'in-app', test: /_aarch64\.AppImage$/ },
  { id: 'linux-deb-x64', os: 'linux', label: 'Debian / Ubuntu', arch: 'x86_64', kind: 'deb', updates: 'notify', test: /_amd64\.deb$/ },
  { id: 'linux-deb-arm', os: 'linux', label: 'Debian / Ubuntu', arch: 'ARM64', kind: 'deb', updates: 'notify', test: /_arm64\.deb$/ },
  { id: 'linux-rpm-x64', os: 'linux', label: 'Fedora / RHEL', arch: 'x86_64', kind: 'rpm', updates: 'notify', test: /\.x86_64\.rpm$/ },
  { id: 'linux-rpm-arm', os: 'linux', label: 'Fedora / RHEL', arch: 'ARM64', kind: 'rpm', updates: 'notify', test: /\.(aarch64|arm64)\.rpm$/ },
];

async function fallback(reason) {
  const version = JSON.parse(await readFile(CONF, 'utf8')).version;
  console.warn(`fetch-release: ${reason} — falling back to v${version} with no per-platform files`);
  return { version, tag: `v${version}`, url: `${RELEASES}/latest`, publishedAt: null, resolved: false, downloads: [] };
}

function headers() {
  const h = { accept: 'application/vnd.github+json', 'user-agent': 'agento-site-build' };
  if (process.env.GITHUB_TOKEN) h.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
  return h;
}

/** Star count for the call to action. A missing count renders no number. */
async function repoStats() {
  try {
    const res = await fetch(REPO_API, { headers: headers(), signal: AbortSignal.timeout(10_000) });
    if (!res.ok) throw new Error(`GitHub answered ${res.status}`);
    const r = await res.json();
    console.log(`fetch-release: repo has ${r.stargazers_count} stars`);
    return { stars: r.stargazers_count, forks: r.forks_count, resolved: true };
  } catch (err) {
    console.warn(`fetch-release: could not read repo stats (${err.message})`);
    return { stars: null, forks: null, resolved: false };
  }
}

async function main() {
  await mkdir(dirname(OUT), { recursive: true });
  await writeFile(REPO_OUT, JSON.stringify(await repoStats(), null, 2) + '\n', 'utf8');

  let data;
  try {
    const res = await fetch(API, { headers: headers(), signal: AbortSignal.timeout(10_000) });
    if (!res.ok) throw new Error(`GitHub answered ${res.status}`);
    const release = await res.json();

    const downloads = PLATFORMS.map((p) => {
      const asset = (release.assets ?? []).find((a) => p.test.test(a.name));
      return asset
        ? { ...p, test: undefined, file: asset.name, href: asset.browser_download_url, size: asset.size }
        : null;
    }).filter(Boolean);

    if (!downloads.length) throw new Error(`release ${release.tag_name} carries no recognised installer`);

    data = {
      version: String(release.tag_name).replace(/^v/, ''),
      tag: release.tag_name,
      url: release.html_url,
      publishedAt: release.published_at,
      resolved: true,
      downloads,
    };
    console.log(`fetch-release: ${data.tag}, ${downloads.length} installers`);
  } catch (err) {
    data = await fallback(err.message);
  }

  await writeFile(OUT, JSON.stringify(data, null, 2) + '\n', 'utf8');
}

await main();
