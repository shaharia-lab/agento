# Releasing

How a desktop release is cut, and the two things that break it.

Desktop releases are tagged `desktop-v*` from the `desktop` branch. The Agento
server keeps its own `v*` tags on `main`. The patterns do not overlap, so both
ship independently from one repository.

- [The flow](#the-flow)
- [Cutting a release](#cutting-a-release)
- [The two guards](#the-two-guards)
- [Release candidates and dry runs](#release-candidates-and-dry-runs)
- [What gets built](#what-gets-built)
- [The update manifest](#the-update-manifest)
- [Signing keys](#signing-keys)
- [If something goes wrong](#if-something-goes-wrong)

---

## The flow

```
  bump tauri.conf.json  ──>  tag desktop-vX.Y.Z  ──>  guard + build matrix
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

1. **Bump the version** in `src-tauri/tauri.conf.json`. Commit it on
   `desktop`.

   ```json
   { "version": "0.2.0" }
   ```

2. **Tag that commit** and push the tag.

   ```bash
   git tag desktop-v0.2.0
   git push origin desktop-v0.2.0
   ```

3. **Wait for the build.** Five runners, one per target: Linux x86_64, Linux
   aarch64, macOS Apple Silicon, macOS Intel, Windows x86_64. Tauri cannot
   cross-compile its bundles, which is why each needs its own runner.

4. **Check the draft release.** Confirm every platform's installer is attached
   and `latest.json` is there. Download one and launch it if the change was
   risky.

5. **Publish the draft.** That fires `promote`, which copies the staged manifest
   to `desktop-latest`. Installed apps start seeing the update within their next
   check.

---

## The two guards

**The tag must equal `desktop-v` plus the version in `tauri.conf.json`.** A guard
job fails the build otherwise, before anything is built.

This is not tidiness. The installed app compares the version baked into it at
build time against the manifest's version, which comes from the tag. If the tag
says `0.2.0` and the conf still says `0.1.0`, every "updated" install keeps
reporting `0.1.0` and is offered the same update forever.

**Every platform must produce a signature.** The manifest script aborts the
release if a payload has no `.sig`, or if a platform produced nothing at all. A
manifest that silently omitted a platform would strand exactly the users already
running it, and their app would keep reporting "up to date" indefinitely.

---

## Release candidates and dry runs

**A prerelease tag** builds and drafts identically, is auto-marked prerelease, and
is **never promoted**:

```bash
git tag desktop-v0.2.0-rc.1
git push origin desktop-v0.2.0-rc.1
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
stable URL. `releases/latest` could not be used: it would point at the Go
server's `v*` releases. That tag is itself marked prerelease so it never
displaces the real latest release on the repository page.

---

## Signing keys

Updates are signed with **our own minisign key**, generated by
`tauri signer generate`. It has nothing to do with Apple or Microsoft code
signing. It only proves an update came from us, which is why in-app updates work
on unsigned macOS builds: Gatekeeper gates the first launch of a downloaded app,
not a bundle the updater swapped in.

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
git tag -d desktop-v0.2.0
git push origin :refs/tags/desktop-v0.2.0
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
