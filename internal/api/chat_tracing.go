package api

import (
	"context"
	"encoding/json"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/trace"
)

// messageTypeUser is the event type emitted when a tool completes.
// Not defined as a constant in the SDK (it's an internal protocol detail),
// so we declare it locally.
const messageTypeUser claude.MessageType = "user"

// toolSpanEntry tracks an in-flight tool_use span keyed by tool_use_id.
type toolSpanEntry struct {
	span trace.Span
}

// openToolSpans starts child spans for every tool_use block inside an
// assistant event. Existing entries are not re-opened (idempotent).
func openToolSpans(
	execSpan trace.Span, raw json.RawMessage,
	toolSpans map[string]toolSpanEntry,
) {
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
		// Make execSpan the parent by injecting it into the context.
		parentCtx := trace.ContextWithSpan(context.Background(), execSpan)
		_, span := otel.Tracer("agento").Start(parentCtx, "tool_use."+blk.Name)
		span.SetAttributes(
			attribute.String("tool.id", blk.ID),
			attribute.String("tool.name", blk.Name),
			attribute.String("tool.input", truncateAttr(string(blk.Input), 512)),
		)
		toolSpans[blk.ID] = toolSpanEntry{span: span}
	}
}

// closeToolSpans ends spans for completed tool_result items inside a "user"
// event. Non-user events are ignored cheaply via the type discriminant.
func closeToolSpans(raw json.RawMessage, toolSpans map[string]toolSpanEntry) {
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
		entry.span.SetAttributes(
			attribute.String("tool.result", truncateAttr(string(c.Content), 512)),
		)
		entry.span.End()
		delete(toolSpans, c.ToolUseID)
	}
}

// enrichExecSpanFromResult adds final result metadata (turns, duration, cost,
// tokens, per-model usage) to the parent execution span.
func enrichExecSpanFromResult(execSpan trace.Span, result *claude.Result) {
	if result == nil {
		return
	}
	execSpan.SetAttributes(
		attribute.Int("agent.num_turns", result.NumTurns),
		attribute.Int64("agent.duration_ms", result.DurationMS),
		attribute.Int64("agent.duration_api_ms", result.DurationAPIMS),
		attribute.Float64("agent.cost_usd", result.TotalCostUSD),
		attribute.Int("agent.input_tokens", result.Usage.InputTokens),
		attribute.Int("agent.output_tokens", result.Usage.OutputTokens),
		attribute.Int("agent.cache_read_tokens", result.Usage.CacheReadInputTokens),
		attribute.Int("agent.cache_creation_tokens", result.Usage.CacheCreationInputTokens),
		attribute.Int("agent.web_searches", result.Usage.WebSearchRequests),
		attribute.Int("agent.permission_denials", len(result.PermissionDenials)),
		attribute.Bool("agent.is_error", result.IsError),
	)

	// Per-model cost/token breakdown as individual span events so they show
	// up in Grafana without cluttering top-level attributes.
	for modelID, mu := range result.ModelUsages {
		execSpan.AddEvent("agent.model_usage",
			trace.WithAttributes(
				attribute.String("model.id", modelID),
				attribute.Int("model.input_tokens", mu.InputTokens),
				attribute.Int("model.output_tokens", mu.OutputTokens),
				attribute.Int("model.cache_read_tokens", mu.CacheReadInputTokens),
				attribute.Int("model.cache_creation_tokens", mu.CacheCreationInputTokens),
				attribute.Float64("model.cost_usd", mu.CostUSD),
			),
		)
	}
}

// addSystemInitEvent annotates the execution span with session initialisation
// metadata from the first "system / init" event.
func addSystemInitEvent(execSpan trace.Span, sys *claude.SystemMessage) {
	if sys == nil || sys.Subtype != claude.SubtypeInit {
		return
	}
	execSpan.AddEvent("agent.session_init",
		trace.WithAttributes(
			attribute.String("agent.model", sys.Model),
			attribute.String("agent.session_id", sys.SessionID),
			attribute.String("agent.claude_version", sys.ClaudeCodeVersion),
			attribute.Int("agent.tool_count", len(sys.Tools)),
			attribute.String("agent.permission_mode", sys.PermissionMode),
		),
	)
}

// recordToolProgress adds a progress span event to the matching in-flight tool
// span. Silently skips if the tool_use_id is not being tracked.
func recordToolProgress(
	tp *claude.ToolProgressMessage, toolSpans map[string]toolSpanEntry,
) {
	if tp == nil || tp.ToolUseID == "" {
		return
	}
	entry, ok := toolSpans[tp.ToolUseID]
	if !ok {
		return
	}
	entry.span.AddEvent("tool.progress",
		trace.WithAttributes(
			attribute.String("tool.message", tp.Message),
			attribute.Float64("tool.progress_pct", tp.Progress),
		),
	)
}

// flushToolSpans ends all remaining in-flight tool spans.
// Called when the event loop exits (e.g. cancellation or error) so spans are
// never left open indefinitely.
func flushToolSpans(toolSpans map[string]toolSpanEntry) {
	for id, entry := range toolSpans {
		entry.span.End()
		delete(toolSpans, id)
	}
}

// truncateAttr truncates a string to at most max bytes for use as a span
// attribute value, appending "…" when truncated.
func truncateAttr(s string, max int) string {
	if len(s) <= max {
		return s
	}
	return s[:max] + "…"
}
