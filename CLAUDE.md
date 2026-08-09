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
- **`internal/claudesessions/`** — Scanner/analytics for Claude Code session JSONL files, cached in SQLite. Sub-agent transcripts under `<session-id>/subagents/agent-*.jsonl` (plus their `.meta.json` sidecars) are scanned into `claude_subagent_cache` with their own mtime tracking and rolled up additively: `ClaudeSessionSummary.Usage` stays main-thread, `SubagentUsage` holds delegated work, and `TotalUsage()` is what aggregate reporting uses. Note every event in a sub-agent transcript carries `isSidechain: true`. Cache-creation tokens are split by cache TTL (`usage.cache_creation.ephemeral_5m/1h_input_tokens`) because the tiers bill differently — 1.25× input for 5-minute, 2× for 1-hour — and cost is priced per assistant message against the effective-dated catalog in `internal/pricing/` (never whole-session at the first-seen model), with an explicit "unknown" path so a model with no published rates contributes no cost rather than being priced as another model. Session labels come from Claude Code's own `custom-title` / `ai-title` transcript events (last occurrence wins; neither carries a timestamp) into `native_title` / `ai_title`, resolved as `custom_title || native_title || ai_title || preview` — Agento's own rename lives in `custom_title`, is never written by a scan, and always wins. `message_count` counts conversational turns — user events carrying genuine input plus assistant events containing a text block — while `event_count` holds the raw top-level event total; `isUserTurnContent` (`processor.go`) is the single predicate behind both the scanner's user-turn count and the insight pipeline's `isTurnStart`, so those two can no longer drift apart; `isAssistantReply` is the scanner-side other half and has no insight consumer. The sidechain check is deliberately left to each caller, because the flag means "delegated work, skip" in a parent transcript but is set on every event of a sub-agent transcript. Beyond conversation events the scanner reads `pr-link` (linked pull requests, deduplicated by URL into `claude_session_pr`), `system`/`compact_boundary` (compaction count plus dropped tokens — `cumulativeDroppedTokens` is a running total, so the max is the session's figure rather than the sum; older Claude Code releases omit it and those boundaries contribute `preTokens - postTokens` instead), and the `agent-name` / `permission-mode` / `mode` / `relocated` / `worktree-state` metadata events. Those metadata events carry no timestamp and are re-appended on every resume, so last-in-file wins, exactly like the title events. `boundsSessionTimeRange` (`scanner.go`) decides what may extend `start_time`/`last_activity` and is a **denylist on purpose** — an allowlist of "conversation events" would silently shrink the range for sessions ending in a timestamped non-conversation event such as `attachment` or `queue-operation`. `pr-link` is denied despite carrying a real timestamp, because it can post-date the conversation and would reorder the sessions list. Bumping `CurrentScannerVersion` (`scanner.go`) forces a one-time re-read of every transcript on the next scan, for changes that leave cached rows incomplete without any file mtime changing. **Per-session cost is stored, not derived** (#188): the scan accumulates it per assistant message and persists the four-part breakdown on `claude_session_cache` (and on `claude_subagent_cache`, rolled up by the same `LEFT JOIN` that sums delegated tokens), so `Cost` is main-thread, `SubagentCost` is delegated, and `TotalCost()` is what aggregate reporting uses — exactly mirroring `Usage`/`SubagentUsage`/`TotalUsage()`. The session list, the detail page and the analytics totals all read that one value; nothing re-prices from aggregate tokens, because a summary keeps no per-message timing and could only approximate by picking one model and one instant for the whole session. `UnpricedModels`/`UnpricedTokens` travel with it so a partly-priced total is disclosed as a floor rather than presented as complete — distinct from a non-billable model, which resolves and contributes a confident $0.00. The cost of storing is that a rate edit no longer reaches cached rows on its own: `claude_cache_metadata.pricing_rev` records the catalog fingerprint the costs were computed under, and a drift invalidates every cached mtime to force a re-read, the same mechanism `scanner_version` uses (`pricingStaleness` in `scanner.go`, `Cache.pricingChanged` in `cache.go`). Re-reading is the only correct response, since re-pricing needs each message's own model and timestamp. That re-read is **asynchronous** (#208): `Cache.List` never scans on the request goroutine — it serves the cached rows immediately and calls `EnsureScan`, which admits exactly one background scan under a short critical section rather than holding `c.mu` for the scan's full duration (~18s on a 1,500-transcript corpus; moving the scan to a goroutine without restructuring the lock would only unblock the *triggering* request). The one exception is a cold cache, which waits up to `coldStartScanWait` because an empty list reads as "no sessions" rather than "not scanned yet". `GET /api/claude-sessions/status` exposes `costs_stale`/`scan_in_progress` so the UI labels figures priced under the old catalog instead of blocking on them — a separate endpoint because `GET /claude-sessions` returns a bare array. `pricing_rev` is advanced inside the scan only after its changes apply, so a failed scan leaves the drift recorded and retryable. Insight pipeline: 9 processors run in a single pass over the parent transcript followed by each sub-agent transcript; `AttributionProcessor` breaks tool calls down by the skill, plugin, MCP server and sub-agent responsible plus the reasoning-effort tier, counting **per `tool_use` block** because Claude Code splits one assistant message into several events that all carry the same `attribution*` fields, and deriving MCP server/tool from the `mcp__<server>__<tool>` block name because `attributionMcpServer`/`attributionMcpTool` are sticky (they hold the last MCP tool touched and persist onto unrelated later turns). All six breakdowns are aggregated and rendered (#202) — `attributionAgent` was previously decoded and dropped, and `effort_breakdown`/`mcp_tool_breakdown` stored but never surfaced; the MCP-tool panel sits directly under the MCP-server one because it is that chart's drill-down, not a peer dimension. `agent_breakdown` is empty for main-thread-only work, since Claude Code stamps `attributionAgent` on sub-agent transcripts only; `insight_worker.go` reacts to event-bus session events with a 5-minute rescan loop for version-bump reprocessing. `journey.go` builds step-by-step session timelines and nests each delegated sub-agent's steps under the `Task` `tool_use` that spawned it (joined exactly on `toolUseId`), with unmatched sub-agents appended to their turn rather than dropped. Claude Code no longer emits `progress` events (none across 2.1.177–2.1.224), so the old `progress` rendering path is gone.
- **`internal/tools/`** — Local in-process MCP server; register built-in tools in `registry.go`.
- **`internal/scheduler/`** — Cron-like task scheduling and job execution with history.
- **`internal/eventbus/`** — In-process pub/sub decoupling components (task completion → notifications, session discovered → insight processing).
- **`internal/notification/`** — Event-driven notifications with SMTP email delivery and templates.
- **`internal/pricing/`** — Effective-dated model pricing catalog. Maintained from **Settings → Model Pricing** (#189) over `/api/pricing/catalog` and `/api/pricing/rates`; `service.PricingService` owns the rules, because the store returns untyped errors that would otherwise collapse into 500s. Adding and correcting a rate are deliberately **separate endpoints, not one upsert** — appending leaves history priced at what it was charged, correcting rewrites already-reported costs, so `AddRate` refuses to overwrite (returning the colliding row so the UI can offer to correct instead) and `CorrectRate` refuses to create. `effective_from` is normalized to second precision because that is what RFC3339 storage round-trips; without it a written row is not findable by the value that wrote it. Every mutation invalidates the session cache so #188's stored costs re-price. Internals: `catalog.json` (embedded built-in seed, checked against provider pricing pages), `store.go` (SQLite `model_pricing` CRUD + FNV-1a revision hash; startup re-seed upserts on `(model_pattern, effective_from)` and never clobbers `user_modified` rows), `resolver.go` (exact > prefix matching, then newest `effective_from <= spent_at`; usage predating every row falls back to the earliest marked estimated). Cost is accumulated per assistant message at its own timestamp; consumers read the process-wide snapshot wired by `Cache.WithPricingStore`. The catalog is **not Anthropic-only** — Claude Code is pointed at compatible backends, so Moonshot Kimi, Z.ai GLM and Alibaba Qwen are seeded too. Anthropic's TTL multipliers (5m 1.25×, 1h 2×, read 0.1×) are only the *default* for the cache columns; each is overridable per rate because other providers publish their own cached-input price and do not split cache writes by TTL at all (Alibaba prices caching as a percentage rule — explicit creation 125% of input, hits 10% — and tiers by context length, which the catalog cannot express, so only the base tier is seeded). Two flags disambiguate a $0.00 row: `billable: false` is a deliberate zero (`<synthetic>`, embeddings) that resolves and so never reaches the unknown-pricing bucket, while an unmatched model does; `estimated: true` marks a best-effort rate, used for the bare family aliases (`opus`) that name no concrete model. Non-billable is the only way to seed all-zero rates — elsewhere a zero is an unfilled entry and fails at parse time, so a half-filled row breaks the build instead of quietly under-reporting. A model whose ID is not on a provider's published pricing page is deliberately left out: a missing row is visible in the unknown bucket, a guessed one is not.
- **`internal/logger/`** — Structured `slog`: rotating system log (lumberjack) + per-session logs at `<logDir>/sessions/<id>.log`; `otelslog` bridge forwards logs to OTel when enabled.
- **`internal/telemetry/`** — OTel providers (OTLP gRPC or Prometheus), config persisted to `<data_dir>/monitoring.json`, hot-reload via `Manager.Update()`, pre-built instruments (`agento.http.*`, `agento.agent.*`, `agento.chat.*`, `agento.storage.*`).
- **`internal/updater/`** — Release checker (cached 1h, feeds the UI update banner) and in-place installer behind `agento update`.
- **`internal/daemon/`** — `agento service` backend: installs and manages Agento as a user-level background service (launchd on macOS, systemd user units on Linux), with embedded unit/plist templates and a `commandRunner` seam for tests.
- **`internal/build/`** — Version variables injected via `-ldflags`.

**Timezones**: UTC end to end in storage and transport; **aggregation and display follow the browser** (#190). The analytics and insights endpoints take a `tz` IANA name (the frontend sends `Intl.DateTimeFormat().resolvedOptions().timeZone`), and `AnalyticsParams.Loc` is applied via `.In(loc)` before any bucket key is derived — a day, hour or weekday is meaningless until you say whose it is. A bare `YYYY-MM-DD` `from`/`to` is a **local** day boundary (`time.ParseInLocation`), not a UTC one. Missing or unrecognized `tz` falls back to UTC rather than erroring, so pre-`tz` callers are unchanged. `main.go` imports `_ "time/tzdata"` because a distroless container has no `/usr/share/zoneinfo` and every `LoadLocation` would silently degrade the dashboard to UTC. Daily bucket walks (`walkBuckets`) step the **calendar day**, not 24 hours — a local day is 23 or 25 hours across a DST transition, and a fixed step duplicates one key while skipping another. One deliberate UTC exception on the frontend: `formatRateDate` (`lib/pricing.ts`), because a pricing rate is keyed to a day rather than an instant.

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
