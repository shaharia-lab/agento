# Development Guide

## Requirements

- Go 1.25+
- Node.js 22+
- npm

---

## Local setup

```bash
git clone https://github.com/shaharia-lab/agento.git
cd agento
```

Install frontend dependencies:

```bash
cd frontend && npm ci --legacy-peer-deps
```

---

## Run in development mode

Open two terminals.

**Terminal 1 — backend**

```bash
make dev-backend
```

**Terminal 2 — frontend (with hot reload)**

```bash
make dev-frontend
```

The backend serves the API on `:8990`. The frontend dev server proxies API calls to it.

---

## Build a production binary

```bash
make build
```

This builds the frontend first, embeds the static files, then compiles the Go binary at `./agento`.

The binary includes version info from the current git state:

```bash
./agento --version
# agento version v0.1.0 (commit abc1234, built 2026-02-26T10:00:00Z)
```

---

## Run tests

```bash
make test
```

---

## Lint

```bash
make lint
```

Runs `go vet`, `golangci-lint` (Go), and ESLint + Prettier (TypeScript).

---

## Project layout

```
agento/
├── cmd/              # Cobra commands (web, ask, update)
├── frontend/         # React + TypeScript UI
├── internal/
│   ├── agent/        # SDK integration, RunOptions, session execution
│   ├── api/          # HTTP handlers
│   ├── build/        # Build-time version variables
│   ├── config/       # AppConfig, AgentConfig, MCP config
│   ├── logger/       # Structured slog loggers (system + per-session), log rotation
│   ├── server/       # HTTP server wiring
│   ├── claudesessions/ # Claude session scanner, analytics, processor pipeline, journey
│   ├── eventbus/       # In-process event bus
│   ├── integrations/   # Integration registry + MCP servers (Google, GitHub, Slack, Jira, Confluence, Telegram)
│   ├── notification/   # Notification system (SMTP email)
│   ├── scheduler/      # Task scheduler and job executor
│   ├── service/        # Business logic (AgentService, ChatService, TaskService, NotificationService, etc.)
│   ├── storage/        # SQLite persistence (~/.agento/agento.db)
│   ├── telemetry/      # OpenTelemetry traces, metrics, logs (config, providers, hot-reload manager)
│   └── tools/          # Local MCP tool server
├── docs/             # Documentation
├── .goreleaser.yaml  # Release configuration
└── Makefile
```

---

## Release process

Releases are created automatically when a `v*` tag is pushed:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The release workflow builds cross-platform binaries, pushes a Homebrew formula to the tap, and creates a GitHub Release with a changelog.

To verify the release config locally (no publish):

```bash
goreleaser release --snapshot --clean
```
