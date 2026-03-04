package agent

import (
	"context"
	"encoding/json"
	"unicode/utf8"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/trace"
)

// MessageTypeUser is the "user" event emitted by the CLI when a tool result
// is returned. It is not a named constant in the SDK.
const MessageTypeUser claude.MessageType = "user"

// ToolSpanEntry tracks an in-flight tool_use span keyed by tool_use_id.
type ToolSpanEntry struct {
	Span trace.Span
}

// OpenToolSpans starts a child span for every tool_use block found in an
// assistant event. Existing entries are skipped (idempotent).
// ctx is used as the parent context so W3C baggage and sampling decisions
// are preserved; runSpan is injected as the parent span.
func OpenToolSpans(ctx context.Context, runSpan trace.Span, raw json.RawMessage, toolSpans map[string]ToolSpanEntry) {
	var msg struct {
		Message struct {
			Content []struct {
				Type  string          `json:"type"`
				ID    string          `json:"id,omitempty"`
				Name  string          `json:"name,omitempty"`
				Input json.RawMessage `json:"input,omitempty"`
			} `json:"content"`
		} `json:"message"`
	}
	if json.Unmarshal(raw, &msg) != nil {
		return
	}
	for _, blk := range msg.Message.Content {
		if blk.Type != "tool_use" || blk.ID == "" {
			continue
		}
		if _, exists := toolSpans[blk.ID]; exists {
			continue
		}
		parentCtx := trace.ContextWithSpan(ctx, runSpan)
		_, span := otel.Tracer("agento").Start(parentCtx, "tool_use."+blk.Name)
		span.SetAttributes(
			attribute.String("tool.id", blk.ID),
			attribute.String("tool.name", blk.Name),
			attribute.String("tool.input", TruncateAttr(string(blk.Input), 512)),
		)
		toolSpans[blk.ID] = ToolSpanEntry{Span: span}
	}
}

// CloseToolSpans ends spans for completed tool_result items in a "user" event.
func CloseToolSpans(raw json.RawMessage, toolSpans map[string]ToolSpanEntry) {
	var msg struct {
		Type    string `json:"type"`
		Message struct {
			Content []struct {
				Type      string          `json:"type"`
				ToolUseID string          `json:"tool_use_id,omitempty"`
				Content   json.RawMessage `json:"content,omitempty"`
			} `json:"content"`
		} `json:"message"`
	}
	if json.Unmarshal(raw, &msg) != nil || msg.Type != "user" {
		return
	}
	for _, c := range msg.Message.Content {
		if c.Type != "tool_result" || c.ToolUseID == "" {
			continue
		}
		entry, ok := toolSpans[c.ToolUseID]
		if !ok {
			continue
		}
		entry.Span.SetAttributes(
			attribute.String("tool.result", TruncateAttr(string(c.Content), 512)),
		)
		entry.Span.End()
		delete(toolSpans, c.ToolUseID)
	}
}

// FlushToolSpans ends all in-flight tool spans. Called when the event loop
// exits to prevent spans from being left open on cancellation or error.
func FlushToolSpans(toolSpans map[string]ToolSpanEntry) {
	for id, entry := range toolSpans {
		entry.Span.End()
		delete(toolSpans, id)
	}
}

// RecordToolProgress adds a progress event to the matching in-flight tool span.
func RecordToolProgress(tp *claude.ToolProgressMessage, toolSpans map[string]ToolSpanEntry) {
	if tp == nil || tp.ToolUseID == "" {
		return
	}
	entry, ok := toolSpans[tp.ToolUseID]
	if !ok {
		return
	}
	entry.Span.AddEvent("tool.progress",
		trace.WithAttributes(
			attribute.String("tool.message", tp.Message),
			attribute.Float64("tool.progress_pct", tp.Progress),
		),
	)
}

