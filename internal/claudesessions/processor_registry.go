package claudesessions

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"sync"
	"time"
)

// ProcessorRegistry manages a set of SessionProcessors and can run them all
// against a session JSONL file in a single sequential pass.
//
// Processors are finalized in registration order, which means processors that
// depend on earlier results (e.g. AutonomyScoreProcessor depends on
// TurnCountProcessor) must be registered after their dependencies.
type ProcessorRegistry struct {
	mu         sync.Mutex
	processors []SessionProcessor
	logger     *slog.Logger
}

// NewProcessorRegistry returns a registry using the given processors in order.
func NewProcessorRegistry(logger *slog.Logger, processors ...SessionProcessor) *ProcessorRegistry {
	return &ProcessorRegistry{
		processors: processors,
		logger:     logger,
	}
}

// DefaultProcessorRegistry constructs a ProcessorRegistry with the full set of
// built-in processors in the correct dependency order.
func DefaultProcessorRegistry(logger *slog.Logger) *ProcessorRegistry {
	return NewProcessorRegistry(logger,
		&TurnCountProcessor{},
		&AutonomyScoreProcessor{},
		&ToolUsageProcessor{toolBreakdown: make(map[string]int)},
		&TimeProfileProcessor{},
		&TokenProfileProcessor{},
		&ErrorRateProcessor{},
		&ConversationDepthProcessor{},
		&SessionRhythmProcessor{},
	)
}

// RunSession opens filePath, feeds every event to all registered processors in
// sequence, then finalizes them and returns a populated SessionInsight.
// It is safe to call from multiple goroutines; a mutex serializes each run.
func (r *ProcessorRegistry) RunSession(sessionID, filePath string) (*SessionInsight, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return nil, fmt.Errorf("opening session file %q: %w", filePath, err)
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			r.logger.Warn("failed to close session file", "file", filePath, "error", cerr)
		}
	}()

	for _, p := range r.processors {
		p.Reset()
	}

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	for sc.Scan() {
		raw := sc.Bytes()
		ev, parseErr := decodeProcessableEvent(raw)
		if parseErr != nil {
			continue
		}
		if ev.Type == "file-history-snapshot" {
			continue
		}
		for _, p := range r.processors {
			p.Process(ev)
		}
	}
	if scanErr := sc.Err(); scanErr != nil {
		return nil, fmt.Errorf("reading session file %q: %w", filePath, scanErr)
	}

	insight := &SessionInsight{
		SessionID:        sessionID,
		ProcessorVersion: CurrentProcessorVersion,
		ScannedAt:        time.Now().UTC(),
		ToolBreakdown:    make(map[string]int),
	}
	for _, p := range r.processors {
		p.Finalize(insight)
	}
	return insight, nil
}

// decodeProcessableEvent unmarshals a raw JSONL line into a ProcessableEvent.
// The Raw field is set to a copy of the original bytes.
func decodeProcessableEvent(raw []byte) (ProcessableEvent, error) {
	var ev ProcessableEvent
	if err := json.Unmarshal(raw, &ev); err != nil {
		return ProcessableEvent{}, err
	}
	ev.Raw = make(json.RawMessage, len(raw))
	copy(ev.Raw, raw)
	return ev, nil
}
