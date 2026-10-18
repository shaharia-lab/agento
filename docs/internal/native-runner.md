# The `Runner` seam and each harness's fail-open combinations (#677)

> Part of Agento's working notes, split out of the root `CLAUDE.md` (#553).
> Read the root file's **How to read these notes** first: where a sentence
> here says a route "forwards", "falls back", or that "Go answers", it is
> history from the Go→Rust port — every route is served in-process now. A
> sentence about a *value* is a specification; a sentence about a *process*
> is a story. Regeneration commands for `parity/` goldens are traceability
> notes, not instructions: the goldens are frozen (`parity/README.md`).

**Every headless run reaches a CLI through `agent_run::Runner`, and through
nothing else.** The scheduler (`schedule/executor.rs::run_agent`), the Telegram
dispatcher (`trigger/dispatcher.rs::execute_and_reply`) and the Slack inbound
turn (`integrations/slack/inbound.rs`) each take the runner from
`agent_run::runner()` and call `.run(…)` or `.resume(…)`. The functions that
spawn, `run_headless` and `run_resumed`, are private to `native/agent_run.rs`,
so the compiler enforces it: a fourth caller cannot name them.

The interactive chat turn is **not** behind the seam. `chat/turn.rs` builds its
own options with a permission handler and streams SSE; it stays Claude-only.

## The trait

```rust
pub trait Runner: Send + Sync + Sized {
    fn harness(&self) -> &'static str;
    fn fail_open(&self, spec: &RunSpec) -> Option<String>;
    fn spawn(&self, checked: FailOpenChecked, spec, prompt, timeout, on_spawn) -> impl Future<Output = Result<RunResult, String>> + Send;
    fn run(&self, spec, prompt, timeout, on_spawn, gate: PermissionGate) -> impl Future<…> + Send; // provided
    fn resume(&self, db_path, chat_id, prompt, settings, timeout, on_spawn, gate) -> impl Future<…> + Send; // provided
}
```

- **`RunSpec`, `RunResult` and `ExecutionSettings` are the neutral types.** A
  caller builds a spec and reads a result without naming the CLI behind them.
- **`harness()`** is a stable lower-case id. `ClaudeRunner` answers `"claude"`.
- **`fail_open(spec)`** answers why a headless run of that spec would start
  with a flag combination under which a tool call nobody approved can run, or
  `None`. It is decided from the spec alone, before anything is spawned or
  bound. A spec the harness refuses to build (an unknown permission mode)
  answers `None`, because that run fails closed.
- **`spawn`** is the harness's own step: spec to flags, spawn, drain, result.
  Its first argument is a `FailOpenChecked`, whose field is private to
  `agent_run.rs`, so nothing outside that module can call it without going
  through the check below.
