package trigger

import (
	"context"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
	telegramintegration "github.com/shaharia-lab/agento/internal/integrations/telegram"
	"github.com/shaharia-lab/agento/internal/storage"
	"github.com/shaharia-lab/agento/internal/tools"
)

// Dispatcher matches incoming messages against trigger rules, runs the
// appropriate agent, and sends the reply back to Telegram.
type Dispatcher struct {
	triggerStore        storage.TriggerStore
	agentStore          storage.AgentStore
	chatStore           storage.ChatStore
	integrationStore    storage.IntegrationStore
	mcpRegistry         *config.MCPRegistry
	localToolsMCP       *tools.LocalMCPConfig
	integrationRegistry *integrations.IntegrationRegistry
	settingsMgr         *config.SettingsManager
	logger              *slog.Logger
}

// DispatcherConfig holds all dependencies for the Dispatcher.
type DispatcherConfig struct {
	TriggerStore        storage.TriggerStore
	AgentStore          storage.AgentStore
	ChatStore           storage.ChatStore
	IntegrationStore    storage.IntegrationStore
	MCPRegistry         *config.MCPRegistry
	LocalToolsMCP       *tools.LocalMCPConfig
	IntegrationRegistry *integrations.IntegrationRegistry
	SettingsMgr         *config.SettingsManager
	Logger              *slog.Logger
}

// NewDispatcher creates a new Dispatcher.
func NewDispatcher(cfg DispatcherConfig) *Dispatcher {
	return &Dispatcher{
		triggerStore:        cfg.TriggerStore,
		agentStore:          cfg.AgentStore,
		chatStore:           cfg.ChatStore,
		integrationStore:    cfg.IntegrationStore,
		mcpRegistry:         cfg.MCPRegistry,
		localToolsMCP:       cfg.LocalToolsMCP,
		integrationRegistry: cfg.IntegrationRegistry,
		settingsMgr:         cfg.SettingsMgr,
		logger:              cfg.Logger,
	}
}

// TelegramUpdate represents the relevant fields from a Telegram Update object.
type TelegramUpdate struct {
	UpdateID int64        `json:"update_id"`
	Message  *TelegramMsg `json:"message,omitempty"`
}

// TelegramMsg represents the relevant fields from a Telegram Message object.
type TelegramMsg struct {
	MessageID int           `json:"message_id"`
	Chat      TelegramChat  `json:"chat"`
	Text      string        `json:"text"`
	From      *TelegramUser `json:"from,omitempty"`
}

// TelegramChat represents a Telegram chat.
type TelegramChat struct {
	ID int64 `json:"id"`
}

// TelegramUser represents a Telegram user.
type TelegramUser struct {
	ID        int64  `json:"id"`
	Username  string `json:"username"`
	FirstName string `json:"first_name"`
}

// HandleTelegramUpdate processes an incoming Telegram update for the given integration.
// It runs asynchronously: returns immediately and processes in the background.
func (d *Dispatcher) HandleTelegramUpdate(
	integrationID, botToken string,
	update TelegramUpdate,
) {
	go d.processTelegramUpdate(integrationID, botToken, update)
}

func (d *Dispatcher) processTelegramUpdate(
	integrationID, botToken string,
	update TelegramUpdate,
) {
	ctx := context.Background()

	if update.Message == nil || update.Message.Text == "" {
		return
	}

	if !d.deduplicateUpdate(ctx, integrationID, update.UpdateID) {
		return
	}

	matchedRule, prompt := d.findMatchingRule(ctx, integrationID, update.Message)
	if matchedRule == nil {
		return
	}

	d.logger.Info("trigger rule matched",
		"rule_id", matchedRule.ID, "rule_name", matchedRule.Name,
		"agent_slug", matchedRule.AgentSlug,
		"chat_id", update.Message.Chat.ID)

	d.executeAndReply(ctx, botToken, update.Message, matchedRule, prompt)
}

