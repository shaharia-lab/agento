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
		func() SessionProcessor {
			p := &AttributionProcessor{}
			p.Reset()
			return p
		},
		func() SessionProcessor { return &TimeProfileProcessor{} },
		func() SessionProcessor { return &TokenProfileProcessor{} },
		func() SessionProcessor { return &ErrorRateProcessor{} },
		func() SessionProcessor { return &ConversationDepthProcessor{} },
		func() SessionProcessor { return &SessionRhythmProcessor{} },
	)
}

// SessionRef identifies **which transcript** an insight belongs to.
//
// It is a struct rather than two string parameters on purpose. `session_insights`
// is keyed on `(session_id, project_path)` since #362, so both halves have to
// reach the processors — and adding a second string beside the first, in front
// of a variadic, is a signature every existing call still compiles against
// while meaning something else entirely. That is not hypothetical: it is what
// happened on the first attempt at this change, and the tests failed at runtime
// with "no session files given" rather than at the call site. A named type
// makes the same mistake a compile error.
type SessionRef struct {
	SessionID   string
	ProjectPath string
}

// RunSession opens filePath, feeds every event to all registered processors in
// sequence, then finalizes them and returns a populated SessionInsight.
// It is safe to call from multiple goroutines concurrently; each call creates
// independent processor instances so no locking is required.
func (r *ProcessorRegistry) RunSession(ref SessionRef, filePath string) (*SessionInsight, error) {
	return r.RunSessionFiles(ref, filePath)
}

// RunSessionFiles is RunSession over several transcripts that belong to one
// session: the parent JSONL followed by each of its sub-agent transcripts.
// All files feed a single set of processors, so the resulting insight covers
// delegated work additively — tool calls, cost and error counts include what
// sub-agents did. The parent must come first: turn-scoped processors derive
// their structure from it, and every sub-agent event is flagged isSidechain,
// which those processors deliberately do not treat as a new turn.
//
// A file that cannot be read is logged and skipped rather than failing the
// whole session; only a failure on the first (parent) file is fatal.
func (r *ProcessorRegistry) RunSessionFiles(
	ref SessionRef, filePaths ...string,
) (*SessionInsight, error) {
	if len(filePaths) == 0 {
		return nil, fmt.Errorf("no session files given for %q", ref.SessionID)
	}
	processors := r.newProcessors()
	if err := r.feedProcessors(filePaths[0], processors); err != nil {
		return nil, err
	}
	for _, fp := range filePaths[1:] {
		if err := r.feedProcessors(fp, processors); err != nil && r.logger != nil {
			r.logger.Warn("skipping unreadable sub-agent transcript", "file", fp, "error", err)
		}
	}
	insight := &SessionInsight{
		SessionID:        ref.SessionID,
		ProjectPath:      ref.ProjectPath,
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
