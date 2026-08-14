//! Subprocess lifecycle and the control protocol, ported from
//! `claude/process.go`.
//!
//! The SDK does not call the Claude API. It spawns the `claude` CLI in
//! bidirectional JSON-lines mode (`--input-format stream-json
//! --output-format stream-json --verbose`, **no `--print`**) and speaks two
//! interleaved protocols down one pair of pipes:
//!
//! * the **message stream** — assistant turns, tool results, deltas, the final
//!   result — which flows to the caller;
//! * the **control protocol** — `control_request` / `control_response` pairs
//!   correlated by `request_id` — which never reaches the caller.
//!
//! ## The handshake order is the whole file's reason for existing
//!
//! `initialize` must be written *after* the reader task is live, because the
//! acknowledgement is a `control_response` and nothing can route one until
//! something is reading stdout and `pending` holds the id. The first user
//! message must be written *after* the acknowledgement, because MCP servers,
//! agents and hooks are configured during it. Getting this wrong does not fail
//! loudly — it races, and the turn starts against a half-configured CLI.
//!
//! ## Two failure modes that are silent by construction
//!
//! * **Every inbound `control_request` must be answered.** A missing reply
//!   hangs the CLI with no error on either side, so every branch of
//!   [`handle_control_request`] writes exactly one response, including the
//!   default branch for requests we only acknowledge.
//! * **`sdkMcpServers` is never sent.** The CLI accepts only an array of
//!   strings there, and rejecting it fails the *entire* initialize — silently
//!   taking hooks, agents, the system prompt and the output format down with
//!   it. Naming a server there would also mark it SDK-hosted, so the CLI would
//!   drop its transport and route tool calls back over `mcp_message`, which
//!   this SDK does not implement. See [`initialize_msg`].

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use serde_json::value::RawValue;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

use super::client::{Stream, StreamControl};
use super::errors::{Error, Result};
use super::hooks::{build_hooks_for_initialize, HookRegistry};
use super::init_types::decode_initialize_response;
use super::messages::{error_event, message_type, parse_line, system_subtype, Event};
use super::options::{thinking, Options};
use super::permissions::{PermissionContext, PermissionResult};
use super::SDK_VERSION;

/// Capacity of the event channel, matching Go's `make(chan Event, 32)`.
const EVENT_CHANNEL_CAPACITY: usize = 32;

/// Ceiling on one stdout line. Go sets the same figure on its `bufio.Scanner`
/// because assistant messages with long content are large; Rust's reader has no
/// default limit at all, so the cap is imposed deliberately rather than
/// inherited, and an over-long line ends the stream with a read error the way
/// Go's scanner does.
///
/// It bounds what is *accepted*, not what is *allocated*: `read_until` has
/// already grown its buffer to the newline by the time the length is checked,
/// where Go's scanner refuses past its fixed buffer. That is a real difference
/// and would matter against a hostile writer; the writer here is the `claude`
/// CLI, spawned by us.
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// How long a terminating process is given before it is killed outright.
const SIGKILL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// A reply to one of our outbound control requests, once correlated.
#[derive(Debug, Default)]
pub(crate) struct ControlResponse {
    pub success: bool,
    pub error: String,
    /// The **innermost** response payload. `None` on replies that carry no
    /// data, which is still a success.
    pub body: Option<Box<RawValue>>,
}

/// Everything the reader task and the control methods share.
pub(crate) struct Shared {
    /// Serialises writes to the subprocess stdin. Safe to use from any task.
    pub stdin: Mutex<Option<tokio::process::ChildStdin>>,
    /// Maps `request_id` → the caller waiting for its reply.
    pub pending: StdMutex<HashMap<String, oneshot::Sender<ControlResponse>>>,
    /// Captured from the `system`/`init` event rather than from the initialize
    /// response, which does not carry it — so it is unknown until a turn
    /// starts.
    pub capabilities: RwLock<Option<Vec<String>>>,
    /// Triggers graceful shutdown, once.
    shutdown_tx: StdMutex<Option<oneshot::Sender<()>>>,
    shutdown_fired: AtomicBool,
}

impl Shared {
    /// Serialises `value` as a JSON line and sends it to stdin.
    pub(crate) async fn write(&self, value: &serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_vec(value)
            .map_err(|e| Error::wrap("encoding a message for stdin", e))?;
        line.push(b'\n');

        let mut guard = self.stdin.lock().await;
        let Some(stdin) = guard.as_mut() else {
            return Err(Error::Other("claude: stdin is closed".into()));
        };
        stdin
            .write_all(&line)
            .await
            .map_err(|e| Error::wrap("writing to stdin", e))?;
        stdin
            .flush()
            .await
            .map_err(|e| Error::wrap("flushing stdin", e))
    }

