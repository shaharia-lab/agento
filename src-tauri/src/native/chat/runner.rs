//! An agent's stored config, turned into the SDK options one chat turn runs
//! under. Mirrors `buildSDKOptions` (`internal/agent/runner.go`).
//!
//! # What this deliberately cannot do yet, and why that is safe
//!
//! Go resolves three kinds of tool: the built-ins, the **local** in-process MCP
//! server (`internal/tools`), and one server per name in `capabilities.mcp` —
//! which `resolveServerConfig` looks up first in the `mcps.yaml` registry and
//! then among the **integrations** (`internal/integrations`). An agent that
//! reached a subprocess with some of those silently missing would be a worse
//! answer than none, so [`build_options`] returns `Err` and the turn is
//! **refused** rather than run degraded. In a chat that is a 500 carrying the
//! reason, produced before any subprocess exists; in a scheduled run it is a
//! recorded `job_history` failure. Silence is the one outcome not allowed.
//!
//! The `local` half of that refusal went in #310: [`build_options`] starts the
//! local tools server itself and hands back the handle, because the listener's
//! life is the handle's — see [`crate::native::tools`]. That is what makes this
//! function `async`.
//!
//! **#311 narrowed the `mcp` half and #375 closed it.** [`mcp_plan`] now
//! resolves a `capabilities.mcp` name exactly as `resolveServerConfig` does, in
//! its order:
//!
//! 1. the **`mcps.yaml` registry** ([`super::mcps_yaml`]) — an external server,
//!    somebody else's subprocess or URL, handed to the CLI in `--mcp-config`;
//! 2. then the **integrations**, hosted in this process as a filtered
//!    in-process MCP server — `registry::HOSTED_TYPES`, which since #313 means
//!    **all six**: `github` (#312), `confluence` (#317), `jira` (#316), `slack`
//!    (#315), `telegram` (#314) and `google` (#313).
//!
//! **The order is load-bearing and it is Go's, not a preference.** A name in
//! *both* is the yaml entry, because `resolveServerConfig` returns the registry
//! hit before it ever looks at the integrations — so a user who shadows an
//! integration id in their own file gets their own server, which is the only
//! answer that keeps a config authored for `agento web` meaning the same thing
//! here.
//!
//! One shape is still refused, and it is the honest one: **a name in neither**.
//! `whatsapp` reaches it by construction — the type is dropped rather than
//! deferred (#273), so a `whatsapp` row is not hostable and nothing else can
//! resolve the name unless the user's own `mcps.yaml` does. Running the turn
//! anyway would silently drop that server's tools, and an agent that quietly
//! loses a tool set gives a worse answer than one that says it cannot run.
//!
//! **What #375 removed is the check that was broader than it needed to be**: the
//! mere *presence* of `<data dir>/mcps.yaml` used to refuse every `mcp`
//! capability, including names that resolved perfectly to hosted integrations.
//! Its stated reason was that a name could resolve to a different server in the
//! Go server than in this port — a hazard that required both to exist, and #391
//! deleted the Go tree. Until it went, one leftover file broke every MCP-backed
//! agent on the machine, which is the state anyone who ever ran `agento web`
//! against the same data directory was in.
//!
//! A **malformed or unreadable** registry is still an error, and it refuses only
//! the turns that would have consulted it: [`mcp_plan`] loads the file after
//! establishing that the agent names at least one MCP server, so an agent that
//! names none is untouched by a typo in it. That ordering is Part A's own
//! lesson applied to Part B, and it is why the file's *path* rather than a
//! pre-loaded registry is what this function takes.

use rusqlite::OptionalExtension;

use crate::claude::options::{permission_mode, Options};
use crate::claude::permissions::PermissionHandler;
use crate::claude::InProcessMcpServer;
use crate::native::agents::{Agent, Capabilities};

/// Every built-in tool, as `allBuiltInTools` lists them. Order is Go's, because
/// it becomes the `--allowedTools` argument and a reordered list is a different
/// command line.
const ALL_BUILT_IN_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Task",
    "TaskOutput",
    "TaskStop",
    "NotebookEdit",
];

/// The `user_settings` row a turn reads — **at most once**, however many of its
/// fields are wanted (#340).
///
/// Go has no equivalent because it needs none: `settingsMgr.Get()` is an
/// in-memory snapshot, so `ClaudeRunConfigDir` and `DefaultModel` are free reads
/// of one value. This port has no manager, so each consumer used to open its own
/// read-only connection and decode the same row — twice for an ordinary turn,
/// three times for one pinned to a named settings profile, on the latency path
/// of every message, for a value that changes about never.
///
/// The cost was not really the connection. It is that **two consumers of one row
/// with different fields is the shape that drifts**: #339's review found that one
/// of them read `load_stored` where Go reads the resolved settings, while the
/// other already went through `resolve`. One load makes that impossible to get
/// wrong twice — and it puts the two resolutions side by side, which is what
/// makes the asymmetry below legible rather than accidental.
///
/// **Each accessor names the Go function it mirrors, and they do not agree**:
///
/// - [`Self::default_model`] is `settingsMgr.Get().DefaultModel`, so it goes
///   through `settings::resolve` — that is what fills `"sonnet"` when nothing is
///   stored and what applies `AGENTO_DEFAULT_MODEL` /
///   `ANTHROPIC_DEFAULT_SONNET_MODEL`.
/// - [`Self::run_config_dir`] is `config.ClaudeRunConfigDir`, which reads
///   `claudeDirs.runOverride` — the value `ApplyClaudeDirs` **stored**, not the
///   resolved settings — and applies `CLAUDE_CONFIG_DIR` itself, ahead of it.
///   Passing it the resolved row instead would diverge for a
///   `CLAUDE_CONFIG_DIR` that is set but not absolute: `resolve` overwrites the
///   field with it, `absolute_dir` then rejects it, and a stored absolute dir Go
///   would have used is skipped for the default.
///
/// So the shared thing is the **stored row**; `resolve` is a pure function
/// applied where Go applies it, and nowhere else.
pub struct TurnSettings {
    /// `None` when there is no database to read — the case the unit tests use
    /// to prove a branch never looked.
    db_path: Option<std::path::PathBuf>,
    /// `None` *inside* the lock means the row could not be read. That is
    /// distinct from a zero row: both resolvers degrade to their own defaults
    /// on it rather than to `resolve`'s, which is what the two separate reads
    /// did and is why an unreadable database still yields "no model option"
    /// rather than `"sonnet"`.
    row: std::sync::OnceLock<Option<crate::native::settings::UserSettings>>,
    /// Observability, so a test can assert "at most once" rather than assuming
    /// it. A `OnceLock` makes over-reading unrepresentable; this makes
    /// *under*-reading — a branch that should never have looked — visible.
    loads: std::sync::atomic::AtomicUsize,
}

