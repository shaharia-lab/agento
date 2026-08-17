//! Endpoints answered by ported Rust code instead of the Go sidecar.
//!
//! This is the far side of the migration seam described in `proxy.rs`. A route
//! listed in [`claims`] is served from here; everything else forwards. Both
//! implementations stay runnable at once, which is what makes a port
//! *verifiable* rather than merely finished — see [`diff`].
//!
//! **Failure means fall back.** Every native handler returns a `Result`, and an
//! `Err` is not turned into an HTTP error: the proxy logs it and forwards the
//! request to Go. A ported route can therefore only ever be as broken as the
//! unported one, and a schema change that outruns the Rust reader degrades to
//! the behaviour the app had before the port instead of a 500.

pub mod active_time;
pub mod agents;
pub mod analytics;
pub mod chat;
pub mod chats;
pub mod claude_settings;
pub mod db;
pub mod diff;
pub mod fs;
pub mod gojson;
pub mod gopath;
pub mod gotime;
pub mod gourl;
pub mod insights;
pub mod integration_credentials;
pub mod integrations;
pub mod migrate;
pub mod monitoring;
pub mod notifications;
pub mod pricing;
pub mod query;
pub mod scan;
pub mod scanner;
pub mod schedule;
pub mod sessions;
pub mod settings;
pub mod tasks;
pub mod tools;
pub mod uploads;
pub mod version;
pub mod writes;

use axum::body::Body;
use axum::http::{header, Method, Response, StatusCode};

use crate::paths;

/// How much of the seam is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Claimed routes are answered by Rust. The default.
    On,
    /// Nothing is claimed; every request forwards. The escape hatch if a port
    /// turns out to be wrong in the field.
    Off,
    /// Go answers, Rust computes alongside and the two are compared. The mode a
    /// port is validated in before it is trusted.
    Diff,
}

/// Read `AGENTO_DESKTOP_NATIVE` once. An unrecognized value is `On` rather than
/// an error: a typo in a developer's shell must not silently disable the code
/// paths the app now ships.
pub fn mode() -> Mode {
    use std::sync::OnceLock;
    static MODE: OnceLock<Mode> = OnceLock::new();

    *MODE.get_or_init(|| {
        match std::env::var("AGENTO_DESKTOP_NATIVE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "0" | "false" => Mode::Off,
            "diff" | "shadow" => Mode::Diff,
            _ => Mode::On,
        }
    })
}

/// A claimed request: the parts a native handler needs.
pub struct Request<'a> {
    pub method: &'a Method,
    pub path: &'a str,
    /// The raw query string without its leading `?`.
    pub query: &'a str,
    /// The `Content-Type` header, or `""` when absent.
    ///
    /// Carried for exactly one route. `POST /api/uploads` is multipart, and a
    /// multipart body cannot be parsed at all without the boundary parameter,
    /// which lives only in this header — every other claimed route decodes JSON
    /// and ignores it.
    pub content_type: &'a str,
    /// The request body, already buffered. Empty for a GET, and empty for the
    /// several writes Go accepts with no payload at all.
    pub body: &'a [u8],
}

/// What a native handler produced.
///
/// # Why the status is here and not implied
///
/// Reads were all `200`, so the seam did not carry a status at all. The writes
/// are not: Go answers `201` on every create, `204` on every delete, `202` on
/// the scan refresh, and `400`/`404`/`409`/`422` on the failure paths — and a
/// created agent answered `200` would be a wire divergence the frontend can
/// see. The handler is the only thing that knows which, so it says.
///
/// # `body: None` is not the same as an empty body
///
/// Go's deletes call `w.WriteHeader(http.StatusNoContent)` directly rather than
/// going through `writeJSON`, so a 204 carries **no `Content-Type` header** and
/// no body — not an empty JSON document, and not a zero-length body under a
/// JSON content type. `None` reproduces that; `Some(vec![])` would not.
#[derive(Debug)]
pub struct Answer {
    pub status: StatusCode,
    /// `None` sends no body and no `Content-Type`.
    pub body: Option<Vec<u8>>,
}

impl Answer {
    /// `200 OK` with a JSON body — what every ported read answers.
    pub fn json(body: Vec<u8>) -> Self {
        Self {
            status: StatusCode::OK,
            body: Some(body),
        }
    }

    /// A JSON body under a status the handler chooses.
    pub fn json_status(status: StatusCode, body: Vec<u8>) -> Self {
        Self {
            status,
            body: Some(body),
        }
    }

    /// A status with **no body and no `Content-Type`**, for the handlers that
    /// call `w.WriteHeader(...)` directly rather than going through `writeJSON`
    /// — `POST /api/claude-sessions/refresh` answers `202` this way.
    pub fn status_only(status: StatusCode) -> Self {
        Self { status, body: None }
    }

