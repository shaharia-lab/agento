package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/integrations"
	"github.com/shaharia-lab/agento/internal/tools"
)

// allBuiltInTools is the full list of Claude Code built-in tools available to agents.
var allBuiltInTools = []string{
	"Read", "Write", "Edit", "Bash", "Glob", "Grep", "WebFetch", "WebSearch", "Task",
	"TaskOutput", "TaskStop", "NotebookEdit",
}

// RunOptions configures an agent invocation.
type RunOptions struct {
	// SessionID resumes an existing session for multi-turn conversations.
	SessionID string

	// NoThinking disables extended thinking regardless of agent config.
	NoThinking bool

	// Variables are template values for {{variable}} interpolation in the system prompt.
	Variables map[string]string

	// LocalToolsMCP is the running in-process local tools MCP server.
	LocalToolsMCP *tools.LocalMCPConfig

	// MCPRegistry provides the SDK configs for external MCP servers.
	MCPRegistry *config.MCPRegistry

	// IntegrationRegistry provides in-process MCP servers for external service integrations.
	IntegrationRegistry *integrations.IntegrationRegistry

	// PermissionHandler is an optional callback invoked for each can_use_tool
	// control_request from claude. When set it overrides the default bypass-all
	// behavior and may block (e.g. to ask a human before a tool runs).
	PermissionHandler claude.PermissionHandler

	// SettingsFilePath is the absolute path to the Claude settings JSON file
	// for this session. When set, the user settings source is loaded via
	// WithSettingSources so the subprocess picks up the profile's configuration.
	// (Future: use WithSettings(filePath) once SDK v0.2.1 is available.)
	SettingsFilePath string
}

// AgentResult is the final result of an agent invocation.
//
//nolint:revive // AgentResult is intentionally named with the package prefix for call-site clarity.
type AgentResult struct {
	SessionID string
	Answer    string
	Thinking  string
	CostUSD   float64
	Usage     UsageStats
}

// UsageStats holds token usage information.
type UsageStats struct {
	InputTokens              int
	OutputTokens             int
	CacheReadInputTokens     int
	CacheCreationInputTokens int
}

// MissingVariableError is returned when a required template variable is absent.
type MissingVariableError struct {
	Variable string
}

func (e *MissingVariableError) Error() string {
	return fmt.Sprintf("missing required template variable: %q", e.Variable)
}

// Interpolate replaces {{variable}} placeholders in template with values from vars.
// Built-in variables (current_date, current_time) are always available.
// Returns MissingVariableError if a referenced variable is not present.
func Interpolate(template string, vars map[string]string) (string, error) {
	now := time.Now()

	builtins := map[string]string{
		"current_date": now.Format("2006-01-02"),
		"current_time": now.Format("15:04:05"),
	}

	result := template
	i := 0
	for {
		start := strings.Index(result[i:], "{{")
		if start == -1 {
			break
		}
		start += i
		end := strings.Index(result[start:], "}}")
		if end == -1 {
			break
		}
		end += start

		name := strings.TrimSpace(result[start+2 : end])

		var value string
		if v, ok := builtins[name]; ok {
			value = v
		} else if v, ok := vars[name]; ok {
			value = v
		} else {
			return "", &MissingVariableError{Variable: name}
		}

		result = result[:start] + value + result[end+2:]
		i = start + len(value)
	}

	return result, nil
}

// buildSDKOptions constructs the claude SDK options for the given agent config and run options.
// ctx is used to scope the lifetime of any per-session MCP servers started for integrations.
func buildSDKOptions(
	ctx context.Context, agentCfg *config.AgentConfig,
	opts RunOptions, systemPrompt string,
) []claude.Option {
	sdkOpts := []claude.Option{
		claude.WithIncludePartialMessages(),
	}

	sdkOpts = appendSettingsOpts(sdkOpts, opts, agentCfg)
	sdkOpts = appendPermissionOpts(sdkOpts, opts, agentCfg)
	sdkOpts = appendModelAndPromptOpts(sdkOpts, agentCfg, opts, systemPrompt)
	sdkOpts = append(sdkOpts, claude.WithThinking(resolveThinkingMode(opts, agentCfg)))

	allowedTools, mcpServers := resolveToolsAndMCP(ctx, agentCfg, opts)
	sdkOpts = appendToolOpts(sdkOpts, agentCfg, allowedTools, mcpServers)

	if opts.PermissionHandler != nil {
		handler := wrapPermissionHandler(opts.PermissionHandler, allowedTools)
		sdkOpts = append(sdkOpts, claude.WithPermissionHandler(handler))
	}

	return sdkOpts
}

