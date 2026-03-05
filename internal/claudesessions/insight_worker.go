package claudesessions

import (
	"context"
	"log/slog"
	"sync"
	"time"

	"github.com/shaharia-lab/agento/internal/eventbus"
)

const (
	// insightWorkerRescanInterval controls how often the worker checks for
	// sessions that need (re-)processing due to a processor version bump.
	insightWorkerRescanInterval = 5 * time.Minute

	// insightWorkerPoolSize is the fixed number of goroutines in the worker
	// pool. This bounds both concurrency and goroutine count regardless of how
	// many sessions are discovered at once.
	insightWorkerPoolSize = 4

	// insightWorkerQueueSize is the capacity of the work channel. Items
	// submitted when the channel is full are dropped with a warning log.
	insightWorkerQueueSize = 100
)

// workItem is a single unit of work for the InsightWorker pool.
type workItem struct {
	sessionID string
	filePath  string
}

// InsightWorker subscribes to session lifecycle events and runs the processor
// pipeline for new or changed sessions. A fixed pool of worker goroutines
// reads from a bounded work channel so that discovering thousands of sessions
// at once does not fan out thousands of goroutines.
//
// Lifecycle:
//  1. Call Start(ctx) to launch the pool and subscribe to the event bus.
//  2. Cancel ctx to signal shutdown (stops event delivery and exits workers).
//  3. Call Wait() after ctx is canceled to drain in-flight work.
type InsightWorker struct {
	store    InsightStorer
	registry *ProcessorRegistry
	bus      eventbus.EventBus
	logger   *slog.Logger

	work     chan workItem // bounded queue feeding the worker pool
	inFlight sync.Map
	wg       sync.WaitGroup
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
		work:     make(chan workItem, insightWorkerQueueSize),
	}
}

// Start registers the event bus subscriber, launches the fixed worker pool,
// and starts the background re-scan goroutine. It returns immediately.
// Cancel the context to initiate shutdown, then call Wait to drain workers.
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
		// Non-blocking enqueue: the subscriber must not block the bus worker pool.
		w.enqueue(sessionID, filePath)
	})

	for range insightWorkerPoolSize {
		w.wg.Add(1)
		go func() {
			defer w.wg.Done()
			w.runWorker(ctx)
		}()
	}

	w.wg.Add(1)
	go func() {
		defer w.wg.Done()
		w.rescanLoop(ctx)
	}()
}

// Wait blocks until all worker goroutines have exited.
// Call after canceling the context passed to Start.
func (w *InsightWorker) Wait() {
	w.wg.Wait()
}

// enqueue submits a session to the work channel in a non-blocking manner.
// If the channel is full the item is dropped and a warning is logged.
func (w *InsightWorker) enqueue(sessionID, filePath string) {
	select {
	case w.work <- workItem{sessionID: sessionID, filePath: filePath}:
	default:
		w.logger.Warn("insight_worker: work queue full, dropping session", "session_id", sessionID)
	}
}

// runWorker reads from the work channel until the context is canceled.
func (w *InsightWorker) runWorker(ctx context.Context) {
	for {
		select {
		case item := <-w.work:
			w.tryProcess(ctx, item.sessionID, item.filePath)
		case <-ctx.Done():
			return
		}
	}
}

// tryProcess deduplicates in-flight sessions before calling processOne.
// If the session is already being processed, the call is a no-op.
func (w *InsightWorker) tryProcess(ctx context.Context, sessionID, filePath string) {
	if _, loaded := w.inFlight.LoadOrStore(sessionID, struct{}{}); loaded {
		return
	}
	defer w.inFlight.Delete(sessionID)
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
// enqueues them for processing. The file path is retrieved from the cache DB,
// avoiding a filesystem walk.
func (w *InsightWorker) rescanOutdated(ctx context.Context) {
	sessions, err := w.store.NeedsProcessing(ctx, CurrentProcessorVersion)
	if err != nil {
		w.logger.Warn("insight_worker: failed to list sessions needing processing", "error", err)
		return
	}
	if len(sessions) == 0 {
		return
	}
	w.logger.Info("insight_worker: re-scanning outdated sessions", "count", len(sessions))

	for _, s := range sessions {
		if ctx.Err() != nil {
			return
		}
		if s.FilePath == "" {
			continue
		}
		w.enqueue(s.SessionID, s.FilePath)
	}
}