// deduplicateUpdate checks and marks the update as processed. Returns true if processing should continue.
func (d *Dispatcher) deduplicateUpdate(ctx context.Context, integrationID string, updateID int64) bool {
	processed, err := d.triggerStore.IsUpdateProcessed(ctx, integrationID, updateID)
	if err != nil {
		d.logger.Error("failed to check update deduplication",
			"integration_id", integrationID, "update_id", updateID, "error", err)
		return false
	}
	if processed {
		return false
	}
	if err := d.triggerStore.MarkUpdateProcessed(ctx, integrationID, updateID); err != nil {
		d.logger.Error("failed to mark update as processed",
			"integration_id", integrationID, "update_id", updateID, "error", err)
		return false
	}
	return true
}

// findMatchingRule loads trigger rules and returns the first matching one with the processed prompt.
func (d *Dispatcher) findMatchingRule(
	ctx context.Context, integrationID string, msg *TelegramMsg,
) (*config.TriggerRule, string) {
	rules, err := d.triggerStore.ListRules(ctx, integrationID)
	if err != nil {
		d.logger.Error("failed to load trigger rules", "integration_id", integrationID, "error", err)
		return nil, ""
	}

	chatIDStr := fmt.Sprintf("%d", msg.Chat.ID)
	for _, rule := range rules {
		if !rule.Enabled {
			continue
		}
		if matched, prompt := matchRule(rule, msg.Text, chatIDStr); matched {
			return rule, prompt
		}
	}
	return nil, ""
}

// executeAndReply runs the agent and sends the result back to Telegram.
func (d *Dispatcher) executeAndReply(
	ctx context.Context, botToken string, msg *TelegramMsg,
	rule *config.TriggerRule, prompt string,
) {
	telegramintegration.SendChatAction(ctx, botToken, msg.Chat.ID)

	agentCfg, err := d.resolveAgent(ctx, rule.AgentSlug)
	if err != nil {
		d.logger.Error("failed to resolve agent for trigger", "agent_slug", rule.AgentSlug, "error", err)
		d.sendErrorReply(ctx, botToken, msg.Chat.ID, msg.MessageID)
		return
	}

	chatSession, err := d.chatStore.CreateSession(ctx, rule.AgentSlug, "", "", "")
	if err != nil {
		d.logger.Error("failed to create chat session for trigger", "error", err)
		d.sendErrorReply(ctx, botToken, msg.Chat.ID, msg.MessageID)
		return
	}

	chatSession.Title = fmt.Sprintf("[Telegram] %s", rule.Name)
	if updateErr := d.chatStore.UpdateSession(ctx, chatSession); updateErr != nil {
		d.logger.Warn("failed to update session title", "error", updateErr)
	}

	opts := agent.RunOptions{
		LocalToolsMCP:       d.localToolsMCP,
		MCPRegistry:         d.mcpRegistry,
		IntegrationRegistry: d.integrationRegistry,
	}

	runCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()

	result, err := agent.RunAgent(runCtx, agentCfg, prompt, opts)
	if err != nil {
		d.logger.Error("agent execution failed for trigger", "rule_id", rule.ID, "error", err)
		d.sendErrorReply(ctx, botToken, msg.Chat.ID, msg.MessageID)
		d.saveSessionMessages(ctx, chatSession, prompt, "")
		return
	}

	d.saveSessionMessages(ctx, chatSession, prompt, result.Answer)
	d.updateSessionUsage(ctx, chatSession, result)

	reply := result.Answer
	if reply == "" {
		reply = "No response generated."
	}
	if replyErr := telegramintegration.SendReply(ctx, botToken, msg.Chat.ID, msg.MessageID, reply); replyErr != nil {
		d.logger.Error("failed to send telegram reply", "chat_id", msg.Chat.ID, "error", replyErr)
	}
}