func appendSettingsOpts(sdkOpts []claude.Option, opts RunOptions, _ *config.AgentConfig) []claude.Option {
	if opts.SettingsFilePath != "" {
		sdkOpts = append(sdkOpts, claude.WithSettingSources(claude.SettingSourceUser))
	}
	return sdkOpts
}

func appendPermissionOpts(sdkOpts []claude.Option, opts RunOptions, agentCfg *config.AgentConfig) []claude.Option {
	if opts.PermissionHandler != nil {
		return append(sdkOpts, claude.WithDefaultPermissions())
	}
	if agentCfg != nil && agentCfg.PermissionMode == "default" {
		return append(sdkOpts, claude.WithDefaultPermissions())
	}
	return append(sdkOpts,
		claude.WithPermissionMode(claude.PermissionModeBypassPermissions),
		claude.WithBypassPermissions(),
	)
}

func appendModelAndPromptOpts(
	sdkOpts []claude.Option, agentCfg *config.AgentConfig,
	opts RunOptions, systemPrompt string,
) []claude.Option {
	if agentCfg != nil && agentCfg.Model != "" {
		sdkOpts = append(sdkOpts, claude.WithModel(agentCfg.Model))
	}
	if systemPrompt != "" {
		sdkOpts = append(sdkOpts, claude.WithSystemPrompt(systemPrompt))
	}
	if opts.SessionID != "" {
		sdkOpts = append(sdkOpts, claude.WithSessionID(opts.SessionID))
	}
	return sdkOpts
}

func resolveThinkingMode(opts RunOptions, agentCfg *config.AgentConfig) claude.ThinkingMode {
	if opts.NoThinking {
		return claude.ThinkingDisabled
	}
	if agentCfg == nil {
		return claude.ThinkingAdaptive
	}
	switch agentCfg.Thinking {
	case "disabled":
		return claude.ThinkingDisabled
	case "enabled":
		return claude.ThinkingEnabled
	default:
		return claude.ThinkingAdaptive
	}
}

// resolveToolsAndMCP builds the allowed tools list and MCP server map from agent capabilities.
func resolveToolsAndMCP(ctx context.Context, agentCfg *config.AgentConfig, opts RunOptions) ([]string, map[string]any) {
	allowedTools := []string{}
	mcpServers := map[string]any{}

	if agentCfg == nil {
		return allowedTools, mcpServers
	}

	caps := agentCfg.Capabilities
	allowedTools = resolveBuiltInTools(caps)
	allowedTools, mcpServers = resolveLocalTools(allowedTools, mcpServers, caps, opts)
	allowedTools, mcpServers = resolveExternalMCP(ctx, allowedTools, mcpServers, caps, opts)

	return allowedTools, mcpServers
}

func resolveBuiltInTools(caps config.AgentCapabilities) []string {
	if len(caps.BuiltIn) > 0 {
		return append([]string{}, caps.BuiltIn...)
	}
	if len(caps.Local) == 0 && len(caps.MCP) == 0 {
		return append([]string{}, allBuiltInTools...)
	}
	return []string{}
}

func resolveLocalTools(
	allowedTools []string, mcpServers map[string]any,
	caps config.AgentCapabilities, opts RunOptions,
) ([]string, map[string]any) {
	if len(caps.Local) > 0 && opts.LocalToolsMCP != nil {
		mcpServers[tools.LocalMCPServerName] = opts.LocalToolsMCP.ServerCfg
		allowedTools = append(allowedTools, opts.LocalToolsMCP.AllowedToolNames(caps.Local)...)
	}
	return allowedTools, mcpServers
}

