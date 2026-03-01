// Package google implements the in-process MCP server for Google service integrations.
package google

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"net/http"
	"time"

	"golang.org/x/oauth2"
	googleoauth "golang.org/x/oauth2/google"

	"github.com/shaharia-lab/agento/internal/config"
)

// calendarScopes are the OAuth2 scopes required for Calendar tools.
var calendarScopes = []string{
	"https://www.googleapis.com/auth/calendar",
}

// gmailScopes are the OAuth2 scopes required for Gmail tools.
var gmailScopes = []string{
	"https://www.googleapis.com/auth/gmail.send",
	"https://www.googleapis.com/auth/gmail.readonly",
}

// driveScopes are the OAuth2 scopes required for Drive tools.
var driveScopes = []string{
	"https://www.googleapis.com/auth/drive",
}

// Scopes returns the union of OAuth2 scopes needed for the enabled services in cfg.
func Scopes(services map[string]config.ServiceConfig) []string {
	seen := make(map[string]bool)
	var out []string

	add := func(ss []string) {
		for _, s := range ss {
			if !seen[s] {
				seen[s] = true
				out = append(out, s)
			}
		}
	}

	if svc, ok := services["calendar"]; ok && svc.Enabled {
		add(calendarScopes)
	}
	if svc, ok := services["gmail"]; ok && svc.Enabled {
		add(gmailScopes)
	}
	if svc, ok := services["drive"]; ok && svc.Enabled {
		add(driveScopes)
	}
	return out
}

// OAuthConfig builds an *oauth2.Config for the given integration credentials, redirect port, and services.
func OAuthConfig(
	creds config.GoogleCredentials, redirectPort int, services map[string]config.ServiceConfig,
) *oauth2.Config {
	return &oauth2.Config{
		ClientID:     creds.ClientID,
		ClientSecret: creds.ClientSecret,
		RedirectURL:  fmt.Sprintf("http://localhost:%d/callback", redirectPort),
		Scopes:       Scopes(services),
		Endpoint:     googleoauth.Endpoint,
	}
}

// BuildAuthURL returns the Google OAuth2 authorization URL for the given integration.
func BuildAuthURL(cfg *config.IntegrationConfig, redirectPort int) (string, error) {
	var creds config.GoogleCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return "", fmt.Errorf("parsing google credentials: %w", err)
	}
	oauthCfg := OAuthConfig(creds, redirectPort, cfg.Services)
	return oauthCfg.AuthCodeURL("state", oauth2.AccessTypeOffline, oauth2.ApprovalForce), nil
}

// CallbackResult holds the result of a completed OAuth callback.
type CallbackResult struct {
	Token *oauth2.Token
	Err   error
}

// StartCallbackServer starts a temporary HTTP server on the given port to receive the OAuth2
// callback from Google. It calls onToken with the exchanged token (or an error) and then stops.
// The server stops when the callback is received or ctx is canceled.
func StartCallbackServer(
	ctx context.Context, port int,
	cfg *config.IntegrationConfig,
	onToken func(*oauth2.Token, error),
) error {
	var creds config.GoogleCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return fmt.Errorf("parsing google credentials: %w", err)
	}
	oauthCfg := OAuthConfig(creds, port, cfg.Services)
	resultCh := make(chan CallbackResult, 1)

	mux := http.NewServeMux()
	mux.HandleFunc("/callback", callbackHandler(oauthCfg, resultCh))

	addr := fmt.Sprintf("localhost:%d", port)
	ln, err := (&net.ListenConfig{}).Listen(ctx, "tcp", addr)
	if err != nil {
		return fmt.Errorf("listening on %s: %w", addr, err)
	}

	srv := &http.Server{
		Handler:      mux,
		ReadTimeout:  30 * time.Second,
		WriteTimeout: 30 * time.Second,
	}

	go func() {
		if serveErr := srv.Serve(ln); serveErr != nil && serveErr != http.ErrServerClosed {
			log.Printf("oauth callback server error: %v", serveErr)
		}
	}()

	go func() {
		defer func() {
			if cerr := srv.Close(); cerr != nil {
				log.Printf("oauth server close error: %v", cerr)
			}
		}()
		select {
		case result := <-resultCh:
			onToken(result.Token, result.Err)
		case <-ctx.Done():
			onToken(nil, ctx.Err())
		}
	}()

	return nil
}

const oauthSuccessHTML = `<!DOCTYPE html><html><body>
<h2>Authentication successful!</h2>
<p>You can close this tab and return to Agento.</p>
<script>window.close();</script>
</body></html>`

func callbackHandler(
	oauthCfg *oauth2.Config, resultCh chan<- CallbackResult,
) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		code := r.URL.Query().Get("code")
		if code == "" {
			errMsg := r.URL.Query().Get("error")
			if errMsg == "" {
				errMsg = "no code in callback"
			}
			resultCh <- CallbackResult{
				Err: fmt.Errorf("oauth callback error: %s", errMsg),
			}
			http.Error(w, "Authentication failed: "+errMsg, http.StatusBadRequest)
			return
		}

		token, err := oauthCfg.Exchange(r.Context(), code)
		if err != nil {
			resultCh <- CallbackResult{
				Err: fmt.Errorf("exchanging code: %w", err),
			}
			http.Error(w, "Failed to exchange token", http.StatusInternalServerError)
			return
		}

		resultCh <- CallbackResult{Token: token}
		w.Header().Set("Content-Type", "text/html")
		if _, werr := fmt.Fprint(w, oauthSuccessHTML); werr != nil {
			return
		}
	}
}

// TokenFromJSON parses a raw JSON token (for storage/retrieval helpers).
func TokenFromJSON(data []byte) (*oauth2.Token, error) {
	var tok oauth2.Token
	if err := json.Unmarshal(data, &tok); err != nil {
		return nil, err
	}
	return &tok, nil
}
