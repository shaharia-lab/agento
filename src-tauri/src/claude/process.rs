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

use std::collections::{BTreeSet, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use serde::Serialize;
use serde_json::value::RawValue;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

use super::client::{Stream, StreamControl};
use super::errors::{Error, Result};
use super::hooks::{build_hooks_for_initialize, HookRegistry};
use super::init_types::decode_initialize_response;
use super::messages::{error_event, message_type, parse_line, system_subtype, Event};
use super::options::{thinking, Options};
use super::permissions::{PermissionContext, PermissionResult, PermissionUpdate};
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

/// How long to wait for stderr to reach end of stream once the process has
/// exited, before reporting whatever was captured.
const STDERR_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a terminating process is given before it is killed outright.
pub(crate) const SIGKILL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

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
    ///
    /// Generic over `Serialize` rather than taking a `serde_json::Value`, so a
    /// caller that must not re-spell what it forwards can hand over a struct
    /// carrying a `RawValue`. Routing that through a `Value` first would sort
    /// its keys and respell its numbers — see `write_control_success_raw`.
    pub(crate) async fn write<T: serde::Serialize + ?Sized>(&self, value: &T) -> Result<()> {
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

    // The child leads its own process group (#594), so a signal sent to the
    // group from outside this future — an app-exit hook, a startup reaper
    // holding only the pid `job_history` recorded — reaches everything the CLI
    // started: its MCP servers, its Bash tool's children. Without it they share
    // Agento's group, and there is no signal that names the run's tree alone.
    //
    // The shutdown path below still signals the pid, not the group. One
    // termination path does go: a terminal's Ctrl-C and hangup reach its
    // foreground group, so an Agento started from a shell (`npm run app`) no
    // longer takes its runs down with it — the orphan the app-exit hook (#595,
    // [`terminate_all_runs`]) covers. On Windows the flag groups the tree for
    // console control events only; the exit hook kills it with `taskkill /T`.
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);

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

    // Registered before anything else can go wrong, and held by the reader task
    // until it has reaped the child — so the app-exit hook (#595) sees every
    // CLI that is still ours to signal, and never one whose pid has been freed.
    // Refused once the app is quitting: a timer that fires inside the exit
    // window must not start a run the hook has already finished looking for.
    let live = match pid.and_then(|pid| LIVE.register(pid)) {
        Some(live) => Some(live),
        None if LIVE.quitting() => {
            if let Some(pid) = pid {
                kill_group(pid);
            }
            return Err(Error::Other(format!("claude: not started: {APP_QUIT}")));
        }
        None => None,
    };

    // Before anything is read, so a crash straight after the spawn still leaves
    // behind a record naming the process. `id()` is `None` only once the child
    // has been reaped, which nothing has done yet.
    if let (Some(hook), Some(pid)) = (&opts.on_spawn, pid) {
        hook(crate::claude::options::Spawned {
            pid,
            started_at: std::time::SystemTime::now(),
        })
        .await;
    }

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
    //
    // The join handle is kept, not detached: `Child::wait` reaps the process but
    // does **not** wait for this task, so reading the buffer straight after it
    // races the last lines out of the pipe — and those lines are the entire
    // content of the error the user sees. (Go does not have the race: its
    // `cmd.Wait` waits for the goroutine copying into the buffer.)
    let stderr_buf = Arc::new(StdMutex::new(String::new()));
    let stderr_task = stderr.map(|stderr| {
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
        })
    });

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
        let stderr_task = stderr_task;
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
            // Reaped: the pid is free for the kernel to reuse, so it leaves the
            // set the app-exit hook signals from this instant.
            drop(live);

            // Drain stderr before reading the buffer, so the message is the
            // whole of what the CLI said rather than whatever had arrived by
            // now. Bounded, because the pipe stays open as long as any process
            // holds it — `claude` spawning a longer-lived grandchild would
            // otherwise wedge this task, and with it the caller's stream. Go
            // waits unbounded here and can hang for the same reason.
            if let Some(task) = stderr_task {
                let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, task).await;
            }

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
    /// One bounded, known divergence, and only for an **explicit** `null` — an
    /// absent key is dropped by both.
    ///
    /// Go's SDK holds this as a `json.RawMessage` (no `omitempty` here), so an
    /// inbound `"input": null` captures the four bytes and survives to Agento's
    /// own `permReq.Input` in `internal/api/chats.go`, *whose* `omitempty` tests
    /// byte length and therefore emits `"input":null` on the `permission_request`
    /// frame. A plain `Option` collapses it to `None` one layer earlier, so the
    /// key is dropped instead.
    ///
    /// Recorded rather than fixed because the CLI always sends an object here,
    /// and because `src/claude/` is the SDK port and does not depend on
    /// `native::gojson`'s `captured_raw` — which is otherwise exactly the helper
    /// for it.
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
    // Go builds this payload as a `map[string]any` too, so going through a
    // `Value` here loses nothing: both sort the keys. Only the caller that
    // forwards bytes it did not author needs the raw form below.
    let raw = payload.and_then(|p| serde_json::value::to_raw_value(&p).ok());
    write_control_success_raw(shared, request_id, raw.as_deref()).await;
}