    /// Closes the subprocess stdin. Used on graceful shutdown, and after the
    /// result in single-turn mode.
    pub(crate) async fn close_stdin(&self) {
        let mut guard = self.stdin.lock().await;
        // Dropping the handle closes the pipe, which is what tells the CLI no
        // more input is coming.
        guard.take();
    }

    /// Triggers graceful shutdown. Idempotent.
    ///
    /// Deliberately **not** reachable from `interrupt`, which aborts the turn
    /// via a control request and leaves the subprocess running.
    pub(crate) fn shutdown(&self) {
        if self.shutdown_fired.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(mut slot) = self.shutdown_tx.lock() {
            if let Some(tx) = slot.take() {
                let _ = tx.send(());
            }
        }
    }

    /// Records the capability list seen on a `system`/`init` event.
    fn set_capabilities(&self, caps: Vec<String>) {
        if let Ok(mut slot) = self.capabilities.write() {
            *slot = Some(caps);
        }
    }
}

/// Generates a random UUID v4, lowercase and hyphenated — the format the CLI
/// sees from every SDK. Used for both request ids and hook callback ids.
pub(crate) fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── Spawn ───────────────────────────────────────────────────────────────────

/// Starts the `claude` subprocess and completes the initialize handshake.
///
/// Returns once the CLI has acknowledged `initialize` — so by the time the
/// caller holds a [`Stream`], MCP servers, agents and hooks are configured. In
/// single-turn mode the first user message has also been written.
pub(crate) async fn spawn_and_stream(opts: Options, prompt: &str) -> Result<Stream> {
    opts.validate()?;
    opts.warn_permission_handler_shadowed();

    let args = opts.build_args();

    let mut command = tokio::process::Command::new(&opts.claude_executable);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Killing the child when the handle drops is a backstop only; the
        // shutdown path below is the one that runs in practice, and it is the
        // one that gives the CLI a chance to exit cleanly.
        .kill_on_drop(true);

    if !opts.cwd.is_empty() {
        command.current_dir(&opts.cwd);
    }

    command.env_clear();
    command.envs(build_env(&opts));

    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::CliNotFound {
                executable: opts.claude_executable.clone(),
            }
        } else {
            Error::Other(format!("claude: start {:?}: {e}", opts.claude_executable))
        }
    })?;

    let stdin = child.stdin.take().ok_or_else(|| {
        Error::Other("claude: stdin pipe: the child exposed no stdin".to_string())
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        Error::Other("claude: stdout pipe: the child exposed no stdout".to_string())
    })?;
    let stderr = child.stderr.take();
    let pid = child.id();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let shared = Arc::new(Shared {
        stdin: Mutex::new(Some(stdin)),
        pending: StdMutex::new(HashMap::new()),
        capabilities: RwLock::new(None),
        shutdown_tx: StdMutex::new(Some(shutdown_tx)),
        shutdown_fired: AtomicBool::new(false),
    });

    // Hook config for the initialize message, plus the registry the reader
    // dispatches `hook_callback` requests through.
    let (hooks_config, hook_registry) = build_hooks_for_initialize(&opts.hooks);

    // Capture stderr. Each line goes to the callback when one is set, and every
    // line is buffered for error reporting on an unexpected exit.
    let stderr_buf = Arc::new(StdMutex::new(String::new()));
    if let Some(stderr) = stderr {
        let buf = stderr_buf.clone();
        let sink = opts.stderr.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(sink) = &sink {
                    sink(&line);
                }
                if let Ok(mut buf) = buf.lock() {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
        });
    }

    let (event_tx, event_rx) = mpsc::channel::<Event>(EVENT_CHANNEL_CAPACITY);
    // A watch rather than a oneshot: the shutdown task waits on it twice —
    // once for the shutdown-vs-exit race, once for the SIGTERM grace period —
    // and a oneshot receiver is consumed by its first await.
    let (proc_done_tx, proc_done_rx) = tokio::sync::watch::channel(false);
    // A second view for the handshake wait below, so a process that dies during
    // startup is reported at once instead of at the timeout.
    let proc_done_watch = proc_done_rx.clone();

    // Graceful shutdown task, mirroring the TypeScript SDK's close():
    //   stdin.end() → SIGTERM → SIGKILL after 5s.
    //
    // Both waits are raced against the process actually exiting, and both are
    // `biased` toward that branch. **Never signal a pid that has already been
    // reaped**: the reader task calls `wait()`, after which the kernel is free
    // to hand that pid to an unrelated process, and a stray SIGTERM/SIGKILL
    // would land on it. Go guards the same two points with `select` on
    // `procDone` for exactly this reason. `biased` matters because on the
    // ordinary path — result received, child reaped, stream dropped — both
    // branches are ready at once and an unbiased select would sometimes pick
    // the signalling one.
    {
        let shared = shared.clone();
        let mut proc_done = proc_done_rx;
        tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = proc_done.changed() => return,
                _ = shutdown_rx => {}
            }
            shared.close_stdin().await;

            if *proc_done.borrow() {
                return;
            }
            terminate(pid);

            tokio::select! {
                biased;
                _ = proc_done.changed() => {}
                _ = tokio::time::sleep(SIGKILL_GRACE) => kill(pid),
            }
        });
    }

    // Reader task: owns stdout, handles control messages, forwards everything
    // else to the caller.
    {
        let shared = shared.clone();
        let opts_for_reader = opts.clone();
        let stderr_buf = stderr_buf.clone();
        tokio::spawn(async move {
            let session_mode = opts_for_reader.session_mode;
            let mut reader = BufReader::new(stdout);
            let mut got_result = false;
            let mut read_error: Option<String> = None;

            loop {
                let line = match read_line(&mut reader).await {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        read_error = Some(e);
                        break;
                    }
                };
                if line.is_empty() {
                    continue;
                }

                // Peek at the message type for fast routing.
                #[derive(serde::Deserialize)]
                struct TypeCheck {
                    #[serde(default, rename = "type")]
                    message_type: String,
                }
                let Ok(peek) = serde_json::from_slice::<TypeCheck>(&line) else {
                    continue; // skip non-JSON lines
                };

                match peek.message_type.as_str() {
                    // These require a response on stdin and must not be
                    // forwarded to the caller.
                    "control_request" => {
                        handle_control_request(&line, &shared, &opts_for_reader, &hook_registry)
                            .await;
                        continue;
                    }
                    // Replies to our own requests. Route to the pending map.
                    "control_response" => {
                        route_control_response(&line, &shared);
                        continue;
                    }
                    _ => {}
                }

                let Some(event) = parse_line(&line) else {
                    continue; // skip malformed lines
                };

                // The CLI advertises its protocol capabilities on system/init,
                // not in the initialize control response, so they are captured
                // here as the event flows past.
                if let Some(system) = &event.system {
                    if system.subtype == system_subtype::INIT {
                        shared.set_capabilities(system.capabilities.clone());
                    }
                }

                let is_result = event.event_type == message_type::RESULT;
                if event_tx.send(event).await.is_err() {
                    // The caller dropped the stream; stop reading.
                    break;
                }

                if is_result {
                    if session_mode {
                        // Emit the result to signal "turn done" but keep stdin
                        // open and the reader running, so the subprocess
                        // survives for the next send.
                    } else {
                        got_result = true;
                        shared.close_stdin().await;
                        break;
                    }
                }
            }

            if let Some(err) = read_error {
                let _ = event_tx
                    .send(error_event(format!("stdout read error: {err}")))
                    .await;
            }

            // Surface stderr on an unexpected exit (bad flag, auth error,
            // crash). Suppressed when the caller asked us to stop.
            let exit = child.wait().await;
            let shutting_down = shared.shutdown_fired.load(Ordering::SeqCst);
            if !got_result && !shutting_down {
                let failed = match &exit {
                    Ok(status) => !status.success(),
                    Err(_) => true,
                };
                if failed {
                    let captured = stderr_buf
                        .lock()
                        .map(|b| b.trim().to_string())
                        .unwrap_or_default();
                    let msg = if !captured.is_empty() {
                        captured
                    } else {
                        match &exit {
                            Ok(status) => format!("claude exited with {status}"),
                            Err(e) => e.to_string(),
                        }
                    };
                    let _ = event_tx.send(error_event(msg)).await;
                }
            }

            let _ = proc_done_tx.send(true);
        });
    }

    // ── initialize handshake ────────────────────────────────────────────────
    // The reader task is now live, so a control_response can be routed.
    // Register the request id BEFORE writing, await the acknowledgement, and
    // cache the payload — it is the SDK's only source of truth for the CLI's
    // models, commands, agents and account.
    let init_request_id = new_uuid();
    let (init_tx, init_rx) = oneshot::channel::<ControlResponse>();
    if let Ok(mut pending) = shared.pending.lock() {
        pending.insert(init_request_id.clone(), init_tx);
    }

    if let Err(e) = shared
        .write(&initialize_msg(&init_request_id, &opts, hooks_config))
        .await
    {
        shared.shutdown();
        return Err(Error::wrap("initialize", e));
    }

    // The wait races three outcomes rather than Go's two. Go waits only for the
    // acknowledgement or the timeout, so a CLI that dies during startup — an
    // unusable `--settings` path, a bad flag, a failed auth — costs the caller
    // the full 60s before it hears anything, even though the answer was known
    // in milliseconds. The pending sender lives in `shared`, not in the reader
    // task, so nothing else would report it either. `biased` puts the
    // acknowledgement first: a reply already in flight when the process exits
    // is still a successful handshake.
    let mut init_proc_done = proc_done_watch;
    let init_response = {
        let outcome = tokio::select! {
            biased;
            reply = init_rx => Some(reply),
            _ = init_proc_done.changed() => None,
            _ = tokio::time::sleep(opts.init_timeout()) => {
                shared.shutdown();
                return Err(Error::Initialize {
                    message: format!(
                        "the CLI did not acknowledge initialize within {:?}",
                        opts.init_timeout()
                    ),
                    timeout: true,
                });
            }
        };

        match outcome {
            Some(Ok(response)) => {
                if !response.success {
                    shared.shutdown();
                    return Err(Error::Initialize {
                        message: response.error,
                        timeout: false,
                    });
                }
                decode_initialize_response(response.body.as_deref())
            }
            // The process exited, or its sender was dropped, before
            // acknowledging. Either way the session never started.
            _ => {
                shared.shutdown();
                let stderr = stderr_buf
                    .lock()
                    .map(|b| b.trim().to_string())
                    .unwrap_or_default();
                let message = if stderr.is_empty() {
                    "the CLI exited before acknowledging initialize".to_string()
                } else {
                    format!("the CLI exited before acknowledging initialize: {stderr}")
                };
                return Err(Error::Initialize {
                    message,
                    timeout: false,
                });
            }
        }
    };

    // Only now is it safe to start a turn: MCP servers, agents and hooks
    // declared in the initialize message are configured. Session mode sends its
    // first message through `send` instead.
    if !opts.session_mode && !prompt.is_empty() {
        if let Err(e) = shared.write(&user_msg(prompt)).await {
            shared.shutdown();
            return Err(Error::wrap("user message", e));
        }
    }

    Ok(Stream::new(
        event_rx,
        StreamControl::new(shared, Arc::new(init_response)),
    ))
}

