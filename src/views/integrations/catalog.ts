/* ============================================================================
   The provider catalog.

   The API has no endpoint describing what a provider needs or what tools a
   service grants — /integrations/available-tools only reports tools already
   turned on, so it cannot drive a connect form for something not yet created.
   The shapes therefore live here: the credential fields each provider needs,
   and the tool names its MCP server hosts. Since #518 it also owns the one
   spelling of an integration's connection state — same question ("what does
   this app say about an integration?"), and `unavailableCopy` below was
   already the precedent for answering it here rather than at a call site.

   WhatsApp is deliberately absent, and this is the list that decides it.
   `type` is a free-form string on the wire, so nothing upstream stops one
   appearing; leaving the entry here is what would offer a pairing flow the app
   cannot complete, because `whatsmeow` has no Rust equivalent and is not being
   ported (issue #273). An older row is still listed — see providerFor.
   ========================================================================== */

import type { IconName } from "../../lib/icons";
import type { Tone } from "../../lib/format";
import type { Eq, Expect } from "../../lib/typeAssert";

export interface CredField {
  key: string;
  label: string;
  /** Rendered as a password input and never echoed back from the server. */
  secret?: boolean;
  placeholder?: string;
  help?: string;
}

/** How an integration proves who it is once it has been created. */
export type AuthKind = "oauth" | "token";

export interface AuthMode {
  /** The `auth_mode` credential value, or "" when the provider has no such field. */
  value: string;
  label: string;
  kind: AuthKind;
  fields: CredField[];
}

export interface ToolInfo {
  name: string;
  description: string;
}

export interface ServiceInfo {
  key: string;
  label: string;
  description: string;
  tools: ToolInfo[];
}

export interface Provider {
  type: string;
  label: string;
  blurb: string;
  icon: IconName;
  tone: Tone;
  /** True when `auth_mode` is part of the credentials object. */
  hasAuthModeField: boolean;
  modes: AuthMode[];
  services: ServiceInfo[];
  /** Telegram is the only provider wired to inbound triggers and webhooks. */
  supportsTriggers?: boolean;
  docs?: string;
}

