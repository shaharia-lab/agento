//! Diff a ported endpoint against a *running* Go server, byte for byte.
//!
//! The unit tests prove the port matches Go over a fixture. This proves it
//! matches over the user's real catalog — every seeded provider, the tiered
//! Alibaba rates, whatever they have edited by hand — which is the check the
//! porting plan calls for before a route is trusted.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! # against the reference instance on :8990 and ~/.agento
//! cargo test --test live_parity -- --ignored --nocapture
//!
//! # or point it somewhere else
//! AGENTO_LIVE_URL=http://127.0.0.1:8996 \
//! AGENTO_LIVE_DB=/tmp/scratch/agento.db \
//!   cargo test --test live_parity -- --ignored --nocapture
//! ```
//!
//! **Read-only.** It issues one GET. Never point it at an instance you are not
//! willing to have read.

use std::path::PathBuf;

use agento_lib::native::sessions::page;
use agento_lib::native::sessions::query::SessionQuery;
use agento_lib::native::{agents, analytics, db, diff, gojson, pricing, settings};

fn live_url() -> String {
    std::env::var("AGENTO_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:8990".to_string())
}

fn live_db() -> PathBuf {
    match std::env::var("AGENTO_LIVE_DB") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        // Not `paths::database_path()`: that answers with the dev directory in
        // a debug build, and this test is about the instance the user actually
        // runs.
        _ => agento_lib::paths::home()
            .expect("a home directory")
            .join(".agento")
            .join("agento.db"),
    }
}

/// Fetch as the frontend does, JSON content type included. `requireJSONContentType`
/// exempts GET, but sending it keeps this request identical to the one the UI
/// makes — and the point is to compare what the app really receives.
async fn fetch(path: &str) -> Vec<u8> {
    let url = format!("{}{path}", live_url());
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Content-Type", "application/json")
        .send()
        .await
        .unwrap_or_else(|e| panic!("GET {url} failed — is Agento running? ({e})"));

    assert!(resp.status().is_success(), "GET {url} -> {}", resp.status());
    resp.bytes().await.expect("reading body").to_vec()
}

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn pricing_catalog_matches_the_live_go_response() {
    let go = fetch("/api/pricing/catalog").await;

    let db = live_db();
    let catalog = pricing::catalog(&db).unwrap_or_else(|e| panic!("reading {}: {e}", db.display()));
    let native = gojson::to_vec(&catalog).expect("encoding catalog");

    println!(
        "go: {} bytes, native: {} bytes, {} models, revision {}",
        go.len(),
        native.len(),
        catalog.models.len(),
        catalog.revision,
    );

    match diff::compare(&go, &native) {
        diff::Outcome::Identical => println!("identical"),
        diff::Outcome::Differs(detail) => panic!("{detail}"),
    }
}

/// Compare a native body against the live server's for the same request.
fn assert_identical(label: &str, go: &[u8], native: &[u8]) {
    println!(
        "{label}: go {} bytes, native {} bytes",
        go.len(),
        native.len()
    );
    match diff::compare(go, native) {
        diff::Outcome::Identical => println!("{label}: identical"),
        diff::Outcome::Differs(detail) => panic!("{label}\n{detail}"),
    }
}

/// Every filter, sort and page shape the list offers, against real data.
///
/// One test rather than one per case: each case is a live HTTP round trip plus
/// a database read, and a single ignored test that covers the whole surface is
/// what actually gets run before a flip.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn session_list_and_facets_match_the_live_go_responses() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");
    let data_settings = settings::load(&conn);

    let cases = [
        "",
        "limit=5",
        "limit=3&sort=cost",
        "limit=3&sort=tokens",
        "limit=3&sort=duration",
        "limit=3&sort=messages",
        "limit=3&sort=nonsense",
        "limit=200",
        "q=claude",
        "q=100%25",
        "favorites=true",
        "links=with",
        "links=without",
        "cost_min=1&cost_max=40",
        "tokens_in_min=1000",
        "tokens_out_max=5000",
        "duration_min=5&duration_max=120",
        "messages_min=10",
        "from=2026-01-01T00:00:00Z&to=2026-12-31T23:59:59Z",
        "windows=1767225600000-1767229200000",
        "sort=cost&cost_min=0.5&limit=7",
    ];

    for case in cases {
        let label = if case.is_empty() { "(no filter)" } else { case };

        let go = fetch(&format!("/api/claude-sessions?{case}")).await;
        let q = SessionQuery::parse(case).expect("parse query");
        let native_page = page::list_page(&conn, &data_settings, &q).expect("native page");
        let native = gojson::to_vec(&native_page).expect("encode page");
        assert_identical(&format!("list [{label}]"), &go, &native);

        let go = fetch(&format!("/api/claude-sessions/facets?{case}")).await;
        let native_facets = page::facets(&conn, &data_settings, &q).expect("native facets");
        let native = gojson::to_vec(&native_facets).expect("encode facets");
        assert_identical(&format!("facets [{label}]"), &go, &native);
    }
}

