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
func buildSDKOptions(ctx context.Context, agentCfg *config.AgentConfig, opts RunOptions, systemPrompt string) []claude.Option {
	sdkOpts := []claude.Option{
		claude.WithIncludePartialMessages(),
	}

	// Load the user settings source when a settings file path is configured.
	// This ensures the claude subprocess reads the selected profile's settings.
	// Note: WithSettingSources(SettingSourceUser) loads ~/.claude/settings.json;
	// per-session custom paths will be supported once SDK WithSettings() is available.
	if opts.SettingsFilePath != "" {
		sdkOpts = append(sdkOpts, claude.WithSettingSources(claude.SettingSourceUser))
	}

	if opts.PermissionHandler != nil {
		// The SDK defaults to bypassPermissions + AllowDangerouslySkipPermissions=true.
		// WithDefaultPermissions() overrides BOTH so the subprocess sends
		// can_use_tool control_requests, which our handler can intercept.
		// Without this, bypassPermissions means can_use_tool is never sent.
		//
		// The allowed tools list is computed later in this function and injected
		// into the handler wrapper via a closure. See wrapPermissionHandler below.
		sdkOpts = append(sdkOpts, claude.WithDefaultPermissions())
	} else {
		// No interactive handler. Use the agent's configured permission mode.
		// "default" uses Claude Code's built-in permission rules.
		// Anything else (empty or "bypass") skips all permission checks.
		if agentCfg != nil && agentCfg.PermissionMode == "default" {
			sdkOpts = append(sdkOpts, claude.WithDefaultPermissions())
		} else {
			sdkOpts = append(sdkOpts,
				claude.WithPermissionMode(claude.PermissionModeBypassPermissions),
				claude.WithBypassPermissions(),
			)
		}
	}

	// Model
	if agentCfg != nil && agentCfg.Model != "" {
		sdkOpts = append(sdkOpts, claude.WithModel(agentCfg.Model))
	}

	// System prompt
	if systemPrompt != "" {
		sdkOpts = append(sdkOpts, claude.WithSystemPrompt(systemPrompt))
	}

	// Session ID (resume)
	if opts.SessionID != "" {
		sdkOpts = append(sdkOpts, claude.WithSessionID(opts.SessionID))
	}

	// Thinking mode
	thinkingMode := claude.ThinkingAdaptive
	if opts.NoThinking {
		thinkingMode = claude.ThinkingDisabled
	} else if agentCfg != nil {
		switch agentCfg.Thinking {
		case "disabled":
			thinkingMode = claude.ThinkingDisabled
		case "enabled":
			thinkingMode = claude.ThinkingEnabled
		default:
			thinkingMode = claude.ThinkingAdaptive
		}
	}
	sdkOpts = append(sdkOpts, claude.WithThinking(thinkingMode))

	// Allowed tools + MCP servers
	allowedTools := []string{}
	mcpServers := map[string]any{}

	if agentCfg != nil {
		caps := agentCfg.Capabilities

		// Built-in tools: use the agent's allowlist, or all tools if none specified.
		if len(caps.BuiltIn) > 0 {
			allowedTools = append(allowedTools, caps.BuiltIn...)
		} else if len(caps.Local) == 0 && len(caps.MCP) == 0 {
			// No explicit capabilities at all — allow everything built-in
			allowedTools = append(allowedTools, allBuiltInTools...)
		}

		// Local tools: add the in-process MCP server and the specific tool names.
		if len(caps.Local) > 0 && opts.LocalToolsMCP != nil {
			mcpServers[tools.LocalMCPServerName] = opts.LocalToolsMCP.ServerCfg
			allowedTools = append(allowedTools, opts.LocalToolsMCP.AllowedToolNames(caps.Local)...)
		}

		// External MCP servers from capabilities.mcp — checks both MCPRegistry and IntegrationRegistry.
		if len(caps.MCP) > 0 {
			for serverName, mcpCap := range caps.MCP {
				var sdkCfg any

				// Check external MCP registry first.
				if opts.MCPRegistry != nil {
					sdkCfg = opts.MCPRegistry.GetSDKConfig(serverName)
				}
				// Fall back to integration registry — start a filtered server with
				// only the tools this agent needs so the model doesn't see extras.
				if sdkCfg == nil && opts.IntegrationRegistry != nil {
					if serverCfg, err := opts.IntegrationRegistry.StartFilteredServer(ctx, serverName, mcpCap.Tools); err == nil {
						sdkCfg = serverCfg
					}
				}
				if sdkCfg == nil {
					continue
				}

				mcpServers[serverName] = sdkCfg
				for _, toolName := range mcpCap.Tools {
					allowedTools = append(allowedTools, fmt.Sprintf("mcp__%s__%s", serverName, toolName))
				}
			}
		}
	}
	// For no-agent direct chats, don't set WithAllowedTools or WithStrictMcpConfig
	// so the user's Anthropic-account MCP servers (claude.ai Gmail, Calendar, etc.)
	// remain available alongside any built-in tools.
	//
	// For agents with explicit capabilities, use WithStrictMcpConfig so only the
	// MCP servers we pass via --mcp-config are loaded. Without this, cloud servers
	// from the user's Anthropic account also appear, and the model may pick those
	// instead of the agent's configured tools — resulting in denied tool calls.

	if len(allowedTools) > 0 {
		sdkOpts = append(sdkOpts, claude.WithAllowedTools(allowedTools...))
	}

	// Compute disallowed built-in tools: everything the agent did NOT select.
	// --disallowedTools tells the CLI to hide these tools from the model entirely,
	// whereas --allowedTools only gates execution.
	if agentCfg != nil && len(agentCfg.Capabilities.BuiltIn) > 0 {
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
	}

	if len(mcpServers) > 0 {
		sdkOpts = append(sdkOpts, claude.WithMcpServers(mcpServers))
		sdkOpts = append(sdkOpts, claude.WithStrictMcpConfig())
	}

	// Now that the allowed tools list is finalized, attach the (possibly wrapped)
	// permission handler. When the agent has an explicit allowlist we wrap the
	// caller-supplied handler so that any tool call not on the list is denied
	// at the permission layer — a second defense line after WithAllowedTools.
	if opts.PermissionHandler != nil {
		handler := wrapPermissionHandler(opts.PermissionHandler, allowedTools)
		sdkOpts = append(sdkOpts, claude.WithPermissionHandler(handler))
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
func StreamAgent(ctx context.Context, agentCfg *config.AgentConfig, question string, opts RunOptions) (*claude.Stream, error) {
	systemPrompt := ""
	if agentCfg != nil {
		interpolated, err := Interpolate(agentCfg.SystemPrompt, opts.Variables)
		if err != nil {
			return nil, err
		}
		systemPrompt = interpolated
	}

	sdkOpts := buildSDKOptions(ctx, agentCfg, opts, systemPrompt)
	return claude.Query(ctx, question, sdkOpts...)
}

// StartSession creates a persistent Claude session and sends the first message.
// The subprocess stays alive across TypeResult events; callers can inject follow-up
// messages via session.Send() without spawning a new process.
// The caller must call session.Close() when the conversation is done.
func StartSession(ctx context.Context, agentCfg *config.AgentConfig, firstMessage string, opts RunOptions) (*claude.Session, error) {
	systemPrompt := ""
	if agentCfg != nil {
		interpolated, err := Interpolate(agentCfg.SystemPrompt, opts.Variables)
		if err != nil {
			return nil, err
		}
		systemPrompt = interpolated
	}

	sdkOpts := buildSDKOptions(ctx, agentCfg, opts, systemPrompt)
	session, err := claude.NewSession(ctx, sdkOpts...)
	if err != nil {
		return nil, fmt.Errorf("creating session: %w", err)
	}

	if err := session.Send(firstMessage); err != nil {
		_ = session.Close()
		return nil, fmt.Errorf("sending first message: %w", err)
	}

	return session, nil
}

// RunAgent runs the agent to completion and returns the final AgentResult.
func RunAgent(ctx context.Context, agentCfg *config.AgentConfig, question string, opts RunOptions) (*AgentResult, error) {
	systemPrompt := ""
	if agentCfg != nil {
		interpolated, err := Interpolate(agentCfg.SystemPrompt, opts.Variables)
		if err != nil {
			return nil, err
		}
		systemPrompt = interpolated
	}

	sdkOpts := buildSDKOptions(ctx, agentCfg, opts, systemPrompt)

	stream, err := claude.Query(ctx, question, sdkOpts...)
	if err != nil {
		return nil, fmt.Errorf("starting agent: %w", err)
	}

	var finalThinking string
	var result *AgentResult
	var resultErr error

	for event := range stream.Events() {
		switch event.Type {
		case claude.TypeAssistant:
			if event.Assistant != nil {
				if t := event.Assistant.Thinking(); t != "" {
					finalThinking = t
				}
			}

		case claude.TypeResult:
			if event.Result != nil {
				if event.Result.IsError {
					msg := event.Result.Result
					if msg == "" && len(event.Result.Errors) > 0 {
						msg = strings.Join(event.Result.Errors, "; ")
					}
					if msg == "" {
						msg = fmt.Sprintf("subtype=%s", event.Result.Subtype)
					}
					resultErr = fmt.Errorf("agent error: %s", msg)
				} else {
					result = &AgentResult{
						SessionID: event.Result.SessionID,
						Answer:    event.Result.Result,
						Thinking:  finalThinking,
						CostUSD:   event.Result.TotalCostUSD,
						Usage: UsageStats{
							InputTokens:              event.Result.Usage.InputTokens,
							OutputTokens:             event.Result.Usage.OutputTokens,
							CacheReadInputTokens:     event.Result.Usage.CacheReadInputTokens,
							CacheCreationInputTokens: event.Result.Usage.CacheCreationInputTokens,
						},
					}
				}
				// Do NOT return early here. Drain the remaining events so the
				// subprocess has time to finish writing the session to disk before
				// the caller's context can be canceled.
			}
		}
	}

	// Stream channel is now closed — subprocess has fully exited.
	if resultErr != nil {
		return nil, resultErr
	}
	if result != nil {
		return result, nil
	}
	return nil, fmt.Errorf("agent finished without returning a result")
}
