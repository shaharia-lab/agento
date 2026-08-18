// Cross-language vectors for the OAuth authorization URLs (#318).
//
// `POST /api/integrations/{id}/auth/start` answers a URL the user's browser is
// sent to, and it is the one response in this port that **cannot** be compared
// by the live diff: the redirect port comes from `integrations.FreePort()`, so
// two implementations answering the same question produce two different URLs.
// The bytes still have to match everywhere else, and every part of them is a
// rule rather than a value:
//
//   - the **scope set and its order**, which for Google is the union of the
//     enabled services' scopes in `Scopes`'s fixed calendar/gmail/drive order,
//     deduplicated — an agent granted the wrong scopes silently loses tools;
//   - the **query encoding**, because `AuthCodeURL` builds through
//     `url.Values.Encode()`, which sorts by key and percent-encodes with Go's
//     own `url.QueryEscape` (a space is `+`, not `%20`);
//   - the **auth-code options**, which differ per provider: Google passes
//     `AccessTypeOffline` and `ApprovalForce` (so a refresh token is issued
//     every time), Slack passes none at all;
//   - the **authorization endpoint**, which for Slack is the v2 path.
//
// A hand-written Rust literal would pin only what its author believed. Here the
// URLs are what Go's own `BuildAuthURL` returned, and both languages assert
// against them: a change to Go's builder fails Go's suite, and a Rust
// divergence fails Rust's.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestOAuthVectors -update-oauth-vectors
package parity

import (
	"encoding/json"
	"flag"
	"os"
	"testing"

	"github.com/shaharia-lab/agento/internal/config"
	googleintegration "github.com/shaharia-lab/agento/internal/integrations/google"
	slackintegration "github.com/shaharia-lab/agento/internal/integrations/slack"
)

const oauthVectorsFile = "oauth_vectors.json"

var updateOAuthVectors = flag.Bool("update-oauth-vectors", false,
	"rewrite "+oauthVectorsFile+" from what Go's BuildAuthURL produces")

// oauthCase is one integration row, and the URL Go builds for it.
type oauthCase struct {
	Name string `json:"name"`
	// Why this case exists, for a reader of the JSON.
	Note string `json:"note"`
	Type string `json:"type"`
	// The raw `credentials` column, verbatim, so the port exercises its own
	// decoder rather than a struct the test built.
	Credentials json.RawMessage `json:"credentials"`
	// The `services` column. Google reads it for scopes; Slack ignores it.
	Services map[string]config.ServiceConfig `json:"services"`
	Port     int                             `json:"port"`
	AuthURL  string                          `json:"auth_url"`
}

type oauthVectors struct {
	Cases []oauthCase `json:"cases"`
}

// enabled is a service the integration has switched on.
func enabled() config.ServiceConfig { return config.ServiceConfig{Enabled: true} }

// off is a service present in the row but switched off — the case that
// separates "reads the map" from "reads the flag".
func off() config.ServiceConfig { return config.ServiceConfig{Enabled: false} }

func oauthCases() []oauthCase {
	googleCreds := json.RawMessage(`{"client_id":"cid-123.apps.googleusercontent.com","client_secret":"secret-456"}`)
	return []oauthCase{
		{
			Name:        "google/all-three-services",
			Note:        "the full scope union, in Scopes's calendar-gmail-drive order",
			Type:        "google",
			Credentials: googleCreds,
			Services: map[string]config.ServiceConfig{
				"calendar": enabled(), "gmail": enabled(), "drive": enabled(),
			},
			Port: 43117,
		},
		{
			Name:        "google/one-service",
			Note:        "only the enabled service's scopes are requested",
			Type:        "google",
			Credentials: googleCreds,
			Services:    map[string]config.ServiceConfig{"gmail": enabled()},
			Port:        43117,
		},
		{
			Name: "google/disabled-service-is-not-a-scope",
			Note: "presence in the map is not enough; Enabled is what counts",
			Type: "google",
			Credentials: json.RawMessage(
				`{"client_id":"cid-123.apps.googleusercontent.com","client_secret":"secret-456"}`),
			Services: map[string]config.ServiceConfig{"calendar": enabled(), "gmail": off()},
			Port:     43117,
		},
		{
			Name:        "google/no-services",
			Note:        "an empty scope set still builds a URL — with scope absent, not empty",
			Type:        "google",
			Credentials: googleCreds,
			Services:    map[string]config.ServiceConfig{},
			Port:        43117,
		},
		{
			Name: "google/credentials-needing-escapes",
			Note: "url.Values.Encode is Go's QueryEscape: a space is +, not %20",
			Type: "google",
			Credentials: json.RawMessage(
				`{"client_id":"a b&c=d","client_secret":"s"}`),
			Services: map[string]config.ServiceConfig{"drive": enabled()},
			Port:     1,
		},
		{
			Name: "slack/bot-scopes",
			Note: "the v2 authorize endpoint, and no access-type or approval-prompt",
			Type: "slack",
			Credentials: json.RawMessage(
				`{"client_id":"9876.5432","client_secret":"slack-secret"}`),
			Services: map[string]config.ServiceConfig{"messaging": enabled()},
			Port:     43118,
		},
		{
			Name: "slack/services-are-ignored",
			Note: "Slack's scopes are fixed, so the same URL as above with no services",
			Type: "slack",
			Credentials: json.RawMessage(
				`{"client_id":"9876.5432","client_secret":"slack-secret"}`),
			Services: map[string]config.ServiceConfig{},
			Port:     43118,
		},
	}
}

