//! Out-of-band session introspection, ported from `claude/sessions.go`.
//!
//! These do not use the streaming protocol at all — they shell out to
//! `claude sessions …` and parse its JSON. They are here rather than in
//! [`super::process`] because they share nothing with it but the executable
//! path and the environment.

use serde::Deserialize;
use serde_json::value::RawValue;

use super::errors::{Error, Result};
use super::options::Options;
use super::process::build_env;

/// Metadata about a stored session, as returned by
/// `claude sessions list --output-format json`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SessionSummary {
    pub id: String,
    pub project: String,
    pub created_at: String,
    pub updated_at: String,
    pub summary: String,
}

/// The messages of a stored session, as returned by
/// `claude sessions get <id> --output-format json`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SessionTranscript {
    pub id: String,
    /// Kept raw: the transcript's message shapes are the CLI's, and a stored
    /// transcript can carry forms this SDK does not model.
    pub messages: Vec<Box<RawValue>>,
}

/// Runs the CLI with `args` and returns its stdout, honouring the executable
/// path, environment and working directory from `opts`.
async fn run_cli(opts: &Options, args: &[&str], context: &str) -> Result<Vec<u8>> {
    let mut command = tokio::process::Command::new(&opts.claude_executable);
    command.args(args);
    command.env_clear();
    command.envs(build_env(opts));
    if !opts.cwd.is_empty() {
        command.current_dir(&opts.cwd);
    }

    let output = command
        .output()
        .await
        .map_err(|e| Error::wrap(context, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Error::Process {
            exit_code: output.status.code().unwrap_or(-1),
            stderr,
            message: context.to_string(),
        });
    }

    Ok(output.stdout)
}

/// Runs `claude sessions list --output-format json` and returns the parsed
/// list.
pub async fn list_sessions(opts: &Options) -> Result<Vec<SessionSummary>> {
    let out = run_cli(
        opts,
        &["sessions", "list", "--output-format", "json"],
        "sessions list",
    )
    .await?;

    serde_json::from_slice(&out).map_err(|e| Error::wrap("sessions list: unmarshal", e))
}

/// Runs `claude sessions get <id> --output-format json` and returns the raw
/// transcript.
pub async fn get_session_messages(opts: &Options, session_id: &str) -> Result<SessionTranscript> {
    if session_id.is_empty() {
        return Err(Error::Other(
            "claude: sessions get: session_id must not be empty".to_string(),
        ));
    }

    let context = format!("sessions get {session_id}");
    let out = run_cli(
        opts,
        &["sessions", "get", session_id, "--output-format", "json"],
        &context,
    )
    .await?;

    serde_json::from_slice(&out).map_err(|e| Error::wrap(&format!("{context}: unmarshal"), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_empty_session_id_is_rejected_without_spawning() {
        let opts = Options::new().with_claude_executable("/nonexistent/claude");
        let err = get_session_messages(&opts, "").await.unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn a_summary_decodes_with_every_field_optional() {
        let list: Vec<SessionSummary> =
            serde_json::from_str(r#"[{"id":"abc"},{"id":"def","project":"/p"}]"#).unwrap();
        assert_eq!(list[0].id, "abc");
        assert!(list[0].project.is_empty());
        assert_eq!(list[1].project, "/p");
    }

    #[test]
    fn a_transcript_keeps_its_messages_verbatim() {
        let t: SessionTranscript =
            serde_json::from_str(r#"{"id":"s1","messages":[{"unknown_shape":[1,2]}]}"#).unwrap();
        assert_eq!(t.id, "s1");
        assert_eq!(t.messages[0].get(), r#"{"unknown_shape":[1,2]}"#);
    }
}
