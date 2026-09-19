//! The Credentials Checker's background worker (#603, design doc §4.3, §5.1,
//! §6): the loop that keeps `credential_scan_state` and `credential_findings`
//! in step with the session corpus.
//!
//! It is `insights::worker`'s shape with a different payload — a boot sweep,
//! then `recv_timeout` over a bounded queue that `scan.rs` announces changed
//! sessions on, falling out every [`RESCAN_INTERVAL`] into a sweep — and it is
//! a `std::thread` for that module's reason: everything it does is blocking
//! rusqlite plus transcript reads, so it is kept off the tokio runtime entirely
//! rather than parking a runtime worker. No `db::blocking`, no `async`.
//! `tests/security_scan_worker.rs` drives [`sync`] itself.
//!
//! ## Unlike the insights worker, it can be stopped
//!
//! The checker is off by default and the user can flip it at any time, and a
//! switched-off checker must read nothing (§6). So there is no `OnceLock`
//! queue: [`WORKER`] is a `Mutex<Option<..>>` holding the running worker's
//! sender and its shared flags. [`sync`] fills it and spawns the thread when the
//! stored switch is on (a no-op while one is running); when it is off, it
//! empties it, sets the worker's `stopped` flag and drops the sender. The loop
//! checks the flag before every
//! pass, after every `recv`, and between the batches of a sweep, so a stop
//! costs at most the batch already being written — and dropping the last
//! sender wakes a `recv_timeout` that would otherwise sleep for five minutes.
//!
//! **A stop is not a join.** [`sync`] is called from the settings `PUT`, which
//! must not wait for a batch to finish, so a `stop` then `start` can briefly
//! overlap the old thread's last batch with the new thread's sweep. That is
//! safe because [`store::record_scan`] is one transaction per session and is
//! idempotent — two writers of the same session write the same rows.
//!
//! ## What is scanned
//!
//! [`session_text`]: every user and assistant message's text, the string leaves
//! of every tool call's `input`, and the textual parts of every tool result —
//! across the parent transcript and its sub-agents', decoded through
//! `insights::transcript`. Unlike `insights::index` nothing is capped and
//! nothing is dropped as "injected": a system reminder can carry a file's
//! contents, and a search index can afford to miss text where a leak detector
//! cannot. Image and other non-text blocks contribute nothing.
//!
//! ## What makes a session pending
//!
//! A sweep scans what `store::needs_scanning` answers: no scan state, or state
//! under an older ruleset. A ruleset bump is therefore noticed the way a
//! `CURRENT_PROCESSOR_VERSION` bump is, by the periodic sweep. A *changed*
//! session is made pending by `scan.rs` itself, which calls
//! `store::mark_changed` on everything it announces before [`enqueue`] — so a
//! change survives its announcement being dropped, by a full queue or by the
//! checker being off, and the next sweep of any kind picks it up.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;

use super::rules::CURRENT_RULESET_VERSION;
use super::scan::{self, Finding};
use super::store::{self, Pending};
use crate::native::db;
use crate::native::insights::{processors, transcript};

/// How often the worker sweeps for sessions nothing announced — a ruleset bump,
/// or an announcement the queue could not hold. `insights::worker`'s value, for
/// its reasons.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Sessions per sweep chunk and per queue drain. Each is written as it is read,
/// so this bounds how long a [`stop`] can take, not memory.
const BATCH_SIZE: usize = 100;

/// The queue's capacity; overflow becomes one sweep (see [`Shared`]).
const QUEUE_SIZE: usize = 100;

/// The flags a running worker shares with [`stop`] and [`enqueue`].
#[derive(Debug, Default)]
struct Shared {
    /// Set by [`stop`]; the loop exits at its next check.
    stopped: AtomicBool,
    /// Set when [`enqueue`] could not deliver something, and swapped back to
    /// `false` **before** the sweep that covers it runs, so an announcement
    /// arriving mid-sweep gets a pass of its own. `insights::worker`'s
    /// `SWEEP_REQUESTED`, per worker rather than per process.
    sweep_requested: AtomicBool,
}

