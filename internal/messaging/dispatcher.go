package messaging

import (
	"context"
	"encoding/json"
	"fmt"
	"log/slog"
	"strings"
	"unicode/utf8"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/service"
	"github.com/shaharia-lab/agento/internal/storage"
)

// Dispatcher receives inbound messages from any Platform, maps them to
// internal chat sessions, runs the agent, and sends the response back.
type Dispatcher struct {
	chatSvc      service.ChatService
	mappingStore storage.ConversationMappingStore
	platforms    *platformRegistry
	logger       *slog.Logger
}

// NewDispatcher creates a Dispatcher wired to the given services.
func NewDispatcher(
	chatSvc service.ChatService,
	mappingStore storage.ConversationMappingStore,
	logger *slog.Logger,
) *Dispatcher {
	return &Dispatcher{
		chatSvc:      chatSvc,
		mappingStore: mappingStore,
		platforms:    newPlatformRegistry(),
		logger:       logger,
	}
}

// RegisterPlatform makes a platform available for outbound messages.
func (d *Dispatcher) RegisterPlatform(p Platform) {
	d.platforms.put(p.ID(), p)
}

// UnregisterPlatform removes a platform from the registry.
func (d *Dispatcher) UnregisterPlatform(id string) {
	d.platforms.delete(id)
}

// HandleMessage implements MessageHandler. It is called by platform adapters
// when an inbound message arrives.
func (d *Dispatcher) HandleMessage(ctx context.Context, msg InboundMessage) error {
	d.logger.Info("inbound message received",
		"platform_type", msg.PlatformType,
		"platform_id", msg.PlatformID,
		"channel_id", msg.ChannelID,
		"user", msg.UserDisplayName,
	)

	if msg.Content == "" {
		d.logger.Debug("ignoring empty message", "channel_id", msg.ChannelID)
		return nil
	}

	// Look up or create the internal chat session for this conversation.
	mapping, err := d.getOrCreateMapping(ctx, msg)
	if err != nil {
		return fmt.Errorf("resolving conversation mapping: %w", err)
	}

	// Send typing indicator while we process.
	if platform, ok := d.platforms.get(msg.PlatformID); ok {
		if typErr := platform.SendTypingIndicator(ctx, msg.ChannelID); typErr != nil {
			d.logger.Debug("typing indicator failed", "error", typErr)
		}
	}

	// Run the agent and collect the response.
	response, err := d.runAgent(ctx, mapping.SessionID, msg.Content)
	if err != nil {
		d.logger.Error("agent run failed",
			"session_id", mapping.SessionID,
			"error", err,
		)
		response = "Sorry, I encountered an error processing your message."
	}

	// Send the response back to the platform.
	platform, ok := d.platforms.get(msg.PlatformID)
	if !ok {
		return fmt.Errorf("platform %q not registered", msg.PlatformID)
	}
	return platform.SendMessage(ctx, OutboundMessage{
		ChannelID:        msg.ChannelID,
		Content:          response,
		ReplyToMessageID: msg.MessageID,
	})
}

// getOrCreateMapping finds the existing mapping or creates a new chat session
// and mapping for the given inbound message.
func (d *Dispatcher) getOrCreateMapping(
	ctx context.Context, msg InboundMessage,
) (*storage.ConversationMapping, error) {
	existing, err := d.mappingStore.GetByPlatformChannel(
		msg.PlatformType, msg.PlatformID, msg.ChannelID,
	)
	if err != nil {
		return nil, err
	}
	if existing != nil {
		return existing, nil
	}

	// Determine agent slug from metadata if provided.
	agentSlug := ""
	if msg.Metadata != nil {
		agentSlug = msg.Metadata["agent_slug"]
	}

	// Create a new chat session for this conversation.
	title := fmt.Sprintf("%s — %s", msg.PlatformType, msg.UserDisplayName)
	if utf8.RuneCountInString(title) > 60 {
		title = string([]rune(title)[:57]) + "..."
	}

	session, err := d.chatSvc.CreateSession(ctx, agentSlug, "", "", "")
	if err != nil {
		return nil, fmt.Errorf("creating chat session: %w", err)
	}

	// Set a meaningful title.
	session.Title = title
	if updateErr := d.chatSvc.UpdateSession(ctx, session); updateErr != nil {
		d.logger.Warn("failed to set session title", "error", updateErr)
	}

	mapping := &storage.ConversationMapping{
		SessionID:    session.ID,
		PlatformType: msg.PlatformType,
		PlatformID:   msg.PlatformID,
		ChannelID:    msg.ChannelID,
		AgentSlug:    agentSlug,
	}
	if createErr := d.mappingStore.Create(mapping); createErr != nil {
		return nil, fmt.Errorf("creating conversation mapping: %w", createErr)
	}

	d.logger.Info("new conversation mapping created",
		"mapping_id", mapping.ID,
		"session_id", session.ID,
		"channel_id", msg.ChannelID,
	)

	return mapping, nil
}

