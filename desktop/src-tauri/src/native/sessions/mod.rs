//! The Claude sessions list, ported from `internal/claudesessions`.
//!
//! Two endpoints, deliberately separate on the Go side and kept separate here:
//! the page changes as the user scrolls, the facets change when the filter
//! changes, and folding them together would recompute a corpus-wide aggregate
//! on every scroll tick.
//!
//! ## The scan trigger
//!
//! On the Go side, **reading is what keeps the corpus fresh**:
//! `Cache.ensureFresh` runs on every read path — `ListPage` on a first page,
//! `Facets` always — and starts a background rescan when the TTL has expired,
//! the pricing catalog has moved, or the idle threshold has changed. Serving
//! these routes from Rust therefore removes the trigger, and nothing would say
//! so: transcripts would stop being re-read, a rate edit would never reach
//! stored costs, and the list would serve indefinitely stale figures.
//!
//! Rather than reimplement that decision — four pieces of metadata, a TTL, a
//! pricing fingerprint and a user-set threshold, each of which could drift out
//! of step with the Go original — this delegates it. `freshness_probe` names a
//! cheap request against the sidecar that goes through `ensureFresh` itself, so
//! the *rules* stay in the code that owns them. It is fire-and-forget: the page
//! never waits for it, exactly as `ListPage` never waits for the rescan it
//! starts.

pub mod corpus;
pub mod detail;
pub mod page;
pub mod projects;
pub mod query;
pub mod summary;

use axum::http::Method;

use crate::native::{db, gojson, settings, Answer, Ctx, Endpoint, Request};

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: Endpoint = Endpoint {
    name: "claude sessions",
    claims,
    serve,
};

/// The four reads this area owns.
///
/// `{id}` is matched last and only as a **single** segment, so it cannot
/// swallow `/facets`, `/projects`, `/insights/summary` (a different registry
/// entry), `{id}/insights` or `{id}/journey`. `/status` and `/refresh` are
/// deliberately absent — see [`serve`].
enum Route<'a> {
    List,
    Facets,
    Projects,
    Detail(&'a str),
}

fn route_of(path: &str) -> Option<Route<'_>> {
    match path {
        "/api/claude-sessions" => return Some(Route::List),
        "/api/claude-sessions/facets" => return Some(Route::Facets),
        "/api/claude-sessions/projects" => return Some(Route::Projects),
        _ => {}
    }
    let rest = path.strip_prefix("/api/claude-sessions/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    // The remaining literal siblings are not session ids. `insights` cannot
    // appear here — `/insights/summary` has a slash and `{id}/insights` is two
    // segments — but `status` and `refresh` are single segments that would
    // otherwise be read as ids.
    if matches!(rest, "status" | "refresh") {
        return None;
    }
    Some(Route::Detail(rest))
}

fn claims(method: &Method, path: &str) -> bool {
    method == Method::GET && route_of(path).is_some()
}

/// Answer one of the four reads.
///
/// **`/status` and `/refresh` are not here, and that is a decision rather than
/// an omission.** Both are about the *scan*: `/status` reports
/// `scan_in_progress`, `files_done` and `files_total`, which are in-memory
/// state of the scanner running inside the Go sidecar, and `/refresh`
/// invalidates the cache and starts one. Rust deliberately does not own
/// scanning — `native/db.rs` opens the database **read-only** precisely so two
/// processes never write one SQLite file — so a native `/status` could only
/// answer `false`/`0`, which would be actively wrong while a Go scan runs and
/// would blank the "Scanning ~/.claude… 412 / 1,373" the list shows during a
/// first run. They move when the scanner is wired in, which is phase 3.
fn serve(ctx: &Ctx, req: &Request) -> Result<Answer, String> {
    if let Some(Route::Detail(id)) = route_of(req.path) {
        return match detail::get(&ctx.db_path, id)? {
            // Falling back lets Go answer its own 404 rather than this having
            // to reproduce the body and the status.
            None => Err(format!("claude session {id:?} not found")),
            Some(d) => Ok(Answer {
                body: gojson::to_vec(&d).map_err(|e| format!("encoding session detail: {e}"))?,
                probe: None,
            }),
        };
    }

    if req.path == "/api/claude-sessions/projects" {
        let projects = projects::list(&ctx.db_path, projects::include_hidden(req.query))?;
        return Ok(Answer {
            body: gojson::to_vec(&projects)
                .map_err(|e| format!("encoding claude projects: {e}"))?,
            probe: None,
        });
    }

    let q = query::SessionQuery::parse(req.query)?;
    let conn = db::open_read_only(&ctx.db_path)?;
    let data_settings = settings::load(&conn);

    let body = if req.path == "/api/claude-sessions" {
        let page = page::list_page(&conn, &data_settings, &q)?;
        gojson::to_vec(&page).map_err(|e| format!("encoding session page: {e}"))?
    } else {
        let facets = page::facets(&conn, &data_settings, &q)?;
        gojson::to_vec(&facets).map_err(|e| format!("encoding session facets: {e}"))?
    };
    Ok(Answer {
        body,
        probe: freshness_probe(req.path, &q),
    })
}

#[cfg(test)]
mod tests_db;

/// The request to fire at the Go sidecar to keep the corpus fresh, if this
/// route is one that would have triggered a rescan.
///
/// A continuation — a request carrying a cursor — deliberately returns `None`,
/// matching `ListPage`: freshness is a property of the scroll, decided when it
/// starts, and re-deciding it per page would put four metadata queries behind
/// every scroll tick to reach a conclusion the first page already reached.
pub fn freshness_probe(path: &str, q: &query::SessionQuery) -> Option<&'static str> {
    match path {
        "/api/claude-sessions" if q.cursor.is_empty() => Some(PROBE_PATH),
        "/api/claude-sessions/facets" => Some(PROBE_PATH),
        _ => None,
    }
}

/// The cheapest request that still runs `ensureFresh`: one page of one row,
/// unfiltered. The filter is irrelevant — freshness is a property of the
/// corpus, not of the query.
///
/// Public because every ported corpus read needs it, not just this one:
/// `Cache.Analytics` calls `ensureFresh` too, so `/api/claude-analytics` fires
/// the same probe.
pub const PROBE_PATH: &str = "/api/claude-sessions?limit=1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_page_probes_and_a_continuation_does_not() {
        let mut q = query::SessionQuery::default();
        assert_eq!(
            freshness_probe("/api/claude-sessions", &q),
            Some(PROBE_PATH)
        );

        q.cursor = "abc".into();
        assert_eq!(freshness_probe("/api/claude-sessions", &q), None);
    }

    #[test]
    fn facets_always_probe_since_go_always_checks() {
        let q = query::SessionQuery {
            cursor: "abc".into(),
            ..Default::default()
        };
        assert_eq!(
            freshness_probe("/api/claude-sessions/facets", &q),
            Some(PROBE_PATH)
        );
    }

    #[test]
    fn an_unrelated_route_never_probes() {
        let q = query::SessionQuery::default();
        assert_eq!(freshness_probe("/api/pricing/catalog", &q), None);
    }
}
