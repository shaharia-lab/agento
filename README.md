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

<br>

> ⭐ **If Agento is useful to you, consider [starring the repository](https://github.com/shaharia-lab/agento) — it helps others discover the project and keeps us motivated to keep building.**

<br>

## ✨ Features

<details>
<summary><strong>💬 Chats — Multi-turn conversations with your agents</strong></summary>
<br>

Start a conversation with any agent you've built and continue it across multiple turns. Sessions are persisted locally so you can pick up right where you left off. Responses stream live via Server-Sent Events — no waiting for the full reply. You can favorite important chats and rename their titles inline to keep your workspace organized.

</details>

<details>
<summary><strong>🗂️ Multi-Chat Workspace — Run multiple conversations in parallel</strong></summary>
<br>

Open several chat sessions simultaneously in a tabbed workspace. Each tab is fully independent with its own agent, session state, and streaming output. Tab state persists across page reloads, so you never lose your place even if you navigate away.

</details>

<details>
<summary><strong>🤖 Agent Builder — Define custom AI agents</strong></summary>
<br>

Create agents with a custom name, system prompt, Claude model, and thinking mode (`adaptive`, `enabled`, or `disabled`). Assign exactly which tools each agent can access — built-in Claude Code tools, local in-process tools, external MCP servers, or third-party integrations. Agents are saved locally on your machine and can also be defined as YAML files. Template variables like `{{current_date}}` and `{{current_time}}` are automatically injected into system prompts at runtime.

**Built-in tools available:** `Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `WebFetch`, `WebSearch`, `Task`

</details>

<details>
<summary><strong>🔌 Integrations — Connect external services as agent tools</strong></summary>
<br>

Turn external services into tools your agents can call directly during a conversation. Each integration runs as an in-process MCP server with no external daemon required.

Supported integrations:
- **Google** — Calendar (read/create events), Gmail (read/send), Drive (search/read files) via OAuth
- **GitHub** — Repos, issues, pull requests, Actions workflows, and releases via personal access token
- **Slack** — Read channels and messages, send messages, search, look up users via bot token
- **Jira** — Browse projects, search issues with JQL, create and update issues via API token
- **Confluence** — Read spaces and pages, search with CQL via API token
- **Telegram** — Send/receive messages, photos, polls, and locations via bot token

</details>

<details>
<summary><strong>🔗 External MCP Servers — Plug in any MCP-compatible tool server</strong></summary>
<br>

Extend agents with any MCP server by listing it in `~/.agento/mcps.yaml`. Supports all three transports: `stdio` (subprocess), `streamable_http`, and `sse`. Environment variable references (`${ENV:VAR_NAME}`) keep credentials out of config files. Once registered, MCP tools appear alongside built-in tools in the agent builder and can be selectively assigned per agent.

</details>

<details>
<summary><strong>⏰ Task Scheduler — Automate recurring agent jobs</strong></summary>
<br>

Schedule any agent to run automatically on a cron expression. Agento runs tasks in the background and records the outcome, duration, and output of every execution. Use this for daily summaries, automated reports, periodic file processing, or any workflow you'd otherwise run manually on a schedule.

</details>

<details>
<summary><strong>📋 Job History — Audit every scheduled run</strong></summary>
<br>

Every task execution is logged with its start time, duration, exit status, and full agent output. Browse the job history page to review what each agent did, diagnose failures, and track trends over time without digging through log files.

</details>

<details>
<summary><strong>🗄️ Claude Sessions — Browse your Claude Code session history</strong></summary>
<br>

Agento automatically scans the Claude Code session JSONL files on your machine and makes them browsable in the UI. View any session's full message history, drill into individual tool calls, and follow the complete session journey from start to finish. Results are cached locally and updated incrementally in the background as new sessions appear.

</details>

<details>
<summary><strong>📊 Token Usage Analytics — Track cost and consumption over time</strong></summary>
<br>

See exactly how many tokens your Claude sessions are consuming, broken down by input, output, and cache tokens. Charts show trends over your chosen date range, and cost estimates let you stay on top of spending before bills arrive. Filter by model to compare usage across Claude Sonnet, Haiku, and Opus.

</details>

<details>
<summary><strong>📈 General Usage Analytics — Understand your session patterns</strong></summary>
<br>

Visualize how many sessions you're running per day, which models you're using most, and when you're most active via an activity heatmap. Compare any date range to spot growth trends or usage spikes across all your Claude Code work.

</details>

<details>
<summary><strong>💡 Insights — Productivity metrics for your AI workflow <em>(experimental)</em></strong></summary>
<br>

A deeper analytics view that goes beyond raw token counts. Insights computes an **Autonomy Score** (how independently Claude worked, based on human interruptions), a **Productivity Score** (composite of autonomy, cache efficiency, and error-free sessions), average tool call counts, session durations, and a top-10 tool usage breakdown. All metrics support period-over-period comparison so you can see whether your agents are becoming more effective over time.

</details>

<details>
<summary><strong>⚙️ Settings — General, Claude Profiles, Appearance, and more</strong></summary>
<br>

All configuration lives in a single Settings page organized into tabs:

- **General** — Set the default working directory and Claude model. Fields locked by environment variables are shown as read-only with the overriding env var name.
- **Claude Settings Profiles** — Create multiple named Claude settings profiles (each stored as `~/.claude/settings_<slug>.json`) and switch between them per agent or per chat. A default profile is auto-created from your existing `~/.claude/settings.json` on first launch.
- **Appearance** — Toggle dark/light mode, choose font size and font family. Changes apply instantly across the entire UI.
- **Notifications** — Configure SMTP email delivery for task completion and agent events. Test delivery directly from the UI and browse the notification log.
- **Monitoring** — Configure OpenTelemetry exporters (OTLP gRPC or Prometheus) for traces, metrics, and logs. Hot-reload settings without restarting. Fields set via environment variables show a lock indicator and return HTTP 409 if you try to override them via UI.
- **Advanced** — Additional low-level configuration options.

</details>

<details>
<summary><strong>💻 CLI — Run agents directly from the terminal</strong></summary>
<br>

Use `agento ask` to send a one-shot query to any agent without opening the browser. Pass a session ID as a positional argument to continue an existing conversation. Useful for scripting, quick lookups, or integrating Agento into other shell workflows.

```bash
agento ask "What changed in the repo today?"
agento ask --agent my-agent "Follow up" <session-id>
```

</details>

<details>
<summary><strong>🔄 Auto-Update — Stay current with one command</strong></summary>
<br>

Agento checks for newer releases on startup and shows an amber banner at the top of the UI when an update is available. Dismiss it per-version or run `agento update` to upgrade in place. The update check is cached for one hour so it doesn't slow down your workflow.

</details>

<details>
<summary><strong>📡 Observability — OpenTelemetry traces, metrics, and logs</strong></summary>
<br>

Every HTTP request, agent run, tool call, and storage operation is instrumented with OpenTelemetry spans and metrics. Configure OTLP gRPC export or a Prometheus pull endpoint directly from the Monitoring settings tab — no config files or restarts needed. Structured JSON logs are written to `~/.agento/logs/system.log` with per-session logs at `~/.agento/logs/sessions/<id>.log`.

</details>

<br>

> 💡 **Missing a feature?** If there's something you'd like to see in Agento, [open an issue on GitHub](https://github.com/shaharia-lab/agento/issues/new) — we'd love to hear from you.



<br>

## 🎬 Demo

See Agento in action across its core features. Click any section below to expand the demo.

<details>
<summary><strong>💬 Chat with an AI Agent</strong></summary>
<br>

Start a multi-turn conversation with any agent you've built. Responses stream live in the browser via Server-Sent Events, so you see output as it's generated — just like a terminal.

[▶ Watch: Chat with an AI Agent](https://github.com/user-attachments/assets/1fa2b716-cbb8-459e-b2e1-f6c252c086c2)

</details>

<details>
<summary><strong>🗂️ Multi-Chat Workspace</strong></summary>
<br>

Open multiple conversations side by side using tabbed workspaces. Each tab maintains its own session state so you can run independent agent tasks in parallel without losing context.

[▶ Watch: Multi-Chat Workspace in Action](https://github.com/user-attachments/assets/91794133-9f90-4eb0-a62c-885be70b3c39)

</details>

<details>
<summary><strong>⭐ Favorites & Chat Title</strong></summary>
<br>

Mark important conversations as favorites and rename chat titles inline — keeping your workspace organized without leaving the chat view.

[▶ Watch: Favorites & Chat Title Rename](https://github.com/user-attachments/assets/9a4892c6-eee5-46c4-af1a-eeb65e613aa3)

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



<br>

## 🚀 Getting Started

### Requirements

- [Claude Code CLI](https://claude.ai/code) installed and authenticated on your machine
- Go 1.25+ and Node.js *(only required when building from source)*

No Anthropic API key is required. Agento uses the Claude Code CLI's existing authentication by default. If you prefer to call the Anthropic API directly, you can set `ANTHROPIC_API_KEY` as an optional override.

### Installation

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

**Homebrew:**

```bash
brew install shaharia-lab/tap/agento
```

**Build from source** *(requires Go 1.25+ and Node.js):*

```bash
git clone https://github.com/shaharia-lab/agento.git
cd agento
make build
```

### Quick Start

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



<br>

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

### Logs

All logs are written in JSON format to `~/.agento/logs/system.log` (or `$AGENTO_DATA_DIR/logs/system.log`). Per-session logs are stored at `~/.agento/logs/sessions/<session-id>.log`.

Set `LOG_LEVEL=debug` to include HTTP request logs.



<br>

## 📚 Additional Resources

- [Agents](docs/agents.md) — Building and configuring custom agents with system prompts, models, tools, and template variables
- [Integrations](docs/integrations.md) — Connecting Google, GitHub, Slack, Jira, Confluence, and Telegram as agent tools
- [MCP Registry](docs/mcp.md) — Plugging in external MCP-compatible tool servers
- [Monitoring](docs/monitoring.md) — Setting up OpenTelemetry traces, metrics, and logs
- [CLI Reference](docs/cli.md) — Full command reference for `agento ask` and `agento update`



<br>

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



<br>

## 🛠️ Development

For architecture overview, local development setup, and contribution guidelines, see the [developer documentation](docs/).



<br>

## 📄 License

MIT. Maintained by [Shaharia Lab](https://github.com/shaharia-lab).
