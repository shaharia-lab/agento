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
    /// `session.model`, or the user's default when the session has none.
    pub fallback_model: String,
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

    let model = spec
        .agent
        .as_ref()
        .map(|a| a.model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| spec.fallback_model.clone());
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

fn capability_count(list: Option<&Vec<String>>) -> usize {
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
        return caps.built_in.clone().unwrap_or_default();
    }
    // `map_or(true, ..)` rather than `is_none_or`, which needs Rust 1.82 and
    // this crate's MSRV is 1.77.
    if capability_count(caps.local.as_ref()) == 0
        && caps.mcp.as_ref().map_or(true, |m| m.is_empty())
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

/// `LoadProfileFilePathIn`, reduced to the case this port supports: the unnamed
/// fallback, `<config dir>/settings.json`.
///
/// A **named** profile records its own absolute path in
/// `settings_profiles.json`, and reading that file is the settings-profile
/// service's job rather than this one's — so a chat pinned to a named profile
/// returns `None` here and simply gets no `--settings`, which is what Go does
/// when the recorded path does not exist. Porting the profile lookup belongs
/// with the profile CRUD.
fn settings_file_in(config_dir: &str, profile_id: &str) -> Option<String> {
    if !profile_id.is_empty() {
        return None;
    }
    let path = std::path::Path::new(config_dir).join("settings.json");
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Some(path.to_string_lossy().into_owned()),
        _ => None,
    }
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

    #[test]
    fn an_explicit_built_in_list_wins() {
        let caps = Capabilities {
            built_in: Some(vec!["Read".into(), "Bash".into()]),
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
                tools: Some(vec!["list_prs".into()]),
            },
        );
        let caps = Capabilities {
            built_in: None,
            local: None,
            mcp: Some(mcp),
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
                mcp: Some(mcp),
            })),
            fallback_model: String::new(),
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
                local: Some(vec!["now".into()]),
                mcp: None,
            })),
            fallback_model: String::new(),
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

    /// A named profile is not resolvable here, so it yields no `--settings`
    /// rather than a guessed path.
    #[test]
    fn a_named_settings_profile_yields_no_settings_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("settings.json"), "{}").expect("write");
        let path = dir.path().to_string_lossy().into_owned();

        assert!(settings_file_in(&path, "").is_some());
        assert!(settings_file_in(&path, "profile-123").is_none());
    }

    #[test]
    fn a_missing_settings_file_is_not_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(settings_file_in(&dir.path().to_string_lossy(), "").is_none());
    }
}
