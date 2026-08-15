//! Live parity for the integration reads: diff all four ported endpoints
//! against a *running* Go server, byte for byte.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_integrations -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **Seed the scratch instance.** An install with no integrations answers `[]`
//! to three of these four endpoints, and three empty arrays diff clean while
//! proving nothing — in particular they exercise neither the secret scrub nor
//! the `authenticated` rule, which are the only reasons this endpoint is
//! interesting. It is a copy, so seeding it is safe:
//!
//! ```sh
//! U=$AGENTO_LIVE_URL
//! curl -s -X POST -H 'Content-Type: application/json' $U/api/integrations \
//!   -d '{"name":"Parity Telegram","type":"telegram","enabled":true,
//!        "credentials":{"bot_token":"PARITY-SECRET"},
//!        "services":{"messaging":{"enabled":true,"tools":["send_message"]}}}'
//! # …then set `auth` directly, since authenticating for real needs a live token:
//! sqlite3 "$AGENTO_LIVE_DB" \
//!   "UPDATE integrations SET auth='{\"access_token\":\"PARITY-SECRET\"}'"
//! ```
//!
//! The suite asserts it found an authenticated integration, because
//! `available-tools` is empty without one.
//!
//! **Go's `available-tools` is not order-stable.** `AvailableTools` ranges
//! `cfg.Services`, a Go map, so an integration with two services emits them in
//! either order. The port collects into a `BTreeMap` and is reproducible, which
//! matches only one of Go's orderings — hence `fetch_until`, the same treatment
//! the analytics suite gives its unstable sorts.
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::{gojson, integrations};

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_integration_reads_match_the_live_go_responses() {
    let db_path = live_db();

    // ── The list ──────────────────────────────────────────────────────────
    let go = fetch("/api/integrations").await;
    let native =
        gojson::to_vec(&integrations::list(&db_path).expect("native list")).expect("encode");
    assert_identical("integrations", &go, &native);

    let listed = integrations::list(&db_path).expect("native list");
    assert!(
        !listed.is_empty(),
        "no integrations on the parity instance — an empty list diffs clean and \
         exercises neither the scrub nor the `authenticated` rule. Seed it first \
         (see this file's header)."
    );

    // The trap this endpoint exists around: whatever the corpus holds, nothing
    // that looks like a credential may be in the response Go sent either.
    let body = String::from_utf8(go).expect("utf8");
    for forbidden in ["credentials", "bot_token", "refresh_token", "client_secret"] {
        assert!(
            !body.contains(forbidden),
            "Go's own response contains {forbidden:?} — the scrub has changed and \
             this port needs to change with it"
        );
    }

    // ── Each integration, by id ───────────────────────────────────────────
    for cfg in &listed {
        let go = fetch(&format!("/api/integrations/{}", cfg.id)).await;
        let native = gojson::to_vec(
            &integrations::get(&db_path, &cfg.id)
                .expect("native get")
                .expect("integration"),
        )
        .expect("encode");
        assert_identical(&format!("integration {}", cfg.id), &go, &native);

        // ── Its trigger rules ─────────────────────────────────────────────
        let go = fetch(&format!("/api/integrations/{}/triggers", cfg.id)).await;
        let native = gojson::to_vec(
            &integrations::list_trigger_rules(&db_path, &cfg.id).expect("native rules"),
        )
        .expect("encode");
        assert_identical(&format!("triggers for {}", cfg.id), &go, &native);
    }

    // ── The tool catalogue ────────────────────────────────────────────────
    //
    // Go's order comes from a map range, so re-ask until one of its orderings
    // matches. A genuine divergence still fails, with the byte offset.
    let native = gojson::to_vec(&integrations::available_tools(&db_path).expect("native tools"))
        .expect("encode");
    let (go, attempt) = fetch_until("/api/integrations/available-tools", "", &native, false).await;
    println!("available-tools matched on attempt {attempt}");
    assert_identical("available-tools", &go, &native);

    assert!(
        listed.iter().any(|c| c.enabled && c.authenticated),
        "no enabled and authenticated integration — `available-tools` is empty, \
         which diffs clean and proves nothing. Seed one (see this file's header)."
    );
}
