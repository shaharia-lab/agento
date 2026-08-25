# Screenshots for the README

Screenshots of **Agento** taken from the real Tauri webview (WebKitGTK,
1440×900 viewport, DPR 1, Linux, light theme), in `light/`.

**Every visible number, project, prompt, branch, PR, agent, task, integration
and chat is synthetic.** They were generated against an isolated fake `HOME`
(`~/.agento-demo`), so nothing from the real `~/.agento` or `~/.claude` was read
or written. See "How they were made" for what is still worth a designer's eye.

## What each shot shows

| `light/…` | View |
|---|---|
| `insights.png` | Insights, top: cards, tool-call attribution, top tools / top agents |
| `insights-breakdowns.png` | Insights, scrolled: skills, plugins, MCP servers/tools, reasoning effort |
| `token-usage.png` | Token Usage, top: cards, composition bar, tokens over time, cache efficiency |
| `cost-by-model.png` | Token Usage, scrolled: cost by model, per-project table, "what the numbers suggest" |
| `activity-heatmap.png` | General Usage, weekly rhythm heatmap |
| `general-usage.png` | General Usage, top |
| `top-sessions.png` | General Usage, ranked sessions |
| `sessions-list.png` | Sessions list with the inspector pane |
| `session-journey.png` | One session's transcript (tool calls, a sub-agent delegation expanded, edits, a failing test run) — the step-by-step replay |
| `session-detail.png` | The same session from the top, with the inspector (activity, tokens, sub-agents) |
| `chats.png` | A chat with an agent: tool calls and a Markdown answer |
| `agents.png` | Agents list + the builder (identity / behaviour) |
| `agent-builder.png` | The builder scrolled to Capabilities: built-in, local and per-integration tool allowlists |
| `tasks.png` | Scheduled Tasks with one task open and its recent runs |
| `job-history.png` | Job history |
| `integrations.png` | Integrations: four connected, GitHub open with its services and tools |
| `settings-pricing.png` | Settings → Pricing, scrolled to the Anthropic rows |
| `settings-data.png` | Settings → Data: idle threshold, hidden projects |
| `settings-claude.png` | Settings → Claude |
| `gateway-overview.png` | LLM Gateway → Overview: listener state, the token, the per-client env snippets |
| `gateway-providers.png` | LLM Gateway → Providers: one upstream account, its adapter, base URL, key field and timeouts |
| `gateway-models.png` | LLM Gateway → Models: an alias with ordered targets and a fallback |
| `gateway-usage.png` | LLM Gateway → Usage: cards, requests / tokens / spend over time, breakdowns by alias and provider |
| `gateway-settings.png` | LLM Gateway → Gateway Settings: enable, port, start with the app, retention horizon |

## The gateway shots are not from the synthetic corpus

The five `gateway-*.png` files are the exception to everything above: they were
taken from a live gateway configuration rather than the fake `HOME`, because the
gateway's views are populated by provider accounts and served requests, and the
synthetic corpus has neither.

Nothing secret is on screen, and the app is what guarantees it rather than a
crop: an API key is never returned by any read, so the Providers field is empty
by construction, and a gateway token is shown once at mint time and stored
nowhere, so the Overview snippets read `<your gateway token>`. What the shots do
carry is the provider names, base URL, model ids and spend of a real setup
(Moonshot and Z.AI GLM, $1.71 over 14 requests). Redo them the same way, from a
configured gateway in the `light` theme at 1440x900, rather than through the
`app-demo-ui-screenshot` skill.

## What a designer may still want to touch

Everything is synthetic, but four things are worth knowing before reuse:

1. **The session inspector's `Config` row** shows the fake home's Claude
   directory (`/home/<user>/.agento-demo/.claude`, truncated). It is the only
   place the real OS user name is visible; the list and breadcrumbs collapse it
   to `~/…`.
2. **Tool-call arguments say `/home/dev/Projects/…`** (a neutral, invented
   home) while project paths display as `~/Projects/…`. Nothing cross-checks
   the two.
3. **Scheduled tasks read "not scheduled"** in the list and show no working
   directory. Both are faithful: the API never populates `next_run_at`, and the
   create body discards `working_directory`/`model`.
4. The **hero session** opened in `session-detail.png` / `session-journey.png`
   is scripted to be coherent ("Harden the file upload validation"); the other
   300 sessions have plausible titles and tool mixes but their prose is drawn
   from a small pool, so do not open a random one expecting a story.

## How they were made (and how to redo them)

By the `app-demo-ui-screenshot` skill
(`.claude/skills/app-demo-ui-screenshot/`): a synthetic Claude Code corpus
generated into a fake `HOME` (`~/.agento-demo`), the dev app launched against
that home so its data dir and scanner never see the real install, agents /
integrations / tasks seeded through the app's own API, and every view
photographed from the real webview via the `ui-verify` skill. The skill's
`SKILL.md` carries the procedure and the traps; run it again after a UI change
and replace `light/`.

This set was where a real bug surfaced rather than a tooling detail: Agento
never wrote `session_insights` — the processors existed but no worker ran them,
so the Insights page was empty on a fresh install and silently stale on a
migrated one, and the Insights shots could only be taken by backfilling the
table out of band with a Go writer built from git history. Fixed by #408; the
app populates the table itself now, so the only thing left of it is that an
Insights shot taken within a minute of launch may still catch the sweep
mid-flight.