// runAgent executes the agent for a given session and returns the text response.
// It uses bypass permissions since messaging platforms have no interactive approval UI.
func (d *Dispatcher) runAgent(ctx context.Context, sessionID, content string) (string, error) {
	opts := agent.RunOptions{
		// No PermissionHandler → bypass permissions (auto-approve all tools).
	}

	agentSession, _, err := d.chatSvc.BeginMessage(ctx, sessionID, content, opts)
	if err != nil {
		return "", fmt.Errorf("begin message: %w", err)
	}
	defer func() {
		if cerr := agentSession.Close(); cerr != nil {
			d.logger.Error("close agent session", "error", cerr)
		}
	}()

	state := d.consumeEvents(ctx, agentSession)

	// Commit the assistant response.
	session, err := d.chatSvc.GetSession(ctx, sessionID)
	if err != nil {
		return state.text, fmt.Errorf("loading session for commit: %w", err)
	}
	if session == nil {
		return state.text, fmt.Errorf("session %q not found for commit", sessionID)
	}

	isFirstMessage := session.Title == "New Chat"
	if commitErr := d.chatSvc.CommitMessage(
		ctx, session,
		state.text, state.sdkSessionID, isFirstMessage,
		state.blocks, state.usage,
	); commitErr != nil {
		d.logger.Error("commit message failed", "session_id", sessionID, "error", commitErr)
	}

	return state.text, nil
}

// agentState accumulates agent output during event consumption.
type agentState struct {
	text         string
	sdkSessionID string
	blocks       []storage.MessageBlock
	usage        agent.UsageStats
}

// consumeEvents drains the agent session event channel and collects the response.
func (d *Dispatcher) consumeEvents(ctx context.Context, session *claude.Session) agentState {
	var state agentState

	for {
		select {
		case event, ok := <-session.Events():
			if !ok {
				return state
			}
			if d.processEvent(event, &state) {
				return state
			}
		case <-ctx.Done():
			return state
		}
	}
}

// processEvent handles a single agent event. Returns true when done.
func (d *Dispatcher) processEvent(event claude.Event, state *agentState) bool {
	switch event.Type {
	case claude.TypeAssistant:
		state.blocks = appendBlocks(state.blocks, event.Raw)

	case claude.TypeResult:
		if event.Result == nil {
			return false
		}
		addUsage(&state.usage, event.Result)

		if event.Result.IsError {
			state.sdkSessionID = ""
			d.logger.Warn("agent returned error result",
				"error", event.Result.Result,
				"errors", event.Result.Errors,
			)
			return true
		}

		state.sdkSessionID = event.Result.SessionID
		state.text = event.Result.Result
		return true
	}
	return false
}

// appendBlocks extracts content blocks from a raw assistant event.
func appendBlocks(blocks []storage.MessageBlock, raw json.RawMessage) []storage.MessageBlock {
	var ae struct {
		Message struct {
			Content []struct {
				Type     string          `json:"type"`
				Text     string          `json:"text,omitempty"`
				Thinking string          `json:"thinking,omitempty"`
				ID       string          `json:"id,omitempty"`
				Name     string          `json:"name,omitempty"`
				Input    json.RawMessage `json:"input,omitempty"`
			} `json:"content"`
		} `json:"message"`
	}
	if json.Unmarshal(raw, &ae) != nil {
		return blocks
	}
	for _, blk := range ae.Message.Content {
		switch blk.Type {
		case "thinking":
			if blk.Thinking != "" {
				blocks = append(blocks, storage.MessageBlock{Type: "thinking", Text: blk.Thinking})
			}
		case "text":
			if blk.Text != "" {
				blocks = append(blocks, storage.MessageBlock{Type: "text", Text: blk.Text})
			}
		case "tool_use":
			blocks = append(blocks, storage.MessageBlock{
				Type: "tool_use", ID: blk.ID, Name: blk.Name, Input: blk.Input,
			})
		}
	}
	return blocks
}

// addUsage accumulates token counts from a result event.
func addUsage(usage *agent.UsageStats, r *claude.Result) {
	if r == nil {
		return
	}
	usage.InputTokens += r.Usage.InputTokens
	usage.OutputTokens += r.Usage.OutputTokens
	usage.CacheCreationInputTokens += r.Usage.CacheCreationInputTokens
	usage.CacheReadInputTokens += r.Usage.CacheReadInputTokens
	usage.WebSearchRequests += r.Usage.WebSearchRequests
}

// ExtractTextFromBlocks returns only the text content from message blocks,
// stripping thinking and tool_use blocks. Useful for messaging platforms
// that only need the final text response.
func ExtractTextFromBlocks(blocks []storage.MessageBlock) string {
	var parts []string
	for _, b := range blocks {
		if b.Type == "text" && b.Text != "" {
			parts = append(parts, b.Text)
		}
	}
	return strings.Join(parts, "\n\n")
}
