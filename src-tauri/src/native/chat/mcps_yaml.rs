//! `<data dir>/mcps.yaml` — the hand-authored registry of **external** MCP
//! servers. Ports `LoadMCPRegistry` / `convertMCPEntry` / `interpolateEnv`
//! (`internal/config/mcp.go`).
//!
//! # What an entry is, and what it is not
//!
//! Everything in this file describes **somebody else's server**: a subprocess
//! the CLI spawns, or a URL it dials. Nothing here is hosted, health-checked or
//! shut down by Agento — [`crate::claude::mcp`]'s header already draws that
//! line, and this module stays on the config side of it. The desktop app has no
//! UI for the file; it is read because a user who authored one for the retired
//! `agento web` server points the app at the same data directory, and because
//! it is the only way to reach an MCP server that is not one of the six
//! integration types.
//!
//! # The three shapes, and the names that do not match
//!
//! The YAML `transport` key and the `type` the CLI is handed are **different
//! spellings**, and only one of the three agrees:
//!
//! | `transport:` | `type` on the wire | struct |
//! |---|---|---|
//! | `stdio` | `stdio` | [`McpStdioServer`] |
//! | `streamable_http` | `http` | [`McpHttpServer`] |
//! | `sse` | `sse` | [`McpSseServer`] |
//!
//! Anything else is an error naming the server, which refuses the turn. Go's
//! `convertMCPEntry` does the same and its wording is reproduced.
//!
//! # A missing file is an empty registry; a broken one is an error
//!
//! `LoadMCPRegistry` answers `os.IsNotExist` with an empty registry and **no**
//! error — the overwhelmingly common case — while an unreadable or unparseable
//! file is an error carrying the path. That asymmetry is the whole reason
//! [`load`] returns `Result<Registry, String>` rather than an `Option`: "there
//! is no registry" and "the registry could not be read" must not collapse into
//! one answer, or a typo in a YAML file silently removes every external server
//! an agent depends on.
//!
//! # `null` is the zero value — except inside a sequence, where it is not
//!
//! A YAML key written with nothing under it (`env:`, `args:`) **is** a null,
//! which is a far more ordinary thing to type than a literal `null` in JSON ever
//! was, so this file needs the rule [`crate::native::gojson::GoMap`] carries for
//! the JSON side. But `yaml.v3` and `encoding/json` **disagree one level down**,
//! and only measuring says which way:
//!
//! | | `env: {A: }` | `args: ["--f", ~]` |
//! |---|---|---|
//! | `encoding/json` / `GoList` | `A: ""` | `["--f", ""]` |
//! | `yaml.v3` | `A: ""` | **`["--f"]`** |
//!
//! `d.sequence` only appends an element when `d.unmarshal` reports it decoded
//! something, and a null reports nothing — so the element is **dropped**, not
//! zero-filled. That is `args`, i.e. an external server's argv: getting it wrong
//! spawns `docs-mcp --f ""` where Go spawned `docs-mcp --f`. So `env` and
//! `headers` use `GoMap` and `args` deliberately does not use `GoList`.
//!
//! # Merge keys are resolved, because `yaml.v3` resolves them
//!
//! `<<: *defaults` is the natural way to share an `env` block across several
//! entries, and `yaml.v3` expands it during decode. `serde_norway` has to be
//! asked ([`serde_norway::Value::apply_merge`]) and otherwise treats `<<` as an
//! unknown key — which would leave `transport` empty and refuse the turn with
//! *"unknown transport"* over a file that is perfectly valid. That is the same
//! user-visible failure Part A of #375 removed, so it is not a nicety.
//!
//! # No decode failure quotes what it was decoding
//!
//! `mcps.yaml` holds credentials — a `Bearer` token under `headers` is the
//! ordinary case — and a refusal's text becomes a chat 500 body, a stored
//! `job_history.error_message` and a line in the exported app log. So a serde
//! message never reaches any of them: a syntax error reports its **location**
//! and a shape error reports the **server name**, which is what a reader
//! searches the file for anyway. `native/integrations/registry.rs` established
//! this rule for integration credentials and the reasoning is identical. (Go's
//! own message truncates the value to eight characters; dropping it entirely is
//! strictly safer and loses nothing a line number does not supply.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::claude::options::{McpHttpServer, McpSseServer, McpStdioServer};
use crate::native::gojson::GoMap;