/// Starts a persistent subprocess for multi-turn conversations.
///
/// Unlike [`spawn_and_stream`] it sends no initial user message — the caller
/// sends each turn. The subprocess survives multiple results and exits only
/// when the session is closed.
pub(crate) async fn spawn_session(mut opts: Options) -> Result<Stream> {
    opts.session_mode = true;
    spawn_and_stream(opts, "").await
}

/// Reads one newline-terminated line, enforcing [`MAX_LINE_BYTES`].
///
/// Returns `Ok(None)` at end of stream.
async fn read_line<R>(reader: &mut BufReader<R>) -> std::result::Result<Option<Vec<u8>>, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    match reader.read_until(b'\n', &mut buf).await {
        Ok(0) => Ok(None),
        Ok(_) => {
            if buf.len() > MAX_LINE_BYTES {
                return Err(format!(
                    "a stdout line exceeded the {MAX_LINE_BYTES}-byte limit"
                ));
            }
            while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                buf.pop();
            }
            Ok(Some(buf))
        }
        Err(e) => Err(e.to_string()),
    }
}

// ─── Inbound control requests ────────────────────────────────────────────────

/// The envelope of an inbound `control_request`.
#[derive(serde::Deserialize, Default)]
struct InboundControl {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    request: InboundControlRequest,
}

