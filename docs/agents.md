# Agents

An agent defines the model, system prompt, and tools it can use. Agents are stored in the SQLite database at `~/.agento/agento.db`. You can create and manage agents from the UI, or define them as YAML files in `~/.agento/agents/` (auto-migrated to SQLite on first startup).

---

## Create an agent

The recommended way is through the UI (see "Manage agents from the UI" below). You can also create a YAML file in `~/.agento/agents/` — it will be imported into the database on next startup.

**Example: `~/.agento/agents/support.yaml`**

```yaml
name: Support Bot
slug: support-bot
description: Answers customer support questions.
model: claude-sonnet-4-6
thinking: adaptive
system_prompt: |
  You are a helpful support agent for Acme Inc.
  Answer questions clearly and concisely.
  Today is {{current_date}}.
capabilities:
  built_in:
    - current_time
```

After saving the file, restart Agento or use the UI to reload agents.

---

## Fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Display name shown in the UI |
| `slug` | No | URL-friendly identifier. Defaults to the file name without `.yaml` |
| `description` | No | Short description shown in the UI |
| `model` | No | Claude model ID. Defaults to `claude-sonnet-4-6` |
| `thinking` | No | `adaptive` (default), `enabled`, or `disabled` |
| `system_prompt` | No | Instructions sent to the model before every conversation |
| `capabilities` | No | Tools the agent can use (see below) |

---

## System prompt templates

The system prompt supports these placeholders:

| Placeholder | Replaced with |
|-------------|--------------|
| `{{current_date}}` | Today's date |
| `{{current_time}}` | Current time |

---

## Capabilities

### Built-in tools

```yaml
capabilities:
  built_in:
    - current_time
    - current_date
```

### Local tools

Local tools run as a local MCP server inside Agento.

```yaml
capabilities:
  local:
    - bash
    - read_file
```

### MCP servers

First register the MCP server in `~/.agento/mcps.yaml`:

```yaml
servers:
  my-server:
    command: /path/to/mcp-server
    args: ["--flag"]
```

Then reference it in the agent:

```yaml
capabilities:
  mcp:
    my-server:
      tools:
        - tool_name_one
        - tool_name_two
```

Leave `tools` empty to allow all tools from that server.

---

## Chat with an agent

1. Open the Agento UI at [http://localhost:8990](http://localhost:8990).
2. Select the agent from the sidebar.
3. Type your message and press Enter.

Tool calls made by the agent are shown inline in the conversation. If the agent needs input from you during a run, a prompt appears automatically.

---

## Manage agents from the UI

You can create, edit, and delete agents from the **Agents** section in the UI without editing YAML files by hand. Changes take effect immediately.

### Creating or editing an agent in the UI

Go to **Agents → New Agent** (or click **Edit** on an existing agent). The form has two areas:

- **System Prompt** (left side on desktop, first tab on mobile) — A full-height editor where you write the agent's instructions. Line numbers are shown for reference. Use `{{current_date}}` and `{{current_time}}` as placeholders.
- **Configuration** (right side on desktop, second tab on mobile) — Collapsible sections for all other settings:
  - **Basic Info** — Name, slug, and description.
  - **Model & Behavior** — Model selection, thinking mode, and permission mode.
  - **Built-in Tools** — Select which built-in tools the agent can use. Leave all unchecked to allow all.
  - **Integration Tools** — If you have connected integrations (e.g. Google), their tools appear here for selection.

Click **Create Agent** or **Update Agent** to save.
