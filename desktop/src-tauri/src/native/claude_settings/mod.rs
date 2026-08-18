//! Claude Code's own `settings.json`, and the named profiles beside it.
//!
//! `internal/api/claude_settings.go`, `internal/api/claude_settings_profiles.go`,
//! `internal/service/claude_settings_profile_service.go` and
//! `internal/config/profiles.go`, in Rust. Nine routes, all of them ported
//! together — see [the cache question](#the-cache-question) below.
//!
//! # This is filesystem state, not SQLite
//!
//! Every other ported area reads rows. This one reads and writes files in a
//! directory Claude Code owns, so the two things that decide correctness are the
//! *path* and the *bytes*, and both are silent when wrong: a settings file
//! written to the wrong dir is not an error, it is a run that quietly gets no
//! settings (#242), and a profile file written with the wrong indentation is a
//! diff in the user's own editor.
//!
//! ## The dir is the run default, and only the fallback follows it
//!
//! [`run_dir`] is `config.ResolveAgentClaudeDir(nil)` — `CLAUDE_CONFIG_DIR`,
//! else the stored global setting, else `~/.claude`. Profiles are a **global**
//! CRUD surface, so they live there rather than in any per-agent override.
//!
//! But that only decides where a *new* file is created. **A named profile keeps
//! the absolute path recorded in `settings_profiles.json`**; only the unnamed
//! fallback (`<dir>/settings.json`) is derived from the dir on every read. So
//! moving the config dir must not silently repoint a named profile, and
//! `profiles::detail_body` reads `file_path` verbatim rather than rebuilding it
//! from the id. `a_named_profile_keeps_its_recorded_path` pins that.
//!
//! ## The bytes
//!
//! Three Go encodings meet here and they are not the same one:
//!
//! - **On the wire**, a `json.RawMessage` field is re-emitted through Go's
//!   `compact` — whitespace stripped, `<`/`>`/`&` escaped, **key order and
//!   number spelling preserved**. So a stored settings file is served with the
//!   keys the user typed, in the order they typed them
//!   ([`super::gojson::compact`]).
//! - **On disk**, every file this surface writes goes through
//!   `json.MarshalIndent(v, "", "  ")` after a round trip through Go's `any` —
//!   which sorts object keys, respells every number as a float64 and escapes
//!   HTML. [`marshal_indent`] is that pair, and
//!   [`super::gojson::indent_compact`] is the `Indent` half.
//! - **The profile file created by `POST .../profiles`** is neither: it is the
//!   current default profile's bytes copied **verbatim**. Reformatting it would
//!   be a diff on a file the user did not ask to change.
//!
//! ## …and the bytes are not always UTF-8
//!
//! `encoding/json` never rejects a document for its bytes and serde_json does,
//! which is not a boundary disagreement but a different answer to "is this
//! JSON". Until #278 every path that parses or re-emits bytes here guarded
//! with [`is_utf8`] and **forwarded**, deferring to Go's answer. With the
//! sidecar gone the split is: a *file* that is not UTF-8 is decoded lossily —
//! the U+FFFD substitution is Go's own answer, so a hand-corrupted
//! settings.json still renders — while a request *body* that is not UTF-8 is
//! a 400, because the app's own requests are always UTF-8.
//!
//! # The cache question
//!
//! The issue that asked for this port warned that `ClaudeSettingsProfileService`
//! caches `settings_profiles.json` in memory, which would leave the sidecar —
//! still the side that *runs agents* and resolves `--settings` on every turn —
//! reading a stale index after a native write.
//!
//! **It does not cache.** `claudeSettingsProfileService` holds one field, a
//! `*slog.Logger`; every one of its seven methods calls
//! `config.LoadProfilesMetadata()`, which is an `os.ReadFile` per call. The
//! agent runner is the same: `appendSettingsOpts` calls
//! `config.LoadProfileFilePathIn`, which re-reads the file at the moment the run
//! starts. There is no in-memory copy for a native write to invalidate, and
//! reproduced evidence for that is in `tests/parity_claude_settings.rs`
//! (`a_native_write_is_visible_to_the_go_server_immediately`), which writes the
//! metadata file underneath a *running* Go server and then asks it.
//!
//! What Go does hold in memory is `config.claudeDirs`, the snapshot deciding
//! *which* dir this is — and that is a different question, answered the way
//! [`super::settings`] already answers it: by re-reading the settings row, which
//! is the authority the snapshot itself is installed from.

pub mod profiles;

use std::io;
use std::path::Path;

use axum::http::Method;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

use super::writes::{finish, WriteError};
use super::{gojson, gopath};

// ─── Paths ────────────────────────────────────────────────────────────────────

/// `config.ClaudeSettingsDirPath` → `config.ClaudeRunConfigDir`: the single dir
/// a run targets when no agent overrides it.
///
/// Read from the settings row rather than from a startup snapshot, for the
/// reason [`super::settings`] documents: a ported handler has no startup wiring
/// to hook into, and the row is what the snapshot is installed from.
pub fn run_dir(db_path: &Path) -> Result<String, String> {
    let conn = super::db::open_read_only(db_path)?;
    let stored = super::settings::load_stored(&conn);
    Ok(super::settings::run_config_dir(&stored.claude_config_dir))
}

/// `config.ClaudeSettingsJSONPathIn`.
pub fn settings_json_path(dir: &str) -> String {
    gopath::join(&[dir, "settings.json"])
}

/// `config.ClaudeSettingsProfilesPath`.
pub fn profiles_path(dir: &str) -> String {
    gopath::join(&[dir, "settings_profiles.json"])
}

// ─── Filesystem, with Go's modes ──────────────────────────────────────────────

/// `os.WriteFile(path, data, 0600)`.
///
/// The mode matters: these files carry API keys and hook commands, and
/// `std::fs::write` would create them `0666 & ~umask`.
///
/// **Truncate-then-write, not temp-file-then-rename**, and that is a choice.
/// `os.WriteFile` truncates, so a crash between the truncate and the write
/// leaves an empty file — and for `settings_profiles.json` an empty index
/// orphans every profile. An atomic replace would be strictly better *and*
/// byte-identical in final content, so it costs no parity. It is not done here
/// because the crash window belongs to the Go original too: fixing it in one
/// implementation and not the other means the desktop app and `agento web`
/// survive a power cut differently, which is a worse thing to debug than the
/// window itself. It is on the upstream list in `desktop/CLAUDE.md`.
pub fn write_file(path: &str, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)
}

/// `os.MkdirAll(dir, 0700)`.
pub fn mkdir_all(dir: &str) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

// ─── Go's `any`, and the four ways its parse can end ──────────────────────────

/// `encoding/json`'s `maxNestingDepth`. Its scanner refuses anything deeper, so
/// `json.Valid` says **false** for a 10001-level document — and so does
/// `json.Decoder.Decode`, which drives the same scanner.
const GO_MAX_NESTING_DEPTH: usize = 10000;