#[derive(serde::Deserialize, Default)]
struct InboundControlRequest {
    #[serde(default)]
    subtype: String,

    // can_use_tool
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    input: Option<Box<RawValue>>,

    // hook_callback
    #[serde(default)]
    callback_id: String,
    #[serde(default)]
    tool_use_id: String,
}

/// Answers a `control_request` with an error `control_response`.
///
/// Every inbound request must be answered — a missing reply hangs the CLI.
async fn write_control_error(shared: &Shared, request_id: &str, message: &str) {
    let _ = shared
        .write(&serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "error",
                "request_id": request_id,
                "error": message,
            }
        }))
        .await;
}

/// Answers a `control_request` with a success `control_response`, optionally
/// carrying a payload.
async fn write_control_success(
    shared: &Shared,
    request_id: &str,
    payload: Option<serde_json::Value>,
) {
    let mut response = serde_json::json!({
        "subtype": "success",
        "request_id": request_id,
    });
    if let Some(payload) = payload {
        response["response"] = payload;
    }
    let _ = shared
        .write(&serde_json::json!({
            "type": "control_response",
            "response": response,
        }))
        .await;
}

/// Handles one inbound `control_request` and writes exactly one response.
///
/// Callbacks are awaited **inline**, mirroring Go, where they run on the
/// goroutine that owns stdout: a slow handler stalls the message stream, which
/// is what makes "block the turn until a human answers" work.
pub(crate) async fn handle_control_request(
    line: &[u8],
    shared: &Shared,
    opts: &Options,
    hook_registry: &HookRegistry,
) {
    let Ok(envelope) = serde_json::from_slice::<InboundControl>(line) else {
        return;
    };
    let request_id = envelope.request_id.as_str();

    match envelope.request.subtype.as_str() {
        "can_use_tool" => {
            // Fail closed. Answering a permission question nobody was asked
            // would grant the tool call, so an absent handler is an error,
            // never an allow.
            let Some(handler) = opts.permission_handler.clone() else {
                write_control_error(shared, request_id, "canUseTool callback is not provided")
                    .await;
                return;
            };

            let context: PermissionContext = serde_json::from_slice::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("request").cloned())
                .and_then(|r| serde_json::from_value(r).ok())
                .unwrap_or_default();

            let original_input = envelope
                .request
                .input
                .as_ref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw.get()).ok());

            let input_for_handler = envelope
                .request
                .input
                .as_ref()
                .and_then(|raw| RawValue::from_string(raw.get().to_owned()).ok());

            let result = handler(
                envelope.request.tool_name.clone(),
                input_for_handler,
                context,
            )
            .await;

            let payload = match result {
                PermissionResult::Allow {
                    updated_input,
                    updated_permissions,
                } => {
                    let mut response = serde_json::json!({ "behavior": "allow" });
                    // The CLI expects the input it should actually run; when
                    // the handler does not rewrite it, echo the original back
                    // verbatim.
                    response["updatedInput"] = updated_input
                        .or(original_input)
                        .unwrap_or(serde_json::Value::Null);
                    if !updated_permissions.is_empty() {
                        response["updatedPermissions"] =
                            serde_json::to_value(&updated_permissions).unwrap_or_default();
                    }
                    response
                }
                PermissionResult::Deny { message, interrupt } => {
                    let mut response = serde_json::json!({
                        "behavior": "deny",
                        "message": message,
                    });
                    if interrupt {
                        response["interrupt"] = serde_json::Value::Bool(true);
                    }
                    response
                }
            };

            write_control_success(shared, request_id, Some(payload)).await;
        }

        "hook_callback" => {
            let Some(hook) = hook_registry.get(&envelope.request.callback_id) else {
                write_control_error(
                    shared,
                    request_id,
                    &format!(
                        "no hook callback found for ID: {}",
                        envelope.request.callback_id
                    ),
                )
                .await;
                return;
            };

            // The event name travels inside the hook input payload, not on the
            // control_request envelope. An absent or unparseable name yields
            // "" rather than dropping the callback — a missing reply hangs the
            // CLI.
            #[derive(serde::Deserialize, Default)]
            struct HookInput {
                #[serde(default)]
                hook_event_name: String,
            }
            let event_name = envelope
                .request
                .input
                .as_ref()
                .and_then(|raw| serde_json::from_str::<HookInput>(raw.get()).ok())
                .unwrap_or_default()
                .hook_event_name;

            let input = envelope
                .request
                .input
                .as_ref()
                .and_then(|raw| RawValue::from_string(raw.get().to_owned()).ok());

            match hook(event_name, input, envelope.request.tool_use_id.clone()).await {
                Ok(output) => {
                    let payload = output.and_then(|o| serde_json::to_value(o).ok());
                    write_control_success(shared, request_id, payload).await;
                }
                Err(message) => write_control_error(shared, request_id, &message).await,
            }
        }

        "elicitation" => {
            let cancelled = serde_json::json!({ "cancel": true });
            let payload = match opts.elicitation_handler.clone() {
                Some(handler) => {
                    let input = envelope
                        .request
                        .input
                        .as_ref()
                        .and_then(|raw| RawValue::from_string(raw.get().to_owned()).ok());
                    handler(input).await.unwrap_or(cancelled)
                }
                None => cancelled,
            };
            write_control_success(shared, request_id, Some(payload)).await;
        }

        // set_model, set_permission_mode, set_max_thinking_tokens,
        // mcp_message: read-only notifications from the CLI. Acknowledge
        // silently — but do acknowledge.
        _ => write_control_success(shared, request_id, None).await,
    }
}

