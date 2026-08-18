//! Live parity for the settings, monitoring and version reads: diff all four
//! ported endpoints against a *running* Go server, byte for byte.
//!
//! The unit tests prove each envelope's shape. This proves the resolution — the
//! settings manager's default-fill and env-override order, the monitoring
//! manager's file-versus-environment precedence — matches over the instance's
//! own row, file and environment.
//!
//! Ignored by default: it needs a real Agento instance and its database, and CI
//! has neither.
//!
//! ```sh
//! cd desktop && eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_settings -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! **Never point this at the instance on :8990.** That is whatever binary the
//! developer installed, which drifts behind the repo — a stale baseline makes a
//! wrong port look verified, and a right one look broken.
//!
//! **Both configurations have a shape that diffs clean while proving nothing.**
//! A settings row that has never been saved is all zeros, and a monitoring file
//! that does not exist is the default — so seed the scratch instance before
//! trusting a pass:
//!
//! - `PUT /api/settings` with a hidden project, a non-default idle threshold, a
//!   font family, a `public_url` and a second `claude_config_dirs` entry (any
//!   existing absolute directory will do), so the string-list columns are
//!   exercised as populated arrays rather than as `null` — and so the config-dir
//!   read resolves a set with an order and a dedup in it rather than one entry;
//! - `PUT /api/monitoring` with `otlp_headers` populated, so the `omitempty` map
//!   is exercised as present as well as absent.
//!
//! **The environment must be identical on both sides.** These endpoints read
//! `AGENTO_*` and `OTEL_*` from the process, and `parity-instance.sh` starts the
//! Go server from this shell — so the same shell running `cargo test` sees the
//! same variables. Exporting one between `start` and the test would diverge the
//! two for a reason that is not a bug.
//!
//! **Read-only.** These issue GETs and nothing else.

mod parity_common;

use parity_common::*;

use agento_lib::native::{db, gojson, monitoring, settings, version};

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_settings_read_matches_the_live_go_response() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("opening the parity database");
    let stored = settings::load_stored(&conn);

    let go = fetch("/api/settings").await;
    let native = gojson::to_vec(&settings::resolve(stored)).expect("encode");
    assert_identical("settings", &go, &native);

    // An all-zero row diffs clean against an all-zero row. Assert the response
    // carries something a save produced, so a pass means the resolution was
    // actually exercised.
    let body = String::from_utf8(go).expect("utf8");
    assert!(
        body.contains(r#""hidden_projects":["#) || body.contains(r#""claude_config_dirs":["#),
        "the parity instance has never saved a settings list — both columns are \
         null, which diffs clean and proves nothing. Seed it with PUT /api/settings \
         (see this file's header).\n{body}"
    );
}

/// The config-dir editor's source (#305).
///
/// Half a row read and half a **filesystem probe**, which is why #266 left it
/// behind: `indexed` resolves the stored preferences the way `GET /api/settings`
/// does, while `candidates` lists the home directory. Both halves are answered
/// from the same process here, and the Go server runs from this shell — so the
/// `HOME` and `CLAUDE_CONFIG_DIR` the two resolve against are the same ones.
///
/// The probe's *rules* are pinned by a unit test over a crafted home, because
/// no developer's real one exercises the exclusions. What this adds is the
/// resolution over whatever is actually there.
#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_claude_config_dirs_read_matches_the_live_go_response() {
    let db_path = live_db();
    let conn = db::open_read_only(&db_path).expect("opening the parity database");

    let go = fetch("/api/settings/claude-config-dirs").await;
    let native = gojson::to_vec(&settings::claude_config_dirs_response(&conn)).expect("encode");
    assert_identical("settings/claude-config-dirs", &go, &native);

    // A one-entry `indexed` is the answer an install that has configured
    // nothing gives, and it diffs clean against itself while proving only that
    // both sides can spell the default dir. Seed a second dir before trusting
    // a pass — the order (default, run dir, extras) and the dedup are the parts
    // that can be wrong.
    let parsed: serde_json::Value = serde_json::from_slice(&go).expect("valid JSON");
    let indexed = parsed["indexed"].as_array().map_or(0, Vec::len);
    let candidates = parsed["candidates"].as_array().map_or(0, Vec::len);
    assert!(
        indexed > 1 || candidates > 0,
        "the parity instance indexes exactly one config dir and has no candidate \
         beside it, so this diffs the default dir against itself. Seed it with \
         PUT /api/settings and a real `claude_config_dirs` entry (see the header).\n{parsed}"
    );
}

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_monitoring_read_matches_the_live_go_response() {
    // The data dir is the database's parent — for the parity instance that is
    // its scratch copy, which is the directory its `monitoring.json` lives in.
    let db_path = live_db();
    let data_dir = db_path.parent().expect("a data directory");

    let go = fetch("/api/monitoring").await;
    let native = gojson::to_vec(&monitoring::response(data_dir)).expect("encode");
    assert_identical("monitoring", &go, &native);
}

#[tokio::test]
#[ignore = "needs a running Agento instance and its database"]
async fn the_version_reads_match_the_live_go_response() {
    let go = fetch("/api/version").await;
    let native = gojson::to_vec(&version::version()).expect("encode");
    assert_identical("version", &go, &native);

    // Every build answers the short-circuit since #278; the live comparison
    // only holds for an unstamped binary, because a *stamped* Go server would
    // take the release-lookup branch this build deliberately does not have.
    // `parity-instance.sh` builds with no `-ldflags`, so this is the live case.
    let go = fetch("/api/version/update-check").await;
    let native = gojson::to_vec(&version::update_check()).expect("encode");
    assert_identical("version/update-check", &go, &native);
}
