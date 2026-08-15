//! Live parity for the chat reads: diff the ported endpoints against a
//! *running* Go server, byte for byte.
//!
//! The unit tests prove the port matches Go over a fixture. This proves it
//! matches over the instance's real rows, which is the check the porting plan
//! calls for before a route is trusted.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_chats -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **An empty chat list diffs clean and proves nothing**, which is the state a
//! machine that has never used the chat UI is in. Seed the scratch instance —
//! it is a copy — through its own API before trusting a pass; the shapes worth
//! seeding are a chat with no messages, one with `thinking`/`text`/`tool_use`
//! blocks, one whose `blocks` column does not parse, and one with non-zero
//! token totals and a settings profile.
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::{chats, gojson};

/// The chat list and every chat it names, against real stored rows.
///
/// The per-chat read is driven from the list rather than from a hardcoded id,
/// so the case only exercises what the instance actually has — and covers every
/// message and block shape stored there rather than the ones a fixture
/// imagines.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn chats_match_the_live_go_responses() {
    let db_path = live_db();

    let go = fetch("/api/chats").await;
    let native = gojson::to_vec(&chats::list(&db_path).expect("native list")).expect("encode");
    assert_identical("chats", &go, &native);

    let listed: serde_json::Value = serde_json::from_slice(&go).expect("json");
    let ids: Vec<String> = listed
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|c| c["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if ids.is_empty() {
        panic!(
            "no chats on the parity instance — an empty list diffs clean and proves nothing. \
             Seed it through its own API first (see this file's header)."
        );
    }

    let mut with_messages = 0;
    for id in &ids {
        let go = fetch(&format!("/api/chats/{id}")).await;
        let detail = chats::get(&db_path, id).expect("native get").expect("chat");
        if !detail.messages.is_empty() {
            with_messages += 1;
        }
        let native = gojson::to_vec(&detail).expect("encode");
        assert_identical(&format!("chat {id}"), &go, &native);
    }

    // The list alone would pass on an instance whose every chat is empty, and
    // the messages are where the interesting encodings live — the raw tool_use
    // input, the block omissions, the per-message timestamps.
    assert!(
        with_messages > 0,
        "{} chats, none with messages: the message shapes were not exercised",
        ids.len()
    );
    println!("{} chats, {with_messages} with messages", ids.len());
}
