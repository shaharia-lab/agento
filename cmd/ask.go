package cmd

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/agent"
	"github.com/shaharia-lab/agento/internal/config"
	"github.com/shaharia-lab/agento/internal/tools"
)

// NewAskCmd returns the "ask" subcommand for one-shot CLI queries.
func NewAskCmd(cfg *config.AppConfig) *cobra.Command {
	var agentSlug string
	var noThinking bool
	var agentsDir string
	var mcpsFile string

	cmd := &cobra.Command{
		Use:   "ask [flags] <question> [session-id]",
		Short: "Ask a question via the CLI",
		Long: `Ask a question directly via the CLI.

Examples:
  agento ask "What is 2+2?"
  agento ask --agent hello-world "What time is it?"
  agento ask --agent hello-world "Follow up" <session-uuid>
  agento ask --agent hello-world --no-thinking "Quick question"`,
		Args: cobra.RangeArgs(1, 2),
		RunE: func(cmd *cobra.Command, args []string) error {
			resolvedAgentsDir := cfg.AgentsDir()
			if cmd.Flags().Changed("agents-dir") {
				resolvedAgentsDir = expandHome(agentsDir)
			}
			resolvedMCPsFile := cfg.MCPsFile()
			if cmd.Flags().Changed("mcps-file") {
				resolvedMCPsFile = expandHome(mcpsFile)
			}
			return runAsk(args, agentSlug, noThinking, resolvedAgentsDir, resolvedMCPsFile)
		},
	}

	cmd.Flags().StringVar(&agentSlug, "agent", "", "Agent slug to use")
	cmd.Flags().BoolVar(&noThinking, "no-thinking", false, "Disable extended thinking")
	cmd.Flags().StringVar(&agentsDir, "agents-dir", "",
		"Directory containing agent YAML files (overrides AGENTO_DATA_DIR)")
	cmd.Flags().StringVar(&mcpsFile, "mcps-file", "", "Path to the MCP registry YAML file (overrides AGENTO_DATA_DIR)")

	return cmd
}

func runAsk(args []string, agentSlug string, noThinking bool, agentsDir, mcpsFile string) error {
	question := args[0]
	sessionID := ""
	if len(args) == 2 {
		sessionID = args[1]
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	agentCfg, mcpRegistry, localToolsMCP, err := loadAskDependencies(ctx, agentSlug, agentsDir, mcpsFile)
	if err != nil {
		return err
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

	consumeAskStream(stream)
	return nil
}

func loadAskDependencies(
	ctx context.Context,
	agentSlug, agentsDir, mcpsFile string,
) (*config.AgentConfig, *config.MCPRegistry, *tools.LocalMCPConfig, error) {
	mcpRegistry, err := config.LoadMCPRegistry(mcpsFile)
	if err != nil {
		return nil, nil, nil, fmt.Errorf("loading MCP registry: %w", err)
	}

	agentRegistry, err := config.LoadAgents(agentsDir, mcpRegistry)
	if err != nil {
		return nil, nil, nil, fmt.Errorf("loading agents: %w", err)
	}

	localToolsMCP, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		return nil, nil, nil, fmt.Errorf("starting local tools MCP server: %w", err)
	}

	var agentCfg *config.AgentConfig
	if agentSlug != "" {
		agentCfg = agentRegistry.Get(agentSlug)
		if agentCfg == nil {
			return nil, nil, nil, fmt.Errorf("agent %q not found", agentSlug)
		}
	}

	return agentCfg, mcpRegistry, localToolsMCP, nil
}

func consumeAskStream(stream *claude.Stream) {
	for event := range stream.Events() {
		switch event.Type {
		case claude.TypeStreamEvent:
			handleAskStreamEvent(event)
		case claude.TypeResult:
			handleAskResult(event)
		}
	}
}

func handleAskStreamEvent(event claude.Event) {
	if event.StreamEvent == nil {
		return
	}
	delta := event.StreamEvent.Event.Delta
	if delta == nil {
		return
	}
	if delta.Type == "thinking_delta" && delta.Thinking != "" {
		fmt.Fprint(os.Stderr, delta.Thinking)
	} else if delta.Type == "text_delta" && delta.Text != "" {
		fmt.Print(delta.Text)
	}
}

func handleAskResult(event claude.Event) {
	if event.Result == nil {
		return
	}
	fmt.Println()
	fmt.Fprintf(os.Stderr, "\nsession: %s\ncost: $%.6f | tokens in=%d out=%d\n",
		event.Result.SessionID,
		event.Result.TotalCostUSD,
		event.Result.Usage.InputTokens,
		event.Result.Usage.OutputTokens,
	)
}

func expandHome(path string) string {
	if strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err == nil {
			return filepath.Join(home, path[2:])
		}
	}
	return path
}
