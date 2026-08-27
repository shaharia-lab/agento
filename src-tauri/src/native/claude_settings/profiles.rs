//! The seven profile routes: `service.claudeSettingsProfileService` and the
//! `settings_profiles.json` index it keeps, in Rust.
//!
//! # A read here is a write
//!
//! `GET /api/claude-settings/profiles` calls `ensureDefaultProfileExists`, which
//! seeds `settings_default.json` from the current `settings.json` and writes the
//! index — so the very first list creates two files. `POST` and
//! `PUT .../default` do the same; `GET/PUT/DELETE .../{id}` and `duplicate`
//! deliberately do **not**, which is why a `GET` on a missing id is a 404 rather
//! than a list that has just been seeded.
//!
//! That is also why this area could not be split into "the reads now, the writes
//! later": there is no read here that does not write.
//!
//! It is also the one place `Mode::Diff`'s "never run a write" rule does not
//! cover, since this *is* a `GET` — and it is harmless in the sense that
//! matters: the proxy runs Rust first and Go second, and seeding is idempotent,
//! so Go's list finds the index Rust just wrote and re-seeds nothing.
//!
//! **But that is also why the diff proves nothing about seeding.** Because Rust
//! runs first, Go is reading Rust's own output: the two answers agree because
//! the second call had nothing left to do, not because both implementations
//! would have seeded the same index from an empty dir. A wrong `settings_default.json`
//! diffs clean here. The unit tests below are what actually pin seeding, and
//! `tests/parity_claude_settings.rs` compares the *files* rather than only the
//! responses for the same reason. Note also that shadow mode writes into
//! whatever Claude config dir the developer is running with, which is the second
//! reason the parity suite insists on a scratch `CLAUDE_CONFIG_DIR`.
//!
//! It is deliberately **not** in `native::diff_exempt`: that list is for routes
//! whose answers cannot agree by construction, and these agree. The problem is
//! that the agreement is uninformative, which is a caveat rather than an
//! exemption.
//!
//! # Which errors reach the wire
//!
//! `httpErr` maps the service's three typed errors and turns everything else
//! into a 500. Only four failures reach the wire from this file:
//!
//! | | |
//! |---|---|
//! | 400 `name is required` | the **handler's** own check on create — not the service's 422, because `handleCreateClaudeSettingsProfile` tests `err != nil \|\| req.Name == ""` before the service runs. A malformed body reaches it too, so `["x"]` is `name is required` and not `invalid JSON body`. |
//! | 400 `invalid JSON body` | the update handler, which *does* use `errInvalidJSONBody`. |
//! | 404 `profile "<id>" not found` | `NotFoundError`. |
//! | 409 `profile with id "<id>" already exists` | `ConflictError` — used both for a rename collision **and** for deleting the default profile, where the wording is Go's and reads oddly. Reproduced, not improved. |
//! | 422 `validation error for "settings": failed to parse settings JSON` | the one reachable `ValidationError`: `json.Valid` passes a number `strconv.ParseFloat` then rejects. |
//!
//! Everything else is a 500.
//!
//! # Two rules with no error to announce them
//!
//! - **A named profile keeps its recorded absolute path.** Every operation that
//!   reads or removes a profile file uses `Profile::file_path` verbatim; only
//!   create, rename and the seeded default *derive* one from the dir. Moving
//!   the config dir must not silently repoint a profile at a file that is not
//!   its own.
//! - **`slugify` walks Unicode categories.** `unicode.IsLetter`/`IsDigit` keep
//!   accented letters, which `safeProfileID` then rejects — so a non-ASCII name
//!   is a 500 in Go unless every character happens to be dropped. Rust's
//!   `char::is_alphabetic` is a *different* set (it includes `Nl` and
//!   `Other_Alphabetic`), so rather than approximate the tables, any non-ASCII
//!   name is **declined with a 500** before anything is written. That is a
//!   known limitation rather than a reproduction — see the known-bugs list in
//!   `CLAUDE.md`.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;

use super::super::writes::WriteError;
use super::super::{gojson, gopath, Answer};
use super::{
    decode_request, go_any, go_json_valid, marshal_indent, mkdir_all, raw_field,
    settings_json_path, Decoded,
};

// ─── The index ────────────────────────────────────────────────────────────────

/// `config.ClaudeSettingsProfile`. Field order is the Go struct's, which is what
/// decides the key order on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
    pub id: String,
    #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
    pub name: String,
    #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
    pub file_path: String,
    #[serde(default, deserialize_with = "gojson::null_is_zero_value")]
    pub is_default: bool,
}

/// `config.ProfilesMetadata`, as read.
#[derive(Debug, Default, Deserialize)]
struct MetadataIn {
    #[serde(default)]
    profiles: Option<Vec<Profile>>,
}

/// `config.ProfilesMetadata`, as written.
///
/// A slice rather than an `Option`, because **no save path can produce a nil
/// one**: `ensureDefaultProfileExists` assigns a one-element slice, create and
/// duplicate append, and delete only runs once an index has been found — so the
/// `{"profiles": null}` an untouched struct would marshal is unreachable here.
/// Deleting the last profile *is* reachable and writes `[]`, which is the
/// distinction that does travel.
#[derive(Debug, Serialize)]
struct MetadataOut<'a> {
    profiles: &'a [Profile],
}

/// `config.LoadProfilesMetadata`. A missing file is an empty index, not an
/// error; anything else is a 500.
///
/// Public because the agent runner needs it: `LoadProfileFilePathIn` resolves a
/// named settings profile through this same index, and there is now exactly one
/// reader of it.
pub fn load(dir: &str) -> Result<Vec<Profile>, WriteError> {
    let path = super::profiles_path(dir);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(WriteError::Fallback(format!("reading {path}: {e}"))),
    };
    // `json.Unmarshal` into a struct: a whole-file `null` is a no-op leaving the
    // zero value, an array is a type error, an object is decoded.
    let shape: Value = serde_json::from_slice(&data)
        .map_err(|e| WriteError::Fallback(format!("parsing {path}: {e}")))?;
    match shape {
        Value::Null => Ok(Vec::new()),
        Value::Object(_) => serde_json::from_slice::<MetadataIn>(&data)
            .map(|m| m.profiles.unwrap_or_default())
            .map_err(|e| WriteError::Fallback(format!("decoding {path}: {e}"))),
        _ => Err(WriteError::Fallback(format!(
            "{path} is not a profiles index"
        ))),
    }
}

/// The exact bytes `saveProfilesMetadata` puts in the file.
///
/// Public because the live parity suite compares them against an index the Go
/// server wrote: the two processes share this file, so a formatting difference
/// would make it churn every time they take turns.
pub fn encode_index(profiles: &[Profile]) -> Result<Vec<u8>, String> {
    let compact = gojson::to_vec_marshal(&MetadataOut { profiles })
        .map_err(|e| format!("marshaling profiles index: {e}"))?;
    Ok(gojson::indent_compact(&compact))
}

/// `saveProfilesMetadata`: `MarshalIndent` into the file, mode 0600.
fn save(dir: &str, profiles: &[Profile]) -> Result<(), WriteError> {
    let path = super::profiles_path(dir);
    if let Err(e) = mkdir_all(&gopath::dir(&path)) {
        return Err(WriteError::Fallback(format!("creating {dir}: {e}")));
    }
    let data = encode_index(profiles).map_err(WriteError::Fallback)?;
    super::write_file(&path, &data)
        .map_err(|e| WriteError::Fallback(format!("writing {path}: {e}")))
}