func resolveExternalMCP(
	ctx context.Context, allowedTools []string,
	mcpServers map[string]any, caps config.AgentCapabilities,
	opts RunOptions,
) ([]string, map[string]any) {
	for serverName, mcpCap := range caps.MCP {
		sdkCfg := resolveServerConfig(ctx, serverName, mcpCap.Tools, opts)
		if sdkCfg == nil {
			continue
		}
		mcpServers[serverName] = sdkCfg
		for _, toolName := range mcpCap.Tools {
			allowedTools = append(allowedTools, fmt.Sprintf("mcp__%s__%s", serverName, toolName))
		}
	}
	return allowedTools, mcpServers
}

func resolveServerConfig(ctx context.Context, serverName string, toolNames []string, opts RunOptions) any {
	if opts.MCPRegistry != nil {
		if cfg := opts.MCPRegistry.GetSDKConfig(serverName); cfg != nil {
			return cfg
		}
	}
	if opts.IntegrationRegistry != nil {
		if cfg, err := opts.IntegrationRegistry.StartFilteredServer(ctx, serverName, toolNames); err == nil {
			return cfg
		}
	}
	return nil
}

func appendToolOpts(
	sdkOpts []claude.Option, agentCfg *config.AgentConfig,
	allowedTools []string, mcpServers map[string]any,
) []claude.Option {
	if len(allowedTools) > 0 {
		sdkOpts = append(sdkOpts, claude.WithAllowedTools(allowedTools...))
	}

	sdkOpts = appendDisallowedTools(sdkOpts, agentCfg)

	if len(mcpServers) > 0 {
		sdkOpts = append(sdkOpts, claude.WithMcpServers(mcpServers))
		sdkOpts = append(sdkOpts, claude.WithStrictMcpConfig())
	}

	return sdkOpts
}

// appendDisallowedTools computes and appends the disallowed built-in tools.
func appendDisallowedTools(sdkOpts []claude.Option, agentCfg *config.AgentConfig) []claude.Option {
	if agentCfg == nil || len(agentCfg.Capabilities.BuiltIn) == 0 {
		return sdkOpts
	}
	selected := make(map[string]bool, len(agentCfg.Capabilities.BuiltIn))
	for _, t := range agentCfg.Capabilities.BuiltIn {
		selected[t] = true
	}
	var disallowed []string
	for _, t := range allBuiltInTools {
		if !selected[t] {
			disallowed = append(disallowed, t)
		}
	}
	if len(disallowed) > 0 {
		sdkOpts = append(sdkOpts, claude.WithDisallowedTools(disallowed...))
	}
	return sdkOpts
}

// wrapPermissionHandler returns a PermissionHandler that enforces the allowed
// tools list before delegating to inner. AskUserQuestion is always allowed
// (it is a special interactive tool, not an external capability).
// When allowedTools is empty (no-agent direct chat) the inner handler is
// returned unwrapped so that all tools are reachable.
func wrapPermissionHandler(inner claude.PermissionHandler, allowedTools []string) claude.PermissionHandler {
	if len(allowedTools) == 0 {
		return inner
	}
	set := make(map[string]struct{}, len(allowedTools))
	for _, t := range allowedTools {
		set[t] = struct{}{}
	}
	return func(toolName string, input json.RawMessage, ctx claude.PermissionContext) claude.PermissionResult {
		// AskUserQuestion is always permitted — it drives the multi-turn Q&A flow.
		if toolName == "AskUserQuestion" {
			return inner(toolName, input, ctx)
		}
		if _, ok := set[toolName]; !ok {
			return claude.PermissionResult{
				Behavior: "deny",
				Message:  fmt.Sprintf("tool %q is not in this agent's allowed capabilities", toolName),
			}
		}
		return inner(toolName, input, ctx)
	}
}

// StreamAgent starts a streaming agent invocation and returns the *claude.Stream.
// The caller is responsible for consuming events from stream.Events().
func StreamAgent(
	ctx context.Context, agentCfg *config.AgentConfig, question string, opts RunOptions,
) (*claude.Stream, error) {
	systemPrompt, err := resolveSystemPrompt(agentCfg, opts)
	if err != nil {
		return nil, err
	}

	sdkOpts := buildSDKOptions(ctx, agentCfg, opts, systemPrompt)
	return claude.Query(ctx, question, sdkOpts...)
}

