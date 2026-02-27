# Agento

**Agento** is a local platform for building and interacting with AI agents through a web UI and CLI. It runs on top of the [Claude Code CLI](https://claude.ai/code) already installed on your machine — no separate API key or cloud account needed.

You can define agents with custom system prompts and tools, start multi-turn conversations with them, and manage everything from a browser or directly from your terminal.

---

## Features

- **Web UI + CLI** — Access agents from a browser or run one-shot queries from the terminal
- **Agent builder** — Define agents declaratively with YAML: name, system prompt, model, thinking mode, and tools
- **Multi-turn conversations** — Resume sessions using session IDs
- **Extended thinking** — Control Claude's reasoning depth per agent (`adaptive`, `enabled`, `disabled`)
- **Template variables** — Inject `{{current_date}}`, `{{current_time}}`, and custom values into system prompts
- **Built-in Claude Code tools** — `Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `WebFetch`, `WebSearch`, `Task`
- **External MCP servers** — Connect any MCP-compatible server via `stdio`, `streamable_http`, or `sse`
- **Real-time streaming** — Responses stream live in the UI via Server-Sent Events

---

## Requirements

- [Claude Code CLI](https://claude.ai/code) installed and authenticated on your machine
- Go 1.24+ and Node.js *(only required when building from source)*

No Anthropic API key is required. Agento uses the Claude Code CLI's existing authentication by default. If you prefer to call the Anthropic API directly, you can set `ANTHROPIC_API_KEY` as an optional override.

---

## Installation

### Download binary

Download the latest release for your platform from [GitHub Releases](https://github.com/shaharia-lab/agento/releases):

| Platform | File |
|----------|------|
| Linux (x86_64) | `agento_Linux_x86_64.tar.gz` |
| Linux (arm64) | `agento_Linux_arm64.tar.gz` |
| macOS Intel | `agento_Darwin_x86_64.tar.gz` |
| macOS Apple Silicon | `agento_Darwin_arm64.tar.gz` |
| Windows (x86_64) | `agento_Windows_x86_64.zip` |

Extract the archive and move the `agento` binary to a directory in your `PATH`:

```bash
tar -xzf agento_Linux_x86_64.tar.gz
sudo mv agento /usr/local/bin/
```

### Homebrew

```bash
brew install shaharia-lab/tap/agento
```

### Build from source

Requires Go 1.24+ and Node.js.

```bash
git clone https://github.com/shaharia-lab/agento.git
cd agento
make build
```

This produces an `agento` binary in the project root.

---

## Quick Start

```bash
agento web
```

This starts Agento on port **8990** and opens your browser automatically. To skip the browser:

```bash
agento web --no-browser
```

To use a different port:

```bash
agento web --port 3000
```

---

## Configuration

No configuration is required. Agento works out of the box using your local Claude Code setup.

All settings are optional and can be overridden with environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `8990` | HTTP server port |
| `AGENTO_DATA_DIR` | `~/.agento` | Root directory for agents, chats, and logs |
| `LOG_LEVEL` | `info` | Log verbosity: `debug`, `info`, `warn`, `error` |
| `ANTHROPIC_API_KEY` | — | Use the Anthropic API directly instead of Claude Code CLI authentication |
| `AGENTO_DEFAULT_MODEL` | *(Claude default)* | Lock the Claude model used for direct chat sessions |
| `AGENTO_WORKING_DIR` | `/tmp/agento/work` | Default working directory for agent sessions |

### Logs

All logs are written in JSON format to `~/.agento/logs/system.log` (or `$AGENTO_DATA_DIR/logs/system.log`). Per-session logs are stored at `~/.agento/logs/sessions/<session-id>.log`.

Set `LOG_LEVEL=debug` to include HTTP request logs.

---

## Agents

Agents are specialized assistants with a custom system prompt, model, and set of tools. You create and manage them from the **Agents** page in the UI, then chat with them from the **Chats** page.

### Agent Definition

1. Open the UI and go to the **Agents** page.
2. Click **New Agent**.
3. Fill in the name, description, and system prompt.
4. Choose the Claude model and thinking mode.
5. Select which tools the agent can use.
6. Click **Save**.

Agents are stored as YAML files in `~/.agento/agents/`. You can also create or edit them directly:

```yaml
name: My Assistant
slug: my-assistant
description: A helpful assistant.
model: claude-sonnet-4-6
thinking: adaptive          # adaptive | enabled | disabled

system_prompt: |
  You are a helpful assistant.
  Today is {{current_date}}.

capabilities:
  built_in:                 # Claude Code built-in tools
    - Read
    - Write
    - Bash
    - WebSearch
  local:                    # Local in-process tools
    - current_time
  mcp:                      # External MCP servers
    my-server:
      tools:
        - tool_name
```

**Available built-in tools:** `Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `WebFetch`, `WebSearch`, `Task`

**Thinking modes:**
- `adaptive` — Claude decides when to use extended thinking
- `enabled` — Extended thinking always on
- `disabled` — No extended thinking

**Template variables:** `{{current_date}}` (YYYY-MM-DD), `{{current_time}}` (HH:MM:SS), plus any custom variables.

---

## MCP Registry

To connect external MCP servers, create `~/.agento/mcps.yaml`:

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

Use `${ENV:VAR_NAME}` to reference environment variables in the config. Supported transports: `stdio`, `streamable_http`, `sse`.

---

## CLI Usage

### ask

Run a one-shot query from the terminal:

```bash
agento ask "What is the capital of France?"
agento ask --agent my-assistant "Summarise this document"
agento ask --agent my-assistant "Follow up question" <session-id>
```

### update

Update Agento to the latest release:

```bash
agento update        # prompts for confirmation
agento update --yes  # skip confirmation
```

---

## Development

For architecture overview, local development setup, and contribution guidelines, see the [developer documentation](docs/).

---

## License

MIT. Maintained by [Shaharia Lab](https://github.com/shaharia-lab).