/// The same, for a payload that is already JSON bytes and must stay them.
///
/// The two envelope structs exist so nothing on this path is rebuilt from a
/// `serde_json::Value`. Their **field order is the wire order**, and it is
/// spelled to match Go: `process.go` builds both envelopes as `map[string]any`,
/// and `encoding/json` sorts map keys — hence `response` before `type`, and
/// `request_id` before `response` before `subtype`. Reordering a field here
/// changes the bytes the CLI receives.
async fn write_control_success_raw(shared: &Shared, request_id: &str, payload: Option<&RawValue>) {
    #[derive(Serialize)]
    struct Body<'a> {
        request_id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        response: Option<&'a RawValue>,
        subtype: &'static str,
    }

    #[derive(Serialize)]
    struct Line<'a> {
        response: Body<'a>,
        #[serde(rename = "type")]
        kind: &'static str,
    }

    let _ = shared
        .write(&Line {
            response: Body {
                request_id,
                response: payload,
                subtype: "success",
            },
            kind: "control_response",
        })
        .await;
}

/// `omitempty` for a bool: Go drops `false`, and only sets `interrupt` at all
/// when it is true.
fn is_false(b: &bool) -> bool {
    !*b
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

            // Both arms are serialised to bytes here rather than to a
            // `serde_json::Value`, because the allow arm carries the tool input
            // the CLI is about to *execute*: a `Value` is a `BTreeMap` without
            // `preserve_order`, so a round trip sorts the keys, respells the
            // numbers (`1.50` → `1.5`) and truncates a decimal beyond f64. Go
            // echoes `envelope.Request.Input` — a `json.RawMessage` — verbatim.
            //
            // Field order is the wire order and matches Go's sorted
            // `map[string]any`: behavior < updatedInput < updatedPermissions,
            // and behavior < interrupt < message.
            let payload = match &result {
                PermissionResult::Allow {
                    updated_input,
                    updated_permissions,
                } => {
                    #[derive(Serialize)]
                    struct AllowResponse<'a> {
                        behavior: &'static str,
                        /// Always sent, `null` included: the CLI expects the
                        /// input it should actually run, and Go marshals a nil
                        /// `json.RawMessage` as `null` rather than dropping it.
                        #[serde(rename = "updatedInput")]
                        updated_input: Option<&'a RawValue>,
                        #[serde(
                            rename = "updatedPermissions",
                            skip_serializing_if = "<[_]>::is_empty"
                        )]
                        updated_permissions: &'a [PermissionUpdate],
                    }

                    serde_json::value::to_raw_value(&AllowResponse {
                        behavior: "allow",
                        // When the handler does not rewrite it, echo the
                        // original back — the same bytes, not the same value.
                        updated_input: updated_input
                            .as_deref()
                            .or(envelope.request.input.as_deref()),
                        updated_permissions,
                    })
                }
                PermissionResult::Deny { message, interrupt } => {
                    #[derive(Serialize)]
                    struct DenyResponse<'a> {
                        behavior: &'static str,
                        #[serde(skip_serializing_if = "is_false")]
                        interrupt: bool,
                        message: &'a str,
                    }

                    serde_json::value::to_raw_value(&DenyResponse {
                        behavior: "deny",
                        interrupt: *interrupt,
                        message,
                    })
                }
            };

            // A failed encode is not a reason to leave the CLI waiting: every
            // inbound control_request must be answered, and an unanswered one
            // hangs it with no error on either side.
            match payload {
                Ok(payload) => {
                    write_control_success_raw(shared, request_id, Some(&payload)).await;
                }
                Err(e) => {
                    write_control_error(
                        shared,
                        request_id,
                        &format!("encoding the permission response: {e}"),
                    )
                    .await;
                }
            }
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