// StartSession creates a persistent Claude session and sends the first message.
// The subprocess stays alive across TypeResult events; callers can inject follow-up
// messages via session.Send() without spawning a new process.
// The caller must call session.Close() when the conversation is done.
func StartSession(
	ctx context.Context, agentCfg *config.AgentConfig, firstMessage string, opts RunOptions,
) (*claude.Session, error) {
	systemPrompt, err := resolveSystemPrompt(agentCfg, opts)
	if err != nil {
		return nil, err
	}

	sdkOpts := buildSDKOptions(ctx, agentCfg, opts, systemPrompt)
	session, err := claude.NewSession(ctx, sdkOpts...)
	if err != nil {
		return nil, fmt.Errorf("creating session: %w", err)
	}

	if err := session.Send(firstMessage); err != nil {
		if cerr := session.Close(); cerr != nil {
			return nil, fmt.Errorf("sending first message: %w (also failed to close session: %v)", err, cerr)
		}
		return nil, fmt.Errorf("sending first message: %w", err)
	}

	return session, nil
}

// RunAgent runs the agent to completion and returns the final AgentResult.
func RunAgent(
	ctx context.Context, agentCfg *config.AgentConfig,
	question string, opts RunOptions,
) (*AgentResult, error) {
	systemPrompt, err := resolveSystemPrompt(agentCfg, opts)
	if err != nil {
		return nil, err
	}

	sdkOpts := buildSDKOptions(ctx, agentCfg, opts, systemPrompt)

	stream, err := claude.Query(ctx, question, sdkOpts...)
	if err != nil {
		return nil, fmt.Errorf("starting agent: %w", err)
	}

	return collectRunResult(stream)
}

// resolveSystemPrompt interpolates the agent's system prompt if an agent config is provided.
func resolveSystemPrompt(agentCfg *config.AgentConfig, opts RunOptions) (string, error) {
	if agentCfg == nil {
		return "", nil
	}
	return Interpolate(agentCfg.SystemPrompt, opts.Variables)
}

func collectRunResult(stream *claude.Stream) (*AgentResult, error) {
	var finalThinking string
	var result *AgentResult
	var resultErr error

	for event := range stream.Events() {
		processRunEvent(event, &finalThinking, &result, &resultErr)
	}

	if resultErr != nil {
		return nil, resultErr
	}
	if result != nil {
		return result, nil
	}
	return nil, fmt.Errorf("agent finished without returning a result")
}

// processRunEvent handles a single event during result collection.
// It updates the thinking, result, and error pointers in place.
// We do NOT return early on TypeResult — the remaining events must be drained
// so the subprocess has time to finish writing the session to disk.
func processRunEvent(event claude.Event, thinking *string, result **AgentResult, resultErr *error) {
	switch event.Type {
	case claude.TypeAssistant:
		if event.Assistant != nil {
			if t := event.Assistant.Thinking(); t != "" {
				*thinking = t
			}
		}
	case claude.TypeResult:
		if event.Result == nil {
			return
		}
		if event.Result.IsError {
			*resultErr = buildResultError(event.Result)
		} else {
			*result = buildAgentResult(event.Result, *thinking)
		}
	}
}

func buildResultError(r *claude.Result) error {
	msg := r.Result
	if msg == "" && len(r.Errors) > 0 {
		msg = strings.Join(r.Errors, "; ")
	}
	if msg == "" {
		msg = fmt.Sprintf("subtype=%s", r.Subtype)
	}
	return fmt.Errorf("agent error: %s", msg)
}

func buildAgentResult(r *claude.Result, thinking string) *AgentResult {
	return &AgentResult{
		SessionID: r.SessionID,
		Answer:    r.Result,
		Thinking:  thinking,
		CostUSD:   r.TotalCostUSD,
		Usage: UsageStats{
			InputTokens:              r.Usage.InputTokens,
			OutputTokens:             r.Usage.OutputTokens,
			CacheReadInputTokens:     r.Usage.CacheReadInputTokens,
			CacheCreationInputTokens: r.Usage.CacheCreationInputTokens,
		},
	}
}