func (d *Dispatcher) updateSessionUsage(
	ctx context.Context, session *storage.ChatSession, result *agent.AgentResult,
) {
	session.SDKSession = result.SessionID
	session.TotalInputTokens = result.Usage.InputTokens
	session.TotalOutputTokens = result.Usage.OutputTokens
	session.TotalCacheCreationTokens = result.Usage.CacheCreationInputTokens
	session.TotalCacheReadTokens = result.Usage.CacheReadInputTokens
	session.UpdatedAt = time.Now().UTC()
	if updateErr := d.chatStore.UpdateSession(ctx, session); updateErr != nil {
		d.logger.Warn("failed to update session after execution", "error", updateErr)
	}
}

// matchRule checks if a message matches the given trigger rule.
// Returns (matched, prompt). The prompt has the prefix stripped if applicable.
func matchRule(rule *config.TriggerRule, text, chatID string) (bool, string) {
	prompt := text

	// Check prefix filter.
	if rule.FilterPrefix != "" {
		if !strings.HasPrefix(strings.ToLower(text), strings.ToLower(rule.FilterPrefix)) {
			return false, ""
		}
		prompt = strings.TrimSpace(text[len(rule.FilterPrefix):])
		if prompt == "" {
			return false, ""
		}
	}

	if !matchesKeywords(rule.FilterKeywords, text) {
		return false, ""
	}

	if !matchesChatIDs(rule.FilterChatIDs, chatID) {
		return false, ""
	}

	return true, prompt
}

// matchesKeywords returns true if keywords is empty or any keyword is found in text (OR logic).
func matchesKeywords(keywords []string, text string) bool {
	if len(keywords) == 0 {
		return true
	}
	lower := strings.ToLower(text)
	for _, kw := range keywords {
		if strings.Contains(lower, strings.ToLower(kw)) {
			return true
		}
	}
	return false
}

// matchesChatIDs returns true if allowedIDs is empty or chatID is in the allowed list (OR logic).
func matchesChatIDs(allowedIDs []string, chatID string) bool {
	if len(allowedIDs) == 0 {
		return true
	}
	for _, id := range allowedIDs {
		if chatID == id {
			return true
		}
	}
	return false
}

func (d *Dispatcher) resolveAgent(ctx context.Context, agentSlug string) (*config.AgentConfig, error) {
	if agentSlug != "" {
		agentCfg, err := d.agentStore.Get(ctx, agentSlug)
		if err != nil {
			return nil, fmt.Errorf("loading agent %q: %w", agentSlug, err)
		}
		if agentCfg == nil {
			return nil, fmt.Errorf("agent %q not found", agentSlug)
		}
		return agentCfg, nil
	}

	// Synthesize minimal config.
	model := "sonnet"
	if d.settingsMgr != nil {
		model = d.settingsMgr.Get().DefaultModel
	}
	return &config.AgentConfig{
		Model:    model,
		Thinking: "adaptive",
	}, nil
}

func (d *Dispatcher) sendErrorReply(ctx context.Context, botToken string, chatID int64, replyToMsgID int) {
	if err := telegramintegration.SendReply(
		ctx, botToken, chatID, replyToMsgID,
		"Sorry, something went wrong.",
	); err != nil {
		d.logger.Error("failed to send error reply", "chat_id", chatID, "error", err)
	}
}

func (d *Dispatcher) saveSessionMessages(
	ctx context.Context, session *storage.ChatSession,
	userPrompt, assistantAnswer string,
) {
	now := time.Now().UTC()
	userMsg := storage.ChatMessage{
		Role:      "user",
		Content:   userPrompt,
		Timestamp: now,
	}
	if err := d.chatStore.AppendMessage(ctx, session.ID, userMsg); err != nil {
		d.logger.Warn("failed to store user message", "error", err)
	}

	if assistantAnswer != "" {
		assistantMsg := storage.ChatMessage{
			Role:      "assistant",
			Content:   assistantAnswer,
			Timestamp: time.Now().UTC(),
		}
		if err := d.chatStore.AppendMessage(ctx, session.ID, assistantMsg); err != nil {
			d.logger.Warn("failed to store assistant message", "error", err)
		}
	}
}
