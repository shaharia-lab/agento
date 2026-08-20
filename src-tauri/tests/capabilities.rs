//! Guards on `capabilities/default.json`.
//!
//! Every bug this file has produced fails the same way: the ACL denies an IPC
//! command, the frontend's wrapper swallows the rejection, and the feature is
//! dead with no error anywhere. Nothing here can be caught by `cargo build` —
//! the capability is valid JSON and a valid ACL in every broken state below —
//! so these assertions are the only thing standing between a one-word edit and
//! a silently dead feature in a shipped build.

use serde_json::Value;

const CAPABILITY: &str = include_str!("../capabilities/default.json");

fn capability() -> Value {
    serde_json::from_str(CAPABILITY).expect("capabilities/default.json parses")
}

/// Permission identifiers granted as bare strings.
fn granted(cap: &Value) -> Vec<String> {
    cap["permissions"]
        .as_array()
        .expect("permissions is an array")
        .iter()
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect()
}

/// True when the capability carries an inline scope object for `identifier`
/// — the `{"identifier": "…", "allow": [{"url": "…"}]}` form, which grants a
/// scope without naming a scope permission.
fn has_inline_scope(cap: &Value, identifier: &str) -> bool {
    cap["permissions"]
        .as_array()
        .expect("permissions is an array")
        .iter()
        .any(|p| {
            p.get("identifier").and_then(Value::as_str) == Some(identifier)
                && p.get("allow")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
        })
}

/// The bug this test exists for: `opener:allow-open-url` enables the command
/// but — in the plugin's own words — "without any pre-configured scope". The
/// scope entries live in the *separate* `opener:allow-default-urls` permission
/// that `opener:default` bundles, and `open_url` checks the scope before doing
/// anything (`is_url_allowed` ends in `.any()`, so an empty allow list refuses
/// every URL). Granting the command alone therefore makes every external link
/// in the app fail with `Not allowed to open url …` — OAuth "Authorize", PR
/// links, release links and every link in chat markdown alike. It failed that
/// way from the first desktop commit until this test was added, because
/// `openExternal` catches the rejection, logs a `console.warn` and returns
/// normally, so the UI advances as if a browser had opened.
#[test]
fn granting_open_url_also_grants_a_url_scope() {
    let cap = capability();
    let perms = granted(&cap);

    if !perms.iter().any(|p| p == "opener:allow-open-url") {
        return; // command not granted at all: nothing to scope
    }

    let scoped = perms
        .iter()
        .any(|p| p == "opener:allow-default-urls" || p == "opener:default")
        || has_inline_scope(&cap, "opener:allow-open-url");

    assert!(
        scoped,
        "capabilities/default.json grants `opener:allow-open-url` with no URL \
         scope, so every open_url call is refused with `Not allowed to open \
         url …` and every external link in the app is silently dead. Add \
         `opener:allow-default-urls` (mailto/tel/http/https), or an inline \
         `allow` scope, beside it."
    );
}

/// The #385 class, in the same file and with the same silence: a release
/// window is navigated to `http://127.0.0.1:<port>` so the UI is same-origin
/// with the API, and Tauri classifies that origin as *remote*. A capability
/// with no `remote.urls` covering it is local-only, which denies every IPC
/// command in shipped builds while dev keeps working — invisible to
/// `npm run app` and `app:alongside` by construction.
#[test]
fn the_loopback_release_origin_is_in_the_remote_scope() {
    let cap = capability();
    let urls: Vec<&str> = cap["remote"]["urls"]
        .as_array()
        .expect("capabilities/default.json has a remote.urls block")
        .iter()
        .filter_map(Value::as_str)
        .collect();

    for host in ["127.0.0.1", "localhost"] {
        assert!(
            urls.iter().any(|u| u.contains(host)),
            "remote.urls does not cover http://{host}:<port>, the origin a \
             release window is navigated to — every IPC command would be \
             denied in shipped builds while dev keeps working. urls: {urls:?}"
        );
    }
}