export const PROVIDERS: Provider[] = [
  {
    type: "google",
    label: "Google",
    blurb: "Calendar, Gmail and Drive through a Google Cloud OAuth client",
    icon: "globe",
    tone: "accent",
    hasAuthModeField: false,
    modes: [
      {
        value: "",
        label: "OAuth 2.0",
        kind: "oauth",
        fields: [
          {
            key: "client_id",
            label: "Client ID",
            placeholder: "….apps.googleusercontent.com",
            help: "From a Google Cloud OAuth 2.0 client of type “Web application”.",
          },
          { key: "client_secret", label: "Client secret", secret: true },
        ],
      },
    ],
    services: [
      {
        key: "calendar",
        label: "Google Calendar",
        description: "Manage events, check availability and schedule meetings",
        tools: [
          { name: "create_event", description: "Create events with attendees and details" },
          { name: "view_events", description: "List and search upcoming or past events" },
        ],
      },
      {
        key: "gmail",
        label: "Gmail",
        description: "Read, send and search email",
        tools: [
          { name: "send_email", description: "Compose and send email messages" },
          { name: "read_email", description: "Read the content of specific emails" },
          { name: "search_email", description: "Search across the mailbox by query" },
        ],
      },
      {
        key: "drive",
        label: "Google Drive",
        description: "Browse, create and download files",
        tools: [
          { name: "list_files", description: "List and search files and folders" },
          { name: "create_file", description: "Create new documents and files" },
          { name: "download_file", description: "Download file contents" },
        ],
      },
    ],
  },
  {
    type: "slack",
    label: "Slack",
    blurb: "Read channels and post as a bot in your workspace",
    icon: "grid",
    tone: "purple",
    hasAuthModeField: true,
    modes: [
      {
        value: "bot_token",
        label: "Bot token",
        kind: "token",
        fields: [
          {
            key: "bot_token",
            label: "Bot token",
            secret: true,
            placeholder: "xoxb-…",
            help: "From your Slack app under OAuth & Permissions.",
          },
        ],
      },
      {
        value: "oauth",
        label: "OAuth",
        kind: "oauth",
        fields: [
          { key: "client_id", label: "Client ID" },
          { key: "client_secret", label: "Client secret", secret: true },
        ],
      },
    ],
    services: [
      {
        key: "messaging",
        label: "Messaging",
        description: "Send messages, read channels, search and list users",
        tools: [
          { name: "list_channels", description: "List channels the bot can reach" },
          { name: "send_message", description: "Send a message to a channel" },
          { name: "read_messages", description: "Read recent messages from a channel" },
          { name: "send_reply", description: "Send a threaded reply to a message" },
          { name: "get_channel_info", description: "Get detailed info about a channel" },
          { name: "list_users", description: "List users in the workspace" },
          { name: "search_messages", description: "Search messages across the workspace" },
        ],
      },
    ],
  },
  {
    type: "github",
    label: "GitHub",
    blurb: "Repositories, issues, pull requests, Actions and releases",
    icon: "branch",
    tone: "teal",
    hasAuthModeField: true,
    modes: [
      {
        value: "pat",
        label: "Personal access token",
        kind: "token",
        fields: [
          {
            key: "personal_access_token",
            label: "Access token",
            secret: true,
            placeholder: "github_pat_… or ghp_…",
            help: "A fine-grained or classic token with the scopes the services below need.",
          },
        ],
      },
    ],
    services: [
      {
        key: "repos",
        label: "Repositories",
        description: "List repos, read details and search code",
        tools: [
          { name: "list_repos", description: "List repositories for the authenticated user" },
          { name: "get_repo", description: "Get details of a specific repository" },
          { name: "search_code", description: "Search code across repositories" },
        ],
      },
      {
        key: "issues",
        label: "Issues",
        description: "List, create and update issues",
        tools: [
          { name: "list_issues", description: "List issues for a repository" },
          { name: "get_issue", description: "Get details of a specific issue" },
          { name: "create_issue", description: "Create a new issue" },
          { name: "update_issue", description: "Update an existing issue" },
        ],
      },
      {
        key: "pull_requests",
        label: "Pull requests",
        description: "List, create and review pull requests",
        tools: [
          { name: "list_pulls", description: "List pull requests for a repository" },
          { name: "get_pull", description: "Get details of a specific pull request" },
          { name: "create_pull", description: "Create a new pull request" },
          { name: "get_pull_diff", description: "Get the diff of a pull request" },
          { name: "list_pull_comments", description: "List review comments on a pull request" },
        ],
      },
      {
        key: "actions",
        label: "Actions",
        description: "Manage workflows, runs and logs",
        tools: [
          { name: "list_workflows", description: "List all workflows in a repository" },
          { name: "list_workflow_runs", description: "List workflow runs" },
          { name: "trigger_workflow", description: "Trigger a workflow dispatch event" },
          { name: "get_workflow_run", description: "Get details of a workflow run" },
          { name: "get_run_logs", description: "Get the logs URL for a workflow run" },
        ],
      },
      {
        key: "releases",
        label: "Releases",
        description: "Manage releases and tags",
        tools: [
          { name: "list_releases", description: "List releases for a repository" },
          { name: "create_release", description: "Create a new release" },
          { name: "list_tags", description: "List tags for a repository" },
        ],
      },
    ],
  },
  {
    type: "telegram",
    label: "Telegram",
    blurb: "Send and read messages, and let inbound messages start agents",
    icon: "send",
    tone: "accent",
    hasAuthModeField: false,
    modes: [
      {
        value: "",
        label: "Bot token",
        kind: "token",
        fields: [
          {
            key: "bot_token",
            label: "Bot token",
            secret: true,
            placeholder: "123456789:AA…",
            help: "Issued by @BotFather when you create the bot.",
          },
        ],
      },
    ],
    services: [
      {
        key: "messaging",
        label: "Messaging",
        description: "Send messages, photos, locations and polls, and manage chats",
        tools: [
          { name: "send_message", description: "Send a text message to a chat" },
          { name: "read_messages", description: "Read recent messages" },
          { name: "get_chat_info", description: "Get detailed info about a chat" },
          { name: "send_photo", description: "Send a photo by URL" },
          { name: "forward_message", description: "Forward a message between chats" },
          { name: "edit_message", description: "Edit a previously sent message" },
          { name: "delete_message", description: "Delete a message from a chat" },
          { name: "pin_message", description: "Pin a message in a chat" },
          { name: "get_chat_members", description: "Get the member count of a chat" },
          { name: "send_location", description: "Send a geographic location" },
          { name: "create_poll", description: "Create a poll in a chat" },
        ],
      },
    ],
    supportsTriggers: true,
  },
  {
    type: "jira",
    label: "Jira",
    blurb: "Projects and issues on an Atlassian site",
    icon: "task",
    tone: "accent",
    hasAuthModeField: false,
    modes: [
      {
        value: "",
        label: "API token",
        kind: "token",
        fields: [
          {
            key: "site_url",
            label: "Site URL",
            placeholder: "https://your-team.atlassian.net",
          },
          { key: "email", label: "Account email", placeholder: "you@example.com" },
          {
            key: "api_token",
            label: "API token",
            secret: true,
            help: "Created under Atlassian account settings → Security → API tokens.",
          },
        ],
      },
    ],
    services: [
      {
        key: "project_management",
        label: "Project management",
        description: "Search, create and transition issues across projects",
        tools: [
          { name: "list_projects", description: "List all accessible projects" },
          { name: "get_project", description: "Get details of a project by key" },
          { name: "search_issues", description: "Search issues using JQL" },
          { name: "get_issue", description: "Get details of an issue by key" },
          { name: "create_issue", description: "Create a new issue in a project" },
          { name: "update_issue", description: "Update fields of an existing issue" },
          { name: "add_comment", description: "Add a comment to an issue" },
          { name: "list_transitions", description: "List available status transitions" },
          { name: "transition_issue", description: "Move an issue to a new status" },
        ],
      },
    ],
  },
  {
    type: "confluence",
    label: "Confluence",
    blurb: "Spaces and pages on an Atlassian site",
    icon: "layers",
    tone: "purple",
    hasAuthModeField: false,
    modes: [
      {
        value: "",
        label: "API token",
        kind: "token",
        fields: [
          {
            key: "site_url",
            label: "Site URL",
            placeholder: "https://your-team.atlassian.net",
          },
          { key: "email", label: "Account email", placeholder: "you@example.com" },
          {
            key: "api_token",
            label: "API token",
            secret: true,
            help: "Created under Atlassian account settings → Security → API tokens.",
          },
        ],
      },
    ],
    services: [
      {
        key: "content",
        label: "Content",
        description: "Browse spaces, search content and write pages",
        tools: [
          { name: "list_spaces", description: "List all available spaces" },
          { name: "get_space", description: "Get details of a space by ID" },
          { name: "search_content", description: "Search content using CQL" },
          { name: "get_page", description: "Retrieve a page including its content" },
          { name: "create_page", description: "Create a new page in a space" },
          { name: "update_page", description: "Update the title and content of a page" },
        ],
      },
    ],
  },
];

