//! Go-compatible JSON encoding.
//!
//! The bar for a ported endpoint is a **byte-identical** response, because the
//! frontend is shared between the Go server and this app. Rust's natural JSON
//! output is not byte-identical to Go's, in three ways that all show up in real
//! payloads:
//!
//! 1. **Floats.** Go writes `3` for `3.0` and `serde_json` writes `3.0`. Every
//!    rate, cost and ratio in the API is a float, so this differs on almost
//!    every response.
//! 2. **HTML escaping.** `json.Encoder` escapes `<`, `>` and `&` by default, so
//!    the catalog's `<synthetic>` model ships as `"\u003csynthetic\u003e"`.
//!    `serde_json` writes those bytes literally.
//! 3. **The trailing newline.** `json.Encoder.Encode` appends one; a
//!    `json.Marshal` would not, but every handler here goes through `writeJSON`,
//!    which uses the encoder.
//!
//! Everything else (object key order from struct field order, `omitempty` via
//! `skip_serializing_if`, `null` for a nil pointer) already lines up, so this
//! module is a `serde_json::ser::Formatter` rather than a hand-rolled writer:
//! ported types stay ordinary `#[derive(Serialize)]` structs.
//!
//! Cross-language parity is pinned by `desktop/parity/gojson_vectors.json`,
//! which this module's tests and `desktop/parity/gojson_parity_test.go` both
//! read — so a divergence fails one language's tests against the other's
//! actual output rather than against someone's belief about it.
//!
//! **One divergence is left standing: NaN and infinity.** Go fails the whole
//! encode (`writeJSON` has already written a 200 by then, so the client gets a
//! truncated body), while `serde_json` short-circuits both to `null` before the
//! formatter is ever consulted — which is why there is no guard here to catch
//! it. Nothing read out of SQLite can be either one, since SQLite stores a NaN
//! as NULL. A *computed* statistic can be, though, so a ported average or ratio
//! guards its own division rather than leaning on the encoder to notice.

use std::io;

use serde::Serialize;
use serde_json::ser::{Formatter, Serializer};
use serde_json::value::RawValue;

/// Encode exactly as the Go server's `writeJSON` does, newline included.
pub fn to_vec<T>(value: &T) -> Result<Vec<u8>, serde_json::Error>
where
    T: ?Sized + Serialize,
{
    let mut buf = Vec::with_capacity(1024);
    let mut ser = Serializer::with_formatter(&mut buf, GoFormatter);
    value.serialize(&mut ser)?;
    // json.Encoder.Encode terminates every value with '\n'.
    buf.push(b'\n');
    Ok(buf)
}

/// Encode as Go's `json.Marshal` does: same escaping and float spelling, but
/// no trailing newline.
///
/// `writeJSON` uses an Encoder and so terminates with one; values marshalled
/// *into* something else — the sessions list's keyset cursor, a JSON column —
/// use `Marshal` and must not.
pub fn to_vec_marshal<T>(value: &T) -> Result<Vec<u8>, serde_json::Error>
where
    T: ?Sized + Serialize,
{
    let mut buf = Vec::with_capacity(256);
    let mut ser = Serializer::with_formatter(&mut buf, GoFormatter);
    value.serialize(&mut ser)?;
    Ok(buf)
}

/// Render a float as `strconv.FormatFloat(f, 'g', -1, 64)`.
///
/// This is NOT the spelling `encoding/json` uses, and the difference is not
/// cosmetic: `'g'` switches to exponent form at 1e6 rather than 1e21 and pads
/// the exponent to two digits without trimming, so 1000000 is `1e+06` here and
/// `1000000` in JSON. The sessions list's cursor is built with it, and a cursor
/// is compared against the one the other implementation minted.
pub fn format_g(value: f64) -> String {
    if !value.is_finite() {
        return format!("{value}");
    }
    let scientific = format!("{value:e}");
    let (mantissa, exp) = match scientific.split_once('e') {
        Some(parts) => parts,
        None => return scientific,
    };
    let exp10: i32 = exp.parse().unwrap_or(0);

    // Go's ftoa with the shortest representation fixes eprec at 6, so exponent
    // form is used when the decimal exponent is below -4 or at least 6.
    if !(-4..6).contains(&exp10) {
        let sign = if exp10 < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp10.abs());
    }
    format!("{value}")
}

/// The parts of Go's encoder that `serde_json` spells differently.
struct GoFormatter;

