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
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A timestamp that serializes exactly as Go's `time.Time` does.
///
/// The offset is preserved rather than normalized: Go renders the zone the
/// value carries, and every stored rate carries `Z`, so normalizing would be
/// invisible until the one row that does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoTime(pub DateTime<FixedOffset>);

impl GoTime {
    /// Parse the RFC 3339 text SQLite holds.
    ///
    /// This is `chrono`'s RFC 3339, which is **not** `time.Parse(time.RFC3339,
    /// …)` — see [`parse_rfc3339`] for the five ways they disagree. The
    /// difference is unreachable here: the only writer of `effective_from` is
    /// this application, which normalizes to second precision on every write
    /// path, so none of the five shapes can be stored. A reader of text some
    /// *other* program wrote wants [`parse_rfc3339`].
    pub fn parse(text: &str) -> Result<Self, String> {
        DateTime::parse_from_rfc3339(text)
            .map(GoTime)
            .map_err(|e| format!("unparsable effective_from {text:?}: {e}"))
    }

    /// A UTC instant as a `GoTime`, for the write paths that stamp
    /// `time.Now().UTC()` and hand the value straight back to a caller.
    pub fn from_utc(t: DateTime<Utc>) -> Self {
        GoTime(t.fixed_offset())
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

/// `time.Now().UTC()` in the rendering a DATETIME column stores.
///
/// Every Go write path stamps `created_at`/`updated_at` this way, and the
/// columns are compared **as text** — the sessions list orders and pages on
/// them. A row written in any other shape (RFC 3339, a fixed-width fraction)
/// sorts into a different place than the rows around it, which is a silently
/// wrong list rather than an error, so the writes go through here rather than
/// formatting at each call site.
pub fn now_go_text() -> String {
    format_go_string(&Utc::now())
}

/// `time.Now()` — **local**, not UTC — in the rendering a DATETIME column
/// stores.
///
/// Almost every Go write stamps UTC and [`now_go_text`] is the one to reach for.
/// This exists for the handful that do not, and the only one so far is
/// `NotificationHandler.Handle`, which sets `entry.CreatedAt = time.Now()`
/// without the `.UTC()` every neighbouring write has.
///
/// It is reproduced rather than corrected because the column is read back
/// **and ordered on as text**: `ListNotifications` is a bare
/// `ORDER BY created_at DESC`, so rows carrying two different zone suffixes sort
/// by their spelling rather than by their instant. Go is at least self-
/// consistent; a second writer stamping UTC into the same table would not be.
/// The rendering also reaches the wire, since `GET /api/notifications/log`
/// serializes what it read.
///
/// The zone abbreviation is `time.Time.String()`'s — `CEST`, not `+02:00` — and
/// chrono's `%Z` over a `chrono_tz` zone is what produces it. A zone the
/// database cannot name falls back to UTC, which is [`now_go_text`]'s answer.
///
/// **One part of Go's rendering is deliberately not reproduced**, so the claim
/// above is "the same zone", not "the same bytes": `time.Time.String()` appends
/// a monotonic reading — ` m=+0.007413217` — whenever the value carries one, and
/// `time.Now()` does. A Go-written row therefore reads
/// `2026-08-18 02:34:11.615509987 +0200 CEST m=+0.007413217` where this writes
/// everything up to `CEST`. Inventing a monotonic reading this process does not
/// have would be worse than omitting it, and nothing reads the suffix:
/// [`GoTime::parse_go_string`] splits at the third space, modernc's own reader
/// strips at `m=`, and `ORDER BY created_at DESC` sorts on the shared prefix.
pub fn now_go_text_local() -> String {
    let Some(zone) = iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| name.parse::<chrono_tz::Tz>().ok())
    else {
        return now_go_text();
    };
    let t = Utc::now().with_timezone(&zone);
    let mut out = t.format("%Y-%m-%d %H:%M:%S").to_string();
    let nanos = t.timestamp_subsec_nanos();
    if nanos > 0 {
        let fraction = format!("{nanos:09}");
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }
    out.push_str(&t.format(" %z %Z").to_string());
    out
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

/// A `DateTime<Utc>` this process is already holding, in the rendering a
/// DATETIME column stores.
///
/// The third sibling of [`now_go_text`] and [`go_text_at`], added for #425:
/// a gateway usage row stamps one instant and then uses **the same one** to
/// resolve the request's price against the catalog, which takes a
/// `DateTime<Utc>`. Formatting at the call site would work, and would also be
/// the third place this format lives; stamping the clock twice instead would
/// let the stored time and the priced time disagree across a rate change.
pub fn go_text(t: &DateTime<Utc>) -> String {
    format_go_string(t)
}

/// An instant given as seconds since the epoch, in the rendering a DATETIME
/// column stores.
///
/// [`now_go_text`]'s sibling for a timestamp that is *computed* rather than
/// read from the clock — #405's `api_tokens.expires_at`, which comes from the
/// JWT's `exp` claim, so the row and the token cannot disagree about when a
/// credential dies. Going through here rather than formatting at the call site
/// is what keeps it sortable as text alongside every other DATETIME.
pub fn go_text_at(epoch_seconds: i64) -> String {
    match DateTime::<Utc>::from_timestamp(epoch_seconds, 0) {
        Some(t) => format_go_string(&t),
        // Out of `DateTime`'s range, which needs an `exp` some 260,000 years
        // out; `MAX_TOKEN_DAYS` puts it far inside. An empty string reads back
        // as a NULL-ish "no expiry" rather than as a wrong date.
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

/// Go's zero `time.Time`, which marshals as `0001-01-01T00:00:00Z`.
///
/// Reachable on the wire: an aggregate that tracks a maximum timestamp starts
/// from the zero value, and a group with nothing in it keeps it.
impl Default for GoTime {
    fn default() -> Self {
        let naive = chrono::NaiveDate::from_ymd_opt(1, 1, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .expect("year 1 is representable");
        GoTime(naive.and_utc().fixed_offset())
    }
}

impl Serialize for GoTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format_go_rfc3339(&self.0))
    }
}

/// The inverse of [`Serialize`], for the request bodies that carry an instant.
///
/// `time.Time.UnmarshalJSON` reads a JSON string with the RFC 3339 layout, so
/// this parses through [`parse_rfc3339`] — Go's own grammar — rather than
/// `chrono::DateTime::parse_from_rfc3339`, which disagrees with it in five
/// ways and would refuse three shapes `encoding/json` accepts. A value that
/// does not parse is a decode error, which `writes::decode_body` renders as
/// the same 400 every other mistyped field answers.
///
/// The offset is preserved, exactly as [`GoTime::parse`] preserves it; a write
/// path that stores through `to_go_string_utc` is what normalizes to UTC, and
/// that is the storage layer's business rather than the decoder's.
impl<'de> Deserialize<'de> for GoTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse_rfc3339(&text)
            .map(GoTime)
            .ok_or_else(|| serde::de::Error::custom(format!("parsing time {text:?}")))
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

/// Go's zero `time.Time`, the instant `IsZero()` tests for.
///
/// It matters because `omitempty` does **not** suppress a struct: `json.Marshal`
/// of an `oauth2.Token` with no expiry emits `"expiry":"0001-01-01T00:00:00Z"`
/// rather than omitting the key, and `Token.Valid()` then treats that token as
/// **never expiring**. Read as an ordinary instant it is permanently in the
/// past, which inverts the meaning.
pub const ZERO: chrono::NaiveDateTime = chrono::NaiveDateTime::new(
    match chrono::NaiveDate::from_ymd_opt(1, 1, 1) {
        Some(date) => date,
        None => unreachable!(),
    },
    match chrono::NaiveTime::from_hms_opt(0, 0, 0) {
        Some(time) => time,
        None => unreachable!(),
    },
);

/// `time.Parse(time.RFC3339, s)`, transcribed — for text Go wrote or that Go
/// will judge.
///
/// [`GoTime::parse`] is `chrono`'s RFC 3339 and is fine for values this
/// application wrote. This is for the two places that read somebody else's:
/// `native/schedule`'s `one_off` `run_at`, which is free-form client input
/// (#275), and `native/integrations/registry`'s Google `auth` expiry, which Go's
/// `oauth2` wrote (#313).
///
/// It exists because **`chrono::DateTime::parse_from_rfc3339` disagrees with Go
/// in five ways**, in both directions. The first three were found by a
/// differential run of 636 shapes against a real `gocron.Scheduler` for #275;
/// the last two by review of #313, from a Go program run against the pinned
/// toolchain:
///
/// | value | Go | `parse_from_rfc3339` |
/// |---|---|---|
/// | `2026-06-01t12:00:00z` | error | accepted |
/// | `2026-06-01T12:00:00,5Z` | accepted (12:00:00.5) | error |
/// | `2026-06-30T23:59:60Z` | `second out of range` | accepted |
/// | `2026-06-01T5:04:05Z` | accepted | error |
/// | `2026-06-01T12:00:00+24:00` | accepted | error |
///
/// The last two are Go being *laxer*, which a stricter port turns into refusing
/// input Go accepts — a schedule that will not build, or an integration that
/// will not host. They exist because `parseStrictRFC3339`
/// (`time/format_rfc3339.go`) has its extra checks compiled out behind
/// `case true:`, so what a stored value actually meets is the **general**
/// parser's laxity: `stdHour` takes one or two digits, `parseNanoseconds`
/// accepts a comma, and the zone offset's hour is range-unchecked. If Go
/// re-enables those checks — the source carries a TODO to do it behind a
/// GODEBUG — this becomes stricter than it needs to be, which is the safe
/// direction.
///
/// #275's version was `parse_from_rfc3339` plus three guards, and #313 was about
/// to add a second copy with two. Guarding a convenient library has now been
/// wrong twice for one reason: the guard list has to be right about *every*
/// disagreement, and enumerating a third party's edge cases is the thing that
/// keeps turning out incomplete. So this parses the grammar and delegates
/// nothing but the calendar arithmetic.
///
/// What the general parser does not allow, and neither does this: a year that is
/// not exactly four digits, a one-digit minute or second, a missing zone, and
/// anything trailing.
pub fn parse_rfc3339(s: &str) -> Option<DateTime<FixedOffset>> {
    let bytes = s.as_bytes();
    let digits = |from: usize, len: usize| -> Option<u32> {
        let slice = s.get(from..from + len)?;
        slice.bytes().all(|b| b.is_ascii_digit()).then_some(())?;
        slice.parse::<u32>().ok()
    };
    let literal =
        |at: usize, want: u8| -> Option<()> { (bytes.get(at) == Some(&want)).then_some(()) };

    // `2006` — exactly four digits, the first of which must be one, so no sign.
    let year = digits(0, 4)?;
    literal(4, b'-')?;
    let month = digits(5, 2)?;
    literal(7, b'-')?;
    let day = digits(8, 2)?;
    // `T` is a literal in the layout, so its case is fixed — not `t`, not a
    // space.
    literal(10, b'T')?;

    // `stdHour` is `getnum(value, false)`: one **or** two digits, unlike the
    // minute and second, which are fixed-width.
    let (hour, mut at) = match digits(11, 2) {
        Some(two) if bytes.get(13) == Some(&b':') => (two, 13),
        _ => (digits(11, 1)?, 12),
    };
    literal(at, b':')?;
    let minute = digits(at + 1, 2)?;
    literal(at + 3, b':')?;
    let second = digits(at + 4, 2)?;
    at += 6;

    // `if len(value) >= 2 && commaOrPeriod(value[0]) && isDigit(value, 1)` — a
    // comma is as good as a period, which is the form chrono refuses.
    let mut nanos = 0u32;
    if matches!(bytes.get(at), Some(b'.' | b',')) {
        let fraction_at = at + 1;
        let mut end = fraction_at;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == fraction_at {
            return None;
        }
        // Scaled to nanoseconds, dropping anything past the ninth digit as
        // `parseNanoseconds` drops it.
        for index in 0..9 {
            nanos *= 10;
            if let Some(digit) = bytes
                .get(fraction_at + index)
                .filter(|b| b.is_ascii_digit())
            {
                nanos += u32::from(digit - b'0');
            }
        }
        at = end;
    }

    // `Z07:00` — a literal `Z`, or a signed `hh:mm`.
    let offset_seconds: i32 = match bytes.get(at) {
        Some(b'Z') => {
            at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let negative = *sign == b'-';
            let zone_hour = i32::try_from(digits(at + 1, 2)?).ok()?;
            literal(at + 3, b':')?;
            let zone_minute = i32::try_from(digits(at + 4, 2)?).ok()?;
            at += 6;
            // **No range check on the hour**: the general parser has none, so
            // `+24:00` is a legal offset to Go.
            let magnitude = (zone_hour * 60 + zone_minute) * 60;
            if negative {
                -magnitude
            } else {
                magnitude
            }
        }
        _ => return None,
    };
    // "extra text" — anything left over fails.
    if at != bytes.len() {
        return None;
    }

    // The range checks `Parse` applies once it has the fields. `from_ymd_opt` is
    // the day-in-month check; `and_hms_nano_opt` rejects hour 24, minute 60 and
    // **second 60**, which is Go's `second out of range` for a leap second.
    let naive = chrono::NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?
        .and_hms_nano_opt(hour, minute, second, nanos)?;
    // chrono's offset type is bounded at ±24h exclusive, so an offset Go accepts
    // but chrono cannot represent is applied by hand and carried as UTC. The
    // instant is identical; only the printed zone differs, and no caller prints
    // one.
    match FixedOffset::east_opt(offset_seconds) {
        Some(offset) => naive.and_local_timezone(offset).single(),
        None => naive
            .checked_sub_signed(chrono::TimeDelta::seconds(offset_seconds.into()))?
            .and_local_timezone(FixedOffset::east_opt(0)?)
            .single(),
    }
}

/// Read a DATETIME column as the `time.Time` the Go driver round-trips.
///
/// One definition rather than one per module: it decides that an unparseable
/// timestamp **fails the row** rather than defaulting, which is what Go's
/// driver does, and four copies of that decision would drift the first time one
/// of them was softened. `index` is the column, so the error names it.
pub fn from_sql_text(text: &str, index: usize) -> rusqlite::Result<GoTime> {
    GoTime::parse_any(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(e)),
        )
    })
}

