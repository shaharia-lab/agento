# User Guide

How to use Agento Desktop, section by section.

New here? Install first: [Installation](installation.md).

- [The window](#the-window)
- [Chats](#chats)
- [Agents](#agents)
- [Integrations](#integrations)
- [Scheduled tasks](#scheduled-tasks)
- [Job history](#job-history)
- [Claude sessions](#claude-sessions)
- [Analytics](#analytics)
- [Settings](#settings)
- [Privacy](#privacy)

---

## The window

Agento is one window with four parts.

```
┌──────────────────────────────────────────────────────┐
│ titlebar: back, forward, sidebar toggle              │
├─────────┬────────────────────────────────────────────┤
│ sidebar │ toolbar                                    │
│         ├──────────┬──────────────────┬──────────────┤
│ sections│ list     │ detail           │ inspector    │
├─────────┴──────────┴──────────────────┴──────────────┤
│ status bar: running agents, model, today's spend     │
└──────────────────────────────────────────────────────┘
```

- **Sidebar**: the sections of the app. `Ctrl B` hides it.
- **List**: what is in the current section. Search and filters sit above it.
- **Detail**: the thing you selected.
- **Inspector**: facts and figures about that thing. `Ctrl I` hides it.
- **Status bar**: always live. Running agents, the active model, today's tokens
  and spend, and connection state.

Drag the thin lines between panes to resize them. The app remembers your window
size and position between launches.

### Getting around fast

Press `Ctrl K` (or `⌘K`) for the command palette. Type a few letters of what you
want and press Enter. It reaches every section and the common actions.

| Shortcut | Action |
| --- | --- |
| `Ctrl K` | Command palette |
| `Ctrl N` | New chat |
| `Ctrl ,` | Settings |
| `Ctrl B` | Sidebar |
| `Ctrl I` | Inspector |
| `Ctrl [` / `Ctrl ]` | Back / forward |
| `Ctrl 1` to `Ctrl 7` | Jump to a section |

On macOS use `⌘` instead of `Ctrl`, and the menu bar carries the same actions.

---

## Chats

A chat is a conversation with an agent, kept for as long as you want it.

**Start one:** press `Ctrl N` or click the `+` above the chat list. Pick an agent
(or none), choose a **working directory**, and type your first message.

The working directory matters. It is the folder the agent can see and act in, the
same way `claude` behaves when you run it from that folder. Click the folder icon
to pick one with your system's file picker.

**While a turn is running:**

- The reply streams in as it is produced.
- Tool calls appear inline, with the input and the result.
- If the agent needs permission for a tool, a prompt appears. Approve or deny.
- If the agent asks you a question, answer it in the composer and the run
  continues.
- **Stop** halts the run. Anything already written stays.

**The inspector** shows the agent, the model, the message count and the token
usage of the whole conversation, split into input, output, cache read and cache
write, with the cost.

Rename a chat from the title, or delete it from the toolbar. Deleting removes its
messages too.

**One thing worth knowing:** a turn that produced no final answer, for example one
you stopped immediately, stores nothing. That is the same rule Claude Code
follows.

---

## Agents

An agent is a saved configuration you can reuse from chats, scheduled tasks and
the command palette.

Fields:

| Field | What it does |
| --- | --- |
| **Name** and **slug** | The slug identifies it in the API and the CLI. It is filled in for you |
| **Description** | For you, not for the model |
| **Model** | Sonnet, Opus or Haiku. Leave empty to use the Claude Code default |
| **Thinking** | Adaptive, always on, or disabled. Controls extended reasoning |
| **Permission mode** | How much an unattended run may do without asking. See below |
| **System prompt** | The agent's instructions |
| **Claude config dir** | Run this agent as a different Claude Code account |
| **Tools** | Exactly what this agent is allowed to use |

### Permissions

Two rules, and they do not depend on the agent's settings:

- **In a chat, Agento asks you before a tool runs.** You are there to answer, so
  the prompt appears in the transcript and the run waits.
- **A tool the agent does not list is denied without asking.** The allowlist is
  enforced whatever the permission mode says.

The **permission mode** field applies to unattended runs, meaning scheduled
tasks, where nothing can answer a prompt. Left unset, those runs proceed without
prompting, because the alternative is a task that hangs until it times out.

**Known limitation:** the permission mode you pick in the form is not currently
saved. The underlying API drops the field, and the desktop app reproduces the
server's behaviour rather than diverging from it. Treat the selector as
informational for now, and rely on the tool allowlist and the working directory
to bound what an agent can do.

### Tools

Tools are an allowlist. An agent can use only what you tick.

- **Built-in tools**: the Claude Code tools, such as reading and writing files,
  running commands and searching.
- **Local tools**: small helpers Agento serves itself.
- **Integration tools**: individual tools from the integrations you have
  connected, for example "send a Slack message" or "create a GitHub issue".

### Template variables

System prompts support `{{current_date}}` and `{{current_time}}`, filled in at the
moment the agent runs. Useful for scheduled tasks that need to know what day it is.

### Running as another Claude account

Set **Claude config dir** on an agent to point at a different Claude Code
configuration directory. That is how a work agent and a personal agent can run in
one copy of Agento, each authenticated as its own account.

---

## Integrations

Integrations connect an outside service and expose it to your agents as tools.

Available in the desktop app:

| Service | What agents can do |
| --- | --- |
| **Google** | Calendar events, Gmail, Drive files |
| **Slack** | List channels, read and send messages, search |
| **GitHub** | Repositories, issues, pull requests, actions, releases |
| **Telegram** | Send messages, and trigger agents from incoming ones |
| **Jira** | Search, create, update and transition issues |
| **Confluence** | Read, search, create and update pages |

**To connect one:** open Integrations and pick a service under **Add
integration**, then fill in the form. Google and Slack use OAuth, so a browser
window opens and you approve access there. GitHub, Telegram, Jira and Confluence
use a token you paste in.

For each integration you choose which **services** are enabled (for example Gmail
but not Drive) and which **tools** within them. Agents then pick from what you
enabled.

**Editing credentials:** when you change an integration that has stored
credentials, Agento asks you to re-enter them rather than saving a blank. That is
deliberate: saving a scrubbed form would wipe the working credential.

### Telegram triggers

A Telegram integration can also run agents on incoming messages. Add a trigger
rule saying which messages match and which agent handles them. The agent's reply
goes back to the same chat.

### If you paired WhatsApp in the web app

The desktop app does not support WhatsApp. An existing WhatsApp integration is
listed and its data is safe, but it cannot be edited or used here.

---

## Scheduled tasks

A scheduled task runs an agent on its own, on a schedule, and records what
happened.

Create one with a name, an agent, and the **prompt** sent to that agent verbatim
on every run.

**Schedules:**

| Type | Meaning |
| --- | --- |
| **Immediately** | Run once, a couple of seconds from now |
| **Once** | Run once at a date and time you pick |
| **Interval** | Every N minutes, hours or days, optionally at a fixed time of day |
| **Cron** | A crontab expression, for anything more specific |

Other options:

- **Working directory**: the folder the run happens in.
- **Model**: override the agent's model for this task.
- **Timeout**: runs longer than this are cancelled and recorded as failed.
- **Save output**: keep the agent's reply in the run history.
- **Stop after**: end the task after N runs. `0` means never.
- **Stop at**: an end date.
- **Enabled**: pause without deleting.

The inspector shows the next run, the last run, the total number of runs and the
last outcome.

**Times are your local time.** They are stored in UTC and converted for display.

**One process only.** If you also run `agento web` against the same data
directory, both would fire every task. Run one or the other.

---

## Job history

Every scheduled run leaves a record: which task, when it started, how long it
took, whether it succeeded, the tokens and cost, and the output if you asked for
it to be saved.

Failed runs carry the reason. A run that hit its timeout says so.

Select one run or several and delete them.

---

## Claude sessions

This is your Claude Code history, indexed and searchable. It covers everything
`claude` has ever done on this machine, not only what you ran through Agento.

**The list** pages as you scroll and filters in the database, so it stays fast at
thousands of sessions. Filter by project, model, date, cost or duration, search
titles and message content, and sort by recent, cost, tokens, duration or message
count.

Each row carries its project, model, turn count, tokens, cost, duration, and
badges for permission mode, linked pull requests and git branch.

**Actions on a session:**

- **Star it** to find it again, then filter to favourites only.
- **Continue in chat** turns that session into an Agento chat that resumes it.
  The app tells you the new chat id; open Chats to carry on there.

**The detail view** replays the run step by step: every prompt, response, tool
call and result in order, with each sub-agent's steps nested under the delegation
that spawned it. When a long unattended run went wrong, this is where you find
where.

Beside it are the session's own metrics: turns, steps per turn, longest
autonomous chain, active duration, time Claude spent working, your own average
reply time, and tool error rate.

### Duration means active time

Claude Code sessions are resumable, so a session you picked up a week later would
otherwise report a week of work. Gaps longer than a threshold you control are
excluded everywhere a duration is shown. Change the threshold in
**Settings → Data**.

### The first scan

On first launch, Agento reads every transcript on your disk. On a large history
that takes a few minutes, and the list shows progress while it works. After that
it only re-reads files that changed.

Some changes force a full re-read: editing a model price, or changing the idle
threshold. That is expected, and it happens in the background.

---

## Analytics

Three views over the same indexed history. All of them take a date range, and
group by hour, day, week, month or year depending on how wide that range is.

### Token usage

Where the tokens and the money go.

- **Token composition**: input, output, cache reads and cache writes kept apart,
  because they bill at very different rates.
- **Tokens over time** and **cache efficiency**.
- **Cost by model**, credited to the model that actually spent it, including work
  done inside sub-agents. Delegating to a cheaper model shows up as a real saving.
- **Projects**, ranked by spend.

Models with no published price are disclosed rather than counted as free, so a
total that is missing something says so.

### General usage

How you work.

- Sessions over time and by model.
- Busiest days, hour of day, weekly rhythm.
- Top sessions by cost, duration and tokens.

The hour chart counts a session in **every** hour it was running, not only the
hour it ended, so the per-hour numbers add up to more than the session count.
That is intentional and the chart says so.

### Insights

Beta, and the most opinionated view.

Headline numbers: sessions, average autonomy score, average turns, cache hit
rate, and total cost. Each compared against the previous period, so you can see
the direction.

Below that, **tool call attribution**: every tool call broken down by the skill,
plugin, MCP server, sub-agent and reasoning effort responsible. This is how you
find the skill quietly burning a third of your calls.

Then a set of cards suggesting concrete things to change. One of them prices what
prompt caching saved you, which is an estimate from list prices and is labelled as
one.

---

## Settings

`Ctrl ,` opens Settings. Seven panes.

### General

- **Working directory**: the default folder for new chats and tasks.
- **Default model**: used when neither the chat nor the agent picks one.
- **Public URL**: only relevant if you also run the server.
- **Updates**: how Agento behaves when a new version exists. See
  [Updates](installation.md#updates).

### Claude

- **Run directory**: which Claude Code configuration directory runs use by
  default.
- **Indexed directories**: which directories analytics reads. Add a second one if
  you use more than one Claude account, and both appear in every total.
- **Settings profiles**: named Claude Code settings files, managed here. Runs use
  the profile marked as default.

Removing a directory from the indexed list hides its sessions rather than
deleting them. Adding it back is instant.

### Appearance

Light, dark, or match the system. Also available from the status bar and the
command palette.

### Notifications

Email delivery over SMTP, and which events send one. Scheduled task outcomes are
the main use. There is a test button that sends one message so you know the
configuration works before you rely on it.

### Data

- **Idle gap threshold**: how long a pause has to be before it stops counting as
  working time. Between 1 and 240 minutes, 10 by default. Changing it re-reads
  your history in the background, because durations are stored rather than
  recomputed on every view.
- **Hidden projects**: keep a project out of every chart and list. Its data is
  kept, so unhiding is immediate.

### Pricing

The model price catalog every cost figure is computed from. It ships filled in
for Anthropic, Moonshot, Z.ai and Alibaba models.

Two separate actions, on purpose:

- **Add a rate** when a price changes going forward. History keeps what it was
  charged.
- **Correct a rate** when the existing entry was wrong. This rewrites costs that
  were already reported.

Either way, your sessions are re-priced in the background afterwards.

### Advanced

Monitoring configuration, shown read-only. The desktop app does not export
telemetry. If you also run `agento web` against the same data directory, that is
where the setting takes effect.

---

## Privacy

- Everything is stored locally, in `~/.agento`.
- Nothing is sent to Anthropic beyond what Claude Code itself sends when an agent
  runs.
- Agento has no account, no telemetry and no analytics of its own.
- The only outbound network calls are the ones you asked for: agent runs, the
  integrations you connected, and the update check (which you can switch off).
- Integration credentials are stored in plain text in the local database. Its
  protection is your user account and your disk. Treat `~/.agento` as sensitive.
- Your Claude Code transcripts in `~/.claude` are read only, never modified.

---

## Getting help

Something not working? Start with [Troubleshooting](troubleshooting.md), then open
an issue at
[github.com/shaharia-lab/agento/issues](https://github.com/shaharia-lab/agento/issues).