// ─── Outbound control responses ──────────────────────────────────────────────

/// Routes a `control_response` to the caller waiting on its `request_id`.
///
/// The wire shape is three levels deep — the correlation id lives **inside** the
/// response object, not at the top level of the envelope:
///
/// ```text
/// {"type":"control_response","response":{"subtype":…,"request_id":…,"error":…,"response":{…}}}
/// ```
///
/// Routing is strictly nested-only, matching the reference SDKs: a line whose
/// response is not an object, or which carries no `request_id`, cannot be
/// correlated to any caller and is dropped. There is no top-level fallback —
/// the CLI has never emitted one, and inventing a shape here is what broke this
/// in the first place.
///
/// The value handed to the caller is the **innermost** response payload, not
/// the wrapper carrying subtype and request_id. The CLI omits that payload
/// entirely on replies that carry no data (a real `set_model` success), in
/// which case the body is `None` and the request still succeeded.
pub(crate) fn route_control_response(line: &[u8], shared: &Shared) {
    #[derive(serde::Deserialize, Default)]
    struct Envelope {
        #[serde(default)]
        response: Inner,
    }
    #[derive(serde::Deserialize, Default)]
    struct Inner {
        #[serde(default)]
        subtype: String,
        #[serde(default)]
        request_id: String,
        #[serde(default)]
        error: String,
        #[serde(default)]
        response: Option<Box<RawValue>>,
    }

    let Ok(envelope) = serde_json::from_slice::<Envelope>(line) else {
        return;
    };
    if envelope.response.request_id.is_empty() {
        return;
    }

    let waiting = shared
        .pending
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(&envelope.response.request_id));

    if let Some(tx) = waiting {
        // Send cannot block; a caller that has gone away simply drops the
        // reply, matching Go's non-blocking select.
        let _ = tx.send(ControlResponse {
            success: envelope.response.subtype != "error",
            error: envelope.response.error,
            body: envelope.response.response,
        });
    }
}

