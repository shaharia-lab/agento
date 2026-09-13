# The chat turn — SSE, permissions, the runner, mcps.yaml

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

## Things that will bite

- **Chat SSE is a POST response**, so `EventSource` cannot be used. Events are
  the raw Claude CLI JSON lines passed through verbatim, plus three synthetic
  ones Agento adds (`user_input_required`, `permission_request`,
  `tools_not_offered`).
- **`AskUserQuestion` is answered by *denying* the tool** with the user's text
  in `Message` — that is how the answer reaches the model. Not a bug.
- **The busy lock has two callers, and they answer with one string** (#564).
  `live::try_lock` was the interactive turn's alone while every headless run
  minted a fresh chat; `agent_run::run_resumed` runs a turn on an *existing*
  chat, so a UI send and an inbound one can now collide on one row — two CLI
  processes each reading the other's stale `sdk_session_id`. Both refuse with
  `live::CHAT_BUSY`, the chat route's own 409 body, because an inbound worker
  queues on exactly that string. It fences the two *runs*, not the two *writes*:
  the interactive turn still releases when its stream ends, before
  `persist::commit` lands (Go's ordering, argued in `live.rs`'s header), so a
  resume starting in that window reads an `sdk_session_id` the UI turn is about
  to write. Two CLI processes on one chat stay impossible; a resume from one
  turn earlier does not. `run_resumed` also **increments** the four
  token totals where `schedule/executor.rs` and `trigger/dispatcher.rs` replace
  them; replacing on a chat the user has also typed into erases the UI turns'
  usage. Both divergences are stated at `run_resumed` and pinned by
  `tests/headless_resume.rs` — do not unify that write-back with the executor's.
- **A continued chat's inherited history is the transcript's, and it is a fixed
  prefix** (#490, migration 37). "Continue in chat" records
  `continued_from_session_id` / `continued_from_project_path` /
  `continued_from_message_count` on `chat_sessions`; the conversation itself is
  never imported into `chat_messages`, because that would duplicate megabytes
  per session and corrupt the chat's own usage and cost figures, which are
  stored rather than derived. `ChatsView` re-reads the source through
  `GET /api/claude-sessions/{id}` and renders `messages.slice(0, count)` above
  its own turns. **The count is a boundary, not a total, and nothing may
  advance it**: the CLI appends a resumed turn to the *same* transcript file, so
  after the first Agento turn the source already holds the local messages and
  rendering the whole transcript beside `chat_messages` shows the newest turn
  twice. The **pair** is stored, not the id (the #362 family), and deliberately
  not `sdk_session_id` — `chat/persist.rs` rewrites that column from whatever
  the stream reports, so it is a live pointer rather than an identity.
- **Continue is idempotent per source session.** One Claude session, one chat:
  `continue_session` looks a chat up before inserting and reopens it, returning
  the same `201 {"chat_id"}`. The lookup also matches a chat whose **own id** is
  the session id, because a turn pins the CLI session to the chat id — so a chat
  Agento created itself is indexed in the corpus under that id, and "continue"
  on one of those reopens the chat it already is rather than cloning it. Rows
  written before migration 37 record no source and keep today's behaviour.
- **A chat's permission mode is the chat's, then the agent's, then `default`.**
  `chat_sessions.permission_mode` (migration 30) beats the rule that an
  interactive handler forces `default`; empty means "no choice", not a mode.
- **There is no terminal SSE event**, and `result` can arrive more than once in
  one request (an `AskUserQuestion` keeps the stream open past it). End the
  turn on stream close, never on `result`.
- **Mid-stream failures arrive as `result` with `is_error: true`**, not as an
  `error` event — HTTP 200 is already committed by then.
- **A turn that produced no final text persists nothing** — not even the user
  message. Keep locally-produced turns in memory until a fresh server
  transcript for that chat loads.

## The chat turn (#276)

**The seam grew a second registry.** `Answer` is a buffered `Vec<u8>` and
`Endpoint::serve` is a sync `fn` on `spawn_blocking` — right for thirteen areas
that hand back a finished document, wrong for a turn that lasts as long as the
model talks. `StreamEndpoint` is async and returns a `Response<Body>` built from
`Body::from_stream`. `native::claims` is the union of both, because the proxy
asks one question; `route_is_native` excludes the streaming ones so the buffered
path cannot try to answer a chat turn with a `Vec<u8>`.

**The four routes share a process-local registry, so they moved together** —
`/messages` puts a session in, the others look one up. But not every chat *can*
run natively: an agent whose tools come from an integration this build cannot
host still needs to fall back, and `runner::build_options` refuses those before
any subprocess exists — but since #313 that is `whatsapp` alone. (Most of that refusal is gone — the **local** server (#310)
and any agent naming only integrations in `HOSTED_TYPES`: **github** since #311,
**confluence** since #317, **jira** since #316, **slack** since #315, **telegram**
since #314, **google** since #313 — which is all six. `build_options`
starts each of them, and is `async` for that reason, returning the listener
handles alongside the options because dropping one stops its server.) That would strand `/stop` for a chat still running on Go, so
the three steering routes answer natively **only when Rust holds a live session
for that chat** and forward otherwise. Go then answers — correctly, because it
is the side that has the session.

Five rules that are silent when broken, all pinned by `tests/chat_turn.rs` —
including, since #298, the deny-with-the-user's-text half of `AskUserQuestion`.
That one was the gap the claim used to paper over: the suite drove
`AskUserQuestion` as an assistant `tool_use` block, which reaches
`extract_ask_user_question` and the post-result continuation and **not the
permission handler at all**. The fake CLI issues a real `can_use_tool` control
request now, and the assertion is on what the SDK wrote *back* — the whole
observable effect of that round trip is a `control_response`, so the fake CLI
logs every stdin line and the tests read it. Reverting the deny to an allow
leaves every frame unchanged and fails only there.

Covered with it: `wrap_permission_handler`'s allowlist (a tool the agent does not
name is denied **without a prompt** — the absence is the assertion), the
`AskUserQuestion` bypass of that allowlist, and the `permission_request` frame
with its allow and deny answers.

**The `init` frame is read, not only forwarded (#556).** `handle_event`'s
`system` arm diffs `SystemMessage::tools` — the list of tools the CLI actually
gave the model, MCP ones already qualified `mcp__<server>__<tool>` — against
what this turn hosted, and `runner::report_tools_offered` warns on the
difference. The two hops disagreeing **is** the defect signal: #501's
`report_hosted_tools` reads names off the *handle*, so it can say a server hosts
nothing, and only this frame can say whether what we hosted reached the model.
#555 is why it exists — every integration was dead for a CLI release while the
log, `"status":"connected"` and the inspector's tool count all said otherwise.

Three rules on it, each of which makes it worthless if broken. The diff is taken
against what the runner **asked for after `--allowedTools`/`--disallowedTools`**
(`runner::narrow_to_command_line`), because a narrowing allowlist that warned on
every turn is worse than silence. **The agreeing case emits nothing at all** —
no line, no frame. And a mismatch is a **warning, never a refusal**: the turn
runs and answers, for `start_local_tools`' own reason. Only a *whole* server's
tools vanishing from a server the CLI called `connected` reaches the user, as a
`tools_not_offered` frame here — rendered by `useChatStream`'s own case as the
amber `banner`, never the red `banner--error`, because the turn answered — and
as text on the `job_history` row for a headless run
(`agent_run::collect_run_result` → `executor::finish`), because a scheduled run
has nobody reading the log. **That row is the first `success` row that can
carry `error_message`**, so `JobsView` titles the group off the status rather
than off the column being non-empty. **The sentence names the server's *label*,
not its `mcpServers` key** — for an integration that key is a v4 UUID the user
has never seen — which is why it is `ToolsDropped::message()` rather than a
function taking a name: with two callers and a free function, one of them passed
the wrong field. The reason behind a miss is in the
CLI's own `--debug-file` log, which Agento never sees; `docs/troubleshooting.md`
carries the hand-run that gets it.

- **`result` is not terminal.** With an `AskUserQuestion` pending the same
  subprocess carries on, so one HTTP request spans several turns and several
  `result` frames. The turn ends on stream close, on an error result, or on a
  final result with nothing pending.
- **A mid-stream failure is a `result` with `is_error: true`**, never an `error`
  event — the 200 was committed before the first frame.
- **An event with no raw line emits nothing.** The SDK synthesizes process
  failures that way, so a crashed subprocess tells the client nothing and the
  stream just ends. Reproduced, not "fixed".
- **`AskUserQuestion` is answered by *denying* the tool** with the user's text as
  the message. That is how the answer reaches the model without the tool running.
- **A turn with no final text persists no messages** — not even the user's — but
  the session row is still written: `updated_at`, the token totals, and on a
  first message a title derived from a message that was never stored.

**The model a turn runs is the agent's or the no-agent branch's — never both**
(#299). `resolveAgentConfig` branches on whether the chat **names an agent**, not
on whether that agent has a model: it returns the agent's config outright, and
`runner.go` then sets a model only when `agentCfg.Model != ""`. So an agent with
an empty model gets **no model option from Agento at all** — the SDK's own
default (`claude-sonnet-4-6`, the same constant in both SDKs) is what reaches the
CLI — and the session's model and the user's default are read only in the
no-agent branch. `RunSpec.no_agent_model` is a closure for that reason: resolving
it eagerly loaded the settings row on every turn of every agent chat to throw the
answer away, *and* treated an agent's empty model as a request for a default Go
would never have given it.

It is named for the branch and not for the fallback because
`Options::fallback_model` already means something else in the same function — the
CLI's `--fallback-model`, for when the *primary* model is unavailable.

**Read the user's default through `settings::resolve`, never `load_stored`.** Go
reads `settingsMgr.Get()`, and `SettingsManager.load` fills `"sonnet"` when
nothing is stored *before* `applyEnvOverrides` applies `AGENTO_DEFAULT_MODEL` /
`ANTHROPIC_DEFAULT_SONNET_MODEL`. The raw `SELECT` has neither: a user who had
never saved settings ran on the SDK default rather than `sonnet` — two different
strings — and one who exported `AGENTO_DEFAULT_MODEL` had it silently ignored.
`resolve` is the documented mirror of `Get()`, and every other caller in the port
already goes through it.

**One `user_settings` read per turn, and the two resolutions on top of it are not
the same** (#340). Go needs no equivalent: `settingsMgr.Get()` is an in-memory
snapshot, so the config dir and the default model are free reads of one value.
This port has no manager, so each consumer opened its own read-only connection
and decoded the same row — twice for an ordinary turn, three times for one pinned
to a named settings profile, on the latency path of every message. #299 removed
the eager *model* read; it did not remove the config-dir one, and the PR should
not be read as having done so.

`runner::TurnSettings` is the shared load: a `OnceLock` over the **stored** row,
carried on `RunSpec` and shared with the `no_agent_model` closure via an `Arc`.
The value is not that it saves a connection — it is that **two consumers of one
row with different fields is the shape that drifts**, which is what #339's review
found when one of them read `load_stored` where Go reads the resolved settings
and the other already went through `resolve`. Putting the two accessors side by
side makes the asymmetry legible, and it is a real asymmetry rather than an
oversight:

- `default_model()` is `settingsMgr.Get().DefaultModel`, so it goes through
  `settings::resolve`.
- `run_config_dir()` is `config.ClaudeRunConfigDir`, which reads
  `claudeDirs.runOverride` — the value `ApplyClaudeDirs` **stored** — and applies
  `CLAUDE_CONFIG_DIR` itself, ahead of it. Handing it the *resolved* row instead
  would diverge for a `CLAUDE_CONFIG_DIR` that is set but not absolute: `resolve`
  overwrites the field with it, `absolute_dir` then rejects it, and a stored
  absolute dir Go would have used is skipped for the default.

So the shared thing is the stored row, and `resolve` is applied where Go applies
it and nowhere else. Two other properties are pinned by counting loads rather
than argued: a turn reads the row **at most once** however many fields it wants,
and an agent carrying an **absolute** `claude_config_dir` reads it **zero** times
— `ResolveAgentClaudeDir` returns before `ClaudeRunConfigDir` for the same
reason. An unreadable database is still `None` rather than a zero row, so the
model stays `""` (i.e. "set no model option") rather than becoming `resolve`'s
`"sonnet"`: a database this process cannot open is not a user who never saved
settings.

**The CLI's cwd is always set; an empty `working_directory` resolves to the
settings default** (#559). Four paths reach the spawn with `""` — `POST /api/chats`,
a task, a trigger rule left on *agent default*, and a continued session — and
`build_options` is the one hop all four share, so the resolution lives there:
`TurnSettings::default_working_dir()` is `resolve`'s `default_working_dir`, so
`AGENTO_WORKING_DIR`, else the stored setting, else `<temp>/agento/work`. It
deliberately does **not** copy `default_model()`'s "unreadable row answers `""`":
there is no "set no cwd" worth having, because that is inheriting the Agento
process's own cwd (`src-tauri/` under `npm run app`), which is the bug. The
directory is created when missing — nothing else creates `<temp>/agento/work` —
and a failed create is a `warn` left to the spawn to report. The stored row keeps
`""`; the Chats list reads `GET /settings` to show where such a chat runs. Since
`with_cwd` travels with `with_setting_sources(["project"])`, those runs also gained
the project setting source, pointed at the default directory
(`runner::tests::an_empty_working_dir_runs_in_the_settings_default`).

**An embedded raw value is compacted and HTML-escaped on the way out, and Go
does it on the way *in*** (#298). `encoding/json` runs
`compact(…, escapeHTML=true)` over a `Marshaler`'s output, so a nested
`json.RawMessage` is whitespace-stripped and has `<`, `>`, `&` and U+2028/9
escaped — while keeping its key order and number spelling. `serde_json` writes a
`RawValue`'s bytes as-is through `write_raw_fragment`, which `GoFormatter` never
sees. Two places that mattered:

- the **synthetic SSE frames**: Go ships `{"question":"a \u0026 b"}` where Rust
  shipped `{"question":"a & b"}`;
- the **stored `blocks` column**: Go compacts **on store**, not on emit — this
  file and `persist.rs` both used to say the opposite — so writing the SDK's bytes
  verbatim left the two implementations' databases different for the same input.
  It was masked on read, because `chats::decode_blocks` compacts what it loads,
  which is exactly why nothing noticed.

**The rule lives on the field, not at the construction site**, at all four of
them: the two SSE structs in `chat/turn.rs` that carry a raw value, plus
`chats::MessageBlock::input` and `sessions::detail::NormalizedBlock::input`.
(`ToolsNotOffered` is the third synthetic frame and carries none, so the rule
does not reach it — it is still encoded through `gojson` like every frame this
side constructs.) `MessageBlock` is why — it has
*two* sinks, the column via `persist::append_message` and the wire via
`GET /api/chats/{id}`, and it had two independent compaction points, one of which
was simply missing. A third construction path would have been silently wrong the
same way. The call-site `compact_raw`s are left as belt-and-braces; compaction is
idempotent, so nothing moved when the field-level rule went in.

**The `tool_use` input must never round-trip through a `serde_json::Value`.**
The first version of `append_assistant_blocks` did, and turned `{"z":1.50,"a":1}`
into `{"a":1,"z":1.5}` — sorted and respelled, with nothing to signal it.
`tests/chat_turn.rs` caught it, which is also why that test's fake CLI emits
literal bytes rather than `json.dumps`: Python normalises `1.50` to `1.5` and
adds spaces, so a byte-exactness test cannot go through it.

**The same rule holds in the other direction, on the input the tool actually
runs with** (#342). `can_use_tool`'s allow arm echoes the CLI's own tool input
back as `updatedInput`, and that echo used to go through a `serde_json::Value` —
so the CLI ran a re-sorted, re-spelled payload, on the *allow* path. Go's
`process.go` is `resp["updatedInput"] = envelope.Request.Input`, a
`json.RawMessage`, echoed verbatim. `PermissionResult::Allow::updated_input` is
therefore `Option<Box<RawValue>>` rather than a `Value` — bytes for both the
echo and a handler's rewrite — which cost no call site, since every handler in
the tree returns `PermissionResult::allow()`. Two consequences worth knowing:
`PermissionResult`'s `PartialEq` is hand-written, because `RawValue` has none and
byte equality is the only comparison the type can honestly offer; and
`write_control_success_raw` builds the two control-response envelopes as structs
whose **field order is the wire order**, spelled to match what `encoding/json`
does to Go's `map[string]any` (`response` before `type`; `request_id` before
`response` before `subtype`).

Both halves are pinned by asserting the **whole** `"updatedInput":…` substring of
the raw logged line — a per-key assertion passes against a reordered, respelled
object, which is why `tests/claude_sdk.rs` grew a `logged_line` beside
`logged_message`, and why the fake CLI now logs the stdin bytes it received
rather than a `json.dumps` of the decoded object.

**A disconnect has to be raced explicitly, not inferred.** The permission
handler is awaited *inline on the SDK's reader task*, so while it is parked no
events arrive and the stream loop has nothing to send — a closed tab is
invisible to every code path that would otherwise notice. All four unbounded
waits (the loop, the post-result continuation, and both permission arms) race
the body channel's closure, which is what Go gets from `r.Context().Done()`.
Without it, closing a tab on an open prompt held the busy lock and leaked a
`claude` subprocess for the life of the process.

The obvious way to write that is a **bug**, and the fix for it is the reason
`Answers.disconnect` is an `mpsc::WeakSender`. A plain `Sender` clone works for
detecting the disconnect, but this struct is reachable from the permission
handler, which the SDK's reader task owns until *stdout* hits EOF — so a strong
clone keeps the body's sender set non-empty past the end of the turn and the SSE
response stays open. That is ~5s when the CLI ignores `SIGTERM` and **unbounded**
when it leaves a grandchild holding stdout, which any backgrounding `Bash` call
produces. `useChatStream.ts` clears its streaming state only in `onDone`, so the
symptom is a chat stuck mid-stream with the composer blocked long after the
commit ran — and Go's handler returns as soon as `consumeAgentEvents` does, so
it is a parity break too. **Nothing that outlives the turn may hold a strong body
sender.**

Each of these is pinned by a test that fails when the fix is reverted —
`a_disconnect_while_a_prompt_is_pending_releases_the_chat` (reads frames until
the prompt arrives *before* disconnecting, or it would only exercise the
pre-existing failed-send path), its `..._silent_cli_...` sibling for the loop
arm, and `the_body_ends_with_the_turn_even_when_a_grandchild_holds_stdout_open`
for the sender strength. Assert the revert fails; a disconnect test that passes
either way is the easy mistake here.

**`AGENTO_CLAUDE_EXECUTABLE`** overrides which binary is spawned, falling back to
`claude_cli::executable()` and then the bare name — see *The one external
dependency* in `docs/internal/src-tauri.md` for the whole order and why a `PATH` scan was never enough. The
override is re-read per call rather than taken from the cache, and that is for
the tests rather than the app: a test binary whose cases each point at a
*different* scripted CLI would otherwise all run the first one's.

## How the runner resolves an MCP name

**How the runner resolves an MCP name, and the one thing it still refuses**
(#375). `chat/runner.rs::mcp_plan` walks `capabilities.mcp` in
`resolveServerConfig`'s order: **the `mcps.yaml` registry first**
(`native/chat/mcps_yaml.rs`), then the integrations. A registry hit is an
*external* server — somebody else's subprocess or URL, handed to the CLI in
`--mcp-config` with nothing bound here; an integration is a filtered in-process
server started per turn. Either is registered under **the name the agent
wrote** — the bare integration id, not `github::server_name`'s `github-<id>`:
the latter is `mcp.NewServer`'s implementation name and never appears on a tool,
while the key is the prefix on every qualified name already in an agent's
allowlist.

The order is Go's and it decides a real case: **a name in both is the yaml
entry**, so a user who shadows an integration id in their own file gets their
own server. One shape is refused, and it is a *refusal* rather than a fallback —
a chat reports it, a scheduled run records a failed `job_history` row: **a name
in neither**, which `whatsapp` reaches by construction since the type is dropped
(#273). A malformed or unreadable `mcps.yaml` refuses too, rather than reading
as "no external servers" and silently dropping a tool set.

**What #375 removed is the check that was broader than it needed to be:** the
mere *presence* of `<data dir>/mcps.yaml` used to refuse every `mcp` capability,
including names that resolved perfectly to hosted integrations. It was guarding
against a name resolving differently in the Go server than here — a hazard that
needed both to exist, and #391 deleted the Go tree. Until it went, one leftover
file (the normal state for anyone who ever ran `agento web` against the same
data directory) disabled every MCP-backed agent on the machine.

Two ordering rules keep that fix from being undone by accident. `mcp_plan` takes
the registry's **path**, not a loaded registry, and loads it only after
establishing that the agent names at least one MCP server — so a typo in the
file cannot refuse a turn that never consults it. And the database is opened
only for a name the registry did not answer, so an all-external agent reads no
`integrations` rows at all. `AGENTO_MCPS_FILE` overrides the path (with the
unprefixed `MCPS_FILE` honoured behind it, because #375's acceptance criteria
named that spelling), which is the only way to point a test at a fixture: a
debug build's `paths::data_dir` ignores the environment by design.

**Four things in `mcps_yaml.rs` were measured against `gopkg.in/yaml.v3` v3.0.1
rather than inferred from the JSON side**, and each is wrong in a way nothing
would report. Measure before changing any of them; the JSON intuition is wrong
about three.

- **Any scalar is a string.** `d.scalar` fills a `string` field from the node's
  own text whatever it resolved to, so `env: {PORT: 8080}`, `command: 123` and
  `DEBUG: true` all decode. `serde` type-checks and rejects them — which is
  *Part A's regression re-entered through the parser*, because `mcp_plan` loads
  the registry for **any** agent naming any MCP server, so one unquoted port
  number would refuse every MCP-backed agent on the machine including ones with
  no entry in the file. `YamlString` is the visitor that accepts them.
- **A null *sequence element* is dropped where a null *map value* is `""`.** So
  `args` is deliberately **not** a `GoList`: `["--f", ~]` is `docs-mcp --f`, not
  `docs-mcp --f ""`. `env`/`headers` keep `GoMap`, which is right for maps.
- **Merge keys (`<<: *anchor`) are expanded during decode** and need
  `Value::apply_merge` here, or a perfectly valid file refuses every MCP-backed
  agent with *"unknown transport"*.
- **A repeated mapping key refuses the whole file, at any depth**, and the
  refusal names the key but no value (#499; `DupChecked`, and the module
  header's *A repeated key refuses the file*). `serde_norway` 0.9.42 already
  refused one, but only through a serde message this module drops, so the user
  read a bare position. The check runs before merge expansion, so a key that
  `<<: *anchor` brings in and an entry also spells out is an override.

Two rules about what a failure here may *say*, both because this file holds
credentials and a refusal's text reaches a chat body, `job_history.error_message`
and the exported app log. **No decode failure quotes what it was decoding**: a
syntax error reports its position and a shape error reports the server name —
`native/integrations/registry.rs`'s rule, same reasoning. And **`McpSource`'s
`Debug` is hand-written to withhold an external config**, because `${ENV:…}` is
resolved at load, so the plan holds the live token the *file never contained*;
`McpPlan` is formatted in every panic message in that module.

The accepted cost of `apply_merge` is that a scalar is resolved before any field
reads it, so a spelling that does not survive a `Value` round trip does not
survive here: `1.50` is `"1.5"` and `0x10` is `"16"` where Go keeps the raw text.
Integers, booleans and strings are exact. Pinned, not reconciled.
