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
//! **Go's `available-tools` is not order-stable, and retrying does not fix it.**
//! `AvailableTools` ranges `cfg.Services`, a Go map, so an integration with two
//! services emits them in either order. The port collects into a `BTreeMap` and
//! is reproducible — strictly better, but it can only ever match *one* of Go's
//! orderings.
//!
//! The analytics suite handles its unstable sorts by re-asking (`fetch_until`),
//! which works there because the orderings come up roughly evenly. Here they do
//! not: measured over 25 requests against a two-service integration, Go emitted
//! 22 of one order and 3 of the other, so twelve attempts miss about one run in
//! five. A test that flakes 20% of the time is worse than no test.
//!
//! So this one endpoint is compared as a **multiset of byte-exact elements**:
//! every element's own rendering must match to the byte, and the two sides must
//! contain exactly the same elements — only the order between them is allowed
//! to differ, which is the only thing Go does not promise. Every other endpoint
//! here is a whole-body byte diff.
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::{gojson, integrations};

/// Compare two JSON arrays as multisets of **byte-exact** elements.
///
/// Elements are captured as `RawValue`, so each keeps the exact substring the
/// server sent — a reordered key or a respelled number *inside* an element
/// still fails, and only the order *between* elements is exempt. See this
/// file's header for why that one degree of freedom is granted here and nowhere
/// else in this suite.
fn assert_same_elements(label: &str, go: &[u8], native: &[u8]) {
    fn elements(body: &[u8], side: &str) -> Vec<String> {
        let text = std::str::from_utf8(body).unwrap_or_else(|e| panic!("{side} is not utf8: {e}"));
        let values: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(text.trim_end())
            .unwrap_or_else(|e| panic!("{side} is not a JSON array: {e}\n{text}"));
        let mut out: Vec<String> = values.iter().map(|v| v.get().to_string()).collect();
        out.sort();
        out
    }

    let (go_elems, native_elems) = (elements(go, "go"), elements(native, "native"));
    println!(
        "{label}: go {} elements, native {} elements",
        go_elems.len(),
        native_elems.len()
    );
    assert_eq!(
        go_elems, native_elems,
        "{label}: the two sides do not contain the same elements"
    );

    // The array's order is Go's map order and is not a property; its *length*
    // and its framing still are.
    assert_eq!(go.len(), native.len(), "{label}: byte length differs");
    println!(
        "{label}: identical as a multiset of {} elements",
        go_elems.len()
    );
}

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
    let native = gojson::to_vec(&integrations::available_tools(&db_path).expect("native tools"))
        .expect("encode");
    let go = fetch("/api/integrations/available-tools").await;
    assert_same_elements("available-tools", &go, &native);

    assert!(
        listed.iter().any(|c| c.enabled && c.authenticated),
        "no enabled and authenticated integration — `available-tools` is empty, \
         which diffs clean and proves nothing. Seed one (see this file's header)."
    );
}