impl Shared {
    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

/// The running worker, as [`stop`] and [`enqueue`] reach it.
struct Running {
    tx: SyncSender<Pending>,
    shared: Arc<Shared>,
}

/// `None` while the checker is off. See the module header.
static WORKER: Mutex<Option<Running>> = Mutex::new(None);

fn worker() -> MutexGuard<'static, Option<Running>> {
    WORKER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start or stop the worker to match the **stored**
/// `credentials_checker_enabled` — at boot, and after every settings save (a
/// no-op unless the stored switch changed).
///
/// The setting is read while [`WORKER`] is held, so two saves racing each
/// other (on, then off) cannot apply their start and stop in the wrong order
/// and leave a worker running under a stored "off": whichever syncs last acts
/// on the last value saved. Unreadable means off — an unreadable setting is not
/// a reason to start scanning a corpus the user may have kept the checker away
/// from.
pub fn sync(db_path: PathBuf) {
    let mut slot = worker();
    let enabled = match db::open_read_only(&db_path) {
        Ok(conn) => crate::native::settings::load_stored(&conn).credentials_checker_enabled,
        Err(e) => {
            log::warn!("credentials checker: cannot read the setting, treating it as off: {e}");
            false
        }
    };
    if enabled {
        start_locked(&mut slot, db_path);
    } else {
        stop_locked(&mut slot);
    }
}

/// Start the worker — a boot sweep, then the queue. A no-op while one runs.
fn start_locked(slot: &mut Option<Running>, db_path: PathBuf) {
    if slot.is_some() {
        return;
    }
    let (tx, rx) = mpsc::sync_channel::<Pending>(QUEUE_SIZE);
    let shared = Arc::new(Shared::default());
    let thread_shared = Arc::clone(&shared);
    match std::thread::Builder::new()
        .name("credentials-checker".into())
        .spawn(move || run(&db_path, &rx, &thread_shared))
    {
        Ok(_) => {
            log::info!("credentials checker: worker started");
            *slot = Some(Running { tx, shared });
        }
        Err(e) => log::warn!("credentials checker: cannot spawn the worker: {e}"),
    }
}

/// Stop the worker. It finishes at most the batch it is writing; a no-op when
/// none runs.
pub fn stop() {
    stop_locked(&mut worker());
}

fn stop_locked(slot: &mut Option<Running>) {
    let Some(running) = slot.take() else {
        return;
    };
    running.shared.stopped.store(true, Ordering::Release);
    // The last long-lived sender: dropping it wakes a waiting `recv_timeout`.
    drop(running.tx);
    log::info!("credentials checker: worker stopped");
}

/// Whether a worker is running.
pub fn is_running() -> bool {
    worker().is_some()
}

/// Hand sessions to the worker. Never blocks, and is a no-op while it is
/// stopped — the sweep a started worker runs first covers anything dropped then.
pub fn enqueue(items: impl IntoIterator<Item = Pending>) {
    // Cloned out so the lock is not held across the sends.
    let Some((tx, shared)) = worker()
        .as_ref()
        .map(|r| (r.tx.clone(), Arc::clone(&r.shared)))
    else {
        return;
    };
    let overflowed = offer(&tx, items);
    if overflowed > 0 {
        shared.sweep_requested.store(true, Ordering::Release);
        log::debug!(
            "credentials checker: {overflowed} announcements overflowed the queue, \
             requesting a sweep"
        );
    }
}

/// Offer every item without blocking, answering how many did not fit.
fn offer(tx: &SyncSender<Pending>, items: impl IntoIterator<Item = Pending>) -> usize {
    items
        .into_iter()
        .filter(|item| tx.try_send(item.clone()).is_err())
        .count()
}

/// The worker loop: sweep, then drain the queue until stopped.
fn run(db_path: &Path, rx: &Receiver<Pending>, shared: &Shared) {
    sweep(db_path, shared);
    while run_once(db_path, rx, shared, RESCAN_INTERVAL) != Pass::Stopped {}
    log::debug!("credentials checker: worker exited");
}

/// What one pass of [`run`] did — for the tests in this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// A batch came off the queue and was written.
    Batch { size: usize, committed: bool },
    /// `recv_timeout` expired into the periodic sweep.
    Swept,
    /// [`stop`] was called, or every sender is gone; [`run`] returns.
    Stopped,
}

