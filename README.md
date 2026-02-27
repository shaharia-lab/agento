# Agento

**Agento** is a Go-based platform for defining and running AI agents powered by [Anthropic's Claude](https://www.anthropic.com/claude) models. It provides a flexible, configuration-driven framework for creating agents with customizable capabilities — including built-in Claude Code tools, local in-process tools, and external [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) servers.

## Features

- **Dual interface**: Run agents via CLI or expose them through an HTTP REST API
- **YAML-driven agents**: Define agents declaratively — no code required for common use cases
- **Multi-turn conversations**: Resume sessions across requests using session IDs
- **Extended thinking**: Control Claude's thinking mode per agent or per request (`adaptive`, `enabled`, `disabled`)
- **Template variables**: Inject dynamic values (date, time, custom variables) into system prompts
- **Three-tier tool system**:
  - Built-in Claude Code tools (`Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `WebFetch`, `WebSearch`, `Task`)
  - Local in-process tools via MCP (e.g., `current_time`)
  - External MCP servers via `stdio`, `streamable_http`, or `sse` transports
- **Streaming support**: Server-Sent Events (SSE) for real-time streaming responses
- **Token usage and cost tracking**: Response metadata includes token counts and estimated USD cost

## Requirements

- Go 1.24+
- An [Anthropic API key](https://console.anthropic.com/)

## Installation

Clone the repository and build the binary:

```bash
git clone git@github.com:shaharia-lab/agento.git
cd agento
make build
```

This produces an `agents-platform` binary in the project root.

## Configuration

### Environment Variables

Copy `.env.example` and fill in your values:

```bash
cp .env.example .env
```

| Variable | Required | Default | Description |
|---|---|---|---|
| `ANTHROPIC_API_KEY` | Yes | — | Your Anthropic API key |
| `AGENTS_DIR` | No | `./agents` | Directory containing agent YAML files |
| `MCPS_FILE` | No | `./mcps.yaml` | Path to the MCP registry YAML file |
| `PORT` | No | `3000` | HTTP server port (for `serve` command) |

### Agent Definitions

Agents are defined as YAML files in the agents directory (default: `./agents`). Each file defines one agent.

```yaml
name: My Assistant
slug: my-assistant          # Used in API URLs; auto-derived from filename if omitted
description: A helpful assistant for general questions.
model: claude-sonnet-4-6    # Claude model to use (default: claude-sonnet-4-6)
thinking: adaptive          # adaptive | enabled | disabled (default: adaptive)

system_prompt: |
  You are a helpful assistant.
  Today's date is {{current_date}}.
  Current time: {{current_time}}.
  User context: {{user_context}}.

capabilities:
  built_in:                 # Claude Code built-in tools
    - Read
    - Write
    - Bash
    - WebSearch
  local:                    # Local in-process tools
    - current_time
  mcp:                      # External MCP servers (keys must match mcps.yaml)
    my-external-server:
      tools:
        - tool_name_1
        - tool_name_2
```

**Available built-in tools**: `Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `WebFetch`, `WebSearch`, `Task`

**Thinking modes**:
- `adaptive` — Claude decides when to use extended thinking
- `enabled` — Extended thinking always on
- `disabled` — No extended thinking

**Template variables**:
- `{{current_date}}` — Current date in `YYYY-MM-DD` format
- `{{current_time}}` — Current time in `HH:MM:SS` format
- Any custom variable passed at runtime via `--variables` or the API request body

### MCP Registry

Define external MCP servers in `mcps.yaml`. Use `${ENV:VAR_NAME}` to reference environment variables:

```yaml
# stdio-based MCP server (subprocess)
my-stdio-server:
  transport: stdio
  command: /path/to/mcp-server
  args:
    - --config
    - /path/to/config.json
  env:
    API_KEY: ${ENV:MY_SERVER_API_KEY}

# HTTP-based MCP server
my-http-server:
  transport: streamable_http
  url: https://api.example.com/mcp
  headers:
    Authorization: Bearer ${ENV:MY_HTTP_TOKEN}

# SSE-based MCP server
my-sse-server:
  transport: sse
  url: https://stream.example.com/mcp
```

## Usage

### CLI

```bash
# Ask a question using the first available agent
./agents-platform ask "What is the capital of France?"

# Use a specific agent by slug
./agents-platform ask --agent my-assistant "Summarize the news today."

# Resume a multi-turn conversation
./agents-platform ask --agent my-assistant "Follow up question" <session-id>

# Disable extended thinking for this request
./agents-platform ask --agent my-assistant --no-thinking "Quick calculation: 2+2"

# Override agents directory and MCP registry file
./agents-platform ask --agents-dir /path/to/agents --mcps-file /path/to/mcps.yaml "Hello"
```

Thinking tokens are written to **stderr**; the final response text goes to **stdout**. The session ID and token usage statistics are printed after each response.

### HTTP Server

Start the server:

```bash
./agents-platform serve
./agents-platform serve --port 8080
./agents-platform serve --agents-dir ./my-agents --mcps-file ./my-mcps.yaml
```

#### Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `GET` | `/agents` | List all loaded agents |
| `POST` | `/{slug}/ask` | Run agent and return full JSON response |
| `POST` | `/{slug}/ask/stream` | Run agent with Server-Sent Events (SSE) streaming |

#### Request Body (`/ask` and `/ask/stream`)

```json
{
  "question": "What is quantum entanglement?",
  "session_id": "optional-uuid-to-resume-session",
  "thinking": true,
  "variables": {
    "user_context": "Professional physicist"
  }
}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `question` | string | Yes | The question or instruction for the agent |
| `session_id` | string | No | Resume a previous conversation session |
| `thinking` | boolean | No | Override agent thinking config (`null` = use agent config) |
| `variables` | object | No | Template variable values for the system prompt |

#### Response (`/ask`)

```json
{
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "question": "What is quantum entanglement?",
  "answer": "Quantum entanglement is a phenomenon where...",
  "thinking": "Let me think about how to explain this clearly...",
  "cost_usd": 0.000342,
  "usage": {
    "input_tokens": 210,
    "output_tokens": 148,
    "cache_read_input_tokens": 0,
    "cache_creation_input_tokens": 0
  }
}
```

#### SSE Stream Events (`/ask/stream`)

Events are sent as `data: <json>\n\n` in the following format:

| Event type | Description |
|---|---|
| `thinking` | Streaming thinking token |
| `message` | Streaming response text chunk |
| `result` | Final aggregated result (same schema as `/ask` response) |
| `error` | Error message |
| `done` | Stream completion signal |

Example event:
```
data: {"type":"message","content":"Quantum entanglement is"}

data: {"type":"done"}
```

## Project Structure

```
agento/
├── agents/                    # Agent YAML definitions
│   ├── hello-world.yaml
│   └── hello-world-2.yaml
├── cmd/                       # CLI commands (cobra)
│   ├── root.go               # Root command setup
│   ├── ask.go                # "ask" subcommand
│   └── serve.go              # "serve" subcommand
├── internal/
│   ├── agent/
│   │   └── runner.go         # Agent execution engine
│   ├── config/
│   │   ├── agent.go          # Agent YAML parsing and registry
│   │   └── mcp.go            # MCP registry loading
│   ├── server/
│   │   └── server.go         # HTTP server and API handlers
│   └── tools/
│       ├── registry.go       # Local in-process MCP server
│       └── current_time.go   # Built-in current_time tool
├── main.go                    # Entry point
├── go.mod
├── Makefile
└── .env.example
```

## Development

```bash
# Install dependencies
make tidy

# Build
make build

# Run tests
make test

# Lint
make lint

# Run the HTTP server directly
make run-serve

# Run the ask command
make run-ask ARGS='--agent hello-world "What time is it?"'

# Clean build artifacts
make clean
```

## Adding a Custom Local Tool

Local tools run in-process via an embedded MCP server. To add one:

1. Create a new file in `internal/tools/` implementing your tool logic.
2. Register it in `internal/tools/registry.go` alongside `current_time`.
3. Add the tool name to an agent's `capabilities.local` list.

See `internal/tools/current_time.go` for a reference implementation.

## Adding an External MCP Server

1. Add the server configuration to `mcps.yaml` (or the file pointed to by `MCPS_FILE`).
2. Reference the server key in the agent's `capabilities.mcp` section.
3. List the specific tools your agent is allowed to use.

MCP tool names are automatically qualified as `mcp__{server_name}__{tool_name}` internally.

## Example Agents

### Hello World (local tool)

`agents/hello-world.yaml` — Uses the built-in `current_time` local tool and adaptive thinking:

```yaml
name: Hello World Agent
slug: hello-world
description: A simple demo agent that answers general questions concisely.
model: claude-sonnet-4-6
thinking: adaptive

system_prompt: |
  You are a helpful assistant.
  Today's date is {{current_date}}.
  Answer concisely and accurately.

capabilities:
  local:
    - current_time
```

### Hello World 2 (web search)

`agents/hello-world-2.yaml` — Uses the `WebSearch` built-in tool, thinking disabled:

```yaml
name: Hello World Agent 2
description: A second demo agent — greets users warmly and answers questions.
model: claude-sonnet-4-6
thinking: disabled

system_prompt: |
  You are a warm and friendly assistant named Alex.
  Today's date is {{current_date}}.
  Always greet the user before answering.

capabilities:
  built_in:
    - WebSearch
```

## License

This project is maintained by [Shaharia Lab](https://github.com/shaharia-lab).
