# Getting Started

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

---

## Options

| Flag | Environment variable | Default | Description |
|------|---------------------|---------|-------------|
| `--port` | `PORT` | `8990` | HTTP server port |
| `--no-browser` | — | false | Do not open the browser on startup |
| — | `AGENTO_DATA_DIR` | `~/.agento` | Directory where agents, chats, and logs are stored |
| — | `AGENTO_DEFAULT_MODEL` | Claude Sonnet | Claude model used for direct (no-agent) chat |
| — | `LOG_LEVEL` | `info` | Log level: `debug`, `info`, `warn`, `error` |
| — | `AGENTO_WORKING_DIR` | `/tmp/agento/work` | Default working directory for agent sessions |
| — | `ANTHROPIC_API_KEY` | — | Anthropic API key (optional if already stored by the Claude CLI) |
| — | `OTEL_EXPORTER_OTLP_ENDPOINT` | — | OTLP gRPC endpoint for traces/metrics/logs (see [Monitoring](monitoring.md)) |
| — | `OTEL_METRICS_EXPORTER` | — | `otlp` or `prometheus` (see [Monitoring](monitoring.md)) |

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

---

## Version

```bash
agento --version
```
