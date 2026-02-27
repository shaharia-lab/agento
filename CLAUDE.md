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
ANTHROPIC_API_KEY=...   # Required
AGENTS_DIR=./agents     # Optional, default: ./agents
PORT=3000               # Optional, default: 3000
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

**`internal/storage/`** — File-based persistence. `FSChatStore` uses JSONL format per session. `FSAgentStore` reads YAML agent definitions. Both implement interfaces for DI.

**`internal/config/`** — Shared configuration layer. `AppConfig` loads from env. `profiles.go` has shared profile types to prevent import cycles. **Import rule**: `config` ← `service` ← `api` (never reverse).

**`internal/tools/`** — Local MCP server running in-process. Register built-in tools here (e.g., `current_time`).

### Frontend

**`frontend/src/lib/api.ts`** — Typed API client for all backend endpoints.
**`frontend/src/types.ts`** — Shared TypeScript types mirroring Go structs.
**`frontend/src/App.tsx`** — React Router routes (Agents, Chats, Settings pages).
**`frontend/src/contexts/`** — Theme and appearance state shared across components.

### Agent Configuration
Agents are defined as YAML files in `~/.agento/agents/` (or `AGENTS_DIR`). Fields: `name`, `slug`, `model`, `system_prompt`, `thinking`, `capabilities` (built_in/local/mcp tools). Template variables: `{{current_date}}`, `{{current_time}}`.

### MCP Integration
External MCP servers defined in `mcps.yaml` (or `MCPS_FILE`). Local in-process tools registered via `internal/tools/registry.go`. Claude settings profiles stored as `~/.claude/settings_<slug>.json` with metadata at `~/.claude/settings_profiles.json`.

## Linting

Go linters active: `errcheck`, `govet`, `staticcheck`, `unused`, `gosec`, `revive`, `bodyclose`, `noctx`. Config in `.golangci.yml`. Pre-commit hooks enforce linting, formatting, and TypeScript checks before every commit.
