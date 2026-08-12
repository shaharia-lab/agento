# Claude Sessions, Analytics and Insights

Claude Code writes a JSONL transcript for every session it runs. Agento reads
those files, caches what it finds in its own SQLite database, and builds the
sessions browser, the cost analytics and the productivity insights on top of
them.

Nothing is uploaded and nothing is sent anywhere: the transcripts stay where
Claude Code wrote them, and Agento only ever reads them.

- [What Agento reads](#what-agento-reads)
- [Multiple Claude accounts](#multiple-claude-accounts)
- [How scanning works](#how-scanning-works)
- [Sessions list](#sessions-list)
- [Session detail and journey](#session-detail-and-journey)
- [Duration means active duration](#duration-means-active-duration)
- [Turns, not events](#turns-not-events)
- [Sub-agents](#sub-agents)
- [Cost](#cost)
- [Analytics dashboard](#analytics-dashboard)
- [Insights](#insights)
- [Hiding projects](#hiding-projects)
- [API reference](#api-reference)
- [Troubleshooting](#troubleshooting)

---

## What Agento reads

Inside a Claude Code config directory (`~/.claude` by default):

| Path | What Agento takes from it |
|------|---------------------------|
| `projects/<encoded-path>/<session-id>.jsonl` | The session transcript: prompts, replies, tool calls, token usage, model, git branch, permission mode |
| `projects/<encoded-path>/<session-id>/subagents/agent-*.jsonl` | Delegated sub-agent transcripts, plus their `.meta.json` sidecars |
| `todos/` | The session's todo list |
| `settings.json`, `settings_<slug>.json` | Claude settings and Agento's named settings profiles |

Beyond the conversation itself the scanner reads the events Claude Code writes
alongside it: `pr-link` (linked pull requests), `compact_boundary` (context
compactions and the tokens dropped by each), the `custom-title` / `ai-title`
naming events, and the `agent-name` / `permission-mode` / `mode` / `relocated` /
`worktree-state` metadata events.

Agento never writes to a transcript. Things you set in Agento — a renamed
session, a favourite — live in Agento's own database and always win over the
title Claude Code recorded.

---

## Multiple Claude accounts

If you run Claude Code under more than one account — work and personal, or two
organisations — each has its own config directory.

**Reading is a set; running is a choice.**

- **Every configured directory is indexed into one corpus.** Analytics is
  retrospective: a machine running two accounts wants both in every total.
  Manage the set in **Settings → Data & Analytics → Claude config directories**,
  which also offers any config directory it finds sitting beside the default one
  so you do not have to type a path. `~/.claude` and the directory agent runs
  target are always indexed and do not need listing.
- **A run authenticates as exactly one account.** Which one is resolved in this
  order: the agent's own `claude_config_dir` → the `CLAUDE_CONFIG_DIR`
  environment variable → the global setting (**Settings → General**) → `~/.claude`.
  See [Agents](agents.md#running-as-a-different-claude-account).

Two consequences worth knowing:

- **A session that exists in two directories is counted once.** Setting up a
  second account by copying the first duplicates every session ID; the first
  directory in the indexing order claims it, so tokens and cost are not
  doubled. The order is `~/.claude` first, then the directory agent runs target,
  then the extras as listed.
- **Removing a directory hides its sessions, it does not delete them.** The
  cached rows stay, so re-adding it costs no re-read. Likewise, a directory
  Agento cannot list right now — an unmounted drive, a permissions error — has
  its rows left alone rather than treated as deleted.

Filter the sessions list to one account with the config-directory filter.

---

## How scanning works

A scan compares each transcript's modification time against what is cached and
reads only what changed. Transcripts are decoded in parallel and written in
batches, so a full re-read of a large corpus is bounded by disk and CPU rather
than by one commit per file.

Scans run on startup, when a page that needs fresh data is opened, and on
demand from the sessions list. They run **in the background**: a request is
served from the cache immediately rather than waiting.

**A full re-read of every transcript is triggered by** — an Agento upgrade that
changes how transcripts are parsed, any edit to the [pricing catalog](pricing.md),
and any change to the [idle threshold](#duration-means-active-duration). All
three change figures that are *stored* per session, so they cannot be applied
without reading the transcripts again. This is normal and safe; on a large
corpus it takes a few minutes.

While a scan is running the sessions list shows its progress
(`Scanning ~/.claude… 412 / 1,373 transcripts`) instead of pretending there is
nothing there. `GET /api/claude-sessions/status` exposes the same information.

---

## Sessions list

The list is filtered, sorted and paged **in SQL**, so it behaves the same with
50 sessions or 5,000. Scrolling loads the next page by cursor rather than by
offset, which means a scan finishing mid-scroll cannot shuffle the pages you
have not reached yet.

Available filters:

| Filter | Notes |
|--------|-------|
| Search | Case-insensitive substring match across session ID, all three title fields, the preview and the project path. `%` and `_` are matched literally |
| Project | Hidden projects are never offered |
| Config directory | Which Claude account the session belongs to |
| Model, permission mode | Values come from the sessions actually present |
| Date range | Local-day boundaries, not UTC |
| Messages, duration, tokens in/out, cost | Inclusive min/max ranges |
| Links | Sessions with or without a linked pull request |
| Favourites | Sessions you starred in Agento |

Totals across the whole filtered set — not just the loaded page — come from a
separate facets request, which is also where the dropdown options and the token
bar scale come from. Day headers group the rows already loaded, so a day's
roll-up always equals the rows shown beneath it.

Every row carries the session's title, project, model, turn count, duration,
tokens, cost, git branch, permission mode and any linked PRs.

---

## Session detail and journey

The detail page shows the session's own metrics — turns, steps per turn,
longest autonomous chain, active duration, time Claude spent working, your
average reply time, tool error rate — plus its token and cost breakdown, its
compactions, and its linked pull requests.

**Journey** reconstructs the run step by step: every prompt, response, tool call
and tool result in order, with each sub-agent's steps nested underneath the
`Task` call that spawned it. A sub-agent whose spawning call cannot be matched
is appended to its turn rather than dropped. The journey is built on read, so it
always reflects the current transcript.

**Continue** resumes the session in a new Agento chat.

---

## Duration means active duration

Claude Code sessions are resumable. A session started on Monday and picked up
the following Monday spans a week — which is not a week of work. Everywhere
Agento shows a duration it means **active** duration: the sum of the gaps
between consecutive events, with any gap longer than the idle threshold
excluded.

The raw first-seen → last-touched span is still shown as secondary context,
because it answers a different question.

**The threshold is yours to set** — **Settings → Data & Analytics → Idle
Threshold**, default 10 minutes, allowed range 1–240 minutes. Below a minute,
reading one long reply ends the sitting; past four hours, active duration stops
differing from the wall-clock span it exists to be different from.

Changing it re-reads every transcript, because the durations are stored rather
than recomputed on read. The same threshold decides:

- the Avg Duration figure and the duration column and filter in the list,
- **Claude working time** — the subset of active gaps that end at a reply from
  Claude,
- the response-time averages, which *exclude* an over-threshold gap rather than
  capping it: a resume is a new sitting, not a 226-hour reply.

---

## Turns, not events

A transcript's raw event count is not a conversation length. Claude Code injects
events that look like user messages but are not: `<system-reminder>` blocks,
slash-command expansions, task notifications, skill-invocation preambles,
`[Request interrupted by user]` markers, and every tool result.

Agento counts a **turn** only when a user event carries genuine human input,
and it applies that same rule in three places — the session's message count, the
insight pipeline's turn segmentation, and the journey's turn boundaries — so
they cannot disagree. The raw event total is kept separately.

Everything derived from turns follows: steps per turn, autonomy score, tokens
per turn, longest autonomous chain, and the response-time averages.

One deliberate exception: a session that contains *no* genuine turn — a slash
command and its expansion, and nothing else — still takes its **preview text**
from the injected wrapper, rendered as `/my-command` or `skill: my-skill`
rather than raw tag soup. An unidentifiable blank row is worse than an
imperfect label.

---

## Sub-agents

Delegated work is scanned from `<session-id>/subagents/agent-*.jsonl` and rolled
up **additively**:

| Figure | Covers |
|--------|--------|
| Usage / Cost | The main thread only |
| Sub-agent usage / cost | Delegated work only |
| Total | Both, and what every aggregate report uses |

**Model attribution is the one deliberate exception.** A sub-agent routinely
runs a different model from its parent, so tokens and cost are credited to the
model that actually spent them — otherwise the one chart that should answer *is
delegation routing work to a cheaper model?* would be incapable of answering it.
Session *counts* per model still follow the parent, because a session belongs to
whoever ran it.

---

## Cost

Cost is computed **per assistant message**, at that message's own model and
timestamp, against the effective-dated [pricing catalog](pricing.md). It is then
stored on the session, because a stored total carries neither the model nor the
timing of the messages behind it and no later pass could reconstruct it.

- **Token types are priced separately.** Input, output, cache reads and cache
  writes bill at very different rates, and cache writes are split by TTL
  (5-minute vs 1-hour) because those tiers bill differently.
- **Cost is also stored keyed by the model that spent it**, which is what makes
  "where is my money going" answerable. Those per-model figures always sum to
  the session total; they re-key money, they never change it.
- **A model with no published rate contributes no cost.** Its tokens are counted
  and reported separately as unknown, so a partly-priced total is disclosed as a
  floor rather than presented as complete. This is different from a model that
  is genuinely free, which resolves and contributes a confident $0.00.

Editing a rate re-prices history on the next scan — see
[Pricing](pricing.md#maintaining-rates-from-the-ui).

---

## Analytics dashboard

**Granularity follows the window** — hourly up to 7 days, daily to 120, weekly
to 3 years, monthly to 12 years, yearly beyond. The series is never truncated,
because a truncated series would be a lie about the window you asked for.

**Everything is bucketed in your browser's timezone.** Storage and transport are
UTC end to end; the frontend sends its IANA zone name and the backend applies it
before deriving any day, hour or weekday. A bare `YYYY-MM-DD` date bound is a
local day boundary. See [Development → Timezones](development.md#timezones).

**Activity by hour counts a session in every hour it was running**, sharing its
tokens out by overlap, rather than only in the hour it ended. Bucketing at the
end time made the chart answer *when do my sessions finish?*. As a result the
per-hour session counts add up to more than the session total — by design, and
stated in the UI. Clicking an hour or a heatmap cell drills into the sessions
that were running then.

**Project breakdown** folds everything past the top 20 into a single `Other
projects` row that carries the count it stands for, so the table stays readable
and its total stays the window's total.

Reports are memoised per window, and the key includes everything that can change
the answer — the last scan, the pricing revision, the idle threshold and the
hidden-project set — so there is no window in which a stale report is served.

---

## Insights

The insight pipeline runs nine processors over each session's transcript and its
sub-agent transcripts, and stores the results. A background worker reprocesses
sessions as they change, sweeping every five minutes.

**Attribution** breaks tool calls down by six dimensions: the skill, the plugin,
the MCP server, the MCP tool, the sub-agent responsible, and the reasoning-effort
tier. The MCP-tool panel sits directly under the MCP-server one because it is
that chart's drill-down. `agent_breakdown` is empty for main-thread-only work,
since Claude Code only stamps it on sub-agent transcripts.

**Cache hit rate has exactly one definition**: cache reads as a share of *every*
input-side token — fresh input plus cache writes plus cache reads. Under that
denominator a model with no prompt caching scores 0 rather than being excused,
which is the whole point of the metric.

The Insights page also derives actionable cards from the stored data. The
cache-savings card is the one figure anywhere that prices a counterfactual, and
it is labelled `Estimated` for exactly that reason.

---

## Hiding projects

Some projects do not belong in your numbers — a scratch directory, a client's
repository, an experiment that skews every average.

**Settings → Data & Analytics → Excluded projects** removes a project from every
figure Agento reports: the sessions list, the analytics dashboard and the insight
cards alike. No project picker will offer it either.

Hidden is not deleted. The transcripts are still scanned and the cached rows stay
correct, so unhiding is immediate and costs no re-read.

---

## API reference

| Endpoint | Purpose |
|----------|---------|
| `GET /api/claude-sessions` | Paged, filtered session list — returns `{items, next_cursor, has_more}` |
| `GET /api/claude-sessions/facets` | Totals, dropdown options and scales across the filtered set |
| `GET /api/claude-sessions/projects` | Projects for the picker (`?include_hidden=true` to include excluded ones) |
| `GET /api/claude-sessions/status` | `files_done` / `files_total` / `scan_in_progress` / `costs_stale` |
| `POST /api/claude-sessions/refresh` | Request a scan |
| `GET /api/claude-sessions/{id}` | One session with its full detail |
| `GET /api/claude-sessions/{id}/insights` | Stored insights for one session |
| `GET /api/claude-sessions/{id}/journey` | Step-by-step timeline, sub-agents nested |
| `POST /api/claude-sessions/{id}/continue` | Resume the session in a new Agento chat |
| `GET /api/claude-sessions/insights/summary` | Aggregate insights for a window |
| `GET /api/claude-analytics` | The analytics report for a window |

List query parameters: `project`, `config_dir`, `q`, `favorites`, `links`
(`any` / `with` / `without`), `permission_mode`, `model`, `from`, `to`,
`sort`, `limit`, `cursor`, and inclusive `_min` / `_max` pairs for `messages`,
`duration`, `tokens_in`, `tokens_out` and `cost`. Analytics and insights
endpoints additionally take `tz` (an IANA zone name).

A malformed numeric bound is ignored rather than rejected — those arrive from a
number input a user is halfway through typing, and blanking the list between
keystrokes would be worse.

---

## Troubleshooting

**"No Claude sessions found" on a machine that has plenty.**
Check that the config directory is indexed (**Settings → Data & Analytics**).
If a scan is running, the list says so and how far along it is.

**Everything is re-reading after an upgrade or a settings change.**
Expected. An upgrade that changes parsing, a pricing edit or an idle-threshold
change all invalidate stored per-session figures. Let it finish once.

**Tokens show up but cost is missing.**
The model has no rate in the catalog. Unpriced models are listed at the top of
**Settings → Model Pricing** — see [Pricing](pricing.md).

**A session appears under the wrong account.**
When the same session ID exists in two config directories, the first one in the
indexing order claims it — `~/.claude`, then the directory agent runs target,
then the extras. Changing which directory runs target (**Settings → General**,
or `CLAUDE_CONFIG_DIR`) changes which of the two wins.

**Durations look far too long.**
Check the idle threshold. A very high value makes active duration converge on
the wall-clock span.
