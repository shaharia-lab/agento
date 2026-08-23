//! `GET /api/monitoring` — the OpenTelemetry configuration the settings page
//! renders, and which of its fields the environment has pinned.
//!
//! **This serves the read, not the telemetry.** Agento does not run OTel
//! providers and is not going to: OpenTelemetry and Prometheus are
//! infrastructure concerns, and this is a local desktop app. What it does have
//! is a settings page that shows the stored configuration, so the endpoint
//! behind that page has to answer. Everything here is `monitoring.json` plus
//! `OTEL_*`; nothing here exports anything.
//!
//! # The two writes answer 501, and that is the decision (#309)
//!
//! `PUT /api/monitoring` and `POST /api/monitoring/test` decline. The
//! alternatives are worse, not that this one is cheap:
//!
//! - **Persisting the config without exporters is a save that changes nothing.**
//!   Writing `monitoring.json` is only half of it; the other half is rebuilding
//!   the providers, which this build does not have. A 200 would tell the user
//!   telemetry is on while nothing is emitted.
//! - **Implementing the exporters** is a large piece of work that reverses the
//!   decision above.
//!
//! So the honest answer is the one that does not claim a reload happened. `501`
//! rather than `404`, because the route exists and this build declines it —
//! a 404 would read as a version mismatch and send someone looking for an
//! upgrade that will never ship, exactly as `unavailableCopy` avoids for
//! WhatsApp.
//!
//! **`GET /api/monitoring` stays.** It reports what `monitoring.json` holds and
//! which `OTEL_*` variables are pinning fields, both of which are still true:
//! the file is read at startup and the variables still lock fields. The
//! settings page renders it read-only and says why.
//!
//! The two env-override mechanisms in Agento are not the same shape and this is
//! the one that returns 409: monitoring answers a conflicting write with an
//! `EnvLockedError`, while `UserSettings` carries a `locked` map and answers
//! with a 400. Both surfaces expose a `locked` map, which is why they look
//! alike from here — but only this one also carries `env_locked`.

use std::collections::BTreeMap;
use std::path::Path;

use axum::http::Method;
use serde::{Deserialize, Serialize};

/// The config as it travels. Mirrors `api.MonitoringConfigDTO`, whose field
/// order is the key order on the wire.
///
/// The interval is milliseconds rather than a `time.Duration` on purpose: Go's
/// duration marshals as a nanosecond count, which is not a number any UI wants
/// to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonitoringConfigDto {
    pub enabled: bool,
    pub metrics_exporter: String,
    pub logs_exporter: String,
    pub otlp_endpoint: String,
    /// `omitempty`, so an install with no headers omits the key entirely rather
    /// than sending `{}`. A `BTreeMap` because Go marshals map keys sorted.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub otlp_headers: BTreeMap<String, String>,
    pub otlp_insecure: bool,
    pub metric_export_interval_ms: i64,
}

/// The envelope. Mirrors `api.MonitoringResponse`.
#[derive(Debug, Clone, Serialize)]
pub struct MonitoringResponse {
    pub settings: MonitoringConfigDto,
    /// Field name → the `OTEL_*` variable pinning it. Never nil on the Go side,
    /// so an unpinned install ships `{}`.
    pub locked: BTreeMap<String, String>,
    pub env_locked: bool,
}

/// `telemetry.DefaultMonitoringConfig`: disabled, no exporters, one minute.
fn default_config() -> MonitoringConfigDto {
    MonitoringConfigDto {
        enabled: false,
        metrics_exporter: "none".to_string(),
        logs_exporter: "none".to_string(),
        otlp_endpoint: String::new(),
        otlp_headers: BTreeMap::new(),
        otlp_insecure: false,
        metric_export_interval_ms: 60_000,
    }
}

/// `telemetry.envVarChecks`, in the map's key order — which is also the order
/// the `locked` map ships in, since Go sorts map keys on marshal.
const ENV_VAR_CHECKS: &[(&str, &str)] = &[
    ("enabled", "OTEL_SDK_DISABLED"),
    ("logs_exporter", "OTEL_LOGS_EXPORTER"),
    ("metric_export_interval", "OTEL_METRIC_EXPORT_INTERVAL"),
    ("metrics_exporter", "OTEL_METRICS_EXPORTER"),
    ("otlp_endpoint", "OTEL_EXPORTER_OTLP_ENDPOINT"),
    ("otlp_headers", "OTEL_EXPORTER_OTLP_HEADERS"),
    ("otlp_insecure", "OTEL_EXPORTER_OTLP_INSECURE"),
];

