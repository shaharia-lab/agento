//! Change one top-level key of a config dir's `settings.json` and leave every
//! other byte of the file as it was (#719).
//!
//! `settings.json` belongs to the user and Claude Code reads it on every run.
//! [`super::put_settings`] replaces the whole document through
//! [`super::marshal_indent`], which sorts keys and respells numbers. That is
//! right for the editor and wrong for the retention prompt (#720), whose
//! write has to change one number and nothing else.
//!
//! # Why this splices bytes
//!
//! A decoded round trip cannot keep the file: `serde_json` is built without
//! `preserve_order`, so a `Value` sorts keys, and any decode respells `1e2` as
//! `100`. So [`splice`] finds the byte span of the key's value with a scanner
//! over the original text and builds `prefix + new value + suffix`. Nothing
//! is decoded and nothing is re-encoded. Only the key names are decoded, to
//! compare them, so a key spelled with an escape (`"cleanup\u0050eriodDays"`)
//! is the key Claude Code sees.
//!
//! - **Present:** only the value's bytes change.
//! - **Absent:** the key is appended as the last member, after the last
//!   member's value. The separator (line ending and indentation) and the
//!   spacing around the `:` are copied from the last member, so an indented
//!   CRLF file gets an indented CRLF line and a one-line file stays on one
//!   line. Appending is the only placement that cannot move an existing key.
//! - **Absent, empty object:** the object becomes `{\n  "key": value\n}`, and
//!   whatever is outside the braces is kept.
//! - **No file:** the file is created holding only that key.
//!
//! # What is refused
//!
//! Everything [`super::retention`] reports as `unknown`: a file that is not
//! UTF-8, not valid JSON, not an object, or names the key twice. A dir that
//! is not one of `settings::indexed_claude_config_dirs` is refused too, so a
//! caller cannot point this at an arbitrary path. A refusal writes nothing.
//! Agento does not repair a file the user has to fix.
//!
//! # Claude Code writes this file too
//!
//! The file is read, spliced, then read again just before the write, and the
//! write is refused if the bytes changed. A small window remains between that
//! second read and the rename in [`super::write_file`]. It is stated here, not
//! solved. The write itself is #668's atomic replace, so a failed write leaves
//! the previous file intact and a symlinked file is written through.
//!
//! # The one caller
//!
//! The retention prompt (#720) is the only intended caller, and it adds the
//! route together with its raise-only guard. There is deliberately no route
//! here: a general "set one key" route would be a way to lower
//! `cleanupPeriodDays`, which permanently deletes transcripts.

// The caller is #720's route; until it lands only the tests reach this.
#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;
use std::io;

use super::{go_json_valid, is_utf8, mkdir_all, settings_json_path, write_file};

/// Why a single-key write was refused. Nothing was written in any case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatchError {
    /// The dir is not one of the indexed config dirs.
    DirNotIndexed,
    /// `settings.json` is not valid UTF-8.
    NotUtf8,
    /// `settings.json` is not valid JSON.
    NotJson,
    /// `settings.json` is valid JSON but not an object.
    NotAnObject,
    /// `settings.json` names the key more than once at the top level.
    DuplicateKey,
    /// The value to write is not exactly one JSON value.
    InvalidValue,
    /// `settings.json` changed between the read and the write.
    ChangedUnderneath,
    /// Reading, creating the dir or writing failed.
    Io(String),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DirNotIndexed => f.write_str("the config dir is not one Agento indexes"),
            Self::NotUtf8 => f.write_str("settings.json is not valid UTF-8; fix the file first"),
            Self::NotJson => f.write_str("settings.json is not valid JSON; fix the file first"),
            Self::NotAnObject => {
                f.write_str("settings.json is not a JSON object; fix the file first")
            }
            Self::DuplicateKey => {
                f.write_str("settings.json names the key more than once; fix the file first")
            }
            Self::InvalidValue => f.write_str("the value is not one JSON value"),
            Self::ChangedUnderneath => {
                f.write_str("settings.json changed while it was being edited; try again")
            }
            Self::Io(e) => write!(f, "settings.json could not be written ({e})"),
        }
    }
}

/// Set `key` to `value` in `<dir>/settings.json`, changing nothing else.
///
/// `indexed` is `settings::indexed_claude_config_dirs`, resolved by the
/// caller on the blocking side, which keeps this module free of SQLite.
/// `dir` must be one of them, compared as stored. `value` is already-encoded
/// JSON, for #720 a whole number.
pub(crate) fn set_top_level_key(
    indexed: &[String],
    dir: &str,
    key: &str,
    value: &str,
) -> Result<(), PatchError> {
    set_with(indexed, dir, key, value, |_| {}, write_file)
}

/// [`set_top_level_key`] with its two seams: `between` runs after the first
/// read and before the second, and `write` stands in for
/// [`super::write_file`]. Tests use both; production passes a no-op and the
/// real write.
fn set_with(
    indexed: &[String],
    dir: &str,
    key: &str,
    value: &str,
    between: impl FnOnce(&str),
    write: impl FnOnce(&str, &[u8]) -> io::Result<()>,
) -> Result<(), PatchError> {
    if !indexed.iter().any(|d| d == dir) {
        return Err(PatchError::DirNotIndexed);
    }
    let path = settings_json_path(dir);
    let first = read_if_present(&path)?;
    let patched = match &first {
        Some(bytes) if !is_utf8(bytes) => return Err(PatchError::NotUtf8),
        Some(bytes) => splice(&String::from_utf8_lossy(bytes), key, value)?,
        None => splice("{}", key, value)?,
    };

    between(&path);
    if read_if_present(&path)? != first {
        return Err(PatchError::ChangedUnderneath);
    }

    mkdir_all(dir).map_err(|e| PatchError::Io(e.to_string()))?;
    write(&path, patched.as_bytes()).map_err(|e| PatchError::Io(e.to_string()))
}

