# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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
cd frontend && npm ci        # Install dependencies
make dev-frontend            # Vite dev server on :5173
npm run build                # TypeScript check + Vite bundle
npm run lint                 # ESLint
npm run typecheck            # TypeScript strict check
npm run format               # Prettier
```

### Development Setup
Two terminals are needed in dev mode:
1. `make dev-backend` — Go API server on `:8990` (or `PORT` env)
2. `make dev-frontend` — Vite dev server on `:5173` (proxies API calls to `:8990`)

### Required Environment
```bash
ANTHROPIC_API_KEY=...        # Optional (uses Claude Code CLI auth if unset)
AGENTO_DATA_DIR=~/.agento   # Optional, default: ~/.agento (supports ~ expansion, e.g. ~/.agento-dev for local dev)
PORT=8990                    # Optional, default: 8990
# OpenTelemetry (all optional — can also be configured via Settings UI)
OTEL_EXPORTER_OTLP_ENDPOINT=localhost:4317  # OTLP collector gRPC endpoint
OTEL_EXPORTER_OTLP_INSECURE=true           # Skip TLS for local collectors
OTEL_METRICS_EXPORTER=otlp                 # "otlp" or "prometheus"
OTEL_LOGS_EXPORTER=otlp                    # "otlp"
OTEL_EXPORTER_OTLP_HEADERS=key=value       # Auth headers for the collector
OTEL_METRIC_EXPORT_INTERVAL=60000          # Push interval in ms (default: 60000)
OTEL_SDK_DISABLED=true                     # Disable telemetry entirely
```

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

### Backend Layers

**`cmd/`** — Cobra CLI commands: `web` (HTTP server), `ask` (CLI), `update` (self-update). `cmd/assets.go` embeds the frontend build; `cmd/assets_dev.go` proxies to Vite.

**`internal/server/`** — Chi router setup with middleware (Recoverer, RequestID, request logger), wrapped in `otelhttp.NewHandler` for automatic HTTP trace/span generation. Mounts `/api` routes, serves SPA, and exposes a dynamic `/metrics` handler (Prometheus pull endpoint, active only when configured). Graceful shutdown with 5s timeout.

**`internal/api/`** — HTTP handlers. `Server` struct holds all service dependencies (including `MonitoringManager` and `AppConfig`). `Mount()` registers all routes. SSE streaming for live sessions via `livesessions.go` (includes per-session mutex for concurrent send serialization). `monitoring.go` exposes `GET/PUT /api/monitoring` and `POST /api/monitoring/test` for OTel configuration. `PATCH /api/chats/{id}` and `PATCH /api/claude-sessions/{id}` support title editing and favorites. `types.go` defines request/response types shared across handlers. `uploads.go` handles `POST /api/uploads` for file/image drag-drop and paste (saves to `tmp-uploads/` directory). `claude_session_insights.go` and `insight_store_adapter.go` expose per-session and aggregated insight metrics. `claude_session_journey.go` provides a step-by-step timeline visualization endpoint.

**`internal/service/`** — Business logic. `ChatService`, `AgentService`, `IntegrationService`, `NotificationService`, `TaskService`, and `ClaudeSettingsProfileService` interfaces decouple handlers from storage. `errors.go` defines typed errors for HTTP mapping.

**`internal/agent/`** — Integration with `github.com/shaharia-lab/claude-agent-sdk-go`. `runner.go` converts agent config to SDK `RunOptions`, executes sessions, streams results. `tracing.go` provides OTel span helpers for per-tool-call and per-run tracing in both chat and scheduler paths.

**`internal/storage/`** — SQLite persistence (`~/.agento/agento.db`). `SQLiteAgentStore`, `SQLiteChatStore`, `SQLiteIntegrationStore`, `SQLiteSettingsStore`, `SQLiteNotificationStore`, `SQLiteTaskStore`, `SQLiteSessionInsightsStore` implement store interfaces. `migrate_fs_to_sqlite.go` handles one-time migration from the legacy filesystem format. `telemetry.go` provides `withStorageSpan` helper for OTel span/metric instrumentation on storage operations. Uses `modernc.org/sqlite` (pure Go, no CGo).

**`internal/config/`** — Shared configuration layer. `AppConfig` loads from env. `profiles.go` has shared profile types to prevent import cycles. **Import rule**: `config` ← `service` ← `api` (never reverse).

**`internal/integrations/`** — Integration system. `registry.go` manages server lifecycle (Start/Stop/Reload). Backends: `google/` (Calendar, Gmail, Drive with OAuth), `github/` (repos, issues, PRs, actions, releases), `slack/` (channels, messages, users), `jira/` (issues, projects, boards), `confluence/` (pages, spaces, search), `telegram/` (messages, chats, media). Each backend runs as an in-process MCP server.

**`internal/claudesessions/`** — Scanner, analytics, and static analysis for Claude Code session JSONL files. `scanner.go` parses session data, `analytics.go` computes token usage and cost metrics, `cache.go` caches results in SQLite. `processor.go` and `processor_registry.go` define the session processor pipeline — 8 processors (turn count, autonomy score, tool usage, time profile, token profile, error rate, conversation depth, session rhythm) run in a single pass over each JSONL file. `insight_worker.go` subscribes to the event bus and runs processors for new/updated sessions with a 5-minute rescan loop for version-bump reprocessing. `journey.go` reconstructs a step-by-step timeline of turns and tool calls for session journey visualization.

**`internal/tools/`** — Local MCP server running in-process. Register built-in tools here (e.g., `current_time`).

**`internal/scheduler/`** — Task scheduler with background job execution. `scheduler.go` manages cron-like scheduling, `executor.go` runs jobs and records history.

**`internal/eventbus/`** — In-process event bus for decoupled communication between components (e.g., task completion triggers notifications, session discovered/updated events trigger insight processing).

**`internal/notification/`** — Notification system with SMTP email support. `handler.go` processes events, `smtp.go` sends emails, `template.go` renders notification content.

**`internal/logger/`** — Structured `slog` loggers for system-wide and per-session logging. `NewSystemLogger` writes JSON to a rotating log file via `lumberjack` (50MB, 3 backups, 30 days, compressed). `NewSessionLogger` writes per-session logs to `<logDir>/sessions/<id>.log`. Includes an `otelslog` bridge that forwards structured logs to the OTel LoggerProvider when telemetry is enabled.

**`internal/telemetry/`** — OpenTelemetry integration. `config.go` reads `OTEL_*` env vars into `MonitoringConfig`. `provider.go` initializes TracerProvider/MeterProvider/LoggerProvider with OTLP gRPC or Prometheus exporters. `manager.go` persists config to `<data_dir>/monitoring.json` and hot-reloads providers via `Update()`. `instruments.go` holds pre-built metric instruments (`agento.http.*`, `agento.agent.*`, `agento.chat.*`, `agento.storage.*`). Returns `EnvLockedError` (HTTP 409) when env vars override UI config.

**`internal/build/`** — Build-time version variables injected via `-ldflags` (Version, CommitSHA, BuildDate).

### Frontend

**`frontend/src/lib/api.ts`** — Typed API client for all backend endpoints (includes `uploadFile`, `insightsApi`, session journey).
**`frontend/src/types.ts`** — Shared TypeScript types mirroring Go structs (includes `SessionInsight`, `InsightSummary`, journey types).
**`frontend/src/App.tsx`** — React Router routes (Agents, Chats, Multi-Chat, Claude Sessions, Session Journey, Analytics/Insights, Settings, Integrations, Tasks pages).
**`frontend/src/contexts/`** — Theme and appearance state shared across components.
**`frontend/src/pages/MultiChatPage.tsx`** — Tabbed workspace for running multiple conversations in parallel with persistent tab state via localStorage.
**`frontend/src/pages/SessionJourneyPage.tsx`** — Step-by-step timeline visualization of Claude Code sessions.
**`frontend/src/pages/InsightsPage.tsx`** — Productivity and efficiency metrics dashboard (autonomy score, cache hit rate, tool usage breakdown).

### Agent Configuration
Agents are stored in the SQLite database (legacy YAML files in `~/.agento/agents/` are auto-migrated on first startup). Create and edit agents via the UI or API. Fields: `name`, `slug`, `model`, `system_prompt`, `thinking`, `permission_mode`, `capabilities` (built_in/local/mcp/integration tools). Permission modes: `bypass` (default), `default`, `plan`, `dontAsk`. Template variables: `{{current_date}}`, `{{current_time}}`.

### MCP Integration
External MCP servers defined in `mcps.yaml` (or `MCPS_FILE`). Local in-process tools registered via `internal/tools/registry.go`. Claude settings profiles stored as `~/.claude/settings_<slug>.json` with metadata at `~/.claude/settings_profiles.json`.

## Available Skills

Custom Claude Code skills in `.claude/skills/`. Invoke with `/skill-name <args>`.

| Skill | Purpose | Usage |
|-------|---------|-------|
| `/architect-reviewer` | Architecture audit — patterns, code smells, tech debt, maintainability | `/architect-reviewer audit the codebase` or `review last 7 days changes` |
| `/security-reviewer` | Security audit — OWASP, CWE, CVSS, attack scenarios, remediation | `/security-reviewer audit the API handlers` |
| `/pr-reviewer` | PR review — correctness, quality, security, UI/UX, cross-platform, docs | `/pr-reviewer 42` or `/pr-reviewer feature/my-branch` |
| `/context-updater` | Documentation & context maintenance — keeps CLAUDE.md, README, docs/ current | `/context-updater since last 7 days` |
| `/engineering` | Development agent — features, bugs, refactoring with full project context | `/engineering add pagination to list endpoints` |
| `/implement-issue` | End-to-end autonomous workflow: GitHub issue → worktree → implementation → CI → PR review → ready PR | `/implement-issue 123` |

All review skills use Opus model, run in forked context, and include cross-platform checks (Linux, macOS, Windows).

## Linting

Go linters active: `govet`, `errcheck`, `staticcheck`, `ineffassign`, `unused`, `bodyclose`, `noctx`, `unconvert`, `revive`, `misspell`, `nakedret`, `unparam`, `durationcheck`, `gocognit`, `funlen`, `lll`, `gocyclo`, `gocritic`, `makezero`, `prealloc`, `gosec`. Config in `.golangci.yml`. Pre-commit hooks enforce linting, formatting, and TypeScript checks before every commit.