/// The persisted file. Mirrors `telemetry.monitoringConfigStore`.
///
/// Every field is an `Option` so a stored `null` decodes to the zero value
/// instead of failing, which is what Go's decoder does. A *type* mismatch still
/// fails the decode — also Go's behaviour, and the whole file then degrades to
/// the default, because `MonitoringManager.Load` returns the error and
/// `initMonitoringManager` logs it and leaves `current` at the env config.
#[derive(Debug, Default, Deserialize)]
struct StoredMonitoring {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    metrics_exporter: Option<String>,
    #[serde(default)]
    logs_exporter: Option<String>,
    #[serde(default)]
    otlp_endpoint: Option<String>,
    #[serde(default)]
    otlp_headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    otlp_insecure: Option<bool>,
    #[serde(default)]
    metric_export_interval_ms: Option<i64>,
}

impl StoredMonitoring {
    /// `telemetry.storeToConfig`.
    ///
    /// The two exporter names are passed through **unvalidated**: the file is
    /// only validated on the way in (`validateMonitoringDTO` on `PUT`), so a
    /// hand-edited `"metrics_exporter": "bogus"` is what `GET` returns. Parsing
    /// it here would answer with something the Go server does not.
    fn into_dto(self) -> MonitoringConfigDto {
        let default = default_config();
        MonitoringConfigDto {
            enabled: self.enabled.unwrap_or(false),
            metrics_exporter: self.metrics_exporter.unwrap_or_default(),
            logs_exporter: self.logs_exporter.unwrap_or_default(),
            otlp_endpoint: self.otlp_endpoint.unwrap_or_default(),
            otlp_headers: self.otlp_headers.unwrap_or_default(),
            otlp_insecure: self.otlp_insecure.unwrap_or(false),
            // A non-positive interval is "unset", not "export continuously".
            metric_export_interval_ms: match self.metric_export_interval_ms.unwrap_or(0) {
                ms if ms > 0 => ms,
                _ => default.metric_export_interval_ms,
            },
        }
    }
}

/// `telemetry.ConfigFromEnv`.
///
/// Note what it does *not* do: an `OTEL_*` variable that is set but names
/// nothing meaningful — `OTEL_SDK_DISABLED=false` alone, say — still locks every
/// field, but leaves the config at its default. The lock and the value are two
/// different questions.
fn config_from_env() -> MonitoringConfigDto {
    let cfg = default_config();

    let sdk_disabled = env_raw("OTEL_SDK_DISABLED");
    if sdk_disabled == "true" || sdk_disabled == "1" {
        return cfg;
    }

    let metrics = env_raw("OTEL_METRICS_EXPORTER").trim().to_ascii_lowercase();
    let logs = env_raw("OTEL_LOGS_EXPORTER").trim().to_ascii_lowercase();
    let endpoint = env_raw("OTEL_EXPORTER_OTLP_ENDPOINT").trim().to_string();

    if metrics.is_empty() && logs.is_empty() && endpoint.is_empty() {
        return cfg;
    }

    let insecure = env_raw("OTEL_EXPORTER_OTLP_INSECURE");
    MonitoringConfigDto {
        enabled: true,
        metrics_exporter: match metrics.as_str() {
            "otlp" => "otlp",
            "prometheus" => "prometheus",
            _ => "none",
        }
        .to_string(),
        logs_exporter: if logs == "otlp" { "otlp" } else { "none" }.to_string(),
        otlp_endpoint: endpoint,
        otlp_headers: parse_otlp_headers(&env_raw("OTEL_EXPORTER_OTLP_HEADERS")),
        otlp_insecure: insecure == "true" || insecure == "1",
        metric_export_interval_ms: match env_raw("OTEL_METRIC_EXPORT_INTERVAL").parse::<i64>() {
            Ok(ms) if ms > 0 => ms,
            _ => cfg.metric_export_interval_ms,
        },
    }
}