/// Whether `src` is UTF-8, which is the one place Go's JSON layer and serde's
/// disagree about *validity* rather than about a boundary.
///
/// `encoding/json` never rejects a document for its bytes. `json.Valid` says
/// true for `{"a":"x\xffy"}`, `Unmarshal` into `any` succeeds and substitutes
/// U+FFFD, `MarshalIndent` writes the replacement character, and the encoder
/// hands a `json.RawMessage` straight through byte for byte — all four verified
/// against the Go toolchain. serde_json splits: its `ignore_str` does **not**
/// validate, so [`go_json_valid`] agrees, but `parse_str` does, so every parse
/// that actually materializes the string fails.
///
/// Left unguarded that produced five *wrong answers* rather than five forwards —
/// a 400 where Go writes the file and answers 200, a seeded default profile
/// whose bytes are not Go's, and a `settings` key silently missing from a 200.
/// So every path that is about to parse or re-emit bytes checks this first and
/// forwards, which is the only answer this port can be sure of: reproducing
/// Go's substitution would mean reproducing where `encoding/json` puts the
/// replacement character, and that is a guess.
pub fn is_utf8(src: &[u8]) -> bool {
    std::str::from_utf8(src).is_ok()
}

/// The `Fallback`/`Undecidable` reason every [`is_utf8`] guard reports.
fn not_utf8_reason(what: &str) -> String {
    format!("{what} is not UTF-8; Go substitutes U+FFFD and serde refuses to parse")
}

/// `json.Valid`: one complete JSON value, trailing whitespace allowed and
/// nothing else.
///
/// Two things about the implementation are deliberate:
///
/// - **`IgnoredAny` rather than `Value`.** Its skip is iterative, so
///   `serde_json`'s 128-level recursion limit — which *does* stop a `Value`
///   decode — never applies. That matters, because a nesting depth Go accepts
///   must not read here as an invalid file: it would turn a 200 into a 500 on
///   `GET /api/claude-settings` and an `exists: true` into an `exists: false` on
///   a profile.
/// - **The depth is then checked by hand**, because having no limit at all is
///   the *other* wrong answer — Go's scanner stops at 10000 and a document past
///   it is invalid to Go.
pub fn go_json_valid(src: &[u8]) -> bool {
    if nesting_depth_exceeds(src, GO_MAX_NESTING_DEPTH) {
        return false;
    }
    let mut de = serde_json::Deserializer::from_slice(src);
    IgnoredAny::deserialize(&mut de)
        .and_then(|_| de.end())
        .is_ok()
}

