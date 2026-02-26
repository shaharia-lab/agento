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

This starts Agento on port **8080** and opens your browser automatically.

---

## Open the UI

Visit [http://localhost:8080](http://localhost:8080) in your browser.

---

## Options

| Flag | Environment variable | Default | Description |
|------|---------------------|---------|-------------|
| `--port` | `PORT` | `8080` | HTTP server port |
| `--no-browser` | — | false | Do not open the browser on startup |
| — | `AGENTO_DATA_DIR` | `~/.agento` | Directory where agents, chats, and logs are stored |
| — | `AGENTO_DEFAULT_MODEL` | Claude Sonnet | Claude model used for direct (no-agent) chat |
| — | `LOG_LEVEL` | `info` | Log level: `debug`, `info`, `warn`, `error` |
| — | `ANTHROPIC_API_KEY` | — | Anthropic API key (optional if already stored by the Claude CLI) |

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