/// How many `${ENV:…}` substitutions one value may take before the expansion is
/// declared circular.
///
/// Go's loop rescans the **whole** string from the start after each
/// substitution, so an environment variable whose value contains the pattern is
/// expanded again — and `FOO=${ENV:FOO}` therefore spins forever, in Go too. A
/// hang inside `build_options` is a chat that never answers and a scheduled run
/// that never records a row, which is strictly worse than the divergence, so the
/// loop is bounded and the bound is an error. Every terminating input behaves
/// exactly as Go's does: a value needs one iteration per placeholder, and no
/// legitimate one is anywhere near this many.
const MAX_SUBSTITUTIONS: usize = 1000;

/// Where the registry file lives.
///
/// `AppConfig.MCPsFile()` is `<data dir>/mcps.yaml` and `cmd/web.go` passed it
/// with no override — the `--mcps-file` flag existed only on `agento ask`. The
/// override here is this build's equivalent of that flag: the same lever
/// `AGENTO_CLAUDE_EXECUTABLE` is for the CLI binary, and the only way to point a
/// test at a fixture without writing into the developer's real data directory
/// (a debug build's [`crate::paths::data_dir`] ignores the environment by
/// design).
///
/// **Two spellings, and the prefixed one is the real name.** Every variable
/// Agento *invents* is `AGENTO_`-prefixed — `AGENTO_DATA_DIR`,
/// `AGENTO_CLAUDE_EXECUTABLE`, `AGENTO_PUBLIC_URL`; the unprefixed ones it reads
/// (`CLAUDE_CONFIG_DIR`, `TZ`) are somebody else's. A bare `MCPS_FILE` would be
/// a new claim on an unqualified name in every user's environment, so
/// `AGENTO_MCPS_FILE` wins. `MCPS_FILE` is honoured behind it because #375's
/// acceptance criteria name it and this file's documentation shipped with it;
/// dropping it would be a change to a published lever rather than a rename
/// before anyone could depend on it.
pub fn path() -> Option<PathBuf> {
    for name in ["AGENTO_MCPS_FILE", "MCPS_FILE"] {
        if let Ok(file) = std::env::var(name) {
            if !file.is_empty() {
                return Some(PathBuf::from(file));
            }
        }
    }
    crate::paths::data_dir().map(|dir| dir.join("mcps.yaml"))
}

/// One entry as it is written in the file, before the transport decides which
/// SDK shape it becomes. `rawMCPEntry`, field for field.
///
/// Every field is an `Option` so that a key present with nothing under it — the
/// natural way to leave a list or a map empty in YAML — decodes to the zero
/// value rather than failing the whole file. See the module header.
#[derive(Debug, Default, Deserialize)]
struct RawEntry {
    #[serde(default)]
    transport: Option<YamlString>,
    #[serde(default)]
    command: Option<YamlString>,
    /// `Vec<Option<_>>` and **not** `GoList`: a null element is *dropped* by
    /// `yaml.v3`, where `GoList` would zero-fill it. See the module header —
    /// this is argv, so the difference is an extra empty argument.
    #[serde(default)]
    args: Option<Vec<Option<YamlString>>>,
    #[serde(default)]
    env: Option<GoMap<YamlString>>,
    #[serde(default)]
    url: Option<YamlString>,
    #[serde(default)]
    headers: Option<GoMap<YamlString>>,
}

/// A string field that accepts **any** scalar, because `yaml.v3` does.
///
/// `d.scalar` sets a `string` field from the node's own text whatever the node
/// resolved to, so `env: {PORT: 8080}`, `command: 123` and `DEBUG: true` all
/// decode with no error — measured against v3.0.1. `serde` type-checks instead
/// and rejects every one of them.
///
/// **That is not a wording difference, it is Part A's regression re-entered
/// through the parser.** [`super::runner::mcp_plan`] loads the registry for
/// *any* agent naming *any* MCP server, so one unquoted port number in a
/// leftover file would refuse every MCP-backed agent on the machine — including
/// agents whose every name resolves to a hosted integration and which have no
/// entry in that file at all. An unquoted scalar is the most ordinary thing
/// there is to write in YAML.
///
/// **One residual divergence, measured and accepted.** `yaml.v3` keeps the
/// node's *raw text*; this sees the value after `serde_norway` has resolved it,
/// because [`serde_norway::Value::apply_merge`] forces a `Value` round trip
/// before any field is read. So a spelling that does not survive that round trip
/// does not survive here: `1.50` is `"1.5"` and `2.0` is `"2"` where Go says
/// `"1.50"` and `"2.0"`. Integers, booleans and strings — which is everything
/// anyone writes in this file — are exact, and the alternative is dropping merge
/// keys, which breaks a whole valid idiom to fix a trailing zero. Pinned by
/// `a_bare_scalar_is_read_as_its_text_the_way_yaml_v3_reads_one`.
#[derive(Clone, Debug, Default, PartialEq)]
struct YamlString(String);