/**
 * The catalog entry for a stored integration's type, if this app has one.
 *
 * `undefined` is a normal answer, not a failure: the `type` column is free-form
 * and a database may hold types this app deliberately does not know — WhatsApp
 * above all. Callers render the row from the stored fields instead of dropping
 * it.
 */
export function providerFor(type: string): Provider | undefined {
  return PROVIDERS.find((p) => p.type === type);
}

/* --- Explaining a type with no catalog entry ------------------------------
   Two different situations reach the same screen and telling them apart is the
   whole point: a type from a newer Agento is something to upgrade into, while
   WhatsApp is one this app will never gain — `whatsmeow` has no Rust equivalent
   and is not being ported (#273). Saying "newer version" to someone who paired a
   phone in an older Agento sends them looking for an update that will never
   ship.

   Either way the row is listed and readable. It cannot be removed, renamed or
   disabled from this app: those controls live in `IntegrationDetail`, which only
   renders for a known provider. That dead end is deliberate for now — see #273's
   scope note — so do not describe the row as deletable.

   A `Map`, not an object literal: `type` is free-form on the wire (the create
   path accepts any non-empty string), and a stored type of `constructor` or
   `toString` would hit `Object.prototype` and render a title built from
   `undefined`.
   ------------------------------------------------------------------------ */