- **`run` is a provided method, and it is the refusal (#694).** It asks
  `fail_open(spec)` first. A fail-open spec under a `PermissionGate::NotFull`
  answers `Err` without spawning:
  `refused: harness claude would run fail-open (<reason>); only permission mode full allows it, and the task's is <mode>`
  (or `… and this run has no task`). Only `PermissionGate::Full` lets a
  fail-open spec through, and only `PermissionGate::for_task(Full)` builds one.
  Every caller has to name a gate, so the compiler makes each one say it.
- **`resume` is a provided method**, because none of it is the harness's. The
  chat's busy lock, the chat lookup and the additive write-back are Agento's,
  and the one harness-specific step in the middle is `self.run`, so a resumed
  run is refused exactly as a fresh one is, on the spec it resumes with (the
  chat's stored mode is only known once the row is read). A refused resume is
  a failed turn: the question is stored with no answer and the lock is
  released. The rules it keeps are in `docs/internal/native-chat.md` (the busy
  lock) and at `run_resumed`.
- **Who passes which gate.** The executor passes
  `PermissionGate::for_task(mode)` on every run of a task, fresh or continued,
  and a refusal is that run's `failed` `job_history` row and its `when:
  always` delivery. The unlinked Telegram rule (`dispatcher::execute_and_reply`)
  and the Slack inbound turn have no task, so they pass `NotFull`: a rule whose
  settings resolve to `bypass` is refused, logged, and answered with the error
  reply, and writes no `job_history` row.
- Pinned by `a_fail_open_spec_never_reaches_the_spawn_unless_the_gate_is_full`
  and `a_resumed_fail_open_run_is_refused_the_same_way` (a fake harness whose
  every spec is fail-open), `claude_refuses_only_what_it_declares_fail_open`,
  `claude_refuses_a_resumed_bypass_chat_with_no_task`,
  `tests/trigger_run.rs::a_bypass_rule_is_refused_and_spawns_nothing` and
  `slack/inbound/tests.rs::a_bypass_rule_is_refused_answered_and_spawns_nothing`.
- **Generic, not `dyn`.** The methods return futures, so a trait object would
  need them boxed, and every caller is on a `Send` path (a scheduler task, an
  inbound worker). `runner()` returns the concrete `ClaudeRunner`. A second
  harness makes `runner()` answer an enum that implements `Runner` by matching;
  no call site changes.

## What `ClaudeRunner` declares fail-open

`ClaudeRunner::fail_open` resolves the spec through
`chat/runner.rs::headless_permission_options`, which makes the same two calls
`build_options` makes (`PermissionChoice::resolve`, then
`PermissionChoice::apply`), and reads the two fields `claude/options.rs` turns
into flags. Two combinations are reported:

1. `--permission-mode bypassPermissions`. Every permission check is skipped.
2. `--allow-dangerously-skip-permissions`, with any mode. The CLI's help reads
   "Enable bypassing all permission checks as an option, without it being
   enabled by default". It skips nothing by itself, but a run that sends it can
   be switched to bypass while it runs, so it is not provably closed.

| Agento mode (stored) | Flags `build_options` sends, headless | Declared fail-open |
|---|---|---|
| `bypass` | `--permission-mode bypassPermissions --allow-dangerously-skip-permissions` | yes, by the mode |
| `plan` | `--permission-mode plan` | no |
| `dontAsk` | `--permission-mode dontAsk` | no |
| `default` | `--permission-mode default` | no |
| empty, agent has none (`Unchosen`) | `--permission-mode dontAsk` | no |
| anything else | the run fails: `agent setup: unknown permission mode "…"` | no (never starts) |

The run's own mode beats its agent's, so each row applies to the mode the run
names, or to the agent's when the run names none.

**A task's run reads none of those (#693).** Every run of a task — scheduled,
manual, event, continued — carries the task's `permission_mode` as
`RunSpec::task_mode`, and in `PermissionChoice::resolve` it beats the run's,
the agent's and the resumed chat's modes outright:

| Task mode (stored) | Flags `build_options` sends, headless | Declared fail-open |
|---|---|---|
| `full` | `--permission-mode bypassPermissions --allow-dangerously-skip-permissions` | yes, by the mode |
| `plan` | `--permission-mode plan` | no |
| `workspace_write` | `--permission-mode acceptEdits` | no |
| `read_only` | `--permission-mode dontAsk --allowedTools Read,Glob,Grep,WebFetch,WebSearch --disallowedTools …,Write,Edit,MultiEdit,NotebookEdit,Bash` | no |

The three non-`full` arms are built on `with_default_permissions()`, so
`--allow-dangerously-skip-permissions` is not sent for them. `plan` and
`workspace_write` keep the agent's allowlist: `acceptEdits` pre-approves edits
on top of it and shrinks nothing, so a `workspace_write` task with no agent, or
an agent that lists no tools, still pre-allows every built-in, `Bash` included.
`read_only`'s allowlist **replaces** the agent's (its MCP and local tools
included, which `dontAsk` then denies) and its five denials are added to
whatever the agent already denies; `--disallowedTools` also beats an allow
rule in a project's `settings.json`, which `dontAsk` alone does not. Pinned by
`each_task_mode_maps_to_its_claude_code_flags`,
`a_read_only_task_keeps_its_agents_own_denials`,
`claude_declares_only_a_full_task_fail_open`, and on the spawned argv by
`tests/trigger_run.rs::an_event_asking_for_more_permissions_runs_read_only_and_changes_no_task`.
`claude_declares_only_bypass_fail_open_for_every_mode` pins the
table over `chats::CHAT_PERMISSION_MODES`, and panics on a mode added there
without an answer here.

**A headless `plan` or `dontAsk` clears the flag (#694).** `Options::new()`
defaults it to true and only `with_default_permissions()` clears it, so until
#694 an explicitly chosen `plan` or `dontAsk` sent it and was declared
fail-open. `PermissionChoice::apply` now takes `interactive` and clears it for
those two modes when there is no permission handler; refusing them instead
would have made every unlinked rule on `plan` unrunnable. **The interactive
chat turn is unchanged**: it still sends the flag for `plan` and `dontAsk`
(`a_chats_own_permission_mode_beats_the_interactive_default`), and it is not
behind the seam. Pinned headless by
`a_headless_plan_or_dont_ask_run_is_not_offered_bypass`. So the only
fail-open headless runs are `bypass` and a task's `full`, and of those only a
task's `full` is let through.

**"Not fail-open" is not "read-only".** `dontAsk` denies only what
`--allowedTools` does not already cover, and an agent with no tool list is
pre-allowed all twelve built-ins, the shell included (#765). A task's
`read_only` closes that for task runs by naming the allowlist itself; a rule
with no task, and a chat, are still as wide as their agent's list.

## Per-harness table: modes, flags, fail-open

The four rows are a task's modes (#693), spelled `read_only`, `plan`,
`workspace_write` and `full` in `tasks::TaskPermissionMode`, mapped to what each
harness offers. Only the Claude Code column is implemented.

### Claude Code 2.1.287

Read from `claude --help` on this box, and from
`src-tauri/src/claude/options.rs`.

| Mode | Flags | Fail-open? |
|---|---|---|
| read-only | no sandbox flag; Agento sends `--permission-mode dontAsk` with a read-only `--allowedTools` and `--disallowedTools Write,Edit,MultiEdit,NotebookEdit,Bash` | no |
| plan | `--permission-mode plan` (headless: the allow flag is never sent, #694) | no |
| prompts denied | `--permission-mode dontAsk` (headless: the allow flag is never sent, #694), or `--permission-prompts none` | no, but as wide as `--allowedTools` |
| workspace-write | `--permission-mode acceptEdits` | no; edits are pre-approved, the rest still prompts |
| full | `--permission-mode bypassPermissions`, or `--dangerously-skip-permissions` | **yes** |
| (any) | plus `--allow-dangerously-skip-permissions` | **yes** (bypass becomes reachable) |

- 2.1.287's help lists the `--permission-mode` choices as `acceptEdits`, `auto`,
  `bypassPermissions`, `manual`, `dontAsk`, `plan`. `default` is not listed but
  is still accepted: `claude -p --permission-mode default` got past argument
  parsing where `--permission-mode bogus` was rejected with the choice list.
  Agento sends `default`.
- `--permission-prompts none` ("anything that would prompt is denied
  automatically") and `--safe-mode`'s "refuses bypassPermissions" are in the
  help and are not used by Agento.

### Codex CLI 0.160.0

Read from `npx @openai/codex@0.160.0 exec --help` and `… --help`, with no
login.

| Mode | Flags | Fail-open? |
|---|---|---|
| read-only | `--sandbox read-only` | no |
| plan | none on `codex exec`; `--sandbox read-only` is the nearest | no |
| workspace-write | `--sandbox workspace-write` (`--add-dir <DIR>` widens it) | no |
| full | `--sandbox danger-full-access` | **yes, with approval policy `never`** |
| full | `--dangerously-bypass-approvals-and-sandbox` | **yes**, by itself |

- **The fail-open combinations are `--sandbox danger-full-access` with
  `--ask-for-approval never`, and `--dangerously-bypass-approvals-and-sandbox`.**
- `-a, --ask-for-approval` takes `on-request` or `never` and is listed on the
  top-level `codex` command only. **`codex exec --help` does not list it** in
  0.160.0. On `exec` the policy would have to arrive through
  `-c approval_policy=…`, which was not tested, and what `exec` does with a
  command that needs approval when nobody is there was not tested either.
  Until that is known, a Codex runner should treat `danger-full-access` on
  `exec` as fail-open whatever the approval policy.
- Two more flags widen a run and belong on a refusal list: `--approve-for-me`
  ("Route approval requests through automatic review using the workspace-write
  sandbox") and `--dangerously-bypass-hook-trust`.
- `--ignore-user-config` and `-c key=value` are how a run avoids both reading
  and writing the user's `~/.codex/config.toml`.

### OpenCode 1.18.34

Read from <https://opencode.ai/docs/permissions/>, `/docs/cli/`,
`/docs/agents/` and `/docs/acp/` on 2026-10-04. The version is npm's `opencode-ai` latest; **the
binary was not run** (its postinstall did not run under `npx` here).

OpenCode has no sandbox. Each tool key (`read`, `edit`, `bash`, `webfetch`,
`task`, `external_directory`, …) is `allow`, `ask` or `deny`.

| Mode | Configuration | Fail-open? |
|---|---|---|
| (nothing sent) | the defaults: "Most permissions default to `allow`" (`external_directory` and `doom_loop` ask; `.env` reads are denied) | **yes** |
| read-only | `OPENCODE_PERMISSION='{"edit":"deny","bash":"deny",…}'` | no |
| plan | `opencode run --agent plan`: the built-in Plan agent, with file edits and `bash` at `ask` (`/docs/agents/`; `--agent` is on `run` in `/docs/cli/`) | **yes with `--auto`**; otherwise it rests on what `run` does with an `ask`, which is not known (below) |
| workspace-write | `edit: allow`, `bash: deny` or `ask`, `external_directory: deny` | no |
| full | `edit` and `bash` at `allow` | **yes** |
| (any) | `opencode run --auto` ("Auto-approve permissions that are not explicitly denied") | **yes** |

- **The fail-open combinations are the default configuration, any configuration
  that leaves `bash` or `edit` at `allow`, and `--auto`.** OpenCode is the one
  harness of the three that is fail-open when Agento sends nothing.
- `OPENCODE_PERMISSION` ("Inlined json permissions config") and
  `OPENCODE_CONFIG_CONTENT` set permissions per run through the environment,
  without writing the user's config.
- The docs do not say what `opencode run` does with an `ask` when `--auto` is
  absent.

## Does an ACP runner fit the trait?

**Yes at the signature, with four things to settle inside it.** ACP is
JSON-RPC over stdio to a subprocess (`opencode acp`): initialize, open a
session with a working directory and a list of MCP servers, send a prompt, read
streamed updates until the turn stops. That is `run`. Loading an earlier
session is `RunSpec.resume_session_id`, so the provided `resume` works
unchanged.

1. **`RunSpec` is neutral in name and Claude-shaped in content.**
   `settings_profile_id`, the agent's `claude_config_dir` and the permission
   vocabulary all mean something only to Claude Code, and
   `chat/runner.rs::build_options` turns a spec into Claude `Options`. A second
   runner needs its own spec-to-configuration mapping; the trait does not
   change.
2. **`fail_open` depends on the agent's configuration, not on the protocol.**
   Over ACP the agent asks the client before a tool call it is configured to
   ask about, and a headless client can deny every request. But OpenCode's
   defaults are `allow`, and an allowed call is never asked about. So an ACP
   runner's `fail_open` has to read the permission configuration it sends, as
   `ClaudeRunner` reads its flags. OpenCode's ACP page does not describe
   permission requests at all.
3. **`RunResult`'s four token counts** come from Claude Code's `result` frame.
   Whether an ACP prompt response carries usage was not established; a runner
   without them stores zeros.
4. **`on_spawn` is `crate::claude::SpawnHook`.** It is a pid callback and
   nothing about it is Claude's, but the type lives in the SDK port and would
   move when a second runner needs it.

Hosted tools fit ACP better than Codex: a session is opened with its MCP
servers, where Codex would need them as `-c` overrides.

## What was not tested

- No Codex or OpenCode run was made. Both tables are flags and documentation,
  with no login. The wider spike (`spike-brief-multi-harness-2026-09-30`: login,
  per-run MCP configuration, transcript location, terms of use) still needs a
  ChatGPT account.
- The OpenCode binary was not executed, so its flags are the docs' and not a
  `--help`'s.
- Whether `--allow-dangerously-skip-permissions` alone lets a *headless* Claude
  Code run reach bypass with no control request from the host was not tested.
  The declaration reports it because it cannot be ruled out from the help text.
- Claude Code's `auto` and `manual` modes, `--permission-prompts none` and
  `--safe-mode` were read, not run.
- What `opencode run` does with an `ask` when `--auto` is absent, which is
  what decides whether its Plan agent is closed on a headless run.
- No ACP session was opened.
- Windows and macOS: every command above was run on Linux.
