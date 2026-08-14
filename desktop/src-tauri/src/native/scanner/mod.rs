//! The Claude session scanner, ported from `internal/claudesessions/scanner.go`
//! and `scan_apply.go` (issue #270).
//!
//! The scanner is what keeps `claude_session_cache` in step with the JSONL
//! transcripts on disk. It walks every configured Claude config dir, diffs what
//! it finds against what is cached, and re-reads the transcripts that changed.
//!
//! ## This port computes; it does not write
//!
//! The Go scanner is a *writer*, and the sidecar runs its own copy of it on
//! every read path. Two processes writing one SQLite file is precisely what
//! `native::db` refuses to allow — it opens read-only so that a second writer
//! is impossible by accident, because the Go server also holds the file open,
//! runs migrations against it and re-seeds the pricing catalog on startup.
//!
//! So this follows the precedent the insight processors set (#263): the logic
//! is ported as what it actually is — a function from a transcript to a row —
//! and verified against the rows Go already wrote, field by field, over the
//! real corpus. That is a stronger check than any fixture, and it needs no
//! writes at all. Wiring it to the live database, and with it retiring the
//! freshness probe, belongs to phase 3, when storage moves and the sidecar
//! stops being a writer.

pub mod cost;
pub mod diff;
pub mod staleness;
pub mod summary_file;
pub mod walk;

/// Bumped whenever the scanner extracts something new from a transcript that
/// already-cached rows would be missing.
///
/// Cached rows carry the version that produced them; when this constant is
/// ahead of the stored one, the next incremental scan re-reads **every** file
/// even though no mtime changed, then records the new version. It must track
/// Go's `CurrentScannerVersion` exactly — a port that disagreed would either
/// force a re-read Go does not want or skip one it does.
pub const CURRENT_SCANNER_VERSION: i64 = 13;
