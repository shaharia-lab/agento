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
}