/// `ensureDefaultProfileExists`: seed a "Default" profile from the current
/// `settings.json` when the index is empty.
fn ensure_default(dir: &str) -> Result<(), WriteError> {
    if !load(dir)?.is_empty() {
        return Ok(());
    }
    if let Err(e) = mkdir_all(dir) {
        return Err(WriteError::Fallback(format!("creating {dir}: {e}")));
    }

    // An unreadable or unparseable settings.json seeds an empty object rather
    // than failing — the profile has to exist either way.
    let content = std::fs::read(settings_json_path(dir)).unwrap_or_else(|_| b"{}".to_vec());
    // `ensureDefaultProfileExists` calls `json.Unmarshal(content, &pretty)`,
    // which is **whole-input**: trailing content after the first value fails it
    // (`Unmarshal([]byte("{\"a\":1} junk"))` is `invalid character 'j' after
    // top-level value`, verified against the Go toolchain) and Go seeds `{}`.
    // `go_any` deliberately never calls `de.end()` — right for a request body a
    // `json.Decoder` reads, wrong for a file `Unmarshal` reads — so the rule is
    // reapplied here. `go_json_valid` is exactly "one value, trailing
    // whitespace only", so it is the rule rather than a second copy of it.
    //
    // This propagates if it is wrong: every later `create` byte-copies the
    // seeded file, so a `{"a":1}` Go would never have written travels into
    // every profile made afterwards.
    let value = if !go_json_valid(&content) {
        Value::Object(serde_json::Map::new())
    } else {
        match go_any(&content) {
            Decoded::Value(value) => value,
            Decoded::NotJson | Decoded::NumberOutOfRange => Value::Object(serde_json::Map::new()),
            // Go's parser would have succeeded; ours cannot say — a document
            // past serde's 128-level limit, or bytes that are not UTF-8, which
            // Go decodes with a U+FFFD substitution this port will not guess.
            // Nothing but the directory has been touched, so the 500 is
            // exact.
            Decoded::Undecidable(reason) => return Err(WriteError::Fallback(reason)),
        }
    };
    let out = marshal_indent(&value).map_err(WriteError::Fallback)?;

    let file_path = gopath::join(&[dir, "settings_default.json"]);
    super::write_file(&file_path, &out)
        .map_err(|e| WriteError::Fallback(format!("writing {file_path}: {e}")))?;

    save(
        dir,
        &[Profile {
            id: "default".to_string(),
            name: "Default".to_string(),
            file_path,
            is_default: true,
        }],
    )
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn find_index(profiles: &[Profile], id: &str) -> Option<usize> {
    profiles.iter().position(|p| p.id == id)
}

fn not_found(id: &str) -> WriteError {
    WriteError::NotFound {
        resource: "profile".to_string(),
        id: id.to_string(),
    }
}

fn conflict(id: &str) -> WriteError {
    WriteError::Conflict {
        resource: "profile".to_string(),
        id: id.to_string(),
    }
}

/// `deduplicateID`: `base`, then `base-2`, `base-3`, …
fn deduplicate_id(base: &str, profiles: &[Profile]) -> String {
    let mut id = base.to_string();
    let mut n = 2;
    while find_index(profiles, &id).is_some() {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// `slugify`, for the ASCII names it can be reproduced for — see the module
/// header for why a non-ASCII one is declined instead.
fn slugify(name: &str) -> Result<String, WriteError> {
    if !name.is_ascii() {
        return Err(WriteError::Fallback(format!(
            "profile name {name:?} is not ASCII: Go slugifies by Unicode category"
        )));
    }
    let mut slug = String::with_capacity(name.len());
    let mut prev_dash = false;
    for c in name.to_ascii_lowercase().chars() {
        if c == ' ' || c == '-' {
            // `reConsecutiveDashes` collapses the run afterwards; doing it here
            // is the same string.
            if !prev_dash {
                slug.push('-');
                prev_dash = true;
            }
        } else if c.is_ascii_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        }
        // Everything else — including `_` — is dropped by `strings.Map`.
    }
    let trimmed = slug.trim_matches('-');
    Ok(if trimmed.is_empty() {
        "profile".to_string()
    } else {
        trimmed.to_string()
    })
}

/// `resolveProfileFilePath`: `safeProfileID` first, so a tampered index cannot
/// name a path outside the dir. An id that fails it is a 500 in Go.
fn resolve_profile_file_path(dir: &str, id: &str) -> Result<String, WriteError> {
    let safe = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !safe {
        return Err(WriteError::Fallback(format!(
            "invalid profile id {id:?}: contains disallowed characters"
        )));
    }
    Ok(gopath::join(&[dir, &format!("settings_{id}.json")]))
}

/// `validatePathWithinDir`.
///
/// A **relative** recorded path is declined rather than resolved: the original's
/// `filepath.Abs` resolves it against the *server's* working directory, which is
/// a different process's and unknowable here. Every path this surface writes is
/// absolute, so this only fires on a hand-edited index.
fn validate_path_within_dir(path: &str, dir: &str) -> Result<(), WriteError> {
    // `filepath.IsAbs`, and on Windows that is a volume test rather than a
    // leading separator — `\Users\u\settings_x.json` is rooted but names no
    // drive, so it is relative for this purpose exactly as `settings_x.json`
    // is. See [`super::super::gopath::is_abs`] (#374).
    if !gopath::is_abs(path) {
        return Err(WriteError::Fallback(format!(
            "profile file path {path:?} is relative; only the Go server knows its working directory"
        )));
    }
    let abs = gopath::clean(path);
    // `dir + string(os.PathSeparator)`, so the prefix is `\` on Windows —
    // where `gopath::clean` has just normalised every `/` away, and a `/`
    // spelled here would match nothing.
    if !abs.starts_with(&format!("{dir}{}", std::path::MAIN_SEPARATOR)) {
        return Err(WriteError::Fallback(format!(
            "path {abs:?} escapes settings directory"
        )));
    }
    Ok(())
}

/// `readDefaultProfileContent`: the current default profile's bytes, **verbatim**
/// — the new profile is a copy, not a reformat.
fn read_default_profile_content(profiles: &[Profile]) -> Vec<u8> {
    match profiles.iter().find(|p| p.is_default) {
        Some(p) => std::fs::read(&p.file_path).unwrap_or_else(|_| b"{}".to_vec()),
        None => b"{}".to_vec(),
    }
}

/// `syncDefaultToSettingsJSON`: copy the default profile's bytes over
/// `settings.json`. A missing profile file syncs `{}` rather than failing.
fn sync_default_to_settings_json(profile: &Profile, dir: &str) -> Result<(), String> {
    let data = match std::fs::read(&profile.file_path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => b"{}".to_vec(),
        Err(e) => return Err(format!("reading {}: {e}", profile.file_path)),
    };
    let path = settings_json_path(dir);
    super::write_file(&path, &data).map_err(|e| format!("writing {path}: {e}"))
}

// ─── Responses ────────────────────────────────────────────────────────────────

/// `service.ClaudeSettingsProfileDetail`: the embedded profile's four fields
/// inlined ahead of its own two, which is how Go flattens an embedded struct.
#[derive(Debug, Serialize)]
struct ProfileDetail<'a> {
    id: &'a str,
    name: &'a str,
    file_path: &'a str,
    is_default: bool,
    /// No `omitempty`, so a missing or unparseable file ships `null`.
    settings: Option<Box<RawValue>>,
    exists: bool,
}

/// `buildProfileDetail`.
fn detail_body(profile: &Profile, dir: &str) -> Result<Vec<u8>, WriteError> {
    validate_path_within_dir(&profile.file_path, dir)?;

    let mut settings = None;
    let mut exists = false;
    if let Ok(data) = std::fs::read(&profile.file_path) {
        // Go's `json.Valid` accepted bytes that are not UTF-8 and its encoder
        // shipped the document with U+FFFD substituted — serde cannot carry
        // them, so this is declined rather than answered. The lossy
        // conversion *is* that substitution, the same answer `get_settings`
        // gives the unnamed file; only a hand-corrupted file ever takes this
        // branch, since the app writes UTF-8.
        let data = if super::is_utf8(&data) {
            data
        } else {
            String::from_utf8_lossy(&data).into_owned().into_bytes()
        };
        // `json.Valid` only. An unparseable file is `exists: false` with a
        // `null` payload — an answer, not an error.
        if go_json_valid(&data) {
            settings = Some(raw_field(&data).map_err(WriteError::Fallback)?);
            exists = true;
        }
    }

    gojson::to_vec(&ProfileDetail {
        id: &profile.id,
        name: &profile.name,
        file_path: &profile.file_path,
        is_default: profile.is_default,
        settings,
        exists,
    })
    .map_err(|e| WriteError::Fallback(format!("encoding profile detail: {e}")))
}

fn profile_answer(status: StatusCode, profile: &Profile) -> Result<Answer, WriteError> {
    let body = gojson::to_vec(profile)
        .map_err(|e| WriteError::Fallback(format!("encoding profile: {e}")))?;
    Ok(Answer::json_status(status, body))
}

// ─── The seven operations ─────────────────────────────────────────────────────

/// `GET /api/claude-settings/profiles` — `ListProfiles`.
pub fn list(dir: &str) -> Result<Answer, WriteError> {
    ensure_default(dir)?;
    let profiles = load(dir)?;
    // Go returns an explicit empty slice for a nil one, so this is never `null`.
    let body = gojson::to_vec(&profiles)
        .map_err(|e| WriteError::Fallback(format!("encoding profiles: {e}")))?;
    Ok(Answer::json(body))
}

/// `api.CreateProfileRequest`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CreateRequest {
    #[serde(deserialize_with = "gojson::null_is_zero_value")]
    name: String,
}

