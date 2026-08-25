# Releasing

How a release is cut, and the two things that break it.

Releases are tagged `v*` from `main`.

**Two tag names matter here and they are not the same thing.** Release tags are
`v1.0.0`, `v1.1.0` and so on — the guard job, the manifest script and the
`promote` job all key off that `v` prefix. The *update manifest* is published to
a separate, fixed tag called **`desktop-latest`**, and that name is frozen: its
download URL is compiled into every installer that has ever shipped
(`plugins.updater.endpoints` in `src-tauri/tauri.conf.json`). Rename it and every
existing install polls a URL that 404s — they are never offered another update
and never report a problem, because "no update available" and "cannot reach the
manifest" look identical from the outside. Change the release tags freely; leave
`desktop-latest` alone.

Two legacy tag namespaces are still in the repository and neither is ever built
again: `desktop-v0.1.x` is what this app's releases were called before `v1.0.0`,
and `v0.1.0` … `v0.11.2` are the retired Agento server's. The second is why
`promote` checks that a release actually carries a `latest.json` before doing
anything — re-publishing one of those old server releases would otherwise reach
it with nothing to promote.

- [The flow](#the-flow)
- [Cutting a release](#cutting-a-release)
- [Release notes](#release-notes)
- [The two guards](#the-two-guards)
- [Release candidates and dry runs](#release-candidates-and-dry-runs)
- [What gets built](#what-gets-built)
- [The update manifest](#the-update-manifest)
- [Signing keys](#signing-keys)
- [If something goes wrong](#if-something-goes-wrong)

---

## The flow

```
  bump tauri.conf.json  ──>  tag vX.Y.Z  ──────────>  guard + build matrix
                                                            │
                                                            v
                                              DRAFT release, installers
                                              attached, latest.json staged
                                                            │
                                            (a human checks the installers)
                                                            │
                                                  publish the draft
                                                            │
                                                            v
                                     promote: copy latest.json to the fixed
                                     `desktop-latest` tag every app polls
```

**Publishing the draft is the release act.** Draft assets are not publicly
downloadable, so nothing has shipped until then. Installed apps cannot even fetch
the manifest.

The manifest is deliberately not published at build time. A live manifest
pointing at draft asset URLs would make every installed app report an update it
cannot download.

---

## Cutting a release

1. **Bump the version in all five places**, and commit them together on `main`:
   `src-tauri/tauri.conf.json`, `package.json`, `package-lock.json` (which
   carries it twice), `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock`.

   ```bash
   npm version 1.1.0 --no-git-tag-version   # package.json + package-lock.json
   # then, by hand:
   # src-tauri/tauri.conf.json → { "version": "1.1.0" }
   # src-tauri/Cargo.toml      → version = "1.1.0"
   cd src-tauri && cargo update -p agento --offline   # Cargo.lock
   ```

   `npm version` is what keeps the lockfile in step; bumping `package.json`
   alone leaves it behind. `cargo update -p agento --offline` does the same job
   for `Cargo.lock` without touching any dependency.

   **All five are enforced now**, by the guard job in step 2 and by CI on every
   PR. `tauri.conf.json` is the one that decides what ships — the bundler bakes
   it into the installer and the updater compares against it — and the rest are
   checked because nothing else ever looks at them. That is not hypothetical:
   `package-lock.json` sat on `0.1.1` for the whole of `0.1.2` (fixed in
   `1f5cc9a`), and `src-tauri/Cargo.toml` sat on `0.1.2` right up to `1.0.0`.
   The lockfile drift is the one with teeth — npm silently repairs that field on
   almost any command, so the first `npm run build` after a checkout leaves the
   tree dirty with a change nobody made, which reads as local work, invites
   someone to discard it, and comes straight back.

2. **Tag that commit** and push the tag.

   ```bash
   git tag v1.1.0
   git push origin v1.1.0
   ```

3. **Wait for the build.** Five runners, one per target: Linux x86_64, Linux
   aarch64, macOS Apple Silicon, macOS Intel, Windows x86_64. Tauri cannot
   cross-compile its bundles, which is why each needs its own runner.

4. **Check the draft release.** Confirm every platform's installer is attached
   and `latest.json` is there. Download one and launch it if the change was
   risky.

5. **Write the release notes** into the draft's body, before publishing. See
   [Release notes](#release-notes) below.

6. **Publish the draft.** That fires `promote`, which copies the staged manifest
   to `desktop-latest`. Installed apps start seeing the update within their next
   check.

---

## Release notes

The workflow does not generate them — the draft release's body is empty until
somebody writes it, and once the draft is published that text is what every user
sees in the in-app update prompt. Two rules:

- **Say what changes for someone who does nothing.** Most releases change
  behaviour for everyone; a release whose headline feature is off by default does
  not, and the notes should say so plainly rather than leaving a reader to guess
  whether they have just been given a new listening port.
- **Anything needing a Gatekeeper or SmartScreen bypass gets a line**, with the
  link to [Installation](installation.md) — builds are neither Apple notarised
  nor Windows code signed.

### Drafted: the first release carrying the LLM Gateway

Ready to paste into that release's draft body, and the reason this section
exists:

> **LLM Gateway (new, and off by default).**
>
> Agento can now serve as a local LLM endpoint for your other tools. Enable it
> and it listens on `127.0.0.1` speaking both the OpenAI and the Anthropic wire
> formats, forwarding to providers you configure with your own API keys — so the
> OpenAI SDK, the Anthropic SDK, Claude Code, LiteLLM or Aider can all be pointed
> at one place. You get ordered fallback between providers when one fails, and a
> Usage dashboard showing what each tool, alias and token spent.
>
> **If you do not turn it on, nothing changes.** No port is bound, no listener
> starts, and the feature costs one database read at launch. It is off on a fresh
> install and off after upgrading.
>
> To turn it on: **LLM Gateway → Gateway Settings**, then add a provider, define
> a model alias, and mint a gateway token from **Overview**. The two base URLs
> are not the same shape — `…:8880/v1` for OpenAI-style clients, `…:8880/anthropic`
> with **no** `/v1` for the Anthropic SDK and Claude Code, which append it
> themselves.
>
> This adds a third token scope, **`llm`**, which reaches the gateway and nothing
> else; existing `read` and `write` tokens are unaffected and are refused by the
> gateway by design. It also adds four database tables and a usage-retention
> setting (90 days by default; `0` keeps everything).
>
> Full walkthrough: [LLM Gateway in the user guide](https://github.com/shaharia-lab/agento/blob/main/docs/user-guide.md#llm-gateway).

---

## The two guards

**The tag must equal `v` plus the version in `tauri.conf.json`** (and in the four
other files listed in step 1). A guard job fails the build otherwise, before
anything is built.

This is not tidiness. The installed app compares the version baked into it at
build time against the manifest's version, which comes from the tag. If the tag
says `1.1.0` and the conf still says `1.0.0`, every "updated" install keeps
reporting `1.0.0` and is offered the same update forever.

**Every platform must produce a signature.** The manifest script aborts the
release if a payload has no `.sig`, or if a platform produced nothing at all. A
manifest that silently omitted a platform would strand exactly the users already
running it, and their app would keep reporting "up to date" indefinitely.

---

## Release candidates and dry runs

**A prerelease tag** builds and drafts identically, is auto-marked prerelease, and
is **never promoted**:

```bash
git tag v1.1.0-rc.1
git push origin v1.1.0-rc.1
```

Share the release page with testers. No installed app is offered it. The promote
job double-checks the version string too, so an rc tag published with the
prerelease box unticked is refused rather than pushed to updaters.

**A private smoke test** needs no tag at all. Run the workflow manually with
`dry_run`. Installers land as CI artifacts and no release is created.

---

## What gets built

| Runner | Target | Artifacts |
| --- | --- | --- |
| `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `.deb`, `.rpm`, `.AppImage` |
| `ubuntu-22.04-arm` | `aarch64-unknown-linux-gnu` | `.deb`, `.rpm`, `.AppImage` |
| `macos-15` | `aarch64-apple-darwin` | `.dmg`, `.app.tar.gz` |
| `macos-15-intel` | `x86_64-apple-darwin` | `.dmg`, `.app.tar.gz` |
| `windows-latest` | `x86_64-pc-windows-msvc` | `-setup.exe` |

Notes worth knowing before touching the matrix:

- **Linux builds on the oldest supported LTS.** The `.deb` links against whatever
  the build host has, so building on 22.04 is what keeps the package installable
  on newer releases.
- **macOS ships two separate builds, not a universal binary.** A universal
  binary would be simpler and would double every download.
- **A job naming a retired runner image queues forever** rather than erroring.
  Check `actions/runner-images` before bumping one.
- **NSIS, not MSI, on Windows.** The updater manifest expects the `-setup.exe`; a
  built-and-dropped `.msi` only wasted CI time.
- **The macOS updater tarball is renamed at collection.** Tauri names it
  `Agento.app.tar.gz` with no architecture, identical on both macOS legs, so the
  merged artifact download would silently overwrite one architecture's payload
  with the other's.

---

## The update manifest

`.github/scripts/build-update-manifest.py` walks the collected artifacts, pairs
each updater payload with its detached signature, and writes `latest.json`.

Platform keys are `tauri-plugin-updater`'s `{os}-{arch}`, where the architecture
uses Rust's spelling: `aarch64`, never `arm64`. A key the plugin does not look up
strands that platform's users silently.

macOS updates ship as `.app.tar.gz`. The `.dmg` is only for the first install.

The manifest is served from a fixed tag, `desktop-latest`, so the updater has a
stable URL that does not move with each release. That tag is itself marked
prerelease so it never displaces the real latest release on the repository page.

Its name outlived the `desktop-v*` release tags on purpose. The URL is baked
into every shipped installer, so the tag has to keep answering for as long as
any of those installs exist — see the note at the top of this file.

---

## Signing keys

Updates are signed with **our own minisign key**, generated by
`tauri signer generate`. It has nothing to do with Apple or Microsoft code
signing. It only proves an update came from us, which is why in-app updates work
on ad-hoc signed, non-notarised macOS builds: Gatekeeper gates the first launch
of a downloaded app, not a bundle the updater swapped in.

| Half | Where |
| --- | --- |
| Private | 1Password, then GCP Secret Manager (`github-repo-agento-tauri-updater-private-key`), then the `TAURI_SIGNING_PRIVATE_KEY` Actions secret, all via `terraform/` |
| Public | `plugins.updater.pubkey` in `tauri.conf.json` |

**Losing the private key means no existing install can ever be updated again.**
Every installed app verifies against the public key it was built with, so a new
key cannot reach them.

Builds are **not** Apple notarised and **not** Windows code signed. First launch
therefore needs a Gatekeeper or SmartScreen bypass, which the release notes and
[Installation](installation.md) both explain.

---

## If something goes wrong

**The guard failed.** The tag and the conf disagree. Delete the tag, fix
`tauri.conf.json`, commit, and re-tag.

```bash
git tag -d v1.1.0
git push origin :refs/tags/v1.1.0
```

**One runner failed.** `fail-fast` is off, so the others still finish, but the
draft release job needs all of them. Re-run the failed job; if it is a mirror or
network failure, that is usually enough.

**The manifest job failed with a missing signature.** A build succeeded without
`TAURI_SIGNING_PRIVATE_KEY` reaching it. Check the secret is still set, then
re-run the build.

**You published the draft by accident.** The manifest is already on
`desktop-latest`. Publish a corrected version rather than trying to withdraw it:
apps that already checked have the old manifest cached anyway, and a manifest
pointing backwards does not undo an installed update.

**The draft looks wrong.** Delete the draft release and the tag, fix, and start
again. Nothing has shipped until a draft is published.