impl<'de> Deserialize<'de> for YamlString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AnyScalar;

        impl serde::de::Visitor<'_> for AnyScalar {
            type Value = YamlString;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a scalar")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<YamlString, E> {
                Ok(YamlString(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<YamlString, E> {
                Ok(YamlString(v.to_string()))
            }
        }

        deserializer.deserialize_any(AnyScalar)
    }
}

/// The parsed registry: server name → the JSON the CLI is handed in
/// `--mcp-config`.
///
/// The values are already `serde_json::Value` rather than an enum over the three
/// structs, because that is the only thing any caller wants from them — the
/// conversion happens once, at load, so a `transport` typo is reported when the
/// file is read rather than when a turn happens to name that server.
#[derive(Default)]
pub struct Registry {
    servers: BTreeMap<String, serde_json::Value>,
}

/// **Hand-written, and it prints the server names without their configs** —
/// `runner::McpSource`'s rule, applied to the type that holds the secret first.
///
/// [`convert`] runs [`interpolate_map`] before the value is stored, so every
/// entry here holds the **resolved** `${ENV:…}` values: the live
/// `Authorization: Bearer …` the file only pointed at. `integrations/registry.rs`
/// states the rule twice — a `{row:?}` in a log line is the same leak as a
/// response field, only later — and this struct is `pub`, so it is the one most
/// likely to be formatted by something written later.
impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("servers", &self.servers.keys())
            .finish()
    }
}

impl Registry {
    /// `GetSDKConfig`: the config for `name`, or `None` when the file does not
    /// name it.
    pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
        self.servers.get(name)
    }

    /// How many servers the file named. Only the tests care.
    #[cfg(test)]
    fn count(&self) -> usize {
        self.servers.len()
    }
}

/// `LoadMCPRegistry`: read `path` and convert every entry.
///
/// `None` — no data directory to resolve — is an empty registry rather than an
/// error, matching the missing-file case: neither is a registry that could not
/// be read, and refusing a turn over a home directory this process could not
/// find would be a strictly worse answer than the one the integrations already
/// give for it.
pub fn load(path: Option<&Path>) -> Result<Registry, String> {
    let Some(path) = path else {
        return Ok(Registry::default());
    };
    let label = path.display().to_string();
    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Registry::default()),
        Err(e) => return Err(format!("reading MCP registry {label:?}: {e}")),
    };
    parse(&label, &data)
}

