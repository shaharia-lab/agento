package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/agent"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/config"
	"github.com/shaharia-lab/agents-platform-cc-go/internal/tools"
	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
)

var askCmd = &cobra.Command{
	Use:   "ask [flags] <question> [session-id]",
	Short: "Ask a question via the CLI",
	Long: `Ask a question directly via the CLI.

Examples:
  agents-platform ask "What is 2+2?"
  agents-platform ask --agent hello-world "What time is it?"
  agents-platform ask --agent hello-world "Follow up" <session-uuid>
  agents-platform ask --agent hello-world --no-thinking "Quick question"`,
	Args: cobra.RangeArgs(1, 2),
	RunE: runAsk,
}

func init() {
	askCmd.Flags().String("agent", "", "Agent slug to use")
	askCmd.Flags().Bool("no-thinking", false, "Disable extended thinking")
	askCmd.Flags().String("agents-dir", "", "Directory containing agent YAML files (overrides AGENTS_DIR env var)")
	askCmd.Flags().String("mcps-file", "", "Path to the MCP registry YAML file (overrides MCPS_FILE env var)")
}

func runAsk(cmd *cobra.Command, args []string) error {
	question := args[0]
	sessionID := ""
	if len(args) == 2 {
		sessionID = args[1]
	}

	agentSlug, _ := cmd.Flags().GetString("agent")
	noThinking, _ := cmd.Flags().GetBool("no-thinking")
	agentsDir := resolveAgentsDir(cmd)
	mcpsFile := resolveMcpsFile(cmd)

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	mcpRegistry, err := config.LoadMCPRegistry(mcpsFile)
	if err != nil {
		return fmt.Errorf("loading MCP registry: %w", err)
	}

	agentRegistry, err := config.LoadAgents(agentsDir, mcpRegistry)
	if err != nil {
		return fmt.Errorf("loading agents: %w", err)
	}

	localToolsMCP, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		return fmt.Errorf("starting local tools MCP server: %w", err)
	}

	var agentCfg *config.AgentConfig
	if agentSlug != "" {
		agentCfg = agentRegistry.Get(agentSlug)
		if agentCfg == nil {
			return fmt.Errorf("agent %q not found", agentSlug)
		}
	}

	runOpts := agent.RunOptions{
		SessionID:     sessionID,
		NoThinking:    noThinking,
		LocalToolsMCP: localToolsMCP,
		MCPRegistry:   mcpRegistry,
	}

	stream, err := agent.StreamAgent(ctx, agentCfg, question, runOpts)
	if err != nil {
		return fmt.Errorf("starting agent: %w", err)
	}

	for event := range stream.Events() {
		switch event.Type {
		case claude.TypeStreamEvent:
			if event.StreamEvent != nil {
				delta := event.StreamEvent.Event.Delta
				if delta != nil {
					if delta.Type == "thinking_delta" && delta.Thinking != "" {
						fmt.Fprint(os.Stderr, delta.Thinking)
					} else if delta.Type == "text_delta" && delta.Text != "" {
						fmt.Print(delta.Text)
					}
				}
			}
		case claude.TypeAssistant:
			if event.Assistant != nil {
				text := event.Assistant.Text()
				if text != "" {
					// Only print if not already streamed via stream events
				}
			}
		case claude.TypeResult:
			if event.Result != nil {
				fmt.Println()
				fmt.Fprintf(os.Stderr, "\nsession: %s\ncost: $%.6f | tokens in=%d out=%d\n",
					event.Result.SessionID,
					event.Result.TotalCostUSD,
					event.Result.Usage.InputTokens,
					event.Result.Usage.OutputTokens,
				)
			}
		}
	}

	return nil
}
