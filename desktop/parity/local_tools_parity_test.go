// Cross-language vectors for the local in-process tools MCP server
// (`internal/tools`, ported in #310 to `desktop/src-tauri/src/native/tools/`).
//
// Two things about that server have to match byte for byte, and neither is
// checkable from one language alone:
//
//   - **The advertised surface.** The server name, the tool names and the
//     reflected input schemas are what `tools/list` hands the CLI, which hands
//     the schema to the model. `mcp__local-tools__current_time` is also in every
//     agent's stored `capabilities.local` allowlist and in every `tool_use`
//     block already written to `chat_messages`, so a rename is not a rename —
//     it is a silent break of agents that exist. The vectors are taken from the
//     **running server** over its real HTTP transport rather than from the
//     source, so a change to the registration path fails here.
//   - **The answer text.** `current_time`'s sentence is what ends up in a stored
//     `tool_result`. It is `time.RFC1123` plus `time.RFC3339` in a named zone,
//     so it depends on the tz database's *abbreviations* — Asia/Kathmandu is
//     `+0545`, Etc/GMT+5 is `-05` — which Go reads from its own tzdata and Rust
//     reads from `chrono-tz`'s. That is exactly the kind of agreement that
//     cannot be assumed.
//
// The Rust half lives in `desktop/src-tauri/src/native/tools/` and reads this
// same file.
//
// Regenerate (only from Go, and only when adding cases):
//
//	go test ./desktop/parity/ -run TestLocalToolsVectors -update-local-tools-vectors
package parity

import (
	"context"
	"encoding/json"
	"flag"
	"os"
	"testing"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"

	"github.com/shaharia-lab/agento/internal/tools"
)

const localToolsVectorsFile = "local_tools_vectors.json"

var updateLocalToolsVectors = flag.Bool("update-local-tools-vectors", false,
	"rewrite local_tools_vectors.json from this Go toolchain")

type localToolVector struct {
	Name          string          `json:"name"`
	QualifiedName string          `json:"qualified_name"`
	Description   string          `json:"description"`
	InputSchema   json.RawMessage `json:"input_schema"`
}

type currentTimeVector struct {
	Timezone string `json:"timezone"`
	At       string `json:"at"`
	Want     string `json:"want,omitempty"`
	Error    string `json:"error,omitempty"`
}

type localToolsVectors struct {
	Comment     []string            `json:"_comment"`
	ServerName  string              `json:"server_name"`
	Tools       []localToolVector   `json:"tools"`
	CurrentTime []currentTimeVector `json:"current_time"`
}

// currentTimeInstants are two UTC instants six months apart, so every zone below
// is recorded on both sides of its DST rule — which is the only way an
// abbreviation table can be checked rather than sampled.
var currentTimeInstants = []string{
	"2026-08-16T21:07:34Z",
	"2026-01-16T21:07:34Z",
}

// currentTimeZones cover each shape a zone abbreviation takes, plus every branch
// of time.LoadLocation that produces a *different sentence*:
//
//   - alphabetic abbreviations, fixed (Asia/Tokyo) and seasonal (America/New_York)
//   - numeric abbreviations with minutes (Asia/Kathmandu, Pacific/Chatham) and
//     without (Etc/GMT+5, America/Sao_Paulo) — the ones a naive %z would get wrong
//   - a zone whose winter offset is exactly zero (Europe/London), where RFC3339
//     renders `Z` rather than `+00:00`
//   - the two short-circuits before any lookup: "" and "UTC"
//   - the two failures, which are not the same message: an unknown name, and a
//     name Go rejects as a path traversal
//
// "Local" is deliberately absent: it resolves to the machine's zone, so a vector
// for it would record this machine rather than the contract.
var currentTimeZones = []string{
	"", "UTC", "EST",
	"America/New_York", "America/Sao_Paulo", "Asia/Tokyo", "Asia/Kolkata",
	"Asia/Kathmandu", "Australia/Lord_Howe", "Etc/GMT+5", "Europe/London",
	"Pacific/Chatham",
	"utc", "Nowhere/Bad", "..", "a/../b", "/etc/localtime",
}

