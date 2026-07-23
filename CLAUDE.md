# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Agento

Agento is a local, self-hosted platform for building and running AI agents through a web UI and CLI. It runs on top of the Claude Code CLI already installed on the user's machine (via `github.com/shaharia-lab/claude-agent-sdk-go`) — no separate API key required. It ships as a single Go binary with the React frontend embedded, persisting everything in SQLite at `~/.agento/agento.db`.

Core domain objects:
- **Agents** — user-defined: name, slug, model, system prompt, thinking mode, permission mode, and an explicit tool allowlist (built-in Claude Code tools, local in-process tools, external MCP servers, integrations).
- **Chats** — persistent multi-turn conversations with agents, streamed live over SSE; a tabbed multi-chat workspace runs conversations in parallel.
- **Integrations** — external services (Google, GitHub, Slack, Jira, Confluence, Telegram, WhatsApp) exposed as agent tools, each running as an in-process MCP server.
- **Tasks** — cron-scheduled agent runs with full job history.
- **Triggers** — rules that match incoming Telegram messages and dispatch agent runs, replying on the same channel.
- **Claude Sessions & Insights** — scans local Claude Code JSONL session files for browsing, token/cost analytics, journey timelines, and productivity metrics.

Cross-cutting: OpenTelemetry instrumentation (hot-reloadable from the UI), SMTP notifications, self-update (`agento update`), and an `agento ask` CLI for one-shot queries.

## Commands

### Backend (Go)
```bash
make build          # Build frontend + Go binary (version-injected)
make build-go       # Build Go binary only
make dev-backend    # Run Go backend with dev tag (hot reload not included)
make test           # go test ./...
make lint           # golangci-lint run ./...
make tidy           # go mod tidy
make generate       # Regenerate all mocks via mockery (reads .mockery.yaml)
```

Run a single Go test:
```bash
go test ./internal/service/... -run TestChatService
```

### Frontend (React/TypeScript)
```bash
cd frontend && npm ci --legacy-peer-deps   # Install dependencies
make dev-frontend            # Vite dev server on :5173
npm run build                # TypeScript check + Vite bundle
npm run lint                 # ESLint
npm run typecheck            # TypeScript strict check
npm run format               # Prettier
```

### E2E (Playwright)
```bash
cd e2e && npm ci && npm test   # Headless run; npm run test:ui / test:headed / test:debug also available
```

### Development Setup
Two terminals are needed in dev mode:
1. `make dev-backend` — Go API server on `:8990` (or `PORT` env)
2. `make dev-frontend` — Vite dev server on `:5173` (proxies API calls to `:8990`)

### Environment
All optional: `ANTHROPIC_API_KEY` (falls back to Claude Code CLI auth), `AGENTO_DATA_DIR` (default `~/.agento`, supports `~` expansion), `PORT` (default `8990`). OpenTelemetry is configured via standard `OTEL_*` env vars or the Settings UI — see `docs/monitoring.md`. Env vars override UI settings; the API returns HTTP 409 (`EnvLockedError`) when a UI update targets an env-locked value.

## Architecture

### Request Flow
```
Browser → Vite (dev) / embedded FS (prod) → React SPA
                                          ↓
                              chi router (internal/server/)
                                          ↓
                              API handlers (internal/api/)
                                          ↓
                              Services (internal/service/)
                                          ↓
                        Storage (internal/storage/) + Agent SDK
```

### Backend Packages

One line per package — list files in a directory to see current contents.

