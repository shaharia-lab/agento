//! Live parity for the Claude settings surface (#304): all nine routes, diffed
//! against a Go server built from **this** checkout.
//!
//! ```sh
//! cd desktop
//! export AGENTO_PARITY_WORKER=304
//! export CLAUDE_CONFIG_DIR=/tmp/agento-parity-claude-304   # required — see below
//! mkdir -p "$CLAUDE_CONFIG_DIR"
//! eval "$(./scripts/parity-instance.sh start)"
//! (cd src-tauri && cargo test --test parity_claude_settings -- --ignored --nocapture)
//! ./scripts/parity-instance.sh stop
//! ```
//!
//! # This suite writes, and what it writes is not in the database
//!
//! `parity-instance.sh` protects the *database* by copying it. It does nothing
//! for `~/.claude`, and this surface writes there: `PUT /api/claude-settings`
//! overwrites `settings.json`, and `PUT .../profiles/{id}/default` overwrites it
//! with a profile's contents. Pointed at the default dir, this suite would
//! rewrite the developer's real Claude Code configuration.
//!
//! So it **refuses to run** unless `CLAUDE_CONFIG_DIR` is exported and names
//! something other than `~/.claude`. That variable is also the first thing
//! `config.ClaudeRunConfigDir` consults, which is the second reason it is
//! required: exporting it *before* `parity-instance.sh start` moves both
//! implementations to one directory, and a diff across two directories would
//! mean nothing.
//!
//! # One directory, so one case at a time
//!
//! Every case here mutates `settings.json` and `settings_profiles.json`, which
//! the whole suite shares. `serial()` makes that safe under the default parallel
//! runner rather than relying on `--test-threads=1` being remembered, and each
//! case sets up what it needs instead of inheriting it from the case before —
//! the first draft did not, and read a file a *previous* case had written while
//! believing it was Go's.
//!
//! # Seeding, and why an empty pass proves nothing
//!
//! A machine with no profiles answers `GET .../profiles` with the one-element
//! list it just seeded, and `GET /api/claude-settings` with `{"exists":false}`.
//! Both diff clean against a port that does nothing, so every case creates its
//! data through the Go server first and asserts the response is not that shape.
//!
//! # The comparison has two halves, as the write suites do
//!
//! A read is asked of both implementations and diffed byte for byte. A write
//! cannot be — whichever runs first changes what the second would see — so Go's
//! answers are pinned here as literals and the unit tests in
//! `native/claude_settings/` assert the same literals against Rust. This half
//! cannot be wrong about what Go does, because it asked.

mod parity_common;

use parity_common::*;

use std::path::PathBuf;
use std::sync::OnceLock;

use tokio::sync::{Mutex, MutexGuard};

use agento_lib::native::claude_settings::{self, profiles, Decoded};
use agento_lib::native::writes::finish;
use agento_lib::native::Answer;

use reqwest::Method;

/// One case at a time: they share two files in one directory.
///
/// `tokio`'s mutex rather than `std`'s, because the guard is held across every
/// `await` in a case — a blocking guard there is `clippy::await_holding_lock`,
/// and it has no poisoning to recover from when a case fails.
async fn serial() -> MutexGuard<'static, ()> {
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
    SERIAL.get_or_init(|| Mutex::new(())).lock().await
}

/// The scratch Claude config dir both sides are pointed at.
///
/// Asserted rather than defaulted: what this guards against is silent and
/// destructive, and a default would make it easy to run without noticing.
fn scratch_claude_dir() -> String {
    let dir = std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_default();
    assert!(
        !dir.trim().is_empty(),
        "CLAUDE_CONFIG_DIR must be exported before `parity-instance.sh start`. \
         This suite writes settings.json and profile files, and without it both \
         implementations would target the developer's real ~/.claude."
    );
    let default = agento_lib::paths::home()
        .expect("a home directory")
        .join(".claude");
    assert_ne!(
        PathBuf::from(&dir),
        default,
        "CLAUDE_CONFIG_DIR must not be the real ~/.claude — this suite overwrites settings.json"
    );
    std::fs::create_dir_all(&dir).expect("creating the scratch Claude config dir");
    dir
}

