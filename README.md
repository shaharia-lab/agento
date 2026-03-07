# Agento

[![Bugs](https://sonarcloud.io/api/project_badges/measure?project=shaharia-lab_agento&metric=bugs)](https://sonarcloud.io/summary/new_code?id=shaharia-lab_agento)
[![Lines of Code](https://sonarcloud.io/api/project_badges/measure?project=shaharia-lab_agento&metric=ncloc)](https://sonarcloud.io/summary/new_code?id=shaharia-lab_agento)
[![Maintainability Rating](https://sonarcloud.io/api/project_badges/measure?project=shaharia-lab_agento&metric=sqale_rating)](https://sonarcloud.io/summary/new_code?id=shaharia-lab_agento)
[![Reliability Rating](https://sonarcloud.io/api/project_badges/measure?project=shaharia-lab_agento&metric=reliability_rating)](https://sonarcloud.io/summary/new_code?id=shaharia-lab_agento)
[![Security Rating](https://sonarcloud.io/api/project_badges/measure?project=shaharia-lab_agento&metric=security_rating)](https://sonarcloud.io/summary/new_code?id=shaharia-lab_agento)
[![Vulnerabilities](https://sonarcloud.io/api/project_badges/measure?project=shaharia-lab_agento&metric=vulnerabilities)](https://sonarcloud.io/summary/new_code?id=shaharia-lab_agento)
[![Release](https://img.shields.io/github/v/release/shaharia-lab/agento)](https://github.com/shaharia-lab/agento/releases)

<img width="1820" height="922" alt="carbon (4)" src="https://github.com/user-attachments/assets/62ce9188-2aeb-4ec3-847c-3b50346d0adb" />

**Agento** is a local platform for building and interacting with AI agents through a web UI and CLI. It runs on top of the [Claude Code CLI](https://claude.ai/code) already installed on your machine — no separate API key or cloud account needed.

You can define agents with custom system prompts and tools, start multi-turn conversations with them, and manage everything from a browser or directly from your terminal.

---

## ✨ Features

- **Web UI + CLI** — Access agents from a browser or run one-shot queries from the terminal
- **Agent builder** — Define agents with custom system prompts, models, thinking modes, and tools via the UI or YAML
- **Multi-turn conversations** — Resume sessions using session IDs
- **Extended thinking** — Control Claude's reasoning depth per agent (`adaptive`, `enabled`, `disabled`)
- **Template variables** — Inject `{{current_date}}`, `{{current_time}}`, and custom values into system prompts
- **Built-in Claude Code tools** — `Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `WebFetch`, `WebSearch`, `Task`
- **External MCP servers** — Connect any MCP-compatible server via `stdio`, `streamable_http`, or `sse`
- **Real-time streaming** — Responses stream live in the UI via Server-Sent Events
- **Integrations** — Connect Google (Calendar, Gmail, Drive), GitHub, Slack, Jira, Confluence, and Telegram as agent tools
- **Task scheduler** — Schedule recurring agent tasks with cron expressions and track job history
- **Notifications** — Event-driven notification system with SMTP email support
- **Observability** — OpenTelemetry traces, metrics, and logs with OTLP and Prometheus exporters, configurable via UI or environment variables
- **Auto-update check** — Banner notification when a newer release is available, with one-command update

---

## 🎬 Demo

See Agento in action across its core features. Click any section below to expand the demo.

<details>
<summary><strong>💬 Chat with an AI Agent</strong></summary>
<br>

Start a multi-turn conversation with any agent you've built. Responses stream live in the browser via Server-Sent Events, so you see output as it's generated — just like a terminal.

[demo.webm](https://github.com/user-attachments/assets/1fa2b716-cbb8-459e-b2e1-f6c252c086c2)

</details>

<details>
<summary><strong>🗂️ Multi-Chat Workspace</strong></summary>
<br>

Open multiple conversations side by side using tabbed workspaces. Each tab maintains its own session state so you can run independent agent tasks in parallel without losing context.

[Multi-Tab Chat - Demo.webm](https://github.com/user-attachments/assets/91794133-9f90-4eb0-a62c-885be70b3c39)

</details>

<details>
<summary><strong>⭐ Favorites & Chat Title</strong></summary>
<br>

Mark important conversations as favorites and rename chat titles inline — keeping your workspace organized without leaving the chat view.

[Favorite and Chat Title Demo.webm](https://github.com/user-attachments/assets/9a4892c6-eee5-46c4-af1a-eeb65e613aa3)

</details>

<details>
<summary><strong>🔌 Integrations (Google, GitHub, Slack, and more)</strong></summary>
<br>

Connect external services — Google Calendar, Gmail, Drive, GitHub, Slack, Jira, Confluence, and Telegram — and expose them as tools your agents can call during a conversation.

<img width="1212" height="361" alt="Integrations" src="https://github.com/user-attachments/assets/6dcb9079-a7ff-47b3-9096-54de2314d544" />

</details>

<details>
<summary><strong>⏰ Task Scheduler</strong></summary>
<br>

Schedule agents to run automatically on a cron expression. Each run is logged with full job history so you can review what the agent did, when it ran, and whether it succeeded.

<img width="1490" height="962" alt="Scheduled Task" src="https://github.com/user-attachments/assets/b5fafaa7-5f3e-4c0c-9b4d-ba85dbcb28cf" />

</details>

<details>
<summary><strong>📊 Token Usage Analytics</strong></summary>
<br>

Track token consumption, cache hit rates, and estimated costs across all your Claude sessions. Charts break down usage by model and time range so you always know where tokens are going.

<img width="1202" height="1545" alt="Token Usage" src="https://github.com/user-attachments/assets/29bcadf2-56ef-4728-badf-c01e4c71a860" />

</details>

<details>
<summary><strong>📈 General Usage Analytics</strong></summary>
<br>

Visualize session volume over time, per-model breakdowns, and activity heatmaps. A quick way to understand how heavily your agents are being used and when.

<img width="1201" height="1327" alt="General Usage" src="https://github.com/user-attachments/assets/e21522b4-7ee9-4728-bb79-a62857d619ae" />

</details>

<details>
<summary><strong>🗄️ Claude Code Session Journey</strong></summary>
<br>

Browse and replay your Claude Code session history with a full timeline of tool calls, messages, and token usage — useful for auditing what an agent actually did during a run.

<img width="1490" height="809" alt="Claude Code Session Journey" src="https://github.com/user-attachments/assets/b45a5cdb-93d9-482a-9b08-b3394907ba40" />

</details>

<details>
<summary><strong>📡 Monitoring & Observability</strong></summary>
<br>

Configure OpenTelemetry exporters for traces, metrics, and logs directly from the Settings UI — no config files needed. Works with any OTLP-compatible collector or Prometheus.

<img width="1271" height="682" alt="Monitoring" src="https://github.com/user-attachments/assets/c515cc62-3070-4576-b20b-86552df074e7" />

</details>

<details>
<summary><strong>🔔 Notifications</strong></summary>
<br>

Set up SMTP email notifications for task completions and agent events. Configure recipients, test delivery, and review the notification log — all from the Settings page.

<img width="1203" height="747" alt="Notifications" src="https://github.com/user-attachments/assets/5451a9ce-82a6-40fa-b86b-f298ab062abb" />

</details>

<details>
<summary><strong>🎨 Appearance Settings</strong></summary>
<br>

Switch between light and dark themes and customize the UI appearance to match your workflow preferences, with changes applied instantly across the entire app.

<img width="731" height="634" alt="Appearance Settings" src="https://github.com/user-attachments/assets/b73b2d73-00ad-4b2a-84d6-ad70785898ae" />

</details>

---

## 📋 Requirements

- [Claude Code CLI](https://claude.ai/code) installed and authenticated on your machine
- Go 1.25+ and Node.js *(only required when building from source)*

No Anthropic API key is required. Agento uses the Claude Code CLI's existing authentication by default. If you prefer to call the Anthropic API directly, you can set `ANTHROPIC_API_KEY` as an optional override.

---

## 📦 Installation

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

Requires Go 1.25+ and Node.js.

```bash
git clone https://github.com/shaharia-lab/agento.git
cd agento
make build
```

This produces an `agento` binary in the project root.

---

## 🚀 Quick Start

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

## ⚙️ Configuration

No configuration is required. Agento works out of the box using your local Claude Code setup.

All settings are optional and can be overridden with environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `PORT` | `8990` | HTTP server port |
| `AGENTO_DATA_DIR` | `~/.agento` | Root directory for agents, chats, and database. Supports `~` expansion (e.g. `~/.agento-dev`) |
| `LOG_LEVEL` | `info` | Log verbosity: `debug`, `info`, `warn`, `error` |
| `ANTHROPIC_API_KEY` | — | Use the Anthropic API directly instead of Claude Code CLI authentication |
| `AGENTO_DEFAULT_MODEL` | *(Claude default)* | Lock the Claude model used for direct chat sessions |
| `AGENTO_WORKING_DIR` | `/tmp/agento/work` | Default working directory for agent sessions |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | — | OTLP gRPC collector endpoint (e.g. `localhost:4317`). Can also be configured via Settings UI |
| `OTEL_METRICS_EXPORTER` | — | `otlp` (push) or `prometheus` (pull via `/metrics`) |
| `OTEL_LOGS_EXPORTER` | — | `otlp` |

### Local Development Isolation

Use `AGENTO_DATA_DIR` to keep development data separate from production:

```bash
# Run against an isolated dev directory — production data is untouched
AGENTO_DATA_DIR=~/.agento-dev make dev-backend
```

The resolved path is logged at startup (`data_dir` field) so you can confirm which directory is in use.

### Logs

All logs are written in JSON format to `~/.agento/logs/system.log` (or `$AGENTO_DATA_DIR/logs/system.log`). Per-session logs are stored at `~/.agento/logs/sessions/<session-id>.log`.

Set `LOG_LEVEL=debug` to include HTTP request logs.

---

## 🤖 Agents

Agents are specialized assistants with a custom system prompt, model, and set of tools. You create and manage them from the **Agents** page in the UI, then chat with them from the **Chats** page.

### Agent Definition

1. Open the UI and go to the **Agents** page.
2. Click **New Agent**.
3. Fill in the name, description, and system prompt.
4. Choose the Claude model and thinking mode.
5. Select which tools the agent can use.
6. Click **Save**.

Agents are stored in the SQLite database at `~/.agento/agento.db`. Legacy YAML files in `~/.agento/agents/` are auto-migrated on first startup. You can also define agents as YAML files:

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

## 🔌 MCP Registry

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

## 💻 CLI Usage

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

## 🛠️ Development

For architecture overview, local development setup, and contribution guidelines, see the [developer documentation](docs/).

---

## 📄 License

MIT. Maintained by [Shaharia Lab](https://github.com/shaharia-lab).