/// One pass: a batch off the queue (or a timeout into a sweep), then a
/// requested sweep. `timeout` is [`RESCAN_INTERVAL`] outside the tests.
fn run_once(db_path: &Path, rx: &Receiver<Pending>, shared: &Shared, timeout: Duration) -> Pass {
    if shared.stopped() {
        return Pass::Stopped;
    }
    let mut batch: BTreeSet<Pending> = BTreeSet::new();
    match rx.recv_timeout(timeout) {
        Ok(item) => {
            batch.insert(item);
            while batch.len() < BATCH_SIZE {
                match rx.try_recv() {
                    Ok(item) => {
                        batch.insert(item);
                    }
                    Err(_) => break,
                }
            }
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            if shared.stopped() {
                return Pass::Stopped;
            }
            sweep(db_path, shared);
            return Pass::Swept;
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => return Pass::Stopped,
    }
    // A stopped worker's queue can still hold items; they are not processed.
    if shared.stopped() {
        return Pass::Stopped;
    }

    let size = batch.len();
    let committed = process_batch(db_path, batch, shared);
    if shared.sweep_requested.swap(false, Ordering::AcqRel) {
        sweep(db_path, shared);
    }
    Pass::Batch { size, committed }
}

/// Every session with no scan state, or one scanned under an older ruleset,
/// processed directly in [`BATCH_SIZE`] chunks — never pushed through the
/// queue, which a first sweep would overflow many times over.
fn sweep(db_path: &Path, shared: &Shared) {
    let pending = match db::open_read_only(db_path)
        .and_then(|conn| store::needs_scanning(&conn, CURRENT_RULESET_VERSION))
    {
        Ok(pending) => pending,
        Err(e) => {
            log::warn!("credentials checker: cannot list sessions to scan: {e}");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    log::info!("credentials checker: scanning {} sessions", pending.len());
    for chunk in pending.chunks(BATCH_SIZE) {
        if shared.stopped() {
            return;
        }
        process_batch(db_path, chunk.iter().cloned().collect(), shared);
    }
}

/// One session's scan, ready to record.
struct Scanned {
    item: Pending,
    /// The scanned text when there are findings, and empty when there are
    /// none: `record_scan` only slices the text at a finding's range, so a
    /// clean session need not keep its transcript in memory until the write.
    text: String,
    findings: Vec<Finding>,
}

/// Read and scan the batch on a bounded reader pool, recording each session as
/// it arrives.
///
/// Readers hand results to this thread over a channel as small as the pool, so
/// at most about twice the pool's worth of transcripts is in memory at once
/// however large a batch is — the scanned text is uncapped, unlike a search
/// document. Writes stay on this
/// one thread, one `record_scan` transaction per session.
///
/// Answers `false` only for a *database* failure. An unreadable transcript is
/// skipped and stays pending for the next sweep, which is not a failure.
fn process_batch(db_path: &Path, batch: BTreeSet<Pending>, shared: &Shared) -> bool {
    // A cache row with no file path has no transcript to read.
    let items: Vec<Pending> = batch
        .into_iter()
        .filter(|item| !item.file_path.is_empty())
        .collect();
    if items.is_empty() {
        return true;
    }
    let mut conn = match db::open_read_write(db_path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("credentials checker: cannot open the database to store a batch: {e}");
            return false;
        }
    };

    let readers = crate::native::scanner::apply::scan_readers().min(items.len());
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::sync_channel::<Scanned>(readers);
    let mut committed = true;
    std::thread::scope(|scope| {
        for _ in 0..readers {
            let (next, items, tx) = (&next, &items, tx.clone());
            scope.spawn(move || loop {
                if shared.stopped() {
                    return;
                }
                let Some(item) = items.get(next.fetch_add(1, Ordering::Relaxed)) else {
                    return;
                };
                let files = processors::session_files(&item.session_id, Path::new(&item.file_path));
                match session_text(&files) {
                    Ok(text) => {
                        let findings = scan::scan(&text);
                        let text = if findings.is_empty() {
                            String::new()
                        } else {
                            text
                        };
                        let scanned = Scanned {
                            item: item.clone(),
                            text,
                            findings,
                        };
                        if tx.send(scanned).is_err() {
                            return;
                        }
                    }
                    // Named by its file path, never its session id — see the
                    // note on the store failure below.
                    Err(e) => log::warn!("credentials checker: skipping {}: {e}", item.file_path),
                }
            });
        }
        // Only the readers' clones remain, so the loop ends when they do.
        drop(tx);
        for s in rx {
            if store::record_scan(
                &mut conn,
                &s.item.session_id,
                &s.item.project_path,
                CURRENT_RULESET_VERSION,
                &s.text,
                &s.findings,
            )
            .is_err()
            {
                // Left pending: no scan-state row moved, so the next sweep
                // retries it. Named by its file path, and `record_scan`'s error
                // is not logged: its text carries the session id, which
                // CodeQL's cleartext-logging rule treats as sensitive in this
                // module (PR #619), and the path already identifies the session.
                log::warn!(
                    "credentials checker: failed to store the scan of {}, leaving it pending",
                    s.item.file_path
                );
                committed = false;
            }
        }
    });
    committed
}

/// The text one session's findings are computed from: its parent transcript,
/// then each sub-agent's, event by event.
///
/// A parent that cannot be read fails the session (it stays pending); an
/// unreadable sub-agent transcript is skipped, as `processors::run` does.
fn session_text(files: &[PathBuf]) -> Result<String, String> {
    let Some((parent, subagents)) = files.split_first() else {
        return Err("no session files".to_string());
    };
    let mut text = TextAccumulator::default();
    for ev in transcript::read(parent)? {
        text.observe(&ev);
    }
    for file in subagents {
        match transcript::read(file) {
            Ok(events) => events.iter().for_each(|ev| text.observe(ev)),
            Err(e) => log::warn!("credentials checker: skipping a sub-agent transcript: {e}"),
        }
    }
    Ok(text.text)
}

/// One session's scanned text, newline separated piece by piece so a match can
/// never be stitched together across two pieces.
#[derive(Debug, Default)]
struct TextAccumulator {
    text: String,
}

impl TextAccumulator {
    fn push(&mut self, piece: &str) {
        if piece.is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(piece);
    }

    fn observe(&mut self, ev: &transcript::Event) {
        let Some(message) = ev.message.as_ref() else {
            return;
        };
        match ev.event_type.as_str() {
            "user" => {
                // The message's own text blocks (or string), then every tool
                // result it carries: a user event is how a tool's output
                // reaches the transcript at all.
                self.push(&transcript::extract_text_content(&message.content));
                for payload in tool_result_payloads(&message.content) {
                    self.push(&transcript::extract_text_content(payload));
                }
            }
            "assistant" => {
                for block in transcript::parse_content_blocks(&message.content) {
                    match block.block_type.as_str() {
                        "text" => self.push(&block.text),
                        "thinking" => self.push(&block.thinking),
                        "tool_use" => self.push_tool_input(&block),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// The string leaves of a tool call's input — a `Bash` command, a `Write`'s
    /// file content, an MCP call's arguments. Keys are schema, not content.
    fn push_tool_input(&mut self, block: &transcript::ContentBlock) {
        let Some(raw) = block.input.as_ref() else {
            return;
        };
        if let Ok(value) = serde_json::from_str::<Value>(raw.get()) {
            self.push_string_leaves(&value);
        }
    }

    fn push_string_leaves(&mut self, value: &Value) {
        match value {
            Value::String(s) => self.push(s),
            Value::Array(items) => items.iter().for_each(|v| self.push_string_leaves(v)),
            Value::Object(fields) => fields.values().for_each(|v| self.push_string_leaves(v)),
            _ => {}
        }
    }
}

/// Each `tool_result` block's payload, borrowed out of the content array. Read
/// by hand rather than through `parse_content_blocks`, whose all-or-nothing
/// decode would let one malformed block hide every result beside it.
fn tool_result_payloads(content: &Value) -> impl Iterator<Item = &Value> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| block.get("content").unwrap_or(&Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A GitHub token built at runtime, so no literal secret sits in the source
    /// for a push-protection scanner to refuse.
    fn token(fill: char) -> String {
        format!("gh{}{}", "p_", fill.to_string().repeat(36))
    }

    fn event(event_type: &str, content: Value) -> transcript::Event {
        transcript::Event {
            event_type: event_type.to_string(),
            message: Some(transcript::Message {
                role: event_type.to_string(),
                content,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn text_of(events: &[transcript::Event]) -> String {
        let mut acc = TextAccumulator::default();
        events.iter().for_each(|ev| acc.observe(ev));
        acc.text
    }

    /// Everything §4.3 names reaches the scanned text: a typed message, an
    /// assistant reply, a `Bash` call's command and its output, and a
    /// `Write`'s file content.
    #[test]
    fn messages_tool_inputs_and_tool_outputs_are_all_scanned() {
        let text = text_of(&[
            event("user", json!("deploy with PLAIN_USER")),
            event(
                "assistant",
                json!([
                    {"type": "text", "text": "PLAIN_REPLY"},
                    {"type": "tool_use", "id": "t1", "name": "Bash",
                     "input": {"command": "echo BASH_INPUT", "timeout": 5}},
                    {"type": "tool_use", "id": "t2", "name": "Write",
                     "input": {"file_path": "/x/.env", "content": "WRITE_CONTENT"}},
                ]),
            ),
            event(
                "user",
                json!([
                    {"type": "tool_result", "tool_use_id": "t1", "content": "BASH_STDOUT"},
                    {"type": "tool_result", "tool_use_id": "t2",
                     "content": [{"type": "text", "text": "READ_BODY"}]},
                ]),
            ),
        ]);

        for piece in [
            "PLAIN_USER",
            "PLAIN_REPLY",
            "BASH_INPUT",
            "/x/.env",
            "WRITE_CONTENT",
            "BASH_STDOUT",
            "READ_BODY",
        ] {
            assert!(text.contains(piece), "{piece} missing from {text:?}");
        }
        assert!(!text.contains("command"), "argument names are not content");
        assert!(!text.contains("Bash"), "a tool's name is not content");
    }

    /// A non-text tool output — an image block — contributes nothing, and a
    /// result carrying only that adds no text at all.
    #[test]
    fn a_non_text_tool_output_contributes_nothing() {
        let text = text_of(&[event(
            "user",
            json!([{"type": "tool_result", "tool_use_id": "t1", "content": [
                {"type": "image", "source": {"type": "base64", "data": "QUJDRA=="}},
            ]}]),
        )]);
        assert_eq!(text, "");
    }

    /// A secret in injected content — a system reminder quoting a file — is
    /// still scanned, where the search index drops it.
    #[test]
    fn injected_user_content_is_scanned() {
        let secret = token('a');
        let text = text_of(&[event(
            "user",
            json!(format!("<system-reminder>{secret}</system-reminder>")),
        )]);
        assert!(text.contains(&secret));
    }

    /// Pieces are newline separated, so two adjacent pieces cannot combine
    /// into one match.
    #[test]
    fn pieces_do_not_run_together() {
        let text = text_of(&[
            event("user", json!("ghp_")),
            event("user", json!("a".repeat(36))),
        ]);
        assert!(scan::scan(&text).is_empty(), "{text:?}");
    }

    fn pending(session: &str) -> Pending {
        Pending {
            session_id: session.into(),
            project_path: "/a".into(),
            file_path: format!("/a/{session}.jsonl"),
        }
    }

    #[test]
    fn everything_past_the_queues_capacity_is_reported_as_overflow() {
        let (tx, _rx) = mpsc::sync_channel::<Pending>(2);
        assert_eq!(offer(&tx, (0..5).map(|i| pending(&format!("s{i}")))), 3);
    }

    /// `enqueue` with no worker running must not panic — it is called from
    /// every scan, with the checker usually off.
    #[test]
    fn enqueue_while_stopped_is_a_no_op() {
        enqueue([pending("s1")]);
    }

    // ─── the loop, against a real database ───────────────────────────────────

    fn fixture_db(dir: &tempfile::TempDir) -> PathBuf {
        let db_path = dir.path().join("agento.db");
        let mut conn = db::ensure_database(&db_path).expect("create");
        crate::native::migrate::apply(&mut conn).expect("migrations");
        db_path
    }

    /// A transcript whose tool output leaks `secret`, registered as a cache row.
    fn seed_session(
        dir: &tempfile::TempDir,
        db_path: &Path,
        session_id: &str,
        secret: &str,
    ) -> Pending {
        let file = dir.path().join(format!("{session_id}.jsonl"));
        let lines = [
            json!({"type": "user", "message": {"role": "user", "content": "print the env"}}),
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": format!("TOKEN={secret}")},
            ]}}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&file, body).expect("write transcript");
        let file_path = file.to_string_lossy().into_owned();

        let conn = db::open_read_write(db_path).expect("open");
        conn.execute(
            "INSERT INTO claude_session_cache
                 (session_id, project_path, file_path, file_mtime, start_time, last_activity)
             VALUES (?1, '/a', ?2, '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00',
                     '2026-01-01 00:00:00+00:00')",
            rusqlite::params![session_id, file_path],
        )
        .expect("cache row");
        Pending {
            session_id: session_id.into(),
            project_path: "/a".into(),
            file_path,
        }
    }

    fn findings(db_path: &Path, session_id: &str) -> Vec<(String, String)> {
        let conn = db::open_read_only(db_path).expect("open");
        let mut stmt = conn
            .prepare(
                "SELECT rule_id, masked_snippet FROM credential_findings
                  WHERE session_id = ?1 ORDER BY location_start",
            )
            .expect("prepare");
        stmt.query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    }

    fn scanned(db_path: &Path, session_id: &str) -> bool {
        let conn = db::open_read_only(db_path).expect("open");
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM credential_scan_state WHERE session_id = ?1)",
            [session_id],
            |row| row.get(0),
        )
        .expect("query")
    }

    /// A queued session is scanned in one pass: its finding is stored masked,
    /// and never as the raw value.
    #[test]
    fn a_queued_session_is_scanned_and_its_finding_stored_masked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let secret = token('b');
        let item = seed_session(&dir, &db_path, "s1", &secret);
        let (tx, rx) = mpsc::sync_channel(4);
        tx.send(item).expect("send");

        let pass = run_once(&db_path, &rx, &Shared::default(), Duration::from_millis(10));

        assert_eq!(
            pass,
            Pass::Batch {
                size: 1,
                committed: true
            }
        );
        let rows = findings(&db_path, "s1");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].0, "github-pat");
        assert!(!rows[0].1.contains(&secret));
    }

    /// A clean session still gets its scan-state row, so it is not pending on
    /// the next sweep.
    #[test]
    fn a_clean_session_is_recorded_as_scanned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let item = seed_session(&dir, &db_path, "s1", "nothing secret here");
        let (tx, rx) = mpsc::sync_channel(4);
        tx.send(item).expect("send");

        run_once(&db_path, &rx, &Shared::default(), Duration::from_millis(10));

        assert!(scanned(&db_path, "s1"));
        assert!(findings(&db_path, "s1").is_empty());
    }

    /// The timeout arm is the periodic sweep, which finds what nothing
    /// announced — the path a ruleset bump is picked up on.
    #[test]
    fn a_timeout_sweeps_the_unannounced_corpus() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        seed_session(&dir, &db_path, "s1", &token('c'));
        let (_tx, rx) = mpsc::sync_channel::<Pending>(4);

        let pass = run_once(&db_path, &rx, &Shared::default(), Duration::from_millis(10));

        assert_eq!(pass, Pass::Swept);
        assert_eq!(findings(&db_path, "s1").len(), 1);
    }

    /// A scan-state row under an older ruleset is pending again, so the sweep
    /// rescans it.
    #[test]
    fn an_older_ruleset_is_rescanned_by_the_sweep() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        seed_session(&dir, &db_path, "s1", &token('d'));
        db::open_read_write(&db_path)
            .expect("open")
            .execute(
                "INSERT INTO credential_scan_state (session_id, project_path, ruleset_version, scanned_at)
                 VALUES ('s1', '/a', ?1, '2026-01-01 00:00:00+00:00')",
                [CURRENT_RULESET_VERSION - 1],
            )
            .expect("old state");

        sweep(&db_path, &Shared::default());

        assert_eq!(findings(&db_path, "s1").len(), 1);
    }

    /// A session already scanned clean, then changed, is not pending by
    /// version alone — `store::mark_changed`, which `scan.rs` calls on every
    /// changed session, is what lets a sweep find it when its announcement was
    /// dropped (a full queue, or the checker off).
    #[test]
    fn a_changed_session_whose_announcement_was_dropped_is_swept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let clean = seed_session(&dir, &db_path, "s1", "nothing secret here");
        sweep(&db_path, &Shared::default());
        assert!(scanned(&db_path, "s1") && findings(&db_path, "s1").is_empty());

        // The transcript now leaks a token, and nothing announced it.
        let line = json!({"type": "user", "message": {"role": "user", "content": token('h')}});
        std::fs::write(&clean.file_path, format!("{line}\n")).expect("rewrite");
        sweep(&db_path, &Shared::default());
        assert!(
            findings(&db_path, "s1").is_empty(),
            "not pending by version"
        );

        let mut conn = db::open_read_write(&db_path).expect("open");
        assert_eq!(store::mark_changed(&mut conn, &[clean]).expect("mark"), 1);
        drop(conn);
        sweep(&db_path, &Shared::default());
        assert_eq!(findings(&db_path, "s1").len(), 1);
    }

    /// A stopped worker processes nothing more — not even what is already
    /// queued — and a sweep it is in the middle of ends.
    #[test]
    fn a_stopped_worker_processes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let item = seed_session(&dir, &db_path, "s1", &token('e'));
        let (tx, rx) = mpsc::sync_channel(4);
        tx.send(item).expect("send");
        let shared = Shared::default();
        shared.stopped.store(true, Ordering::Release);

        assert_eq!(
            run_once(&db_path, &rx, &shared, Duration::from_millis(10)),
            Pass::Stopped
        );
        sweep(&db_path, &shared);
        assert!(!scanned(&db_path, "s1"));
    }

    /// Dropping every sender — what [`stop`] does — ends the loop.
    #[test]
    fn a_disconnected_queue_stops_the_loop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let (tx, rx) = mpsc::sync_channel::<Pending>(4);
        drop(tx);
        assert_eq!(
            run_once(&db_path, &rx, &Shared::default(), Duration::from_secs(60)),
            Pass::Stopped
        );
    }

    /// An overflowed announcement is picked up by a sweep right after the next
    /// batch, not at the next five-minute tick.
    #[test]
    fn a_requested_sweep_follows_the_batch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = fixture_db(&dir);
        let queued = seed_session(&dir, &db_path, "s1", &token('f'));
        seed_session(&dir, &db_path, "s2", &token('g'));
        let (tx, rx) = mpsc::sync_channel(4);
        tx.send(queued).expect("send");
        let shared = Shared::default();
        shared.sweep_requested.store(true, Ordering::Release);

        run_once(&db_path, &rx, &shared, Duration::from_millis(10));

        assert!(
            scanned(&db_path, "s2"),
            "the dropped announcement was swept"
        );
        assert!(!shared.sweep_requested.load(Ordering::Acquire));
    }
}
