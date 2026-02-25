package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"

	"github.com/shaharia-lab/agento/internal/config"
)

// NewRootCmd returns the root cobra command wired with the provided AppConfig.
func NewRootCmd(cfg *config.AppConfig) *cobra.Command {
	root := &cobra.Command{
		Use:   "agento",
		Short: "Agento — AI Agents Platform",
		Long:  "A platform for running Claude agents defined in YAML configuration files.",
	}
	return root
}

// Execute is the entrypoint called from main. It loads config, wires the
// command tree, and runs the root command.
func Execute() {
	cfg, err := config.Load()
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}

	root := NewRootCmd(cfg)
	root.AddCommand(NewWebCmd(cfg))
	root.AddCommand(NewAskCmd(cfg))

	if err := root.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
