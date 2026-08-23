# Agento Desktop

A native desktop app for Agento: build AI agents, chat with them, schedule them,
and see exactly what your Claude Code usage costs. Everything runs on your own
machine.

One window, no browser tab, no server to start.

![Agento Desktop's Insights view: cost, autonomy, cache-hit and tool-error cards over your Claude Code sessions, with tool calls attributed to skills, MCP servers and sub-agents](docs/screenshots/light/insights.png)

> This is the **`desktop`** branch, and the app is the whole of it (#388).
> [`main`](https://github.com/shaharia-lab/agento/tree/main) carries Agento's Go
> web server, which is a separate download and has its own releases on `v*`
> tags. Desktop releases are tagged `desktop-v*`.

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

## What you get

### Every token type, priced properly

Input, output, cache reads and cache writes bill at very different rates, so
Agento keeps them apart instead of multiplying one total by one price. The model
with the most tokens is often not the model taking your money.

![Token Usage: token composition, tokens over time, cache efficiency](docs/screenshots/light/token-usage.png)

Cost is attributed to the model that spent it, including work done inside
sub-agents, and to the project it was spent on.

<details>
<summary>Show screenshot</summary>

![Cost by model and by project](docs/screenshots/light/cost-by-model.png)

</details>

### Find out whether you are getting more effective

Insights goes past raw counts: turns per session, how far Claude got before it
had to ask you something, cache hit rate and tool error rate, then every tool
call attributed to the skill, plugin, MCP server or sub-agent responsible.

<details>
<summary>Show screenshot</summary>

![Tool calls attributed to skills, plugins, MCP servers and reasoning effort](docs/screenshots/light/insights-breakdowns.png)

</details>

Durations mean active time, not wall clock. Idle gaps beyond a threshold you
control are excluded everywhere a duration is shown.

### Understand your working patterns

Sessions per day, model mix, busiest days, and a weekly heatmap that counts a
session in every hour it was running rather than only the hour it finished.

<details>
<summary>Show screenshot</summary>

![Weekly rhythm heatmap and busiest sessions](docs/screenshots/light/activity-heatmap.png)

</details>

### Browse and search every session you have ever run

Filtered and paged in SQL, so it stays fast whether you have 50 sessions or
5,000. Search titles and content, filter by project, model, date, cost or
duration, and see permission mode, linked pull requests, tokens and cost on
every row, with the inspector beside it.

<details>
<summary>Show screenshot</summary>

![The Sessions list with the inspector](docs/screenshots/light/sessions-list.png)

</details>

### Replay any session step by step

Open a session and read the whole run in order: every prompt, response, tool
call and result, with sub-agent delegations and failing commands where they
happened. When a long autonomous run goes wrong, this is where you find out
where.

![A session transcript with tool calls and a sub-agent delegation expanded](docs/screenshots/light/session-journey.png)

Each session carries its own metrics: messages, active duration, sub-agent time,
tokens by type and cost.

<details>
<summary>Show screenshot</summary>

![Session detail with the inspector's activity and token panels](docs/screenshots/light/session-detail.png)

</details>

### Chat with your agents

Every chat runs through the Claude Code CLI you already have, with the agent's
system prompt, model and tool allowlist applied. Tool calls and Markdown answers
render inline; the inspector shows what the turn cost.

<details>
<summary>Show screenshot</summary>

![A chat with the code-reviewer agent](docs/screenshots/light/chats.png)

</details>

### Build agents without writing code

Give an agent a name, a system prompt, a model, a thinking mode and an explicit
list of tools it may use. Template variables like `{{current_date}}` are filled
in at runtime.

<details>
<summary>Show screenshot</summary>

![The agents list and builder](docs/screenshots/light/agents.png)

</details>

Tools are an allowlist: built-in tools, Agento's local tools, and each connected
integration's tools, ticked one by one.

<details>
<summary>Show screenshot</summary>

![The agent builder's capabilities section](docs/screenshots/light/agent-builder.png)

</details>

### Put your agents on a schedule

Run any agent on a cron expression, a fixed interval, or once at a specific
time. Every execution is recorded with its status, duration and full output.

<details>
<summary>Show screenshot</summary>

![Scheduled tasks with recent runs](docs/screenshots/light/tasks.png)

</details>

### Connect the tools you already use

Each integration runs as an in-process MCP server inside the app, so there is no
extra daemon to operate. GitHub, Slack, Jira, Confluence, Telegram and Google
(Calendar, Gmail, Drive) are built in; any other MCP server can be added through
`~/.agento/mcps.yaml`.

<details>
<summary>Show screenshot</summary>

![Integrations: GitHub connected, with its services and tools](docs/screenshots/light/integrations.png)

</details>

### Keep the pricing catalog honest

Rates ship for Anthropic, Moonshot, Z.ai and Alibaba models and are
effective-dated. A model with no published rate is reported as unknown instead
of being quietly priced as something else.

<details>
<summary>Show screenshot</summary>

![The model pricing catalog](docs/screenshots/light/settings-pricing.png)

</details>

### Your data stays yours

Agento reads `~/.claude` and caches results in a local SQLite database. Nothing
is uploaded, there is no account, and there is no server component. Projects you
would rather leave out of the numbers can be hidden from every report, and the
idle threshold behind the duration metrics is yours to set.

<details>
<summary>Show screenshot</summary>

![Data settings: idle threshold and hidden projects](docs/screenshots/light/settings-data.png)

</details>

*Every screenshot above is taken from the app over a synthetic dataset; see
[`docs/screenshots/README.md`](docs/screenshots/README.md).*

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