// listTools dials the real server the way the CLI does and returns what it
// advertises.
func listTools(t *testing.T) []*mcp.Tool {
	t.Helper()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	cfg, err := tools.StartLocalMCPServer(ctx)
	if err != nil {
		t.Fatalf("starting the local tools server: %v", err)
	}

	client := mcp.NewClient(&mcp.Implementation{Name: "parity", Version: "1"}, nil)
	session, err := client.Connect(ctx, &mcp.StreamableClientTransport{Endpoint: cfg.ServerCfg.URL}, nil)
	if err != nil {
		t.Fatalf("connecting to %s: %v", cfg.ServerCfg.URL, err)
	}
	defer func() { _ = session.Close() }()

	listed, err := session.ListTools(ctx, nil)
	if err != nil {
		t.Fatalf("tools/list: %v", err)
	}
	return listed.Tools
}

func TestLocalToolsVectors(t *testing.T) {
	want := localToolsVectors{
		Comment: []string{
			"Cross-language parity vectors for internal/tools, the local in-process MCP server.",
			"Generated from Go, then frozen. Read by desktop/parity/local_tools_parity_test.go",
			"(Go) and by desktop/src-tauri/src/native/tools/ (Rust). Every value is exactly what",
			"Go produces, so a divergence fails one language against the other's real output",
			"rather than against a belief about it.",
			"'tools' is taken from a live tools/list over the server's own HTTP transport.",
			"'current_time' is FormatCurrentTime at a fixed instant, since time.Now() would",
			"pin one moment on one machine.",
			"Regenerate with: go test ./desktop/parity/ -run TestLocalToolsVectors -update-local-tools-vectors",
		},
		ServerName: tools.LocalMCPServerName,
	}

	cfg := &tools.LocalMCPConfig{}
	for _, tool := range listTools(t) {
		schema, err := json.Marshal(tool.InputSchema)
		if err != nil {
			t.Fatalf("encoding %s's input schema: %v", tool.Name, err)
		}
		want.Tools = append(want.Tools, localToolVector{
			Name:          tool.Name,
			QualifiedName: cfg.AllowedToolName(tool.Name),
			Description:   tool.Description,
			InputSchema:   schema,
		})
	}

	for _, at := range currentTimeInstants {
		instant, err := time.Parse(time.RFC3339, at)
		if err != nil {
			t.Fatalf("parsing %q: %v", at, err)
		}
		for _, tz := range currentTimeZones {
			vector := currentTimeVector{Timezone: tz, At: at}
			text, err := tools.FormatCurrentTime(tz, instant)
			if err != nil {
				vector.Error = err.Error()
			} else {
				vector.Want = text
			}
			want.CurrentTime = append(want.CurrentTime, vector)
		}
	}

	encoded, err := json.MarshalIndent(want, "", "  ")
	if err != nil {
		t.Fatalf("encoding vectors: %v", err)
	}
	encoded = append(encoded, '\n')

	if *updateLocalToolsVectors {
		if err := os.WriteFile(localToolsVectorsFile, encoded, 0o600); err != nil {
			t.Fatalf("writing %s: %v", localToolsVectorsFile, err)
		}
		t.Logf("wrote %s", localToolsVectorsFile)
		return
	}

	frozen, err := os.ReadFile(localToolsVectorsFile)
	if err != nil {
		t.Fatalf("reading %s (regenerate with -update-local-tools-vectors): %v",
			localToolsVectorsFile, err)
	}
	if string(frozen) != string(encoded) {
		t.Fatalf("%s is stale: this Go toolchain produces different results.\n"+
			"Regenerate with -update-local-tools-vectors and check what moved — the Rust "+
			"port in native/tools/ reads the same file and will fail against it. A moved "+
			"tool name or server name is not a cosmetic diff: it is in every agent's stored "+
			"allowlist and in every tool_use block already written.",
			localToolsVectorsFile)
	}
}