/// Paging is where a keyset implementation goes wrong quietly: a cursor that
/// disagrees by one formatting character repeats or skips rows rather than
/// failing. This walks several pages through both implementations, each time
/// feeding Rust the cursor **Go** minted, so the two have to agree on the
/// cursor's bytes as well as the page's.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn cursors_interoperate_across_several_pages() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");
    let data_settings = settings::load(&conn);

    for sort in ["recent", "cost", "tokens", "duration", "messages"] {
        let mut cursor = String::new();
        for page_number in 1..=4 {
            let case = format!("limit=5&sort={sort}&cursor={cursor}");
            let go = fetch(&format!("/api/claude-sessions?{case}")).await;

            let q = SessionQuery::parse(&case).expect("parse query");
            let native_page = page::list_page(&conn, &data_settings, &q).expect("native page");
            let native = gojson::to_vec(&native_page).expect("encode page");
            assert_identical(&format!("{sort} page {page_number}"), &go, &native);

            // Continue from Go's cursor, so a divergence in how the two mint
            // one shows up as a divergent page rather than staying hidden.
            let parsed: serde_json::Value = serde_json::from_slice(&go).expect("json");
            cursor = parsed["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if cursor.is_empty() {
                break;
            }
        }
    }
}

/// A cursor minted under one sort must be refused by another, not silently
/// paged with. Go answers 400; the native handler's error means the proxy falls
/// back to Go, which answers the same 400 — so the observable behaviour is
/// identical either way.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn a_cursor_from_another_sort_is_refused() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");
    let data_settings = settings::load(&conn);

    let first = fetch("/api/claude-sessions?limit=2&sort=cost").await;
    let parsed: serde_json::Value = serde_json::from_slice(&first).expect("json");
    let cursor = parsed["next_cursor"].as_str().unwrap_or_default();
    if cursor.is_empty() {
        println!("corpus too small to page; skipping");
        return;
    }

    let q = SessionQuery::parse(&format!("limit=2&sort=recent&cursor={cursor}")).expect("parse");
    let err = page::list_page(&conn, &data_settings, &q).expect_err("mismatch");
    assert!(err.contains("does not match the requested sort"), "{err}");
}

/// The agents list and every agent it names, against real stored rows.
///
/// The per-agent read is driven from the list rather than from a hardcoded
/// slug, so the case only tests what the instance actually has — and covers
/// every capability shape stored there rather than the one a fixture imagines.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn agents_match_the_live_go_responses() {
    let db_path = live_db();

    let go = fetch("/api/agents").await;
    let native = gojson::to_vec(&agents::list(&db_path).expect("native list")).expect("encode");
    assert_identical("agents", &go, &native);

    let listed: serde_json::Value = serde_json::from_slice(&go).expect("json");
    let slugs: Vec<String> = listed
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|a| a["slug"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if slugs.is_empty() {
        println!("no agents configured; per-agent read not exercised");
        return;
    }

    for slug in slugs {
        let go = fetch(&format!("/api/agents/{slug}")).await;
        let agent = agents::get(&db_path, &slug)
            .expect("native get")
            .expect("agent");
        let native = gojson::to_vec(&agent).expect("encode");
        assert_identical(&format!("agent {slug}"), &go, &native);
    }
}

