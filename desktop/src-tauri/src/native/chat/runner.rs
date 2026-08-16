//! An agent's stored config, turned into the SDK options one chat turn runs
//! under. Mirrors `buildSDKOptions` (`internal/agent/runner.go`).
//!
//! # What this deliberately cannot do yet, and why that is safe
//!
//! Go resolves three kinds of tool: the built-ins, the **local** in-process MCP
//! server (`internal/tools`), and one MCP server per configured **integration**
//! (`internal/integrations`). Rust has neither of the latter two — they are
//! #277's and #282's — and an agent whose capabilities name them would get a
//! subprocess with the tools silently missing, which is a worse answer than the
//! sidecar's.
//!
//! So [`build_options`] refuses: an agent with `local` or `mcp` capabilities
//! returns `Err`, the seam forwards, and Go runs that chat exactly as before.
//! This is the same "a route moves only when Rust reproduces every effect" rule
//! #274 established, applied per *agent* rather than per route — which is why
//! `chat::stream` also forwards `/input`, `/permission` and `/stop` for any chat
//! it does not itself hold a live session for. Without that, a chat running on
//! Go would have its stop button claimed by a Rust registry that never saw it.

use rusqlite::OptionalExtension;

use crate::claude::options::Options;
use crate::claude::permissions::PermissionHandler;
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

/// What the chat service knows about the turn before the subprocess starts.
pub struct RunSpec {
    /// The agent, or `None` for a chat with no agent slug — which Go models as
    /// a synthesized config carrying only a model.
    pub agent: Option<Agent>,
    /// The model a chat with **no agent** runs: `session.model`, or the user's
    /// default when the session has none.
    ///
    /// A closure, because computing it opens a second read-only SQLite
    /// connection and loads the settings row — and Go never does that for a
    /// chat that names an agent. `resolveAgentConfig` returns the agent's own
    /// config outright in that case and the default is only read in the
    /// no-agent branch, so an eager value is work whose result is thrown away.
    /// Called at most once, in the one arm below that needs it.
    pub fallback_model: Box<dyn Fn() -> String + Send + Sync>,
    pub working_dir: String,
    pub settings_profile_id: String,
    /// `Some` resumes an existing CLI session; `None` pins a new one to the
    /// chat id so the two identifiers stay in step.
    pub resume_session_id: Option<String>,
    pub chat_id: String,
}

