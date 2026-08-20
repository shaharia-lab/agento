//! Error types, ported from `claude/errors.go`.
//!
//! The Go SDK exposes four distinct error types rather than one enum, because a
//! caller branches on them (`CLINotFoundError` means "install the CLI",
//! `InitializeError{Timeout}` means "the CLI was still starting"). Rust models
//! the same distinctions as variants of one enum so `?` composes, and keeps the
//! `Display` text byte-for-byte identical to Go's — the strings surface in the
//! chat UI and in logs, and a port that reworded them would look like a
//! behaviour change to anyone reading a bug report.

use std::fmt;

/// Everything that can go wrong talking to the `claude` CLI.
#[derive(Debug)]
pub enum Error {
    /// The `claude` binary could not be found or executed.
    CliNotFound { executable: String },

    /// The subprocess exited non-zero.
    Process {
        exit_code: i32,
        stderr: String,
        message: String,
    },

    /// A JSON line from stdout could not be decoded.
    CliJsonDecode { line: String, source: String },

    /// The initialize handshake was rejected, or never acknowledged.
    ///
    /// The session never started and the subprocess has been shut down. A
    /// rejection usually points at something in the options the CLI could not
    /// accept (an invalid agent definition, an unusable MCP server config);
    /// `timeout` means the CLI was still starting up, which MCP servers can
    /// make slow.
    Initialize { message: String, timeout: bool },

    /// The stream ended, or the caller's context was cancelled, before the
    /// operation completed.
    Cancelled,

    /// Anything else, carrying the message Go would have formatted.
    Other(String),
}

impl Error {
    /// Builds the `claude: <context>: <source>` wrapping Go produces with `%w`.
    pub(crate) fn wrap(context: &str, source: impl fmt::Display) -> Self {
        Error::Other(format!("claude: {context}: {source}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CliNotFound { executable } => {
                write!(f, "claude: binary not found: {executable:?}")
            }
            Error::Process {
                exit_code,
                stderr,
                message,
            } => {
                let detail = if stderr.is_empty() { message } else { stderr };
                write!(f, "claude: process error (exit {exit_code}): {detail}")
            }
            Error::CliJsonDecode { line, source } => {
                write!(f, "claude: JSON decode error: {source} (line: {line})")
            }
            Error::Initialize { message, timeout } => {
                if *timeout {
                    write!(f, "claude: initialize timed out: {message}")
                } else {
                    write!(f, "claude: initialize rejected by the CLI: {message}")
                }
            }
            Error::Cancelled => write!(f, "claude: cancelled"),
            Error::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// The crate-wide result type.
pub type Result<T> = std::result::Result<T, Error>;
