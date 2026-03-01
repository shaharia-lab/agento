package slack

import (
	"context"
	"fmt"
	"log"
	"net"
	"net/http"
	"time"

	"golang.org/x/oauth2"

	"github.com/shaharia-lab/agento/internal/config"
)

// oauthScopes are the OAuth2 scopes required for the Slack integration.
var oauthScopes = []string{
	"channels:read",
	"channels:history",
	"chat:write",
	"users:read",
	"search:read",
	"groups:read",
	"groups:history",
}

// OAuthConfig builds an *oauth2.Config for the given Slack integration credentials and redirect port.
func OAuthConfig(creds config.SlackCredentials, redirectPort int) *oauth2.Config {
	return &oauth2.Config{
		ClientID:     creds.ClientID,
		ClientSecret: creds.ClientSecret,
		RedirectURL:  fmt.Sprintf("http://localhost:%d/callback", redirectPort),
		Scopes:       oauthScopes,
		Endpoint: oauth2.Endpoint{
			AuthURL:  "https://slack.com/oauth/v2/authorize",
			TokenURL: "https://slack.com/api/oauth.v2.access",
		},
	}
}

// BuildAuthURL returns the Slack OAuth2 authorization URL for the given integration.
func BuildAuthURL(cfg *config.IntegrationConfig, redirectPort int) (string, error) {
	var creds config.SlackCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return "", fmt.Errorf("parsing slack credentials: %w", err)
	}
	oauthCfg := OAuthConfig(creds, redirectPort)
	// Slack V2 OAuth tokens do not expire by default and don't use refresh tokens.
	// If the Slack app has "Token Rotation" enabled, this flow would need to handle
	// refresh tokens via the oauth2 library's token source.
	return oauthCfg.AuthCodeURL("state"), nil
}

// CallbackResult holds the result of a completed OAuth callback.
type CallbackResult struct {
	Token *oauth2.Token
	Err   error
}

// StartCallbackServer starts a temporary HTTP server on the given port to receive the OAuth2
// callback from Slack. It calls onToken with the exchanged token (or an error) and then stops.
// The server stops when the callback is received or ctx is canceled.
func StartCallbackServer(
	ctx context.Context, port int,
	cfg *config.IntegrationConfig,
	onToken func(*oauth2.Token, error),
) error {
	var creds config.SlackCredentials
	if err := cfg.ParseCredentials(&creds); err != nil {
		return fmt.Errorf("parsing slack credentials: %w", err)
	}
	oauthCfg := OAuthConfig(creds, port)
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
			log.Printf("slack oauth callback server error: %v", serveErr)
		}
	}()

	go func() {
		defer func() {
			if cerr := srv.Close(); cerr != nil {
				log.Printf("slack oauth server close error: %v", cerr)
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
