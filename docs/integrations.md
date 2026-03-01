# Integrations

Integrations connect external services to Agento so your agents can interact with them. Each integration runs as an in-process MCP server, making its tools available to agents.

Currently supported:
- **Google** — Calendar, Gmail, Drive (OAuth 2.0)
- **GitHub** — Repos, issues, pull requests, actions, releases (Personal Access Token)
- **Slack** — Channels, messages, users (Bot Token / OAuth)
- **Jira** — Issues, projects, boards (API Token)
- **Confluence** — Pages, spaces, search (API Token)
- **Telegram** — Messages, chats, media (Bot Token)

All integrations are managed from the **Integrations** page in the UI. Each has its own setup flow — click the service card to configure credentials, enable tools, and connect.

---

## Google Integration

### Prerequisites

You need a Google Cloud OAuth 2.0 Client ID. To create one:

1. Go to [Google Cloud Console](https://console.cloud.google.com/)
2. Create a project (or select an existing one)
3. Navigate to **APIs & Services > Credentials**
4. Click **Create Credentials > OAuth client ID**
5. Select **Desktop app** as the application type
6. Copy the **Client ID** and **Client Secret**

Enable the APIs your agents will use:
- **Google Calendar API** — for calendar tools
- **Gmail API** — for email tools
- **Google Drive API** — for file tools

### Creating an Integration

1. Open Agento and go to **Integrations** in the sidebar
2. Click the **Google** card to start setup
3. Enter a name, your Client ID, and Client Secret
4. Enable the services you want (Calendar, Gmail, Drive) and select tools per service
5. Click **Save** — a Google sign-in window opens
6. Complete the OAuth flow — you'll see "Connected" when done

### Available Tools

#### Calendar
| Tool | Description |
|------|-------------|
| `create_event` | Create an event on the user's primary Google Calendar |
| `view_events` | List events within a time range |

#### Gmail
| Tool | Description |
|------|-------------|
| `send_email` | Send an email |
| `read_email` | Read a message by its ID |
| `search_email` | Search messages using Gmail query syntax (e.g. `from:alice@example.com is:unread`) |

#### Drive
| Tool | Description |
|------|-------------|
| `list_files` | List files and folders |
| `create_file` | Create a text file |
| `download_file` | Download a file's content by ID |

### Adding Integration Tools to an Agent

1. Go to **Agents** and create or edit an agent
2. In the **Capabilities** section, find **Integration Tools**
3. Select the specific tools you want this agent to have
4. Save the agent

Only the tools you select will be available to the agent. The agent will not see or be able to use any tools you didn't enable.

### Managing Integrations

- **Edit**: Click an integration from the list to update services, tools, or credentials
- **Re-authenticate**: If the token expires, click "Re-authenticate" on the integration detail page
- **Delete**: Remove an integration from the detail page. Agents referencing it will lose access to those tools.

Changes take effect immediately — no restart required.

---

## GitHub Integration

### Prerequisites

A GitHub Personal Access Token with the scopes your agents need (e.g., `repo`, `workflow`).

### Setup

1. Go to **Integrations** and click the **GitHub** card
2. Enter a name and your Personal Access Token
3. Select the tools you want to enable
4. Click **Save**

### Available Tools

| Tool | Description |
|------|-------------|
| `list_repos` | List repositories for the authenticated user |
| `get_repo` | Get details of a specific repository |
| `search_code` | Search code across repositories |
| `list_issues` | List issues in a repository |
| `get_issue` | Get details of a specific issue |
| `create_issue` | Create a new issue |
| `update_issue` | Update an existing issue |
| `list_pulls` | List pull requests in a repository |
| `get_pull` | Get details of a pull request |
| `create_pull` | Create a new pull request |
| `get_pull_diff` | Get the diff of a pull request |
| `list_pull_comments` | List comments on a pull request |
| `list_workflows` | List GitHub Actions workflows |
| `list_workflow_runs` | List runs for a workflow |
| `trigger_workflow` | Trigger a workflow dispatch |
| `get_workflow_run` | Get details of a workflow run |
| `get_run_logs` | Get logs for a workflow run |
| `list_releases` | List releases in a repository |
| `create_release` | Create a new release |
| `list_tags` | List tags in a repository |

---

## Slack Integration

### Prerequisites

A Slack Bot Token (`xoxb-...`) or OAuth app credentials. The bot must be invited to the channels it needs to access.

### Setup

1. Go to **Integrations** and click the **Slack** card
2. Enter a name and your Bot Token (or complete the OAuth flow)
3. Select the tools you want to enable
4. Click **Save**

### Available Tools

| Tool | Description |
|------|-------------|
| `list_channels` | List channels the bot can access |
| `get_channel_info` | Get details of a channel |
| `read_messages` | Read recent messages from a channel |
| `send_message` | Send a message to a channel |
| `send_reply` | Send a threaded reply |
| `list_users` | List workspace users |
| `search_messages` | Search messages (requires user token) |

---

## Jira Integration

### Prerequisites

A Jira API Token and your Atlassian site URL (e.g., `https://yoursite.atlassian.net`).

### Setup

1. Go to **Integrations** and click the **Jira** card
2. Enter a name, site URL, email, and API token
3. Select the tools you want to enable
4. Click **Save**

### Available Tools

| Tool | Description |
|------|-------------|
| `list_projects` | List accessible Jira projects |
| `get_project` | Get project details by key |
| `search_issues` | Search issues with JQL |
| `get_issue` | Get issue details by key (e.g., PROJ-123) |
| `create_issue` | Create a new issue |

---

## Confluence Integration

### Prerequisites

A Confluence API Token and your Atlassian site URL.

### Setup

1. Go to **Integrations** and click the **Confluence** card
2. Enter a name, site URL, email, and API token
3. Select the tools you want to enable
4. Click **Save**

### Available Tools

| Tool | Description |
|------|-------------|
| `list_spaces` | List Confluence spaces |
| `get_space` | Get space details by ID |
| `search_content` | Search content with CQL |
| `get_page` | Get page content and metadata by ID |
| `create_page` | Create a new page in a space |

---

## Telegram Integration

### Prerequisites

A Telegram Bot Token from [@BotFather](https://t.me/botfather).

### Setup

1. Go to **Integrations** and click the **Telegram** card
2. Enter a name and your Bot Token
3. Select the tools you want to enable
4. Click **Save**

### Available Tools

| Tool | Description |
|------|-------------|
| `send_message` | Send a text message to a chat |
| `send_photo` | Send a photo by URL |
| `send_location` | Send a geographic location |
| `create_poll` | Create a poll in a chat |
| `read_messages` | Read recent messages (bot updates) |