/// The half of [`load`] that has no filesystem in it, so a fixture is a `&str`.
///
/// `label` is the path, and it appears **only** in the parse failure — which is
/// where Go puts it. A `convertMCPEntry` failure is returned from
/// `LoadMCPRegistry` unwrapped, naming the server rather than the file, because
/// the server name is what a reader searches the file for.
fn parse(label: &str, data: &str) -> Result<Registry, String> {
    let mut root: serde_norway::Value =
        serde_norway::from_str(data).map_err(|e| syntax_error(label, &e))?;

    // An **empty** file is a YAML null, and so is one holding nothing but `---`
    // or a comment. Go's `yaml.Unmarshal` leaves the destination map nil for all
    // three and returns no error, and an empty `mcps.yaml` left behind by an
    // earlier experiment is exactly the kind of file #375 exists to stop
    // breaking turns.
    if root.is_null() {
        return Ok(Registry::default());
    }
    // `yaml.v3` expands `<<: *anchor` during decode; this has to be asked. The
    // anchor's own entry stays in the map and is a server like any other, which
    // is also what Go does with it.
    root.apply_merge().map_err(|_| {
        // Its own sentence has no position and no user content, but it is a
        // serde message and this module does not forward those. `<<` is the only
        // thing `apply_merge` can fail on, so naming it is the whole diagnosis.
        format!("parsing MCP registry {label:?}: a merge key (`<<`) does not name a mapping")
    })?;

    let raw: BTreeMap<String, serde_norway::Value> =
        serde_norway::from_value(root).map_err(|_| {
            format!(
                "parsing MCP registry {label:?}: the top level is not a mapping of \
             server name to configuration"
            )
        })?;

    let mut servers = BTreeMap::new();
    for (name, value) in raw {
        // The serde message is deliberately dropped — it quotes the offending
        // value, and a `headers` written as a string instead of a map is a
        // `Bearer` token. The server name is what a reader searches for.
        let entry: RawEntry = serde_norway::from_value(value).map_err(|_| {
            format!(
                "MCP server {name:?}: the entry does not decode — it must be a mapping, \
                 with args a list and env and headers mappings"
            )
        })?;
        let config = convert(&name, entry)?;
        servers.insert(name, config);
    }
    Ok(Registry { servers })
}

/// A parse failure reported by **position**, never by content.
///
/// See the module header: this text reaches a chat body, a stored
/// `job_history.error_message` and the app log, and `serde_norway` prints the
/// whole offending scalar.
fn syntax_error(label: &str, e: &serde_norway::Error) -> String {
    match e.location() {
        Some(at) => format!(
            "parsing MCP registry {label:?}: does not decode at line {} column {}",
            at.line(),
            at.column()
        ),
        None => format!("parsing MCP registry {label:?}: does not decode"),
    }
}

/// `convertMCPEntry`: one raw entry to the SDK config its transport names.
fn convert(name: &str, entry: RawEntry) -> Result<serde_json::Value, String> {
    let transport = entry.transport.unwrap_or_default().0;
    let value = match transport.as_str() {
        "stdio" => serde_json::to_value(McpStdioServer {
            server_type: "stdio".to_string(),
            command: entry.command.unwrap_or_default().0,
            // `flatten` is the null-element drop `d.sequence` performs.
            args: entry
                .args
                .unwrap_or_default()
                .into_iter()
                .flatten()
                .map(|arg| arg.0)
                .collect(),
            env: interpolate_map(name, entry.env)?,
        }),
        "streamable_http" => serde_json::to_value(McpHttpServer {
            server_type: "http".to_string(),
            url: entry.url.unwrap_or_default().0,
            headers: interpolate_map(name, entry.headers)?,
        }),
        "sse" => serde_json::to_value(McpSseServer {
            server_type: "sse".to_string(),
            url: entry.url.unwrap_or_default().0,
            headers: interpolate_map(name, entry.headers)?,
        }),
        other => {
            return Err(format!(
                "MCP server {name:?}: unknown transport {other:?} \
                 (must be stdio, streamable_http, or sse)"
            ))
        }
    };
    value.map_err(|e| format!("MCP server {name:?}: encoding the server config: {e}"))
}

/// `interpolateEnvMap`: `${ENV:VAR}` substitution over every value of a map.
fn interpolate_map(
    name: &str,
    map: Option<GoMap<YamlString>>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (key, value) in map.map(|m| m.0).unwrap_or_default() {
        let expanded =
            interpolate(&value.0).map_err(|e| format!("MCP server {name:?} key {key:?}: {e}"))?;
        out.insert(key, expanded);
    }
    Ok(out)
}

