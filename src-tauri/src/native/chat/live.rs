//! The live-session registry: what `/input`, `/permission` and `/stop` reach
//! into while `/messages` is streaming.
//!
//! Mirrors `liveSessionStore` (`internal/api/livesessions.go`).
//!
//! # Why the four routes cannot be split
//!
//! This registry is **process-local**, and it is the only thing connecting the
//! four chat routes. `/messages` puts a session in; the other three look one up
//! and 409 when they cannot find it. Port `/messages` alone and `/stop` — still
//! answered by the Go sidecar — looks in a registry that will always be empty,
//! so the user's stop button silently does nothing. The four move together or
//! not at all.
//!
//! # Two maps, not one
//!
//! `sessions` holds the handles the other routes need. `in_flight` is the
//! *busy* set behind [`LiveSessions::try_lock`], which exists because a second
//! POST arriving mid-stream would read a stale `sdk_session_id` from the
//! database and start a new Claude CLI session instead of resuming the right
//! one.
//!
//! They are separate because they have different lifetimes — and Go's
//! `delete` clears **both**, which means the busy lock is released when the
//! stream ends, *before* the commit runs, contradicting its own doc comment.
//! That is reproduced here deliberately (`release` is what the stream's guard
//! calls, and the commit happens after) so the two implementations agree about
//! when a second send becomes possible.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use tokio::sync::mpsc;

use crate::claude::client::StreamControl;

/// What the non-streaming routes need to reach a running turn.
///
/// Deliberately **not** the `Session` itself: reading events needs `&mut`, and
/// that belongs to the one task streaming the response. `StreamControl` is the
/// clonable half, which is all `/stop` requires.
///
/// `question_tx` and `permission_req_tx` are absent for the same reason they are
/// absent in Go: they only ever flow handler → stream, and are owned by the
/// streaming request rather than shared.
pub struct LiveSession {
    pub control: StreamControl,
    /// The answer to an `AskUserQuestion`. Capacity 1, matching Go — which is
    /// why "is it awaiting input?" is approximated by "is the buffer free?".
    pub input_tx: mpsc::Sender<String>,
    /// The allow/deny for a tool prompt. Capacity 1, same approximation.
    pub permission_resp_tx: mpsc::Sender<bool>,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, LiveSession>,
    in_flight: HashSet<String>,
}

/// The process-wide registry.
pub struct LiveSessions {
    inner: Mutex<Inner>,
}

impl LiveSessions {
    fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Claim the right to stream a turn for `id`, or `None` when one is already
    /// running — which the caller turns into Go's 409.
    ///
    /// Returns a plain bool rather than an unlock guard: the release point is
    /// not the end of a scope but the end of the stream, and tying it to a
    /// guard's `Drop` would move it *after* the commit, which is the one thing
    /// this must not do (see the module header).
    pub fn try_lock(&self, id: &str) -> bool {
        let mut inner = self.lock();
        if inner.in_flight.contains(id) {
            return false;
        }
        inner.in_flight.insert(id.to_string());
        true
    }

    pub fn put(&self, id: &str, session: LiveSession) {
        self.lock().sessions.insert(id.to_string(), session);
    }

    /// Clone the two senders and the control handle for a live turn.
    ///
    /// Cloning rather than handing out a borrow keeps the registry lock held
    /// only for the lookup, so a slow send cannot block `/stop`.
    pub fn get(
        &self,
        id: &str,
    ) -> Option<(StreamControl, mpsc::Sender<String>, mpsc::Sender<bool>)> {
        let inner = self.lock();
        inner.sessions.get(id).map(|s| {
            (
                s.control.clone(),
                s.input_tx.clone(),
                s.permission_resp_tx.clone(),
            )
        })
    }

    /// End the turn: forget the handles **and** release the busy lock, exactly
    /// as Go's `delete` does.
    pub fn release(&self, id: &str) {
        let mut inner = self.lock();
        inner.sessions.remove(id);
        inner.in_flight.remove(id);
    }

    /// A poisoned mutex is not a reason to fail a chat: the data behind it is
    /// two collections of handles, and a panic while holding the lock leaves
    /// them structurally intact. Recovering keeps a single bad turn from
    /// wedging every later one.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The one registry. A chat turn is process state, so this is a process global —
/// the same reason Go hangs it off the single `api.Server`.
pub fn registry() -> &'static LiveSessions {
    static REGISTRY: OnceLock<LiveSessions> = OnceLock::new();
    REGISTRY.get_or_init(LiveSessions::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh registry per test — the real one is a process global, and tests
    /// sharing it would depend on each other's ordering.
    fn fresh() -> LiveSessions {
        LiveSessions::new()
    }

    #[test]
    fn a_second_send_is_refused_while_one_is_in_flight() {
        let live = fresh();
        assert!(live.try_lock("chat-1"));
        assert!(!live.try_lock("chat-1"), "a second send must be refused");
        // A different chat is unaffected.
        assert!(live.try_lock("chat-2"));

        live.release("chat-1");
        assert!(live.try_lock("chat-1"), "releasing frees the lock");
    }

    /// The lock is taken before the session is registered, so every one of the
    /// other three routes 409s during that window. Go has the same gap and the
    /// UI relies on it: the stop button is only live once streaming starts.
    #[test]
    fn a_locked_but_unregistered_chat_has_no_session() {
        let live = fresh();
        assert!(live.try_lock("chat-1"));
        assert!(live.get("chat-1").is_none());
    }

    #[test]
    fn release_clears_both_the_session_and_the_lock() {
        let live = fresh();
        assert!(live.try_lock("chat-1"));
        // `put` needs a real StreamControl, which needs a subprocess; the
        // registry half is exercised through the integration test instead.
        live.release("chat-1");
        assert!(live.get("chat-1").is_none());
        assert!(live.try_lock("chat-1"));
    }
}
