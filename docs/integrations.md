# Integrations

Integrations connect external services to Agento so your agents can interact with them. Currently supported: **Google** (Calendar, Gmail, Drive).

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