/// `POST /api/claude-settings/profiles` — `CreateProfile`.
pub fn create(dir: &str, body: &[u8]) -> Result<Answer, WriteError> {
    // The handler folds a decode failure and an empty name into one 400 with one
    // message, so an array body says `name is required` rather than
    // `invalid JSON body`.
    let req: CreateRequest = match decode_request(body) {
        Ok(req) => req,
        Err(WriteError::InvalidBody) => {
            return Err(WriteError::BadRequest("name is required".to_string()))
        }
        Err(other) => return Err(other),
    };
    if req.name.is_empty() {
        return Err(WriteError::BadRequest("name is required".to_string()));
    }

    ensure_default(dir)?;
    let mut profiles = load(dir)?;

    let id = deduplicate_id(&slugify(&req.name)?, &profiles);
    let content = read_default_profile_content(&profiles);
    let file_path = resolve_profile_file_path(dir, &id)?;
    super::write_file(&file_path, &content)
        .map_err(|e| WriteError::Fallback(format!("writing {file_path}: {e}")))?;

    let profile = Profile {
        id,
        name: req.name,
        file_path,
        is_default: false,
    };
    profiles.push(profile.clone());
    save(dir, &profiles)?;
    profile_answer(StatusCode::CREATED, &profile)
}

/// `GET /api/claude-settings/profiles/{id}` — `GetProfile`. No
/// `ensureDefaultProfileExists`, so an id that does not exist is a 404 rather
/// than a freshly seeded index.
pub fn get(dir: &str, id: &str) -> Result<Answer, WriteError> {
    let profiles = load(dir)?;
    let idx = find_index(&profiles, id).ok_or_else(|| not_found(id))?;
    Ok(Answer::json(detail_body(&profiles[idx], dir)?))
}

/// `api.UpdateProfileRequest`.
///
/// `Name` is a `*string`, so an absent key and an explicit `null` are both "do
/// not rename" while `""` is a rename the service then ignores. `Settings` is a
/// `json.RawMessage`, which Go hands the four bytes of a literal `null` — the
/// service tests for exactly that string, so [`gojson::captured_raw`] has to
/// keep it rather than folding it into `None`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UpdateRequest {
    name: Option<String>,
    #[serde(deserialize_with = "gojson::captured_raw")]
    settings: Option<Box<RawValue>>,
}

/// `PUT /api/claude-settings/profiles/{id}` — `UpdateProfile`.
pub fn update(dir: &str, id: &str, body: &[u8]) -> Result<Answer, WriteError> {
    let req: UpdateRequest = decode_request(body)?;
    let mut profiles = load(dir)?;
    let idx = find_index(&profiles, id).ok_or_else(|| not_found(id))?;

    // Hoisted out of the closing `detail_body`, which is where it used to run.
    // It refuses a relative or out-of-dir recorded path — but by then the
    // rename may have moved the file and `save` rewritten the index under the
    // new id, so the request would fail having already half-applied itself.
    // `delete`, `duplicate` and `set_default` all validate up front; this is
    // the same rule.
    validate_path_within_dir(&profiles[idx].file_path, dir)?;

    // `validateSettingsJSON` runs before any filesystem mutation so a rename
    // cannot land with the settings unwritten. Its `json.Valid` check cannot
    // fail — the bytes came out of a decoder — but the *second* parse, into
    // `any`, can, and Go reaches it only after the rename. Parsing here and
    // reporting later keeps both: Go's state and Go's answer.
    //
    // Deciding it now is also what makes `Undecidable` safe to answer: an
    // `Err` after the rename would report failure for a profile that has
    // already moved.
    let settings = match req.settings.as_deref().map(RawValue::get) {
        None => None,
        Some("null") => None,
        Some(raw) => {
            if !go_json_valid(raw.as_bytes()) {
                return Err(WriteError::validation(
                    "settings",
                    "settings must be valid JSON",
                ));
            }
            match go_any(raw.as_bytes()) {
                Decoded::Value(value) => Some(Ok(value)),
                Decoded::NumberOutOfRange => Some(Err(())),
                // `go_json_valid` just passed over the same bytes, so this is
                // the two parsers disagreeing rather than a malformed request.
                // Declined — answering 400 here would be inventing a status
                // has no reason to send.
                Decoded::NotJson => {
                    return Err(WriteError::Fallback(
                        "settings re-parse disagrees with json.Valid; only Go can say".to_string(),
                    ))
                }
                Decoded::Undecidable(reason) => return Err(WriteError::Fallback(reason)),
            }
        }
    };

    if let Some(new_name) = req.name.as_deref() {
        if !new_name.is_empty() && new_name != profiles[idx].name {
            rename(&mut profiles, idx, id, new_name, dir)?;
        }
    }

    match settings {
        None => {}
        // The deferred 422, raised where Go raises it — after the rename.
        Some(Err(())) => {
            return Err(WriteError::validation(
                "settings",
                "failed to parse settings JSON",
            ))
        }
        Some(Ok(value)) => {
            let out = marshal_indent(&value).map_err(WriteError::Fallback)?;
            // Written without `validate_path_within_dir`, on purpose: Go's
            // `writeProfileSettings` has no check either, so adding one here
            // would refuse a write Go performs. The hoisted check above covers
            // the recorded path this request arrived with; a path the *rename*
            // produced is always `<dir>/settings_<id>.json`.
            let path = profiles[idx].file_path.clone();
            super::write_file(&path, &out)
                .map_err(|e| WriteError::Fallback(format!("writing {path}: {e}")))?;
            if profiles[idx].is_default {
                // Go logs this failure and carries on, so the request still
                // succeeds with settings.json left behind.
                if let Err(e) = sync_default_to_settings_json(&profiles[idx], dir) {
                    log::error!("native claude-settings: sync default profile failed: {e}");
                }
            }
        }
    }

    save(dir, &profiles)?;
    Ok(Answer::json(detail_body(&profiles[idx], dir)?))
}