/// The analytics report, across the windows, timezones and granularities the
/// dashboards actually request.
///
/// **Go's own response is not byte-stable here**, and that is not a defect in
/// this port. Several builders collect into a Go map and then sort with
/// `sort.Slice`, which is unstable, so two rows tying on the sort key come out
/// in a random order — on this corpus `sessions_per_model` has two models with
/// one session each, and repeated uncached requests swap them. The Rust port
/// sorts ties deterministically (by model or project name), so it agrees with
/// *one* of the orderings Go produces. `fetch_analytics_until` therefore asks
/// Go again — evicting its memo in between — rather than failing on the first
/// disagreement, and reports how many attempts it took.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn claude_analytics_matches_the_live_go_responses() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("open database");
    let data_settings = settings::load(&conn);

    let mut cases = vec![
        // The default window: no from/to at all, which each side resolves
        // against its own clock.
        "".to_string(),
        // One bucket width per band, so every branch of walk_buckets runs.
        "from=2026-08-08&to=2026-08-14".to_string(), // hourly
        "from=2026-06-01&to=2026-08-14".to_string(), // daily
        "from=2025-01-01&to=2026-08-14".to_string(), // weekly
        "from=2018-01-01&to=2026-08-14".to_string(), // monthly
        "from=2000-01-01&to=2026-08-14".to_string(), // yearly
        // Timezones that move a day boundary, an hour cell and a weekday.
        "from=2026-06-01&to=2026-08-14&tz=Europe/Berlin".to_string(),
        "from=2026-06-01&to=2026-08-14&tz=Asia/Kolkata".to_string(),
        "from=2026-06-01&to=2026-08-14&tz=Pacific/Auckland".to_string(),
        // A window spanning a DST transition, where a fixed 24h step would
        // duplicate one key and skip another.
        "from=2026-03-01&to=2026-03-31&tz=America/New_York".to_string(),
        // RFC 3339 bounds carry their own offset; bare dates are local days.
        "from=2026-08-01T06:30:00Z&to=2026-08-13T18:00:00%2B02:00&tz=Europe/Berlin".to_string(),
        // An empty window, which returns Go's zero-valued summary — the one
        // place `unknown_pricing_models` is null rather than [].
        "from=1990-01-01&to=1990-12-31".to_string(),
        // A garbage bound falls back to the default window rather than erroring.
        "from=not-a-date&to=2026-08-14".to_string(),
    ];

    // A project filter, taken from the corpus rather than hardcoded, plus one
    // that matches nothing.
    let listed = fetch("/api/claude-analytics?from=2020-01-01&to=2026-12-31").await;
    let parsed: serde_json::Value = serde_json::from_slice(&listed).expect("json");
    if let Some(project) = parsed["projects"].as_array().and_then(|p| p.first()) {
        let encoded: String =
            form_urlencoded::byte_serialize(project.as_str().unwrap_or_default().as_bytes())
                .collect();
        cases.push(format!("from=2020-01-01&to=2026-12-31&project={encoded}"));
    }
    cases.push("from=2020-01-01&to=2026-12-31&project=/nowhere".to_string());

    for case in &cases {
        let label = if case.is_empty() { "(defaults)" } else { case };
        let native = gojson::to_vec(
            &analytics::analytics(&conn, &data_settings, case).expect("native analytics"),
        )
        .expect("encode analytics");

        let (go, attempts) = fetch_analytics_until(case, &native).await;
        if attempts > 1 {
            println!("analytics [{label}]: matched Go's ordering on attempt {attempts}");
        }
        assert_identical(&format!("analytics [{label}]"), &go, &native);
    }
}

/// Ask Go for the same report until it answers with the bytes `want` has, or
/// the attempts run out — returning the last response either way, so a genuine
/// divergence still fails the byte comparison with its offset and context.
///
/// Between attempts the memo has to be evicted, or every retry returns the
/// first answer verbatim: `analyticsCacheSize` is 20 entries keyed by the
/// window, so 21 throwaway windows push the target out.
async fn fetch_analytics_until(case: &str, want: &[u8]) -> (Vec<u8>, usize) {
    // Twelve rather than a handful: each attempt is an independent coin flip on
    // every tie, and a corpus with two ties would flake often at six.
    const ATTEMPTS: usize = 12;

    let mut last = Vec::new();
    for attempt in 1..=ATTEMPTS {
        last = fetch(&format!("/api/claude-analytics?{case}")).await;
        if last == want {
            return (last, attempt);
        }
        for day in 1..=21 {
            fetch(&format!(
                "/api/claude-analytics?from=2019-01-01&to=2019-01-{day:02}"
            ))
            .await;
        }
    }
    (last, ATTEMPTS)
}
