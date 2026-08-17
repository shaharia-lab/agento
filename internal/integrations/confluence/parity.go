package confluence

import (
	"context"
	"fmt"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"

	"github.com/shaharia-lab/agento/internal/config"
)

// This file is the seam `desktop/parity/confluence_parity_test.go` builds its
// cross-language vectors through, and it exists for exactly one reason: the
// vectors have to come from the **real** server, not from a restatement of it.
//
// `desktop/parity` is a different package, so it can neither reach
// `buildMCPServer` nor stand a server up against a local fake — `Start` insists
// on an HTTPS site URL, and an `httptest.Server` is plaintext loopback. The
// alternative — a generator that rebuilt the tool set from this package's source
// — would freeze someone's reading of the code rather than the code, and the
// whole point of the vectors is that a change here fails the Rust port in
// `desktop/src-tauri/src/native/integrations/confluence/`.
//
// #312 did the same thing for GitHub, where the seam is a package variable
// (`SetAPIBase`) because the base is process-wide there. A Confluence site URL
// comes out of the row, so the seam is a parameter instead — which is the
// narrower of the two: there is no process state to leave pointing somewhere
// else, and nothing here can redirect a *live* integration.
//
// Nothing below changes behavior or wording: `Start` remains the only way the
// app builds this server, and this is `Start` with one line removed.

// StartAtSiteURL is [Start] against siteURL, skipping only ValidateSiteURL.
//
// Do not call outside tests. What it is is a primitive for pointing every
// Confluence request in the process — each one bearing a user's API token in a
// `Basic` header — at an arbitrary host over plaintext, which is precisely what
// ValidateSiteURL exists to refuse. It is exported only because `desktop/parity`
// is a different package and the vectors have to come from the real server; the
// Rust port needs no equivalent concession, since `confluence_tools` already
// takes the site URL as an argument and a test can pass loopback to it directly.
//
// The credential parse is deliberately still Go's own, so the `credentials`
// column travels the shipped path; only the site URL is substituted.
func StartAtSiteURL(
	ctx context.Context, cfg *config.IntegrationConfig, siteURL string,
) (claude.McpHTTPServer, error) {
	if !cfg.IsAuthenticated() {
		return claude.McpHTTPServer{}, fmt.Errorf("integration %q has no auth token", cfg.ID)
	}

	var creds config.AtlassianCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("parsing confluence credentials for %q: %w", cfg.ID, err)
	}

	server := buildMCPServer(cfg, siteURL, creds.Email, creds.APIToken)

	serverCfg, err := claude.StartInProcessMCPServer(ctx, cfg.ID, server)
	if err != nil {
		return claude.McpHTTPServer{}, fmt.Errorf("starting in-process MCP server for %q: %w", cfg.ID, err)
	}

	return serverCfg, nil
}
