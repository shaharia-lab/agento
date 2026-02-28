# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

### Backend (Go)
```bash
make build          # Build frontend + Go binary (version-injected)
make build-go       # Build Go binary only
make dev-backend    # Run Go backend with dev tag (hot reload not included)
make test           # go test ./...
make lint           # go vet ./... (golangci-lint via pre-commit)
make tidy           # go mod tidy
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
AGENTO_DATA_DIR=~/.agento   # Optional, default: ~/.agento
PORT=8990                    # Optional, default: 8990
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

**`internal/server/`** — Chi router setup with middleware (Recoverer, RequestID, request logger). Mounts `/api` routes and serves SPA. Graceful shutdown with 5s timeout.

**`internal/api/`** — HTTP handlers. `Server` struct holds all service dependencies. `Mount()` registers all routes. SSE streaming for live sessions via `livesessions.go`.

**`internal/service/`** — Business logic. `ChatService` and `AgentService` interfaces decouple handlers from storage. `errors.go` defines typed errors for HTTP mapping.

**`internal/agent/runner.go`** — Integration with `github.com/shaharia-lab/claude-agent-sdk-go`. Converts agent config to SDK `RunOptions`, executes sessions, streams results.

**`internal/storage/`** — SQLite persistence (`~/.agento/agento.db`). `SQLiteAgentStore`, `SQLiteChatStore`, `SQLiteIntegrationStore`, `SQLiteSettingsStore` implement store interfaces. `migrate_fs_to_sqlite.go` handles one-time migration from the legacy filesystem format. Uses `modernc.org/sqlite` (pure Go, no CGo).

**`internal/config/`** — Shared configuration layer. `AppConfig` loads from env. `profiles.go` has shared profile types to prevent import cycles. **Import rule**: `config` ← `service` ← `api` (never reverse).

**`internal/integrations/`** — Integration system. `registry.go` manages server lifecycle (Start/Stop/Reload). `google/` has OAuth flow, in-process MCP servers for Calendar, Gmail, and Drive tools.

**`internal/claudesessions/`** — Scanner and analytics for Claude Code session JSONL files. `scanner.go` parses session data, `analytics.go` computes token usage and cost metrics, `cache.go` caches results in SQLite.

**`internal/tools/`** — Local MCP server running in-process. Register built-in tools here (e.g., `current_time`).

### Frontend

**`frontend/src/lib/api.ts`** — Typed API client for all backend endpoints.
**`frontend/src/types.ts`** — Shared TypeScript types mirroring Go structs.
**`frontend/src/App.tsx`** — React Router routes (Agents, Chats, Settings pages).
**`frontend/src/contexts/`** — Theme and appearance state shared across components.

### Agent Configuration
Agents are stored in the SQLite database (legacy YAML files in `~/.agento/agents/` are auto-migrated on first startup). Create and edit agents via the UI or API. Fields: `name`, `slug`, `model`, `system_prompt`, `thinking`, `permission_mode`, `capabilities` (built_in/local/mcp/integration tools). Template variables: `{{current_date}}`, `{{current_time}}`.

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

All review skills use Opus model, run in forked context, and include cross-platform checks (Linux, macOS, Windows).

## Linting

Go linters active: `errcheck`, `govet`, `staticcheck`, `unused`, `gosec`, `revive`, `bodyclose`, `noctx`. Config in `.golangci.yml`. Pre-commit hooks enforce linting, formatting, and TypeScript checks before every commit.
