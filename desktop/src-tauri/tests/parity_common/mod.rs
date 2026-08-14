//! Shared plumbing for the live parity tests.
//!
//! Each `tests/parity_*.rs` is its own binary and compiles its own copy of this
//! module, so anything it does not use is dead — hence the crate-level allow
//! rather than a lint suppression per helper.
//!
//! The suites are split per area on purpose: they are the files every port
//! touches, and one file meant every concurrent port collided in it. Splitting
//! them also lets `cargo test --test parity_analytics` run one area's diff
//! rather than all of them.

#![allow(dead_code)]

use std::path::PathBuf;

use agento_lib::native::diff;

pub fn live_url() -> String {
    std::env::var("AGENTO_LIVE_URL").unwrap_or_else(|_| "http://127.0.0.1:8990".to_string())
}

pub fn live_db() -> PathBuf {
    match std::env::var("AGENTO_LIVE_DB") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        // Not `paths::database_path()`: that answers with the dev directory in
        // a debug build, and these tests are about the instance the user
        // actually runs.
        _ => agento_lib::paths::home()
            .expect("a home directory")
            .join(".agento")
            .join("agento.db"),
    }
}

/// Fetch as the frontend does, JSON content type included.
/// `requireJSONContentType` exempts GET, but sending it keeps this request
/// identical to the one the UI makes — and the point is to compare what the app
/// really receives.
pub async fn fetch(path: &str) -> Vec<u8> {
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

/// Compare a native body against the live server's for the same request.
pub fn assert_identical(label: &str, go: &[u8], native: &[u8]) {
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

/// Ask Go for the same response until it answers with the bytes `want` has, or
/// the attempts run out — returning the last response either way, so a genuine
/// divergence still fails the byte comparison with its offset and context.
///
/// This exists because **Go's own response is not always byte-stable**: several
/// builders collect into a map — random iteration order — and then sort with
/// `sort.Slice`, which is unstable, so two rows tying on the sort key come out
/// in either order. See "Go itself is not always byte-stable" in
/// `desktop/CLAUDE.md`.
///
/// `evict_memo` is for the analytics report, which Go memoizes: without
/// eviction every retry returns the first answer verbatim. `analyticsCacheSize`
/// is 20 entries keyed by the window, so 21 throwaway windows push the target
/// out.
pub async fn fetch_until(
    path: &str,
    case: &str,
    want: &[u8],
    evict_memo: bool,
) -> (Vec<u8>, usize) {
    // Twelve rather than a handful: each attempt is an independent coin flip on
    // every tie, and a corpus with two ties would flake often at six.
    const ATTEMPTS: usize = 12;

    let mut last = Vec::new();
    for attempt in 1..=ATTEMPTS {
        last = fetch(&format!("{path}?{case}")).await;
        if last == want {
            return (last, attempt);
        }
        if evict_memo {
            for day in 1..=21 {
                fetch(&format!(
                    "/api/claude-analytics?from=2019-01-01&to=2019-01-{day:02}"
                ))
                .await;
            }
        }
    }
    (last, ATTEMPTS)
}
