# Agento Desktop

A native desktop client for [Agento](https://github.com/shaharia-lab/agento),
built with **Tauri 2 + Rust + React**.

The backend is native Rust (`src-tauri/src/native/`), a subsystem-by-subsystem
port of the Agento Go server completed with #278 — the bundled Go sidecar is
gone. Behaviour is pinned to the Go implementation by the byte-level parity
corpus in `parity/`; see [CLAUDE.md](CLAUDE.md) for how that works.

---

## Running it

```bash
npm install
npm run app        # Tauri dev window, hot reload on save
```

Development runs against `~/.agento-desktop-dev`, not your real `~/.agento` —
two Agento processes sharing a data directory share a scheduler, which would
double-fire scheduled tasks. Release builds use the real one.

Other scripts:

| Command | What it does |
| --- | --- |
| `npm run dev` | Vite only, in a browser tab — fastest loop for pure layout work |
| `npm run app` | The real desktop window with hot reload |
| `npm run app:build` | Production bundle (`.deb`, `.AppImage`, `.rpm`) |
| `npm run build` | Typecheck + build the frontend alone |

### Linux system dependencies

Already installed on this machine. On a fresh box:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

---

## Why it doesn't look like the web app

The web version is a page: a nav rail, a big `<h1>`, a primary button in the top
right, and a wide column of cards. That layout is correct for a browser and
wrong for a window. The desktop build deliberately breaks from it:

**Three panes, not one page.** Every section is `list → detail → inspector`,
resizable by dragging the hairline dividers. You navigate *within* a window
instead of replacing its contents, which is why there's no page-level heading
anywhere — the titlebar already says where you are.

**Density.** 13px base type, 26px rows, 24px toolbar controls, hairline
`0.5px` borders. The web app runs roughly 16px/48px; at desktop distances that
reads as a website embedded in a frame.

**The window owns its chrome.** `decorations: false`, so the titlebar is ours:
drag region, back/forward, sidebar toggle, and window controls that follow
platform convention (right on Linux/Windows, reserved space for the traffic
lights on macOS).

**A status bar.** Persistent, always truthful — running agents, active model,
today's tokens and spend, connection state. Web apps almost never have one.

**Selection behaves natively.** The source list and table rows fill with the
accent colour when the window is focused and drop to neutral grey when it
isn't — the single strongest "this is a real app" signal, and something almost
no web port does.

**Keyboard first.** A ⌘K palette plus a real native menu (`src-tauri/src/menu.rs`)
that emits actions the webview handles, so the menu path and the shortcut path
run identical code.

**No browser affordances.** Text isn't selectable except where you'd actually
read or copy it, the context menu is suppressed, scrollbars are overlay-style,
and focus rings appear for keyboard navigation only.

---

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl K` | Command palette |
| `Ctrl B` | Toggle sidebar |
| `Ctrl I` | Toggle inspector |
| `Ctrl N` | New chat |
| `Ctrl ,` | Settings |
| `Ctrl [` / `Ctrl ]` | Back / forward |
| `Ctrl 1`–`7` | Jump to a section |

---

## Layout

```
┌──────────────────────────────────────────────────────┐
│ titlebar — drag, nav, window controls                │
├─────────┬────────────────────────────────────────────┤
│ sidebar │ toolbar                                    │
│ (source ├──────────┬──────────────────┬──────────────┤
│  list)  │ list     │ detail           │ inspector    │
├─────────┴──────────┴──────────────────┴──────────────┤
│ status bar                                           │
└──────────────────────────────────────────────────────┘
```

## Project structure

```
src/
  lib/
    api.ts         fetch wrapper + POST-based SSE for chat streaming
    types.ts       TypeScript mirrors of the Go JSON, field-for-field
    hooks.ts       useResource / useDebounced / usePoll
    format.ts      shared number, money, duration and date formatting
    stats.ts       sidebar + status bar counters
    icons.tsx      16px / 1.5-stroke icon set
    nav.ts         sidebar sections and view ids
    tauri.ts       window + menu bridge; degrades to a plain browser tab
  styles/
    tokens.css     type scale, spacing, control metrics, light + dark palettes
    base.css       app-shell resets (no document scrolling, no text selection)
    shell.css      titlebar, sidebar, panes, splitters, status bar
    controls.css   buttons, segmented controls, fields, switches, menus
    views.css      list rows, transcript, tables, dashboard, inspector, forms
  components/      TitleBar, Sidebar, StatusBar, CommandPalette, ui primitives
  views/           one file per section

src-tauri/
  src/lib.rs       app setup: database, migrations, api server, window, menu
  src/proxy.rs     axum server: routes every request to src/native/
  src/native/      the ported backend — one module per API area
  src/claude/      the Claude Agent SDK, ported from Go
  src/menu.rs      native menu; emits `menu://action` to the webview
  tauri.conf.json  undecorated 1280×820 window, CSP, bundle targets
  capabilities/    the window permissions the custom chrome needs
```

## Theming

Three states — light, dark, and match-system — set via the status bar, the
Appearance pane, or the palette. Tokens are defined on bare `:root` for light,
then re-declared under both `@media (prefers-color-scheme: dark)` and
`:root[data-theme="dark"]`, so an explicit choice wins in either direction.

---

## Not built yet

Multi-window, tray icon, native context menus, drag-and-drop, virtualised lists
for the session table, and Tauri auto-update.