/// Whether any point in `src` nests deeper than `limit` containers.
///
/// A byte scan rather than a parse: this runs on input that may not be valid
/// JSON at all, and an unbalanced closer simply cannot take the depth below
/// zero.
fn nesting_depth_exceeds(src: &[u8], limit: usize) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in src {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// `serde_json`'s recursion limit, told apart from a real syntax error.
///
/// Matched on the message because `Category` does not distinguish it: the
/// alternative is `disable_recursion_limit`, which trades a wrong answer for a
/// stack overflow. Pinned by `a_document_deeper_than_serdes_limit_forwards`.
fn is_recursion_limit(e: &serde_json::Error) -> bool {
    e.to_string().contains("recursion limit exceeded")
}

/// What `json.NewDecoder(body).Decode(&raw json.RawMessage)` followed by
/// `json.Unmarshal(raw, &pretty any)` produces.
#[derive(Debug)]
pub enum Decoded {
    /// A value Go's `any` can hold, with every number already widened to
    /// float64 the way `encoding/json` widens them.
    Value(Value),
    /// `Decode` failed — no JSON value at the start of the body, or no body at
    /// all. Both handlers answer 400 for this.
    NotJson,
    /// `Decode` succeeded (a `json.RawMessage` accepts any syntactically valid
    /// value) but `Unmarshal` into `any` did not, because
    /// `strconv.ParseFloat` reports a number out of float64's range. A
    /// *different* 400 message, and on the profile path a 422 — so the two
    /// cases cannot be collapsed.
    NumberOutOfRange,
    /// Neither parser is the authority. Forward.
    Undecidable(String),
}

/// `json.NewDecoder(body).Decode(&raw json.RawMessage)`: the syntax half.
///
/// A `Decoder` reads a **stream**, so it stops at the end of the first value and
/// ignores whatever follows — `{"a":1}trailing` is a successful decode in Go.
/// `serde_json::from_slice` would reject it, which is why this drives the
/// `Deserializer` by hand and never calls `end()`.
///
/// **It returns the first value's bytes**, and that is not a convenience. The
/// caller's next step is `json.Unmarshal(raw, &any)` over exactly what `Decode`
/// captured, so a port that handed it `body` again would be scanning bytes Go
/// never looked at: `{"a":1} 1e999` is a 200 in Go and was a
/// `400 invalid JSON settings` here, because [`numbers_fit_float64`] read the
/// whole slice while the decode read only the head. One value in, one value on.
///
/// `Err(None)` is Go's decode error; `Err(Some(reason))` is a document this
/// port's parser will not judge.
pub fn decode_stream_head(body: &[u8]) -> Result<&[u8], Option<String>> {
    // `json.Decoder` scans with `encoding/json`'s scanner, so the 10000-level
    // cap applies to `Decode` exactly as it applies to `json.Valid`. Verified
    // against Go 1.26.5: a 10001-deep body is `exceeded max depth`, which the
    // handlers turn into their 400 — while serde's `ignore_value` is iterative
    // and would have decoded it happily.
    if nesting_depth_exceeds(body, GO_MAX_NESTING_DEPTH) {
        return Err(None);
    }
    if !is_utf8(body) {
        return Err(Some(not_utf8_reason("the request body")));
    }
    let mut de = serde_json::Deserializer::from_slice(body);
    // `&RawValue` rather than `IgnoredAny`: its skip is the same iterative
    // `ignore_value`, and it hands back the span that was skipped.
    match <&RawValue>::deserialize(&mut de) {
        Ok(raw) => Ok(raw.get().as_bytes()),
        Err(e) if is_recursion_limit(&e) => Err(Some(e.to_string())),
        Err(_) => Err(None),
    }
}

/// `json.NewDecoder(r.Body).Decode(&req)` for a request **struct**.
///
/// # Why this is not [`super::writes::decode_body`]
///
/// The shared helper is right for the SQLite writes it was built for, and wrong
/// twice over here — both times observably:
///
/// - **It shape-checks through a `serde_json::Value`**, whose parser rejects a
///   number outside float64's range. `{"settings":{"n":1e999}}` would then be a
///   400, and Go answers **422**: the value rides in a `json.RawMessage`, which
///   is not parsed until the service looks at it. That is the one reachable
///   `ValidationError` on this surface, so losing it loses the case.
/// - **It requires end of input.** A `json.Decoder` reads a *stream* and stops
///   at the end of the first value, so `{"name":"x"} junk` is a successful
///   decode in Go. `serde_json::from_slice` is not a stream and rejects it.
///
/// What is left is Go's own rule, which only needs the first token: an object
/// decodes field by field, a `null` is the documented no-op leaving the zero
/// value, and anything else — an array, a scalar, nothing at all — is the type
/// error the handler turns into its 400.
///
/// Duplicate keys forward for the reason `decode_body` documents: `encoding/json`
/// keeps the last occurrence but type-checks every one, and serde refuses them
/// outright, so only Go can answer. Safe because this runs before any mutation.
///
/// The two guards ahead of the decode are the two places serde and
/// `encoding/json` disagree about the *body* rather than about a value:
///
/// - **Depth.** `Decode` drives `encoding/json`'s scanner, so it stops at 10000
///   levels — including inside a field the struct ignores. serde routes an
///   unknown field to `IgnoredAny`, whose skip is iterative and counts nothing,
///   so `{"name":"x","junk":[×10001]}` decoded here with `name == "x"` and
///   *created a profile Go refuses*, answering 201 where Go answers
///   `400 name is required`. A state-changing divergence, which is why the
///   check is on the body and not on the fields.
/// - **UTF-8.** See [`is_utf8`]: Go substitutes U+FFFD into a `string` field
///   and carries on, serde fails the decode. Forwarded rather than guessed.
pub(crate) fn decode_request<T>(body: &[u8]) -> Result<T, WriteError>
where
    T: serde::de::DeserializeOwned + Default,
{
    if nesting_depth_exceeds(body, GO_MAX_NESTING_DEPTH) {
        // `json.Decoder`'s scanner stops at 10000, so this never reaches the
        // struct's fields and the handler sees a zero-valued request.
        return Err(WriteError::InvalidBody);
    }
    if !is_utf8(body) {
        // Go substituted U+FFFD and carried on; with nothing to forward to
        // (#278) the strict decode's refusal is the answer, and the app's own
        // requests are always UTF-8.
        return Err(WriteError::InvalidBody);
    }
    match first_token(body) {
        Some(b'{') => {
            let mut de = serde_json::Deserializer::from_slice(body);
            match T::deserialize(&mut de) {
                Ok(req) => Ok(req),
                // Go took the last occurrence; a 400 since #278, exactly as
                // `writes::decode_body` now answers duplicates.
                Err(e) if e.to_string().starts_with("duplicate field") => {
                    Err(WriteError::InvalidBody)
                }
                Err(_) => Err(WriteError::InvalidBody),
            }
        }
        Some(b'n') if trim_leading_space(body).starts_with(b"null") => Ok(T::default()),
        _ => Err(WriteError::InvalidBody),
    }
}

/// The first byte JSON considers significant, or `None` for a blank body.
fn first_token(body: &[u8]) -> Option<u8> {
    trim_leading_space(body).first().copied()
}

fn trim_leading_space(body: &[u8]) -> &[u8] {
    let start = body
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .unwrap_or(body.len());
    &body[start..]
}

/// Both halves at once, for the callers whose Go original has no observable
/// step between them.
pub fn decode_go_any(body: &[u8]) -> Decoded {
    match decode_stream_head(body) {
        // The **first value's** bytes, not `body`: that is what Go's `Decode`
        // captured and what its `Unmarshal` then sees.
        Ok(first) => go_any(first),
        Err(None) => Decoded::NotJson,
        Err(Some(reason)) => Decoded::Undecidable(reason),
    }
}

/// `json.Unmarshal(raw, &v any)` for a body already known to be syntactically
/// valid JSON.
pub fn go_any(body: &[u8]) -> Decoded {
    // Go parses these; serde refuses to. See [`is_utf8`].
    if !is_utf8(body) {
        return Decoded::Undecidable(not_utf8_reason("the value"));
    }
    if !numbers_fit_float64(body) {
        return Decoded::NumberOutOfRange;
    }
    let mut de = serde_json::Deserializer::from_slice(body);
    match Value::deserialize(&mut de) {
        Ok(value) => match widen_numbers(value) {
            Some(value) => Decoded::Value(value),
            None => Decoded::NumberOutOfRange,
        },
        // A `Value` decode *is* recursive, unlike the `IgnoredAny` skip above,
        // so 129 levels stop it where Go carries on to 10000. That one is
        // forwarded; an ordinary syntax error is Go's own decode failure, which
        // `ensureDefaultProfileExists` handles by seeding an empty object.
        Err(e) if is_recursion_limit(&e) => Decoded::Undecidable(e.to_string()),
        Err(_) => Decoded::NotJson,
    }
}

/// Whether every number literal in `src` survives `strconv.ParseFloat(s, 64)`.
///
/// Go decodes into `any`, which makes **every** number a float64 and fails the
/// whole document when one does not fit — `1e999` is a 400, not an infinity.
/// The check is done over the bytes rather than left to `serde_json`, because
/// the two parsers do not agree on the boundary: `1e-999` underflows to `0`
/// with no error in Go (verified against the Go toolchain), while serde_json
/// raises `NumberOutOfRange` only for the overflow half. Overflow is the whole
/// of Go's rule, so overflow is the whole of this one.
fn numbers_fit_float64(src: &[u8]) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < src.len() {
        let byte = src[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        // Outside a string, only a number token can begin with `-` or a digit —
        // `true`, `false` and `null` contain neither, and `+`, `.`, `e` and `E`
        // never start one.
        if byte == b'-' || byte.is_ascii_digit() {
            let start = i;
            while i < src.len() && matches!(src[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
            {
                i += 1;
            }
            let token = std::str::from_utf8(&src[start..i]).unwrap_or("");
            // A token this port cannot parse at all is one it will not judge —
            // the `Value` decode below decides, and forwards if it cannot.
            if let Ok(f) = token.parse::<f64>() {
                if !f.is_finite() {
                    return false;
                }
            }
            continue;
        }
        i += 1;
    }
    true
}

/// Every number as a float64, which is the only numeric type Go's `any` has.
///
/// This is not cosmetic. `9007199254740993` round-trips through Go as
/// `9007199254740992`, and serde_json would have kept it as a `u64` and written
/// the odd value back. The file on disk would differ from the one Go writes for
/// the same request.
fn widen_numbers(value: Value) -> Option<Value> {
    Some(match value {
        Value::Number(n) => {
            let f = n.as_f64()?;
            Value::Number(serde_json::Number::from_f64(f)?)
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(widen_numbers)
                .collect::<Option<Vec<_>>>()?,
        ),
        Value::Object(fields) => {
            let mut out = serde_json::Map::new();
            for (key, item) in fields {
                out.insert(key, widen_numbers(item)?);
            }
            Value::Object(out)
        }
        other => other,
    })
}

/// `json.MarshalIndent(v, "", "  ")`, which is `Marshal` followed by `Indent`.
pub fn marshal_indent(value: &Value) -> Result<Vec<u8>, String> {
    let compact =
        gojson::to_vec_marshal(value).map_err(|e| format!("marshaling settings JSON: {e}"))?;
    Ok(gojson::indent_compact(&compact))
}

/// Carry stored bytes to the wire the way a `json.RawMessage` field does:
/// through Go's `compact`, so key order and number spelling survive.
///
/// `Result` rather than `Option` on purpose. Every caller puts this behind a
/// `skip_serializing_if` or a nullable field, so a `None` here would not be an
/// error — it would be a **200 with the `settings` key quietly missing**, which
/// is exactly what non-UTF-8 bytes used to produce. Callers now have to say what
/// a failure means, and all of them say "forward".
pub fn raw_field(bytes: &[u8]) -> Result<Box<RawValue>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| not_utf8_reason("the stored bytes"))?;
    RawValue::from_string(gojson::compact(text))
        .map_err(|e| format!("re-emitting stored JSON: {e}"))
}

// ─── GET / PUT /api/claude-settings ───────────────────────────────────────────

/// `api.claudeSettingsResponse`. `settings` is `omitempty` over a
/// `json.RawMessage`, so the "no file" answer is `{"exists":false}` and not
/// `{"exists":false,"settings":null}`.
#[derive(Debug, Serialize)]
struct ClaudeSettingsResponse {
    exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<Box<RawValue>>,
}

/// `handleGetClaudeSettings`.
///
/// Takes the resolved dir rather than the database, so a unit test can drive it
/// over a temp directory — [`run_dir`] is applied once at the seam.
pub fn get_settings(dir: &str) -> Result<super::Answer, String> {
    let path = settings_json_path(dir);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let body = gojson::to_vec(&ClaudeSettingsResponse {
                exists: false,
                settings: None,
            })
            .map_err(|e| format!("encoding claude settings: {e}"))?;
            return Ok(super::Answer::json(body));
        }
        // Go answers 500 here, and this port does not invent 500s.
        Err(e) => return Err(format!("reading {path}: {e}")),
    };

    // Go's encoder substitutes U+FFFD for bytes that are not UTF-8 and ships
    // the document; serde cannot carry them at all. Until #278 this forwarded
    // so Go could answer; now the lossy conversion *is* Go's substitution —
    // same shape, same page kept alive for a hand-corrupted file.
    let data = if is_utf8(&data) {
        data
    } else {
        String::from_utf8_lossy(&data).into_owned().into_bytes()
    };

    if !go_json_valid(&data) {
        // Also a 500 in Go ("Claude settings file contains invalid JSON").
        return Err(format!("{path} contains invalid JSON"));
    }

    let body = gojson::to_vec(&ClaudeSettingsResponse {
        exists: true,
        settings: Some(raw_field(&data)?),
    })
    .map_err(|e| format!("encoding claude settings: {e}"))?;
    Ok(super::Answer::json(body))
}

