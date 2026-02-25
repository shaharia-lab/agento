.PHONY: build build-frontend build-go dev-frontend dev-backend run-serve run-ask tidy lint test clean

BINARY := agento

# ── Production build ──────────────────────────────────────────────────────────
build: build-frontend build-go

build-frontend:
	cd frontend && npm install && npm run build

build-go:
	go build -o $(BINARY) .

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
	go vet ./...

test:
	go test ./...

# ── Clean ─────────────────────────────────────────────────────────────────────
clean:
	rm -f $(BINARY)
	rm -rf frontend/dist
	rm -rf frontend/node_modules
