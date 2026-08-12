# Getting Started

**Requirements:** the [Claude Code CLI](https://claude.ai/code), installed and
authenticated. If `claude` runs in your terminal, Agento works — it uses the
authentication Claude Code already has, so no Anthropic API key is needed.

## Install

**Homebrew (macOS and Linux)**

```bash
brew tap shaharia-lab/homebrew-tap
brew install agento
```

**Download a binary**

Go to [Releases](https://github.com/shaharia-lab/agento/releases), download the archive for your platform, extract it, and put `agento` somewhere on your `$PATH`.

```bash
# Example: Linux x86_64
tar -xzf agento_Linux_x86_64.tar.gz
sudo mv agento /usr/local/bin/
```

**Build from source**

```bash
git clone https://github.com/shaharia-lab/agento.git
cd agento
make build
```

---

## Start the server

```bash
agento web
```

This starts Agento on port **8990** and opens your browser automatically.

---

## Open the UI

Visit [http://localhost:8990](http://localhost:8990) in your browser.

Agento binds to **loopback only** and ships **without authentication** — it is
meant to run on the machine you are working at. To reach it from a phone or
another computer, see [Security](security.md#reaching-agento-from-another-device).

---

## What happens on first run

Agento finds the Claude Code history already on your disk and starts indexing
it. On a large corpus the first scan takes a few minutes; the sessions list shows
its progress while it runs, and everything else is usable in the meantime.

When it finishes you have a searchable history of every Claude Code session,
cost analytics and productivity insights — see
[Claude Sessions](claude-sessions.md). Nothing is uploaded; the transcripts are
read where they already are and cached locally.

From there:

- Build an [agent](agents.md) and chat with it
- Put it on a [schedule](tasks.md)
- Connect [integrations](integrations.md) so agents can use your tools

---

## Options

| Flag | Environment variable | Default | Description |
|------|---------------------|---------|-------------|
| `--port` | `PORT` | `8990` | HTTP server port |
| `--no-browser` | — | false | Do not open the browser on startup |
| — | `AGENTO_BIND` | `127.0.0.1` | Interface to listen on. `0.0.0.0` exposes an unauthenticated API to your whole network — read [Security](security.md) first |
| — | `AGENTO_PUBLIC_URL` | — | The externally reachable URL, when Agento is behind a reverse proxy, a tunnel, or serving Telegram webhooks |
| — | `AGENTO_DATA_DIR` | `~/.agento` | Directory where agents, chats, and logs are stored. `~` is expanded |
| — | `CLAUDE_CONFIG_DIR` | `~/.claude` | Claude Code's own variable — which account agent runs authenticate as |
| — | `AGENTO_DEFAULT_MODEL` | `sonnet` | Claude model used for direct (no-agent) chat |
| — | `ANTHROPIC_DEFAULT_SONNET_MODEL` | — | Anthropic's standard variable, used as a soft default when the above is unset |
| — | `LOG_LEVEL` | `info` | Log level: `debug`, `info`, `warn`, `error` |
| — | `AGENTO_WORKING_DIR` | `/tmp/agento/work` | Default working directory for agent sessions |
| — | `ANTHROPIC_API_KEY` | — | Anthropic API key (optional if already stored by the Claude CLI) |
| — | `OTEL_EXPORTER_OTLP_ENDPOINT` | — | OTLP gRPC endpoint for traces/metrics/logs (see [Monitoring](monitoring.md)) |
| — | `OTEL_METRICS_EXPORTER` | — | `otlp` or `prometheus` (see [Monitoring](monitoring.md)) |

Most of these also have a field in **Settings**. An environment variable always
wins, and the UI shows the field as locked when one is set.

**Example: run on a different port**

```bash
agento web --port 9090
# or
PORT=9090 agento web
```

**Example: custom data directory**

```bash
AGENTO_DATA_DIR=/data/agento agento web
```

---

## Run Agento in the background

`agento web` runs in the foreground. To keep Agento running across logout, reboot, and crashes, install it as a user-level background service:

```bash
agento service install
```

- **macOS** — a LaunchAgent at `~/Library/LaunchAgents/com.shaharialab.agento.plist`. It starts at **login** (not at boot) and restarts automatically on crash.
- **Linux** — a systemd user unit at `~/.config/systemd/user/agento.service` with `Restart=on-failure`. `install` also runs `loginctl enable-linger $USER` so the service keeps running after you log out (required on headless/SSH machines).

Manage it with:

```bash
agento service status      # installed/enabled/running, PID, URL, log path (exit 1 when not running)
agento service stop        # stop without removing
agento service start       # start again
agento service restart
agento service logs -f     # tail the service log (~/.agento/logs/service.log)
agento service uninstall   # stop, disable, and remove the unit — no residue
```

The unit runs `agento web --no-browser` with your install-time `PATH`, `PORT`, and `AGENTO_DATA_DIR` baked in, so the service can find the Claude Code CLI. Windows is not supported.

---

## Update

```bash
agento update
```

This checks GitHub for a newer release and replaces the binary in place. Add `--yes` to skip the confirmation prompt.

When Agento runs as a background service (see above), a successful `agento update` also restarts the service so the new version goes live immediately. Use `--no-restart` to skip that and restart it later with `agento service restart`.

---

## Version

```bash
agento --version
```

---

## Command reference

```
agento web [--port int] [--no-browser]              Start the web UI
agento ask [--agent slug] [--no-thinking]           Ask an agent a one-off question
           [--agents-dir path] [--mcps-file path]
           <question> [session-id]
agento update [-y] [--no-restart]                   Update to the latest release
agento service <install|uninstall|start|stop|restart|status|logs>
agento service logs [-f] [-n lines]                 Read the service log
```

Passing a session ID to `agento ask` continues that conversation.

---

## Where to go next

- [Claude Sessions](claude-sessions.md) — the analytics built from your Claude Code history
- [Agents](agents.md) — system prompts, models, tools, template variables
- [Tasks](tasks.md) — running agents on a schedule
- [Integrations](integrations.md) — Google, GitHub, Slack, Jira, Confluence, Telegram, WhatsApp
- [Pricing](pricing.md) — how cost is calculated and how to maintain the catalog
- [Security](security.md) — network exposure, guards, and where your data lives
- [Monitoring](monitoring.md) — OpenTelemetry traces, metrics and logs
- [Development](development.md) — architecture and contribution workflow