#[cfg(test)]
mod rfc3339_tests {
    use super::*;
    use chrono::Timelike;

    /// The five disagreements [`parse_rfc3339`] exists for, each measured
    /// against the pinned Go toolchain rather than reasoned about.
    #[test]
    fn the_five_places_chrono_disagrees_with_go() {
        // Go rejects, chrono accepts.
        assert!(parse_rfc3339("2026-06-01t12:00:00z").is_none());
        assert!(parse_rfc3339("2026-06-01T12:00:00z").is_none());
        assert!(parse_rfc3339("2026-06-01t12:00:00Z").is_none());
        assert!(
            parse_rfc3339("2026-06-30T23:59:60Z").is_none(),
            "a leap second is `second out of range` to Go"
        );

        // Go accepts, chrono rejects. These are the direction that turns into
        // refusing input Go takes.
        let comma = parse_rfc3339("2026-06-01T12:00:00,5Z").expect("Go accepts a comma");
        assert_eq!(comma.nanosecond(), 500_000_000);
        assert_eq!(
            comma,
            parse_rfc3339("2026-06-01T12:00:00.5Z").expect("and a period")
        );

        let short_hour = parse_rfc3339("2026-06-01T5:04:05Z").expect("a one-digit hour");
        assert_eq!(short_hour.hour(), 5);

        let far_offset =
            parse_rfc3339("2026-06-01T12:00:00+24:00").expect("the offset hour is unbounded");
        assert_eq!(
            far_offset.to_utc(),
            parse_rfc3339("2026-05-31T12:00:00Z")
                .expect("the same instant")
                .to_utc(),
            "an offset chrono cannot represent is still the right instant"
        );
    }