/// `telemetry.parseOTLPHeaders`: `key=value,key2=value2`. A pair with no `=`,
/// or one whose `=` opens it, is dropped rather than stored under an empty key.
fn parse_otlp_headers(raw: &str) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    if raw.is_empty() {
        return headers;
    }
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        match pair.find('=') {
            Some(idx) if idx > 0 => {
                headers.insert(
                    pair[..idx].trim().to_string(),
                    pair[idx + 1..].trim().to_string(),
                );
            }
            _ => continue,
        }
    }
    headers
}

fn env_raw(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// The answer `GET /api/monitoring` gives, resolved the way the manager does.
///
/// **The environment wins outright.** `MonitoringManager.Load` returns before
/// touching the file when any `OTEL_*` variable is set, so a stored config is
/// not merged with the environment — it is ignored. Reproducing that matters
/// more than it looks: the two configurations routinely disagree, and merging
/// would show the user a page that describes neither.
pub fn response(data_dir: &Path) -> MonitoringResponse {
    let locked: BTreeMap<String, String> = ENV_VAR_CHECKS
        .iter()
        .filter(|(_, var)| !env_raw(var).is_empty())
        .map(|(field, var)| (field.to_string(), var.to_string()))
        .collect();
    let env_locked = !locked.is_empty();

    let settings = if env_locked {
        config_from_env()
    } else {
        load_file(&data_dir.join("monitoring.json"))
    };

    MonitoringResponse {
        settings,
        locked,
        env_locked,
    }
}

/// Read `monitoring.json`. A missing file, an unreadable one and a malformed
/// one all resolve to the default, because that is where `current` is left in
/// each case — the manager seeds it from the env config (which, with no
/// variables set, *is* the default) and only replaces it on a clean load.
fn load_file(path: &Path) -> MonitoringConfigDto {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!("native monitoring: reading {}: {e}", path.display());
            }
            return default_config();
        }
    };
    match serde_json::from_slice::<StoredMonitoring>(&raw) {
        Ok(stored) => stored.into_dto(),
        Err(e) => {
            log::warn!("native monitoring: parsing {}: {e}", path.display());
            default_config()
        }
    }
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "monitoring",
    claims,
    serve,
};

/// What both declined routes say. One sentence, and it says what *is* true
/// rather than only refusing — the stored configuration is still readable, so
/// the reader is not left thinking the settings page is broken.
///
/// It used to name a second Agento to go and run instead. There isn't one: the
/// server was deleted (#391) and Agento is this app. A refusal that points at
/// something that does not exist is worse than a plain refusal.
const DECLINED: &str = "Agento does not export telemetry; \
the stored configuration is shown read-only and cannot be changed here";

