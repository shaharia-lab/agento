//! The Claude session insights pipeline and the summary it feeds.
//!
//! Two halves, deliberately split by what they can safely do today:
//!
//! - [`summary`] answers `GET /api/claude-sessions/insights/summary` by reading
//!   the `session_insights` rows. It is live behind the seam.
//! - [`processors`] recomputes those rows from a transcript. It is **pure
//!   computation and writes nothing** — see below.
//!
//! ## Why the pipeline does not write
//!
//! On the Go side `insight_worker.go` is a background writer: it subscribes to
//! session events, reprocesses on a version bump, and upserts `session_insights`.
//! Porting that *writer* now would put two processes on one SQLite file — the
//! Go sidecar's worker and this one — racing each other over the same rows.
//! Storage and the write endpoints move together later.
//!
//! So the processors are ported as what they actually are: a function from a
//! transcript to a `SessionInsight`. That is enough to verify them against real
//! data — run them over the same transcripts the Go worker already processed and
//! compare against the rows it stored — and it is the whole of the logic. When
//! the storage layer moves, the worker is a loop around this.
//!
//! ## What decides the numbers
//!
//! `isUserTurnContent` is the single predicate behind three turn-segmentation
//! sites (the scanner's `message_count`, this pipeline's `turn_count`, and the
//! journey timeline), so a change to it moves all three plus everything derived
//! from `turn_count`. Both `CurrentScannerVersion` and `CurrentProcessorVersion`
//! must be bumped together when it changes — bumping one recreates exactly the
//! drift the shared predicate exists to prevent.

pub mod processors;
pub mod summary;
pub mod transcript;

use axum::http::Method;

use crate::native::{db, gojson, sessions, settings, Answer, Ctx, Endpoint, Request};

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
    Ok(Answer::json(
        gojson::to_vec(&summary).map_err(|e| format!("encoding insights summary: {e}"))?,
    )
    .with_probe(sessions::PROBE_PATH))
}