impl Formatter for GoFormatter {
    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(go_float(value, format!("{value}"), format!("{value:e}")).as_bytes())
    }

    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        // Go picks the shortest representation that round-trips at the value's
        // own bit size, so a float32 is formatted from its float32 digits.
        writer
            .write_all(go_float(value as f64, format!("{value}"), format!("{value:e}")).as_bytes())
    }

    /// Runs on every stretch of string content `serde_json` decided needs no
    /// escape — which is where Go's extra HTML escaping has to be applied.
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        let mut start = 0;
        for (i, ch) in fragment.char_indices() {
            let escape = match ch {
                '<' => "\\u003c",
                '>' => "\\u003e",
                '&' => "\\u0026",
                // Valid JSON but not valid JavaScript: Go escapes both so its
                // output can be embedded in a <script> block.
                '\u{2028}' => "\\u2028",
                '\u{2029}' => "\\u2029",
                _ => continue,
            };
            writer.write_all(&fragment.as_bytes()[start..i])?;
            writer.write_all(escape.as_bytes())?;
            start = i + ch.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[start..])
    }

    // `write_char_escape` is deliberately NOT overridden. Go and `serde_json`
    // agree on every escape they both emit — the short forms for quote,
    // backslash, `\b`, `\f`, `\n`, `\r`, `\t`, and lowercase `\u00xx` for the
    // remaining control bytes — and neither escapes `/`. The parity vectors
    // cover all of them, so a future divergence in either language fails a test
    // rather than silently shipping.
}

/// Render a float the way `encoding/json` does.
///
/// Go formats with `'f'` (no exponent, shortest round-trip) unless the
/// magnitude is outside `[1e-6, 1e21)`, where it switches to `'e'` and then
/// trims a single leading zero out of the exponent (`1e-07` → `1e-7`). Rust's
/// `Display` and `LowerExp` already produce the shortest round-trip digits; the
/// only spelling difference left is the exponent's sign, which Go always emits.
///
/// The two renderings are passed in rather than computed here so the same
/// arithmetic serves `f32` and `f64` without a generic.
fn go_float(value: f64, fixed: String, exponential: String) -> String {
    let abs = value.abs();
    if abs == 0.0 || (1e-6..1e21).contains(&abs) {
        return fixed;
    }
    match exponential.split_once('e') {
        // A negative exponent is already minimal, which is what Go's trim
        // leaves behind.
        Some((mantissa, exp)) if exp.starts_with('-') => format!("{mantissa}e{exp}"),
        Some((mantissa, exp)) => format!("{mantissa}e+{exp}"),
        None => exponential,
    }
}

// ─── `encoding/json`'s compact, for values carried verbatim ──────────────────
//
// A `json.RawMessage` is re-encoded by `Marshal` through `compact`, which
// strips whitespace outside strings and HTML-escapes, and **changes nothing
// else** — key order and number spelling survive. Any port that carries a
// stored JSON value through to the wire needs this, so it lives beside the
// encoder rather than in whichever module happened to need it first (chat
// message blocks did; session-detail `tool_use` inputs do too).

/// Decode a stored JSON string array the way `storage.decodeStringList` does:
/// blank or unparseable is **nil** (`null` on the wire), a stored `[]` is an
/// empty slice (`[]`).
///
/// The distinction is stored, not cosmetic — `user_settings.hidden_projects` is
/// `null` on an install that has never saved and `[]` on one that has — so the
/// rule lives in one place rather than in each module that meets a such a
/// column.
pub fn decode_string_list(raw: &str) -> Option<Vec<String>> {
    if raw.trim().is_empty() {
        return None;
    }
    match serde_json::from_str::<Option<Vec<String>>>(raw) {
        Ok(values) => values,
        Err(e) => {
            log::warn!("native: malformed stored string array {raw:?}: {e}");
            None
        }
    }
}

/// Decode a field the way Go does: a JSON `null` is the **zero value**, not a
/// type error.
///
/// `json.Unmarshal` treats `null` as a no-op for every type Agento decodes, so
/// `{"parentUuid":null}` leaves `""` and returns no error. `serde` rejects it,
/// and the consequences are out of proportion to the cause: a rejected field
/// fails its whole struct, a failed struct drops its whole event, and a dropped
/// event is simply absent from a transcript with nothing to signal it. That is
/// how the first user message of every conversation went missing from
/// `GET /api/claude-sessions/{id}` in #271 — `parentUuid` is `null` on the
/// event that starts one.
///
/// Genuinely unparseable input still fails, which is also what Go does.
pub fn null_is_zero_value<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    use serde::Deserialize;
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A JSON array decoded Go's way: a `null` **element** is the element type's
/// zero value (#295).
///
/// [`null_is_zero_value`] covers the field itself and stops there, because
/// `serde` only consults it where it is attached — so `{"ids":null}` was
/// already `None` while `{"ids":[null]}` was a type error, and Go answers
/// `[""]` to the second with no error at all. On a *write* route that
/// difference is a 400 for a request Go applies.
///
/// # Why this is a type and not a `deserialize_with` function
///
/// It was a function first, and that was a **regression**. `serde`'s derive
/// makes a field carrying `deserialize_with` *required* — the `missing_field`
/// path that lets a bare `Option` default to `None` is not generated — so every
/// call site had to add `#[serde(default)]`. That attribute also feeds the
/// derive's `visit_seq` arm, which errors with `invalid length` only for fields
/// that have **no** default: adding it turned `{"capabilities":[]}` and
/// `{"capabilities":{"mcp":{"s":[]}}}` from the 400 Go answers into a created
/// agent. Widening an over-accept is the one direction this port must not move
/// in, and the fix for a null shipped inside it.
///
/// A type carries the rule instead. `Option<GoList<T>>` needs no attribute at
/// all: missing is `None`, `null` is `None`, and the struct stays as strict
/// about its own shape as it was before. See
/// `a_container_default_would_have_widened_the_struct_from_array_over_accept`.
///
/// Serialization is unchanged — a newtype struct serializes as its inner value
/// — so no response byte moves. Genuinely unparseable elements (`{"ids":[1]}`)
/// still fail, which is also what Go does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GoList<T>(pub Vec<T>);

