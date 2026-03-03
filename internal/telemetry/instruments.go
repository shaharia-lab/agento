package telemetry

import (
	"sync/atomic"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/metric"
)

const meterName = "agento"

// globalInstruments holds the singleton metric instruments created at startup.
// atomic.Pointer gives safe concurrent access between init and hot-path readers.
var globalInstruments atomic.Pointer[Instruments] //nolint:gochecknoglobals

// setGlobalInstruments stores instr as the process-wide instruments.
// Called once from Init after the global MeterProvider is registered.
func setGlobalInstruments(instr *Instruments) { globalInstruments.Store(instr) }

// GetGlobalInstruments returns the instruments created at startup.
// Returns nil if Init has not been called yet.
func GetGlobalInstruments() *Instruments { return globalInstruments.Load() }

// Instruments holds all OTel metric instruments for Agento.
type Instruments struct {
	HTTPRequestsTotal   metric.Int64Counter
	HTTPRequestDuration metric.Float64Histogram

	AgentRunsTotal    metric.Int64Counter
	AgentRunDuration  metric.Float64Histogram
	AgentInputTokens  metric.Int64Counter
	AgentOutputTokens metric.Int64Counter

	ChatSessionsCreated metric.Int64Counter
	ChatSessionsDeleted metric.Int64Counter

	StorageOpsTotal   metric.Int64Counter
	StorageOpDuration metric.Float64Histogram
}

// NewInstruments creates and registers all metric instruments using the global MeterProvider.
func NewInstruments() (*Instruments, error) {
	m := otel.GetMeterProvider().Meter(meterName)

	httpInstr, err := newHTTPInstruments(m)
	if err != nil {
		return nil, err
	}

	agentInstr, err := newAgentInstruments(m)
	if err != nil {
		return nil, err
	}

	chatInstr, err := newChatInstruments(m)
	if err != nil {
		return nil, err
	}

	storageInstr, err := newStorageInstruments(m)
	if err != nil {
		return nil, err
	}

	return &Instruments{
		HTTPRequestsTotal:   httpInstr.requestsTotal,
		HTTPRequestDuration: httpInstr.requestDuration,
		AgentRunsTotal:      agentInstr.runsTotal,
		AgentRunDuration:    agentInstr.runDuration,
		AgentInputTokens:    agentInstr.inputTokens,
		AgentOutputTokens:   agentInstr.outputTokens,
		ChatSessionsCreated: chatInstr.created,
		ChatSessionsDeleted: chatInstr.deleted,
		StorageOpsTotal:     storageInstr.opsTotal,
		StorageOpDuration:   storageInstr.opDuration,
	}, nil
}

type httpInstruments struct {
	requestsTotal   metric.Int64Counter
	requestDuration metric.Float64Histogram
}

func newHTTPInstruments(m metric.Meter) (httpInstruments, error) {
	reqTotal, err := m.Int64Counter("agento.http.requests.total",
		metric.WithDescription("Total number of HTTP requests"))
	if err != nil {
		return httpInstruments{}, err
	}

	reqDur, err := m.Float64Histogram("agento.http.request.duration",
		metric.WithDescription("HTTP request duration in seconds"),
		metric.WithUnit("s"))
	if err != nil {
		return httpInstruments{}, err
	}

	return httpInstruments{requestsTotal: reqTotal, requestDuration: reqDur}, nil
}

type agentInstruments struct {
	runsTotal    metric.Int64Counter
	runDuration  metric.Float64Histogram
	inputTokens  metric.Int64Counter
	outputTokens metric.Int64Counter
}

func newAgentInstruments(m metric.Meter) (agentInstruments, error) {
	runsTotal, err := m.Int64Counter("agento.agent.runs.total",
		metric.WithDescription("Total number of agent runs"))
	if err != nil {
		return agentInstruments{}, err
	}

	runDur, err := m.Float64Histogram("agento.agent.run.duration",
		metric.WithDescription("Agent run duration in seconds"),
		metric.WithUnit("s"))
	if err != nil {
		return agentInstruments{}, err
	}

	inputTok, err := m.Int64Counter("agento.agent.input_tokens.total",
		metric.WithDescription("Total input tokens consumed by agent runs"))
	if err != nil {
		return agentInstruments{}, err
	}

	outputTok, err := m.Int64Counter("agento.agent.output_tokens.total",
		metric.WithDescription("Total output tokens produced by agent runs"))
	if err != nil {
		return agentInstruments{}, err
	}

	return agentInstruments{
		runsTotal:    runsTotal,
		runDuration:  runDur,
		inputTokens:  inputTok,
		outputTokens: outputTok,
	}, nil
}

type chatInstruments struct {
	created metric.Int64Counter
	deleted metric.Int64Counter
}

func newChatInstruments(m metric.Meter) (chatInstruments, error) {
	created, err := m.Int64Counter("agento.chat.sessions.created.total",
		metric.WithDescription("Total number of chat sessions created"))
	if err != nil {
		return chatInstruments{}, err
	}

	deleted, err := m.Int64Counter("agento.chat.sessions.deleted.total",
		metric.WithDescription("Total number of chat sessions deleted"))
	if err != nil {
		return chatInstruments{}, err
	}

	return chatInstruments{created: created, deleted: deleted}, nil
}

type storageInstruments struct {
	opsTotal   metric.Int64Counter
	opDuration metric.Float64Histogram
}

func newStorageInstruments(m metric.Meter) (storageInstruments, error) {
	opsTotal, err := m.Int64Counter("agento.storage.operations.total",
		metric.WithDescription("Total number of storage operations"))
	if err != nil {
		return storageInstruments{}, err
	}

	opDur, err := m.Float64Histogram("agento.storage.operation.duration",
		metric.WithDescription("Storage operation duration in seconds"),
		metric.WithUnit("s"))
	if err != nil {
		return storageInstruments{}, err
	}

	return storageInstruments{opsTotal: opsTotal, opDuration: opDur}, nil
}