impl TurnSettings {
    /// The settings behind `db_path`, read on first use.
    pub fn from_db(db_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            db_path: Some(db_path.into()),
            row: std::sync::OnceLock::new(),
            loads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// No database: every field is its zero value, and nothing is opened.
    pub fn none() -> Self {
        Self {
            db_path: None,
            row: std::sync::OnceLock::new(),
            loads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The stored row, loaded once. An unreadable database is `None`, not a
    /// failure: a turn that cannot see the settings still runs, on whatever
    /// each resolver's own fallback is.
    fn stored(&self) -> Option<&crate::native::settings::UserSettings> {
        self.row
            .get_or_init(|| {
                self.loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let db_path = self.db_path.as_deref()?;
                let conn = crate::native::db::open_read_only(db_path).ok()?;
                Some(crate::native::settings::load_stored(&conn))
            })
            .as_ref()
    }

    /// `settingsMgr.Get().DefaultModel` — see the type's docs for why this one
    /// goes through `resolve` and [`Self::run_config_dir`] does not.
    ///
    /// No row at all answers `""`, which `build_options` reads as "set no model
    /// option" rather than as a failure. Deliberately *not* `resolve`'s
    /// `"sonnet"`: a database this process cannot open is not a user who has
    /// never saved settings, and the turn is better off on the SDK's own
    /// default than on a value invented from a read that did not happen.
    pub(crate) fn default_model(&self) -> String {
        let Some(row) = self.stored() else {
            return String::new();
        };
        crate::native::settings::resolve(row.clone())
            .settings
            .default_model
    }

    /// `config.ClaudeRunConfigDir`: `CLAUDE_CONFIG_DIR`, else the stored global
    /// setting, else the default.
    fn run_config_dir(&self) -> String {
        let stored = self
            .stored()
            .map_or("", |row| row.claude_config_dir.as_str());
        crate::native::settings::run_config_dir(stored)
    }

    /// How many times the row was actually read. The acceptance criterion of
    /// #340 is a number, so it is asserted rather than argued.
    #[cfg(test)]
    fn loads(&self) -> usize {
        self.loads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// What the chat service knows about the turn before the subprocess starts.
pub struct RunSpec {
    /// The agent, or `None` for a chat with no agent slug — which Go models as
    /// a synthesized config carrying only a model.
    pub agent: Option<Agent>,
    /// The model a chat with **no agent** runs: `session.model`, or the user's
    /// default when the session has none.
    ///
    /// Named for the branch rather than for the fallback, because
    /// [`crate::claude::options::Options::fallback_model`] already means
    /// something else in this very function — the CLI's `--fallback-model`, used
    /// when the *primary* model is unavailable.
    ///
    /// A closure, because computing it opens a second read-only SQLite
    /// connection and loads the settings row — and Go never does that for a
    /// chat that names an agent. `resolveAgentConfig` returns the agent's own
    /// config outright in that case and the default is only read in the
    /// no-agent branch, so an eager value is work whose result is thrown away.
    /// Called at most once, in the one arm below that needs it.
    pub no_agent_model: Box<dyn Fn() -> String + Send + Sync>,
    /// The `user_settings` row, shared by every consumer in this file *and* by
    /// the closure above — hence the `Arc`. See [`TurnSettings`].
    pub settings: std::sync::Arc<TurnSettings>,
    pub working_dir: String,
    pub settings_profile_id: String,
    /// `RunOptions.PermissionMode` — the conversation's own choice, which
    /// overrides both the agent's configured mode and the
    /// interactive-handler-forces-default rule in [`build_options`]. Empty is
    /// "no choice recorded", which is what every caller but the chat turn
    /// passes, and it leaves the pre-existing rules exactly as they were.
    pub permission_mode: String,
    /// `Some` resumes an existing CLI session; `None` falls through to
    /// [`Self::custom_session_id`].
    pub resume_session_id: Option<String>,
    /// `RunOptions.CustomSessionID`. A chat turn passes its chat id, so a new
    /// CLI session and the row that owns it share one identifier. **A scheduled
    /// run passes `""`** — Go's `buildRunOptions` sets neither session field, so
    /// the CLI generates its own id and `saveSessionResults` stores it back onto
    /// the chat row afterwards. Pinning one there would be a different id from
    /// the one the transcript is filed under.
    pub custom_session_id: String,
}

/// Build the SDK options for one chat turn.
///
/// `Err` means "this build cannot run this chat", and it is answered rather
/// than run degraded — a 500 in a chat, a recorded failure in a scheduled run.
/// Every failure here happens before a subprocess exists, including the one
/// that binds a port: a local tools server that failed to start is dropped on
/// the way out, so a refusal leaves no listener behind.
///
/// The second half of the pair is the **tool listeners** this turn owns: the
/// local tools server for an agent that named a local tool, plus one per
/// integration in its `capabilities.mcp`. The caller owns them, and dropping one
/// stops its server — so they have to outlive the subprocess that dials them.
pub async fn build_options(
    spec: &RunSpec,
    permission_handler: Option<PermissionHandler>,
) -> Result<(Options, Vec<InProcessMcpServer>), String> {
    let caps = spec.agent.as_ref().map(|a| &a.capabilities);
    // Decided **before** anything binds a port. Everything below this line
    // either cannot fail or is dropped on the way out, so a refusal leaves no
    // listener behind.
    let mcp_plan = mcp_plan(
        caps,
        crate::paths::database_path().as_deref(),
        super::mcps_yaml::path().as_deref(),
    )?;

    // `--include-partial-messages` is unconditional in Go, and it is what makes
    // `stream_event` frames exist at all — the UI's token-by-token rendering
    // depends on them.
    let mut opts = Options::new().with_include_partial_messages();

    // A working dir also selects the project setting source, exactly as Go
    // does. Note `--settings` below can suppress it; that is Go's behaviour too.
    if !spec.working_dir.is_empty() {
        opts = opts
            .with_cwd(spec.working_dir.clone())
            .with_setting_sources(["project"]);
    }

    let config_dir = resolve_agent_config_dir(spec.agent.as_ref(), &spec.settings);
    if config_dir != crate::native::settings::default_claude_config_dir()
        || std::env::var("CLAUDE_CONFIG_DIR").is_ok_and(|v| !v.is_empty())
    {
        // Overrides the value the process inherited, which is what lets a
        // per-agent override beat the `CLAUDE_CONFIG_DIR` the app started with.
        opts = opts.with_env([("CLAUDE_CONFIG_DIR", config_dir.clone())]);
    }

    if let Some(path) = settings_file_in(&config_dir, &spec.settings_profile_id, &spec.settings) {
        // Only name a file that exists: a config dir Claude Code has never
        // written has no settings.json, and `--settings` on a missing path is
        // an error rather than a no-op.
        opts = opts.with_settings(path);
    }

    // `appendPermissionOpts`, in Go's own precedence order.
    //
    // The run's own mode wins outright. It is the conversation-level choice a
    // user made in the New Chat bar, and it is the only way past the
    // interactive branch below — which is why it exists: a chat *always* has a
    // permission handler, so before migration 30 there was no way to say "stop
    // asking me" for one conversation, and a `plan` or `dontAsk` agent silently
    // behaved as `default` in the chat UI.
    //
    // Absent that choice the pre-existing rules are untouched: an interactive
    // permission handler forces default permissions, overriding whatever the
    // agent configured; **without one the agent's own mode applies**, which is
    // the branch a scheduled run takes (#275) — nothing is there to answer a
    // prompt, so a `bypass` agent must actually bypass rather than block
    // forever.
    let mode = if spec.permission_mode.is_empty() {
        match permission_handler {
            Some(_) => Some("default"),
            None => spec.agent.as_ref().map(|a| a.permission_mode.as_str()),
        }
    } else {
        Some(spec.permission_mode.as_str())
    };
    opts = match mode {
        Some("default") => opts.with_default_permissions(),
        Some("plan") => opts.with_permission_mode(permission_mode::PLAN),
        Some("dontAsk") => opts.with_permission_mode(permission_mode::DONT_ASK),
        // "bypass", empty, unknown, or no agent at all. Go sets the mode
        // *and* the bypass flag, so both are set here.
        _ => opts
            .with_permission_mode(permission_mode::BYPASS_PERMISSIONS)
            .with_bypass_permissions(),
    };

    opts = opts.with_claude_executable(claude_executable());

    // `resolveAgentConfig` branches on whether the **chat names an agent**, not
    // on whether that agent has a model: it returns the agent's config outright,
    // and `runner.go` then sets a model only when `agentCfg.Model != ""`. So an
    // agent with an empty model runs with **no model option at all** — neither
    // the session's nor the user's default is consulted for it, and treating the
    // fallback as a general default would run such an agent on a different model
    // from the one Go picks.
    let model = match spec.agent.as_ref() {
        Some(agent) => agent.model.clone(),
        None => (spec.no_agent_model)(),
    };
    if !model.is_empty() {
        opts = opts.with_model(model);
    }

    if let Some(agent) = &spec.agent {
        if !agent.system_prompt.is_empty() {
            // Go interpolates `{{current_date}}` / `{{current_time}}` here.
            opts = opts.with_system_prompt(interpolate(&agent.system_prompt));
        }
    }

    // `appendModelAndPromptOpts`: resume wins, then a custom id, then neither —
    // and "neither" is a real branch rather than an impossible one, because a
    // scheduled run supplies no custom id.
    match &spec.resume_session_id {
        Some(id) if !id.is_empty() => opts = opts.with_session_id_to_resume(id.clone()),
        _ if !spec.custom_session_id.is_empty() => {
            opts = opts.with_session_id(spec.custom_session_id.clone())
        }
        _ => {}
    }

    opts = opts.with_thinking(thinking_mode(spec.agent.as_ref()));

    // `resolveToolsAndMCP`'s order is the `--allowedTools` argument's order, so
    // it is part of the command line: built-ins first, then the local server's
    // qualified names.
    let mut allowed = allowed_tools(caps);
    let local_tools;
    (opts, local_tools) = start_local_tools(opts, caps, &mut allowed).await?;
    let mut tool_servers: Vec<InProcessMcpServer> = local_tools.into_iter().collect();
    opts =
        start_integration_servers(opts, mcp_plan.as_ref(), &mut allowed, &mut tool_servers).await?;

    if !allowed.is_empty() {
        opts = opts.with_allowed_tools(allowed.iter().cloned());
    }
    // `appendDisallowedTools` keys on the agent's **explicit built-in list**,
    // not on the allowlist it produced. The difference only shows once local
    // tools exist: an agent naming `local: [current_time]` and no built-ins has
    // a non-empty allowlist and still gets no `--disallowedTools` from Go, where
    // subtracting the allowlist from the built-ins would deny all twelve.
    if let Some(selected) = caps
        .and_then(|c| c.built_in.as_deref())
        .filter(|list| !list.is_empty())
    {
        let disallowed: Vec<String> = ALL_BUILT_IN_TOOLS
            .iter()
            .filter(|t| !selected.iter().any(|s| s == *t))
            .map(|t| (*t).to_string())
            .collect();
        if !disallowed.is_empty() {
            opts = opts.with_disallowed_tools(disallowed);
        }
    }

    // `if opts.PermissionHandler != nil` — the last thing Go appends, and the
    // one option a scheduled run has none of.
    if let Some(handler) = permission_handler {
        opts = opts.with_permission_handler(wrap_permission_handler(handler, allowed));
    }
    Ok((opts, tool_servers))
}

/// Where one `capabilities.mcp` name resolved to.
///
/// `resolveServerConfig` returns a bare `any` and the caller cannot tell the two
/// apart, because in Go both are already a finished SDK config. Here they are
/// genuinely different things — one is a document to pass along, the other is a
/// listener this process has to bind — so the distinction is a type rather than
/// a convention.
enum McpSource {
    /// A `mcps.yaml` entry: the `--mcp-config` document the CLI dials or
    /// spawns. Nothing in this process hosts it, so there is no handle and
    /// nothing to shut down.
    External(serde_json::Value),
    /// An integration row this build can host. The listener is bound in
    /// [`start_integration_servers`], not here, because a refusal must leave no
    /// listener behind.
    Integration,
}

/// **Hand-written, and it prints `External(..)` without the document.**
///
/// The obvious `#[derive(Debug)]` would be a credential leak with a long fuse.
/// By the time a config reaches [`McpSource::External`] its `${ENV:…}`
/// placeholders have been *resolved* — `mcps_yaml::interpolate` runs at load —
/// so the value holds live `Authorization` headers and API tokens that the file
/// itself never contained. `registry::HostingRow` derives neither `Serialize`
/// **nor `Debug`** for exactly this reason: a `{plan:?}` in a log line is the
/// same leak as a response field, only later. Every test here asserts on the
/// *variant*, never on the formatting.
impl std::fmt::Debug for McpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::External(_) => f.write_str("External(..)"),
            Self::Integration => f.write_str("Integration"),
        }
    }
}

/// One entry of `capabilities.mcp`, resolved far enough to say "this build can
/// run this turn".
#[derive(Debug)]
struct McpServerSpec {
    /// The server name — which is both the `mcpServers` key and the prefix the
    /// CLI puts on every tool that server hosts. For an integration it is the
    /// bare integration id; for an external server it is whatever the user
    /// called it in `mcps.yaml`.
    id: String,
    tools: Vec<String>,
    source: McpSource,
}

/// What this turn will run with, and the database the integration rows came
/// from.
///
/// One `Vec` in the capabilities' own order rather than a collection per source:
/// `--allowedTools` is built by walking it, and its order is part of the command
/// line, so splitting the walk in two would group every integration's tools
/// ahead of every external one for no reason. `capabilities.mcp` is a
/// [`crate::native::gojson::GoMap`], so that order is already stable — Go ranges
/// a map here and its own argument differs run to run.
///
/// **This carries live credentials**, and the `Debug` derive is only safe
/// because [`McpSource`]'s is hand-written to withhold them. An external
/// server's config arrives with its `${ENV:…}` placeholders already resolved, so
/// it holds the very `Authorization` headers and tokens the file only pointed
/// at. Read `McpSource`'s `Debug` impl before adding a field here, and do not
/// add `Serialize`.
#[derive(Debug)]
struct McpPlan {
    db_path: Option<std::path::PathBuf>,
    servers: Vec<McpServerSpec>,
}

/// `resolveExternalMCP`'s inputs, checked before any of them is acted on.
///
/// `Ok(None)` is an agent that names no MCP server — the overwhelmingly common
/// case, and one that must open **neither** the database nor `mcps.yaml`. `Err`
/// refuses the whole turn: a name that resolves to nothing, or a registry file
/// that exists and cannot be read.
///
/// `db_path` and `mcps_path` are parameters rather than reads so the decision
/// can be driven over a scratch database and a fixture file instead of the
/// machine's own. Note the second is a **path**, not a loaded registry: loading
/// it in the caller would make a broken file refuse turns that never name an
/// external server, which is the exact shape of the bug #375 removed.
///
/// The database is opened lazily too — only for a name the registry did not
/// answer — so an agent whose every MCP server comes from `mcps.yaml` runs with
/// no `integrations` read at all, exactly as `resolveServerConfig` short-circuits.
fn mcp_plan(
    caps: Option<&Capabilities>,
    db_path: Option<&std::path::Path>,
    mcps_path: Option<&std::path::Path>,
) -> Result<Option<McpPlan>, String> {
    let Some(mcp) = caps.and_then(|c| c.mcp.as_ref()).filter(|m| !m.is_empty()) else {
        return Ok(None);
    };

    let registry = super::mcps_yaml::load(mcps_path)?;

    let mut servers = Vec::with_capacity(mcp.len());
    let mut needs_db = false;
    for (id, capability) in mcp.iter() {
        // `resolveServerConfig` asks the registry first and returns on a hit,
        // so a name in both is the yaml entry and the integrations are never
        // consulted for it.
        let source = match registry.get(id) {
            Some(config) => McpSource::External(config.clone()),
            None => {
                let db_path =
                    db_path.ok_or("no home directory to resolve the data dir".to_string())?;
                if !crate::native::integrations::registry::can_host(db_path, id)? {
                    return Err(format!(
                        "agent uses MCP server {id:?}, which is named by no mcps.yaml entry \
                         and is not an integration this build can host (#375)"
                    ));
                }
                needs_db = true;
                McpSource::Integration
            }
        };
        servers.push(McpServerSpec {
            id: id.clone(),
            tools: capability
                .tools
                .iter()
                .flat_map(|list| list.iter())
                .cloned()
                .collect(),
            source,
        });
    }
    Ok(Some(McpPlan {
        // `Some` exactly when an integration has to be started from it. A plan
        // made entirely of external servers carries no path, which is what makes
        // "this turn read no database" assertable rather than assumed.
        db_path: needs_db
            .then(|| db_path.map(std::path::Path::to_path_buf))
            .flatten(),
        servers,
    }))
}

/// `resolveExternalMCP`: register every resolved server and qualify the tool
/// names the agent asked for, once [`mcp_plan`] has said each one resolves.
///
/// Registered under the **bare integration id**, because that is the
/// `mcpServers` key Go uses (`StartInProcessMCPServer(ctx, cfg.ID, …)`) and so
/// the prefix on every qualified tool name already in an agent's allowlist. The
/// integration's *MCP implementation* name is `github-<id>`, a different string
/// that never appears on a tool.
///
/// An **external** server is registered under whatever the user named it in
/// `mcps.yaml`, for the same reason: that key is the prefix the CLI puts on its
/// tools, so it is already the string in the agent's stored allowlist.
///
/// Two departures from Go, both deliberate, and #375 extends the first to the
/// external half:
///
/// - **A start failure refuses the turn rather than being skipped.** The
///   reference `resolveServerConfig` discards the error and returns `nil`, so
///   the turn runs without that server's tools — silently, which is the
///   behaviour worth diverging from: an agent that quietly loses a tool set
///   gives a worse answer than one that says it cannot run. `start_local_tools`
///   already follows this rule, and an `mcps.yaml` entry that cannot be
///   converted refuses in [`super::mcps_yaml`] rather than being dropped from
///   the registry.
/// - **The map is a `BTreeMap`, so the order is stable.** Go ranges a map, so
///   its own `--allowedTools` order for two MCP servers differs between runs.
///   Strictly better, and it matches one of the orders Go produces.
async fn start_integration_servers(
    mut opts: Options,
    plan: Option<&McpPlan>,
    allowed: &mut Vec<String>,
    servers: &mut Vec<InProcessMcpServer>,
) -> Result<Options, String> {
    let Some(plan) = plan else {
        return Ok(opts);
    };

    for spec in &plan.servers {
        opts = match &spec.source {
            // Somebody else's server: the document goes on the command line and
            // nothing is bound here, so there is no handle to push.
            McpSource::External(config) => opts.with_mcp_server(&spec.id, config),
            McpSource::Integration => {
                // `Some` by construction — `mcp_plan` records the path exactly
                // when it resolved a name against the database — but a plan is
                // a value and this is the one place that would misbehave
                // silently if that ever stopped being true.
                let db_path = plan.db_path.as_deref().ok_or_else(|| {
                    format!("no database to start the MCP server for {:?}", spec.id)
                })?;
                let server = crate::native::integrations::registry::start_filtered_server(
                    db_path,
                    &spec.id,
                    &spec.tools,
                )
                .await?;
                let registered = opts.with_mcp_server(&spec.id, server.config());
                servers.push(server);
                registered
            }
        }
        .map_err(|e| format!("registering the MCP server for {:?}: {e}", spec.id))?
        // `appendToolOpts` adds `--strict-mcp-config` whenever it registers
        // any server at all, so the CLI does not also load the user's own
        // `.mcp.json`. Setting a bool, so saying it once per server is the
        // same as saying it once.
        .with_strict_mcp_config();
        allowed.extend(crate::native::integrations::registry::allowed_tool_names(
            &spec.id,
            &spec.tools,
        ));
    }
    Ok(opts)
}

/// `resolveLocalTools`: register the in-process server and qualify the names the
/// agent asked for.
///
/// Two rules ported exactly, both of which look like oversights until they are
/// not:
///
/// - **The names are not checked against what the server hosts.** An agent
///   naming a local tool that no longer exists gets `mcp__local-tools__gone` in
///   its allowlist and a model that cannot call it, rather than a run that
///   refuses to start. That is Go's behaviour and it is the kinder failure.
/// - **`--strict-mcp-config` travels with the server**, because Go adds it
///   whenever it registers any MCP server at all. Without it the CLI would also
///   load the user's own `.mcp.json`, so an agent's allowlist would stop being
///   the whole story about what it can reach.
///
/// The server is started **only** for an agent that named a local tool, which
/// is `len(caps.Local) > 0` on the Go side. Go has one server per process and
/// simply does not reference it otherwise; here not starting it is the same
/// thing, minus a bound port.
async fn start_local_tools(
    opts: Options,
    caps: Option<&Capabilities>,
    allowed: &mut Vec<String>,
) -> Result<(Options, Option<InProcessMcpServer>), String> {
    let Some(local) = caps.and_then(|c| c.local.as_deref()) else {
        return Ok((opts, None));
    };
    if local.is_empty() {
        return Ok((opts, None));
    }

    let server = crate::native::tools::start_local_mcp_server()
        .await
        .map_err(|e| format!("starting local MCP server: {e}"))?;
    let opts = opts
        .with_mcp_server(crate::native::tools::LOCAL_MCP_SERVER_NAME, server.config())
        .map_err(|e| format!("registering the local MCP server: {e}"))?
        .with_strict_mcp_config();
    allowed.extend(crate::native::tools::allowed_tool_names(local.iter()));
    Ok((opts, Some(server)))
}

fn capability_count(list: Option<&crate::native::gojson::GoList<String>>) -> usize {
    list.map(|l| l.len()).unwrap_or(0)
}

/// `resolveBuiltInTools`: an explicit list wins; otherwise *all* built-ins, but
/// only when the agent names no other tools at all.
fn allowed_tools(caps: Option<&Capabilities>) -> Vec<String> {
    let Some(caps) = caps else {
        // No agent means no tools list, which the SDK reads as "no restriction".
        return Vec::new();
    };
    if capability_count(caps.built_in.as_ref()) > 0 {
        return caps.built_in.as_deref().cloned().unwrap_or_default();
    }
    if capability_count(caps.local.as_ref()) == 0 && caps.mcp.as_ref().is_none_or(|m| m.is_empty())
    {
        return ALL_BUILT_IN_TOOLS
            .iter()
            .map(|t| (*t).to_string())
            .collect();
    }
    Vec::new()
}

/// `resolveThinkingMode`. An absent or unrecognized value is `adaptive`.
fn thinking_mode(agent: Option<&Agent>) -> &'static str {
    match agent.map(|a| a.thinking.as_str()) {
        Some("disabled") => "disabled",
        Some("enabled") => "enabled",
        _ => "adaptive",
    }
}

/// `wrapPermissionHandler`: enforce the allowlist before delegating.
///
/// Two behaviours worth keeping exactly: an empty allowlist returns the inner
/// handler **unwrapped** (a no-agent chat reaches every tool), and a tool
/// outside the list is denied *without* the user ever seeing a prompt.
/// `AskUserQuestion` is always allowed — it is the interactive Q&A mechanism,
/// not a capability.
fn wrap_permission_handler(inner: PermissionHandler, allowed: Vec<String>) -> PermissionHandler {
    if allowed.is_empty() {
        return inner;
    }
    let set: std::collections::HashSet<String> = allowed.into_iter().collect();
    std::sync::Arc::new(move |tool_name: String, input, ctx| {
        if tool_name == "AskUserQuestion" || set.contains(&tool_name) {
            return inner(tool_name, input, ctx);
        }
        let message = format!("tool {tool_name:?} is not in this agent's allowed capabilities");
        Box::pin(async move { crate::claude::permissions::PermissionResult::deny(message) })
    })
}

/// `ResolveAgentClaudeDir`: the agent's own override when it is absolute,
/// otherwise the run default.
fn resolve_agent_config_dir(agent: Option<&Agent>, settings: &TurnSettings) -> String {
    if let Some(agent) = agent {
        if !agent.claude_config_dir.is_empty() {
            let normalized = crate::native::settings::normalize(&agent.claude_config_dir);
            if let Some(dir) = crate::native::settings::absolute_dir(&normalized) {
                // An absolute per-agent override answers outright, and the
                // settings row is never touched — `ResolveAgentClaudeDir`
                // returns before `ClaudeRunConfigDir` for the same reason.
                return dir;
            }
        }
    }
    // A relative override is discarded rather than honoured: the subprocess
    // resolves it against its own cwd — inside the user's repo — and would read
    // it as trusted config.
    settings.run_config_dir()
}

/// `config.LoadProfileFilePathIn` plus the caller's `os.Stat`: the settings file
/// a run names with `--settings`, or `None` when there is none to name.
///
/// The two halves are separate rules and both matter (#242):
///
/// - **Which path.** An empty profile id is the unnamed fallback,
///   `<config dir>/settings.json`, which follows the dir the run targets. A
///   named profile keeps the **absolute path recorded in the index** — a
///   profile is a file the user created and pointed at, so it is not rebuilt
///   from the id. An id that matches nothing falls back to the *default*
///   profile's path, and only then to `<config dir>/settings.json`.
/// - **Whether to name it.** `--settings` on a missing path is an error rather
///   than a no-op, so a path that is not a file is `None`.
///
/// Until this landed the runner returned `None` for every non-empty profile id,
/// so a chat or task pinned to a named profile ran with **no `--settings` at
/// all** while the Go server passed the recorded path — the same class of silent
/// wrong-account failure #242 existed for.
fn settings_file_in(config_dir: &str, profile_id: &str, settings: &TurnSettings) -> Option<String> {
    // Go reads the index with `LoadProfilesMetadata()`, which resolves
    // `ClaudeSettingsProfilesPath()` — the **run default** dir, not `config_dir`.
    // That asymmetry is deliberate upstream and load-bearing here: profiles are
    // a global CRUD surface, so an agent with its own `claude_config_dir` still
    // resolves its named profile against the global index while its *fallback*
    // follows its own dir. Reading the index out of the override would silently
    // find nothing and hand the run the wrong account's settings.
    //
    // Resolved only for a *named* profile: the unnamed fallback reads no index.
    // It costs nothing now — this is the same `TurnSettings` the config dir came
    // from, so it is the row already in hand rather than a third connection.
    let index_dir = if profile_id.is_empty() {
        String::new()
    } else {
        settings.run_config_dir()
    };
    settings_file_from(config_dir, &index_dir, profile_id)
}

/// [`settings_file_in`] with the index dir passed in, so the resolution can be
/// driven over a scratch directory instead of the machine's real one.
fn settings_file_from(config_dir: &str, index_dir: &str, profile_id: &str) -> Option<String> {
    let path = profile_file_path_in(config_dir, index_dir, profile_id);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Some(path),
        _ => None,
    }
}