/// `CREATE_NEW_PROCESS_GROUP` from `winbase.h`, spelled out rather than taking
/// a Windows API crate for one constant.
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

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

// ─── Live processes, and the app-exit hook ───────────────────────────────────

/// The reason a run ends with when the app quits under it (#595). Written onto
/// the run's `job_history` row by the executor.
pub(crate) const APP_QUIT: &str = "terminated: app quit";

/// Every CLI this process has spawned and not yet reaped.
///
/// A GUI quit does not run destructors — `kill_on_drop` and `Stream::drop`
/// never fire — so without this set nothing at exit knows which processes are
/// still running on the app's behalf, and each becomes an orphan that goes on
/// working with nobody reading its output (#593).
static LIVE: LiveProcesses = LiveProcesses::new();

/// How often [`LiveProcesses::terminate_all`] looks for the groups it signalled.
const LIVENESS_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// A set of spawned, un-reaped CLI pids. Each pid also names the CLI's process
/// group, because the spawn makes the child a group leader (#594).
///
/// An instance rather than bare statics so the tests can own one: the
/// process-wide one is [`LIVE`], and setting *its* `quitting` inside a test
/// binary would make every concurrently running spawn refuse to start.
pub(crate) struct LiveProcesses {
    state: StdMutex<LiveState>,
}

struct LiveState {
    /// Set once, by [`LiveProcesses::terminate_all`]; never cleared.
    quitting: bool,
    pids: BTreeSet<u32>,
}

/// A pid's membership of a [`LiveProcesses`], ended by dropping it — which the
/// reader task does straight after `wait()` has reaped the child.
pub(crate) struct LiveEntry {
    owner: &'static LiveProcesses,
    pid: u32,
}

impl Drop for LiveEntry {
    fn drop(&mut self) {
        self.owner.lock().pids.remove(&self.pid);
    }
}

/// What [`LiveProcesses::terminate_all`] did, for the exit log line.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Terminated {
    /// Groups sent SIGTERM (on Windows: killed, there being no gentler step).
    pub signalled: usize,
    /// Groups still alive at the end of the grace period, and killed outright.
    pub killed: usize,
}