/// `handleUpdateClaudeSettings`.
///
/// **The path is the trap.** This writes `settings.json` inside the *run* config
/// dir, which is the file `--settings` resolves against on every agent run
/// (#242). Getting it wrong is silent: the run simply gets no settings.
/// The step order is Go's, not a tidier one. `Decode` runs first, then the path
/// is resolved, then `MkdirAll`, and only then is the value parsed into `any` —
/// so a body carrying an out-of-range number is a 400 *after* the directory has
/// been created. Creating a directory is the one effect here that is idempotent
/// and invisible, but reordering it would still be a port that guessed.
pub fn put_settings(dir: &str, body: &[u8]) -> Result<super::Answer, WriteError> {
    // 1. `json.NewDecoder(r.Body).Decode(&incoming)` — `errInvalidJSONBody`.
    // `incoming` is the first value only, and step 4 parses *that*, not `body`.
    // Both arms are 400s since #278: `Some(reason)` marked the shapes whose Go
    // wording only the sidecar could supply (non-UTF-8 bytes, the exact
    // depth-limit message), and with it gone the class is what remains.
    let incoming = decode_stream_head(body).map_err(|reason| {
        if let Some(reason) = reason {
            log::debug!("claude settings body refused: {reason}");
        }
        WriteError::InvalidBody
    })?;

    // 2. `config.ClaudeSettingsJSONPath()`, and 3. `os.MkdirAll(dir, 0700)`.
    let path = settings_json_path(dir);
    if let Err(e) = mkdir_all(dir) {
        return Err(WriteError::Fallback(format!("creating {dir}: {e}")));
    }

    // 4. `json.Unmarshal(incoming, &pretty)` — the handler's own message, which
    // is not `errInvalidJSONBody`.
    let value = match go_any(incoming) {
        Decoded::Value(value) => value,
        Decoded::NumberOutOfRange => {
            return Err(WriteError::BadRequest("invalid JSON settings".to_string()))
        }
        Decoded::Undecidable(reason) => return Err(WriteError::Fallback(reason)),
        // Step 1 captured a complete value and checked its bytes, so this means
        // the two parsers disagree about what JSON is. Forward rather than pick
        // a side: the arm that used to answer 400 here was reachable through
        // non-UTF-8 bytes, where Go writes the file and answers 200.
        Decoded::NotJson => {
            return Err(WriteError::Fallback(
                "the decoded value does not re-parse; only Go can say".to_string(),
            ))
        }
    };

    let out = marshal_indent(&value).map_err(WriteError::Fallback)?;
    if let Err(e) = write_file(&path, &out) {
        return Err(WriteError::Fallback(format!("writing {path}: {e}")));
    }

    let answer = gojson::to_vec(&ClaudeSettingsResponse {
        exists: true,
        settings: Some(raw_field(&out).map_err(WriteError::Fallback)?),
    })
    .map_err(|e| WriteError::Fallback(format!("encoding claude settings: {e}")))?;
    Ok(super::Answer::json(answer))
}

// ─── The seam ─────────────────────────────────────────────────────────────────

/// This module's entry in `native::ENDPOINTS`. One area, nine routes: the reads
/// cannot be split from the writes here, because `GET .../profiles` *is* a write
/// — `ensureDefaultProfileExists` seeds the index and the default profile file
/// on first read.
pub const ENDPOINT: super::Endpoint = super::Endpoint {
    name: "claude-settings",
    claims,
    serve,
};