/// Enter a case: hold the serial lock, check the instance, and confirm Rust
/// resolves the *same* run config dir the Go server was started with.
///
/// That last check is this port's first trap in one line. If `run_dir` picked a
/// different directory every diff below would compare two unrelated files — and,
/// worse, `PUT /api/claude-settings` would write `settings.json` somewhere no
/// agent run reads it.
async fn enter() -> (MutexGuard<'static, ()>, String) {
    let guard = serial().await;
    assert!(
        std::env::var("AGENTO_LIVE_URL").is_ok(),
        "parity_claude_settings mutates the filesystem and refuses to guess an \
         instance. Run `eval \"$(./scripts/parity-instance.sh start)\"` first."
    );
    let dir = scratch_claude_dir();
    assert_eq!(
        claude_settings::run_dir(&live_db()).expect("resolving the run config dir"),
        dir,
        "the native run config dir must be the one the Go server was started with"
    );
    (guard, dir)
}

/// Unwrap a native answer's body, asserting its status on the way past.
fn native_body(label: &str, answer: Result<Answer, String>, want_status: u16) -> Vec<u8> {
    let answer = answer.unwrap_or_else(|e| panic!("{label}: native handler forwarded: {e}"));
    assert_eq!(
        answer.status.as_u16(),
        want_status,
        "{label}: native status"
    );
    answer.body.unwrap_or_default()
}

