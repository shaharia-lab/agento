# Agento Desktop

A native desktop app for [Agento](https://github.com/shaharia-lab/agento): build
AI agents, chat with them, schedule them, and see exactly what your Claude Code
usage costs. Everything runs on your own machine.

One window, no browser tab, no server to start.

- **Docs:** [User Guide](docs/user-guide.md) | [Installation](docs/installation.md) | [Troubleshooting](docs/troubleshooting.md)
- **For contributors:** [Development](docs/development.md) | [Architecture](docs/architecture.md) | [Releasing](docs/releasing.md)

---

## Before you install

Agento Desktop needs the **[Claude Code CLI](https://claude.ai/code)**, installed
and signed in. If `claude` runs in your terminal, you are ready.

There is no Anthropic API key to enter. Agento reuses the sign-in Claude Code
already has. The app checks for the CLI on launch and tells you if it is missing.

---

## Download

Get the files from the
[latest desktop release](https://github.com/shaharia-lab/agento/releases?q=desktop&expanded=true).
Release tags start with `desktop-v`.

| Platform | Download | Auto-update |
| --- | --- | --- |
| **macOS** (Apple Silicon) | `Agento_<version>_aarch64.dmg` | Yes, in-app |
| **macOS** (Intel) | `Agento_<version>_x64.dmg` | Yes, in-app |
| **Windows** (x64) | `Agento_<version>_x64-setup.exe` | Yes, in-app |
| **Linux** (any distro) | `Agento_<version>_amd64.AppImage` or `_aarch64.AppImage` | Yes, in-app |
| **Linux** (Debian, Ubuntu) | `Agento_<version>_amd64.deb` or `_arm64.deb` | No, notify only |
| **Linux** (Fedora, RHEL, openSUSE) | `Agento-<version>-1.x86_64.rpm` or `.aarch64.rpm` | No, notify only |

**Auto-update** means the app can download and install a new version itself, then
restart. The `.deb` and `.rpm` packages are owned by your system package manager,
so Agento never overwrites them: it tells you a new version exists and links to
the download. Pick the AppImage if you want in-app updates on Linux.

Every download is also published with a `.sig` file. That signature is Agento's
own update key, used by the in-app updater to verify a download.

---

## Install

### macOS

1. Open the `.dmg` and drag **Agento** into Applications.
2. Launch it. macOS blocks it the first time, because the app is not signed with
   an Apple Developer certificate.
3. Open **System Settings → Privacy & Security**, scroll down, and click
   **Open Anyway**. Confirm once.

Only the first launch needs this. Updates installed by the app do not.

### Windows

1. Run `Agento_<version>_x64-setup.exe`.
2. Windows SmartScreen warns about an unrecognized publisher. Click **More info**,
   then **Run anyway**.
3. The installer asks for administrator rights, because it installs for all users.

The bundled WebView2 runtime installs automatically if your machine does not
already have it.

### Linux, AppImage

```bash
chmod +x Agento_*.AppImage
./Agento_*.AppImage
```

No installation, no root. Keep the file wherever you like. The app updates itself
in place.

### Linux, Debian or Ubuntu

```bash
sudo apt install ./Agento_*_amd64.deb
```

### Linux, Fedora, RHEL or openSUSE

```bash
sudo dnf install ./Agento-*.x86_64.rpm
```

Both packages declare their GTK and WebKitGTK dependencies, so your package
manager pulls in what is missing.

---

## First run

The app opens on **Chats**. On launch it starts reading the Claude Code history
already on your disk. A large history takes a few minutes to index the first
time, and the Sessions view shows progress while it works. Everything else is
usable meanwhile.

Your data lives in `~/.agento` (`%USERPROFILE%\.agento` on Windows) as a single
SQLite file. Nothing is uploaded anywhere.

Read the [User Guide](docs/user-guide.md) next.

---

## Keyboard shortcuts

`Ctrl` on Windows and Linux, `⌘` on macOS.

| Shortcut | Action |
| --- | --- |
| `Ctrl K` | Command palette |
| `Ctrl N` | New chat |
| `Ctrl ,` | Settings |
| `Ctrl B` | Show or hide the sidebar |
| `Ctrl I` | Show or hide the inspector |
| `Ctrl [` / `Ctrl ]` | Back / forward |
| `Ctrl 1` to `Ctrl 7` | Jump to a section |

---

## Building from source

```bash
cd desktop
npm install
npm run app          # dev window with hot reload
npm run app:build    # installers for your platform
```

See [Development](docs/development.md) for the full setup, including Linux system
dependencies and how the parity test suite works.

---

## License

MIT, same as the rest of the Agento repository.