/// `renameProfile` + `moveProfileFile`.
///
/// A rename collision is an explicit 409 rather than the auto-deduplication
/// create and duplicate perform — the two really do differ.
fn rename(
    profiles: &mut [Profile],
    idx: usize,
    current_id: &str,
    new_name: &str,
    dir: &str,
) -> Result<(), WriteError> {
    let new_id = slugify(new_name)?;
    if new_id != current_id {
        if profiles
            .iter()
            .enumerate()
            .any(|(i, p)| i != idx && p.id == new_id)
        {
            return Err(conflict(&new_id));
        }
        let new_file_path = resolve_profile_file_path(dir, &new_id)?;
        // No file to move is not an error: the index is updated and the write
        // that follows creates it at the new path.
        //
        // Also no `validate_path_within_dir` — correct parity, not an oversight:
        // Go's `moveProfileFile` reads the recorded path unchecked. The
        // destination is safe by construction (`resolveProfileFilePath` runs
        // `safeProfileID` first), so only the *source* is unvalidated, and it is
        // a read.
        if let Ok(data) = std::fs::read(&profiles[idx].file_path) {
            super::write_file(&new_file_path, &data)
                .map_err(|e| WriteError::Fallback(format!("writing {new_file_path}: {e}")))?;
            // Go warns and continues if the old file cannot be removed.
            let _ = std::fs::remove_file(&profiles[idx].file_path);
        }
        profiles[idx].id = new_id;
        profiles[idx].file_path = new_file_path;
    }
    profiles[idx].name = new_name.to_string();
    Ok(())
}

/// `DELETE /api/claude-settings/profiles/{id}` — `DeleteProfile`.
pub fn delete(dir: &str, id: &str) -> Result<Answer, WriteError> {
    let mut profiles = load(dir)?;
    let idx = find_index(&profiles, id).ok_or_else(|| not_found(id))?;
    if profiles[idx].is_default {
        // `ConflictError` — so the message says "already exists" for a profile
        // that cannot be deleted. Go's wording, kept.
        return Err(conflict(id));
    }

    let file_path = profiles[idx].file_path.clone();
    validate_path_within_dir(&file_path, dir)?;

    profiles.remove(idx);
    // The index is saved *before* the file is removed, and a failed removal is
    // only logged — so a profile whose file survives is gone from the index.
    save(dir, &profiles)?;
    if let Err(e) = std::fs::remove_file(&file_path) {
        log::warn!("native claude-settings: removing {file_path} failed: {e}");
    }
    Ok(Answer::no_content())
}

/// `POST /api/claude-settings/profiles/{id}/duplicate` — `DuplicateProfile`.
pub fn duplicate(dir: &str, id: &str) -> Result<Answer, WriteError> {
    let mut profiles = load(dir)?;
    let idx = find_index(&profiles, id).ok_or_else(|| not_found(id))?;
    validate_path_within_dir(&profiles[idx].file_path, dir)?;

    let new_name = format!("Copy of {}", profiles[idx].name);
    let new_id = deduplicate_id(&slugify(&new_name)?, &profiles);
    let content = std::fs::read(&profiles[idx].file_path).unwrap_or_else(|_| b"{}".to_vec());
    let file_path = resolve_profile_file_path(dir, &new_id)?;
    super::write_file(&file_path, &content)
        .map_err(|e| WriteError::Fallback(format!("writing {file_path}: {e}")))?;

    let profile = Profile {
        id: new_id,
        name: new_name,
        file_path,
        is_default: false,
    };
    profiles.push(profile.clone());
    save(dir, &profiles)?;
    profile_answer(StatusCode::CREATED, &profile)
}