    /// `204 No Content`: no body, no `Content-Type`.
    pub fn no_content() -> Self {
        Self {
            status: StatusCode::NO_CONTENT,
            body: None,
        }
    }
}

/// What every handler needs to reach the data the Go server owns.
///
/// Deliberately not an open connection: the modules differ in what they want —
/// one read-only handle, two, or a path passed through to a helper that opens
/// its own — and pre-opening one here would make the registry decide something
/// only the handler knows.
pub struct Ctx {
    pub db_path: std::path::PathBuf,
}

/// One ported area of the API: whether it claims a request, and how it answers.
///
/// The pair travels together so claiming a route and implementing it are the
/// same edit. Splitting them across two files is how a route ends up claimed by
/// a handler that does not exist, which fails as a fallback to Go — silently.
pub struct Endpoint {
    /// Shown when a claimed request has no handler. Not on the wire.
    pub name: &'static str,
    pub claims: fn(&Method, &str) -> bool,
    pub serve: fn(&Ctx, &Request) -> Result<Answer, String>,
}

/// A request whose answer is a *stream*, not a buffered body.
///
/// # Why this is a second registry rather than a wider `Answer`
///
/// [`Answer`] is a `Vec<u8>` and [`Endpoint::serve`] is a **sync** `fn` the
/// proxy runs on `spawn_blocking` — the right shape for thirteen areas that
/// read SQLite and hand back a finished document. A chat turn is neither: it is
/// async, it lasts as long as the model talks, and buffering it would defeat
/// the point of SSE.
///
/// Widening `Answer` to express both would make every buffered handler carry a
/// streaming case it never uses, and would put an async runtime in front of a
/// blocking read. So streaming gets its own registry, and the two share only
/// `claims`-style routing. `native::claims` is the union, because the proxy asks
/// one question: is this route ours?
pub struct StreamEndpoint {
    pub name: &'static str,
    pub claims: fn(&Method, &str) -> bool,
    /// Owned rather than borrowed, so the returned future is `'static` and can
    /// outlive the call — which it must, since the response body is produced
    /// long after `serve` returns.
    pub serve: fn(StreamRequest) -> BoxFuture<'static, Result<Response<Body>, String>>,
}

