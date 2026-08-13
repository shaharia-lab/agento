//! Go's `time.Time` on the wire.
//!
//! `time.Time` marshals as RFC 3339 with a *variable-length* fraction:
//! `time.RFC3339Nano` drops trailing zeros, and drops the decimal point with
//! them when the fraction is zero. Nothing in `chrono` spells exactly that —
//! `SecondsFormat::AutoSi` rounds up to 0, 3, 6 or 9 digits — so it is written
//! out here.
//!
//! Rates are normalized to second precision on every write path, so today the
//! fraction is always absent. It is handled anyway because the *next* ported
//! endpoint reads timestamps that Go wrote from live clocks, and finding this
//! divergence again there would cost the same day twice.

use chrono::{DateTime, FixedOffset, Utc};
use serde::{Serialize, Serializer};

/// A timestamp that serializes exactly as Go's `time.Time` does.
///
/// The offset is preserved rather than normalized: Go renders the zone the
/// value carries, and every stored rate carries `Z`, so normalizing would be
/// invisible until the one row that does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoTime(pub DateTime<FixedOffset>);

impl GoTime {
    /// Parse the RFC 3339 text SQLite holds, as `time.Parse(time.RFC3339, …)`
    /// does on the Go side.
    pub fn parse(text: &str) -> Result<Self, String> {
        DateTime::parse_from_rfc3339(text)
            .map(GoTime)
            .map_err(|e| format!("unparsable effective_from {text:?}: {e}"))
    }

    /// The instant, for ordering and for "is this rate in force yet".
    pub fn instant(&self) -> DateTime<Utc> {
        self.0.with_timezone(&Utc)
    }

    /// `t.UTC().Format(time.RFC3339Nano)` — the form the catalog revision hash
    /// mixes in, which is UTC regardless of the stored offset.
    pub fn rfc3339_nano_utc(&self) -> String {
        format_go_rfc3339(&self.instant().fixed_offset())
    }
}

/// Go's `time.Time.String()` layout, which is what every DATETIME column in
/// the database actually holds.
///
/// The Go server writes timestamps through `database/sql`, and the driver
/// stores a `time.Time` by its `String()` rendering —
/// `2026-06-14 18:25:20.492 +0000 UTC` — not RFC 3339. Two consequences a
/// reader must respect: the fraction is variable-length (trailing zeros are
/// already trimmed), and the ordering the sessions list pages on is *lexical
/// over this text*, which coincides with chronological order only because
/// every value is UTC.
const GO_STRING_LAYOUT: &str = "%Y-%m-%d %H:%M:%S%.f %z";

impl GoTime {
    /// Parse the `time.Time.String()` text a DATETIME column holds.
    ///
    /// The trailing zone abbreviation (`UTC`, `CEST`) is dropped rather than
    /// interpreted: Go prints it for humans and it is redundant beside the
    /// numeric offset, which is what actually fixes the instant.
    pub fn parse_go_string(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        // Split off the abbreviation: everything from the third space on.
        let numeric = match trimmed.match_indices(' ').nth(2) {
            Some((idx, _)) => &trimmed[..idx],
            None => trimmed,
        };
        DateTime::parse_from_str(numeric, GO_STRING_LAYOUT)
            .map(GoTime)
            .map_err(|e| format!("unparsable Go timestamp {text:?}: {e}"))
    }

    /// Parse either form: the `time.Time.String()` text the DATETIME columns
    /// hold, or RFC 3339 as the pricing catalog stores it.
    pub fn parse_any(text: &str) -> Result<Self, String> {
        Self::parse(text).or_else(|_| Self::parse_go_string(text))
    }

    /// The wire rendering, for a value that is not being serialized directly.
    pub fn to_rfc3339_nano(self) -> String {
        format_go_rfc3339(&self.0)
    }
}

/// Render a timestamp as Go's `time.Time.String()` in UTC — the exact text the
/// database driver writes into a DATETIME column, and therefore the only shape
/// a bound parameter may take.
///
/// The columns are compared **as text**: both the sessions list's `ORDER BY`
/// and every time predicate depend on lexical order matching chronological
/// order, which holds only because every stored value is UTC with its trailing
/// fractional zeros trimmed. A bound formatted any other way — RFC 3339, a
/// fixed nine-digit fraction — sorts into a different place and silently
/// mis-filters rather than failing.
pub fn to_go_string_utc(t: GoTime) -> String {
    format_go_string(&t.instant())
}