/// `config.LoadProfileFilePathIn`, path resolution only.
fn profile_file_path_in(config_dir: &str, index_dir: &str, profile_id: &str) -> String {
    let fallback = std::path::Path::new(config_dir)
        .join("settings.json")
        .to_string_lossy()
        .into_owned();
    if profile_id.is_empty() {
        return fallback;
    }
    // `if err != nil { return fallback }` — an unreadable or malformed index is
    // the fallback, not a failure.
    let Ok(profiles) = crate::native::claude_settings::profiles::load(index_dir) else {
        return fallback;
    };
    if let Some(profile) = profiles.iter().find(|p| p.id == profile_id) {
        return profile.file_path.clone();
    }
    if let Some(profile) = profiles.iter().find(|p| p.is_default) {
        return profile.file_path.clone();
    }
    fallback
}

/// Which `claude` binary to spawn.
///
/// The SDK defaults to the bare name resolved on `PATH`, which is exactly what
/// the desktop app cannot rely on: a GUI process inherits a minimal `PATH`, and
/// `find_claude_cli` exists precisely because of that — it is what the startup
/// banner already uses to tell the user whether the CLI is installed. Spawning
/// with the bare name while the banner says "found" would be the two disagreeing.
///
/// `AGENTO_CLAUDE_EXECUTABLE` overrides both. It is a real need — a second
/// install, or a wrapper script — and it is also how the turn tests point the
/// runner at a scripted fake CLI instead of a real one.
fn claude_executable() -> String {
    if let Ok(explicit) = std::env::var("AGENTO_CLAUDE_EXECUTABLE") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    crate::find_claude_cli().unwrap_or_else(|| "claude".to_string())
}

