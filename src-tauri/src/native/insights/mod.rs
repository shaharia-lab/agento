//! The Claude session insights pipeline and the summary it feeds.
//!
//! Four parts:
//!
//! - [`processors`] recomputes one session's row from its transcripts. Pure
//!   computation — a function from a transcript to a `SessionInsight`.
//! - [`index`] routes the *same* decoded events into the three
//!   `session_search` text columns (#435), so the search index is a second
//!   product of one read rather than a second read.
//! - [`store`] is the `session_insights` half: what needs recomputing, the
//!   upsert, and the reconcile. **Every statement there keys on
//!   `(session_id, project_path)`**, which is where it parts company with the
//!   Go store; read its header before touching any of them.
//! - [`worker`] is the loop that joins the two — the boot sweep, the five-minute
//!   rescan, and the queue `scan.rs` announces changed sessions on.
//! - [`summary`] answers `GET /api/claude-sessions/insights/summary` by reading
//!   the rows back.
//!
//! ## The writer arrived late, and its absence was invisible
//!
//! This file used to say the pipeline deliberately wrote nothing, because
//! porting `insight_worker.go`'s upsert "would put two processes on one SQLite
//! file — the Go sidecar's worker and this one", and that "when the storage
//! layer moves, the worker is a loop around this". The storage layer moved
//! (#274, #278) and the Go tree was deleted (#391); the loop was not written
//! until #408.
//!
//! What that cost is worth keeping in mind, because nothing reported it: with
//! no writer, `session_insights` had exactly one remaining mutation in the whole
//! codebase — `scan.rs`'s `UPDATE … SET processor_version = 0`, which *queues*
//! rows for reprocessing. So a fresh install scanned its entire corpus and
//! Insights still read "0 sessions analysed", a migrated install silently
//! stopped gaining rows at the cut-over, and an idle-threshold change zeroed
//! every existing row's version and then left the summary reading figures
//! computed under the old threshold for good. Every one of those looks exactly
//! like a corpus with nothing interesting in it.
//!
//! ## What decides the numbers
//!
//! `isUserTurnContent` is the single predicate behind three turn-segmentation
//! sites (the scanner's `message_count`, this pipeline's `turn_count`, and the
//! journey timeline), so a change to it moves all three plus everything derived
//! from `turn_count`. Both `CurrentScannerVersion` and `CurrentProcessorVersion`
//! must be bumped together when it changes — bumping one recreates exactly the
//! drift the shared predicate exists to prevent.

pub mod index;
pub mod processors;
pub mod store;
pub mod summary;
pub mod transcript;
pub mod worker;

use axum::http::Method;

use crate::native::{db, gojson, settings, Answer, Ctx, Endpoint, Request};

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: Endpoint = Endpoint {
    name: "claude session insights",
    claims,
    serve,
};

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && path == "/api/claude-sessions/insights/summary"
}

fn serve(ctx: &Ctx, req: &Request) -> Result<Answer, String> {
    let conn = db::open_read_only(&ctx.db_path)?;
    let data_settings = settings::load(&conn);
    let summary = summary::summary(&conn, &data_settings, req.query)?;
    // It reads the corpus through Cache.List, which runs ensureFresh.
    super::scan::ensure_scan(ctx.db_path.clone());
    Ok(Answer::json(
        gojson::to_vec(&summary).map_err(|e| format!("encoding insights summary: {e}"))?,
    ))
}