fn claims(method: &Method, path: &str) -> bool {
    match path {
        "/api/monitoring" => method == Method::GET || method == Method::PUT,
        "/api/monitoring/test" => method == Method::POST,
        // `/metrics` is the read half of the same declined feature (#278). It
        // is mounted at the root, outside `/api`, and the write-routes audit is
        // writes-only by design — so nothing had decided it until the cut-over
        // forced the question. Dropping it *deliberately* rather than letting
        // it 404: a 404 reads as a version mismatch, where the truth is that
        // this build exports no telemetry — the same reasoning as the two
        // 501'd writes above, so it lives with them.
        "/metrics" => method == Method::GET,
        _ => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    if req.method != Method::GET || req.path == "/metrics" {
        // Claimed so the decline is answered deliberately. `finish` renders it
        // as `{"error": …}`, the shape every other failure in the API uses, so
        // the settings page needs no special case.
        return super::writes::finish(Err(super::writes::WriteError::NotImplemented(
            DECLINED.to_string(),
        )));
    }
    // The data dir is the database's parent by construction — `paths::data_dir`
    // joins `agento.db` onto it, and `paths::tests::database_sits_beside_the_data_dir`
    // pins that. It is also what makes the parity instance work: the same
    // derivation points both the database read and this file read at its
    // scratch copy.
    let data_dir = ctx
        .db_path
        .parent()
        .ok_or("no data directory beside the database")?;
    let body = super::gojson::to_vec(&response(data_dir))
        .map_err(|e| format!("encoding monitoring config: {e}"))?;
    Ok(super::Answer::json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private directory per test. Not a fixed path under the system temp
    /// dir: two agents running `cargo test` in separate worktrees is a
    /// supported arrangement here, and a shared path would have each run
    /// deleting the other's fixture mid-test.
    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn write(dir: &tempfile::TempDir, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("monitoring.json");
        std::fs::write(&path, body).expect("write");
        path
    }

    #[test]
    fn an_absent_file_is_the_default_config() {
        let dir = scratch();
        assert_eq!(
            load_file(&dir.path().join("monitoring.json")),
            default_config()
        );
    }

    /// The failure the manager swallows: it logs and leaves `current` where the
    /// env config put it, so a corrupt file reads as "not configured" rather
    /// than taking the endpoint down.
    #[test]
    fn a_malformed_file_degrades_to_the_default() {
        let dir = scratch();
        assert_eq!(load_file(&write(&dir, "{not json")), default_config());
    }

    /// A stored `null` is not a malformed file — Go decodes it into the zero
    /// value without complaint, and so must this.
    #[test]
    fn stored_nulls_decode_to_zero_values() {
        let dir = scratch();
        let path = write(
            &dir,
            r#"{"enabled":null,"metrics_exporter":null,"metric_export_interval_ms":null}"#,
        );
        let dto = load_file(&path);
        assert!(!dto.enabled);
        assert_eq!(dto.metrics_exporter, "");
        assert_eq!(dto.metric_export_interval_ms, 60_000);
    }

    /// A non-positive interval means "unset". Zero is what a file written
    /// before the field existed carries.
    #[test]
    fn a_non_positive_interval_falls_back_to_a_minute() {
        let dir = scratch();
        let zero = write(&dir, r#"{"enabled":true,"metric_export_interval_ms":0}"#);
        assert_eq!(load_file(&zero).metric_export_interval_ms, 60_000);
        let negative = write(&dir, r#"{"enabled":true,"metric_export_interval_ms":-5}"#);
        assert_eq!(load_file(&negative).metric_export_interval_ms, 60_000);
    }

    /// An exporter name the UI would never write is passed through, because
    /// validation happens on `PUT` and `GET` reports what is stored.
    #[test]
    fn an_unknown_exporter_name_is_not_corrected() {
        let dir = scratch();
        let path = write(
            &dir,
            r#"{"metrics_exporter":"bogus","logs_exporter":"otlp"}"#,
        );
        let dto = load_file(&path);
        assert_eq!(dto.metrics_exporter, "bogus");
        assert_eq!(dto.logs_exporter, "otlp");
    }

    /// The resolution rule, not just the file read: with nothing pinned, the
    /// stored config *is* the answer, `locked` is empty and `env_locked` is
    /// false. The precedence in the other direction — any `OTEL_*` set means the
    /// file is ignored rather than merged — cannot be asserted here without
    /// mutating process-global state that every other test shares, so it is
    /// covered by `tests/parity_settings.rs`, which diffs an env-locked instance
    /// against Go.
    #[test]
    fn an_unpinned_install_answers_with_the_stored_config() {
        if ENV_VAR_CHECKS
            .iter()
            .any(|(_, var)| !env_raw(var).is_empty())
        {
            // This shell exports an OTEL_* variable, so the not-env-locked path
            // is unreachable. Skipping beats asserting the wrong branch.
            return;
        }

        let dir = scratch();
        write(
            &dir,
            r#"{"enabled":true,"metrics_exporter":"prometheus","logs_exporter":"none",
                "otlp_endpoint":"collector:4317","otlp_headers":{"x-key":"v"},
                "otlp_insecure":true,"metric_export_interval_ms":15000}"#,
        );

        let answer = response(dir.path());
        assert!(!answer.env_locked);
        assert!(answer.locked.is_empty());
        assert!(answer.settings.enabled);
        assert_eq!(answer.settings.metrics_exporter, "prometheus");
        assert_eq!(answer.settings.otlp_endpoint, "collector:4317");
        assert_eq!(answer.settings.metric_export_interval_ms, 15_000);
        assert_eq!(
            answer
                .settings
                .otlp_headers
                .get("x-key")
                .map(String::as_str),
            Some("v")
        );
    }

    /// An install that has never opened the monitoring page has no file at all,
    /// and that is the shape the endpoint answers with most often.
    #[test]
    fn an_install_with_no_file_answers_with_the_default() {
        if ENV_VAR_CHECKS
            .iter()
            .any(|(_, var)| !env_raw(var).is_empty())
        {
            return;
        }
        let dir = scratch();
        let answer = response(dir.path());
        assert_eq!(answer.settings, default_config());
        assert!(!answer.env_locked);
        assert!(answer.locked.is_empty());
    }

    #[test]
    fn headers_parse_the_way_the_env_var_is_written() {
        assert!(parse_otlp_headers("").is_empty());
        let headers = parse_otlp_headers("x-api-key=secret, x-tenant = acme ,,broken,=novalue");
        assert_eq!(headers.get("x-api-key").map(String::as_str), Some("secret"));
        assert_eq!(headers.get("x-tenant").map(String::as_str), Some("acme"));
        assert_eq!(headers.len(), 2, "{headers:?}");
    }

    /// Empty headers are omitted, not sent as `{}` — the field carries
    /// `omitempty` and that is one of the four ways a port drifts.
    #[test]
    fn the_response_shape_is_the_go_envelope() {
        let body = super::super::gojson::to_vec(&MonitoringResponse {
            settings: default_config(),
            locked: BTreeMap::new(),
            env_locked: false,
        })
        .expect("encode");

        assert_eq!(
            String::from_utf8(body).expect("utf8"),
            concat!(
                r#"{"settings":{"enabled":false,"metrics_exporter":"none","#,
                r#""logs_exporter":"none","otlp_endpoint":"","otlp_insecure":false,"#,
                r#""metric_export_interval_ms":60000},"locked":{},"env_locked":false}"#,
                "\n"
            )
        );
    }

    #[test]
    fn populated_headers_are_sent_with_sorted_keys() {
        let mut settings = default_config();
        settings.otlp_headers = parse_otlp_headers("z-last=2,a-first=1");
        let body = super::super::gojson::to_vec(&settings).expect("encode");
        assert!(String::from_utf8(body)
            .expect("utf8")
            .contains(r#""otlp_headers":{"a-first":"1","z-last":"2"}"#));
    }

    #[test]
    fn the_read_and_both_declined_writes_are_claimed() {
        assert!(claims(&Method::GET, "/api/monitoring"));
        // Claimed so this build answers them rather than a sidecar that is
        // going away — see the module header.
        assert!(claims(&Method::PUT, "/api/monitoring"));
        assert!(claims(&Method::POST, "/api/monitoring/test"));

        assert!(!claims(&Method::GET, "/api/monitoring/test"));
        assert!(!claims(&Method::DELETE, "/api/monitoring"));
        assert!(!claims(&Method::GET, "/api/monitoring/"));
        assert!(!claims(&Method::PUT, "/api/monitoring/test"));
    }

    /// A declined route must be **answered**, not forwarded: forwarding would
    /// reach the sidecar, which would happily save the config and reload its
    /// own providers — the outcome this decision exists to stop.
    #[test]
    fn the_writes_answer_501_rather_than_forwarding() {
        let ctx = super::super::Ctx {
            db_path: std::path::PathBuf::from("/nonexistent/agento.db"),
        };
        for (method, path) in [
            (Method::PUT, "/api/monitoring"),
            (Method::POST, "/api/monitoring/test"),
        ] {
            let answer = serve(
                &ctx,
                &super::super::Request {
                    method: &method,
                    path,
                    query: "",
                    content_type: "application/json",
                    secret_token: "",
                    body: br#"{"enabled":true,"otlp_endpoint":"localhost:4317"}"#,
                },
            )
            .expect("answered here, not forwarded");
            assert_eq!(answer.status, axum::http::StatusCode::NOT_IMPLEMENTED);
            let body = String::from_utf8(answer.body.expect("body")).expect("utf-8");
            assert_eq!(
                body,
                format!("{{\"error\":\"{DECLINED}\"}}\n"),
                "the shape is the API's ordinary error envelope"
            );
        }
    }

    /// The database path is nonsense in the test above on purpose: a declined
    /// route must not need one. The read still does.
    #[test]
    fn the_read_is_untouched_by_the_decision() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = super::super::Ctx {
            db_path: dir.path().join("agento.db"),
        };
        let answer = serve(
            &ctx,
            &super::super::Request {
                method: &Method::GET,
                path: "/api/monitoring",
                query: "",
                content_type: "",
                secret_token: "",
                body: &[],
            },
        )
        .expect("the read still answers");
        assert_eq!(answer.status, axum::http::StatusCode::OK);
    }
}