/// Which of the five chi routes a path is, if any.
#[derive(Debug, PartialEq, Eq)]
enum Route<'a> {
    /// `/api/claude-settings`
    Settings,
    /// `/api/claude-settings/profiles`
    Profiles,
    /// `/api/claude-settings/profiles/{id}`
    Profile(&'a str),
    /// `/api/claude-settings/profiles/{id}/duplicate`
    Duplicate(&'a str),
    /// `/api/claude-settings/profiles/{id}/default`
    Default(&'a str),
}

/// chi's `{id}` matches exactly one segment and never an empty one, so an id
/// containing a slash is a different route and `/profiles/` is no route at all.
fn route(path: &str) -> Option<Route<'_>> {
    if path == "/api/claude-settings" {
        return Some(Route::Settings);
    }
    let rest = path.strip_prefix("/api/claude-settings/profiles")?;
    if rest.is_empty() {
        return Some(Route::Profiles);
    }
    let rest = rest.strip_prefix('/')?;
    let (id, action) = match rest.split_once('/') {
        Some((id, action)) => (id, Some(action)),
        None => (rest, None),
    };
    if id.is_empty() {
        return None;
    }
    match action {
        None => Some(Route::Profile(id)),
        Some("duplicate") => Some(Route::Duplicate(id)),
        Some("default") => Some(Route::Default(id)),
        Some(_) => None,
    }
}

fn claims(method: &Method, path: &str) -> bool {
    match route(path) {
        Some(Route::Settings) => matches!(*method, Method::GET | Method::PUT),
        Some(Route::Profiles) => matches!(*method, Method::GET | Method::POST),
        Some(Route::Profile(_)) => matches!(*method, Method::GET | Method::PUT | Method::DELETE),
        Some(Route::Duplicate(_)) => *method == Method::POST,
        Some(Route::Default(_)) => *method == Method::PUT,
        None => false,
    }
}

fn serve(ctx: &super::Ctx, req: &super::Request) -> Result<super::Answer, String> {
    // Go's modes and Go's `filepath` are what this module writes with, and
    // neither is verified on Windows — the same decision `super::fs` makes,
    // answered as a 501 since #278 removed the sidecar that used to take it.
    if !cfg!(unix) {
        return super::Answer::error(
            axum::http::StatusCode::NOT_IMPLEMENTED,
            "the Claude settings surface is not supported on Windows in this build",
        );
    }
    // Resolved once, here: every handler below works inside one directory, and
    // a failure to resolve it forwards before anything is written.
    let dir = &run_dir(&ctx.db_path)?;
    match (route(req.path), req.method) {
        (Some(Route::Settings), &Method::GET) => get_settings(dir),
        (Some(Route::Settings), &Method::PUT) => finish(put_settings(dir, req.body)),
        (Some(Route::Profiles), &Method::GET) => finish(profiles::list(dir)),
        (Some(Route::Profiles), &Method::POST) => finish(profiles::create(dir, req.body)),
        (Some(Route::Profile(id)), &Method::GET) => finish(profiles::get(dir, id)),
        (Some(Route::Profile(id)), &Method::PUT) => finish(profiles::update(dir, id, req.body)),
        (Some(Route::Profile(id)), &Method::DELETE) => finish(profiles::delete(dir, id)),
        (Some(Route::Duplicate(id)), &Method::POST) => finish(profiles::duplicate(dir, id)),
        (Some(Route::Default(id)), &Method::PUT) => finish(profiles::set_default(dir, id)),
        _ => Err(format!(
            "{} {} is claimed but has no handler",
            req.method, req.path
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::Answer;
    use axum::http::StatusCode;

    // ─── Routing ──────────────────────────────────────────────────────────────

    #[test]
    fn the_five_routes_are_claimed_and_their_neighbours_are_not() {
        assert!(claims(&Method::GET, "/api/claude-settings"));
        assert!(claims(&Method::PUT, "/api/claude-settings"));
        assert!(claims(&Method::GET, "/api/claude-settings/profiles"));
        assert!(claims(&Method::POST, "/api/claude-settings/profiles"));
        assert!(claims(&Method::GET, "/api/claude-settings/profiles/work"));
        assert!(claims(&Method::PUT, "/api/claude-settings/profiles/work"));
        assert!(claims(
            &Method::DELETE,
            "/api/claude-settings/profiles/work"
        ));
        assert!(claims(
            &Method::POST,
            "/api/claude-settings/profiles/work/duplicate"
        ));
        assert!(claims(
            &Method::PUT,
            "/api/claude-settings/profiles/work/default"
        ));

        // Methods chi does not route.
        assert!(!claims(&Method::POST, "/api/claude-settings"));
        assert!(!claims(&Method::DELETE, "/api/claude-settings/profiles"));
        assert!(!claims(&Method::POST, "/api/claude-settings/profiles/work"));
        assert!(!claims(
            &Method::GET,
            "/api/claude-settings/profiles/work/duplicate"
        ));
        assert!(!claims(
            &Method::POST,
            "/api/claude-settings/profiles/work/default"
        ));

        // Shapes chi routes to nothing, plus the Agento settings row next door.
        assert!(!claims(&Method::GET, "/api/claude-settings/"));
        assert!(!claims(&Method::GET, "/api/claude-settings/profiles/"));
        assert!(!claims(&Method::GET, "/api/claude-settings/profiles/a/b/c"));
        assert!(!claims(
            &Method::GET,
            "/api/claude-settings/profiles/a/other"
        ));
        assert!(!claims(&Method::GET, "/api/settings"));
        assert!(!claims(&Method::GET, "/api/claude-settings-profiles"));
    }

    #[test]
    fn an_id_is_one_segment_and_never_empty() {
        assert_eq!(
            route("/api/claude-settings/profiles/default"),
            Some(Route::Profile("default"))
        );
        assert_eq!(
            route("/api/claude-settings/profiles/default/duplicate"),
            Some(Route::Duplicate("default"))
        );
        assert_eq!(
            route("/api/claude-settings/profiles/default/default"),
            Some(Route::Default("default"))
        );
        assert_eq!(route("/api/claude-settings/profiles//duplicate"), None);
    }

    // ─── Paths ────────────────────────────────────────────────────────────────

    #[test]
    fn the_two_well_known_files_sit_in_the_dir() {
        assert_eq!(settings_json_path("/h/.claude"), "/h/.claude/settings.json");
        assert_eq!(
            profiles_path("/h/.claude"),
            "/h/.claude/settings_profiles.json"
        );
        // `filepath.Join` cleans, so a trailing separator does not double up.
        assert_eq!(
            settings_json_path("/h/.claude/"),
            "/h/.claude/settings.json"
        );
    }

    // ─── Go's `any` ───────────────────────────────────────────────────────────

    /// The literals here are the Go toolchain's own answers, printed by a probe
    /// over `json.Unmarshal` into `any` followed by `json.MarshalIndent`.
    #[test]
    fn marshal_indent_is_gos_marshal_indent() {
        let cases: &[(&str, &str)] = &[
            ("{}", "{}"),
            ("[]", "[]"),
            ("{\"a\":1}", "{\n  \"a\": 1\n}"),
            (
                r#"{"b":[1,2],"a":{"c":{}}}"#,
                "{\n  \"a\": {\n    \"c\": {}\n  },\n  \"b\": [\n    1,\n    2\n  ]\n}",
            ),
            (
                r#"{"x":"<script>&</script>"}"#,
                "{\n  \"x\": \"\\u003cscript\\u003e\\u0026\\u003c/script\\u003e\"\n}",
            ),
            (
                r#"{"n":1e3,"m":0.1,"big":9007199254740993}"#,
                "{\n  \"big\": 9007199254740992,\n  \"m\": 0.1,\n  \"n\": 1000\n}",
            ),
            ("123", "123"),
            (r#""str""#, r#""str""#),
            ("null", "null"),
            ("true", "true"),
            (
                r#"{"nested":{"deep":[{"k":"v"},[]]}}"#,
                "{\n  \"nested\": {\n    \"deep\": [\n      {\n        \"k\": \"v\"\n      },\n      []\n    ]\n  }\n}",
            ),
        ];
        for (src, want) in cases {
            let value = match decode_go_any(src.as_bytes()) {
                Decoded::Value(v) => v,
                other => panic!("{src} decoded as {other:?}"),
            };
            let got = String::from_utf8(marshal_indent(&value).expect("marshal")).expect("utf8");
            assert_eq!(&got, want, "src {src}");
        }
    }

    /// A key that sorts after another in insertion order must come first on
    /// disk, because Go marshals a `map[string]any` with its keys sorted. This
    /// is also the canary for `serde_json/preserve_order` arriving through a
    /// transitive dependency, which would silently flip it.
    #[test]
    fn object_keys_are_sorted_the_way_a_go_map_sorts_them() {
        let value = match decode_go_any(br#"{"z":1,"A":2,"a":3,"0":4}"#) {
            Decoded::Value(v) => v,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            String::from_utf8(gojson::to_vec_marshal(&value).expect("marshal")).unwrap(),
            r#"{"0":4,"A":2,"a":3,"z":1}"#
        );
    }

    /// `json.NewDecoder(...).Decode` reads a stream: it stops at the end of the
    /// first value and never looks at what follows. `from_slice` would 400 this.
    #[test]
    fn trailing_bytes_after_the_first_value_are_ignored_as_a_decoder_ignores_them() {
        assert!(matches!(
            decode_go_any(br#"{"a":1}trailing"#),
            Decoded::Value(_)
        ));
        // …and *nothing* downstream may look at them either. `{"a":1} 1e999` is
        // a 200 in Go, because `Decode` hands `Unmarshal` only `{"a":1}`. This
        // was a `400 invalid JSON settings` while the number scan read the whole
        // body — the case the assertion above missed, because `trailing`
        // contains no number.
        assert!(matches!(
            decode_go_any(br#"{"a":1} 1e999"#),
            Decoded::Value(_)
        ));
        // The first value is also all that is *written*.
        assert_eq!(
            decode_stream_head(br#"{"a":1} 1e999"#).expect("decodes"),
            br#"{"a":1}"#
        );
    }

    /// **`json.Decoder` enforces the scanner's 10000-level cap.** Verified
    /// against Go 1.26.5: a 10001-deep body is `exceeded max depth` — including
    /// when the depth is inside a field the struct ignores, where serde's
    /// iterative `IgnoredAny` skip counts nothing and would have decoded it.
    #[test]
    fn a_decode_stops_where_gos_scanner_stops() {
        #[derive(Debug, Default, Deserialize)]
        #[serde(default)]
        struct Probe {
            name: String,
        }

        // Depth 10001: the object plus 10000 arrays.
        let too_deep = format!(
            r#"{{"name":"x","junk":{}{}}}"#,
            "[".repeat(10000),
            "]".repeat(10000)
        );
        assert_eq!(
            decode_request::<Probe>(too_deep.as_bytes()).unwrap_err(),
            WriteError::InvalidBody,
            "Go's Decode errors, so `name` never lands and the handler 400s"
        );
        assert!(matches!(
            decode_go_any(too_deep.as_bytes()),
            Decoded::NotJson
        ));

        // Depth 10000 exactly: Go decodes it, and so must this.
        let deep_enough = format!(
            r#"{{"name":"x","junk":{}{}}}"#,
            "[".repeat(9999),
            "]".repeat(9999)
        );
        let decoded: Probe = decode_request(deep_enough.as_bytes()).expect("Go decodes this");
        assert_eq!(decoded.name, "x");
    }

    /// **Go's JSON layer is not UTF-8-strict and serde's is.** Every one of
    /// these is `true`/`Ok` in Go — `json.Valid`, `Unmarshal` into `any`,
    /// `MarshalIndent`, and the encoder's `json.RawMessage` passthrough — so
    /// none of them may be a Rust *answer*. They forward.
    #[test]
    fn bytes_that_are_not_utf8_forward_rather_than_answering() {
        let src = b"{\"a\":\"\xff\"}";

        // `json.Valid` says true, and so does this — the skip does not validate.
        assert!(go_json_valid(src));
        // …but every parse that materializes the string forwards.
        assert!(matches!(go_any(src), Decoded::Undecidable(_)));
        assert!(matches!(decode_go_any(src), Decoded::Undecidable(_)));
        assert!(
            decode_stream_head(src).unwrap_err().is_some(),
            "a forward, not Go's decode error"
        );

        #[derive(Debug, Default, Deserialize)]
        #[serde(default)]
        struct Probe {
            #[serde(deserialize_with = "gojson::null_is_zero_value")]
            a: String,
        }
        assert!(matches!(
            decode_request::<Probe>(src).unwrap_err(),
            WriteError::InvalidBody
        ));

        // And the wire re-emission, which used to drop the key silently.
        assert!(raw_field(src).is_err());
    }

    #[test]
    fn an_empty_or_malformed_body_is_not_json() {
        for body in [&b""[..], b"   ", b"{not json", b"{\"a\":", b"@"] {
            assert!(
                matches!(decode_go_any(body), Decoded::NotJson),
                "body {body:?}"
            );
        }
    }

    /// Go's rule is `strconv.ParseFloat`'s: **overflow** fails the whole
    /// document, underflow quietly becomes zero. Both halves verified against
    /// the Go toolchain.
    #[test]
    fn only_an_overflowing_number_fails_the_way_go_fails() {
        for body in [&b"1e999"[..], b"-1e999", br#"{"a":{"b":[1e999]}}"#] {
            assert!(
                matches!(decode_go_any(body), Decoded::NumberOutOfRange),
                "body {body:?}"
            );
        }
        // Underflow is a zero, not an error — and it marshals back as `0`.
        for (body, want) in [
            (&b"1e-999"[..], "0"),
            (b"1e-324", "0"),
            (b"0e-999", "0"),
            (b"1e-320", "1e-320"),
            (b"1e308", "1e+308"),
            (b"1e21", "1e+21"),
            (b"1e-7", "1e-7"),
            (b"1e-6", "0.000001"),
        ] {
            let value = match decode_go_any(body) {
                Decoded::Value(v) => v,
                other => panic!("body {body:?} decoded as {other:?}"),
            };
            assert_eq!(
                String::from_utf8(gojson::to_vec_marshal(&value).unwrap()).unwrap(),
                want,
                "body {body:?}"
            );
        }
    }

    /// A number *inside a string* is not a number token, so a string containing
    /// `1e999` must not fail the document.
    #[test]
    fn a_number_shaped_string_is_not_a_number() {
        assert!(matches!(
            decode_go_any(br#"{"a":"1e999"}"#),
            Decoded::Value(_)
        ));
        // …including one that ends in an escaped quote, which is where a naive
        // string tracker loses its place.
        assert!(matches!(
            decode_go_any(br#"{"a":"x\"1e999","b":1}"#),
            Decoded::Value(_)
        ));
    }

    /// A `Value` decode stops at 128 levels and Go's at 10000. This port
    /// answers neither way — it forwards, so Go gives whatever answer Go gives.
    #[test]
    fn a_document_deeper_than_serdes_limit_forwards() {
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        assert!(matches!(
            decode_go_any(deep.as_bytes()),
            Decoded::Undecidable(_)
        ));
        // …but `json.Valid` still answers, because the skip is iterative. A
        // port that used `Value` here would call a 5000-level settings file
        // invalid and turn Go's 200 into a 500.
        assert!(go_json_valid(deep.as_bytes()));
    }

    /// Go's scanner caps nesting at 10000, so having *no* limit is the other
    /// wrong answer.
    #[test]
    fn json_valid_stops_where_gos_scanner_stops() {
        let ok = format!("{}{}", "[".repeat(10000), "]".repeat(10000));
        assert!(go_json_valid(ok.as_bytes()));
        let too_deep = format!("{}{}", "[".repeat(10001), "]".repeat(10001));
        assert!(!go_json_valid(too_deep.as_bytes()));
        // Braces and brackets share the counter, and a bracket inside a string
        // is not nesting.
        assert!(go_json_valid(br#"{"a":"[[[[["}"#));
    }

    #[test]
    fn json_valid_wants_one_whole_value_and_nothing_after_it() {
        assert!(go_json_valid(b"{\"a\":1}"));
        assert!(go_json_valid(b"  123  "));
        assert!(!go_json_valid(b"{\"a\":1} junk"));
        assert!(!go_json_valid(b""));
        assert!(!go_json_valid(b"{"));
    }

    /// The wire form of a stored file: compacted and HTML-escaped, but with the
    /// key order and number spelling the user typed left alone.
    #[test]
    fn a_stored_settings_file_reaches_the_wire_through_gos_compact() {
        let raw = b"{\n  \"z\": 1.50,\n  \"a\": \"<b>\"\n}";
        let body = gojson::to_vec(&ClaudeSettingsResponse {
            exists: true,
            settings: raw_field(raw).ok(),
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"exists\":true,\"settings\":{\"z\":1.50,\"a\":\"\\u003cb\\u003e\"}}\n"
        );
    }

    /// `omitempty` over a `json.RawMessage`: the absent case is two keys short
    /// of the present one, not a `null`.
    #[test]
    fn a_missing_settings_file_omits_the_key_rather_than_nulling_it() {
        let body = gojson::to_vec(&ClaudeSettingsResponse {
            exists: false,
            settings: None,
        })
        .expect("encode");
        assert_eq!(String::from_utf8(body).unwrap(), "{\"exists\":false}\n");
    }

    // ─── The two write paths, over a temp dir ─────────────────────────────────

    /// A settings dir with the given files, standing in for `~/.claude`.
    pub(super) fn claude_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn writing_settings_pretty_prints_the_file_and_compacts_the_answer() {
        let dir = claude_dir();
        let path = settings_json_path(&dir.path().to_string_lossy());

        let value = match decode_go_any(br#"{"z":1,"a":{"b":[1,2]}}"#) {
            Decoded::Value(v) => v,
            other => panic!("{other:?}"),
        };
        let out = marshal_indent(&value).expect("marshal");
        write_file(&path, &out).expect("write");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "{\n  \"a\": {\n    \"b\": [\n      1,\n      2\n    ]\n  },\n  \"z\": 1\n}"
        );
        let body = gojson::to_vec(&ClaudeSettingsResponse {
            exists: true,
            settings: raw_field(&out).ok(),
        })
        .expect("encode");
        assert_eq!(
            String::from_utf8(body).unwrap(),
            "{\"exists\":true,\"settings\":{\"a\":{\"b\":[1,2]},\"z\":1}}\n"
        );
    }

    /// The mode is part of the write: these files carry API keys and hook
    /// commands, and `std::fs::write` would create them world-readable.
    #[cfg(unix)]
    #[test]
    fn files_are_created_0600_and_directories_0700() {
        use std::os::unix::fs::PermissionsExt;

        let root = claude_dir();
        let dir = format!("{}/nested/.claude", root.path().to_string_lossy());
        mkdir_all(&dir).expect("mkdir");
        let path = settings_json_path(&dir);
        write_file(&path, b"{}").expect("write");

        let file = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(file & 0o777, 0o600, "settings.json must be 0600");
        let dir_mode = std::fs::metadata(&dir).expect("stat").permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700, "the config dir must be 0700");
    }

    // ─── The two handlers, against the answers Go gave ────────────────────────
    //
    // Every literal below was recorded from a running Go server by
    // `tests/parity_claude_settings.rs`. A write cannot be asked of both
    // implementations at once, so the comparison lives across the two files.

    fn body_of(answer: Answer) -> String {
        String::from_utf8(answer.body.expect("a body")).expect("utf8")
    }

    /// The read, in both its states.
    #[test]
    fn the_settings_read_answers_what_go_answers() {
        let root = claude_dir();
        let dir = root.path().to_string_lossy().into_owned();

        // No file at all.
        assert_eq!(
            body_of(get_settings(&dir).expect("read")),
            "{\"exists\":false}\n"
        );

        // A file, carried through `compact` — key order and `1.50` intact,
        // `<` escaped.
        write_file(
            &settings_json_path(&dir),
            b"{\n  \"z\": 1.50,\n  \"tag\": \"<b>\"\n}",
        )
        .expect("write");
        assert_eq!(
            body_of(get_settings(&dir).expect("read")),
            "{\"exists\":true,\"settings\":{\"z\":1.50,\"tag\":\"\\u003cb\\u003e\"}}\n"
        );

        // An unparseable file is a 500 in Go, and this port does not invent
        // 500s — it forwards.
        write_file(&settings_json_path(&dir), b"not json").expect("write");
        assert!(get_settings(&dir).is_err());

        // A file that is valid JSON to Go but not UTF-8 is served lossily
        // since #278 — the U+FFFD substitution is Go's own answer, so a
        // hand-corrupted file still renders instead of erroring the page.
        write_file(&settings_json_path(&dir), b"{\"a\":\"\xff\"}").expect("write");
        let answer = get_settings(&dir).expect("lossy settings read");
        let body = String::from_utf8(answer.body.expect("body")).expect("utf8");
        assert!(
            body.contains("\u{fffd}") && body.contains("\"exists\":true"),
            "{body}"
        );
    }

    /// The write: every status and message, and the file left behind.
    #[test]
    fn the_settings_write_answers_what_go_answers() {
        let root = claude_dir();
        let dir = root.path().to_string_lossy().into_owned();

        for body in [&b""[..], b"   ", b"{not json"] {
            let err = put_settings(&dir, body).unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "body {body:?}");
            assert_eq!(err.message(), "invalid JSON body", "body {body:?}");
        }

        // The second parse has its own message — collapsing the two would be a
        // wire divergence on a request that is a 400 either way.
        let err = put_settings(&dir, br#"{"n":1e999}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "invalid JSON settings");

        // Underflow is a zero, not an error.
        assert_eq!(
            body_of(put_settings(&dir, br#"{"tiny":1e-999}"#).expect("write")),
            "{\"exists\":true,\"settings\":{\"tiny\":0}}\n"
        );

        // A `Decoder` stops at the first value — and so does everything after
        // it, including the number scan. `1e999` past the first value is a 200.
        assert_eq!(
            body_of(put_settings(&dir, br#"{"trailing":true} and then some"#).expect("write")),
            "{\"exists\":true,\"settings\":{\"trailing\":true}}\n"
        );
        assert_eq!(
            body_of(put_settings(&dir, br#"{"trailing":true} 1e999"#).expect("write")),
            "{\"exists\":true,\"settings\":{\"trailing\":true}}\n"
        );

        // Not UTF-8: Go writes the file with U+FFFD substituted and answers
        // 200. This port cannot reproduce that, so it forwards rather than
        // answering the 400 it used to.
        assert!(matches!(
            put_settings(&dir, b"{\"a\":\"\xff\"}").unwrap_err(),
            WriteError::Fallback(_)
        ));

        // Deeper than the scanner's 10000 levels: `Decode` fails, so this is
        // `errInvalidJSONBody` and not the second parse's message.
        let too_deep = format!("{}{}", "[".repeat(10001), "]".repeat(10001));
        let err = put_settings(&dir, too_deep.as_bytes()).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "invalid JSON body");

        // A scalar is a JSON value, so it is written as one.
        assert_eq!(
            body_of(put_settings(&dir, b"123").expect("write")),
            "{\"exists\":true,\"settings\":123}\n"
        );

        // The round trip, in the response and on disk.
        let answer = put_settings(&dir, br#"{"z":1,"a":{"b":[1,2]},"rate":1.50,"tag":"<b>"}"#)
            .expect("write");
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(
            body_of(answer),
            concat!(
                "{\"exists\":true,\"settings\":{\"a\":{\"b\":[1,2]},\"rate\":1.5,",
                "\"tag\":\"\\u003cb\\u003e\",\"z\":1}}\n"
            )
        );
        assert_eq!(
            std::fs::read_to_string(settings_json_path(&dir)).expect("settings.json"),
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
            )
        );
    }

    /// The two ways `decode_request` differs from the shared `decode_body`, both
    /// of which a port that reused it would get wrong on this surface.
    #[test]
    fn the_request_decoder_is_gos_decoder_and_not_the_shared_one() {
        #[derive(Debug, Default, Deserialize)]
        #[serde(default)]
        struct Probe {
            name: String,
            #[serde(deserialize_with = "gojson::captured_raw")]
            settings: Option<Box<RawValue>>,
        }

        // 1. A number no float64 holds rides through untouched, because a
        //    `json.RawMessage` is not parsed at decode time. `decode_body`'s
        //    `Value` shape check rejects it, which would turn Go's 422 into a
        //    400.
        let decoded: Probe = decode_request(br#"{"settings":{"n":1e999}}"#).expect("decodes");
        assert_eq!(
            decoded.settings.as_deref().map(RawValue::get),
            Some(r#"{"n":1e999}"#)
        );

        // 2. A `Decoder` stops at the first value; `from_slice` wants EOF.
        let decoded: Probe = decode_request(br#"{"name":"x"} junk"#).expect("decodes");
        assert_eq!(decoded.name, "x");

        // Everything else is Go's rule: `null` is the zero value, an array or a
        // scalar is a type error, and a blank body is one too.
        for body in [&b"null"[..], b"  null  "] {
            let zero: Probe = decode_request(body).expect("null is the zero value");
            assert_eq!(zero.name, "");
            assert!(zero.settings.is_none());
        }
        for body in [
            &b""[..],
            b"   ",
            b"[]",
            br#"["x"]"#,
            b"123",
            b"\"s\"",
            b"nope",
        ] {
            assert_eq!(
                decode_request::<Probe>(body).unwrap_err(),
                WriteError::InvalidBody,
                "body {body:?}"
            );
        }

        // Duplicate keys are a 400 since #278: Go type-checked every
        // occurrence and kept the last, serde can do neither, and there is no
        // sidecar left to defer to — the same rule `writes::decode_body` now
        // applies.
        assert!(matches!(
            decode_request::<Probe>(br#"{"name":"a","name":"b"}"#).unwrap_err(),
            WriteError::InvalidBody
        ));
    }

    /// The dir is created if it is not there, because a machine that has never
    /// run Claude Code has no `~/.claude` — and `MkdirAll` runs *before* the
    /// value is parsed, so it happens even on the request that then 400s.
    #[test]
    fn the_write_creates_the_config_dir_before_it_parses() {
        let root = claude_dir();
        let dir = format!("{}/never-used", root.path().to_string_lossy());

        assert!(put_settings(&dir, br#"{"n":1e999}"#).is_err());
        assert!(
            std::path::Path::new(&dir).is_dir(),
            "MkdirAll runs before the second parse, so the dir exists either way"
        );

        put_settings(&dir, br#"{"model":"opus"}"#).expect("write");
        assert_eq!(
            std::fs::read_to_string(settings_json_path(&dir)).expect("settings.json"),
            "{\n  \"model\": \"opus\"\n}"
        );
    }
}
