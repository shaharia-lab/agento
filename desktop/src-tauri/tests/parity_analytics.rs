//! Live parity for the analytics report: diff the ported endpoint against a *running* Go
//! server, byte for byte.
//!
//! The unit tests prove the port matches Go over a fixture. This proves it
//! matches over the user's real data, which is the check the porting plan calls
//! for before a route is trusted.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_analytics -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::{analytics, db, gojson, settings};

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