/// The same rendering for an epoch-milliseconds value, which is how the
/// analytics drill-down encodes its windows.
pub fn go_string_from_millis(ms: i64) -> String {
    match DateTime::from_timestamp_millis(ms) {
        Some(t) => format_go_string(&t),
        // Out of range for a timestamp. An empty bound matches nothing, which
        // is the safe direction: a filter that silently matched everything
        // would present an unfiltered list as a filtered one.
        None => String::new(),
    }
}

fn format_go_string(t: &DateTime<Utc>) -> String {
    let mut out = t.format("%Y-%m-%d %H:%M:%S").to_string();
    let nanos = t.timestamp_subsec_nanos();
    if nanos > 0 {
        let fraction = format!("{nanos:09}");
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }
    out.push_str(" +0000 UTC");
    out
}

impl Serialize for GoTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format_go_rfc3339(&self.0))
    }
}

/// Render a timestamp as Go's `time.RFC3339Nano` layout:
/// `2006-01-02T15:04:05.999999999Z07:00`, where the `9`s mean "trailing zeros
/// removed, and the point with them".
fn format_go_rfc3339(t: &DateTime<FixedOffset>) -> String {
    let mut out = t.format("%Y-%m-%dT%H:%M:%S").to_string();

    let nanos = t.timestamp_subsec_nanos();
    if nanos > 0 {
        let fraction = format!("{nanos:09}");
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }

    let offset = t.offset().local_minus_utc();
    if offset == 0 {
        out.push('Z');
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        let abs = offset.abs();
        out.push_str(&format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(text: &str) -> String {
        let t = GoTime::parse(text).expect("parse");
        String::from_utf8(crate::native::gojson::to_vec(&t).expect("encode"))
            .expect("utf-8")
            .trim_end()
            .to_string()
    }

    /// The DATETIME columns hold `time.Time.String()`, and the vectors record
    /// what Go gets when it parses that text back and renders it for the wire.
    #[test]
    fn stored_timestamps_parse_and_render_as_go_does() {
        #[derive(serde::Deserialize)]
        struct Vectors {
            go_times: Vec<Vector>,
        }
        #[derive(serde::Deserialize)]
        struct Vector {
            value: String,
            want: String,
        }

        let raw = include_str!("../../../parity/gojson_vectors.json");
        let vectors: Vectors = serde_json::from_str(raw).expect("parity vectors parse");
        assert!(!vectors.go_times.is_empty(), "no timestamp vectors");

        for v in vectors.go_times {
            let parsed = GoTime::parse_go_string(&v.value).expect(&v.value);
            assert_eq!(parsed.to_rfc3339_nano(), v.want, "parsing {:?}", v.value);
        }
    }

    /// Round trip: the text a bound parameter carries has to be the text the
    /// column already holds, or a range filter compares two different shapes.
    #[test]
    fn rendering_a_stored_timestamp_reproduces_it_exactly() {
        for text in [
            "2026-06-14 18:25:20.492 +0000 UTC",
            "2020-01-01 00:00:00 +0000 UTC",
            "2026-08-13 21:51:41.123456789 +0000 UTC",
        ] {
            let parsed = GoTime::parse_go_string(text).expect(text);
            assert_eq!(to_go_string_utc(parsed), text);
        }
    }

    #[test]
    fn utc_renders_with_z_as_go_does() {
        assert_eq!(encoded("2020-01-01T00:00:00Z"), "\"2020-01-01T00:00:00Z\"");
    }

    #[test]
    fn an_offset_is_preserved_not_normalized() {
        assert_eq!(
            encoded("2026-08-13T10:30:00+02:00"),
            "\"2026-08-13T10:30:00+02:00\""
        );
    }

    #[test]
    fn trailing_zeros_leave_the_fraction_the_way_go_leaves_it() {
        assert_eq!(
            encoded("2026-08-13T10:30:00.500000000Z"),
            "\"2026-08-13T10:30:00.5Z\""
        );
        assert_eq!(
            encoded("2026-08-13T10:30:00.123456789Z"),
            "\"2026-08-13T10:30:00.123456789Z\""
        );
        assert_eq!(
            encoded("2026-08-13T10:30:00.000000000Z"),
            "\"2026-08-13T10:30:00Z\""
        );
    }

    #[test]
    fn the_hash_form_is_utc_whatever_the_stored_offset() {
        let t = GoTime::parse("2026-08-13T10:30:00+02:00").expect("parse");
        assert_eq!(t.rfc3339_nano_utc(), "2026-08-13T08:30:00Z");
    }
}