impl<'de, T> serde::Deserialize<'de> for GoList<T>
where
    T: serde::Deserialize<'de> + Default,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self(
            Vec::<Option<T>>::deserialize(deserializer)?
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect(),
        ))
    }
}

impl<T> std::ops::Deref for GoList<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> From<Vec<T>> for GoList<T> {
    fn from(values: Vec<T>) -> Self {
        Self(values)
    }
}

/// The same rule for a `null` **value** of a JSON object (#295).
///
/// `{"mcp":{"s":null}}` is `{"s":{"tools":null}}` to Go — the zero struct, no
/// error — and a type error to `serde`. Exactly [`GoList`] with a map in place
/// of the array, and it sits beside it because a struct carrying both shapes is
/// only faithful when both are covered.
///
/// `BTreeMap<String, V>` rather than a generic map: Go marshals map keys sorted,
/// so every map on this wire is a `BTreeMap` already, and generalising over the
/// key and container would cost inference at every call site for no caller.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GoMap<V>(pub std::collections::BTreeMap<String, V>);

impl<'de, V> serde::Deserialize<'de> for GoMap<V>
where
    V: serde::Deserialize<'de> + Default,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self(
            std::collections::BTreeMap::<String, Option<V>>::deserialize(deserializer)?
                .into_iter()
                .map(|(key, value)| (key, value.unwrap_or_default()))
                .collect(),
        ))
    }
}

impl<V> std::ops::Deref for GoMap<V> {
    type Target = std::collections::BTreeMap<String, V>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<V> From<std::collections::BTreeMap<String, V>> for GoMap<V> {
    fn from(entries: std::collections::BTreeMap<String, V>) -> Self {
        Self(entries)
    }
}

/// A nested struct that decodes from a JSON **object only**, as Go's does
/// (#337).
///
/// # The shape this closes
///
/// `serde` builds a struct from a JSON *array*, positionally, and `encoding/json`
/// answers `cannot unmarshal array into Go value of type …`.
/// [`super::writes::decode_body`] guards that at the **body** level — #274 added
/// the object check because `POST /api/agents` with `["My Agent"]` would
/// otherwise have created an agent — but nothing checked a value *inside* the
/// body:
///
/// ```json
/// {"capabilities":[["Read"],null,null]}
/// {"capabilities":{"mcp":{"g":[null]}}}
/// ```
///
/// Both were accepted here and are 400 to Go. The accepted set was not even
/// uniform, which is what made it hard to reason about: the derive's `visit_seq`
/// errors only when the array runs out of elements for a field that has **no**
/// default, so what a struct accepted was exactly "as many elements as it has
/// fields without a default" — three for `Capabilities`, one for
/// `McpCapability`, two for `ServiceConfig`, and *zero* for the notification
/// structs, whose every field carries `#[serde(default)]`.
///
/// **This is the direction that matters.** Every other decode divergence in the
/// port has been an over-*reject* — a request Go applies is refused, which is
/// visible and safe: it answers an error rather than writing a bad row.
/// This one is an over-*accept*: it writes a row Go refuses, so the two
/// implementations' databases diverge with nothing to report it, and the
/// nothing errors, so nothing reports it.
///
/// # Why a type, and not a `deserialize_with`
///
/// The same lesson [`GoList`] carries, and #336 proved it again by trying the
/// other way for #295: `serde`'s derive makes a field carrying
/// `deserialize_with` **required**, so every call site must add
/// `#[serde(default)]` — and that attribute is exactly what opens the
/// `visit_seq` arm. A fix applied to the field would widen the hole it was
/// closing. A type needs no attribute at all.
///
/// # How it works, and what it costs
///
/// `deserialize_map` is the whole mechanism: `serde_json` answers it with
/// `invalid type: sequence` for anything that is not `{`, and the visitor then
/// hands the `MapAccess` to `T`'s own derived impl, so the inner struct's
/// strictness, field names and `deserialize_with` rules are untouched.
///
/// Serialization is a newtype and therefore transparent, so **no response byte
/// moves** — the wrapper is on request *and* response structs (`Capabilities` is
/// both) and only the decode side can tell.
///
/// `null` and "missing" still work, because both are decided one level out:
/// `Option<GoStruct<T>>` gets `None` from `visit_none` and from `missing_field`
/// respectively, and `GoMap<GoStruct<T>>` maps a `null` **value** to the zero
/// struct, which is #295's rule unchanged.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GoStruct<T>(pub T);

impl<'de, T> serde::Deserialize<'de> for GoStruct<T>
where
    T: serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ObjectOnly<T>(std::marker::PhantomData<T>);