// ─── Stdin message builders ──────────────────────────────────────────────────

/// Builds the `initialize` control_request sent at session start.
///
/// This is how the system prompt, agents, hooks and output format are passed in
/// bidirectional mode.
///
/// `sdkMcpServers` is **deliberately never sent**. That key declares
/// SDK-*hosted* servers: the CLI keeps no transport for them and instead routes
/// their JSON-RPC traffic back as `mcp_message` control_requests, which this
/// SDK does not implement — the calls would be acknowledged and dropped.
/// `start_in_process_mcp_server` binds a real loopback HTTP listener instead,
/// so every server reaches the CLI as an ordinary transport through
/// `--mcp-config` and is dialled directly.
///
/// The CLI also accepts only an array of strings there (verified against
/// 2.1.224: omitted, `[]` and `["name"]` succeed; any object or array-of-objects
/// is rejected with "sdkMcpServers and webSearchIsolationExemptMcpServers must
/// be arrays of strings", which fails the *entire* initialize and silently takes
/// hooks, agents, the system prompt and the output format down with it).
pub(crate) fn initialize_msg(
    request_id: &str,
    opts: &Options,
    hooks_config: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    // A preset takes precedence over a plain string when both are set.
    let system_prompt = match &opts.system_prompt_preset {
        Some(preset) => serde_json::to_value(preset).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::String(opts.system_prompt.clone()),
    };

    let mut request = serde_json::json!({
        "subtype": "initialize",
        "systemPrompt": system_prompt,
        "appendSystemPrompt": opts.append_system_prompt,
        "hooks": hooks_config,
        "agents": opts.agents,
        "promptSuggestions": opts.prompt_suggestions,
    });

    if let Some(format) = &opts.output_format {
        request["outputFormat"] = serde_json::Value::String(format.format_type.clone());
        if let Some(schema) = &format.schema {
            request["jsonSchema"] = schema.clone();
        }
    }

    if let Some(sandbox) = &opts.sandbox {
        request["sandbox"] = serde_json::to_value(sandbox).unwrap_or(serde_json::Value::Null);
    }

    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": request,
    })
}

/// Builds the user message sent to stdin.
pub(crate) fn user_msg(prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": prompt,
        },
        "parent_tool_use_id": serde_json::Value::Null,
        "session_id": "",
    })
}

// ─── Environment ─────────────────────────────────────────────────────────────

/// The environment for the `claude` subprocess.
///
/// * Inherits the parent environment, so the Claude Code OAuth session passes
///   through.
/// * Strips `CLAUDECODE` so the subprocess can launch even inside an existing
///   session (mirroring `delete process.env.CLAUDECODE` in the TS SDK).
/// * Strips `CLAUDE_CODE_ENTRYPOINT` and `CLAUDE_AGENT_SDK_VERSION` so we can
///   set our own.
/// * Sets `MAX_THINKING_TOKENS=0` when thinking is disabled — the documented
///   way to turn it off — or the caller's budget when they set one.
/// * Merges the caller's extra variables **last**, so they win.
pub(crate) fn build_env(opts: &Options) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();

    for (key, value) in std::env::vars() {
        let stripped = matches!(
            key.as_str(),
            "CLAUDECODE"
                | "CLAUDE_CODE_ENTRYPOINT"
                | "CLAUDE_AGENT_SDK_VERSION"
                | "MAX_THINKING_TOKENS"
        ) || (!opts.cwd.is_empty() && key == "PWD")
            // Also strip any caller-supplied key so theirs can override.
            || opts.env.contains_key(&key);
        if !stripped {
            out.push((key, value));
        }
    }

    // The entrypoint identifies which SDK is driving the CLI, for Anthropic's
    // telemetry. The Go SDK reports `sdk-go`; this one is a different SDK and
    // says so, following the same `sdk-<language>` convention. It is a
    // reporting label only — nothing in the protocol or the SSE stream depends
    // on its value.
    out.push(("CLAUDE_CODE_ENTRYPOINT".into(), "sdk-rust".into()));
    out.push(("CLAUDE_AGENT_SDK_VERSION".into(), SDK_VERSION.into()));

    if opts.thinking == thinking::DISABLED {
        out.push(("MAX_THINKING_TOKENS".into(), "0".into()));
    } else if opts.max_thinking_tokens > 0 {
        out.push((
            "MAX_THINKING_TOKENS".into(),
            opts.max_thinking_tokens.to_string(),
        ));
    }

    // Set PWD when a working directory is configured, matching the Python SDK.
    if !opts.cwd.is_empty() {
        out.push(("PWD".into(), opts.cwd.clone()));
    }

    for (key, value) in &opts.env {
        out.push((key.clone(), value.clone()));
    }

    out
}

