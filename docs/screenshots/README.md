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

One thing it documents is a real finding rather than a tooling detail: **Agento
never writes `session_insights`** — the processors exist but no worker runs
them, so the Insights page is empty on a fresh install and silently stale on a
migrated one. The Insights shots exist only because the skill backfills that
table out of band. That is a bug worth fixing, not a property of the
screenshots.