// AddSystemInitEvent annotates the run span with session init metadata.
func AddSystemInitEvent(runSpan trace.Span, sys *claude.SystemMessage) {
	if sys == nil || sys.Subtype != claude.SubtypeInit {
		return
	}
	runSpan.AddEvent("agent.session_init",
		trace.WithAttributes(
			attribute.String("agent.model", sys.Model),
			attribute.String("agent.session_id", sys.SessionID),
			attribute.String("agent.claude_version", sys.ClaudeCodeVersion),
			attribute.Int("agent.tool_count", len(sys.Tools)),
			attribute.String("agent.permission_mode", sys.PermissionMode),
		),
	)
}

// rawResultExtras holds result fields the SDK struct cannot parse because
// the CLI emits them in camelCase or in a nested structure.
type rawResultExtras struct {
	ModelUsage map[string]struct {
		InputTokens              int     `json:"inputTokens"`
		OutputTokens             int     `json:"outputTokens"`
		CacheReadInputTokens     int     `json:"cacheReadInputTokens"`
		CacheCreationInputTokens int     `json:"cacheCreationInputTokens"`
		WebSearchRequests        int     `json:"webSearchRequests"`
		CostUSD                  float64 `json:"costUSD"`
	} `json:"modelUsage"`
	Usage struct {
		ServerToolUse struct {
			WebSearchRequests int `json:"web_search_requests"`
		} `json:"server_tool_use"`
	} `json:"usage"`
}

// EnrichSpanFromResult adds final result metadata to a span.
// raw is needed to recover camelCase fields (modelUsage, web_search_requests)
// that the SDK's Result struct cannot parse due to JSON key mismatches.
func EnrichSpanFromResult(span trace.Span, result *claude.Result, raw json.RawMessage) {
	if result == nil {
		return
	}

	var extras rawResultExtras
	if err := json.Unmarshal(raw, &extras); err != nil {
		extras = rawResultExtras{}
	}

	span.SetAttributes(
		attribute.Int("agent.num_turns", result.NumTurns),
		attribute.Int64("agent.duration_ms", result.DurationMS),
		attribute.Int64("agent.duration_api_ms", result.DurationAPIMS),
		attribute.Float64("agent.cost_usd", result.TotalCostUSD),
		attribute.Int("agent.input_tokens", result.Usage.InputTokens),
		attribute.Int("agent.output_tokens", result.Usage.OutputTokens),
		attribute.Int("agent.cache_read_tokens", result.Usage.CacheReadInputTokens),
		attribute.Int("agent.cache_creation_tokens", result.Usage.CacheCreationInputTokens),
		attribute.Int("agent.web_searches", extras.Usage.ServerToolUse.WebSearchRequests),
		attribute.Int("agent.permission_denials", len(result.PermissionDenials)),
		attribute.Bool("agent.is_error", result.IsError),
	)

	for modelID, mu := range extras.ModelUsage {
		span.AddEvent("agent.model_usage",
			trace.WithAttributes(
				attribute.String("model.id", modelID),
				attribute.Int("model.input_tokens", mu.InputTokens),
				attribute.Int("model.output_tokens", mu.OutputTokens),
				attribute.Int("model.cache_read_tokens", mu.CacheReadInputTokens),
				attribute.Int("model.cache_creation_tokens", mu.CacheCreationInputTokens),
				attribute.Int("model.web_searches", mu.WebSearchRequests),
				attribute.Float64("model.cost_usd", mu.CostUSD),
			),
		)
	}
}

// TruncateAttr truncates s to at most max bytes for use as a span attribute
// value, appending "…" when truncated. It walks back to a valid UTF-8 rune
// boundary so multi-byte characters are never split.
func TruncateAttr(s string, max int) string {
	if len(s) <= max {
		return s
	}
	// Walk back from max until we land on a valid rune-start byte.
	for max > 0 && !utf8.RuneStart(s[max]) {
		max--
	}
	return s[:max] + "…"
}