/// Everything a streaming handler needs, owned.
pub struct StreamRequest {
    pub method: Method,
    pub path: String,
    pub body: Vec<u8>,
    pub db_path: std::path::PathBuf,
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Areas whose answer streams. Only chat execution, and likely only ever.
const STREAM_ENDPOINTS: &[StreamEndpoint] = &[chat::ENDPOINT];

/// Whether this request is answered by a *streaming* native handler.
pub fn claims_stream(method: &Method, path: &str) -> bool {
    STREAM_ENDPOINTS.iter().any(|e| (e.claims)(method, path))
}

/// Answer a claimed streaming request. `Err` forwards, exactly as for a
/// buffered one — and the same rule applies: a handler must fail *before* it
/// has any effect, because the forward re-runs it. For a chat turn that means
/// every check happens before the subprocess is spawned.
pub async fn serve_stream(req: StreamRequest) -> Result<Response<Body>, String> {
    for endpoint in STREAM_ENDPOINTS {
        if (endpoint.claims)(&req.method, &req.path) {
            return (endpoint.serve)(req).await;
        }
    }
    Err(format!(
        "{} {} is claimed but has no handler",
        req.method, req.path
    ))
}

/// Every ported area, in match order.
///
/// **Adding an endpoint is one appended line here plus its own module.** That
/// is the point: this file used to hold one `match` covering every route, so
/// two ports in flight at once always collided in the same hunk. Nothing here
/// knows what any module does, and no module knows about any other.
///
/// Order matters only where two entries could claim the same path, which is a
/// mistake rather than a feature — `an_endpoint_claims_at_most_one_path`
/// asserts none do.
const ENDPOINTS: &[Endpoint] = &[
    pricing::ENDPOINT,
    agents::ENDPOINT,
    sessions::ENDPOINT,
    analytics::ENDPOINT,
    insights::ENDPOINT,
    chats::ENDPOINT,
    tasks::ENDPOINT,
    settings::ENDPOINT,
    claude_settings::ENDPOINT,
    monitoring::ENDPOINT,
    version::ENDPOINT,
    notifications::ENDPOINT,
    fs::ENDPOINT,
    integrations::ENDPOINT,
    scan::ENDPOINT,
    uploads::ENDPOINT,
];

/// Whether this request is answered by ported Rust code.
///
/// Each module matches on the exact path, so an unported sibling — or a
/// trailing slash, which chi treats as a different route — falls through to Go
/// and keeps whatever answer Go gives it.
pub fn claims(method: &Method, path: &str) -> bool {
    ENDPOINTS.iter().any(|e| (e.claims)(method, path)) || claims_stream(method, path)
}

/// Whether a request may be *executed* natively in the seam's current mode.
///
/// This exists for exactly one reason, and it is the sharpest hazard #274
/// introduced. `Mode::Diff` runs **both** implementations and compares them:
/// Go answers, Rust computes alongside. For a read that is the whole point.
/// For a write it means the mutation is applied twice — two agents created, a
/// row deleted and then deleted again, a counter advanced by two. There is no
/// diff worth that, and the failure would look like a user double-clicking.
///
/// So in `Diff` mode a non-`GET` is not run natively at all; it simply
/// forwards, and Go remains the only writer. Ported writes are verified the
/// ordinary way — unit tests over a temp database, and a live parity run
/// against a scratch instance — not by shadowing production traffic.
///
/// The rule is blanket rather than per-endpoint on purpose: a flag on
/// [`Endpoint`] saying "this one mutates" is a flag someone forgets on the one
/// that does.
pub fn may_serve(mode: Mode, method: &Method) -> bool {
    match mode {
        Mode::Off => false,
        Mode::On => true,
        Mode::Diff => method == Method::GET,
    }
}

/// Routes whose diff is **expected** to differ, and why.
///
/// Diff mode compares Rust's answer against Go's. That only means anything when
/// both are answering the same question, and since #289 one route is not:
/// `/api/claude-sessions/status` reports the state of a scan, the sidecar is
/// started with `AGENTO_SCANNER=off`, and so Go always says `false`/`0`/`0`
/// while Rust says whatever is actually happening. Comparing them would report a
/// permanent false difference and train the reader to ignore the diff output —
/// which is worse than not comparing, because it hides the real ones.
///
/// This is deliberately a **short, named list** rather than a flag on the
/// endpoint: an entry here is a claim that the two implementations cannot agree
/// by construction, which should stay rare enough to enumerate.
pub fn diff_exempt(path: &str) -> bool {
    path == "/api/claude-sessions/status"
}

/// Work the shell owes a request the **sidecar** answered.
///
/// The seam's usual direction is "Rust answers, Go is the fallback". This is the
/// other one: a forwarded request can have an effect inside the shell, because
/// `AGENTO_INTEGRATIONS` tells the sidecar not to host the integration types
/// this process hosts — so Go's own `registry.Reload` after a successful
/// `POST /api/integrations/{id}/auth/validate` reaches nothing, and the shell
/// has to fire its own (#311). See `integrations::reload_after_forward` for why
/// that is the only route on this list.
///
/// Called from the proxy after Go's response is in hand, and only for a 2xx.
/// Spawned rather than awaited, which is what Go's own `reloadIntegration` does
/// with it: the answer has already been produced and a restart is not something
/// the client waits on.
pub fn after_forward(method: &Method, path: &str, status: StatusCode) {
    if !status.is_success() {
        return;
    }
    let Some(id) = integrations::reload_after_forward(method, path).map(str::to_string) else {
        return;
    };
    let Some(db_path) = paths::database_path() else {
        log::warn!("no data dir; not reloading integration {id:?} after {path}");
        return;
    };
    tokio::spawn(async move {
        integrations::registry::reload_after_auth(&db_path, &id).await;
    });
}

/// Answer a claimed request. `Err` means "fall back to the Go sidecar".
pub fn serve(req: &Request) -> Result<Answer, String> {
    let ctx = Ctx {
        db_path: paths::database_path().ok_or("no home directory to resolve the data dir")?,
    };
    for endpoint in ENDPOINTS {
        if (endpoint.claims)(req.method, req.path) {
            return (endpoint.serve)(&ctx, req);
        }
    }
    Err(format!(
        "{} {} is claimed but has no handler",
        req.method, req.path
    ))
}

/// Wrap a native answer in the response Go would have produced.
///
/// The header set is Go's, exactly: `Content-Type: application/json` with no
/// charset. The frontend does not care, but a diff of the whole exchange would.
///
/// A `None` body sends neither header nor payload, because that is what a bare
/// `w.WriteHeader(204)` does — see [`Answer`].
pub fn response(answer: Answer) -> Response<Body> {
    let builder = Response::builder().status(answer.status);
    let built = match answer.body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body)),
        None => builder.body(Body::empty()),
    };
    built.unwrap_or_else(|_| Response::new(Body::empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ported_reads_are_claimed_and_their_siblings_are_not() {
        assert!(claims(&Method::GET, "/api/pricing/catalog"));
        assert!(claims(&Method::GET, "/api/claude-sessions"));
        assert!(claims(&Method::GET, "/api/claude-sessions/facets"));
        assert!(claims(&Method::GET, "/api/claude-analytics"));

        // The rate writes moved in #306. All three share one path and differ
        // only by method, so a method this API does not have must not be
        // swallowed by the path match.
        assert!(claims(&Method::POST, "/api/pricing/rates"));
        assert!(claims(&Method::PUT, "/api/pricing/rates"));
        assert!(claims(&Method::DELETE, "/api/pricing/rates"));
        assert!(!claims(&Method::GET, "/api/pricing/rates"));
        assert!(!claims(&Method::PATCH, "/api/pricing/rates"));
        assert!(!claims(&Method::POST, "/api/pricing/rates/"));

        // The scan lifecycle moved with the scan itself (#289). `status` and
        // `refresh` are single segments, so the detail route still has to
        // exclude them by name rather than by shape — and each is claimed for
        // exactly one method.
        assert!(claims(&Method::POST, "/api/claude-sessions/refresh"));
        assert!(claims(&Method::GET, "/api/claude-sessions/status"));
        assert!(!claims(&Method::GET, "/api/claude-sessions/refresh"));
        assert!(!claims(&Method::POST, "/api/claude-sessions/status"));
        // The detail read is claimed, but only as a single segment — a nested
        // path under the same namespace must not be swallowed.
        assert!(claims(&Method::GET, "/api/claude-sessions/abc-123"));
        assert!(claims(&Method::GET, "/api/claude-sessions/projects"));
        assert!(!claims(&Method::GET, "/api/claude-sessions/"));
        // The rename/favourite write shares that path and is separated by
        // method (#296). Every other method on it still forwards.
        assert!(claims(&Method::PATCH, "/api/claude-sessions/abc-123"));
        assert!(!claims(&Method::PUT, "/api/claude-sessions/abc-123"));
        assert!(!claims(&Method::DELETE, "/api/claude-sessions/abc-123"));
        assert!(!claims(&Method::PATCH, "/api/claude-sessions/"));
        assert!(!claims(&Method::PATCH, "/api/claude-sessions/abc/journey"));
        assert!(!claims(
            &Method::GET,
            "/api/claude-sessions/abc-123/journey"
        ));
        assert!(claims(
            &Method::GET,
            "/api/claude-sessions/insights/summary"
        ));
        // The per-session insight record is a different route and stays with Go.
        assert!(!claims(&Method::GET, "/api/claude-sessions/abc/insights"));

        // Agents: the two reads and, since #274, the three writes. `duplicate`
        // is a different route and still forwards.
        assert!(claims(&Method::GET, "/api/agents"));
        assert!(claims(&Method::GET, "/api/agents/my-agent"));
        assert!(claims(&Method::POST, "/api/agents"));
        assert!(claims(&Method::PUT, "/api/agents/my-agent"));
        assert!(claims(&Method::DELETE, "/api/agents/my-agent"));
        assert!(!claims(&Method::POST, "/api/agents/my-agent/duplicate"));
        assert!(!claims(&Method::GET, "/api/agents/my-agent/duplicate"));
        assert!(!claims(&Method::GET, "/api/agents/"));

        // Chats: the two reads and, since #274, the CRUD. The streaming turn
        // stays with Go — `/messages` is a POST-based SSE response, which is the
        // one thing the proxy must never buffer, and #276 owns it.
        assert!(claims(&Method::GET, "/api/chats"));
        assert!(claims(&Method::GET, "/api/chats/abc-123"));
        assert!(claims(&Method::POST, "/api/chats"));
        assert!(claims(&Method::DELETE, "/api/chats"));
        assert!(claims(&Method::PATCH, "/api/chats/abc-123"));
        assert!(claims(&Method::DELETE, "/api/chats/abc-123"));
        // Since #276 the four streaming actions are ours too — but through the
        // *streaming* registry, which is why `claims` (the union) says yes.
        assert!(claims(&Method::POST, "/api/chats/abc-123/messages"));
        assert!(claims(&Method::POST, "/api/chats/abc-123/stop"));
        assert!(claims(&Method::POST, "/api/chats/abc-123/input"));
        assert!(claims(&Method::POST, "/api/chats/abc-123/permission"));
        assert!(claims_stream(&Method::POST, "/api/chats/abc-123/messages"));
        // …and only the streaming one: the buffered registry must not also
        // claim them, or a chat turn would be answered with a `Vec<u8>`.
        assert!(!ENDPOINTS
            .iter()
            .any(|e| (e.claims)(&Method::POST, "/api/chats/abc-123/messages")));
        assert!(!claims(&Method::GET, "/api/chats/abc-123/messages"));
        assert!(!claims(&Method::GET, "/api/chats/abc-123/stop"));
        assert!(!claims(&Method::GET, "/api/chats/"));

        // Tasks and job history: the five reads. The two POST actions share the
        // `/api/tasks/{id}` prefix and must not be swallowed by it.
        assert!(claims(&Method::GET, "/api/tasks"));
        assert!(claims(&Method::GET, "/api/tasks/abc-123"));
        assert!(claims(&Method::GET, "/api/tasks/abc-123/job-history"));
        assert!(claims(&Method::GET, "/api/job-history"));
        assert!(claims(&Method::GET, "/api/job-history/abc-123"));
        // Task writes all touch the scheduler, so they are #275's; the two
        // job-history deletes are pure row removals and moved in #274.
        assert!(!claims(&Method::POST, "/api/tasks"));
        assert!(!claims(&Method::PUT, "/api/tasks/abc-123"));
        assert!(!claims(&Method::POST, "/api/tasks/abc-123/pause"));
        assert!(!claims(&Method::POST, "/api/tasks/abc-123/resume"));
        assert!(!claims(&Method::DELETE, "/api/tasks/abc-123"));
        assert!(claims(&Method::DELETE, "/api/job-history"));
        assert!(claims(&Method::DELETE, "/api/job-history/abc-123"));
        assert!(!claims(&Method::GET, "/api/tasks/"));
        assert!(!claims(&Method::GET, "/api/job-history/"));

        // Settings: the row read and, since #305, the config-dir probe. The
        // write is implemented in `settings::update` but stays unclaimed: the
        // sidecar holds its own copy of these preferences in memory and is
        // still serving routes that read it — see that module's `claims`.
        assert!(claims(&Method::GET, "/api/settings"));
        assert!(claims(&Method::GET, "/api/settings/claude-config-dirs"));
        assert!(!claims(&Method::PUT, "/api/settings"));
        // Claude Code's own settings.json and the profiles beside it: a
        // different tree entirely, and since #304 all nine routes are ours —
        // reads included, because `GET .../profiles` seeds the index and so is
        // itself a write.
        assert!(claims(&Method::GET, "/api/claude-settings"));
        assert!(claims(&Method::PUT, "/api/claude-settings"));
        assert!(claims(&Method::GET, "/api/claude-settings/profiles"));
        assert!(claims(&Method::POST, "/api/claude-settings/profiles"));
        assert!(claims(&Method::GET, "/api/claude-settings/profiles/work"));
        assert!(claims(&Method::PUT, "/api/claude-settings/profiles/work"));
        assert!(claims(
            &Method::DELETE,
            "/api/claude-settings/profiles/work"
        ));
        assert!(claims(
            &Method::POST,
            "/api/claude-settings/profiles/work/duplicate"
        ));
        assert!(claims(
            &Method::PUT,
            "/api/claude-settings/profiles/work/default"
        ));
        assert!(!claims(&Method::POST, "/api/claude-settings"));
        assert!(!claims(&Method::GET, "/api/claude-settings/profiles/"));

        // Monitoring: the read, plus the two writes this build **declines**
        // (#309). They are claimed rather than forwarded on purpose — a forward
        // would reach a sidecar that would save the config and reload its own
        // providers, which is the outcome the decision exists to stop.
        assert!(claims(&Method::GET, "/api/monitoring"));
        assert!(claims(&Method::PUT, "/api/monitoring"));
        assert!(claims(&Method::POST, "/api/monitoring/test"));
        assert!(!claims(&Method::DELETE, "/api/monitoring"));
        assert!(!claims(&Method::GET, "/api/monitoring/test"));

        // Version: both reads are claimed, but the update check falls back for
        // a stamped release — that branch is the self-updater, not a read.
        assert!(claims(&Method::GET, "/api/version"));
        assert!(claims(&Method::GET, "/api/version/update-check"));
        assert!(!claims(&Method::GET, "/api/version/"));

        // Notifications: the two reads, and since #307 the settings write and
        // the test send — the one route in the port that dials a server we do
        // not run.
        assert!(claims(&Method::GET, "/api/notifications/settings"));
        assert!(claims(&Method::GET, "/api/notifications/log"));
        assert!(claims(&Method::PUT, "/api/notifications/settings"));
        assert!(claims(&Method::POST, "/api/notifications/test"));
        // Each route is claimed for its own methods and no others.
        assert!(!claims(&Method::GET, "/api/notifications/test"));
        assert!(!claims(&Method::POST, "/api/notifications/settings"));
        assert!(!claims(&Method::DELETE, "/api/notifications/settings"));
        assert!(!claims(&Method::PUT, "/api/notifications/log"));
        assert!(!claims(&Method::GET, "/api/notifications"));

        // The filesystem listing and the one directory create, on the platforms
        // `gopath` speaks (#296).
        assert!(claims(&Method::GET, "/api/fs"));
        assert!(claims(&Method::POST, "/api/fs/mkdir"));
        assert!(!claims(&Method::GET, "/api/fs/mkdir"));
        assert!(!claims(&Method::POST, "/api/fs"));

        // Uploads (#308): the one multipart route, and the only claimed one
        // with no read at all.
        assert!(claims(&Method::POST, "/api/uploads"));
        assert!(!claims(&Method::GET, "/api/uploads"));
        assert!(!claims(&Method::POST, "/api/uploads/"));

        // Continuing a Claude session (#308) is a POST under the sessions
        // prefix, and must not be reachable by any other method or shape.
        assert!(claims(
            &Method::POST,
            "/api/claude-sessions/abc-123/continue"
        ));
        assert!(!claims(
            &Method::GET,
            "/api/claude-sessions/abc-123/continue"
        ));
        assert!(!claims(&Method::POST, "/api/claude-sessions//continue"));
        assert!(!claims(&Method::POST, "/api/claude-sessions/continue"));
        assert!(!claims(
            &Method::POST,
            "/api/claude-sessions/abc/journey/continue"
        ));

        // Integrations: the four reads, plus every write that is not an OAuth
        // flow, a webhook registration or WhatsApp. Anything that dials a
        // remote service or reads in-memory OAuth state stays with Go.
        assert!(claims(&Method::GET, "/api/integrations"));
        assert!(claims(&Method::GET, "/api/integrations/available-tools"));
        assert!(claims(&Method::GET, "/api/integrations/abc"));
        assert!(claims(&Method::GET, "/api/integrations/abc/triggers"));
        assert!(claims(&Method::POST, "/api/integrations"));
        assert!(claims(&Method::PUT, "/api/integrations/abc/triggers/r1"));
        // Ours since #311. These reload and stop the live MCP server, which is
        // now hosted here and nowhere else — the sidecar runs with
        // `AGENTO_INTEGRATIONS=off`.
        assert!(claims(&Method::PUT, "/api/integrations/abc"));
        assert!(claims(&Method::DELETE, "/api/integrations/abc"));
        assert!(!claims(&Method::PATCH, "/api/integrations/abc"));
        assert!(!claims(&Method::GET, "/api/integrations/abc/auth/status"));
        assert!(!claims(
            &Method::GET,
            "/api/integrations/abc/webhook/status"
        ));
        assert!(!claims(&Method::GET, "/api/integrations/abc/whatsapp/qr"));
    }

    /// #294: what every `claims` function above matches on is the path **chi**
    /// routes on, not the raw request target.
    ///
    /// `proxy.rs` runs `gourl::route_path` once before this registry is asked
    /// anything, so the property worth pinning here is that both spellings a
    /// percent-encoded id can arrive in reach `claims` the way Go's router would
    /// deliver them — including the one where the right answer is *not* to
    /// decode.
    #[test]
    fn a_claim_matches_on_the_path_chi_would_route_on() {
        let route = |raw: &str| gourl::route_path(raw).expect("a routable target");

        // Canonical escaping: Go decodes, so `agents::slug_of` extracts `a b`,
        // and the row `agents::update` looks up is the one Go updates. Matching
        // the raw target claimed `a%20b` and answered 404 for a live agent.
        assert_eq!(route("/api/agents/a%20b"), "/api/agents/a b");
        assert!(claims(&Method::PUT, &route("/api/agents/a%20b")));

        // Non-canonical: `-` needs no escaping, so Go keeps the escaped form and
        // so must this. The issue's own example, and the case already correct.
        assert_eq!(route("/api/agents/a%2Db"), "/api/agents/a%2Db");
        assert!(claims(&Method::PUT, &route("/api/agents/a%2Db")));

        // An encoded separator stays encoded, which is what keeps a one-segment
        // route one segment. A blanket decode — the obvious fix — would make
        // this `/api/agents/a/b`, `slug_of` would reject it, and a PUT Go
        // applies would forward instead of being claimed.
        assert_eq!(route("/api/agents/a%2Fb"), "/api/agents/a%2Fb");
        assert!(claims(&Method::PUT, &route("/api/agents/a%2Fb")));

        // The same rule reaches every module's segment, not just agents'.
        assert!(claims(&Method::GET, &route("/api/chats/a%20b")));
        assert!(claims(&Method::GET, &route("/api/tasks/a%20b")));
        assert!(claims(
            &Method::PUT,
            &route("/api/integrations/a%20b/triggers/r%201")
        ));
        assert!(claims(
            &Method::POST,
            &route("/api/claude-sessions/a%20b/continue")
        ));

        // Canonicality is a property of the **whole** path, not of a segment:
        // one non-canonical escape anywhere leaves every segment raw, `r%201`
        // included, even though on its own it would have decoded. This is the
        // case a future `slug_of`/`route_of` change is most likely to get
        // wrong, and it is why the rule is applied to the path rather than to
        // each id.
        assert_eq!(
            route("/api/integrations/a%2Db/triggers/r%201"),
            "/api/integrations/a%2Db/triggers/r%201"
        );
        assert!(claims(
            &Method::PUT,
            &route("/api/integrations/a%2Db/triggers/r%201")
        ));

        // A malformed target has no route path at all: `url.ParseRequestURI`
        // rejects it, so Go answers 400 from inside `net/http` and the proxy
        // forwards rather than inventing one.
        assert!(gourl::route_path("/api/agents/a%2").is_none());
    }

    /// The write surface, asserted route by route against the table
    /// `desktop/parity/write_routes.json` records (#296).
    ///
    /// #293 accounted for the deferred writes **by category** — scheduler, chat
    /// execution, integrations, scan — which reads well and cannot be audited:
    /// nothing said whether the categories covered every route, and two escaped
    /// all of them. The Go half of this pair walks the real router and fails
    /// when a write route has no recorded decision; this half fails when a
    /// route's real disposition stops matching the decision.
    ///
    /// So a route cannot be claimed, unclaimed, added or removed without the
    /// frozen file moving — which is what makes the next agent able to *audit*
    /// the split rather than trust it.
    #[test]
    fn every_write_route_matches_its_recorded_disposition() {
        use serde::Deserialize;

        #[derive(Deserialize)]
        struct Row {
            method: String,
            route: String,
            /// `native`, `deferred` or `dropped`. The last two are both "not
            /// ours" to `claims`, and are a different answer to everyone else.
            status: String,
            owner: String,
            reason: String,
        }

        #[derive(Deserialize)]
        struct Table {
            routes: Vec<Row>,
        }

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../parity/write_routes.json");
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {path}: {e} — regenerate it from Go"));
        let table: Table = serde_json::from_str(&raw).expect("parsing write routes");

        // Completeness is the Go half's job — it walks the router and fails on
        // an unclassified route — so this only guards against reading an empty
        // or truncated file, and deliberately carries no count that would drift.
        assert!(!table.routes.is_empty(), "no write routes in {path}");

        for row in table.routes {
            let method = Method::from_bytes(row.method.as_bytes())
                .unwrap_or_else(|_| panic!("bad method {:?}", row.method));
            // The table carries chi's patterns; `claims` matches concrete
            // paths, so every parameter segment gets a sample value.
            //
            // Generic rather than a list of the three names in use today: an
            // unsubstituted `{taskID}` would leave literal braces, `claims`
            // would answer `false`, and a **deferred** route would then pass
            // having asserted nothing — silently, and on the larger half of the
            // table.
            let concrete = row
                .route
                .split('/')
                .map(|segment| {
                    if segment.starts_with('{') && segment.ends_with('}') {
                        "sample"
                    } else {
                        segment
                    }
                })
                .collect::<Vec<_>>()
                .join("/");

            // Membership first: `status == "native"` collapses three legal
            // values to a boolean, so a typo on a *deferred* row would compare
            // false-to-false and assert nothing — silently, on the larger half.
            assert!(
                matches!(row.status.as_str(), "native" | "deferred" | "dropped"),
                "{} {} has status {:?}; want native, deferred or dropped",
                row.method,
                row.route,
                row.status,
            );

            assert_eq!(
                claims(&method, &concrete),
                row.status == "native",
                "{} {} — the table says status={:?} ({}, {}). Either the claim moved \
                 and the table is stale, or a route was claimed without deciding \
                 about it. Regenerate with: go test ./desktop/parity/ -run \
                 TestWriteRoutes -update-write-routes",
                row.method,
                row.route,
                row.status,
                row.owner,
                row.reason,
            );
        }
    }

    #[test]
    fn an_unhandled_claim_is_an_error_so_the_proxy_falls_back() {
        assert!(serve(&Request {
            method: &Method::GET,
            path: "/api/nothing-here",
            query: "",
            content_type: "",
            body: &[],
        })
        .is_err());
    }

    /// The single most dangerous thing #274 could get wrong.
    ///
    /// `Diff` mode runs Go *and* Rust and compares them. For a read that is the
    /// whole point; for a write it applies the mutation twice. If this test
    /// ever goes green with `Diff` allowing a POST, turning on shadow mode
    /// silently doubles every create the user makes.
    #[test]
    fn shadow_mode_never_executes_a_write() {
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(
                !may_serve(Mode::Diff, &method),
                "{method} must not run natively in diff mode — it would apply twice"
            );
            // …but it is perfectly fine in the normal mode, which is the whole
            // point of the port.
            assert!(may_serve(Mode::On, &method));
            assert!(!may_serve(Mode::Off, &method));
        }

        // Reads are unaffected in every mode but Off.
        assert!(may_serve(Mode::Diff, &Method::GET));
        assert!(may_serve(Mode::On, &Method::GET));
        assert!(!may_serve(Mode::Off, &Method::GET));
    }

    /// Go's deletes call `w.WriteHeader(204)` directly rather than going
    /// through `writeJSON`, so there is no `Content-Type` and no body. An empty
    /// JSON body under a JSON content type is a different response.
    #[test]
    fn a_no_content_answer_carries_neither_body_nor_content_type() {
        let resp = response(Answer::no_content());
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(resp.headers().get(header::CONTENT_TYPE).is_none());

        let created = response(Answer::json_status(StatusCode::CREATED, b"{}".to_vec()));
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(
            created.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );

        // The read path is unchanged: 200 plus the JSON header.
        let read = response(Answer::json(b"[]".to_vec()));
        assert_eq!(read.status(), StatusCode::OK);
        assert_eq!(
            read.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    /// Two modules claiming one path is a merge accident, not a feature: the
    /// registry would silently hand the request to whichever was listed first,
    /// and the other module's tests would keep passing.
    #[test]
    fn no_two_endpoints_claim_the_same_request() {
        let paths = [
            "/api/pricing/catalog",
            "/api/claude-sessions",
            "/api/claude-sessions/facets",
            "/api/claude-analytics",
            "/api/claude-sessions/insights/summary",
            "/api/agents",
            "/api/agents/my-agent",
            "/api/chats",
            "/api/chats/abc-123",
            "/api/tasks",
            "/api/tasks/abc-123",
            "/api/tasks/abc-123/job-history",
            "/api/job-history",
            "/api/job-history/abc-123",
            "/api/settings",
            "/api/settings/claude-config-dirs",
            "/api/claude-settings",
            "/api/claude-settings/profiles",
            "/api/claude-settings/profiles/work",
            "/api/monitoring",
            "/api/version",
            "/api/version/update-check",
            "/api/notifications/settings",
            "/api/notifications/log",
            "/api/fs",
            "/api/integrations",
            "/api/integrations/available-tools",
            "/api/integrations/abc",
            "/api/integrations/abc/triggers",
        ];
        for path in paths {
            let owners: Vec<&str> = ENDPOINTS
                .iter()
                .filter(|e| (e.claims)(&Method::GET, path))
                .map(|e| e.name)
                .collect();
            assert_eq!(owners.len(), 1, "{path} is claimed by {owners:?}");
        }
    }

    /// Every entry must be reachable. An endpoint appended to `ENDPOINTS` whose
    /// `claims` never fires is dead code that reads as a shipped port.
    #[test]
    fn every_registered_endpoint_claims_something() {
        let probes = [
            "/api/pricing/catalog",
            "/api/claude-sessions",
            "/api/claude-sessions/facets",
            "/api/claude-sessions/status",
            "/api/claude-analytics",
            "/api/claude-sessions/insights/summary",
            "/api/agents",
            "/api/agents/my-agent",
            "/api/chats",
            "/api/chats/abc-123",
            "/api/tasks",
            "/api/tasks/abc-123",
            "/api/tasks/abc-123/job-history",
            "/api/job-history",
            "/api/job-history/abc-123",
            "/api/settings",
            "/api/settings/claude-config-dirs",
            "/api/claude-settings",
            "/api/claude-settings/profiles",
            "/api/claude-settings/profiles/work",
            "/api/monitoring",
            "/api/version",
            "/api/version/update-check",
            "/api/notifications/settings",
            "/api/notifications/log",
            "/api/fs",
            "/api/integrations",
            "/api/integrations/available-tools",
            "/api/integrations/abc",
            "/api/integrations/abc/triggers",
        ];
        // Paired with a method, because not every area has a read: `uploads`
        // claims a POST and nothing else, and a GET-only probe list would
        // report it as dead code.
        let writes = [
            (Method::POST, "/api/uploads"),
            (Method::POST, "/api/claude-sessions/abc/continue"),
        ];
        for endpoint in ENDPOINTS {
            let reachable = probes.iter().any(|p| (endpoint.claims)(&Method::GET, p))
                || writes.iter().any(|(m, p)| (endpoint.claims)(m, p));
            assert!(
                reachable,
                "{} claims none of the probe paths; add one when you add a route",
                endpoint.name
            );
        }
    }
}