/// The file's bytes, or `None` when there is no file yet.
fn read_if_present(path: &str) -> Result<Option<Vec<u8>>, PatchError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PatchError::Io(e.to_string())),
    }
}

/// `src` with the top-level `key` set to `value`. Pure: no I/O.
pub(crate) fn splice(src: &str, key: &str, value: &str) -> Result<String, PatchError> {
    let value = value.trim_matches(is_json_space);
    if !go_json_valid(value.as_bytes()) {
        return Err(PatchError::InvalidValue);
    }
    if !go_json_valid(src.as_bytes()) {
        return Err(PatchError::NotJson);
    }
    let object = scan_object(src.as_bytes()).ok_or(PatchError::NotAnObject)?;

    let mut matching = Vec::new();
    for member in &object.members {
        let name: String = serde_json::from_str(&src[member.key.0..member.key.1])
            .map_err(|_| PatchError::NotJson)?;
        if name == key {
            matching.push(member);
        }
    }

    let out = match (matching.as_slice(), object.members.last()) {
        ([member], _) => format!(
            "{}{value}{}",
            &src[..member.value.0],
            &src[member.value.1..]
        ),
        ([], Some(last)) => {
            // The bytes before the last member's key, back to the comma (or
            // the brace) before it, are this file's member separator.
            let sep_start = match object.members.len() {
                1 => object.open + 1,
                n => object.members[n - 2].comma + 1,
            };
            let sep = &src[sep_start..last.key.0];
            let colon = &src[last.key.1..last.value.0];
            format!(
                "{},{sep}{}{colon}{value}{}",
                &src[..last.value.1],
                encode_key(key),
                &src[last.value.1..]
            )
        }
        ([], None) => {
            let newline = if src.contains("\r\n") { "\r\n" } else { "\n" };
            format!(
                "{}{{{newline}  {}: {value}{newline}}}{}",
                &src[..object.open],
                encode_key(key),
                &src[object.close + 1..]
            )
        }
        _ => return Err(PatchError::DuplicateKey),
    };

    // The value was validated and the splice only joins whole tokens, so this
    // holds by construction. Checked anyway: the file is the user's.
    if !go_json_valid(out.as_bytes()) {
        return Err(PatchError::InvalidValue);
    }
    Ok(out)
}

fn encode_key(key: &str) -> String {
    serde_json::to_string(key).expect("a string always encodes")
}

fn is_json_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// One top-level member, as byte spans of the original text.
#[derive(Debug)]
struct Member {
    /// The key, quotes included.
    key: (usize, usize),
    /// The value, without surrounding whitespace.
    value: (usize, usize),
    /// The comma after the value; meaningless for the last member.
    comma: usize,
}

#[derive(Debug)]
struct Object {
    open: usize,
    close: usize,
    members: Vec<Member>,
}

/// The top-level object's members, or `None` when the document is not an
/// object. `src` has already passed [`go_json_valid`], so the scan only has to
/// find boundaries; it still never indexes out of bounds.
fn scan_object(src: &[u8]) -> Option<Object> {
    let open = skip_space(src, 0);
    if src.get(open) != Some(&b'{') {
        return None;
    }
    let mut members = Vec::new();
    let mut pos = skip_space(src, open + 1);
    if src.get(pos) == Some(&b'}') {
        return Some(Object {
            open,
            close: pos,
            members,
        });
    }
    loop {
        let key_start = pos;
        let key_end = skip_string(src, key_start)?;
        pos = skip_space(src, key_end);
        if src.get(pos) != Some(&b':') {
            return None;
        }
        let value_start = skip_space(src, pos + 1);
        let value_end = skip_value(src, value_start)?;
        pos = skip_space(src, value_end);
        let member = Member {
            key: (key_start, key_end),
            value: (value_start, value_end),
            comma: pos,
        };
        members.push(member);
        match src.get(pos) {
            Some(b',') => pos = skip_space(src, pos + 1),
            Some(b'}') => {
                return Some(Object {
                    open,
                    close: pos,
                    members,
                })
            }
            _ => return None,
        }
    }
}

fn skip_space(src: &[u8], mut pos: usize) -> usize {
    while matches!(src.get(pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        pos += 1;
    }
    pos
}

/// The end of the string starting at `pos`, one past its closing quote.
fn skip_string(src: &[u8], mut pos: usize) -> Option<usize> {
    if src.get(pos) != Some(&b'"') {
        return None;
    }
    pos += 1;
    loop {
        match src.get(pos)? {
            b'\\' => pos += 2,
            b'"' => return Some(pos + 1),
            _ => pos += 1,
        }
    }
}

/// The end of the value starting at `pos`. Iterative, so a nesting depth Go
/// accepts cannot overflow the stack.
fn skip_value(src: &[u8], pos: usize) -> Option<usize> {
    match src.get(pos)? {
        b'"' => skip_string(src, pos),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut pos = pos;
            loop {
                match src.get(pos)? {
                    b'"' => {
                        pos = skip_string(src, pos)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(pos + 1);
                        }
                    }
                    _ => {}
                }
                pos += 1;
            }
        }
        _ => {
            let mut end = pos;
            while !matches!(
                src.get(end),
                None | Some(b' ' | b'\t' | b'\n' | b'\r' | b',' | b'}' | b']')
            ) {
                end += 1;
            }
            Some(end)
        }
    }
}

#[cfg(test)]
#[path = "tests_patch.rs"]
mod tests;
