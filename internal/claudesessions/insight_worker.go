package claudesessions

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"sync"
	"time"

	"github.com/shaharia-lab/agento/internal/eventbus"
)

const (
	// insightWorkerRescanInterval controls how often the worker checks for
	// sessions that need (re-)processing due to a processor version bump.
	insightWorkerRescanInterval = 5 * time.Minute

	// maxConcurrentInsightWorkers limits the number of simultaneously running
	// processor pipeline calls so that insight processing cannot saturate all
	// available file-descriptor and CPU resources.
	maxConcurrentInsightWorkers = 4
)

// InsightWorker is a background goroutine that subscribes to session lifecycle
// events on the event bus and runs the processor pipeline for any new or changed
// session. It also performs a periodic sweep to catch sessions that were scanned
// before a processor version bump.
//
// The event-bus subscriber returns immediately (non-blocking) by dispatching
// each processOne call into its own goroutine. A shared semaphore limits
// concurrency, and a sync.Map deduplicates in-flight sessions so the same
// session is never processed twice simultaneously.
type InsightWorker struct {
	store    InsightStorer
	registry *ProcessorRegistry
	bus      eventbus.EventBus
	logger   *slog.Logger

	sem      chan struct{} // bounded concurrency — capacity maxConcurrentInsightWorkers
	inFlight sync.Map      // set of sessionIDs currently being processed (value: struct{})
}

// NewInsightWorker creates an InsightWorker. Call Start to begin processing.
func NewInsightWorker(
	store InsightStorer,
	registry *ProcessorRegistry,
	bus eventbus.EventBus,
	logger *slog.Logger,
) *InsightWorker {
	return &InsightWorker{
		store:    store,
		registry: registry,
		bus:      bus,
		logger:   logger,
		sem:      make(chan struct{}, maxConcurrentInsightWorkers),
	}
}

// Start registers the worker as an event bus listener and launches the
// background re-scan goroutine. It returns immediately; cancel ctx to stop.
//
// The event-bus subscriber returns immediately by dispatching each session into
// a goroutine. This avoids blocking the shared event bus worker pool on file I/O.
func (w *InsightWorker) Start(ctx context.Context) {
	w.bus.Subscribe(func(ev eventbus.Event) {
		if ev.Type != eventbus.EventSessionDiscovered && ev.Type != eventbus.EventSessionUpdated {
			return
		}
		sessionID := ev.Payload[eventbus.PayloadKeySessionID]
		filePath := ev.Payload[eventbus.PayloadKeyFilePath]
		if sessionID == "" || filePath == "" {
			return
		}
		// Dispatch immediately so the event bus worker is not blocked.
		go w.tryProcess(ctx, sessionID, filePath)
	})

	go w.rescanLoop(ctx)
}

// tryProcess acquires a semaphore slot and deduplicates in-flight sessions before
// calling processOne. If the session is already being processed, or if the
// semaphore is full and the context is canceled, the call is a no-op.
func (w *InsightWorker) tryProcess(ctx context.Context, sessionID, filePath string) {
	// Deduplicate: if the session is already in-flight, skip.
	if _, loaded := w.inFlight.LoadOrStore(sessionID, struct{}{}); loaded {
		return
	}
	defer w.inFlight.Delete(sessionID)

	// Acquire a semaphore slot (blocks until one is available or ctx is done).
	select {
	case w.sem <- struct{}{}:
	case <-ctx.Done():
		return
	}
	defer func() { <-w.sem }()

	w.processOne(ctx, sessionID, filePath)
}

// processOne runs the processor pipeline for a single session and upserts the result.
func (w *InsightWorker) processOne(ctx context.Context, sessionID, filePath string) {
	insight, err := w.registry.RunSession(sessionID, filePath)
	if err != nil {
		w.logger.Warn("insight_worker: failed to process session",
			"session_id", sessionID, "error", err)
		return
	}
	if err := w.store.Upsert(ctx, insight); err != nil {
		w.logger.Warn("insight_worker: failed to upsert insight",
			"session_id", sessionID, "error", err)
	}
}

// rescanLoop runs at startup and then every insightWorkerRescanInterval to
// re-process sessions whose insight row has an outdated processor_version.
func (w *InsightWorker) rescanLoop(ctx context.Context) {
	// Run immediately on startup.
	w.rescanOutdated(ctx)

	ticker := time.NewTicker(insightWorkerRescanInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			w.rescanOutdated(ctx)
		}
	}
}

// rescanOutdated finds all sessions whose insight is missing or outdated and
// re-processes them concurrently via tryProcess.
func (w *InsightWorker) rescanOutdated(ctx context.Context) {
	sessionIDs, err := w.store.NeedsProcessing(ctx, CurrentProcessorVersion)
	if err != nil {
		w.logger.Warn("insight_worker: failed to list sessions needing processing", "error", err)
		return
	}
	if len(sessionIDs) == 0 {
		return
	}
	w.logger.Info("insight_worker: re-scanning outdated sessions", "count", len(sessionIDs))

	for _, sessionID := range sessionIDs {
		if ctx.Err() != nil {
			return
		}
		filePath, findErr := findSessionFilePath(sessionID)
		if findErr != nil || filePath == "" {
			continue
		}
		// Dispatch each session concurrently so the rescan loop is not serialized
		// behind the semaphore. tryProcess handles deduplication with in-flight sessions
		// triggered by the event bus.
		go w.tryProcess(ctx, sessionID, filePath)
	}
}

// findSessionFilePath locates the JSONL file for the given session ID by
// searching ~/.claude/projects/. Returns an empty string if not found.
func findSessionFilePath(sessionID string) (string, error) {
	projectsDir := filepath.Join(ClaudeHome(), "projects")
	entries, err := os.ReadDir(projectsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil
		}
		return "", err
	}
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		candidate := filepath.Join(projectsDir, e.Name(), sessionID+jsonlExt)
		if _, statErr := os.Stat(candidate); statErr == nil {
			return candidate, nil
		}
	}
	return "", nil
}