        impl<'de, T> serde::de::Visitor<'de> for ObjectOnly<T>
        where
            T: serde::Deserialize<'de>,
        {
            type Value = GoStruct<T>;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(GoStruct)
            }
        }

        deserializer.deserialize_map(ObjectOnly(std::marker::PhantomData))
    }
}

impl<T> std::ops::Deref for GoStruct<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for GoStruct<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<T> for GoStruct<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

/// Deserialize a value as-is, including an explicit `null`.
///
/// `Option<Box<RawValue>>`'s own impl turns `null` into `None`, which would drop
/// a key Go emits: `omitempty` on a `json.RawMessage` tests the byte length, and
/// the four bytes of `null` are not empty.
pub fn captured_raw<'de, D>(deserializer: D) -> Result<Option<Box<RawValue>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    Box::<RawValue>::deserialize(deserializer).map(Some)
}

/// Re-encode a captured raw JSON value the way Go's `compact` does.
///
/// Marshalling a `json.RawMessage` runs `encoding/json`'s `compact` with HTML
/// escaping on, which:
///
/// - drops whitespace *outside* strings,
/// - escapes `<`, `>`, `&` and U+2028/U+2029 wherever they appear,
/// - and **changes nothing else** — object keys keep the order they were stored
///   in and numbers keep the digits they were stored with.
///
/// Those last two are why this is a byte pass rather than a decode/re-encode: a
/// stored `{"z":1.50,"a":[1,2]}` stays `{"z":1.50,"a":[1,2]}`, while a
/// `serde_json::Value` round trip would give `{"a":[1,2],"z":1.5}` — reordered
/// and respelled, with nothing to signal it.
///
/// **Both implementations write compacted bytes** — Go because `chatService`
/// marshals through a `json.RawMessage`, and this port since #298, which is what
/// that issue fixed. Applying it on read as well is what makes a hand-edited or
/// pre-#298 row match too; compaction is idempotent, so the two are not in
/// tension.
pub fn compact_raw(raw: Box<RawValue>) -> Box<RawValue> {
    let compacted = compact(raw.get());
    if compacted == raw.get() {
        return raw;
    }
    // Compacting valid JSON leaves valid JSON, so the fallback is unreachable —
    // and keeping the original is the harmless direction if it ever is not.
    RawValue::from_string(compacted).unwrap_or(raw)
}

/// Serialize an **embedded** raw value the way `encoding/json` emits one.
///
/// `encoding/json` runs `compact(…, escapeHTML=true)` over a `Marshaler`'s
/// output, so a `json.RawMessage` nested inside a marshalled value is
/// whitespace-stripped **and** has `<`, `>`, `&` and U+2028/9 escaped — while
/// keeping its key order and number spelling. `serde_json` writes a `RawValue`'s
/// bytes as-is through `write_raw_fragment`, which [`GoFormatter`] never sees,
/// so an unescaped `&` ships where Go ships `\u0026`.
///
/// Attached to the field rather than applied at each construction site, so a
/// future one cannot forget it: the type says how it is emitted.
pub fn serialize_compacted<S>(raw: &RawValue, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::Error;
    let compacted = compact(raw.get());
    // The no-op case is now the *only* case in practice — every construction
    // site already compacts — so short-circuiting is what keeps this from
    // costing an allocation and a full re-parse per `tool_use` input on every
    // serialize, including a long transcript's worth on
    // `GET /api/claude-sessions/{id}`. `compact_raw` has the same guard.
    if compacted == raw.get() {
        return raw.serialize(serializer);
    }
    let compacted =
        RawValue::from_string(compacted).map_err(|e| S::Error::custom(e.to_string()))?;
    compacted.serialize(serializer)
}

/// [`serialize_compacted`] for an optional field.
///
/// Only ever reached with `Some`, since every user pairs it with
/// `skip_serializing_if = "Option::is_none"` — but `None` is written as `null`
/// rather than unwrapped, so the two attributes stay independent.
pub fn serialize_compacted_option<S>(
    raw: &Option<Box<RawValue>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match raw {
        Some(raw) => serialize_compacted(raw, serializer),
        None => serializer.serialize_none(),
    }
}

