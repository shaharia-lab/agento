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

use agento_lib::native::{diff, gojson, pricing};

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