// ─── Signals ─────────────────────────────────────────────────────────────────

/// Sends SIGTERM, giving the CLI a chance to exit cleanly.
#[cfg(unix)]
fn terminate(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: kill(2) with a pid we spawned. A reaped pid yields ESRCH,
        // which is the outcome we already ignore.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

/// Windows has no SIGTERM; the kill below is the only available step.
#[cfg(not(unix))]
fn terminate(_pid: Option<u32>) {}

#[cfg(unix)]
fn kill(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: as above.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill(_pid: Option<u32>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::options::{permission_mode, SystemPromptPreset};

    /// A `Shared` with no subprocess behind it, for exercising response routing
    /// on its own. Mirrors Go's `pendingStream` helper.
    fn pending_shared(request_id: &str) -> (Arc<Shared>, oneshot::Receiver<ControlResponse>) {
        let (tx, rx) = oneshot::channel();
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let shared = Arc::new(Shared {
            stdin: Mutex::new(None),
            pending: StdMutex::new(HashMap::from([(request_id.to_string(), tx)])),
            capabilities: RwLock::new(None),
            shutdown_tx: StdMutex::new(Some(shutdown_tx)),
            shutdown_fired: AtomicBool::new(false),
        });
        (shared, rx)
    }

    #[test]
    fn a_response_routes_on_the_nested_request_id() {
        let (shared, rx) = pending_shared("req-1");
        route_control_response(
            br#"{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"ok":true}}}"#,
            &shared,
        );
        let got = rx.blocking_recv().expect("the caller must be woken");
        assert!(got.success);
        // The caller gets the innermost payload, not the wrapper carrying
        // subtype and request_id.
        assert_eq!(got.body.as_ref().unwrap().get(), r#"{"ok":true}"#);
    }

    #[test]
    fn a_top_level_request_id_resolves_nothing() {
        // Routing is strictly nested-only. Inventing a top-level fallback is
        // what broke this in the first place.
        let (shared, rx) = pending_shared("req-1");
        route_control_response(
            br#"{"type":"control_response","request_id":"req-1","response":{"subtype":"success"}}"#,
            &shared,
        );
        drop(shared);
        assert!(rx.blocking_recv().is_err(), "nothing should have been sent");
    }

    #[test]
    fn a_reply_with_no_payload_is_still_a_success() {
        // A real set_model success carries no inner response at all.
        let (shared, rx) = pending_shared("req-2");
        route_control_response(
            br#"{"type":"control_response","response":{"subtype":"success","request_id":"req-2"}}"#,
            &shared,
        );
        let got = rx.blocking_recv().unwrap();
        assert!(got.success);
        assert!(got.body.is_none());
    }

    #[test]
    fn an_error_subtype_carries_its_message() {
        let (shared, rx) = pending_shared("req-3");
        route_control_response(
            br#"{"type":"control_response","response":{"subtype":"error","request_id":"req-3","error":"nope"}}"#,
            &shared,
        );
        let got = rx.blocking_recv().unwrap();
        assert!(!got.success);
        assert_eq!(got.error, "nope");
    }

    #[test]
    fn unroutable_lines_are_dropped_without_disturbing_the_pending_map() {
        let (shared, rx) = pending_shared("req-4");
        for line in [
            &br#"{"type":"control_response","response":"not-an-object"}"#[..],
            &br#"{"type":"control_response","response":{"subtype":"success"}}"#[..],
            &br#"{"type":"control_response","response":{"request_id":"someone-else"}}"#[..],
            &br#"not json at all"#[..],
        ] {
            route_control_response(line, &shared);
        }
        assert_eq!(
            shared.pending.lock().unwrap().len(),
            1,
            "the waiting caller is still registered"
        );
        drop(shared);
        assert!(rx.blocking_recv().is_err());
    }

    fn init_request(opts: &Options) -> serde_json::Value {
        let (hooks, _) = build_hooks_for_initialize(&opts.hooks);
        initialize_msg("req-1", opts, hooks)
    }

    #[test]
    fn initialize_never_sends_sdk_mcp_servers() {
        // The CLI rejects any object form, and a rejection fails the whole
        // initialize — silently taking hooks, agents and the system prompt with
        // it. The key must simply not be there.
        let opts = Options::new()
            .with_mcp_server(
                "local-tools",
                crate::claude::McpHttpServer {
                    server_type: "http".into(),
                    url: "http://127.0.0.1:1".into(),
                    headers: Default::default(),
                },
            )
            .unwrap();
        let msg = init_request(&opts);
        assert!(msg["request"].get("sdkMcpServers").is_none());
    }

    #[test]
    fn initialize_always_carries_its_five_baseline_keys() {
        let msg = init_request(&Options::new());
        let request = &msg["request"];
        assert_eq!(request["subtype"], "initialize");
        for key in [
            "systemPrompt",
            "appendSystemPrompt",
            "hooks",
            "agents",
            "promptSuggestions",
        ] {
            assert!(request.get(key).is_some(), "{key} must always be present");
        }
        // Empty rather than absent — the CLI expects objects here.
        assert_eq!(request["hooks"], serde_json::json!({}));
        assert_eq!(request["agents"], serde_json::json!({}));
    }

    #[test]
    fn a_preset_replaces_the_plain_system_prompt() {
        let opts = Options::new()
            .with_system_prompt("ignored")
            .with_system_prompt_preset(SystemPromptPreset {
                preset_type: "preset".into(),
                preset: "claude_code".into(),
                append: "extra".into(),
            });
        let msg = init_request(&opts);
        assert_eq!(msg["request"]["systemPrompt"]["preset"], "claude_code");
        assert_eq!(msg["request"]["systemPrompt"]["append"], "extra");
    }

    #[test]
    fn the_envelope_carries_the_request_id_at_the_top_level() {
        let msg = init_request(&Options::new());
        assert_eq!(msg["type"], "control_request");
        assert_eq!(msg["request_id"], "req-1");
    }

    #[test]
    fn a_user_message_carries_the_nulls_the_cli_expects() {
        let msg = user_msg("hello");
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"message":{"content":"hello","role":"user"},"parent_tool_use_id":null,"session_id":"","type":"user"}"#
        );
    }

    #[test]
    fn the_environment_strips_the_four_variables_it_owns() {
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_ENTRYPOINT", "cli");

        let env = build_env(&Options::new());
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();

        assert!(
            !keys.contains(&"CLAUDECODE"),
            "an existing session must not block the spawn"
        );
        assert_eq!(
            env.iter()
                .filter(|(k, _)| k == "CLAUDE_CODE_ENTRYPOINT")
                .count(),
            1,
            "the inherited entrypoint is replaced, not duplicated"
        );

        std::env::remove_var("CLAUDECODE");
        std::env::remove_var("CLAUDE_CODE_ENTRYPOINT");
    }

    #[test]
    fn disabled_thinking_sets_the_token_budget_to_zero() {
        let env = build_env(&Options::new().with_thinking(thinking::DISABLED));
        assert_eq!(
            env.iter()
                .rev()
                .find(|(k, _)| k == "MAX_THINKING_TOKENS")
                .map(|(_, v)| v.as_str()),
            Some("0")
        );
    }

    #[test]
    fn caller_supplied_variables_win_over_the_inherited_ones() {
        std::env::set_var("AGENTO_SDK_ENV_PROBE", "inherited");
        let env = build_env(&Options::new().with_env([("AGENTO_SDK_ENV_PROBE", "override")]));
        let values: Vec<&str> = env
            .iter()
            .filter(|(k, _)| k == "AGENTO_SDK_ENV_PROBE")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(values, vec!["override"], "the inherited value is stripped");
        std::env::remove_var("AGENTO_SDK_ENV_PROBE");
    }

    #[test]
    fn a_working_directory_replaces_pwd() {
        std::env::set_var("PWD", "/somewhere/else");
        let env = build_env(&Options::new().with_cwd("/project"));
        let pwd: Vec<&str> = env
            .iter()
            .filter(|(k, _)| k == "PWD")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(pwd, vec!["/project"]);
        std::env::remove_var("PWD");
    }

    #[test]
    fn bypass_is_the_default_permission_posture() {
        assert_eq!(
            Options::new().permission_mode,
            permission_mode::BYPASS_PERMISSIONS
        );
    }
}
