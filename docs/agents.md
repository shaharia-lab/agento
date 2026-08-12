# Agents

An agent is a saved configuration: a name, a system prompt, a model, a thinking
mode, a permission mode, and an explicit list of the tools it may use. Save it
once and run it from a chat, a [scheduled task](tasks.md), a
[Telegram trigger](integrations.md#triggers-run-an-agent-from-an-incoming-message)
or the CLI.

Agents live in the SQLite database at `~/.agento/agento.db`. YAML files in
`~/.agento/agents/` are imported into it on startup — that path is a legacy
import, not the source of truth.

- [Create an agent](#create-an-agent)
- [Fields](#fields)
- [Thinking and permission modes](#thinking-and-permission-modes)
- [Tools](#tools)
- [System prompt templates](#system-prompt-templates)
- [Running as a different Claude account](#running-as-a-different-claude-account)
- [Claude settings profiles](#claude-settings-profiles)
- [Running an agent](#running-an-agent)

---

## Create an agent

Go to **Agents → New Agent**. The form has two areas:

- **System Prompt** — a full-height editor for the agent's instructions.
- **Configuration** — Basic Info (name, slug, description), Model & Behavior
  (model, thinking mode, permission mode), Built-in Tools, and Integration
  Tools for any [integration](integrations.md) you have connected.

Changes take effect immediately.

Agents can also be defined as YAML and dropped into `~/.agento/agents/`, which
is useful for seeding a fresh install from version control:

```yaml
name: Support Bot
slug: support-bot
description: Answers customer support questions.
model: claude-sonnet-4-6
thinking: adaptive
permission_mode: plan
system_prompt: |
  You are a helpful support agent for Acme Inc.
  Answer questions clearly and concisely.
  Today is {{current_date}}.
capabilities:
  built_in:
    - Read
    - Grep
  local:
    - current_time
```

The file is imported on the next startup. After that, edit the agent in the UI —
the database is what runs.

---

## Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Display name shown in the UI |
| `slug` | No | URL-friendly identifier. Defaults to the file name without `.yaml` |
| `description` | No | Short description shown in the UI |
| `model` | No | Claude model ID. Defaults to `claude-sonnet-4-6` |
| `thinking` | No | `adaptive` (default), `enabled`, or `disabled` |
| `permission_mode` | No | `bypass` (default), `default`, `plan`, or `dontAsk` |
| `system_prompt` | No | Instructions sent to the model before every conversation |
| `capabilities` | No | Which tools the agent may use (see below) |
| `claude_config_dir` | No | Run as a different Claude Code account (see below) |

---

## Thinking and permission modes

**Thinking** — `adaptive` lets Claude decide, `enabled` forces extended
thinking, `disabled` turns it off. `agento ask --no-thinking` overrides it for a
single CLI run.

**Permission mode** decides what happens when the agent wants to use a tool:

| Mode | Behaviour |
|------|-----------|
| `bypass` (default) | Tools run without asking |
| `default` | Claude Code's normal prompting; the UI asks you in-chat |
| `plan` | Plans first, acts only after you approve |
| `dontAsk` | Runs permitted tools without prompting |

`bypass` is the default because most agents are run unattended. It also means an
agent holding `Bash` can do anything you can — so give an agent only the tools it
needs, and prefer `plan` for anything that writes files or runs commands. See
[Security](security.md#agent-permission-modes).

---

## Tools

### Built-in Claude Code tools

`Read`, `Write`, `Edit`, `Bash`, `Glob`, `Grep`, `WebFetch`, `WebSearch`,
`Task`, `TaskOutput`, `TaskStop`, `NotebookEdit`. The agent form offers the
commonly used ones as checkboxes; YAML accepts any name from that list.

```yaml
capabilities:
  built_in:
    - Read
    - Grep
    - Bash
```

**Selecting none allows all of them.** An explicit list is an allowlist —
anything not on it is unavailable to the agent.

### Local tools

Local tools run inside Agento as an in-process MCP server. Currently:

| Tool | Description |
|------|-------------|
| `current_time` | The current date and time for an IANA timezone (defaults to UTC) |

```yaml
capabilities:
  local:
    - current_time
```

### Integration tools

Every [integration](integrations.md) you connect — Google, GitHub, Slack, Jira,
Confluence, Telegram, WhatsApp — exposes its tools in the agent form under
**Integration Tools**. Select the individual tools this agent should have; it
cannot see the ones you leave unselected.

### External MCP servers

Register the server in `~/.agento/mcps.yaml`. The file is a map of server name
to configuration, and `transport` is required:

```yaml
# stdio — a local process
my-server:
  transport: stdio
  command: /path/to/mcp-server
  args: ["--flag"]
  env:
    API_KEY: ${ENV:MY_SERVER_API_KEY}

# streamable HTTP
remote-server:
  transport: streamable_http
  url: https://mcp.example.com/mcp
  headers:
    Authorization: Bearer ${ENV:REMOTE_TOKEN}

# server-sent events
legacy-server:
  transport: sse
  url: https://mcp.example.com/sse
```

`${ENV:VAR_NAME}` reads from Agento's own environment, so tokens stay out of the
file. A referenced variable that is not set is an error at load time rather than
a confusing failure later.

Then reference the server from the agent:

```yaml
capabilities:
  mcp:
    my-server:
      tools:
        - tool_name_one
        - tool_name_two
```

Leave `tools` empty to allow every tool that server exposes.

---

## System prompt templates

| Placeholder | Replaced with |
|-------------|--------------|
| `{{current_date}}` | Today's date |
| `{{current_time}}` | The current time |

Substitution happens at run time, so a scheduled task always sees the date it
actually ran on.

---

## Running as a different Claude account

Claude Code keeps its credentials, projects and settings in a config directory —
`~/.claude` unless `CLAUDE_CONFIG_DIR` says otherwise. Setting
`claude_config_dir` on an agent points *that agent's runs* at a different
directory, which is how a work agent and a personal agent stay live in one
Agento instance.

The directory a run targets is resolved in this order:

1. the agent's own `claude_config_dir`
2. the `CLAUDE_CONFIG_DIR` environment variable
3. the global default (**Settings → General → Claude config directory**)
4. `~/.claude`

The environment variable comes before the stored setting because it is what the
surrounding environment has already chosen for every subprocess — and Agento
refuses to store a setting that conflicts with it.

This applies to every entry point: chats, scheduled tasks, Telegram triggers and
`agento ask`. Claude's `settings.json` is resolved **inside the directory the run
targets**, and omitted when it does not exist there.

Analytics works differently: it indexes *every* configured directory into one
corpus, because reporting is retrospective. See
[Claude Sessions](claude-sessions.md#multiple-claude-accounts).

---

## Claude settings profiles

A settings profile is a named Claude Code `settings.json` you can switch between
per agent, per chat or per [task](tasks.md). Profiles are stored as
`settings_<slug>.json` in the run's config directory, with their metadata in
`settings_profiles.json`; a default profile is created from your existing
`settings.json` on first launch.

Manage them under **Settings → Claude Settings**. Because profiles are a global
CRUD surface, they live in the default run directory rather than inside any
per-agent override — a named profile keeps its recorded absolute path.

---

## Running an agent

**Chat** — pick the agent in the sidebar and type. Tool calls appear inline as
they happen, responses stream live, and if the agent needs your input a prompt
appears in the conversation. Drag and drop files or paste images straight into
the input. The multi-chat workspace runs several conversations in parallel, each
tab with its own agent and session.

**Scheduled** — see [Tasks](tasks.md).

**From an incoming message** — see
[Triggers](integrations.md#triggers-run-an-agent-from-an-incoming-message).

**CLI**

```bash
agento ask "What changed in the repo today?"
agento ask --agent code-reviewer "Review the staged diff"
agento ask --agent code-reviewer "Follow up on that" <session-id>
agento ask --no-thinking --agent code-reviewer "Quick check"
```

Passing a session ID continues that conversation. `agento ask` honours the
stored Claude config directory, so it authenticates as the same account the web
UI does.
