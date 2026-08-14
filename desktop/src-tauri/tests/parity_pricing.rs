//! Live parity for the pricing catalog: diff the ported endpoint against a *running* Go
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
//! (cd src-tauri && cargo test --test parity_pricing -- --ignored --nocapture)
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

use agento_lib::native::{gojson, pricing};

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn pricing_catalog_matches_the_live_go_response() {
    let go = fetch("/api/pricing/catalog").await;

    let db = live_db();
    let catalog = pricing::catalog(&db).unwrap_or_else(|e| panic!("reading {}: {e}", db.display()));
    let native = gojson::to_vec(&catalog).expect("encoding catalog");

    println!(
        "{} models, revision {}",
        catalog.models.len(),
        catalog.revision
    );
    assert_identical("pricing catalog", &go, &native);
}
