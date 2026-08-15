//! Live parity for the notification reads: diff both ported endpoints against a
//! *running* Go server, byte for byte.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_notifications -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **Both endpoints have a degenerate shape that diffs clean while proving
//! nothing**, and on a fresh scratch copy both are in it: the settings column
//! defaults to `{}` and the log table is empty (`null`). Seed before trusting a
//! pass:
//!
//! - `PUT /api/notifications/settings` with a full provider **including a
//!   password** and `preferences.scheduled_tasks.on_failed: false` — the mask
//!   and the `*bool` `omitempty` are the two rules this endpoint exists to get
//!   right, and neither is exercised by the zero value;
//! - **the log has no POST endpoint.** Rows are written only by the notification
//!   handler on a real delivery attempt, so insert them straight into the
//!   scratch database — one `sent` and one with a non-empty `error_msg`.
//!
//! ```sh
//! sqlite3 "$AGENTO_LIVE_DB" "INSERT INTO notification_log
//!   (event_type, provider, subject, status, error_msg, created_at) VALUES
//!   ('task.finished','smtp','Agento: done','sent','','2026-08-01 10:00:00 +0000 UTC'),
//!   ('task.failed','smtp','Agento: failed','error','dial tcp: refused','2026-08-02 11:30:00 +0000 UTC');"
//! ```
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::{gojson, notifications};

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_notification_settings_read_matches_the_live_go_response() {
    let db_path = live_db();

    let go = fetch("/api/notifications/settings").await;
    let native = gojson::to_vec(&notifications::get_settings(&db_path).expect("native settings"))
        .expect("encode");
    assert_identical("notifications/settings", &go, &native);

    let body = String::from_utf8(go).expect("utf8");
    assert!(
        body.contains(r#""password":"***""#),
        "the parity instance has no SMTP password stored, so the mask — the one \
         rule this endpoint exists for — is not exercised. Seed it with \
         PUT /api/notifications/settings (see this file's header).\n{body}"
    );
}

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_notification_log_read_matches_the_live_go_response() {
    let db_path = live_db();

    // Default, an explicit limit, and the values that mean "fifty" rather than
    // "none" — the handler's rule, which is not the store's.
    for query in ["", "?limit=1", "?limit=0", "?limit=abc"] {
        let go = fetch(&format!("/api/notifications/log{query}")).await;
        let limit = notifications::log_limit(query.trim_start_matches('?'));
        let native = gojson::to_vec(&notifications::list_log(&db_path, limit).expect("native log"))
            .expect("encode");
        assert_identical(&format!("notifications/log{query}"), &go, &native);
    }

    let go = fetch("/api/notifications/log").await;
    assert_ne!(
        String::from_utf8(go).expect("utf8").trim(),
        "null",
        "the parity instance has never logged a notification — `null` diffs \
         clean against `null` and proves nothing. Insert rows directly; there \
         is no POST endpoint for them (see this file's header)."
    );
}