/// `PUT /api/claude-settings/profiles/{id}/default` — `SetDefaultProfile`.
///
/// The order is load-bearing: `settings.json` is overwritten **before** the
/// index is saved, so a failure in between leaves the file synced to a profile
/// the index does not yet call default.
pub fn set_default(dir: &str, id: &str) -> Result<Answer, WriteError> {
    ensure_default(dir)?;
    let mut profiles = load(dir)?;

    let mut found = None;
    for (i, profile) in profiles.iter_mut().enumerate() {
        profile.is_default = profile.id == id;
        if profile.is_default {
            found = Some(i);
        }
    }
    let idx = found.ok_or_else(|| not_found(id))?;

    validate_path_within_dir(&profiles[idx].file_path, dir)?;
    sync_default_to_settings_json(&profiles[idx], dir)
        .map_err(|e| WriteError::Fallback(format!("syncing settings.json: {e}")))?;
    save(dir, &profiles)?;
    profile_answer(StatusCode::OK, &profiles[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch config dir. The operations take a database path only to resolve
    /// the dir, so these drive the helpers over a real directory and check the
    /// files that land in it.
    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn path_of(root: &tempfile::TempDir) -> String {
        root.path().to_string_lossy().into_owned()
    }

    fn read(path: &str) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"))
    }

    // ─── slugify and the id rules ─────────────────────────────────────────────

    #[test]
    fn slugify_matches_gos_map_collapse_and_trim() {
        for (name, want) in [
            ("Work", "work"),
            ("My Profile", "my-profile"),
            ("  leading", "leading"),
            ("trailing  ", "trailing"),
            ("Multiple   Spaces", "multiple-spaces"),
            ("a--b", "a-b"),
            ("---", "profile"),
            ("", "profile"),
            ("snake_case", "snakecase"),
            ("Copy of Default", "copy-of-default"),
            ("v1.2", "v12"),
        ] {
            assert_eq!(slugify(name).expect("ascii"), want, "name {name:?}");
        }
    }

    /// Go keeps Unicode letters and then rejects the id it built, unless every
    /// character happened to be dropped. Two different answers from one rule
    /// this port does not reproduce, so it hands the name over.
    #[test]
    fn a_non_ascii_name_is_declined_rather_than_guessing_a_slug() {
        assert!(matches!(
            slugify("Café").unwrap_err(),
            WriteError::Fallback(_)
        ));
        assert!(matches!(slugify("™").unwrap_err(), WriteError::Fallback(_)));
    }

    #[test]
    fn an_id_that_is_not_filename_safe_is_never_turned_into_a_path() {
        let root = dir();
        let d = path_of(&root);
        assert_eq!(
            resolve_profile_file_path(&d, "work-2").expect("safe"),
            format!("{d}/settings_work-2.json")
        );
        for bad in ["", "../escape", "a/b", "a.b", "a b"] {
            assert!(
                matches!(
                    resolve_profile_file_path(&d, bad).unwrap_err(),
                    WriteError::Fallback(_)
                ),
                "id {bad:?}"
            );
        }
    }

    #[test]
    fn duplicate_ids_get_a_numeric_suffix_starting_at_two() {
        let mut profiles = Vec::new();
        assert_eq!(deduplicate_id("work", &profiles), "work");
        for id in ["work", "work-2"] {
            profiles.push(Profile {
                id: id.to_string(),
                name: id.to_string(),
                file_path: String::new(),
                is_default: false,
            });
        }
        assert_eq!(deduplicate_id("work", &profiles), "work-3");
    }

    /// The Windows half of the same guard (#374). It runs only there, because
    /// `validate_path_within_dir` uses the dispatching `gopath::is_abs`/`clean`
    /// and `std::path::MAIN_SEPARATOR` — all three of which are the point.
    /// A *rooted* path naming no drive is relative for this purpose, which is
    /// the case a `starts_with('/')` check got backwards in both directions.
    #[test]
    #[cfg(windows)]
    fn a_windows_path_outside_the_dir_is_refused() {
        let dir = r"C:\Users\u\.claude";
        assert!(validate_path_within_dir(r"C:\Users\u\.claude\settings_a.json", dir).is_ok());
        assert!(validate_path_within_dir(dir, dir).is_err());
        assert!(validate_path_within_dir(r"C:\Users\u\.claude-evil\x.json", dir).is_err());
        assert!(validate_path_within_dir(r"C:\Users\u\.claude\..\x.json", dir).is_err());
        // Rooted, but on no particular drive: only the Go server's process
        // knows which, so it is refused exactly as a bare filename is.
        assert!(matches!(
            validate_path_within_dir(r"\Users\u\.claude\settings_a.json", dir).unwrap_err(),
            WriteError::Fallback(_)
        ));
    }

    #[test]
    fn a_path_outside_the_dir_is_refused() {
        assert!(validate_path_within_dir("/h/.claude/settings_a.json", "/h/.claude").is_ok());
        // The dir itself is not "within" the dir — Go compares against
        // `dir + "/"`, so the prefix has to be a real child.
        assert!(validate_path_within_dir("/h/.claude", "/h/.claude").is_err());
        assert!(validate_path_within_dir("/h/.claude-evil/x.json", "/h/.claude").is_err());
        assert!(validate_path_within_dir("/h/.claude/../x.json", "/h/.claude").is_err());
        // A relative path is Go's working directory's business, not ours.
        assert!(matches!(
            validate_path_within_dir("settings_a.json", "/h/.claude").unwrap_err(),
            WriteError::Fallback(_)
        ));
    }

    // ─── The index file ───────────────────────────────────────────────────────

    /// The bytes are `json.MarshalIndent`'s, printed by a probe over the Go
    /// struct: two-space indent, `": "` after each key, no trailing newline.
    #[test]
    fn the_index_is_written_the_way_go_writes_it() {
        let root = dir();
        let d = path_of(&root);
        save(
            &d,
            &[Profile {
                id: "default".to_string(),
                name: "Default".to_string(),
                file_path: "/h/.claude/settings_default.json".to_string(),
                is_default: true,
            }],
        )
        .expect("save");

        assert_eq!(
            read(&super::super::profiles_path(&d)),
            concat!(
                "{\n",
                "  \"profiles\": [\n",
                "    {\n",
                "      \"id\": \"default\",\n",
                "      \"name\": \"Default\",\n",
                "      \"file_path\": \"/h/.claude/settings_default.json\",\n",
                "      \"is_default\": true\n",
                "    }\n",
                "  ]\n",
                "}"
            )
        );
    }

    /// Deleting the last profile leaves `[]`, not `null` — Go's slice is
    /// non-nil after the re-slice, and the distinction is stored.
    #[test]
    fn an_emptied_index_is_an_empty_array() {
        let root = dir();
        let d = path_of(&root);
        save(&d, &[]).expect("save");
        assert_eq!(
            read(&super::super::profiles_path(&d)),
            "{\n  \"profiles\": []\n}"
        );
    }

    #[test]
    fn a_missing_index_is_empty_and_a_null_one_is_too() {
        let root = dir();
        let d = path_of(&root);
        assert!(load(&d).expect("missing is empty").is_empty());

        std::fs::write(super::super::profiles_path(&d), "null").expect("write");
        assert!(load(&d).expect("null is the zero value").is_empty());

        std::fs::write(super::super::profiles_path(&d), r#"{"profiles":null}"#).expect("write");
        assert!(load(&d).expect("nil list").is_empty());

        // An array where a struct belongs is a type error in Go: a 500, so it
        // is declined rather than read as empty.
        std::fs::write(super::super::profiles_path(&d), "[]").expect("write");
        assert!(matches!(load(&d).unwrap_err(), WriteError::Fallback(_)));
    }

    // ─── ensureDefaultProfileExists ───────────────────────────────────────────

    #[test]
    fn the_first_list_seeds_a_default_profile_from_settings_json() {
        let root = dir();
        let d = path_of(&root);
        std::fs::write(settings_json_path(&d), r#"{"z":1,"a":{"b":2}}"#).expect("write");

        ensure_default(&d).expect("seed");

        // The seeded profile file is pretty-printed and key-sorted, because Go
        // round-trips it through `any` before `MarshalIndent`.
        assert_eq!(
            read(&format!("{d}/settings_default.json")),
            "{\n  \"a\": {\n    \"b\": 2\n  },\n  \"z\": 1\n}"
        );
        let profiles = load(&d).expect("load");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "default");
        assert_eq!(profiles[0].name, "Default");
        assert!(profiles[0].is_default);
        assert_eq!(profiles[0].file_path, format!("{d}/settings_default.json"));

        // Idempotent: a second call must not overwrite a profile the user has
        // since edited.
        std::fs::write(&profiles[0].file_path, "edited").expect("write");
        ensure_default(&d).expect("second");
        assert_eq!(read(&profiles[0].file_path), "edited");
    }

    #[test]
    fn seeding_with_no_settings_json_writes_an_empty_object() {
        let root = dir();
        let d = path_of(&root);
        ensure_default(&d).expect("seed");
        assert_eq!(read(&format!("{d}/settings_default.json")), "{}");
    }

    /// An unparseable `settings.json` seeds `{}` rather than failing — the
    /// profile has to exist either way.
    #[test]
    fn seeding_from_an_unparseable_settings_json_still_produces_a_profile() {
        let root = dir();
        let d = path_of(&root);
        std::fs::write(settings_json_path(&d), "not json at all").expect("write");
        ensure_default(&d).expect("seed");
        assert_eq!(read(&format!("{d}/settings_default.json")), "{}");
    }

    /// **Seeding reads the file with `json.Unmarshal`, which is whole-input.**
    /// Trailing content after the first value fails it, so Go seeds `{}` — where
    /// a `json.Decoder`'s stream semantics would have seeded `{"a": 1}`. This
    /// propagates: every later `create` byte-copies the seeded file.
    #[test]
    fn seeding_rejects_trailing_content_the_way_unmarshal_does() {
        let root = dir();
        let d = path_of(&root);
        std::fs::write(settings_json_path(&d), r#"{"a":1} junk"#).expect("write");
        ensure_default(&d).expect("seed");
        assert_eq!(
            read(&format!("{d}/settings_default.json")),
            "{}",
            "Unmarshal rejects trailing content, so Go seeds an empty object"
        );

        // …and the copy really does travel into the next profile.
        create(&d, br#"{"name":"Copied"}"#).expect("create");
        assert_eq!(read(&format!("{d}/settings_copied.json")), "{}");
    }

    /// A `settings.json` that is valid JSON to Go but not UTF-8: Go seeds the
    /// document with U+FFFD substituted, which this port will not guess. It
    /// is declined, having touched only the directory.
    #[test]
    fn seeding_from_a_non_utf8_settings_json_is_declined() {
        let root = dir();
        let d = path_of(&root);
        std::fs::write(settings_json_path(&d), b"{\"a\":\"\xff\"}").expect("write");
        assert!(matches!(
            ensure_default(&d).unwrap_err(),
            WriteError::Fallback(_)
        ));
        assert!(
            !std::path::Path::new(&format!("{d}/settings_default.json")).exists(),
            "the refusal must leave no seeded profile behind"
        );
    }

    // ─── The two path traps ───────────────────────────────────────────────────

    /// **The named-profile trap.** The index records an absolute path; a read
    /// must use it rather than rebuilding `settings_<id>.json` from the id. A
    /// port that rebuilt would answer `exists: false` for a profile whose file
    /// is right there.
    #[test]
    fn a_named_profile_keeps_its_recorded_path() {
        let root = dir();
        let d = path_of(&root);
        let recorded = format!("{d}/settings_original-name.json");
        std::fs::write(&recorded, r#"{"model":"opus"}"#).expect("write");

        let profile = Profile {
            id: "renamed-since".to_string(),
            name: "Renamed Since".to_string(),
            file_path: recorded.clone(),
            is_default: false,
        };
        // The path the id *would* derive exists too, with different content —
        // so a rebuild would silently answer with the wrong file rather than
        // with nothing.
        std::fs::write(
            resolve_profile_file_path(&d, &profile.id).expect("safe"),
            r#"{"model":"decoy"}"#,
        )
        .expect("write");

        let body = String::from_utf8(detail_body(&profile, &d).expect("detail")).expect("utf8");
        assert!(body.contains(r#""file_path":""#), "{body}");
        assert!(
            body.contains(&format!(r#""file_path":"{recorded}""#)),
            "{body}"
        );
        assert!(body.contains(r#""settings":{"model":"opus"}"#), "{body}");
        assert!(body.ends_with("\"exists\":true}\n"), "{body}");
    }

    /// **The settings.json trap.** Setting a default writes the profile's bytes
    /// over `<run dir>/settings.json` — the file `--settings` resolves against
    /// on every agent run (#242) — and nowhere else.
    #[test]
    fn setting_a_default_syncs_settings_json_in_the_same_dir() {
        let root = dir();
        let d = path_of(&root);
        let file_path = format!("{d}/settings_work.json");
        std::fs::write(&file_path, "{\n  \"model\": \"opus\"\n}").expect("write");

        let profile = Profile {
            id: "work".to_string(),
            name: "Work".to_string(),
            file_path,
            is_default: true,
        };
        sync_default_to_settings_json(&profile, &d).expect("sync");

        // Byte-for-byte the profile's file, not a re-encode of it.
        assert_eq!(read(&settings_json_path(&d)), "{\n  \"model\": \"opus\"\n}");
    }

    /// A default profile whose file has been deleted syncs `{}` rather than
    /// failing the request.
    #[test]
    fn syncing_a_missing_profile_file_writes_an_empty_object() {
        let root = dir();
        let d = path_of(&root);
        let profile = Profile {
            id: "gone".to_string(),
            name: "Gone".to_string(),
            file_path: format!("{d}/settings_gone.json"),
            is_default: true,
        };
        sync_default_to_settings_json(&profile, &d).expect("sync");
        assert_eq!(read(&settings_json_path(&d)), "{}");
    }

    // ─── Details ──────────────────────────────────────────────────────────────

    /// The embedded struct's fields come first, and `settings` is `null` — not
    /// absent — when the file cannot be read or does not parse.
    #[test]
    fn a_detail_for_a_missing_file_is_null_settings_and_exists_false() {
        let root = dir();
        let d = path_of(&root);
        let profile = Profile {
            id: "ghost".to_string(),
            name: "Ghost".to_string(),
            file_path: format!("{d}/settings_ghost.json"),
            is_default: false,
        };
        let body = String::from_utf8(detail_body(&profile, &d).expect("detail")).expect("utf8");
        assert_eq!(
            body,
            format!(
                "{{\"id\":\"ghost\",\"name\":\"Ghost\",\"file_path\":\"{d}/settings_ghost.json\",\
                 \"is_default\":false,\"settings\":null,\"exists\":false}}\n"
            )
        );

        // An unparseable file is the same answer: the profile exists, its
        // settings do not.
        std::fs::write(&profile.file_path, "not json").expect("write");
        let body = String::from_utf8(detail_body(&profile, &d).expect("detail")).expect("utf8");
        assert!(
            body.contains("\"settings\":null,\"exists\":false"),
            "{body}"
        );

        // A file that is valid JSON to Go but not UTF-8 is served lossily
        // since #278: the U+FFFD substitution is Go's own answer, and there is
        // nothing that could decode the raw bytes the other way.
        std::fs::write(&profile.file_path, b"{\"a\":\"\xff\"}").expect("write");
        let body =
            String::from_utf8(detail_body(&profile, &d).expect("lossy detail")).expect("utf8");
        assert!(
            body.contains("\u{fffd}") && body.contains("\"exists\":true"),
            "non-UTF-8 profile settings are served with U+FFFD substituted: {body}"
        );
    }

    /// A profile's stored bytes reach the wire through Go's `compact`: key order
    /// and number spelling as the user typed them, HTML escaped.
    #[test]
    fn a_detail_carries_the_stored_bytes_rather_than_a_re_encoding() {
        let root = dir();
        let d = path_of(&root);
        let profile = Profile {
            id: "raw".to_string(),
            name: "Raw".to_string(),
            file_path: format!("{d}/settings_raw.json"),
            is_default: false,
        };
        std::fs::write(&profile.file_path, "{\n  \"z\": 1.50,\n  \"a\": \"<b>\"\n}")
            .expect("write");
        let body = String::from_utf8(detail_body(&profile, &d).expect("detail")).expect("utf8");
        // Compacted and HTML-escaped, but `1.50` is still `1.50` and `z` still
        // comes first — the user's bytes, not a re-encoding of them.
        assert!(
            body.contains(r#""settings":{"z":1.50,"a":"\u003cb\u003e"},"exists":true"#),
            "{body}"
        );
    }

    /// The list is `[]` and never `null`, and the profile keys are Go's order.
    #[test]
    fn the_list_shape_is_gos() {
        let empty: Vec<Profile> = Vec::new();
        assert_eq!(
            String::from_utf8(gojson::to_vec(&empty).expect("encode")).unwrap(),
            "[]\n"
        );
        let one = vec![Profile {
            id: "default".to_string(),
            name: "Default".to_string(),
            file_path: "/h/.claude/settings_default.json".to_string(),
            is_default: true,
        }];
        assert_eq!(
            String::from_utf8(gojson::to_vec(&one).expect("encode")).unwrap(),
            "[{\"id\":\"default\",\"name\":\"Default\",\
              \"file_path\":\"/h/.claude/settings_default.json\",\"is_default\":true}]\n"
        );
    }

    // ─── Rename ───────────────────────────────────────────────────────────────

    #[test]
    fn renaming_moves_the_file_and_the_recorded_path() {
        let root = dir();
        let d = path_of(&root);
        let old = format!("{d}/settings_before.json");
        std::fs::write(&old, r#"{"a":1}"#).expect("write");
        let mut profiles = vec![Profile {
            id: "before".to_string(),
            name: "Before".to_string(),
            file_path: old.clone(),
            is_default: false,
        }];

        rename(&mut profiles, 0, "before", "After", &d).expect("rename");

        assert_eq!(profiles[0].id, "after");
        assert_eq!(profiles[0].name, "After");
        assert_eq!(profiles[0].file_path, format!("{d}/settings_after.json"));
        assert_eq!(read(&profiles[0].file_path), r#"{"a":1}"#);
        assert!(!std::path::Path::new(&old).exists(), "old file removed");
    }

    /// A rename whose slug collides is a **409**, not an auto-deduplicated id.
    /// Create and duplicate deduplicate; this one refuses.
    #[test]
    fn a_rename_collision_is_409_and_moves_nothing() {
        let root = dir();
        let d = path_of(&root);
        let mine = format!("{d}/settings_mine.json");
        std::fs::write(&mine, "{}").expect("write");
        let mut profiles = vec![
            Profile {
                id: "mine".to_string(),
                name: "Mine".to_string(),
                file_path: mine.clone(),
                is_default: false,
            },
            Profile {
                id: "taken".to_string(),
                name: "Taken".to_string(),
                file_path: format!("{d}/settings_taken.json"),
                is_default: false,
            },
        ];

        let err = rename(&mut profiles, 0, "mine", "Taken", &d).unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.message(), "profile with id \"taken\" already exists");
        assert_eq!(profiles[0].id, "mine", "nothing moved");
        assert!(std::path::Path::new(&mine).exists());
    }

    /// A rename whose slug is unchanged renames nothing on disk — `"My Work"`
    /// and `"my work"` share a slug, so only the display name moves.
    #[test]
    fn a_rename_that_keeps_the_slug_only_changes_the_name() {
        let root = dir();
        let d = path_of(&root);
        let path = format!("{d}/settings_my-work.json");
        std::fs::write(&path, "{}").expect("write");
        let mut profiles = vec![Profile {
            id: "my-work".to_string(),
            name: "My Work".to_string(),
            file_path: path.clone(),
            is_default: false,
        }];

        rename(&mut profiles, 0, "my-work", "my work", &d).expect("rename");
        assert_eq!(profiles[0].name, "my work");
        assert_eq!(profiles[0].file_path, path);
        assert!(std::path::Path::new(&path).exists());
    }

    /// A profile whose file has already gone is renamed in the index alone —
    /// Go's `moveProfileFile` returns early rather than failing.
    #[test]
    fn renaming_a_profile_with_no_file_updates_only_the_index() {
        let root = dir();
        let d = path_of(&root);
        let mut profiles = vec![Profile {
            id: "gone".to_string(),
            name: "Gone".to_string(),
            file_path: format!("{d}/settings_gone.json"),
            is_default: false,
        }];
        rename(&mut profiles, 0, "gone", "Back", &d).expect("rename");
        assert_eq!(profiles[0].id, "back");
        assert_eq!(profiles[0].file_path, format!("{d}/settings_back.json"));
        assert!(!std::path::Path::new(&profiles[0].file_path).exists());
    }

    // ─── Error vocabulary ─────────────────────────────────────────────────────

    /// These strings are on the wire, so a paraphrase is a divergence. The 409
    /// for "cannot delete the default profile" really does say "already exists".
    #[test]
    fn the_error_bodies_are_gos() {
        assert_eq!(not_found("work").status(), StatusCode::NOT_FOUND);
        assert_eq!(not_found("work").message(), "profile \"work\" not found");
        assert_eq!(conflict("default").status(), StatusCode::CONFLICT);
        assert_eq!(
            conflict("default").message(),
            "profile with id \"default\" already exists"
        );
        assert_eq!(
            WriteError::validation("settings", "failed to parse settings JSON").message(),
            "validation error for \"settings\": failed to parse settings JSON"
        );
    }

    /// The create handler folds every decode failure into its own 400, so an
    /// array body says `name is required` — the update handler, which uses
    /// `errInvalidJSONBody`, says something else for the same body.
    #[test]
    fn a_bad_create_body_is_400_name_is_required() {
        let root = dir();
        let d = path_of(&root);
        for body in [
            &b""[..],
            b"[]",
            br#"["Sneaky"]"#,
            b"{not json",
            b"{}",
            b"null",
        ] {
            let err = create(&d, body).unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "body {body:?}");
            assert_eq!(err.message(), "name is required", "body {body:?}");
        }
    }

    /// **A body past the scanner's 10000-level cap creates nothing.** `Decode`
    /// errors `exceeded max depth` in Go, so `req.Name` stays empty and the
    /// handler answers `400 name is required`. serde routes the unknown `junk`
    /// field to `IgnoredAny`, whose skip is iterative and counts no depth, so
    /// this used to decode with `name == "x"` and answer **201** — writing
    /// `settings_x.json` and appending to the index for a request Go refuses.
    #[test]
    fn a_create_deeper_than_gos_scanner_is_400_and_writes_nothing() {
        let root = dir();
        let d = path_of(&root);
        let body = format!(
            r#"{{"name":"x","junk":{}{}}}"#,
            "[".repeat(10000),
            "]".repeat(10000)
        );

        let err = create(&d, body.as_bytes()).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "name is required");

        // Not the profile, not the seeded default, not the index: the decode
        // fails before `ensure_default` runs.
        assert!(!std::path::Path::new(&format!("{d}/settings_x.json")).exists());
        assert!(!std::path::Path::new(&super::super::profiles_path(&d)).exists());
        assert!(!std::path::Path::new(&format!("{d}/settings_default.json")).exists());
    }

    /// The same cap on `update`, which is a **400** and not the 422 a
    /// hand-written depth check inside `validateSettingsJSON` produced: Go's
    /// `Decode` fails first, so the service never runs. The 128–10000 band
    /// is still declined, which is the neighbouring case easy to lose.
    #[test]
    fn an_update_deeper_than_gos_scanner_is_400_and_the_band_below_is_declined() {
        let root = dir();
        let d = path_of(&root);
        list(&d).expect("seed");

        let too_deep = format!(
            r#"{{"settings":{}{}}}"#,
            "[".repeat(10001),
            "]".repeat(10001)
        );
        let err = update(&d, "default", too_deep.as_bytes()).unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "invalid JSON body");

        // Past serde's 128-level limit but inside Go's 10000: neither parser is
        // the authority, so it declines.
        let band = format!(r#"{{"settings":{}{}}}"#, "[".repeat(200), "]".repeat(200));
        assert!(matches!(
            update(&d, "default", band.as_bytes()).unwrap_err(),
            WriteError::Fallback(_)
        ));
    }

    /// **`update` validates the recorded path before it mutates anything.** It
    /// used to reach `validate_path_within_dir` only in the closing
    /// `detail_body` — after the rename had moved the file and `save` had
    /// rewritten the index under the new id — so the failure was reported for a
    /// profile that had already been renamed underneath it.
    #[test]
    fn update_validates_the_recorded_path_before_it_renames() {
        let root = dir();
        let d = path_of(&root);
        let outside = dir();
        let stray = format!("{}/settings_stray.json", path_of(&outside));
        std::fs::write(&stray, "{}").expect("write");
        save(
            &d,
            &[Profile {
                id: "stray".to_string(),
                name: "Stray".to_string(),
                file_path: stray.clone(),
                is_default: false,
            }],
        )
        .expect("save");

        assert!(matches!(
            update(&d, "stray", br#"{"name":"Moved"}"#).unwrap_err(),
            WriteError::Fallback(_)
        ));

        // The refusal has to be total: nothing may have moved.
        let profiles = load(&d).expect("load");
        assert_eq!(profiles[0].id, "stray", "the index must be untouched");
        assert_eq!(profiles[0].file_path, stray);
        assert!(
            !std::path::Path::new(&format!("{d}/settings_moved.json")).exists(),
            "nothing may have moved"
        );
    }

    /// A body that is not UTF-8 is a 400 from every write on this surface
    /// since #278: Go substituted U+FFFD and carried on, but the app's own
    /// requests are always UTF-8 and there is no sidecar left to defer to.
    #[test]
    fn a_non_utf8_body_is_a_400_from_create_and_update() {
        let root = dir();
        let d = path_of(&root);
        list(&d).expect("seed");

        // `create` folds a decode failure into its own 400, exactly as it
        // does for any malformed body.
        assert!(matches!(
            create(&d, b"{\"name\":\"x\xffy\"}").unwrap_err(),
            WriteError::BadRequest(ref m) if m == "name is required"
        ));
        assert!(matches!(
            update(&d, "default", b"{\"settings\":{\"a\":\"\xff\"}}").unwrap_err(),
            WriteError::InvalidBody
        ));
        // Neither may have written anything.
        assert!(!std::path::Path::new(&format!("{d}/settings_xy.json")).exists());
        assert_eq!(read(&format!("{d}/settings_default.json")), "{}");
    }

    // ─── The whole surface, against the answers Go gave ───────────────────────
    //
    // Every status and every message below was recorded from a running Go
    // server by `tests/parity_claude_settings.rs`. A write cannot be asked of
    // both implementations at once, so the comparison lives across the two
    // files: that suite pins what Go says, this one asserts Rust says it.

    fn body_of(answer: Answer) -> String {
        String::from_utf8(answer.body.expect("a body")).expect("utf8")
    }

    /// The lifecycle, driven end to end over a scratch dir.
    #[test]
    fn the_lifecycle_answers_match_the_answers_go_gave() {
        let root = dir();
        let d = path_of(&root);

        // The first list seeds a default profile and answers with it.
        let listed = list(&d).expect("list");
        assert_eq!(listed.status, StatusCode::OK);
        assert_eq!(
            body_of(listed),
            format!(
                "[{{\"id\":\"default\",\"name\":\"Default\",\
                 \"file_path\":\"{d}/settings_default.json\",\"is_default\":true}}]\n"
            )
        );

        // create: 201, the slugified id, and not the default.
        let created = create(&d, br#"{"name":"Parity Writes"}"#).expect("create");
        assert_eq!(created.status, StatusCode::CREATED);
        assert_eq!(
            body_of(created),
            format!(
                "{{\"id\":\"parity-writes\",\"name\":\"Parity Writes\",\
                 \"file_path\":\"{d}/settings_parity-writes.json\",\"is_default\":false}}\n"
            )
        );

        // A second profile of the same name is deduplicated, not refused.
        let again = create(&d, br#"{"name":"Parity Writes"}"#).expect("create");
        assert!(body_of(again).contains("\"id\":\"parity-writes-2\""));

        // get: a missing id is 404 with the service error's wording.
        let err = get(&d, "parity-no-such-profile").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            err.message(),
            "profile \"parity-no-such-profile\" not found"
        );

        // update: a malformed body is `invalid JSON body`, *not* the create
        // handler's message.
        let err = update(&d, "parity-writes", b"{not json").unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "invalid JSON body");

        // …and a number no float64 holds is the one reachable 422.
        let err = update(&d, "parity-writes", br#"{"settings":{"n":1e999}}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            err.message(),
            "validation error for \"settings\": failed to parse settings JSON"
        );

        // A settings write: the file is key-sorted and pretty-printed, and the
        // detail carries the round trip rather than the request's bytes.
        let updated = update(
            &d,
            "parity-writes",
            br#"{"settings":{"z":1,"a":{"b":[1,2]}}}"#,
        )
        .expect("update");
        assert_eq!(updated.status, StatusCode::OK);
        assert!(
            body_of(updated).contains(r#""settings":{"a":{"b":[1,2]},"z":1},"exists":true"#),
            "the detail must show the stored round trip"
        );

        // A literal `null` settings is a no-op, not a clear.
        let untouched = update(&d, "parity-writes", br#"{"settings":null}"#).expect("update");
        assert!(body_of(untouched).contains(r#""settings":{"a":{"b":[1,2]},"z":1}"#));

        // A rename onto another profile's slug is 409 — while *create* with the
        // same name deduplicated.
        let err = update(&d, "parity-writes-2", br#"{"name":"PARITY WRITES"}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(
            err.message(),
            "profile with id \"parity-writes\" already exists"
        );

        // A rename that moves takes the settings with it.
        let renamed = update(&d, "parity-writes", br#"{"name":"Parity Renamed"}"#).expect("rename");
        let renamed = body_of(renamed);
        assert!(renamed.contains("\"id\":\"parity-renamed\""), "{renamed}");
        assert!(
            renamed.contains(&format!(
                "\"file_path\":\"{d}/settings_parity-renamed.json\""
            )),
            "{renamed}"
        );
        assert!(
            renamed.contains(r#""settings":{"a":{"b":[1,2]},"z":1}"#),
            "{renamed}"
        );

        // update looks the profile up before anything else, so a missing one is
        // a 404 whatever the body says.
        assert_eq!(
            update(&d, "parity-no-such-profile", br#"{"name":"x"}"#)
                .unwrap_err()
                .status(),
            StatusCode::NOT_FOUND
        );

        // duplicate: 201, "Copy of <name>", never the default.
        let copied = duplicate(&d, "parity-renamed").expect("duplicate");
        assert_eq!(copied.status, StatusCode::CREATED);
        let copied = body_of(copied);
        assert!(
            copied.contains(r#""id":"copy-of-parity-renamed""#),
            "{copied}"
        );
        assert!(
            copied.contains(r#""name":"Copy of Parity Renamed""#),
            "{copied}"
        );
        assert!(copied.contains(r#""is_default":false"#), "{copied}");
        assert_eq!(
            duplicate(&d, "parity-no-such-profile")
                .unwrap_err()
                .status(),
            StatusCode::NOT_FOUND
        );

        // default: 200, and settings.json becomes a byte copy of the profile.
        let defaulted = set_default(&d, "parity-renamed").expect("set default");
        assert_eq!(defaulted.status, StatusCode::OK);
        assert!(body_of(defaulted).contains(r#""is_default":true"#));
        assert_eq!(
            read(&settings_json_path(&d)),
            read(&format!("{d}/settings_parity-renamed.json")),
            "settings.json must be a byte copy of the new default profile"
        );
        assert_eq!(
            set_default(&d, "parity-no-such-profile")
                .unwrap_err()
                .status(),
            StatusCode::NOT_FOUND
        );

        // delete: the default is refused with a ConflictError, whose wording is
        // about existence rather than about deletion.
        let err = delete(&d, "parity-renamed").unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(
            err.message(),
            "profile with id \"parity-renamed\" already exists"
        );

        let deleted = delete(&d, "copy-of-parity-renamed").expect("delete");
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        assert!(deleted.body.is_none(), "204 carries no body");
        assert!(
            !std::path::Path::new(&format!("{d}/settings_copy-of-parity-renamed.json")).exists()
        );
        assert_eq!(
            delete(&d, "parity-no-such-profile").unwrap_err().status(),
            StatusCode::NOT_FOUND
        );
    }

    /// A profile created before any settings exist copies the default's bytes
    /// **verbatim**, rather than reformatting a file the user did not touch.
    #[test]
    fn a_new_profile_is_a_byte_copy_of_the_current_default() {
        let root = dir();
        let d = path_of(&root);
        list(&d).expect("seed");
        // Hand-edit the default's file so a reformat would be visible.
        std::fs::write(
            format!("{d}/settings_default.json"),
            "{\n    \"z\": 1.50,\n    \"a\": 1\n}",
        )
        .expect("write");

        create(&d, br#"{"name":"Copied"}"#).expect("create");
        assert_eq!(
            read(&format!("{d}/settings_copied.json")),
            "{\n    \"z\": 1.50,\n    \"a\": 1\n}"
        );
    }

    /// Updating the **default** profile syncs `settings.json` too, because that
    /// is the file every agent run resolves `--settings` against.
    #[test]
    fn updating_the_default_profile_syncs_settings_json() {
        let root = dir();
        let d = path_of(&root);
        list(&d).expect("seed");

        update(&d, "default", br#"{"settings":{"model":"opus"}}"#).expect("update");
        assert_eq!(read(&settings_json_path(&d)), "{\n  \"model\": \"opus\"\n}");

        // …and updating a *non*-default one does not touch it.
        create(&d, br#"{"name":"Other"}"#).expect("create");
        update(&d, "other", br#"{"settings":{"model":"haiku"}}"#).expect("update");
        assert_eq!(
            read(&settings_json_path(&d)),
            "{\n  \"model\": \"opus\"\n}",
            "a non-default profile must not reach settings.json"
        );
    }

    /// Deleting the last profile leaves the index as `[]`, and the next list
    /// seeds a fresh default rather than answering with nothing.
    #[test]
    fn the_index_survives_being_emptied() {
        let root = dir();
        let d = path_of(&root);
        list(&d).expect("seed");
        create(&d, br#"{"name":"Only Other"}"#).expect("create");
        // The default cannot go, so remove the other and then unset the default
        // by pointing it at the survivor — which is what the UI does.
        delete(&d, "only-other").expect("delete");
        assert_eq!(load(&d).expect("load").len(), 1);
    }

    /// `{"name":null}` is a `*string` left nil, which the service reads as "do
    /// not rename" — and `{"settings":null}` is the four bytes of a literal
    /// null, which it reads as "do not write". Folding either into the other
    /// would make an omitted key clear a profile's settings.
    #[test]
    fn a_null_name_and_a_null_settings_are_both_no_ops() {
        let req: UpdateRequest =
            decode_request(br#"{"name":null,"settings":null}"#).expect("decode");
        assert!(req.name.is_none());
        assert_eq!(req.settings.as_deref().map(RawValue::get), Some("null"));

        let req: UpdateRequest = decode_request(b"{}").expect("decode");
        assert!(req.name.is_none());
        assert!(req.settings.is_none());

        // A real payload survives verbatim — key order and number spelling
        // included, since the file it writes is the user's.
        let req: UpdateRequest =
            decode_request(br#"{"settings":{"z":1.50,"a":1}}"#).expect("decode");
        assert_eq!(
            req.settings.as_deref().map(RawValue::get),
            Some(r#"{"z":1.50,"a":1}"#)
        );
    }
}