/// Build the SDK options for one chat turn.
///
/// `Err` means "this port cannot run this chat" and forwards to Go — never a
/// user-visible failure.
pub fn build_options(
    spec: &RunSpec,
    permission_handler: PermissionHandler,
) -> Result<Options, String> {
    let caps = spec.agent.as_ref().map(|a| &a.capabilities);
    if let Some(caps) = caps {
        if capability_count(caps.local.as_ref()) > 0
            || caps.mcp.as_ref().is_some_and(|m| !m.is_empty())
        {
            return Err(
                "agent uses local or MCP tools, which are not ported yet (#277/#282)".to_string(),
            );
        }
    }

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

    let config_dir = resolve_agent_config_dir(spec.agent.as_ref());
    if config_dir != crate::native::settings::default_claude_config_dir()
        || std::env::var("CLAUDE_CONFIG_DIR").is_ok_and(|v| !v.is_empty())
    {
        // Overrides the value the process inherited, which is what lets a
        // per-agent override beat the `CLAUDE_CONFIG_DIR` the app started with.
        opts = opts.with_env([("CLAUDE_CONFIG_DIR", config_dir.clone())]);
    }

    if let Some(path) = settings_file_in(&config_dir, &spec.settings_profile_id) {
        // Only name a file that exists: a config dir Claude Code has never
        // written has no settings.json, and `--settings` on a missing path is
        // an error rather than a no-op.
        opts = opts.with_settings(path);
    }

    // An interactive permission handler forces default permissions, overriding
    // whatever the agent configured. That is Go's behaviour and it is why a
    // `plan` or `dontAsk` agent still prompts in the chat UI.
    opts = opts.with_default_permissions();

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
        None => (spec.fallback_model)(),
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

    match &spec.resume_session_id {
        Some(id) if !id.is_empty() => opts = opts.with_session_id_to_resume(id.clone()),
        _ => opts = opts.with_session_id(spec.chat_id.clone()),
    }

    opts = opts.with_thinking(thinking_mode(spec.agent.as_ref()));

    let allowed = allowed_tools(caps);
    if !allowed.is_empty() {
        opts = opts.with_allowed_tools(allowed.iter().cloned());
        // Go explicitly disallows every built-in that was not selected, so an
        // allowlist is a denylist too.
        let disallowed: Vec<String> = ALL_BUILT_IN_TOOLS
            .iter()
            .filter(|t| !allowed.iter().any(|a| a == *t))
            .map(|t| (*t).to_string())
            .collect();
        if !disallowed.is_empty() {
            opts = opts.with_disallowed_tools(disallowed);
        }
    }

    opts = opts.with_permission_handler(wrap_permission_handler(permission_handler, allowed));
    Ok(opts)
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
fn resolve_agent_config_dir(agent: Option<&Agent>) -> String {
    if let Some(agent) = agent {
        if !agent.claude_config_dir.is_empty() {
            let normalized = crate::native::settings::normalize(&agent.claude_config_dir);
            if let Some(dir) = crate::native::settings::absolute_dir(&normalized) {
                return dir;
            }
        }
    }
    // A relative override is discarded rather than honoured: the subprocess
    // resolves it against its own cwd — inside the user's repo — and would read
    // it as trusted config.
    crate::native::settings::run_config_dir(&stored_config_dir())
}

fn stored_config_dir() -> String {
    let Some(db_path) = crate::paths::database_path() else {
        return String::new();
    };
    let Ok(conn) = crate::native::db::open_read_only(&db_path) else {
        return String::new();
    };
    crate::native::settings::load_stored(&conn).claude_config_dir
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
fn settings_file_in(config_dir: &str, profile_id: &str) -> Option<String> {
    // Go reads the index with `LoadProfilesMetadata()`, which resolves
    // `ClaudeSettingsProfilesPath()` — the **run default** dir, not `config_dir`.
    // That asymmetry is deliberate upstream and load-bearing here: profiles are
    // a global CRUD surface, so an agent with its own `claude_config_dir` still
    // resolves its named profile against the global index while its *fallback*
    // follows its own dir. Reading the index out of the override would silently
    // find nothing and hand the run the wrong account's settings.
    //
    // Resolved only for a *named* profile: the unnamed fallback reads no index,
    // and this opens the database.
    let index_dir = if profile_id.is_empty() {
        String::new()
    } else {
        crate::native::settings::run_config_dir(&stored_config_dir())
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
fn interpolate(prompt: &str) -> String {
    if !prompt.contains("{{") {
        return prompt.to_string();
    }
    let now = chrono::Local::now();
    prompt
        .replace("{{current_date}}", &now.format("%Y-%m-%d").to_string())
        .replace("{{current_time}}", &now.format("%H:%M:%S").to_string())
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
                    settings_profile_id
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
            // wording. Forward rather than reproduce a second 404 shape.
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

    /// A `RunSpec` whose fallback records whether it was ever asked for.
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
                fallback_model: Box::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    "fallback-model".to_string()
                }),
                working_dir: String::new(),
                settings_profile_id: String::new(),
                resume_session_id: None,
                chat_id: "c1".into(),
            },
            calls,
        )
    }

    fn no_op_handler() -> PermissionHandler {
        std::sync::Arc::new(|_, _, _| {
            Box::pin(async { crate::claude::permissions::PermissionResult::allow() })
        })
    }

    /// The whole of #299: a chat that names an agent never asks for the
    /// fallback, because Go's `resolveAgentConfig` returns the agent's config
    /// outright and never reads the user's default. Resolving it eagerly opened
    /// a second read-only SQLite connection and loaded the settings row on every
    /// turn, to discard the answer.
    #[test]
    fn an_agent_chat_never_resolves_the_fallback_model() {
        let mut agent = agent_with(Capabilities::default());
        agent.model = "agent-model".into();
        let (spec, calls) = spec_with(Some(agent));
        let opts = build_options(&spec, no_op_handler()).expect("options");

        assert_eq!(opts.model, "agent-model");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the fallback was resolved for a chat that cannot use it"
        );
    }

    /// The other half of the same rule, and a real parity fix rather than a
    /// cleanup: Go branches on whether the chat **names an agent**, not on
    /// whether that agent has a model. `runner.go` sets a model only when
    /// `agentCfg.Model != ""`, so an agent with an empty one runs with **no
    /// model option at all** — the session's model and the user's default are
    /// never consulted for it.
    #[test]
    fn an_agent_with_no_model_runs_with_none_rather_than_the_fallback() {
        let mut agent = agent_with(Capabilities::default());
        agent.model = String::new();
        let (spec, calls) = spec_with(Some(agent));
        let opts = build_options(&spec, no_op_handler()).expect("options");

        // `with_model` is never called, so the SDK's own default stands —
        // which is what Go's `defaultOptions()` leaves in place too.
        assert_eq!(
            opts.model,
            crate::claude::options::DEFAULT_MODEL,
            "an agent's empty model is not a request for the session's or the user's"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// The one branch that does need it — and it is asked exactly once.
    #[test]
    fn a_chat_with_no_agent_resolves_the_fallback_once() {
        let (spec, calls) = spec_with(None);
        let opts = build_options(&spec, no_op_handler()).expect("options");

        assert_eq!(opts.model, "fallback-model");
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
            },
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

    /// The boundary this port draws: an agent needing tools Rust cannot supply
    /// forwards, rather than running with them silently missing.
    #[test]
    fn an_agent_needing_mcp_refuses_to_build_options() {
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "github".to_string(),
            crate::native::agents::McpCapability { tools: None },
        );
        let spec = RunSpec {
            agent: Some(agent_with(Capabilities {
                built_in: None,
                local: None,
                mcp: Some(mcp.into()),
            })),
            fallback_model: Box::new(String::new),
            working_dir: String::new(),
            settings_profile_id: String::new(),
            resume_session_id: None,
            chat_id: "c1".into(),
        };
        let handler: PermissionHandler = std::sync::Arc::new(|_, _, _| {
            Box::pin(async { crate::claude::permissions::PermissionResult::allow() })
        });
        assert!(build_options(&spec, handler).is_err());
    }

    #[test]
    fn an_agent_needing_local_tools_also_refuses() {
        let spec = RunSpec {
            agent: Some(agent_with(Capabilities {
                built_in: None,
                local: Some(vec!["now".into()].into()),
                mcp: None,
            })),
            fallback_model: Box::new(String::new),
            working_dir: String::new(),
            settings_profile_id: String::new(),
            resume_session_id: None,
            chat_id: "c1".into(),
        };
        let handler: PermissionHandler = std::sync::Arc::new(|_, _, _| {
            Box::pin(async { crate::claude::permissions::PermissionResult::allow() })
        });
        assert!(build_options(&spec, handler).is_err());
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
