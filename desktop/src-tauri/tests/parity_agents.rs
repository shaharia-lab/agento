//! Live parity for the agent reads: diff the ported endpoint against a *running* Go
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
//! (cd src-tauri && cargo test --test parity_agents -- --ignored --nocapture)
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

use agento_lib::native::{agents, gojson};

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