/// Create a profile through Go, returning its id.
async fn create_profile(name: &str) -> String {
    let (status, body) = send(
        Method::POST,
        "/api/claude-settings/profiles",
        Some(&format!(r#"{{"name":"{name}"}}"#)),
    )
    .await;
    assert_eq!(
        status,
        201,
        "creating {name}: {}",
        String::from_utf8_lossy(&body)
    );
    serde_json::from_slice::<serde_json::Value>(&body).expect("json")["id"]
        .as_str()
        .expect("an id")
        .to_string()
}

async fn delete_profile(id: &str) {
    let _ = send(
        Method::DELETE,
        &format!("/api/claude-settings/profiles/{id}"),
        None,
    )
    .await;
}

// ─── The reads ────────────────────────────────────────────────────────────────

/// `GET /api/claude-settings`, over a file Go wrote.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR"]
async fn the_claude_settings_read_matches_the_live_go_response() {
    let (_serial, dir) = enter().await;

    let (status, _) = send(
        Method::PUT,
        "/api/claude-settings",
        Some(r#"{"z":1,"model":"opus","env":{"B":"2","A":"1"},"tag":"<b>","rate":1.50}"#),
    )
    .await;
    assert_eq!(status, 200, "seeding PUT /api/claude-settings");

    let go = fetch("/api/claude-settings").await;
    let body = String::from_utf8(go.clone()).expect("utf8");
    assert!(
        body.contains(r#""exists":true"#) && body.contains("\"model\""),
        "the scratch dir has no settings.json — seeding failed, and an \
         `{{\"exists\":false}}` answer diffs clean while proving nothing.\n{body}"
    );

    let native = native_body("claude-settings", claude_settings::get_settings(&dir), 200);
    assert_identical("claude-settings", &go, &native);

    // The path trap, checked against the filesystem rather than against a
    // response: the file `--settings` resolves against is the one in the run
    // config dir (#242), and it is the file this endpoint just wrote.
    let on_disk = std::fs::read_to_string(format!("{dir}/settings.json")).expect("settings.json");
    assert!(
        on_disk.contains("\"model\": \"opus\""),
        "PUT /api/claude-settings must write settings.json inside the run config dir\n{on_disk}"
    );
}

/// The list and every profile's detail.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR"]
async fn the_profile_reads_match_the_live_go_response() {
    let (_serial, dir) = enter().await;

    // Beyond the default the first list seeds, so this is not the one-element
    // shape a port that does nothing also produces.
    let a = create_profile("Parity Read A").await;
    let b = create_profile("Parity Read B").await;
    let missing = create_profile("Parity Read Missing").await;
    let unparseable = create_profile("Parity Read Broken").await;

    // …and one profile with real settings, so `settings` is a compacted
    // document rather than `{}` for at least one row.
    let (status, _) = send(
        Method::PUT,
        &format!("/api/claude-settings/profiles/{a}"),
        Some(r#"{"settings":{"model":"opus","env":{"B":"2","A":"1"},"tag":"<b>"}}"#),
    )
    .await;
    assert_eq!(status, 200, "seeding a profile's settings");

    // The other two states of `settings`/`exists`, which a profile created
    // through the API never reaches on its own: no file at all, and a file that
    // is not JSON. Both are `{"settings":null,"exists":false}` — an *answer*,
    // not an error — and a port that treated either as a failure would forward
    // a request Go answers with a 200.
    std::fs::remove_file(format!("{dir}/settings_{missing}.json")).expect("removing");
    std::fs::write(format!("{dir}/settings_{unparseable}.json"), "not json").expect("writing");

    let go = fetch("/api/claude-settings/profiles").await;
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&go).expect("json");
    assert!(
        listed.len() >= 3,
        "fewer profiles than this case created — an almost empty list diffs \
         clean and proves nothing: {}",
        String::from_utf8_lossy(&go)
    );

    let native = native_body("profiles", finish(profiles::list(&dir)), 200);
    assert_identical("claude-settings/profiles", &go, &native);

    let mut with_settings = 0;
    let mut without_settings = 0;
    for profile in &listed {
        let id = profile["id"].as_str().expect("an id");
        let path = format!("/api/claude-settings/profiles/{id}");
        let go = fetch(&path).await;
        if String::from_utf8_lossy(&go).contains(r#""exists":true"#) {
            with_settings += 1;
        } else {
            without_settings += 1;
        }
        let native = native_body(&path, finish(profiles::get(&dir, id)), 200);
        assert_identical(&path, &go, &native);
    }
    assert!(
        with_settings > 0 && without_settings > 0,
        "both halves of the `settings`/`exists` pair must be exercised, \
         got {with_settings} present and {without_settings} absent"
    );

    for id in [&a, &b, &missing, &unparseable] {
        delete_profile(id).await;
    }
}

/// The two files this surface writes are shared with the Go server, so the bytes
/// Rust *would* write have to be the bytes Go *did* write — otherwise the files
/// churn every time the two implementations take turns.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR"]
async fn the_files_rust_writes_are_the_files_go_wrote() {
    let (_serial, dir) = enter().await;

    // Make Go write both, rather than trusting whatever an earlier case left —
    // this case read its own suite's output the first time it was written.
    let id = create_profile("Index Parity").await;
    let (status, _) = send(
        Method::PUT,
        "/api/claude-settings",
        Some(r#"{"z":1,"a":{"b":[1,2]},"rate":1.50,"tag":"<b>"}"#),
    )
    .await;
    assert_eq!(status, 200);

    let listed = fetch("/api/claude-settings/profiles").await;
    let listed: Vec<profiles::Profile> = serde_json::from_slice(&listed).expect("profiles");
    assert!(!listed.is_empty(), "no profiles to compare");

    let go_index = std::fs::read(format!("{dir}/settings_profiles.json")).expect("the index");
    let native = profiles::encode_index(&listed).expect("encode");
    assert_identical("settings_profiles.json", &go_index, &native);

    // The pretty-printed settings file, re-derived through the whole chain:
    // Go's `any` (key sort, float widening), Go's `Marshal` (HTML escaping,
    // float spelling) and Go's `Indent` (two spaces, `": "`, no trailing
    // newline).
    let go_settings = std::fs::read(format!("{dir}/settings.json")).expect("settings.json");
    let value = match claude_settings::decode_go_any(&go_settings) {
        Decoded::Value(value) => value,
        other => panic!("Go wrote a settings.json this port cannot parse: {other:?}"),
    };
    let native = claude_settings::marshal_indent(&value).expect("marshal");
    assert_identical("settings.json", &go_settings, &native);

    delete_profile(&id).await;
}

// ─── Go's answers for the writes, pinned as literals ──────────────────────────

/// `PUT /api/claude-settings`: the answers, and the file left behind.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR; it writes"]
async fn the_claude_settings_write_answers_match_go() {
    let (_serial, dir) = enter().await;

    for body in ["", "   ", "{not json"] {
        let (status, answer) = send(Method::PUT, "/api/claude-settings", Some(body)).await;
        assert_eq!(status, 400, "body {body:?}");
        assert_eq!(
            String::from_utf8_lossy(&answer).trim_end(),
            r#"{"error":"invalid JSON body"}"#,
            "body {body:?}"
        );
    }

    // Syntactically valid, but no float64 holds it — a *different* 400, raised
    // by the second parse rather than by `Decode`.
    let (status, answer) = send(Method::PUT, "/api/claude-settings", Some(r#"{"n":1e999}"#)).await;
    assert_eq!(status, 400);
    assert_eq!(
        String::from_utf8_lossy(&answer).trim_end(),
        r#"{"error":"invalid JSON settings"}"#
    );

    // Underflow is *not* out of range: Go's `ParseFloat` returns a zero for it,
    // which is the half of the rule a port that leaned on serde_json's own
    // `NumberOutOfRange` would get wrong.
    let (status, answer) = send(
        Method::PUT,
        "/api/claude-settings",
        Some(r#"{"tiny":1e-999}"#),
    )
    .await;
    assert_eq!(
        status, 200,
        "an underflowing number is a zero, not an error"
    );
    assert_eq!(
        String::from_utf8_lossy(&answer).trim_end(),
        r#"{"exists":true,"settings":{"tiny":0}}"#
    );

    // A `Decoder` reads a stream, so bytes after the first value are not looked
    // at. `serde_json::from_slice` would have rejected this.
    let (status, answer) = send(
        Method::PUT,
        "/api/claude-settings",
        Some(r#"{"trailing":true} and then some"#),
    )
    .await;
    assert_eq!(status, 200, "trailing bytes are ignored by a json.Decoder");
    assert_eq!(
        String::from_utf8_lossy(&answer).trim_end(),
        r#"{"exists":true,"settings":{"trailing":true}}"#
    );

    // A scalar is a JSON value, so it is accepted and written as one.
    let (status, answer) = send(Method::PUT, "/api/claude-settings", Some("123")).await;
    assert_eq!(status, 200, "a bare number is a JSON value Go accepts");
    assert_eq!(
        String::from_utf8_lossy(&answer).trim_end(),
        r#"{"exists":true,"settings":123}"#
    );

    // The round trip: keys sorted, `1.50` respelled `1.5`, `<` escaped — in the
    // response *and* in the file, which is two-space indented with no trailing
    // newline.
    let (status, answer) = send(
        Method::PUT,
        "/api/claude-settings",
        Some(r#"{"z":1,"a":{"b":[1,2]},"rate":1.50,"tag":"<b>"}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8_lossy(&answer).trim_end(),
        concat!(
            "{\"exists\":true,\"settings\":{\"a\":{\"b\":[1,2]},\"rate\":1.5,",
            "\"tag\":\"\\u003cb\\u003e\",\"z\":1}}"
        ),
        "the response is compacted, and `<` is escaped on the way out"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{dir}/settings.json")).expect("settings.json"),
        concat!(
            "{\n",
            "  \"a\": {\n",
            "    \"b\": [\n",
            "      1,\n",
            "      2\n",
            "    ]\n",
            "  },\n",
            "  \"rate\": 1.5,\n",
            "  \"tag\": \"\\u003cb\\u003e\",\n",
            "  \"z\": 1\n",
            "}"
        ),
        "the file is json.MarshalIndent's output"
    );

    // Leave something ordinary behind.
    let (status, _) = send(
        Method::PUT,
        "/api/claude-settings",
        Some(r#"{"model":"opus"}"#),
    )
    .await;
    assert_eq!(status, 200);
}

/// Every profile write, and the exact status and body Go answers each with.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR; it writes"]
async fn the_profile_write_answers_match_go() {
    let (_serial, _dir) = enter().await;
    for stale in ["parity-writes", "parity-writes-2", "parity-renamed"] {
        delete_profile(stale).await;
    }

    // ── create ──
    // A decode failure and an empty name are one 400 with one message, because
    // the *handler* tests `err != nil || req.Name == ""` before the service
    // runs — so the service's 422 for an empty name is unreachable, and an array
    // body says `name is required` rather than `invalid JSON body`.
    for body in ["", "[]", r#"["Sneaky"]"#, "{not json", "{}", "null"] {
        let (status, answer) =
            send(Method::POST, "/api/claude-settings/profiles", Some(body)).await;
        assert_eq!(status, 400, "create body {body:?}");
        assert_eq!(
            String::from_utf8_lossy(&answer).trim_end(),
            r#"{"error":"name is required"}"#,
            "create body {body:?}"
        );
    }

    let (status, created) = send(
        Method::POST,
        "/api/claude-settings/profiles",
        Some(r#"{"name":"Parity Writes"}"#),
    )
    .await;
    assert_eq!(status, 201, "create must be 201");
    let profile: serde_json::Value = serde_json::from_slice(&created).expect("json");
    assert_eq!(
        profile["id"], "parity-writes",
        "the id is the slugified name"
    );
    assert_eq!(profile["is_default"], false);
    println!("create: {}", String::from_utf8_lossy(&created));

    // A second profile with the same name is **deduplicated**, not refused —
    // the opposite of what a rename onto the same slug does.
    let (status, again) = send(
        Method::POST,
        "/api/claude-settings/profiles",
        Some(r#"{"name":"Parity Writes"}"#),
    )
    .await;
    assert_eq!(status, 201);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&again).expect("json")["id"],
        "parity-writes-2",
        "create deduplicates the id and keeps the name"
    );

    // ── get ──
    let (status, missing) = send(
        Method::GET,
        "/api/claude-settings/profiles/parity-no-such-profile",
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(
        String::from_utf8_lossy(&missing).trim_end(),
        r#"{"error":"profile \"parity-no-such-profile\" not found"}"#
    );

    // ── update ──
    let (status, bad_body) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-writes",
        Some("{not json"),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        String::from_utf8_lossy(&bad_body).trim_end(),
        r#"{"error":"invalid JSON body"}"#,
        "the update handler uses errInvalidJSONBody, unlike the create handler"
    );

    // An out-of-range number passes `json.Valid` and then fails the parse into
    // `any`: the one reachable 422 on this surface.
    let (status, out_of_range) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-writes",
        Some(r#"{"settings":{"n":1e999}}"#),
    )
    .await;
    assert_eq!(status, 422, "a number out of float64 range is a 422");
    assert_eq!(
        String::from_utf8_lossy(&out_of_range).trim_end(),
        r#"{"error":"validation error for \"settings\": failed to parse settings JSON"}"#
    );

    // A settings write, and the detail it answers with. The keys come back
    // sorted because the file was written through `MarshalIndent` over `any`.
    let (status, updated) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-writes",
        Some(r#"{"settings":{"z":1,"a":{"b":[1,2]}}}"#),
    )
    .await;
    assert_eq!(status, 200);
    let detail: serde_json::Value = serde_json::from_slice(&updated).expect("json");
    assert_eq!(detail["exists"], true);
    assert_eq!(detail["settings"].to_string(), r#"{"a":{"b":[1,2]},"z":1}"#);

    // A literal `null` settings is a no-op, not a clear. Folding it into an
    // absent key — or an absent key into an empty document — would silently
    // erase a profile on a rename-only request.
    let (status, untouched) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-writes",
        Some(r#"{"settings":null}"#),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&untouched).expect("json")["settings"]
            .to_string(),
        r#"{"a":{"b":[1,2]},"z":1}"#,
        "a literal null settings must not clear the file"
    );

    // A rename whose slug collides with **another** profile is a 409. The
    // collision is on the slug, so the names differ while the ids would not.
    let (status, collision) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-writes-2",
        Some(r#"{"name":"PARITY WRITES"}"#),
    )
    .await;
    assert_eq!(status, 409, "a rename collision is 409");
    assert_eq!(
        String::from_utf8_lossy(&collision).trim_end(),
        r#"{"error":"profile with id \"parity-writes\" already exists"}"#
    );

    // A rename that does move: the id and the recorded path follow the slug,
    // and the settings move with them rather than staying at the old path.
    let (status, renamed) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-writes",
        Some(r#"{"name":"Parity Renamed"}"#),
    )
    .await;
    assert_eq!(status, 200);
    let renamed: serde_json::Value = serde_json::from_slice(&renamed).expect("json");
    assert_eq!(renamed["id"], "parity-renamed");
    assert!(renamed["file_path"]
        .as_str()
        .expect("a path")
        .ends_with("/settings_parity-renamed.json"));
    assert_eq!(
        renamed["settings"].to_string(),
        r#"{"a":{"b":[1,2]},"z":1}"#
    );

    let (status, _) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-no-such-profile",
        Some(r#"{"name":"x"}"#),
    )
    .await;
    assert_eq!(status, 404, "update looks the profile up first");

    // ── duplicate ──
    let (status, copied) = send(
        Method::POST,
        "/api/claude-settings/profiles/parity-renamed/duplicate",
        None,
    )
    .await;
    assert_eq!(status, 201, "duplicate must be 201");
    let copy: serde_json::Value = serde_json::from_slice(&copied).expect("json");
    assert_eq!(copy["name"], "Copy of Parity Renamed");
    assert_eq!(copy["id"], "copy-of-parity-renamed");
    assert_eq!(copy["is_default"], false);

    let (status, _) = send(
        Method::POST,
        "/api/claude-settings/profiles/parity-no-such-profile/duplicate",
        None,
    )
    .await;
    assert_eq!(status, 404);

    // ── default ──
    let (status, defaulted) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-renamed/default",
        Some("{}"),
    )
    .await;
    assert_eq!(status, 200, "setting a default must be 200");
    let defaulted: serde_json::Value = serde_json::from_slice(&defaulted).expect("json");
    assert_eq!(defaulted["is_default"], true);

    // The second path trap: it synced `settings.json` in the run config dir,
    // byte for byte from the profile's own file.
    let dir = scratch_claude_dir();
    assert_eq!(
        std::fs::read_to_string(format!("{dir}/settings.json")).expect("settings.json"),
        std::fs::read_to_string(defaulted["file_path"].as_str().expect("path"))
            .expect("profile file"),
        "settings.json must be a byte copy of the new default profile"
    );

    let (status, _) = send(
        Method::PUT,
        "/api/claude-settings/profiles/parity-no-such-profile/default",
        Some("{}"),
    )
    .await;
    assert_eq!(status, 404);

    // ── delete ──
    // The default profile cannot be deleted, and Go says so with a
    // `ConflictError` — whose wording is about existence, not about deletion.
    let (status, refused) = send(
        Method::DELETE,
        "/api/claude-settings/profiles/parity-renamed",
        None,
    )
    .await;
    assert_eq!(status, 409, "deleting the default profile is 409");
    assert_eq!(
        String::from_utf8_lossy(&refused).trim_end(),
        r#"{"error":"profile with id \"parity-renamed\" already exists"}"#
    );

    let (status, gone) = send(
        Method::DELETE,
        "/api/claude-settings/profiles/copy-of-parity-renamed",
        None,
    )
    .await;
    assert_eq!(status, 204, "delete must be 204");
    assert!(gone.is_empty(), "204 carries no body");

    let (status, _) = send(
        Method::DELETE,
        "/api/claude-settings/profiles/parity-no-such-profile",
        None,
    )
    .await;
    assert_eq!(status, 404);

    // Hand the default back and clear this case's rows.
    let (status, _) = send(
        Method::PUT,
        "/api/claude-settings/profiles/default/default",
        Some("{}"),
    )
    .await;
    assert_eq!(status, 200);
    for cleanup in ["parity-renamed", "parity-writes-2"] {
        delete_profile(cleanup).await;
    }
}

// ─── The two places Go's JSON layer is not serde's ────────────────────────────

/// **`encoding/json` is not UTF-8-strict and serde_json is**, and Go's answers
/// here are the ones that make that a divergence rather than a curiosity: it
/// serves invalid bytes verbatim, decodes them into `any` with U+FFFD
/// substituted, and writes the substituted document back. None of that is
/// reproducible in Rust, so every one of these must be a **forward** — the port
/// answering at all would be answering wrongly.
///
/// This is the live half: it asks Go what it really does. The unit tests assert
/// Rust forwards for each of the same inputs.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR; it writes"]
async fn bytes_that_are_not_utf8_get_the_cutover_answers() {
    let (_serial, dir) = enter().await;

    // ── the read of a file Go accepted verbatim: lossy since #278 ──
    let raw: &[u8] = b"{\"model\":\"opus\",\"tag\":\"x\xffy\"}";
    std::fs::write(format!("{dir}/settings.json"), raw).expect("write");

    let go = fetch("/api/claude-settings").await;
    assert!(
        go.windows(2).any(|w| w == b"\xff\"") || String::from_utf8_lossy(&go).contains("\u{fffd}"),
        "Go is expected to ship the stored bytes through its encoder; it sent {}",
        String::from_utf8_lossy(&go)
    );
    // Until #278 the native read forwarded so Go could ship the raw bytes.
    // Now it answers Go's own substitution: U+FFFD, `exists: true`, the
    // document intact.
    let native = claude_settings::get_settings(&dir).expect("lossy native read");
    let native = String::from_utf8(native.body.expect("body")).expect("utf8");
    assert!(
        native.contains("\u{fffd}") && native.contains(r#""exists":true"#),
        "{native}"
    );

    // ── the write Go performed and Rust refuses as a 400 since #278 ──
    let (status, answer) = send_raw(
        Method::PUT,
        "/api/claude-settings",
        "application/json",
        b"{\"tag\":\"x\xffy\"}".to_vec(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "Go substitutes U+FFFD and writes the file: {}",
        String::from_utf8_lossy(&answer)
    );
    assert!(matches!(
        claude_settings::put_settings(&dir, b"{\"tag\":\"x\xffy\"}"),
        Err(agento_lib::native::writes::WriteError::InvalidBody)
    ));

    // ── the same on a profile's own file: lossy, like the unnamed one ──
    let id = create_profile("Parity Not Utf8").await;
    std::fs::write(format!("{dir}/settings_{id}.json"), raw).expect("write");
    let go = fetch(&format!("/api/claude-settings/profiles/{id}")).await;
    assert!(
        String::from_utf8_lossy(&go).contains(r#""exists":true"#),
        "Go's `json.Valid` accepts these bytes, so the detail is `exists: true` \
         with the document — not `settings: null`"
    );
    let native = finish(profiles::get(&dir, &id)).expect("lossy native detail");
    let native = String::from_utf8(native.body.expect("body")).expect("utf8");
    assert!(
        native.contains("\u{fffd}") && native.contains(r#""exists":true"#),
        "{native}"
    );
    delete_profile(&id).await;

    // Leave the shared dir holding something ordinary.
    let (status, _) = send(
        Method::PUT,
        "/api/claude-settings",
        Some(r#"{"model":"opus"}"#),
    )
    .await;
    assert_eq!(status, 200);
}

/// **`json.Decoder.Decode` enforces `encoding/json`'s 10000-level cap**, even
/// inside a field the request struct ignores — so a 10001-deep body never
/// reaches `req.Name` and the create handler answers `400 name is required`.
///
/// serde routes an unknown field to `IgnoredAny`, whose skip is iterative and
/// counts no depth, so this decoded with `name == "x"` and answered **201**:
/// a profile file written and an index entry appended for a request Go refuses.
/// The live half is here because the claim is about Go's scanner, not ours.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR; it writes"]
async fn a_body_deeper_than_gos_scanner_creates_nothing() {
    let (_serial, dir) = enter().await;

    let body = format!(
        r#"{{"name":"Parity Too Deep","junk":{}{}}}"#,
        "[".repeat(10000),
        "]".repeat(10000)
    );
    let (status, answer) = send(Method::POST, "/api/claude-settings/profiles", Some(&body)).await;
    assert_eq!(status, 400, "Go's Decode stops at 10000 levels");
    assert_eq!(
        String::from_utf8_lossy(&answer).trim_end(),
        r#"{"error":"name is required"}"#,
        "the decode fails, so the handler sees an empty name"
    );
    assert!(
        !std::path::Path::new(&format!("{dir}/settings_parity-too-deep.json")).exists(),
        "Go wrote no profile file, so neither may the port"
    );

    // The port answers the same 400 — and, crucially, writes nothing either.
    let native = native_body(
        "create too deep",
        finish(profiles::create(&dir, body.as_bytes())),
        400,
    );
    assert_identical("create too deep", &answer, &native);
    assert!(!std::path::Path::new(&format!("{dir}/settings_parity-too-deep.json")).exists());

    // One level shallower is a real create in both, which is what makes the
    // assertion above about the *cap* rather than about deep bodies generally.
    let shallower = format!(
        r#"{{"name":"Parity Deep Enough","junk":{}{}}}"#,
        "[".repeat(9999),
        "]".repeat(9999)
    );
    let (status, _) = send(
        Method::POST,
        "/api/claude-settings/profiles",
        Some(&shallower),
    )
    .await;
    assert_eq!(status, 201, "10000 levels is inside Go's cap");
    delete_profile("parity-deep-enough").await;
}

// ─── The cache question, reproduced rather than reasoned about ────────────────

/// **The evidence behind claiming the writes.**
///
/// The issue warned that `ClaudeSettingsProfileService` caches the index, which
/// would leave the sidecar — still the side that runs agents and resolves
/// `--settings` on every turn — reading a stale copy after a native write.
///
/// It does not. This writes `settings_profiles.json` *underneath a running Go
/// server*, with this port's own encoder and no Go request in between, and then
/// asks that same server. If the service held the index in memory the rename
/// would be invisible to it; every method calls `config.LoadProfilesMetadata`,
/// which is an `os.ReadFile` per call, so it is not.
///
/// The reads before the write are deliberate: a cache that was never populated
/// would pass this for the wrong reason.
#[tokio::test]
#[ignore = "needs a running parity instance and a scratch CLAUDE_CONFIG_DIR; it writes"]
async fn a_native_write_is_visible_to_the_go_server_immediately() {
    let (_serial, dir) = enter().await;

    let id = create_profile("Cache Evidence").await;

    let before = fetch("/api/claude-settings/profiles").await;
    assert!(
        String::from_utf8_lossy(&before).contains(&format!("\"{id}\"")),
        "the profile this case created is not in the list"
    );
    let _ = fetch(&format!("/api/claude-settings/profiles/{id}")).await;

    let marker = "Renamed Behind The Server's Back";
    let mut listed: Vec<profiles::Profile> = serde_json::from_slice(&before).expect("profiles");
    for profile in &mut listed {
        if profile.id == id {
            profile.name = marker.to_string();
        }
    }
    std::fs::write(
        format!("{dir}/settings_profiles.json"),
        profiles::encode_index(&listed).expect("encode"),
    )
    .expect("writing the index");

    let after = fetch("/api/claude-settings/profiles").await;
    assert!(
        String::from_utf8_lossy(&after).contains(marker),
        "the Go server did not see a write to settings_profiles.json — it caches \
         the index after all, and these writes must not be claimed.\n{}",
        String::from_utf8_lossy(&after)
    );

    // The per-profile read goes through the same load, so check that too.
    let detail = fetch(&format!("/api/claude-settings/profiles/{id}")).await;
    let detail: serde_json::Value = serde_json::from_slice(&detail).expect("json");
    assert_eq!(detail["name"], marker);

    delete_profile(&id).await;
}