const RETIRED_TYPES = new Map<string, { label: string; reason: string }>([
  [
    "whatsapp",
    {
      label: "WhatsApp",
      reason:
        "Agento does not support WhatsApp, so this connection cannot be paired, edited or used. Nothing has been deleted — the connection and its settings are listed here exactly as they were stored.",
    },
  ],
]);

/** Title and body for an integration whose type `providerFor` does not know. */
export function unavailableCopy(type: string): { title: string; text: string } {
  const retired = RETIRED_TYPES.get(type);
  if (retired) {
    return { title: `${retired.label} is not available here`, text: retired.reason };
  }
  return {
    title: `Unknown provider “${type}”`,
    text: "This integration was created by a newer version of Agento than this app knows about.",
  };
}

/* --- One connection state, one word (#518) --------------------------------
   `Integration.authenticated` renders in four places that are on screen at the
   same time — the sidebar row's preview line, the detail toolbar badge, the
   Authorisation section's Status row and the inspector's State row — and each
   was written at its own call site, so one row read `GitHub · Not connected` in
   the sidebar and `Not authenticated` in the inspector two inches apart. Two
   words for one boolean reads as two states, and the reporter went looking for
   a second thing to fix.

   `Connected` is the word, in both directions. It is the one the surrounding
   screen already speaks — the sidebar's own `Connected` group heading, the
   `Nothing connected yet.` empty state, the connect screen's `Not connected
   yet — …` — and it is the user's word rather than the wire's. That last part
   matters beyond taste: `authenticated` is a *wire* field whose meaning is
   under review (#513), so copy spelled after the column would have to be
   re-read every time the column moves. This is the only place it is spelled,
   so if #513 changes what the boolean means, one function changes.

   The tone travels with the label because both badge sites need it and
   `format.ts`'s `Tone` is emphatically not a status→tone map — it hashes a key
   to pick a stable *tile* colour. Status colour is an explicit `badge--green` /
   `badge--amber` class, so the union here is written out rather than imported.
   ------------------------------------------------------------------------ */

/** What a verified integration is called. */
export const CONNECTED = "Connected";
/** What an unverified one is called — the same word, negated. */
export const NOT_CONNECTED = "Not connected";

export interface ConnectionState {
  label: string;
  tone: "green" | "amber";
}

/**
 * The one description of an integration's connection state.
 *
 * Takes the boolean rather than the row: it is the only input, and passing the
 * wire type would put `lib/types.ts` into a module that otherwise describes
 * only what this app knows about providers.
 */
export function connectionState(authenticated: boolean): ConnectionState {
  return authenticated
    ? { label: CONNECTED, tone: "green" }
    : { label: NOT_CONNECTED, tone: "amber" };
}

/**
 * Respell either word and these stop compiling, which fails `npm run build`
 * and therefore CI — the frontend has no test harness, so this is the guard.
 * Exported so `noUnusedLocals` does not delete it; see `lib/typeAssert.ts` for
 * why `Eq` and not `extends`.
 */
export type PinConnected = Expect<Eq<typeof CONNECTED, "Connected">>;
export type PinNotConnected = Expect<Eq<typeof NOT_CONNECTED, "Not connected">>;

/**
 * The mode a stored integration is using.
 *
 * `authMode` is the row's recorded `auth_mode` — a discriminator the scrubbed
 * read carries since #513. It is empty for every single-mode provider, and for
 * a Slack row written before that field existed, so the first mode remains the
 * fallback. What it must not be is a *guess* for a provider with more than one
 * mode: Slack's first mode is `bot_token`, so guessing showed an OAuth row the
 * bot-token tab and its credential fields.
 */
export function modeFor(provider: Provider, authMode: string): AuthMode {
  return provider.modes.find((m) => m.value === authMode) ?? provider.modes[0];
}

export function serviceLabel(provider: Provider | undefined, key: string): string {
  return provider?.services.find((s) => s.key === key)?.label ?? key;
}