pub fn compact(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;

    while i < bytes.len() {
        let byte = bytes[i];

        // U+2028 (E2 80 A8) and U+2029 (E2 80 A9): valid JSON, invalid
        // JavaScript, so Go escapes both.
        if byte == 0xE2
            && i + 2 < bytes.len()
            && bytes[i + 1] == 0x80
            && (bytes[i + 2] & !1) == 0xA8
        {
            out.extend_from_slice(if bytes[i + 2] == 0xA8 {
                b"\\u2028"
            } else {
                b"\\u2029"
            });
            i += 3;
            continue;
        }

        match byte {
            b'<' => out.extend_from_slice(b"\\u003c"),
            b'>' => out.extend_from_slice(b"\\u003e"),
            b'&' => out.extend_from_slice(b"\\u0026"),
            b' ' | b'\t' | b'\n' | b'\r' if !in_string => {}
            _ => out.push(byte),
        }

        // Track string context so whitespace *inside* a string survives. The
        // three escaped bytes above can never change it.
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        }

        i += 1;
    }

    // Unreachable: every byte dropped or inserted above is ASCII, and the one
    // multi-byte sequence handled is replaced whole, so a valid `&str` in stays
    // valid UTF-8 out. Returning the input uncompacted is the harmless
    // direction if that ever stops being true.
    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

// ─── `encoding/json`'s Indent, for the files Go writes rather than serves ────

/// `json.Indent` with an empty prefix, applied to compact Go-encoded JSON.
///
/// `json.MarshalIndent` is literally `Marshal` followed by `Indent`, so a port
/// that already has [`to_vec_marshal`] gets the indented form by running this
/// over its output rather than by growing a second `Formatter`. That is the
/// faithful decomposition, and it is the one that keeps float spelling and HTML
/// escaping in exactly one place.
///
/// The two rules that are easy to guess wrong, both taken from
/// `encoding/json/indent.go`:
///
/// - **`:` becomes `": "`** — a colon *and a space*. Go adds spacing around
///   punctuation on the way out, not while encoding.
/// - **An empty object or array stays `{}` / `[]`.** The indent after an opening
///   brace is *delayed* until a value actually follows, which is the whole point
///   of the `need_indent` flag; writing the newline eagerly and trimming it back
///   produces `{\n}` for `{}`.
///
/// This is Agento's settings files, not a response: the Claude settings profile
/// surface writes `settings.json`, `settings_<slug>.json` and
/// `settings_profiles.json` with `MarshalIndent` so a person can read them.
///
/// # Precondition: the input is already compact
///
/// Named `indent_compact` because it is not `json.Indent`, which re-indents
/// *any* well-formed JSON by discarding the whitespace it finds. This one only
/// inserts, so pre-existing whitespace between tokens survives into the output
/// and the result is wrong. That is not a limitation worth removing: the only
/// callers hand it [`to_vec_marshal`]'s output, which is compact by
/// construction, and `MarshalIndent` is defined as exactly that pair.
pub fn indent_compact(src: &[u8]) -> Vec<u8> {
    debug_assert!(
        !has_whitespace_between_tokens(src),
        "indent_compact needs Marshal's compact output; \
         whitespace between tokens survives into the result"
    );
    indent_compact_inner(src)
}

/// Whether `src` carries whitespace outside a string — the precondition
/// [`indent_compact`] asserts. Debug builds only, so the scan is free in
/// release.
#[cfg(debug_assertions)]
fn has_whitespace_between_tokens(src: &[u8]) -> bool {
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
            b' ' | b'\t' | b'\n' | b'\r' => return true,
            _ => {}
        }
    }
    false
}

#[cfg(not(debug_assertions))]
fn has_whitespace_between_tokens(_src: &[u8]) -> bool {
    false
}

