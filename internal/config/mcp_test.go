package config

import (
	"os"
	"path/filepath"
	"testing"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestMCPRegistry_Has(t *testing.T) {
	r := &MCPRegistry{servers: map[string]any{
		"server1": claude.McpStdioServer{},
	}}
	assert.True(t, r.Has("server1"))
	assert.False(t, r.Has("missing"))
}

func TestMCPRegistry_GetSDKConfig(t *testing.T) {
	cfg := claude.McpStdioServer{Command: "test"}
	r := &MCPRegistry{servers: map[string]any{"s": cfg}}

	assert.Equal(t, cfg, r.GetSDKConfig("s"))
	assert.Nil(t, r.GetSDKConfig("missing"))
}

func TestMCPRegistry_All(t *testing.T) {
	r := &MCPRegistry{servers: map[string]any{
		"a": claude.McpStdioServer{},
		"b": claude.McpHTTPServer{},
	}}
	all := r.All()
	assert.Len(t, all, 2)
	// Mutating the returned map shouldn't affect the registry
	all["c"] = nil
	assert.False(t, r.Has("c"))
}

func TestLoadMCPRegistry(t *testing.T) {
	tests := []struct {
		name    string
		yaml    string
		check   func(t *testing.T, r *MCPRegistry)
		wantErr string
	}{
		{
			name: "stdio server",
			yaml: `
server1:
  transport: stdio
  command: node
  args: ["--flag"]
`,
			check: func(t *testing.T, r *MCPRegistry) {
				assert.True(t, r.Has("server1"))
				cfg := r.GetSDKConfig("server1")
				stdio, ok := cfg.(claude.McpStdioServer)
				require.True(t, ok)
				assert.Equal(t, "stdio", stdio.Type)
				assert.Equal(t, "node", stdio.Command)
				assert.Equal(t, []string{"--flag"}, stdio.Args)
			},
		},
		{
			name: "http server",
			yaml: `
httpserver:
  transport: streamable_http
  url: http://localhost:3000
`,
			check: func(t *testing.T, r *MCPRegistry) {
				cfg := r.GetSDKConfig("httpserver")
				http, ok := cfg.(claude.McpHTTPServer)
				require.True(t, ok)
				assert.Equal(t, "http", http.Type)
				assert.Equal(t, "http://localhost:3000", http.URL)
			},
		},
		{
			name: "sse server",
			yaml: `
sseserver:
  transport: sse
  url: http://localhost:4000/sse
`,
			check: func(t *testing.T, r *MCPRegistry) {
				cfg := r.GetSDKConfig("sseserver")
				sse, ok := cfg.(claude.McpSSEServer)
				require.True(t, ok)
				assert.Equal(t, "sse", sse.Type)
				assert.Equal(t, "http://localhost:4000/sse", sse.URL)
			},
		},
		{
			name:    "unknown transport",
			yaml:    "bad:\n  transport: grpc\n",
			wantErr: "unknown transport",
		},
		{
			name:    "invalid yaml",
			yaml:    "{{invalid",
			wantErr: "parsing MCP registry",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			fp := filepath.Join(dir, "mcps.yaml")
			require.NoError(t, os.WriteFile(fp, []byte(tt.yaml), 0o600))

			r, err := LoadMCPRegistry(fp)
			if tt.wantErr != "" {
				assert.ErrorContains(t, err, tt.wantErr)
				return
			}
			require.NoError(t, err)
			tt.check(t, r)
		})
	}
}

func TestLoadMCPRegistry_FileNotExist(t *testing.T) {
	r, err := LoadMCPRegistry("/nonexistent/mcps.yaml")
	require.NoError(t, err)
	assert.NotNil(t, r)
	assert.Empty(t, r.All())
}

func TestInterpolateEnv(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		envVars map[string]string
		want    string
		wantErr string
	}{
		{
			name:  "no interpolation",
			input: "plain value",
			want:  "plain value",
		},
		{
			name:    "single var",
			input:   "${ENV:MY_VAR}",
			envVars: map[string]string{"MY_VAR": "hello"},
			want:    "hello",
		},
		{
			name:    "var in middle",
			input:   "prefix-${ENV:MY_VAR}-suffix",
			envVars: map[string]string{"MY_VAR": "mid"},
			want:    "prefix-mid-suffix",
		},
		{
			name:    "multiple vars",
			input:   "${ENV:A}:${ENV:B}",
			envVars: map[string]string{"A": "x", "B": "y"},
			want:    "x:y",
		},
		{
			name:    "missing env var",
			input:   "${ENV:MISSING_VAR}",
			wantErr: "required env var",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			for k, v := range tt.envVars {
				t.Setenv(k, v)
			}
			got, err := interpolateEnv(tt.input)
			if tt.wantErr != "" {
				assert.ErrorContains(t, err, tt.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestLoadMCPRegistry_WithEnvInterpolation(t *testing.T) {
	t.Setenv("TEST_API_KEY", "secret123")

	dir := t.TempDir()
	fp := filepath.Join(dir, "mcps.yaml")
	yaml := `
server:
  transport: stdio
  command: test
  env:
    API_KEY: "${ENV:TEST_API_KEY}"
`
	require.NoError(t, os.WriteFile(fp, []byte(yaml), 0o600))

	r, err := LoadMCPRegistry(fp)
	require.NoError(t, err)

	cfg := r.GetSDKConfig("server")
	stdio, ok := cfg.(claude.McpStdioServer)
	require.True(t, ok)
	assert.Equal(t, "secret123", stdio.Env["API_KEY"])
}