- **`cmd/`** — Cobra commands: `web` (HTTP server), `ask` (CLI), `update` (self-update). Frontend embedding lives at repo root: `assets.go` (`//go:embed frontend/dist`, prod) and `assets_dev.go` (nil FS → server proxies to Vite), wired through `cmd/webfs.go`.
- **`internal/server/`** — Chi router + middleware, wrapped in `otelhttp` for automatic tracing. Mounts `/api`, serves the SPA, exposes a dynamic `/metrics` Prometheus endpoint. Graceful shutdown with 5s timeout.
- **`internal/api/`** — HTTP handlers. `Server` struct holds all service dependencies; `Mount()` registers routes. SSE streaming in `livesessions.go` (per-session mutex serializes concurrent sends). Shared request/response types in `types.go`.
- **`internal/service/`** — Business logic behind interfaces (`ChatService`, `AgentService`, `IntegrationService`, `NotificationService`, `TaskService`, `ClaudeSettingsProfileService`) so handlers never touch storage directly. Typed errors in `errors.go` map to HTTP statuses.
- **`internal/agent/`** — Claude Agent SDK integration: converts agent config to SDK `RunOptions`, executes sessions, streams results; OTel span helpers for per-tool-call and per-run tracing.
- **`internal/storage/`** — SQLite persistence via `modernc.org/sqlite` (pure Go, **no CGo**). One `SQLite*Store` per domain implementing a store interface. `migrate_fs_to_sqlite.go` migrates the legacy filesystem format once. `withStorageSpan` instruments all operations.
- **`internal/config/`** — `AppConfig` from env; shared profile types in `profiles.go` to avoid import cycles.
- **`internal/integrations/`** — Integration registry (Start/Stop/Reload lifecycle). One subpackage per backend (google, github, slack, jira, confluence, telegram, whatsapp), each an in-process MCP server.
- **`internal/trigger/`** — Dispatcher matching incoming Telegram messages against trigger rules, running the matched agent (bounded concurrency), and replying via Telegram.
- **`internal/claudesessions/`** — Scanner/analytics for Claude Code session JSONL files, cached in SQLite. Insight pipeline: 8 processors run in a single pass per file; `insight_worker.go` reacts to event-bus session events with a 5-minute rescan loop for version-bump reprocessing. `journey.go` builds step-by-step session timelines.
- **`internal/tools/`** — Local in-process MCP server; register built-in tools in `registry.go`.
- **`internal/scheduler/`** — Cron-like task scheduling and job execution with history.
- **`internal/eventbus/`** — In-process pub/sub decoupling components (task completion → notifications, session discovered → insight processing).
- **`internal/notification/`** — Event-driven notifications with SMTP email delivery and templates.
- **`internal/logger/`** — Structured `slog`: rotating system log (lumberjack) + per-session logs at `<logDir>/sessions/<id>.log`; `otelslog` bridge forwards logs to OTel when enabled.
- **`internal/telemetry/`** — OTel providers (OTLP gRPC or Prometheus), config persisted to `<data_dir>/monitoring.json`, hot-reload via `Manager.Update()`, pre-built instruments (`agento.http.*`, `agento.agent.*`, `agento.chat.*`, `agento.storage.*`).
- **`internal/updater/`** — Release checker (cached 1h, feeds the UI update banner) and in-place installer behind `agento update`.
- **`internal/build/`** — Version variables injected via `-ldflags`.

**Import rule**: `config` ← `service` ← `api` (never reverse).

### Frontend

- `frontend/src/lib/api.ts` — typed API client for all backend endpoints.
- `frontend/src/types.ts` — TypeScript types mirroring Go structs; keep in sync when changing API types.
- `frontend/src/App.tsx` — React Router routes; one page component per feature under `frontend/src/pages/`.
- `frontend/src/contexts/` — theme/appearance state.

### Agent Configuration
Agents are stored in SQLite (legacy YAML files in `~/.agento/agents/` are auto-migrated on first startup); create/edit via UI or API. Permission modes: `bypass` (default), `default`, `plan`, `dontAsk`. System prompts support `{{current_date}}` and `{{current_time}}` template variables.

### MCP Integration
External MCP servers are defined in `mcps.yaml` (or `MCPS_FILE`); local in-process tools go in `internal/tools/registry.go`. Claude settings profiles are stored as `~/.claude/settings_<slug>.json` with metadata in `~/.claude/settings_profiles.json`.

## Linting

Go: `golangci-lint` with a strict linter set — config in `.golangci.yml`. Frontend: ESLint + Prettier + strict TypeScript. Pre-commit hooks enforce linting, formatting, and type checks on every commit.
