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
make test                                        # all Go tests
go test ./internal/service/... -run TestChatService   # a single Go test

cd frontend && npm run test                      # Vitest, run once
cd frontend && npm run test:watch                # the watcher

make e2e-setup                                   # first time: Playwright + Chromium
make e2e                                         # Playwright against the built binary
```

Two suites need a word of explanation:

- **`make bench-scale`** runs the scale harness against a generated `~/.claude`
  corpus and asserts the scan, sessions-list and analytics budgets against it.
  `SCALE=medium` (default) is ~800 sessions; `SCALE=large` is 5,000 sessions
  across 500 projects and writes about a gigabyte. It sits behind the `scale`
  build tag, so `make test` never runs it.
- **The Claude Sessions e2e specs read the machine's real `~/.claude`** and skip
  when it is too small to exercise what they check. They exist because two
  behaviours cannot be verified through a CDP-driven Chrome tab, which reports
  `visibilityState: "hidden"`: the list's infinite-scroll sentinel
  (IntersectionObserver stops delivering) and the transcript's timeline jump
  (smooth scrolling does not animate).

Frontend tests run under jsdom with `src/test/setup.ts` loaded for every suite,
which registers jest-dom's matchers and an `afterEach(cleanup)` — a break there
breaks every file at once, not just the one you edited.

Regenerate mocks after changing an interface:

```bash
make generate    # mockery, reads .mockery.yaml
```

---

## Lint

```bash
make lint                          # golangci-lint over ./...
cd frontend && npm run lint        # ESLint
cd frontend && npm run typecheck   # tsc -b
cd frontend && npm run format      # Prettier
```

`npm run typecheck` uses `tsc -b` deliberately: the root `tsconfig.json` is a
solution file with `"files": []`, so a plain `tsc --noEmit` checks nothing and
exits 0.

Pre-commit hooks enforce all of the above on every commit.

---

## Project layout

```
agento/
├── cmd/              # Cobra commands (web, ask, update, service)
├── frontend/         # React + TypeScript UI
├── internal/
│   ├── agent/          # SDK integration, RunOptions, session execution
│   ├── api/            # HTTP handlers
│   ├── build/          # Build-time version variables
│   ├── claudesessions/ # Claude session scanner, analytics, processor pipeline, journey
│   ├── config/         # AppConfig, AgentConfig, MCP config, Claude config dirs, settings
│   ├── daemon/         # `agento service` — launchd / systemd user units
│   ├── eventbus/       # In-process event bus
│   ├── integrations/   # Integration registry + in-process MCP servers
│   │                   #   (Google, GitHub, Slack, Jira, Confluence, Telegram, WhatsApp)
│   ├── logger/         # Structured slog loggers (system + per-session), log rotation
│   ├── notification/   # Notification system (SMTP email)
│   ├── pricing/        # Effective-dated model pricing catalog and resolver
│   ├── scheduler/      # Task scheduler and job executor
│   ├── server/         # HTTP server wiring, router, API guards
│   ├── service/        # Business logic (AgentService, ChatService, TaskService, …)
│   ├── storage/        # SQLite persistence (~/.agento/agento.db) and migrations
│   ├── telemetry/      # OpenTelemetry traces, metrics, logs (config, providers, hot-reload)
│   ├── tools/          # Local MCP tool server
│   ├── trigger/        # Inbound-message dispatcher (Telegram triggers)
│   └── updater/        # Release check and in-place self-update
├── e2e/              # Playwright end-to-end tests
├── docs/             # Documentation
├── .goreleaser.yaml  # Release configuration
└── Makefile
```

**Import rule:** `config` ← `service` ← `api`, never the reverse.

---

## Conventions worth knowing before you change things

**Database migrations** are appended to the migrations slice in
`internal/storage/`; each one must also bump the hardcoded expected version in
`sqlite_test.go`. Migrations are forward-only.

**Cached-figure version constants.** Anything the scanner or the insight
pipeline *stores* per session is only recomputed when the data looks stale.
Changing how a stored figure is derived therefore means bumping the matching
constant — `CurrentScannerVersion` for scanner output, `CurrentProcessorVersion`
for insight output — to force a re-read. Some predicates are shared by both
sides (turn segmentation is the notable one), and a change there needs **both**
bumped, or the two halves drift apart.

User-caused staleness works the same way but cannot be a constant: the pricing
catalog revision and the idle threshold are recorded alongside the cache, and a
drift in either invalidates it.

**Per-session metrics are defined twice, deliberately** — in SQL
(`internal/claudesessions/session_query.go`, which the list filters and sorts on)
and in TypeScript (`frontend/src/lib/sessionMetrics.ts`, which renders the row).
They must agree, or a row showing $36.30 gets hidden by "cost at most $40".
`internal/claudesessions/testdata/session_metric_vectors.json` is read by both
languages' tests so a change to one definition fails the other's.

**API types are mirrored** in `frontend/src/types.ts`; keep them in step when
changing a response shape.

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

---

## Timezones

**Storage and transport are UTC, end to end.** Session timestamps are parsed
from the JSONL `Z` values, cached as UTC, and serialized as UTC on every API
response. Nothing below the presentation layer knows about the user's zone.

**Aggregation and display follow the browser.** A day, an hour and a weekday
only mean something once you say whose they are — so the analytics and insights
endpoints take a `tz` query parameter (an IANA name such as `Europe/Berlin`),
which the frontend fills in from
`Intl.DateTimeFormat().resolvedOptions().timeZone`. The backend resolves it and
applies `.In(loc)` before deriving any bucket key, and interprets a bare
`YYYY-MM-DD` `from`/`to` as a local day boundary rather than a UTC one.

A missing or unrecognized `tz` falls back to UTC rather than erroring: analytics
is a read-only dashboard, and refusing to render it over a bad zone string is
worse than rendering what every caller got before the parameter existed.

Two consequences worth knowing:

- **The zone database is embedded** (`_ "time/tzdata"` in `main.go`). Without
  it `time.LoadLocation` depends on the host's `/usr/share/zoneinfo`, which a
  distroless or scratch container does not have — every lookup would fail and
  silently fall the whole dashboard back to UTC. Costs about 450KB.
- **Daily bucket walks step the calendar day, not 24 hours.** A local day is 23
  or 25 hours long across a DST transition, so a fixed step drifts off the wall
  clock and duplicates one day key while skipping another. The heatmap grid
  stays a fixed 24 columns: a spring-forward day has one empty cell and a
  fall-back day has one doubled, which is the honest answer for a "when do I
  work?" chart.

Non-analytics timestamps already render locally through `toLocale*` helpers and
need nothing special. One deliberate exception: `formatRateDate`
(`frontend/src/lib/pricing.ts`) pins UTC, because a pricing rate is keyed to a
day rather than an instant — see `docs/pricing.md`.