    /// Everything both agree on, including the shapes that must be refused.
    #[test]
    fn the_ordinary_grammar_is_unchanged() {
        assert!(parse_rfc3339("2026-06-01T12:00:00Z").is_some());
        assert!(parse_rfc3339("2026-06-01T12:00:00+02:00").is_some());
        assert!(parse_rfc3339("2026-06-01T12:00:00-05:30").is_some());
        assert!(parse_rfc3339("2026-06-01T12:00:00.123456789Z").is_some());
        // Digits past the ninth are dropped, not refused.
        assert_eq!(
            parse_rfc3339("2026-06-01T12:00:00.1234567891Z")
                .expect("ten digits")
                .nanosecond(),
            123_456_789
        );

        for refused in [
            "",
            "2026-06-01",
            "2026-06-01 12:00:00Z",
            "2026-06-01T12:00:00",
            "2026-06-01T12:00:00Z ",
            "2026-13-01T12:00:00Z",
            "2026-06-31T12:00:00Z",
            "2026-06-01T24:00:00Z",
            "2026-06-01T12:60:00Z",
            "2026-06-01T12:00:00.Z",
            // A one-digit minute or second is fixed-width in the layout.
            "2026-06-01T12:0:00Z",
            "2026-06-01T12:00:0Z",
            // The year is exactly four digits and unsigned.
            "226-06-01T12:00:00Z",
            "+2026-06-01T12:00:00Z",
            "12026-06-01T12:00:00Z",
        ] {
            assert!(
                parse_rfc3339(refused).is_none(),
                "{refused:?} must be refused"
            );
        }
    }

    /// The sentinel Go's own writer emits for a zero `time.Time`.
    #[test]
    fn the_zero_time_is_recognisable() {
        let zero = parse_rfc3339("0001-01-01T00:00:00Z").expect("a valid instant");
        assert_eq!(zero.naive_utc(), ZERO);
        assert_ne!(
            parse_rfc3339("0001-01-01T00:00:00+05:30")
                .expect("valid")
                .naive_utc(),
            ZERO,
            "an offset makes it a different instant, and not Go's zero"
        );
    }
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
