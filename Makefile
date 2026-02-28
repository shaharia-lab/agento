.PHONY: build build-frontend build-go dev-frontend dev-backend run-serve run-ask tidy lint test clean

BINARY := agento

VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
COMMIT  ?= $(shell git rev-parse --short HEAD 2>/dev/null || echo "unknown")
DATE    ?= $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")
LDFLAGS := -s -w \
  -X github.com/shaharia-lab/agento/internal/build.Version=$(VERSION) \
  -X github.com/shaharia-lab/agento/internal/build.CommitSHA=$(COMMIT) \
  -X github.com/shaharia-lab/agento/internal/build.BuildDate=$(DATE)

# ── Production build ──────────────────────────────────────────────────────────
build: build-frontend build-go

build-frontend:
	cd frontend && npm install && npm run build

build-go:
	go build -ldflags "$(LDFLAGS)" -o $(BINARY) .

# ── Development (frontend + backend run separately) ────────────────────────────
dev-frontend:
	cd frontend && npm run dev

dev-backend:
	go run -tags dev .

# ── Legacy shortcuts ──────────────────────────────────────────────────────────
run-serve: build
	./$(BINARY) serve

run-ask: build
	./$(BINARY) ask $(ARGS)

# ── Code quality ──────────────────────────────────────────────────────────────
tidy:
	go mod tidy

lint:
	golangci-lint run ./...

test:
	go test ./...

# ── Clean ─────────────────────────────────────────────────────────────────────
clean:
	rm -f $(BINARY)
	rm -rf frontend/dist
	rm -rf frontend/node_modules
