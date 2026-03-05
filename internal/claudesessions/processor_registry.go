package claudesessions

import (
	"bufio"
	"encoding/json"
	"fmt"
	"log/slog"
	"os"
	"time"
)

// ProcessorFactory is a constructor function that creates a fresh SessionProcessor.
// Using factories rather than shared processor instances allows RunSession to be
// called concurrently without a global lock.
type ProcessorFactory func() SessionProcessor

// ProcessorRegistry manages a set of SessionProcessor factories and can run them
// all against a session JSONL file in a single sequential pass. Each RunSession
// call creates fresh processor instances, so concurrent calls are fully independent
// and no mutex is required.
//
// Factories are invoked in registration order, which means processors that depend
// on earlier results (e.g. AutonomyScoreProcessor depends on TurnCountProcessor)
// must be registered after their dependencies.
type ProcessorRegistry struct {
	factories []ProcessorFactory
	logger    *slog.Logger
}

// NewProcessorRegistry returns a registry using the given processor factories in order.
func NewProcessorRegistry(logger *slog.Logger, factories ...ProcessorFactory) *ProcessorRegistry {
	return &ProcessorRegistry{
		factories: factories,
		logger:    logger,
	}
}

// DefaultProcessorRegistry constructs a ProcessorRegistry with the full set of
// built-in processors in the correct dependency order.
func DefaultProcessorRegistry(logger *slog.Logger) *ProcessorRegistry {
	return NewProcessorRegistry(logger,
		func() SessionProcessor { return &TurnCountProcessor{} },
		func() SessionProcessor { return &AutonomyScoreProcessor{} },
		func() SessionProcessor { return &ToolUsageProcessor{toolBreakdown: make(map[string]int)} },
		func() SessionProcessor { return &TimeProfileProcessor{} },
		func() SessionProcessor { return &TokenProfileProcessor{} },
		func() SessionProcessor { return &ErrorRateProcessor{} },
		func() SessionProcessor { return &ConversationDepthProcessor{} },
		func() SessionProcessor { return &SessionRhythmProcessor{} },
	)
}

// RunSession opens filePath, feeds every event to all registered processors in
// sequence, then finalizes them and returns a populated SessionInsight.
// It is safe to call from multiple goroutines concurrently; each call creates
// independent processor instances so no locking is required.
func (r *ProcessorRegistry) RunSession(sessionID, filePath string) (*SessionInsight, error) {
	processors := r.newProcessors()
	if err := r.feedProcessors(filePath, processors); err != nil {
		return nil, err
	}
	insight := &SessionInsight{
		SessionID:        sessionID,
		ProcessorVersion: CurrentProcessorVersion,
		ScannedAt:        time.Now().UTC(),
		ToolBreakdown:    make(map[string]int),
	}
	for _, p := range processors {
		p.Finalize(insight)
	}
	return insight, nil
}

// newProcessors creates a fresh set of processor instances from the registered factories.
func (r *ProcessorRegistry) newProcessors() []SessionProcessor {
	processors := make([]SessionProcessor, len(r.factories))
	for i, f := range r.factories {
		processors[i] = f()
	}
	return processors
}

// feedProcessors opens filePath and feeds each decoded event to all processors.
func (r *ProcessorRegistry) feedProcessors(filePath string, processors []SessionProcessor) error {
	f, err := os.Open(filePath) //nolint:gosec
	if err != nil {
		return fmt.Errorf("opening session file %q: %w", filePath, err)
	}
	defer func() {
		if cerr := f.Close(); cerr != nil {
			if r.logger != nil {
				r.logger.Warn("failed to close session file", "file", filePath, "error", cerr)
			}
		}
	}()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	for sc.Scan() {
		ev, parseErr := decodeProcessableEvent(sc.Bytes())
		if parseErr != nil || ev.Type == "file-history-snapshot" {
			continue
		}
		for _, p := range processors {
			p.Process(ev)
		}
	}
	if scanErr := sc.Err(); scanErr != nil {
		return fmt.Errorf("reading session file %q: %w", filePath, scanErr)
	}
	return nil
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
