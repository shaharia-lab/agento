package config

import (
	"fmt"
	"os"
	"strings"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
	"gopkg.in/yaml.v3"
)

// rawMCPEntry is used for initial YAML parsing before transport-specific typing.
type rawMCPEntry struct {
	Transport string            `yaml:"transport"`
	Command   string            `yaml:"command"`
	Args      []string          `yaml:"args"`
	Env       map[string]string `yaml:"env"`
	URL       string            `yaml:"url"`
	Headers   map[string]string `yaml:"headers"`
}

// MCPRegistry holds the parsed MCP server configurations.
type MCPRegistry struct {
	servers map[string]any // values are claude.McpStdioServer, claude.McpHTTPServer, or claude.McpSSEServer
}

// Has reports whether the registry contains a server with the given name.
func (r *MCPRegistry) Has(name string) bool {
	_, ok := r.servers[name]
	return ok
}

// GetSDKConfig returns the SDK-compatible MCP server config for the given server name,
// or nil if not found.
func (r *MCPRegistry) GetSDKConfig(name string) any {
	return r.servers[name]
}

// All returns the full map of server name → SDK config. Used when building MCP server
// maps for agent invocations.
func (r *MCPRegistry) All() map[string]any {
	out := make(map[string]any, len(r.servers))
	for k, v := range r.servers {
		out[k] = v
	}
	return out
}

// LoadMCPRegistry reads the MCP registry YAML file at filePath and returns a populated
// MCPRegistry. If the file does not exist, an empty registry is returned (not an error).
func LoadMCPRegistry(filePath string) (*MCPRegistry, error) {
	data, err := os.ReadFile(filePath) //nolint:gosec // path is from admin-configured data dir
	if err != nil {
		if os.IsNotExist(err) {
			return &MCPRegistry{servers: make(map[string]any)}, nil
		}
		return nil, fmt.Errorf("reading MCP registry %q: %w", filePath, err)
	}

	var raw map[string]rawMCPEntry
	if err := yaml.Unmarshal(data, &raw); err != nil {
		return nil, fmt.Errorf("parsing MCP registry %q: %w", filePath, err)
	}

	registry := &MCPRegistry{servers: make(map[string]any)}

	for name, entry := range raw {
		switch entry.Transport {
		case "stdio":
			env, err := interpolateEnvMap(name, entry.Env)
			if err != nil {
				return nil, err
			}
			registry.servers[name] = claude.McpStdioServer{
				Type:    "stdio",
				Command: entry.Command,
				Args:    entry.Args,
				Env:     env,
			}

		case "streamable_http":
			headers, err := interpolateEnvMap(name, entry.Headers)
			if err != nil {
				return nil, err
			}
			registry.servers[name] = claude.McpHTTPServer{
				Type:    "http",
				URL:     entry.URL,
				Headers: headers,
			}

		case "sse":
			headers, err := interpolateEnvMap(name, entry.Headers)
			if err != nil {
				return nil, err
			}
			registry.servers[name] = claude.McpSSEServer{
				Type:    "sse",
				URL:     entry.URL,
				Headers: headers,
			}

		default:
			return nil, fmt.Errorf("MCP server %q: unknown transport %q (must be stdio, streamable_http, or sse)", name, entry.Transport)
		}
	}

	return registry, nil
}

// interpolateEnvMap applies ${ENV:VAR_NAME} substitution to all values in m.
func interpolateEnvMap(serverName string, m map[string]string) (map[string]string, error) {
	if len(m) == 0 {
		return m, nil
	}
	out := make(map[string]string, len(m))
	for k, v := range m {
		interpolated, err := interpolateEnv(v)
		if err != nil {
			return nil, fmt.Errorf("MCP server %q key %q: %w", serverName, k, err)
		}
		out[k] = interpolated
	}
	return out, nil
}

// interpolateEnv replaces all ${ENV:VAR_NAME} patterns in s with the corresponding
// environment variable values. Returns an error if a referenced variable is not set.
func interpolateEnv(s string) (string, error) {
	result := s
	for {
		start := strings.Index(result, "${ENV:")
		if start == -1 {
			break
		}
		end := strings.Index(result[start:], "}")
		if end == -1 {
			break
		}
		end += start
		varName := result[start+6 : end]
		value := os.Getenv(varName)
		if value == "" {
			return "", fmt.Errorf("required env var %q is not set", varName)
		}
		result = result[:start] + value + result[end+1:]
	}
	return result, nil
}