fn indent_compact_inner(src: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(src.len() * 2);
    let mut depth = 0usize;
    let mut need_indent = false;
    let mut in_string = false;
    let mut escaped = false;

    let newline = |out: &mut Vec<u8>, depth: usize| {
        out.push(b'\n');
        for _ in 0..depth {
            out.extend_from_slice(b"  ");
        }
    };

    for &byte in src {
        // Punctuation inside a string is content, so the whole switch below is
        // skipped for it — the same reason [`compact`] tracks string context.
        if in_string {
            out.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if byte == b'"' {
            if need_indent {
                need_indent = false;
                depth += 1;
                newline(&mut out, depth);
            }
            in_string = true;
            out.push(byte);
            continue;
        }

        if need_indent && byte != b'}' && byte != b']' {
            need_indent = false;
            depth += 1;
            newline(&mut out, depth);
        }

        match byte {
            b'{' | b'[' => {
                need_indent = true;
                out.push(byte);
            }
            b',' => {
                out.push(byte);
                newline(&mut out, depth);
            }
            b':' => out.extend_from_slice(b": "),
            b'}' | b']' => {
                if need_indent {
                    // An empty object or array: suppress the delayed indent.
                    need_indent = false;
                } else {
                    depth = depth.saturating_sub(1);
                    newline(&mut out, depth);
                }
                out.push(byte);
            }
            _ => out.push(byte),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    /// Cross-language vectors, shared with `desktop/parity/gojson_parity_test.go`.
    /// Each entry is a value and the exact bytes Go's encoder produces for it.
    #[derive(serde::Deserialize)]
    struct Vectors {
        floats: Vec<FloatVector>,
        strings: Vec<StringVector>,
        cursor_floats: Vec<FloatVector>,
    }

    #[derive(serde::Deserialize)]
    struct FloatVector {
        value: f64,
        want: String,
    }

    #[derive(serde::Deserialize)]
    struct StringVector {
        value: String,
        want: String,
    }

    fn vectors() -> Vectors {
        let raw = include_str!("../../../parity/gojson_vectors.json");
        serde_json::from_str(raw).expect("parity vectors parse")
    }

    /// Every vector is a whole `writeJSON` response, so it carries the
    /// encoder's newline; the vector file records the value alone.
    fn encoded(value: &impl Serialize) -> String {
        let bytes = to_vec(value).expect("encode");
        let text = String::from_utf8(bytes).expect("utf-8");
        text.strip_suffix('\n')
            .expect("trailing newline")
            .to_string()
    }

    #[test]
    fn floats_match_go() {
        for v in vectors().floats {
            assert_eq!(encoded(&v.value), v.want, "float {}", v.value);
        }
    }

    #[test]
    fn strings_match_go() {
        for v in vectors().strings {
            assert_eq!(encoded(&v.value), v.want, "string {:?}", v.value);
        }
    }

    /// `strconv.FormatFloat(_, 'g', -1, 64)`, which the sessions list's cursor
    /// uses and which is deliberately not the JSON spelling: exponent form
    /// starts at 1e6 rather than 1e21, and the exponent is padded to two digits
    /// rather than trimmed.
    #[test]
    fn cursor_floats_match_gos_g_format() {
        for v in vectors().cursor_floats {
            assert_eq!(format_g(v.value), v.want, "format_g({})", v.value);
        }
    }

    #[test]
    fn the_two_float_spellings_really_do_differ() {
        // Guards the pair above from being "fixed" into one function.
        assert_eq!(encoded(&1_000_000.0_f64), "1000000");
        assert_eq!(format_g(1_000_000.0), "1e+06");
    }

    #[test]
    fn encode_appends_the_encoders_newline() {
        assert_eq!(to_vec(&1_i64).expect("encode"), b"1\n");
    }

    #[test]
    fn struct_fields_keep_declaration_order_and_omitempty() {
        #[derive(Serialize)]
        struct Row {
            id: i64,
            rate: f64,
            label: Option<String>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            tiers: Vec<i64>,
        }

        let got = encoded(&Row {
            id: 7,
            rate: 3.0,
            label: None,
            tiers: vec![],
        });
        assert_eq!(got, r#"{"id":7,"rate":3,"label":null}"#);
    }

    /// Pins the one known divergence from Go, so it stays a documented choice
    /// rather than a surprise the next ported endpoint discovers.
    #[test]
    fn non_finite_floats_become_null_where_go_would_fail_the_encode() {
        assert_eq!(encoded(&f64::NAN), "null");
        assert_eq!(encoded(&f64::INFINITY), "null");
    }
    #[test]
    fn compact_leaves_already_compact_json_untouched() {
        let compact_json = r#"{"a":1,"b":[true,null],"s":"x y"}"#;
        assert_eq!(compact(compact_json), compact_json);
    }

    /// Whitespace *inside* a string is content, not formatting.
    #[test]
    fn compact_keeps_whitespace_inside_strings() {
        assert_eq!(
            compact("{ \"k\" : \"a b\\tc\\n d\" }"),
            "{\"k\":\"a b\\tc\\n d\"}"
        );
    }

    /// A quote closes a string unless it is itself escaped — get that wrong and
    /// every space after the first `\"` is stripped out of the payload.
    #[test]
    fn compact_tracks_escaped_quotes() {
        assert_eq!(compact(r#"{"k":"a \" b"}"#), r#"{"k":"a \" b"}"#);
        assert_eq!(compact(r#"{ "k" : "a \\" }"#), r#"{"k":"a \\"}"#);
    }

    #[test]
    fn compact_escapes_the_characters_go_escapes() {
        assert_eq!(compact(r#"{"k":"<&>"}"#), r#"{"k":"\u003c\u0026\u003e"}"#);
        assert_eq!(
            compact("{\"k\":\"a\u{2028}b\u{2029}c\"}"),
            r#"{"k":"a\u2028b\u2029c"}"#
        );
    }

    /// Multi-byte UTF-8 has to survive a byte-wise pass intact.
    #[test]
    fn compact_preserves_multibyte_content() {
        assert_eq!(
            compact(r#"{ "k" : "ünïcödé 😀" }"#),
            r#"{"k":"ünïcödé 😀"}"#
        );
    }

    // ─── #295: a `null` inside a container ───────────────────────────────────

    #[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
    #[serde(default)]
    struct Listy {
        ids: Option<GoList<String>>,
    }

    /// Deliberately **without** `#[serde(default)]` — `Default` is derived
    /// because `GoMap` needs it, which is a different thing. These two stand for
    /// the structs #295 must not loosen: `Capabilities` and `McpCapability`.
    #[derive(Debug, Default, serde::Deserialize)]
    struct Inner {
        ids: Option<GoList<String>>,
    }

    #[derive(Debug, serde::Deserialize)]
    struct Strict {
        mcp: Option<GoMap<Inner>>,
        other: Option<String>,
    }

    /// Every answer here was measured against `encoding/json` rather than
    /// reasoned about — the nil-versus-empty rows in particular, which is the
    /// distinction the outer `Option` exists to keep.
    #[test]
    fn a_null_array_element_is_the_elements_zero_value() {
        let ids = |body: &str| {
            serde_json::from_str::<Listy>(body)
                .unwrap_or_else(|e| panic!("{body}: {e}"))
                .ids
                .map(|list| list.0)
        };

        // The case #295 is about: Go answers [""] with no error.
        assert_eq!(ids(r#"{"ids":[null]}"#), Some(vec![String::new()]));
        assert_eq!(
            ids(r#"{"ids":["a",null,"b"]}"#),
            Some(vec!["a".into(), String::new(), "b".into()])
        );

        // The nil-versus-empty distinction the outer `Option` carries, unmoved.
        assert_eq!(ids(r#"{"ids":null}"#), None);
        assert_eq!(ids(r#"{}"#), None);
        assert_eq!(ids(r#"{"ids":[]}"#), Some(Vec::new()));
        assert_eq!(ids(r#"{"ids":["a"]}"#), Some(vec!["a".into()]));
    }

    /// A `null` map value is the value type's zero, which for a struct is every
    /// field at *its* zero — so the nesting has to survive.
    #[test]
    fn a_null_map_value_is_the_values_zero_value() {
        let parsed: Strict =
            serde_json::from_str(r#"{"mcp":{"s":null},"other":null}"#).expect("null value");
        let mcp = parsed.mcp.expect("a map");
        assert_eq!(mcp.len(), 1);
        assert!(mcp["s"].ids.is_none());

        // And the two rules compose: a null element inside a null-able map
        // value.
        let parsed: Strict = serde_json::from_str(r#"{"mcp":{"s":{"ids":[null]}},"other":null}"#)
            .expect("nested null");
        assert_eq!(
            parsed.mcp.expect("a map")["s"].ids.as_deref(),
            Some(&vec![String::new()])
        );
    }

    /// A field of either type needs **no** `#[serde(default)]`, which is the
    /// whole reason they are types. `Strict` has no container attribute and
    /// still accepts a missing field.
    #[test]
    fn a_missing_field_needs_no_default_attribute() {
        let parsed: Strict = serde_json::from_str(r#"{}"#).expect("both fields missing");
        assert!(parsed.mcp.is_none());
        assert!(parsed.other.is_none());
    }

    /// The regression these types exist to avoid, pinned so it cannot come
    /// back.
    ///
    /// The first version of #295 used `deserialize_with` functions, which make
    /// a field **required** — so every call site had to add
    /// `#[serde(default)]`. That attribute also feeds the derive's `visit_seq`
    /// arm, which rejects a short array only for fields with no default: it
    /// turned `{"capabilities":[]}` from the 400 Go answers into a created
    /// agent. Widening an over-accept is the one direction this port must not
    /// move in, and it would have shipped inside a fix for a `null`.
    #[test]
    fn a_container_default_would_have_widened_the_struct_from_array_over_accept() {
        // Without the attribute — what `Capabilities` looks like now — an array
        // is still refused, exactly as Go refuses it.
        assert!(serde_json::from_str::<Strict>(r#"[]"#).is_err());
        assert!(serde_json::from_str::<Strict>(r#"{"mcp":{"s":[]}}"#).is_err());

        // With it, both are accepted. `Listy` carries `#[serde(default)]` for
        // its own reasons (`{}` is a legal bulk-delete body), which is why the
        // helpers were free there and not here.
        assert!(serde_json::from_str::<Listy>(r#"[]"#).is_ok());
    }

    /// A full-length array used to be **accepted**, by serde's positional
    /// `visit_seq`, where Go refuses it. That was the last shape #295 left
    /// standing, and #337 closed it with [`GoStruct`].
    ///
    /// Inverted rather than deleted: the assertion is the boundary between the
    /// two changes, and `Strict` still stands for `Capabilities` — so the wrap
    /// has to be applied *by the holder*, exactly as `AgentRequest` applies it.
    /// `Strict` itself, unwrapped, is still built from a sequence, which is what
    /// the second half asserts and what makes "a field cannot protect itself"
    /// concrete.
    #[test]
    fn a_full_length_positional_array_is_refused_once_the_holder_wraps_it() {
        let body = r#"[{"s":{"ids":["x"]}},"y"]"#;

        let wrapped = serde_json::from_str::<GoStruct<Strict>>(body);
        assert!(
            wrapped.is_err(),
            "a JSON array must not build a struct (#337)"
        );

        // Unwrapped, serde still does it. This is not a wish — it is why the
        // rule lives on the holder rather than on the struct.
        let parsed: Strict =
            serde_json::from_str(body).expect("serde still builds it positionally");
        assert_eq!(parsed.other.as_deref(), Some("y"));
    }

    /// Everything [`GoStruct`] must leave alone, which is most of its behaviour.
    #[test]
    fn a_wrapped_struct_keeps_every_rule_but_the_array_one() {
        #[derive(Debug, Default, serde::Deserialize)]
        struct Holder {
            inner: Option<GoStruct<Inner>>,
            map: Option<GoMap<GoStruct<Inner>>>,
        }

        // An object decodes, and the inner struct's own rules still apply —
        // including #295's null element.
        let parsed: Holder = serde_json::from_str(r#"{"inner":{"ids":[null]}}"#).expect("object");
        assert_eq!(
            parsed.inner.expect("inner").ids.as_deref(),
            Some(&vec![String::new()])
        );

        // Missing and explicit `null` are decided one level out, by `Option`,
        // so neither reaches the visitor.
        let parsed: Holder = serde_json::from_str(r#"{}"#).expect("missing");
        assert!(parsed.inner.is_none());
        let parsed: Holder = serde_json::from_str(r#"{"inner":null}"#).expect("null");
        assert!(parsed.inner.is_none());

        // A `null` map **value** is still the zero struct (#295) — the wrapper
        // sits inside `GoMap`, which resolves the null before it is reached.
        let parsed: Holder = serde_json::from_str(r#"{"map":{"s":null}}"#).expect("null value");
        assert!(parsed.map.expect("map")["s"].ids.is_none());

        // And the shape it exists for, at both depths.
        assert!(serde_json::from_str::<Holder>(r#"{"inner":[null]}"#).is_err());
        assert!(serde_json::from_str::<Holder>(r#"{"map":{"s":[null]}}"#).is_err());

        // A scalar is refused too, as it always was and as Go refuses it.
        assert!(serde_json::from_str::<Holder>(r#"{"inner":"x"}"#).is_err());
        assert!(serde_json::from_str::<Holder>(r#"{"inner":3}"#).is_err());
    }

    /// The wrapper is a **decode-time** rule: it is a newtype, so it serializes
    /// as its inner value and no response byte moves. `Capabilities` is on both
    /// sides of the wire, so this is load-bearing rather than incidental.
    #[test]
    fn a_wrapped_struct_serializes_as_its_inner_value() {
        #[derive(serde::Serialize)]
        struct Out {
            inner: GoStruct<Listy>,
        }

        assert_eq!(
            String::from_utf8(
                to_vec_marshal(&Out {
                    inner: GoStruct(Listy {
                        ids: Some(GoList(vec!["a".to_string()])),
                    }),
                })
                .expect("encode")
            )
            .expect("utf-8"),
            r#"{"inner":{"ids":["a"]}}"#
        );
    }

    /// The half that must *not* change: `null` is a zero value, but a wrong
    /// **type** is still an error — to Go as much as to serde. Without this the
    /// types would read as "accept anything".
    #[test]
    fn a_wrongly_typed_element_still_fails() {
        assert!(serde_json::from_str::<Listy>(r#"{"ids":[1]}"#).is_err());
        assert!(serde_json::from_str::<Listy>(r#"{"ids":[{}]}"#).is_err());
        assert!(serde_json::from_str::<Listy>(r#"{"ids":"a"}"#).is_err());
        assert!(serde_json::from_str::<Strict>(r#"{"mcp":{"s":1}}"#).is_err());
    }

    /// Serialization is untouched: a newtype struct serializes as its inner
    /// value, so no response byte moves.
    #[test]
    fn the_types_serialize_as_their_inner_value() {
        assert_eq!(
            String::from_utf8(to_vec_marshal(&GoList(vec!["a", "b"])).expect("encode"))
                .expect("utf-8"),
            r#"["a","b"]"#
        );
        let mut map = std::collections::BTreeMap::new();
        map.insert("k".to_string(), 1u8);
        assert_eq!(
            String::from_utf8(to_vec_marshal(&GoMap(map)).expect("encode")).expect("utf-8"),
            r#"{"k":1}"#
        );
    }
}