// buildAuthURL is `startProviderCallback`'s dispatch, without the callback
// server — the URL half is the only part a vector can carry.
func buildAuthURL(t *testing.T, c oauthCase) string {
	t.Helper()
	cfg := &config.IntegrationConfig{
		ID:          "vec",
		Type:        c.Type,
		Credentials: c.Credentials,
		Services:    c.Services,
	}
	var (
		url string
		err error
	)
	switch c.Type {
	case "google":
		url, err = googleintegration.BuildAuthURL(cfg, c.Port)
	case "slack":
		url, err = slackintegration.BuildAuthURL(cfg, c.Port)
	default:
		t.Fatalf("case %q has type %q, which has no OAuth flow", c.Name, c.Type)
	}
	if err != nil {
		t.Fatalf("case %q: building auth URL: %v", c.Name, err)
	}
	return url
}

func TestOAuthVectors(t *testing.T) {
	cases := oauthCases()
	for i := range cases {
		cases[i].AuthURL = buildAuthURL(t, cases[i])
	}

	encoded, err := json.MarshalIndent(oauthVectors{Cases: cases}, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateOAuthVectors {
		if writeErr := os.WriteFile(oauthVectorsFile, encoded, 0o600); writeErr != nil {
			t.Fatalf("writing %s: %v", oauthVectorsFile, writeErr)
		}
		return
	}

	stored, err := os.ReadFile(oauthVectorsFile)
	if err != nil {
		t.Fatalf("reading %s: %v (regenerate with -update-oauth-vectors)", oauthVectorsFile, err)
	}
	if string(stored) != string(encoded) {
		t.Fatalf("%s is stale: Go's BuildAuthURL no longer produces it.\n"+
			"Regenerate with: go test ./desktop/parity/ -run TestOAuthVectors -update-oauth-vectors",
			oauthVectorsFile)
	}
}

// TestOAuthScopeUnionIsOrderedNotSetLike pins the property the vectors above
// can only sample: `Scopes` walks calendar, then gmail, then drive, and
// deduplicates — so the scope *string* is deterministic even though `services`
// is a Go map with random iteration order.
//
// Without this, a port could pass every vector by sorting the scopes and still
// diverge on a services map the vectors do not contain.
func TestOAuthScopeUnionIsOrderedNotSetLike(t *testing.T) {
	all := map[string]config.ServiceConfig{
		"drive": enabled(), "gmail": enabled(), "calendar": enabled(),
	}
	want := []string{
		"https://www.googleapis.com/auth/calendar",
		"https://www.googleapis.com/auth/gmail.send",
		"https://www.googleapis.com/auth/gmail.readonly",
		"https://www.googleapis.com/auth/drive",
	}
	// Repeated because the divergence this guards against is a map order that
	// happens to agree once.
	for i := 0; i < 50; i++ {
		got := googleintegration.Scopes(all)
		if len(got) != len(want) {
			t.Fatalf("scope count: got %d, want %d", len(got), len(want))
		}
		for j := range want {
			if got[j] != want[j] {
				t.Fatalf("scope %d: got %q, want %q", j, got[j], want[j])
			}
		}
	}
}
