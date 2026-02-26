package agent

import (
	"context"
	"fmt"
	"strings"
	"time"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/tools"
)

// allBuiltInTools is the full list of Claude Code built-in tools available to agents.
var allBuiltInTools = []string{
	"Read", "Write", "Edit", "Bash", "Glob", "Grep", "WebFetch", "WebSearch", "Task",
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

	// PermissionHandler is an optional callback invoked for each can_use_tool
	// control_request from claude. When set it overrides the default bypass-all
	// behavior and may block (e.g. to ask a human before a tool runs).
	PermissionHandler claude.PermissionHandler
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
func buildSDKOptions(agentCfg *config.AgentConfig, opts RunOptions, systemPrompt string) []claude.Option {
	sdkOpts := []claude.Option{
		claude.WithPermissionMode(claude.PermissionModeBypassPermissions),
		claude.WithBypassPermissions(),
		claude.WithIncludePartialMessages(),
	}

	if opts.PermissionHandler != nil {
		sdkOpts = append(sdkOpts, claude.WithPermissionHandler(opts.PermissionHandler))
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

		// External MCP servers from capabilities.mcp
		if len(caps.MCP) > 0 && opts.MCPRegistry != nil {
			for serverName, mcpCap := range caps.MCP {
				sdkCfg := opts.MCPRegistry.GetSDKConfig(serverName)
				if sdkCfg == nil {
					continue
				}
				mcpServers[serverName] = sdkCfg
				for _, toolName := range mcpCap.Tools {
					allowedTools = append(allowedTools, fmt.Sprintf("mcp__%s__%s", serverName, toolName))
				}
			}
		}
	} else {
		// No agent config — allow all built-in tools
		allowedTools = allBuiltInTools
	}

	if len(allowedTools) > 0 {
		sdkOpts = append(sdkOpts, claude.WithAllowedTools(allowedTools...))
	}

	if len(mcpServers) > 0 {
		sdkOpts = append(sdkOpts, claude.WithMcpServers(mcpServers))
	}

	return sdkOpts
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

	sdkOpts := buildSDKOptions(agentCfg, opts, systemPrompt)
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

	sdkOpts := buildSDKOptions(agentCfg, opts, systemPrompt)
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

	sdkOpts := buildSDKOptions(agentCfg, opts, systemPrompt)

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