/// The two template variables Agento substitutes into a system prompt.
///
/// [`crate::native::template::interpolate_lenient`], shared with the scheduler
/// (#275) so there is one substitution loop. **Lenient is this caller's
/// behaviour, not an accident of it:** Go's `resolveSystemPrompt` fails a turn
/// on an unknown `{{name}}`, while this path has always left it in place — and
/// an agent whose prompt contains a literal `{{…}}` for some other reason would
/// otherwise stop having its date substituted at all. The scheduler's own caller
/// uses the strict form, because there a failure is a recorded job-history row
/// rather than a broken chat.
fn interpolate(prompt: &str) -> String {
    crate::native::template::interpolate_lenient(prompt)
}

/// The chat session row the turn runs against.
pub struct ChatRow {
    pub id: String,
    pub title: String,
    pub agent_slug: String,
    pub sdk_session_id: String,
    pub working_dir: String,
    pub model: String,
    pub settings_profile_id: String,
    /// The conversation's own permission mode, empty when none was chosen.
    pub permission_mode: String,
}

/// Load the chat and its agent. `Ok(None)` is Go's 404.
pub fn load(
    db_path: &std::path::Path,
    id: &str,
) -> Result<Option<(ChatRow, Option<Agent>)>, String> {
    let conn = crate::native::db::open_read_only(db_path)?;
    let row = conn
        .query_row(
            "SELECT id, title, agent_slug, sdk_session_id, working_directory, model,
                    settings_profile_id, permission_mode
             FROM chat_sessions WHERE id = ?1",
            [id],
            |row| {
                Ok(ChatRow {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    agent_slug: row.get(2)?,
                    sdk_session_id: row.get(3)?,
                    working_dir: row.get(4)?,
                    model: row.get(5)?,
                    settings_profile_id: row.get(6)?,
                    permission_mode: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("loading chat {id:?}: {e}"))?;

    let Some(row) = row else {
        return Ok(None);
    };

    let agent = if row.agent_slug.is_empty() {
        None
    } else {
        match crate::native::agents::get(db_path, &row.agent_slug)? {
            Some(agent) => Some(agent),
            // Go returns NotFoundError{"agent"} here, a 404 with its own
            // wording. Decline rather than invent a second 404 shape.
            None => return Err(format!("agent {:?} not found", row.agent_slug)),
        }
    };
    Ok(Some((row, agent)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn agent_with(caps: Capabilities) -> Agent {
        Agent {
            name: "A".into(),
            slug: "a".into(),
            description: String::new(),
            model: "claude-sonnet-4-6".into(),
            thinking: "adaptive".into(),
            permission_mode: String::new(),
            system_prompt: String::new(),
            capabilities: caps,
            claude_config_dir: String::new(),
        }
    }

    /// A `RunSpec` whose no-agent model records whether it was ever asked for.
    ///
    /// The count is the point: #299 is about *not* resolving it, and a test that
    /// only checked the resulting model would pass with the work still being
    /// done on every turn.
    /// Note the models below are all distinct from [`DEFAULT_MODEL`], which is
    /// what `Options::default()` already carries — `agent_with`'s happens to be
    /// exactly that string, so a test reusing it could not tell "we set the
    /// model" from "we never called `with_model`".
    fn spec_with(agent: Option<Agent>) -> (RunSpec, std::sync::Arc<AtomicUsize>) {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&calls);
        (
            RunSpec {
                agent,
                no_agent_model: Box::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    "no-agent-model".to_string()
                }),
                settings: std::sync::Arc::new(TurnSettings::none()),
                working_dir: String::new(),
                settings_profile_id: String::new(),
                permission_mode: String::new(),
                resume_session_id: None,
                custom_session_id: "c1".into(),
            },
            calls,
        )
    }

    /// A `RunSpec` for an agent with `caps`, when the test is about the tools
    /// rather than about the model.
    fn spec_for(caps: Capabilities) -> RunSpec {
        RunSpec {
            agent: Some(agent_with(caps)),
            no_agent_model: Box::new(String::new),
            settings: std::sync::Arc::new(TurnSettings::none()),
            working_dir: String::new(),
            settings_profile_id: String::new(),
            permission_mode: String::new(),
            resume_session_id: None,
            custom_session_id: "c1".into(),
        }
    }

    /// The chat's shape: `Some(handler)`, which is what forces default
    /// permissions. `None` is the scheduled-run shape and has its own test.
    fn no_op_handler() -> Option<PermissionHandler> {
        Some(std::sync::Arc::new(|_, _, _| {
            Box::pin(async { crate::claude::permissions::PermissionResult::allow() })
        }))
    }

    /// The whole of #299: a chat that names an agent never asks for the
    /// no-agent model, because Go's `resolveAgentConfig` returns the agent's config
    /// outright and never reads the user's default. Resolving it eagerly opened
    /// a second read-only SQLite connection and loaded the settings row on every
    /// turn, to discard the answer.
    #[tokio::test]
    async fn an_agent_chat_never_resolves_the_no_agent_model() {
        let mut agent = agent_with(Capabilities::default());
        agent.model = "agent-model".into();
        let (spec, calls) = spec_with(Some(agent));
        let (opts, servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert!(servers.is_empty(), "no agent named a local tool");

        assert_eq!(opts.model, "agent-model");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the no-agent model was resolved for a chat that cannot use it"
        );
    }

    /// The other half of the same rule, and a real parity fix rather than a
    /// cleanup: Go branches on whether the chat **names an agent**, not on
    /// whether that agent has a model. `runner.go` sets a model only when
    /// `agentCfg.Model != ""`, so an agent with an empty one runs with **no
    /// model option at all** — the session's model and the user's default are
    /// never consulted for it.
    #[tokio::test]
    async fn an_agent_with_no_model_runs_with_none_rather_than_the_no_agent_model() {
        let mut agent = agent_with(Capabilities::default());
        agent.model = String::new();
        let (spec, calls) = spec_with(Some(agent));
        let (opts, servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert!(servers.is_empty(), "no agent named a local tool");

        // `with_model` is never called, so the SDK's own default stands —
        // which is what Go's `defaultOptions()` leaves in place too.
        assert_eq!(
            opts.model,
            crate::claude::options::DEFAULT_MODEL,
            "an agent's empty model is not a request for the session's or the user's"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// The whole of migration 30, from the side that decides what the CLI does.
    ///
    /// A chat always has an interactive permission handler, so before this the
    /// `Some(_)` arm forced `default` unconditionally and a conversation had no
    /// way to opt out. All four modes have to reach the CLI *through* that
    /// handler, so the assertion is on the resolved options rather than on
    /// which branch ran.
    #[tokio::test]
    async fn a_chats_own_permission_mode_beats_the_interactive_default() {
        // The bypass flag is not a function of the mode, and reproducing that
        // is the point: Go's `appendPermissionOpts` calls `WithPermissionMode`
        // alone for "plan" and "dontAsk", so the SDK's own default (bypass on)
        // survives for both — only "default" clears it and only "bypass" sets
        // it deliberately. A port that tidied this into "flag == is bypass"
        // would change what two of the four modes send.
        for (chosen, want_mode, want_bypass) in [
            ("bypass", permission_mode::BYPASS_PERMISSIONS, true),
            ("default", permission_mode::DEFAULT, false),
            ("plan", permission_mode::PLAN, true),
            ("dontAsk", permission_mode::DONT_ASK, true),
        ] {
            let (mut spec, _) = spec_with(Some(agent_with(Capabilities::default())));
            spec.permission_mode = chosen.to_string();
            let (opts, _servers) = build_options(&spec, no_op_handler())
                .await
                .expect("options");
            assert_eq!(
                opts.permission_mode, want_mode,
                "a chat asking for {chosen:?} did not get it"
            );
            assert_eq!(
                opts.allow_dangerously_skip_permissions, want_bypass,
                "the bypass flag for {chosen:?} is not what Go's appendPermissionOpts leaves"
            );
        }
    }

    /// The other half, and the one that makes the migration safe: an empty mode
    /// is *not* a fifth mode. Every row written before migration 30 has one, so
    /// the pre-existing rules — handler forces `default`, no handler falls to
    /// the agent's own — have to be untouched.
    #[tokio::test]
    async fn an_unset_chat_mode_leaves_the_previous_rules_alone() {
        let mut agent = agent_with(Capabilities::default());
        agent.permission_mode = "plan".into();

        let (spec, _) = spec_with(Some(agent.clone()));
        let (opts, _servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert_eq!(
            opts.permission_mode,
            permission_mode::DEFAULT,
            "an interactive handler still overrides the agent when the chat is silent"
        );

        let (spec, _) = spec_with(Some(agent));
        let (opts, _servers) = build_options(&spec, None).await.expect("options");
        assert_eq!(
            opts.permission_mode,
            permission_mode::PLAN,
            "without a handler the agent's own mode still applies"
        );
    }

    /// A settings row on disk, migrated and empty.
    fn settings_db() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        drop(conn);
        file
    }

    /// #340's acceptance, and the only form it can take: a number.
    ///
    /// Three consumers with different fields — the config dir, the settings
    /// profile's index dir, and the no-agent model — used to open a read-only
    /// connection each, on the latency path of every message.
    #[tokio::test]
    async fn a_turn_reads_the_settings_row_at_most_once() {
        let file = settings_db();
        let settings = std::sync::Arc::new(TurnSettings::from_db(file.path()));

        let mut spec = spec_for(Capabilities::default());
        spec.settings = std::sync::Arc::clone(&settings);
        // A *named* profile is what reaches the index dir; the unnamed fallback
        // reads no index at all.
        spec.settings_profile_id = "work".into();

        let (_opts, servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert!(servers.is_empty(), "no agent named a local tool");

        // And the fourth consumer, which an agent chat never reaches — asked
        // here so the count covers every field a turn can want.
        let _ = settings.default_model();

        assert_eq!(
            settings.loads(),
            1,
            "the settings row was read more than once for one turn"
        );
    }

    /// The zero case, which is the other half of the same rule: an absolute
    /// per-agent override answers outright, so nothing opens the database at
    /// all. `ResolveAgentClaudeDir` returns before `ClaudeRunConfigDir` for
    /// exactly this reason.
    #[tokio::test]
    async fn an_absolute_per_agent_override_never_reads_the_settings_row() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut agent = agent_with(Capabilities::default());
        agent.claude_config_dir = dir.path().to_string_lossy().into_owned();

        // A path that cannot be opened: a read would still answer, so only the
        // counter can tell "did not need it" from "needed it and got nothing".
        let settings = std::sync::Arc::new(TurnSettings::from_db(
            "/nonexistent/agento/definitely-not-a-db",
        ));
        let mut spec = spec_for(Capabilities::default());
        spec.agent = Some(agent);
        spec.settings = std::sync::Arc::clone(&settings);

        let (_opts, _servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");

        assert_eq!(settings.loads(), 0);
    }

    /// The precedence itself, unchanged: an absolute per-agent override wins,
    /// and anything else defers to `ClaudeRunConfigDir`.
    ///
    /// The second and third assertions are identities rather than literals, so
    /// they hold whether or not the developer running them exports
    /// `CLAUDE_CONFIG_DIR` — and they still fail if the fall-through arm stops
    /// going through the shared row.
    #[test]
    fn the_config_dir_precedence_is_override_then_the_run_dir() {
        let dir = tempfile::tempdir().expect("temp dir");
        let absolute = dir.path().to_string_lossy().into_owned();
        let file = settings_db();
        let settings = TurnSettings::from_db(file.path());

        let mut agent = agent_with(Capabilities::default());
        agent.claude_config_dir = absolute.clone();
        assert_eq!(
            resolve_agent_config_dir(Some(&agent), &settings),
            absolute,
            "an absolute per-agent override is the run's dir"
        );

        // A relative override is discarded rather than honoured: the subprocess
        // would resolve it against its own cwd, inside the user's repo.
        agent.claude_config_dir = "relative/dir".into();
        assert_eq!(
            resolve_agent_config_dir(Some(&agent), &settings),
            settings.run_config_dir()
        );

        assert_eq!(
            resolve_agent_config_dir(None, &settings),
            settings.run_config_dir()
        );
    }

    /// The one branch that does need it — and it is asked exactly once.
    #[tokio::test]
    async fn a_chat_with_no_agent_resolves_its_model_once() {
        let (spec, calls) = spec_with(None);
        let (opts, servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert!(servers.is_empty(), "no agent named a local tool");

        assert_eq!(opts.model, "no-agent-model");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_explicit_built_in_list_wins() {
        let caps = Capabilities {
            built_in: Some(vec!["Read".into(), "Bash".into()].into()),
            local: None,
            mcp: None,
        };
        assert_eq!(allowed_tools(Some(&caps)), vec!["Read", "Bash"]);
    }

    /// The rule that is easy to invert: *all* built-ins only when the agent
    /// names nothing else at all.
    #[test]
    fn no_capabilities_at_all_means_every_built_in() {
        let caps = Capabilities::default();
        assert_eq!(allowed_tools(Some(&caps)).len(), ALL_BUILT_IN_TOOLS.len());
        assert_eq!(allowed_tools(Some(&caps))[0], "Read");
    }

    #[test]
    fn naming_only_mcp_tools_yields_no_built_ins() {
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "github".to_string(),
            crate::native::agents::McpCapability {
                tools: Some(vec!["list_prs".into()].into()),
            }
            .into(),
        );
        let caps = Capabilities {
            built_in: None,
            local: None,
            mcp: Some(mcp.into()),
        };
        assert!(allowed_tools(Some(&caps)).is_empty());
    }

    #[test]
    fn no_agent_means_no_tool_restriction() {
        assert!(allowed_tools(None).is_empty());
    }

    // ─── Which agents this port will run (#310, #311) ─────────────────────────

    fn caps_naming(server: &str, tools: Option<Vec<String>>) -> Capabilities {
        let mut mcp = BTreeMap::new();
        mcp.insert(
            server.to_string(),
            crate::native::agents::McpCapability {
                tools: tools.map(Into::into),
            }
            .into(),
        );
        Capabilities {
            built_in: None,
            local: None,
            mcp: Some(mcp.into()),
        }
    }

    fn db_with_integration(id: &str, integration_type: &str) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        let mut conn = rusqlite::Connection::open(file.path()).expect("open");
        crate::native::migrate::apply(&mut conn).expect("migrate");
        conn.execute(
            "INSERT INTO integrations (id, name, type, enabled, credentials, auth, services,
                                       created_at, updated_at)
             VALUES (?1, ?1, ?2, 1, '{\"auth_mode\":\"pat\",\"personal_access_token\":\"t\"}',
                     '{\"ok\":true}',
                     '{\"repos\":{\"enabled\":true,\"tools\":[\"list_repos\",\"get_repo\"]}}',
                     '2026-01-01 00:00:00 +0000 UTC', '2026-01-01 00:00:00 +0000 UTC')",
            rusqlite::params![id, integration_type],
        )
        .expect("seed");
        file
    }

    /// An agent naming no MCP server must not open the database at all — the
    /// overwhelmingly common case, and the reason the plan is an `Option`.
    #[test]
    fn no_mcp_capability_is_no_plan_and_no_database_read() {
        assert!(mcp_plan(None, None, None).expect("no caps").is_none());
        assert!(mcp_plan(Some(&Capabilities::default()), None, None)
            .expect("no mcp")
            .is_none());
        // Present but empty is the same as absent, exactly as `local: []` is.
        let empty = Capabilities {
            built_in: None,
            local: None,
            mcp: Some(BTreeMap::new().into()),
        };
        assert!(mcp_plan(Some(&empty), None, None).expect("empty").is_none());
    }

    /// A `mcps.yaml` file holding these entries, at a path a test can pass.
    fn mcps_yaml(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
        let path = dir.join("mcps.yaml");
        std::fs::write(&path, contents).expect("write mcps.yaml");
        path
    }

    /// The refusal that survives #375: a name that resolves to **nothing**.
    ///
    /// The two that did not are pinned separately below — a name with no
    /// integration row is now answered by `mcps.yaml`, and the presence of that
    /// file no longer refuses anything on its own.
    #[test]
    fn an_mcp_name_that_resolves_to_nothing_is_refused() {
        let file = db_with_integration("gh-1", "github");

        // A type with no Rust starter, and the reason `whatsapp` is the
        // stand-in: it is not deferred but dropped, because its starter opens a
        // live whatsmeow connection registered in a package global rather than
        // merely a port. With no `mcps.yaml` entry naming it, nothing resolves
        // it.
        let whatsapp = db_with_integration("wa-1", "whatsapp");
        let err = mcp_plan(
            Some(&caps_naming("wa-1", None)),
            Some(whatsapp.path()),
            None,
        )
        .expect_err("whatsapp cannot be hosted");
        assert!(err.contains(r#"MCP server "wa-1""#), "{err}");

        // A name with no integration row and no yaml entry.
        assert!(mcp_plan(
            Some(&caps_naming("not-an-integration", None)),
            Some(file.path()),
            None
        )
        .is_err());

        // One unresolvable name among several refuses the whole turn: a partial
        // tool set is the failure the refusal exists to prevent.
        let mut mixed = caps_naming("gh-1", None);
        if let Some(mcp) = mixed.mcp.as_mut() {
            mcp.0.insert(
                "gg-1".to_string(),
                crate::native::agents::McpCapability { tools: None }.into(),
            );
        }
        assert!(mcp_plan(Some(&mixed), Some(file.path()), None).is_err());
    }

    /// **Part A of #375, and the whole of the regression it names.** An
    /// `mcps.yaml` on disk that has nothing to say about this agent's servers
    /// must not refuse it.
    ///
    /// Before #375 the mere existence of the file was an early `Err`, so one
    /// leftover from an `agento web` install disabled every MCP-backed agent on
    /// the machine — including the six integration types that are fully
    /// implemented here and would have worked. Reverting the deleted arm fails
    /// this test on its first assertion.
    #[test]
    fn a_registry_that_names_none_of_the_agents_servers_does_not_refuse_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = db_with_integration("gh-1", "github");
        let caps = caps_naming("gh-1", Some(vec!["get_repo".into()]));

        for contents in [
            "",
            "# left over from `agento web`\n",
            "something-else:\n  transport: stdio\n  command: /usr/bin/other\n",
        ] {
            let path = mcps_yaml(dir.path(), contents);
            let plan = mcp_plan(Some(&caps), Some(file.path()), Some(&path))
                .unwrap_or_else(|e| panic!("{contents:?} must not refuse the turn: {e}"))
                .expect("a plan");
            let [spec] = &plan.servers[..] else {
                panic!("one server");
            };
            assert!(
                matches!(spec.source, McpSource::Integration),
                "{contents:?} resolved somewhere unexpected: {:?}",
                spec.source
            );
        }
    }

    /// Part B: a name the registry answers is an **external** server, resolved
    /// without opening the database at all — `resolveServerConfig` returns on
    /// the registry hit and never reaches the integrations.
    #[test]
    fn a_yaml_name_resolves_externally_and_reads_no_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = mcps_yaml(
            dir.path(),
            "docs:\n  transport: stdio\n  command: /usr/bin/docs-mcp\n  args: [--root, /srv]\n",
        );
        let caps = caps_naming("docs", Some(vec!["search".into()]));

        // `db_path: None` is what makes "no database was read" an assertion
        // rather than an assumption: any fall-through to the integrations is
        // the `no home directory` error.
        let plan = mcp_plan(Some(&caps), None, Some(&path))
            .expect("the registry answers")
            .expect("a plan");
        assert!(plan.db_path.is_none(), "no integration, no database");
        let [spec] = &plan.servers[..] else {
            panic!("one server");
        };
        let McpSource::External(config) = &spec.source else {
            panic!("expected the yaml entry, got {:?}", spec.source);
        };
        assert_eq!(
            config,
            &serde_json::json!({
                "type": "stdio",
                "command": "/usr/bin/docs-mcp",
                "args": ["--root", "/srv"],
            })
        );
    }

    /// **The plan holds live credentials and its `Debug` must not print them.**
    ///
    /// `${ENV:…}` is resolved at load, so an external config carries the actual
    /// `Authorization` header — a value the file itself never contained, which
    /// is what makes the derive dangerous rather than merely untidy. Same rule
    /// as `registry::HostingRow`, which derives neither `Debug` nor `Serialize`;
    /// the difference here is that the plan *is* formatted, in every panic
    /// message in this module.
    ///
    /// Asserted on the token's absence, which is the form that survives the
    /// message being reworded.
    #[test]
    fn the_plans_debug_does_not_carry_an_interpolated_secret() {
        const SECRET: &str = "sk-live-NOTAREALTOKEN";
        let _env = crate::paths::tests::env_lock();
        let _token = crate::paths::tests::EnvVar::set("AGENTO_TEST_PLAN_TOKEN", SECRET);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = mcps_yaml(
            dir.path(),
            "weather:\n  transport: streamable_http\n  url: https://w.example/mcp\n  \
             headers:\n    Authorization: \"Bearer ${ENV:AGENTO_TEST_PLAN_TOKEN}\"\n",
        );
        let plan = mcp_plan(Some(&caps_naming("weather", None)), None, Some(&path))
            .expect("resolvable")
            .expect("a plan");

        // The turn really does run with the resolved token…
        let McpSource::External(config) = &plan.servers[0].source else {
            panic!("expected the yaml entry");
        };
        assert_eq!(
            config["headers"]["Authorization"],
            format!("Bearer {SECRET}")
        );
        // …and formatting the plan does not disclose it.
        assert!(
            !format!("{plan:?}").contains(SECRET),
            "the plan's Debug leaked the token: {plan:?}"
        );
    }

    /// `resolveServerConfig` asks the registry **first**, so a name in both is
    /// the yaml entry. Getting this backwards would run a turn against a
    /// different tool server than the config the user wrote asks for.
    #[test]
    fn a_name_in_both_resolves_to_the_yaml_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = db_with_integration("gh-1", "github");
        let path = mcps_yaml(
            dir.path(),
            "gh-1:\n  transport: sse\n  url: https://mine.example/sse\n",
        );

        let plan = mcp_plan(
            Some(&caps_naming("gh-1", Some(vec!["get_repo".into()]))),
            Some(file.path()),
            Some(&path),
        )
        .expect("resolvable")
        .expect("a plan");
        let [spec] = &plan.servers[..] else {
            panic!("one server");
        };
        let McpSource::External(config) = &spec.source else {
            panic!("the registry has to win: {:?}", spec.source);
        };
        assert_eq!(config["url"], "https://mine.example/sse");
    }

    /// A registry that exists and cannot be parsed refuses the turn, naming the
    /// file — the one thing that must **not** be silently treated as "no
    /// external servers", because that would drop a tool set without saying so.
    #[test]
    fn an_unreadable_registry_refuses_and_names_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = mcps_yaml(dir.path(), "docs:\n  transport: carrier-pigeon\n");
        let err = mcp_plan(
            Some(&caps_naming("docs", None)),
            None,
            Some(dir.path().join("mcps.yaml").as_path()),
        )
        .expect_err("a broken registry is not an empty one");
        assert!(err.contains(r#"MCP server "docs""#), "{err}");
        assert!(err.contains("unknown transport"), "{err}");

        // …and it refuses **only** turns that reach it. An agent naming no MCP
        // server is untouched by a typo in a file it never consults, which is
        // Part A's lesson applied to the parser.
        std::fs::write(&path, "\t not: [even, yaml\n").expect("write");
        assert!(mcp_plan(Some(&Capabilities::default()), None, Some(&path))
            .expect("no mcp capability, no registry read")
            .is_none());
    }

    /// The whole of #311's runner half: a github-only agent runs natively, with
    /// the server registered under the **bare integration id** — the
    /// `mcpServers` key Go uses, and the prefix on every qualified name already
    /// in the agent's stored allowlist. The MCP implementation name
    /// (`github-<id>`) must not appear anywhere on a tool.
    #[tokio::test]
    async fn a_github_agent_runs_natively_under_the_integration_id() {
        let file = db_with_integration("gh-1", "github");
        let caps = caps_naming("gh-1", Some(vec!["get_repo".into()]));

        let plan = mcp_plan(Some(&caps), Some(file.path()), None)
            .expect("hostable")
            .expect("a plan");
        let mut allowed = allowed_tools(Some(&caps));
        let mut servers = Vec::new();
        let opts =
            start_integration_servers(Options::new(), Some(&plan), &mut allowed, &mut servers)
                .await
                .expect("started");

        let [server] = &servers[..] else {
            panic!("one integration, one listener");
        };
        let registered = opts
            .mcp_servers
            .get("gh-1")
            .expect("registered under the integration id");
        assert_eq!(registered["type"], "http");
        assert_eq!(registered["url"], server.url());
        assert!(
            !opts.mcp_servers.contains_key("github-gh-1"),
            "the implementation name is not the map key"
        );
        // `appendToolOpts` adds it whenever any MCP server is registered.
        assert!(opts.strict_mcp_config);
        assert_eq!(allowed, vec!["mcp__gh-1__get_repo".to_string()]);
    }

    /// Go appends a qualified name for whatever the agent asked for, registered
    /// or not — the same rule the local tools follow.
    #[tokio::test]
    async fn an_unhosted_tool_name_is_still_qualified() {
        let file = db_with_integration("gh-1", "github");
        let caps = caps_naming("gh-1", Some(vec!["get_repo".into(), "gone".into()]));
        let plan = mcp_plan(Some(&caps), Some(file.path()), None)
            .expect("hostable")
            .expect("a plan");

        let mut allowed = Vec::new();
        let mut servers = Vec::new();
        start_integration_servers(Options::new(), Some(&plan), &mut allowed, &mut servers)
            .await
            .expect("started");
        assert_eq!(
            allowed,
            vec![
                "mcp__gh-1__get_repo".to_string(),
                "mcp__gh-1__gone".to_string()
            ]
        );
    }

    /// An external server reaches `--mcp-config` verbatim, binds nothing, and is
    /// walked in the **same** pass as an integration — so `--allowedTools` stays
    /// in the capabilities' order rather than grouping by source.
    ///
    /// The three transports are pinned in `mcps_yaml`'s own tests, at the bytes.
    /// What this adds is the half that is `runner`'s: registration under the
    /// user's own name, no listener handle, and `--strict-mcp-config` set by an
    /// external server alone (without it the CLI would also load the user's
    /// `.mcp.json`, so the agent's allowlist would stop being the whole story).
    #[tokio::test]
    async fn an_external_server_is_registered_without_binding_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = db_with_integration("gh-1", "github");
        let path = mcps_yaml(
            dir.path(),
            "zz-docs:\n  transport: streamable_http\n  url: https://docs.example/mcp\n",
        );
        // `zz-` sorts after `gh-1`, so the order below is the map's and not an
        // artefact of which source was walked first.
        let mut caps = caps_naming("gh-1", Some(vec!["get_repo".into()]));
        if let Some(mcp) = caps.mcp.as_mut() {
            mcp.0.insert(
                "zz-docs".to_string(),
                crate::native::agents::McpCapability {
                    tools: Some(vec!["search".to_string()].into()),
                }
                .into(),
            );
        }

        let plan = mcp_plan(Some(&caps), Some(file.path()), Some(&path))
            .expect("resolvable")
            .expect("a plan");
        let mut allowed = Vec::new();
        let mut servers = Vec::new();
        let opts =
            start_integration_servers(Options::new(), Some(&plan), &mut allowed, &mut servers)
                .await
                .expect("started");

        assert_eq!(
            servers.len(),
            1,
            "only the integration binds a listener; an external server is somebody else's"
        );
        assert_eq!(
            opts.mcp_servers.get("zz-docs"),
            Some(&serde_json::json!({"type": "http", "url": "https://docs.example/mcp"})),
            "the yaml document is handed over verbatim, under the user's own name"
        );
        assert!(opts.strict_mcp_config);
        assert_eq!(
            allowed,
            vec![
                "mcp__gh-1__get_repo".to_string(),
                "mcp__zz-docs__search".to_string()
            ]
        );
    }

    /// The whole of #310: an agent naming a local tool builds options rather
    /// than refusing, and what it builds is the server registered
    /// under `local-tools`, the qualified name in the allowlist, and
    /// `--strict-mcp-config` alongside so the user's own `.mcp.json` cannot add
    /// to what the agent may reach.
    #[tokio::test]
    async fn an_agent_naming_a_local_tool_runs_natively() {
        let spec = spec_for(Capabilities {
            built_in: None,
            local: Some(vec!["current_time".into()].into()),
            mcp: None,
        });
        let (opts, servers) = build_options(&spec, no_op_handler())
            .await
            .expect("a local tool is supplied natively now");

        let [server] = &servers[..] else {
            panic!("exactly one listener is handed back, because dropping it stops it");
        };
        let registered = opts
            .mcp_servers
            .get("local-tools")
            .expect("registered under the name the CLI prefixes with");
        assert_eq!(registered["type"], "http");
        assert_eq!(registered["url"], server.url());
        assert!(opts.strict_mcp_config);
        assert_eq!(
            opts.allowed_tools,
            vec!["mcp__local-tools__current_time".to_string()]
        );
    }

    /// `appendDisallowedTools` keys on the agent's explicit built-in list, and
    /// this agent has none — so Go sends no `--disallowedTools` even though its
    /// allowlist is non-empty. Subtracting the allowlist from the built-ins
    /// instead would deny all twelve and change what the agent can do.
    #[tokio::test]
    async fn naming_only_a_local_tool_disallows_nothing() {
        let spec = spec_for(Capabilities {
            built_in: None,
            local: Some(vec!["current_time".into()].into()),
            mcp: None,
        });
        let (opts, _servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert!(
            opts.disallowed_tools.is_empty(),
            "Go disallows nothing here: {:?}",
            opts.disallowed_tools
        );
    }

    /// Built-ins first, then the local server's qualified names — the order Go
    /// appends them in, which is the order they reach `--allowedTools`. The
    /// denylist is still the complement of the *built-in* list.
    #[tokio::test]
    async fn built_ins_and_local_tools_share_one_allowlist_in_gos_order() {
        let spec = spec_for(Capabilities {
            built_in: Some(vec!["Read".into(), "Bash".into()].into()),
            local: Some(vec!["current_time".into(), "gone".into()].into()),
            mcp: None,
        });
        let (opts, _servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");

        assert_eq!(
            opts.allowed_tools,
            vec![
                "Read".to_string(),
                "Bash".to_string(),
                "mcp__local-tools__current_time".to_string(),
                // A local tool the server does not host is still qualified —
                // Go never checks, and a run that refused to start would be the
                // worse failure.
                "mcp__local-tools__gone".to_string(),
            ]
        );
        assert!(!opts.disallowed_tools.contains(&"Read".to_string()));
        assert!(opts.disallowed_tools.contains(&"Write".to_string()));
        assert_eq!(opts.disallowed_tools.len(), ALL_BUILT_IN_TOOLS.len() - 2);
    }

    /// No local tools means no listener and no MCP server — a port bound for an
    /// agent that never dials it is a leak, not a no-op.
    #[tokio::test]
    async fn an_agent_naming_no_local_tools_binds_nothing() {
        let spec = spec_for(Capabilities::default());
        let (opts, servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert!(servers.is_empty());
        assert!(opts.mcp_servers.is_empty());
        assert!(!opts.strict_mcp_config);
    }

    /// `local: []` — **present but empty** — is the distinction `GoList` exists
    /// to preserve, and it decides whether the agent gets twelve built-ins or
    /// none. Go's `len(caps.Local) > 0` makes an empty list the same as an
    /// absent one on both counts: no listener, and still "names no tools at
    /// all", so all the built-ins.
    #[tokio::test]
    async fn a_present_but_empty_local_list_is_the_same_as_none() {
        let spec = spec_for(Capabilities {
            built_in: None,
            local: Some(vec![].into()),
            mcp: None,
        });
        let (opts, servers) = build_options(&spec, no_op_handler())
            .await
            .expect("options");
        assert!(servers.is_empty());
        assert!(opts.mcp_servers.is_empty());
        assert!(!opts.strict_mcp_config);
        assert_eq!(opts.allowed_tools.len(), ALL_BUILT_IN_TOOLS.len());
    }

    #[test]
    fn thinking_defaults_to_adaptive() {
        assert_eq!(thinking_mode(None), "adaptive");
        let mut agent = agent_with(Capabilities::default());
        agent.thinking = "disabled".into();
        assert_eq!(thinking_mode(Some(&agent)), "disabled");
        agent.thinking = "nonsense".into();
        assert_eq!(thinking_mode(Some(&agent)), "adaptive");
        agent.thinking = String::new();
        assert_eq!(thinking_mode(Some(&agent)), "adaptive");
    }

    #[test]
    fn a_prompt_with_no_template_is_untouched() {
        assert_eq!(interpolate("plain prompt"), "plain prompt");
        let out = interpolate("today is {{current_date}}");
        assert!(out.starts_with("today is 20"), "{out}");
        assert!(!out.contains("{{"));
    }

    /// Write a profiles index into `dir` and return its path prefix.
    fn seed_index(
        dir: &std::path::Path,
        profiles: &[crate::native::claude_settings::profiles::Profile],
    ) {
        let bytes =
            crate::native::claude_settings::profiles::encode_index(profiles).expect("encode");
        std::fs::write(dir.join("settings_profiles.json"), bytes).expect("write index");
    }

    fn profile(
        id: &str,
        file_path: &str,
        is_default: bool,
    ) -> crate::native::claude_settings::profiles::Profile {
        crate::native::claude_settings::profiles::Profile {
            id: id.to_string(),
            name: id.to_string(),
            file_path: file_path.to_string(),
            is_default,
        }
    }

    /// **A named profile resolves to the path the index records** — not to
    /// `<dir>/settings.json`, and not to nothing. Before this was implemented a
    /// run pinned to a named profile got no `--settings` at all while the Go
    /// server passed the recorded path, which is a silent wrong-account run.
    #[test]
    fn a_named_settings_profile_resolves_to_its_recorded_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("settings.json"), "{}").expect("write");
        // Deliberately *not* `settings_work.json`: the index is the authority,
        // so a resolver that rebuilt the name from the id would miss this.
        let recorded = dir.path().join("settings_renamed-since.json");
        std::fs::write(&recorded, "{}").expect("write");
        let recorded = recorded.to_string_lossy().into_owned();
        seed_index(dir.path(), &[profile("work", &recorded, false)]);
        let path = dir.path().to_string_lossy().into_owned();

        assert_eq!(
            settings_file_from(&path, &path, "work"),
            Some(recorded.clone())
        );
        // The unnamed fallback still follows the *run* dir rather than the index.
        assert_eq!(
            settings_file_from(&path, &path, ""),
            Some(
                dir.path()
                    .join("settings.json")
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    /// An id the index does not carry falls back to the **default** profile, and
    /// only then to `<dir>/settings.json`. Both branches are Go's.
    #[test]
    fn an_unknown_profile_id_falls_back_to_the_default_then_to_settings_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("settings.json"), "{}").expect("write");
        let fallback = dir
            .path()
            .join("settings.json")
            .to_string_lossy()
            .into_owned();
        let default_path = dir.path().join("settings_default.json");
        std::fs::write(&default_path, "{}").expect("write");
        let default_path = default_path.to_string_lossy().into_owned();
        let path = dir.path().to_string_lossy().into_owned();

        seed_index(dir.path(), &[profile("default", &default_path, true)]);
        assert_eq!(
            settings_file_from(&path, &path, "no-such-profile"),
            Some(default_path)
        );

        // No default either: the unnamed fallback.
        seed_index(dir.path(), &[profile("other", "/nowhere/x.json", false)]);
        assert_eq!(
            settings_file_from(&path, &path, "no-such-profile"),
            Some(fallback.clone())
        );

        // No index at all is the same fallback, not a failure.
        std::fs::remove_file(dir.path().join("settings_profiles.json")).expect("rm");
        assert_eq!(settings_file_from(&path, &path, "anything"), Some(fallback));
    }

    /// The index lives in the **run default** dir even when the run targets an
    /// agent's own `claude_config_dir`, because profiles are a global surface —
    /// while the unnamed fallback follows the run's dir. `LoadProfileFilePathIn`
    /// takes `dir` for the fallback and calls `LoadProfilesMetadata()`, which
    /// does not.
    #[test]
    fn the_index_is_read_from_the_global_dir_and_the_fallback_from_the_runs() {
        let global = tempfile::tempdir().expect("tempdir");
        let agent = tempfile::tempdir().expect("tempdir");
        let recorded = global.path().join("settings_work.json");
        std::fs::write(&recorded, "{}").expect("write");
        let recorded = recorded.to_string_lossy().into_owned();
        seed_index(global.path(), &[profile("work", &recorded, false)]);
        std::fs::write(agent.path().join("settings.json"), "{}").expect("write");

        let agent_dir = agent.path().to_string_lossy().into_owned();
        let global_dir = global.path().to_string_lossy().into_owned();
        assert_eq!(
            settings_file_from(&agent_dir, &global_dir, "work"),
            Some(recorded)
        );
        assert_eq!(
            settings_file_from(&agent_dir, &global_dir, ""),
            Some(
                agent
                    .path()
                    .join("settings.json")
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    /// `--settings` on a missing path is an error rather than a no-op, so a
    /// recorded path whose file has gone is named nothing at all.
    #[test]
    fn a_missing_settings_file_is_not_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(settings_file_from(
            &dir.path().to_string_lossy(),
            &dir.path().to_string_lossy(),
            ""
        )
        .is_none());

        let path = dir.path().to_string_lossy().into_owned();
        seed_index(
            dir.path(),
            &[profile(
                "gone",
                &format!("{path}/settings_gone.json"),
                false,
            )],
        );
        assert!(settings_file_from(&path, &path, "gone").is_none());
    }
}