impl LiveProcesses {
    pub(crate) const fn new() -> Self {
        Self {
            state: StdMutex::new(LiveState {
                quitting: false,
                pids: BTreeSet::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LiveState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a spawned child. `None` once [`Self::terminate_all`] has begun:
    /// the check and the insert share one lock, so a child is either in the
    /// snapshot the exit hook signals or refused here, never neither.
    pub(crate) fn register(&'static self, pid: u32) -> Option<LiveEntry> {
        let mut state = self.lock();
        if state.quitting {
            return None;
        }
        state.pids.insert(pid);
        Some(LiveEntry { owner: self, pid })
    }

    pub(crate) fn quitting(&self) -> bool {
        self.lock().quitting
    }

    /// Stops every registered CLI's process tree, in at most `grace` plus one
    /// poll: SIGTERM to each group, then SIGKILL to any group still alive when
    /// the grace runs out. The same two steps, and the same grace, as a turn's
    /// own shutdown path — applied to the group rather than the pid, because at
    /// exit there is no later chance to reach the CLI's MCP servers and tool
    /// children.
    ///
    /// Marks the set quitting first, so nothing registers behind the snapshot.
    ///
    /// **A group is only signalled while it can still be ours.** Its pid stays
    /// registered until the leader is reaped, so no other process can hold it;
    /// once the leader is gone the group may live on in grandchildren, and a
    /// pgid is never handed out while any member remains. A group that has
    /// emptied is dropped from the loop and never signalled again.
    pub(crate) async fn terminate_all(&self, grace: std::time::Duration) -> Terminated {
        let mut alive: Vec<u32> = {
            let mut state = self.lock();
            state.quitting = true;
            state.pids.iter().copied().collect()
        };
        let signalled = alive.len();
        if signalled == 0 {
            return Terminated::default();
        }

        for &pid in &alive {
            terminate_group(pid);
        }
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            // Narrowed every pass, never re-derived from the snapshot: a group
            // seen empty once may have had its pid reused since, and probing
            // it again would find — and at the deadline kill — a stranger.
            alive.retain(|&pid| self.group_alive(pid));
            if alive.is_empty() {
                return Terminated {
                    signalled,
                    killed: 0,
                };
            }
            if tokio::time::Instant::now() >= deadline {
                for &pid in &alive {
                    kill_group(pid);
                }
                return Terminated {
                    signalled,
                    killed: alive.len(),
                };
            }
            tokio::time::sleep(LIVENESS_POLL).await;
        }
    }

    /// Whether any member of `pid`'s group — the un-reaped leader included —
    /// still exists. `EPERM` means it exists and is not ours to signal, which
    /// cannot happen for a group we created but is "alive" either way.
    #[cfg(unix)]
    fn group_alive(&self, pid: u32) -> bool {
        group_exists(pid)
    }

    /// Windows has no group to probe; the leader being un-reaped is the answer.
    #[cfg(not(unix))]
    fn group_alive(&self, pid: u32) -> bool {
        self.lock().pids.contains(&pid)
    }
}

/// Whether the app is quitting, as far as the agent runs are concerned.
pub(crate) fn app_quitting() -> bool {
    LIVE.quitting()
}

/// Whether `pid` is a CLI this process spawned and has not yet reaped — a run
/// of *this* session, which the startup reaper (#596) must never touch.
pub(crate) fn is_live_run(pid: u32) -> bool {
    LIVE.lock().pids.contains(&pid)
}

/// Stops every in-flight CLI run of this process — scheduled, manual, trigger
/// and chat alike, since all of them spawn through [`spawn_and_stream`].
/// Bounded by [`SIGKILL_GRACE`]. Called by the app-exit hook in `lib.rs`.
pub(crate) async fn terminate_all_runs() -> Terminated {
    LIVE.terminate_all(SIGKILL_GRACE).await
}

/// `pid` as a process-group id for `kill(-pgid, …)`, refusing the two values
/// whose negation is not a group: `kill(0, …)` signals Agento's own group and
/// `kill(-1, …)` every process the user owns.
#[cfg(unix)]
fn group_id(pid: u32) -> Option<libc::pid_t> {
    let pgid = libc::pid_t::try_from(pid).ok()?;
    (pgid > 1).then_some(pgid)
}

/// Whether any member of the group `pid` leads still exists. `EPERM` means it
/// exists and is not ours to signal, which is "alive" either way.
#[cfg(unix)]
fn group_exists(pid: u32) -> bool {
    let Some(pgid) = group_id(pid) else {
        return false;
    };
    // SAFETY: signal 0 checks existence and permission and sends nothing.
    let rc = unsafe { libc::kill(-pgid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn terminate_group(pid: u32) {
    if let Some(pgid) = group_id(pid) {
        // SAFETY: kill(2) on a group we created; ESRCH for an emptied group is
        // the outcome the caller already treats as done.
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    if let Some(pgid) = group_id(pid) {
        // SAFETY: as above.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

/// Windows has no SIGTERM, so both steps are the tree kill: `taskkill /T` walks
/// the parent-pid links from the CLI down, which is what a Job Object would
/// give and all this needs at exit.
#[cfg(windows)]
fn terminate_group(pid: u32) {
    kill_group(pid);
}

#[cfg(windows)]
fn kill_group(pid: u32) {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW` from `winbase.h`: no console flashes up on quit.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if let Err(e) = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
    {
        log::warn!("taskkill pid={pid}: {e}");
    }
}

// ─── Orphans of a previous session (#596) ────────────────────────────────────

/// The reason the startup reaper writes on a row whose CLI it found still
/// running and stopped.
pub(crate) const ORPHAN_RECOVERED: &str = "orphaned: recovered on startup";

/// The reason the startup reaper writes on a row whose CLI is gone — or was
/// never recorded, or whose pid now names some other process.
pub(crate) const ORPHAN_UNCLEAN_EXIT: &str = "orphaned: app did not exit cleanly";

/// How far a live process's start time may sit from the spawn time a
/// `job_history` row recorded and still be the process that row names.
///
/// The row's time is taken straight after `spawn()` returns, and `ps` reports
/// elapsed time in whole seconds, so a match lands within about one second; the
/// rest is headroom for a loaded machine. Windows reports the creation time to
/// 100ns and shares the constant: the headroom is for the machine, not the
/// probe. A pid reused by an unrelated process would have to have been started
/// within the same few seconds as the CLI it replaced, which a pid space that
/// wraps only after tens of thousands of spawns does not produce.
const ORPHAN_START_TOLERANCE: std::time::Duration = std::time::Duration::from_secs(5);

/// What [`stop_orphan`] found at a recorded pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Orphan {
    /// Nothing there that is provably the recorded CLI: it has exited, or its
    /// pid now names a process that started at another time. Nothing was
    /// signalled.
    NotOurs,
    /// The recorded CLI was still running, and its process group was stopped —
    /// by SIGTERM, or by SIGKILL once the grace ran out (`killed`).
    Stopped { killed: bool },
}

/// Stops the process group a previous session's run left behind, **only if**
/// `pid` is alive and started at `started_at` (within
/// [`ORPHAN_START_TOLERANCE`]).
///
/// The identity check is the whole point: the previous session is gone, so
/// nothing holds the pid, and the OS may have handed it to anything since. A
/// pid alone is not an identity. The same two steps as the app-exit hook —
/// SIGTERM to the group, SIGKILL to whatever is left after `grace` — and it
/// blocks for up to `grace`, so it belongs on a blocking thread.
///
/// A leader that has exited is `NotOurs` even if its group lives on in
/// grandchildren: with the leader gone there is no start time to check, and a
/// group id can be reused once its last member exits.
#[cfg(unix)]
pub(crate) fn stop_orphan(
    pid: u32,
    started_at: std::time::SystemTime,
    grace: std::time::Duration,
) -> Orphan {
    if group_id(pid).is_none() || !started_at_matches(pid, started_at) {
        return Orphan::NotOurs;
    }
    terminate_group(pid);
    let deadline = std::time::Instant::now() + grace;
    while group_exists(pid) {
        if std::time::Instant::now() >= deadline {
            kill_group(pid);
            return Orphan::Stopped { killed: true };
        }
        std::thread::sleep(LIVENESS_POLL);
    }
    Orphan::Stopped { killed: false }
}

/// Windows: the same identity check and the same two steps, by handle rather
/// than by group (#613). `GetProcessTimes` is the start-time probe, and the
/// handle it is read through is held until the end — an open handle keeps the
/// process object, and so its pid, from being reused, which closes the window
/// between the check and the `taskkill` that a pid alone would leave open.
///
/// Both steps are the tree kill (see [`terminate_group`]), so `killed` is only
/// `true` when the first `taskkill` did not finish the leader within `grace`.
/// As on Unix, a leader that has exited is `NotOurs`: with no process there is
/// no creation time to check.
#[cfg(windows)]
pub(crate) fn stop_orphan(
    pid: u32,
    started_at: std::time::SystemTime,
    grace: std::time::Duration,
) -> Orphan {
    let Some(process) = win32::Process::open(pid) else {
        return Orphan::NotOurs;
    };
    let matches = process.created_at().is_some_and(|live_start| {
        let gap = live_start
            .duration_since(started_at)
            .or_else(|_| started_at.duration_since(live_start))
            .unwrap_or_default();
        gap <= ORPHAN_START_TOLERANCE
    });
    if !matches || process.has_exited() {
        return Orphan::NotOurs;
    }
    terminate_group(pid);
    if process.wait(grace) {
        return Orphan::Stopped { killed: false };
    }
    kill_group(pid);
    Orphan::Stopped { killed: true }
}

/// The four `kernel32` calls the Windows arm of [`stop_orphan`] needs, spelled
/// out rather than taking a Windows API crate for them — the same choice as
/// [`CREATE_NEW_PROCESS_GROUP`]. `std` already links `kernel32`.
#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;
    use std::time::{Duration, SystemTime};

    type Handle = *mut c_void;

    /// `FILETIME`: 100ns intervals since 1601-01-01 UTC, split in two words.
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;
    /// `WaitForSingleObject`'s `INFINITE`; a finite wait stays below it.
    const INFINITE: u32 = u32::MAX;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn GetProcessTimes(
            process: Handle,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    /// An open process handle, closed on drop.
    pub(super) struct Process(Handle);

    impl Process {
        /// `None` when there is no such process or it is not ours to query —
        /// either way it cannot be proven to be the recorded CLI.
        pub(super) fn open(pid: u32) -> Option<Self> {
            // SAFETY: plain FFI call; a null return is the failure we check.
            let handle =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
            (!handle.is_null()).then_some(Self(handle))
        }

        /// When the process was created, or `None` if Windows will not say.
        pub(super) fn created_at(&self) -> Option<SystemTime> {
            let mut creation = FileTime::default();
            let (mut exit, mut kernel, mut user) = Default::default();
            // SAFETY: a live handle opened with the query right, and four
            // out-pointers to locals that outlive the call.
            let ok = unsafe {
                GetProcessTimes(self.0, &mut creation, &mut exit, &mut kernel, &mut user)
            };
            if ok == 0 {
                return None;
            }
            super::filetime_to_system_time(
                (u64::from(creation.high) << 32) | u64::from(creation.low),
            )
        }

        /// Whether the process has already exited — a handle outlives it.
        pub(super) fn has_exited(&self) -> bool {
            self.wait(Duration::ZERO)
        }

        /// Waits up to `timeout` for the process to exit; `true` if it did.
        pub(super) fn wait(&self, timeout: Duration) -> bool {
            let millis =
                u32::try_from(timeout.as_millis()).map_or(INFINITE - 1, |ms| ms.min(INFINITE - 1));
            // SAFETY: a live handle opened with `SYNCHRONIZE`.
            unsafe { WaitForSingleObject(self.0, millis) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            // SAFETY: the handle is ours and closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// A Windows `FILETIME` — 100ns intervals since 1601-01-01 UTC — as a
/// `SystemTime`. `None` for a time before the Unix epoch, which no process
/// Agento started can have.
#[cfg(any(windows, test))]
fn filetime_to_system_time(intervals: u64) -> Option<std::time::SystemTime> {
    /// Seconds from 1601-01-01 to 1970-01-01.
    const EPOCH_GAP_SECS: u64 = 11_644_473_600;
    let since_1601 = std::time::Duration::from_secs(intervals / 10_000_000)
        + std::time::Duration::from_nanos(intervals % 10_000_000 * 100);
    let since_1970 = since_1601.checked_sub(std::time::Duration::from_secs(EPOCH_GAP_SECS))?;
    std::time::UNIX_EPOCH.checked_add(since_1970)
}

/// Whether the live process `pid` started within [`ORPHAN_START_TOLERANCE`] of
/// `started_at`. `false` when it does not exist or `ps` cannot say.
///
/// `ps -o etime=` rather than `/proc/<pid>/stat`, because it is the one probe
/// Linux and macOS answer identically — so the Linux CI exercises the macOS
/// path too.
#[cfg(unix)]
fn started_at_matches(pid: u32, started_at: std::time::SystemTime) -> bool {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    // `ps -p` exits 1 when there is no such process.
    if !out.status.success() {
        return false;
    }
    let Some(elapsed) = parse_etime(&String::from_utf8_lossy(&out.stdout)) else {
        return false;
    };
    let Some(live_start) = std::time::SystemTime::now().checked_sub(elapsed) else {
        return false;
    };
    let gap = live_start
        .duration_since(started_at)
        .or_else(|_| started_at.duration_since(live_start))
        .unwrap_or_default();
    gap <= ORPHAN_START_TOLERANCE
}

/// `ps`'s `etime`: `[[dd-]hh:]mm:ss`, padded with spaces.
#[cfg(unix)]
fn parse_etime(text: &str) -> Option<std::time::Duration> {
    let text = text.trim();
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let fields = clock
        .split(':')
        .map(|f| f.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    let (hours, minutes, seconds) = match fields.as_slice() {
        [m, s] => (0, *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    Some(std::time::Duration::from_secs(
        ((days * 24 + hours) * 60 + minutes) * 60 + seconds,
    ))
}

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

    // ─── The app-exit hook (#595) ────────────────────────────────────────────

    /// Spawns `script` under `sh` as its own group leader, the way the CLI is
    /// spawned, registers it, and reaps it on a task the way the reader does —
    /// dropping the entry only once `wait()` returns. Returns once the script
    /// has printed its first line, so a trap it sets is in place before any
    /// test signals it.
    #[cfg(unix)]
    async fn spawn_registered(live: &'static LiveProcesses, script: &str) -> u32 {
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .expect("spawning sh");
        let pid = child.id().expect("a live child has a pid");
        let entry = live.register(pid).expect("not quitting yet");
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let ready = stdout.next_line().await.unwrap();
        assert_eq!(ready.as_deref(), Some("ready"));
        tokio::spawn(async move {
            let _ = child.wait().await;
            drop(entry);
            drop(stdout);
        });
        pid
    }

    #[cfg(unix)]
    async fn wait_until_gone(live: &LiveProcesses, pid: u32) {
        for _ in 0..100 {
            if !live.group_alive(pid) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("process group {pid} is still alive");
    }

    /// The ordinary quit: every group honours SIGTERM, so the whole tree — the
    /// leader *and* the grandchild it backgrounded — is gone well inside the
    /// grace, and nothing needs killing.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_all_stops_every_tree_that_honours_sigterm() {
        static LIVE: LiveProcesses = LiveProcesses::new();
        let a = spawn_registered(&LIVE, "sleep 60 & echo ready; wait").await;
        let b = spawn_registered(&LIVE, "sleep 60 & echo ready; wait").await;

        let started = std::time::Instant::now();
        let done = LIVE.terminate_all(std::time::Duration::from_secs(5)).await;

        assert_eq!(
            done,
            Terminated {
                signalled: 2,
                killed: 0
            }
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert!(!LIVE.group_alive(a) && !LIVE.group_alive(b));
    }

    /// The bound: a tree that ignores SIGTERM is killed when the grace runs out,
    /// so quitting waits for the grace and no longer.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_all_kills_a_tree_that_ignores_sigterm_after_the_grace() {
        static LIVE: LiveProcesses = LiveProcesses::new();
        let pid = spawn_registered(&LIVE, "trap '' TERM; sleep 60 & echo ready; wait").await;

        let grace = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let done = LIVE.terminate_all(grace).await;
        let took = started.elapsed();

        assert_eq!(
            done,
            Terminated {
                signalled: 1,
                killed: 1
            }
        );
        assert!(took >= grace, "killed before the grace ran out: {took:?}");
        assert!(
            took < std::time::Duration::from_secs(3),
            "unbounded: {took:?}"
        );
        wait_until_gone(&LIVE, pid).await;
    }

    /// A tree that honours SIGTERM beside one that does not: the first is gone
    /// long before the deadline and only the second is killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_all_kills_only_the_trees_still_alive_at_the_deadline() {
        static LIVE: LiveProcesses = LiveProcesses::new();
        let polite = spawn_registered(&LIVE, "sleep 60 & echo ready; wait").await;
        let stubborn = spawn_registered(&LIVE, "trap '' TERM; sleep 60 & echo ready; wait").await;

        let done = LIVE
            .terminate_all(std::time::Duration::from_millis(500))
            .await;

        assert_eq!(
            done,
            Terminated {
                signalled: 2,
                killed: 1
            }
        );
        assert!(!LIVE.group_alive(polite));
        wait_until_gone(&LIVE, stubborn).await;
    }

    /// A child spawned after the hook took its snapshot would outlive it, so
    /// the set refuses to take one.
    #[cfg(unix)]
    #[tokio::test]
    async fn nothing_registers_once_terminate_all_has_begun() {
        static LIVE: LiveProcesses = LiveProcesses::new();
        assert!(!LIVE.quitting());

        let done = LIVE.terminate_all(std::time::Duration::from_secs(5)).await;

        assert_eq!(done, Terminated::default());
        assert!(LIVE.quitting());
        assert!(LIVE.register(std::process::id()).is_none());
    }

    /// `kill(0, …)` is Agento's own group and `kill(-1, …)` is every process the
    /// user owns; neither is ever a CLI's group.
    #[cfg(unix)]
    #[test]
    fn group_id_refuses_the_pids_whose_negation_is_not_a_group() {
        assert_eq!(group_id(0), None);
        assert_eq!(group_id(1), None);
        assert_eq!(group_id(u32::MAX), None);
        assert_eq!(group_id(4242), Some(4242));
    }

    #[cfg(unix)]
    #[test]
    fn etime_parses_every_width_ps_prints() {
        let secs = |s: u64| Some(std::time::Duration::from_secs(s));
        assert_eq!(parse_etime("      00:07\n"), secs(7));
        assert_eq!(parse_etime("12:34"), secs(12 * 60 + 34));
        assert_eq!(parse_etime("01:02:03"), secs(3600 + 2 * 60 + 3));
        assert_eq!(
            parse_etime("2-01:02:03"),
            secs(2 * 86_400 + 3600 + 2 * 60 + 3)
        );
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("7"), None);
        assert_eq!(parse_etime("a:b"), None);
    }

    /// A live pid whose start time is not the recorded one is some other
    /// process that inherited the number, and must not be signalled.
    #[cfg(unix)]
    #[test]
    fn an_orphan_is_stopped_only_when_its_start_time_matches() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let spawned = std::time::SystemTime::now();

        let a_day_before = spawned - std::time::Duration::from_secs(86_400);
        let grace = std::time::Duration::from_secs(5);
        assert_eq!(stop_orphan(pid, a_day_before, grace), Orphan::NotOurs);
        assert!(child.try_wait().expect("try_wait").is_none(), "left alone");

        // Reaped concurrently, as init reaps a real orphan, so the group empties.
        let waiter = std::thread::spawn(move || child.wait());
        assert_eq!(
            stop_orphan(pid, spawned, grace),
            Orphan::Stopped { killed: false }
        );
        let status = waiter.join().expect("join").expect("wait");
        assert!(!status.success(), "stopped by the signal: {status:?}");
        assert!(!group_exists(pid));
    }

    /// The Windows probe reads a `FILETIME`; its epoch is 1601, not 1970.
    #[test]
    fn a_filetime_converts_to_system_time() {
        const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
        assert_eq!(
            filetime_to_system_time(UNIX_EPOCH_AS_FILETIME),
            Some(std::time::UNIX_EPOCH)
        );
        assert_eq!(
            filetime_to_system_time(UNIX_EPOCH_AS_FILETIME + 15_000_001),
            Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(1_500_000_100))
        );
        assert_eq!(filetime_to_system_time(UNIX_EPOCH_AS_FILETIME - 1), None);
        // Past what Windows' own `SystemTime` can hold (it is a `FILETIME`
        // there), so `None` on Windows and `Some` elsewhere — but never a panic.
        let _ = filetime_to_system_time(u64::MAX);
    }

    /// The Windows arm of `an_orphan_is_stopped_only_when_its_start_time_matches`:
    /// `GetProcessTimes` is the identity and `taskkill /T /F` the stop. Runs in
    /// `ci.yml`'s `windows_rules`, the only place it can.
    #[cfg(windows)]
    #[test]
    fn a_windows_orphan_is_stopped_only_when_its_start_time_matches() {
        use std::os::windows::process::CommandExt;
        let mut child = std::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .expect("spawn ping");
        let pid = child.id();
        let spawned = std::time::SystemTime::now();

        let a_day_before = spawned - std::time::Duration::from_secs(86_400);
        let grace = std::time::Duration::from_secs(5);
        assert_eq!(stop_orphan(pid, a_day_before, grace), Orphan::NotOurs);
        assert!(child.try_wait().expect("try_wait").is_none(), "left alone");

        assert_eq!(
            stop_orphan(pid, spawned, grace),
            Orphan::Stopped { killed: false }
        );
        let status = child.wait().expect("wait");
        assert!(!status.success(), "stopped by taskkill: {status:?}");
        assert_eq!(stop_orphan(pid, spawned, grace), Orphan::NotOurs, "gone");
    }
}