/// `interpolateEnv`: replace every `${ENV:VAR}` with that variable's value.
///
/// Three details are Go's and each is easy to "improve" into a divergence:
///
/// - **An unset *or empty* variable is an error.** Go tests `value == ""`, so
///   `FOO=` and an absent `FOO` are the same refusal — deliberate, because a
///   silently empty credential in an MCP server's environment is the failure
///   this check exists to catch.
/// - **An unterminated `${ENV:` is left alone**, not an error: the scan stops at
///   the missing `}` and returns what it has.
/// - **The scan restarts from the beginning after each substitution**, so a
///   substituted value containing the pattern is expanded in turn. See
///   [`MAX_SUBSTITUTIONS`] for the one input where that does not terminate.
fn interpolate(value: &str) -> Result<String, String> {
    const OPEN: &str = "${ENV:";
    let mut result = value.to_string();
    for _ in 0..MAX_SUBSTITUTIONS {
        let Some(start) = result.find(OPEN) else {
            return Ok(result);
        };
        let Some(end) = result[start..].find('}').map(|at| at + start) else {
            return Ok(result);
        };
        let var = &result[start + OPEN.len()..end];
        let replacement = std::env::var(var).unwrap_or_default();
        if replacement.is_empty() {
            return Err(format!("required env var {var:?} is not set"));
        }
        result = format!("{}{replacement}{}", &result[..start], &result[end + 1..]);
    }
    // The value is **not** interpolated into this: it is raw file content, which
    // is what the module's no-echo rule is about, and `interpolate_map` has
    // already named the server and the key it is under.
    Err(format!(
        "more than {MAX_SUBSTITUTIONS} `${{ENV:…}}` substitutions, which almost \
         certainly means an environment variable expands to itself"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::tests::{env_lock, EnvVar};

    /// The three transports, at the bytes the CLI is handed.
    ///
    /// Pinned as whole JSON documents rather than field by field: the `type`
    /// values are not the `transport` values, the keys are the SDK's spellings
    /// and not the YAML's, and an empty `args`/`env`/`headers` is **absent**
    /// rather than `[]`/`{}` — four independent ways to be quietly wrong that a
    /// per-field assertion would each miss on its own.
    #[test]
    fn the_three_transports_reach_the_cli_in_the_sdk_shapes() {
        let _env = env_lock();
        let _token = EnvVar::set("AGENTO_TEST_MCPS_TOKEN", "s3cret");
        let registry = parse(
            "mcps.yaml",
            r#"
docs:
  transport: stdio
  command: /usr/bin/docs-mcp
  args: ["--root", "/srv/docs"]
  env:
    LOG: debug
    TOKEN: "Bearer ${ENV:AGENTO_TEST_MCPS_TOKEN}"
weather:
  transport: streamable_http
  url: https://weather.example/mcp
  headers:
    X-Key: "${ENV:AGENTO_TEST_MCPS_TOKEN}"
ticker:
  transport: sse
  url: https://ticker.example/sse
"#,
        )
        .expect("a well-formed registry");
        assert_eq!(registry.count(), 3);

        assert_eq!(
            registry.get("docs").expect("docs"),
            &serde_json::json!({
                "type": "stdio",
                "command": "/usr/bin/docs-mcp",
                "args": ["--root", "/srv/docs"],
                "env": {"LOG": "debug", "TOKEN": "Bearer s3cret"},
            })
        );
        // `streamable_http` in the file is `http` on the wire. The two spellings
        // are not interchangeable and only this transport differs.
        assert_eq!(
            registry.get("weather").expect("weather"),
            &serde_json::json!({
                "type": "http",
                "url": "https://weather.example/mcp",
                "headers": {"X-Key": "s3cret"},
            })
        );
        // No `headers` key at all, because the SDK struct skips an empty map —
        // `{"headers":{}}` would be a different `--mcp-config` argument.
        assert_eq!(
            registry.get("ticker").expect("ticker"),
            &serde_json::json!({"type": "sse", "url": "https://ticker.example/sse"})
        );
        assert!(registry.get("absent").is_none());
    }

    /// A key written with nothing under it is a YAML null, which is how a person
    /// actually leaves a section empty — and it is the zero value, not a type
    /// error.
    ///
    /// **One level down the two encodings part company, and this pins the
    /// difference**: a null *map value* is `""` and a null *sequence element* is
    /// **dropped**. Both measured against `gopkg.in/yaml.v3` v3.0.1 rather than
    /// inferred from `encoding/json`, which zero-fills both — using `GoList`
    /// here would put an extra empty argument in an external server's argv.
    #[test]
    fn a_null_is_the_zero_value_at_the_field_and_one_level_down() {
        let registry = parse(
            "mcps.yaml",
            r#"
bare:
  transport: stdio
  command: run-me
  args:
  env:
holes:
  transport: stdio
  command: run-me-too
  args: ["--first", ~]
  env:
    EMPTY:
"#,
        )
        .expect("nulls decode");
        assert_eq!(
            registry.get("bare").expect("bare"),
            &serde_json::json!({"type": "stdio", "command": "run-me"}),
            "a null list and a null map are absent, not `[]` and `{{}}`"
        );
        assert_eq!(
            registry.get("holes").expect("holes"),
            &serde_json::json!({
                "type": "stdio",
                "command": "run-me-too",
                "args": ["--first"],
                "env": {"EMPTY": ""},
            }),
            "a null argument is dropped, not sent as an empty one"
        );
    }

    /// An unquoted scalar is a **string**, because `yaml.v3` reads a `string`
    /// field from the node's own text whatever it resolved to.
    ///
    /// Rejecting these would be Part A's regression re-entered through the
    /// parser: the registry is loaded for *any* agent naming *any* MCP server,
    /// so one unquoted port number in a leftover file would refuse every
    /// MCP-backed agent on the machine. Every row here was measured against
    /// `gopkg.in/yaml.v3` v3.0.1 — including the last two, which are the
    /// accepted divergence: `serde_norway` has already resolved the scalar by
    /// the time this sees it, because `apply_merge` forces a `Value` round trip.
    #[test]
    fn a_bare_scalar_is_read_as_its_text_the_way_yaml_v3_reads_one() {
        let registry = parse(
            "mcps.yaml",
            r#"
a:
  transport: stdio
  command: 123
  args: [--port, 8080, --ratio, 1.5, --on, true]
  env:
    PORT: 8080
    DEBUG: true
    RATE: 1.5
    YES: yes
"#,
        )
        .expect("bare scalars are strings");
        assert_eq!(
            registry.get("a").expect("a"),
            &serde_json::json!({
                "type": "stdio",
                "command": "123",
                "args": ["--port", "8080", "--ratio", "1.5", "--on", "true"],
                // `yes` is a string to both — `yaml.v3` dropped YAML 1.1's
                // boolean spellings, and serde_norway never had them.
                "env": {"DEBUG": "true", "PORT": "8080", "RATE": "1.5", "YES": "yes"},
            })
        );

        // The residual divergence, pinned rather than reconciled: `yaml.v3`
        // keeps the node's raw text (`"0x10"`, `"1.50"`, `"2.0"`), and a
        // resolved scalar has lost it. Reconciling would mean giving up
        // `apply_merge`, which breaks a whole valid idiom to fix a trailing
        // zero.
        let registry = parse(
            "mcps.yaml",
            "a:\n  transport: stdio\n  env:\n    H: 0x10\n    T: 1.50\n    Z: 2.0\n",
        )
        .expect("resolved scalars");
        assert_eq!(
            registry.get("a").expect("a")["env"],
            serde_json::json!({"H": "16", "T": "1.5", "Z": "2"})
        );
    }

    /// `<<: *anchor` is how a person shares an `env` block across entries, and
    /// `yaml.v3` expands it during decode.
    ///
    /// Without `apply_merge` this file decodes with `transport` unset and the
    /// turn is refused with *"unknown transport"* — a valid registry breaking
    /// every MCP-backed agent, which is the failure #375 exists to remove.
    #[test]
    fn a_merge_key_is_expanded_the_way_yaml_v3_expands_it() {
        let registry = parse(
            "mcps.yaml",
            r#"
base: &base
  transport: stdio
  command: /usr/bin/shared
docs:
  <<: *base
  args: ["--docs"]
"#,
        )
        .expect("merge keys resolve");
        assert_eq!(
            registry.get("docs").expect("docs"),
            &serde_json::json!({
                "type": "stdio",
                "command": "/usr/bin/shared",
                "args": ["--docs"],
            })
        );
        // The anchor's own entry stays and is a server like any other — Go's
        // map holds it too, so it is not filtered out here either.
        assert_eq!(
            registry.get("base").expect("base"),
            &serde_json::json!({"type": "stdio", "command": "/usr/bin/shared"})
        );
    }

    /// **No refusal quotes what it was decoding.** A `Bearer` token under
    /// `headers` is the ordinary content of this file, and a refusal's text
    /// becomes a chat body, a stored `job_history.error_message` and a line in
    /// the app log the docs tell users to attach to a bug report.
    ///
    /// Asserted on the token's *absence* rather than on the wording, which is
    /// the only form that keeps holding when a message is reworded.
    #[test]
    fn a_decode_failure_never_echoes_the_value_it_failed_on() {
        const SECRET: &str = "sk-live-NOTAREALTOKEN";

        // A shape error: `headers` written as a string rather than a mapping.
        let err = parse(
            "mcps.yaml",
            &format!(
                "weather:\n  transport: streamable_http\n  url: https://w.example\n  \
                 headers: \"Bearer {SECRET}\"\n"
            ),
        )
        .expect_err("headers is not a mapping");
        assert!(!err.contains(SECRET), "the value leaked: {err}");
        assert!(err.contains(r#"MCP server "weather""#), "{err}");

        // A syntax error, which is reported by position instead.
        let err = parse(
            "mcps.yaml",
            &format!("weather:\n  url: \"Bearer {SECRET}\n"),
        )
        .expect_err("unterminated quote");
        assert!(!err.contains(SECRET), "the value leaked: {err}");
        assert!(err.contains("at line"), "{err}");

        // And the registry that succeeds is the one that actually holds the
        // resolved token, so its own `Debug` must withhold it — the same rule as
        // `runner::McpSource`, one struct earlier in the chain.
        let _env = env_lock();
        let _token = EnvVar::set("AGENTO_TEST_MCPS_REGISTRY_TOKEN", SECRET);
        let registry = parse(
            "mcps.yaml",
            "weather:\n  transport: streamable_http\n  url: https://w.example\n  \
             headers:\n    Authorization: \"Bearer ${ENV:AGENTO_TEST_MCPS_REGISTRY_TOKEN}\"\n",
        )
        .expect("resolvable");
        assert_eq!(
            registry.get("weather").expect("weather")["headers"]["Authorization"],
            format!("Bearer {SECRET}"),
            "the turn really does run with the resolved token"
        );
        let rendered = format!("{registry:?}");
        assert!(!rendered.contains(SECRET), "Debug leaked it: {rendered}");
        assert!(
            rendered.contains("weather"),
            "the names are still useful: {rendered}"
        );
    }

    /// A file with nothing in it is an empty registry and **no** error. This is
    /// the shape Part A of #375 is about: an unrelated leftover file must not
    /// refuse a turn.
    #[test]
    fn an_empty_or_commentary_file_is_an_empty_registry() {
        for data in ["", "\n", "---\n", "# nothing here yet\n", "null\n"] {
            let registry =
                parse("mcps.yaml", data).unwrap_or_else(|e| panic!("{data:?} should parse: {e}"));
            assert_eq!(registry.count(), 0, "{data:?}");
        }
    }

    /// A missing file is an empty registry; anything else about the file is an
    /// error naming it, so a typo cannot read as "no external servers".
    #[test]
    fn a_missing_file_is_empty_and_a_broken_one_names_itself() {
        assert_eq!(load(None).expect("no path").count(), 0);

        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("mcps.yaml");
        assert_eq!(load(Some(&missing)).expect("missing").count(), 0);

        std::fs::write(&missing, "docs: [not, a, mapping]\n").expect("write");
        let err = load(Some(&missing)).expect_err("malformed");
        assert!(err.contains(r#"MCP server "docs""#), "{err}");

        std::fs::write(&missing, "- not\n- a\n- mapping\n").expect("write");
        let err = load(Some(&missing)).expect_err("not a mapping at all");
        assert!(err.contains("parsing MCP registry"), "{err}");
        assert!(
            err.contains(&missing.display().to_string()),
            "the message has to name the file: {err}"
        );
    }

    /// An unknown transport is Go's sentence, naming the server and the value.
    #[test]
    fn an_unknown_transport_names_the_server() {
        let err =
            parse("mcps.yaml", "docs:\n  transport: websocket\n").expect_err("unknown transport");
        assert_eq!(
            err,
            "MCP server \"docs\": unknown transport \"websocket\" \
             (must be stdio, streamable_http, or sse)"
        );
        // A missing `transport` is the same refusal with the empty string in it,
        // because Go's switch has no separate arm for it.
        let err = parse("mcps.yaml", "docs:\n  command: x\n").expect_err("no transport");
        assert!(err.contains(r#"unknown transport """#), "{err}");
    }

    /// `${ENV:…}` substitution, including the two Go behaviours that read as
    /// bugs: an empty variable is "not set", and an unterminated pattern is left
    /// alone rather than refused.
    #[test]
    fn env_interpolation_follows_gos_rules() {
        let _env = env_lock();
        let _set = EnvVar::set("AGENTO_TEST_MCPS_A", "alpha");
        let _blank = EnvVar::set("AGENTO_TEST_MCPS_BLANK", "");
        let _unset = EnvVar::unset("AGENTO_TEST_MCPS_MISSING");

        assert_eq!(interpolate("plain").expect("no pattern"), "plain");
        assert_eq!(
            interpolate("a-${ENV:AGENTO_TEST_MCPS_A}-b-${ENV:AGENTO_TEST_MCPS_A}").expect("twice"),
            "a-alpha-b-alpha"
        );
        // Unterminated: the scan stops, the text survives verbatim.
        assert_eq!(
            interpolate("${ENV:AGENTO_TEST_MCPS_A").expect("unterminated"),
            "${ENV:AGENTO_TEST_MCPS_A"
        );
        assert_eq!(
            interpolate("${ENV:AGENTO_TEST_MCPS_MISSING}").expect_err("unset"),
            r#"required env var "AGENTO_TEST_MCPS_MISSING" is not set"#
        );
        // Set but empty is the same refusal — Go compares against `""`, not
        // against presence.
        assert!(interpolate("${ENV:AGENTO_TEST_MCPS_BLANK}").is_err());
    }

    /// Go's rescan-from-the-start expands a substituted value in turn — and
    /// spins forever on one that expands to itself. The bound turns that hang
    /// into an error; every terminating input is unaffected.
    #[test]
    fn a_self_referential_variable_is_an_error_rather_than_a_hang() {
        let _env = env_lock();
        let _outer = EnvVar::set("AGENTO_TEST_MCPS_OUTER", "${ENV:AGENTO_TEST_MCPS_INNER}");
        let _inner = EnvVar::set("AGENTO_TEST_MCPS_INNER", "done");
        assert_eq!(
            interpolate("${ENV:AGENTO_TEST_MCPS_OUTER}").expect("nested expands"),
            "done"
        );

        let _loop = EnvVar::set("AGENTO_TEST_MCPS_LOOP", "${ENV:AGENTO_TEST_MCPS_LOOP}");
        let err = interpolate("${ENV:AGENTO_TEST_MCPS_LOOP}").expect_err("circular");
        assert!(err.contains("expands to itself"), "{err}");
    }

    /// A failed substitution names the server **and** the key it was under, so
    /// the message points at a line of the file.
    #[test]
    fn a_failed_substitution_names_the_server_and_the_key() {
        let _env = env_lock();
        let _unset = EnvVar::unset("AGENTO_TEST_MCPS_MISSING");
        let err = parse(
            "mcps.yaml",
            "docs:\n  transport: stdio\n  command: x\n  env:\n    TOKEN: \"${ENV:AGENTO_TEST_MCPS_MISSING}\"\n",
        )
        .expect_err("unset variable");
        assert!(err.contains(r#"MCP server "docs" key "TOKEN""#), "{err}");
        assert!(err.contains("is not set"), "{err}");
    }

    /// The override selects the file, the prefixed spelling wins the tie, and an
    /// empty value is not a path.
    #[test]
    fn the_environment_override_selects_the_file() {
        let _env = env_lock();
        let default = crate::paths::data_dir().map(|dir| dir.join("mcps.yaml"));

        {
            let _unprefixed = EnvVar::set("MCPS_FILE", "/tmp/legacy.yaml");
            let _none = EnvVar::unset("AGENTO_MCPS_FILE");
            assert_eq!(path(), Some(PathBuf::from("/tmp/legacy.yaml")));

            // #375's own spelling still works, but `AGENTO_`-prefixed is the
            // name and takes precedence when both are set.
            let _prefixed = EnvVar::set("AGENTO_MCPS_FILE", "/tmp/current.yaml");
            assert_eq!(path(), Some(PathBuf::from("/tmp/current.yaml")));
        }

        let _a = EnvVar::unset("AGENTO_MCPS_FILE");
        let _b = EnvVar::unset("MCPS_FILE");
        assert_eq!(path(), default);

        let _empty = EnvVar::set("AGENTO_MCPS_FILE", "");
        assert_eq!(path(), default, "an empty value is not a path");
    }
}
