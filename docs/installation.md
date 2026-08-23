# Installation

Everything you need to get Agento Desktop running, keep it updated, and remove it
again.

- [Requirements](#requirements)
- [Which file do I download](#which-file-do-i-download)
- [macOS](#macos)
- [Windows](#windows)
- [Linux](#linux)
- [Updates](#updates)
- [Where your data lives](#where-your-data-lives)
- [Uninstalling](#uninstalling)

---

## Requirements

**The Claude Code CLI, installed and signed in.** Agento runs every agent by
launching `claude` on your machine. If `claude` works in your terminal, Agento
works.

You do **not** need an Anthropic API key. Agento uses the authentication Claude
Code already holds.

If the CLI is missing, the app still opens and shows a banner explaining what to
install. Analytics and history still work; chats and scheduled runs do not.

Operating systems:

| Platform | Minimum |
| --- | --- |
| macOS | 10.15 Catalina, Apple Silicon or Intel |
| Windows | Windows 10 or 11, 64 bit |
| Linux | Any distro with GTK 3 and WebKitGTK 4.1 |

---

## Which file do I download

Downloads are on the
[releases page](https://github.com/shaharia-lab/agento/releases/latest). Releases
are tagged `v<version>`.

Two older tag shapes are still visible in the repository's release list and
neither is something to download today: `desktop-v0.1.x` was Agento Desktop
before it took over the plain `v*` tags, and `v0.1.0` through `v0.11.2` were the
Agento server, which is retired. Anything from `v1.0.0` onward is this app.

| Platform | File | Auto-update |
| --- | --- | --- |
| macOS, Apple Silicon (M1 and later) | `Agento_<version>_aarch64.dmg` | Yes |
| macOS, Intel | `Agento_<version>_x64.dmg` | Yes |
| Windows x64 | `Agento_<version>_x64-setup.exe` | Yes |
| Linux x86_64, portable | `Agento_<version>_amd64.AppImage` | Yes |
| Linux ARM64, portable | `Agento_<version>_aarch64.AppImage` | Yes |
| Debian, Ubuntu x86_64 | `Agento_<version>_amd64.deb` | Notify only |
| Debian, Ubuntu ARM64 | `Agento_<version>_arm64.deb` | Notify only |
| Fedora, RHEL, openSUSE x86_64 | `Agento-<version>-1.x86_64.rpm` | Notify only |
| Fedora, RHEL, openSUSE ARM64 | `Agento-<version>-1.aarch64.rpm` | Notify only |

The `.sig` files beside each download are used by the in-app updater. You do not
need to download them.

Not sure which Mac you have: **Apple menu → About This Mac**. "Apple M1" or later
means take the `aarch64` build.

---

## macOS

1. Open the `.dmg`.
2. Drag **Agento** to the **Applications** folder.
3. Eject the disk image and launch Agento from Applications.

### The first launch is blocked

macOS refuses to open the app the first time and says it cannot be verified. This
is expected. The app is not signed with a paid Apple Developer certificate, so
Gatekeeper treats it as an unidentified developer.

To allow it:

1. Try to open Agento once, and dismiss the warning.
2. Open **System Settings → Privacy & Security**.
3. Scroll to the Security section. You will see a line about Agento being blocked.
4. Click **Open Anyway** and confirm.

Command line alternative, if you prefer:

```bash
xattr -dr com.apple.quarantine /Applications/Agento.app
```

You only do this once. Updates the app installs itself are not quarantined, so
they launch without any prompt.

---

## Windows

1. Run `Agento_<version>_x64-setup.exe`.
2. SmartScreen shows "Windows protected your PC" because the installer is not
   signed with a code signing certificate. Click **More info**, then
   **Run anyway**.
3. Accept the administrator prompt. Agento installs for all users on the machine.
4. Launch **Agento** from the Start menu.

Agento needs the Microsoft WebView2 runtime. The installer carries a copy and
installs it if your machine does not already have it, so nothing is downloaded
during setup.

---

## Linux

### AppImage, recommended

Works on any distribution and updates itself.

```bash
chmod +x Agento_1.0.0_amd64.AppImage
./Agento_1.0.0_amd64.AppImage
```

Keep the file anywhere you like, for example `~/Applications`. There is no
installation step and no root access needed. When the app updates itself, it
replaces that file in place, so put it somewhere you can write to.

If your desktop does not show a launcher for it, install
[AppImageLauncher](https://github.com/TheAssassin/AppImageLauncher) or create a
`.desktop` file by hand.

### Debian and Ubuntu

```bash
sudo apt install ./Agento_1.0.0_amd64.deb
```

### Fedora, RHEL, openSUSE

```bash
sudo dnf install ./Agento-1.0.0-1.x86_64.rpm
```

Both packages declare `libwebkit2gtk-4.1` and `gtk3` as dependencies, so your
package manager installs whatever is missing. If a dependency cannot be resolved,
your distribution is likely older than WebKitGTK 4.1: use the AppImage instead.

### The binary is called `agento`

The package and the executable it installs are both `agento`, at
`/usr/bin/agento`.

Before `v1.0.0` the executable was `/usr/bin/agento-desktop`: the Agento server
CLI installed a binary called `agento`, and a package dropping a second one onto
your `PATH` would have shadowed it. That CLI is retired, so the name came back.

Upgrading a `.deb` or `.rpm` across that change needs nothing from you — the
package name never changed, so it is an ordinary file replacement within the same
package and your package manager removes `agento-desktop` as it adds `agento`.
The application launcher is regenerated too. Only your own scripts or shortcuts
naming `agento-desktop` need updating.

---

## Updates

Agento checks for updates on launch and from **Settings → General → Updates**, and
you can also check on demand from the **About** screen.

You choose the behaviour:

| Setting | What happens |
| --- | --- |
| **Download and install automatically** | Agento fetches the update, installs it and restarts itself |
| **Notify me** | Agento checks on launch and shows a badge; you install when you want to |
| **Never check** | No update checks at all |

This preference is stored per install, not per account, because the same person
may run an AppImage on one machine and a `.deb` on another.

### What each install type can do

| Install | Behaviour |
| --- | --- |
| macOS `.dmg` | Installs updates in place, then restarts |
| Windows `.exe` | Installs updates in place, then restarts |
| Linux AppImage | Replaces the AppImage file in place, then restarts |
| Linux `.deb` and `.rpm` | Tells you a version is available and links to the download. Nothing is installed automatically |

`.deb` and `.rpm` are notify only because `dpkg` and `rpm` own the files they
installed. If Agento overwrote them, your package database would describe a
version that is no longer on disk. Update those installs the way you update
everything else on the system, by downloading the newer package and installing it.

### Update safety

Every update payload is signed with Agento's own signing key, and the app carries
the matching public key. A download that does not verify is rejected. This is
independent of Apple and Microsoft code signing, which is why in-app updates work
smoothly even though the first launch needs a Gatekeeper or SmartScreen bypass.

### Updates are one-way

An update can add to the database, and Agento refuses to write to a database
newer than itself rather than risk corrupting it. So going *back* to an older
version stops working at the first release that changed the schema — the older
build launches and then fails on every action.

That is the only reason to take a copy of `~/.agento` before a major upgrade. It
is a single directory, so a copy is the whole backup:

```bash
cp -r ~/.agento ~/.agento.backup
```

---

## Where your data lives

Everything is in one directory, and one SQLite database inside it.

| Platform | Path |
| --- | --- |
| macOS and Linux | `~/.agento/agento.db` |
| Windows | `%USERPROFILE%\.agento\agento.db` |

That holds your agents, chats, scheduled tasks, job history, integration
settings and the index of your Claude Code sessions. To back Agento up, copy that
directory while the app is closed.

Agento reads your Claude Code history from `~/.claude` (or whatever
`CLAUDE_CONFIG_DIR` points at). It only reads: your transcripts are never
modified.

Application logs:

| Platform | Path |
| --- | --- |
| Linux | `~/.local/share/com.shaharialab.agento/logs/Agento.log` |
| macOS | `~/Library/Logs/com.shaharialab.agento/Agento.log` |
| Windows | `%LOCALAPPDATA%\com.shaharialab.agento\logs\Agento.log` |

---

## Uninstalling

| Install | How |
| --- | --- |
| macOS | Drag `Agento.app` from Applications to the Trash |
| Windows | Settings → Apps → Agento → Uninstall |
| AppImage | Delete the `.AppImage` file |
| Debian, Ubuntu | `sudo apt remove agento` |
| Fedora, RHEL | `sudo dnf remove Agento` |

None of these removes your data. To delete that too:

```bash
rm -rf ~/.agento
```

Your Claude Code history in `~/.claude` is untouched either way.
