//! Decode tolerance — the one place Go's `encoding/json` and `serde_json`
//! genuinely disagree, and the disagreement is load-bearing.
//!
//! `encoding/json` populates every field it processed successfully *before*
//! returning an error, so one unexpected field degrades that field and leaves
//! the rest of the message intact. `serde_json` aborts the whole struct on the
//! first mismatch. The Go SDK depends on the former: `parseLine` records the
//! failure in `Event.DecodeErr` and keeps whatever decoded, because a CLI that
//! adds or reshapes a field must not blank an entire message (see #23, and
//! `decode_tolerance_test.go`, which mutates `num_turns` into an array and
//! asserts the rest of the result survives).
//!
//! [`lenient`] is a `deserialize_with` that routes one field through
//! [`serde_json::Value`] and falls back to `Default` when it does not fit —
//! per-field degradation, exactly Go's. Because it never fails, the surrounding
//! `from_slice` never fails either, so the *reason* would be lost; [`lenient`]
//! therefore records the first mismatch it sees in a thread-local slot that
//! [`decode`] reads back. Decoding is synchronous, so no other decode can
//! interleave on the same thread between clearing and reading that slot.
//!
//! Two deliberate exemptions:
//!
//! * **`RawValue` fields skip [`lenient`].** They accept any JSON so they
//!   cannot mismatch, and routing them through `Value` would re-serialize them,
//!   losing the verbatim bytes that are the only reason to keep a raw copy.
//! * **JSON `null` is not a mismatch.** Go unmarshals `null` into a non-pointer
//!   as a no-op and returns no error, so a null field degrades silently here
//!   too; recording it would report drift on every message that omits an
//!   optional field by sending `null`.

use serde::{Deserialize, Deserializer};
use std::cell::RefCell;

thread_local! {
    /// First field-level mismatch seen during the current [`decode`] call.
    static FIRST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Deserializes a field, falling back to `Default` when the JSON does not fit
/// the target type. Never fails, which is what makes the surrounding struct
/// tolerant field by field.
pub(crate) fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    // Deserializing into Value cannot fail for well-formed JSON, so a type
    // mismatch surfaces at from_value and degrades to Default there.
    let value = serde_json::Value::deserialize(deserializer)?;

    // Go treats null as absent, without error. Match that, and do not report it.
    if value.is_null() {
        return Ok(T::default());
    }

    match serde_json::from_value(value) {
        Ok(decoded) => Ok(decoded),
        Err(err) => {
            record_error(err.to_string());
            Ok(T::default())
        }
    }
}

/// Records a field mismatch, keeping the first — Go's `DecodeErr` is whichever
/// error `json.Unmarshal` returned, and it stops at the first.
fn record_error(message: String) {
    FIRST_ERROR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(message);
        }
    });
}

/// Decodes `bytes` into `T`, mirroring Go's partial-population semantics.
///
/// Returns the decoded value plus the first field-level mismatch, if any. The
/// caller stores that message the way Go stores `Event.DecodeErr`: as a record
/// that the typed view is incomplete, alongside a raw copy that always is.
///
/// A payload that is not JSON at all — or whose shape does not fit `T` at the
/// top level, which no `deserialize_with` can rescue — yields `T::default()`
/// and the parse error, the same place Go lands.
pub(crate) fn decode<T>(bytes: &[u8]) -> (T, Option<String>)
where
    T: serde::de::DeserializeOwned + Default,
{
    FIRST_ERROR.with(|slot| *slot.borrow_mut() = None);

    let (value, top_level_error) = match serde_json::from_slice::<T>(bytes) {
        Ok(value) => (value, None),
        Err(err) => (T::default(), Some(err.to_string())),
    };

    let field_error = FIRST_ERROR.with(|slot| slot.borrow_mut().take());
    (value, top_level_error.or(field_error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct Sample {
        #[serde(deserialize_with = "lenient")]
        num_turns: i64,
        #[serde(deserialize_with = "lenient")]
        result: String,
    }

    #[test]
    fn a_bad_field_degrades_only_that_field() {
        // The shape decode_tolerance_test.go mutates: num_turns as an array.
        let (value, err) = decode::<Sample>(br#"{"num_turns":[1,2],"result":"kept"}"#);
        assert_eq!(value.num_turns, 0, "the bad field falls back to its zero");
        assert_eq!(value.result, "kept", "the good field survives alongside it");
        assert!(err.is_some(), "the mismatch is reported, not swallowed");
    }

    #[test]
    fn a_clean_payload_reports_nothing() {
        let (value, err) = decode::<Sample>(br#"{"num_turns":3,"result":"ok"}"#);
        assert_eq!(
            value,
            Sample {
                num_turns: 3,
                result: "ok".into()
            }
        );
        assert!(err.is_none());
    }

    #[test]
    fn null_is_absent_rather_than_a_mismatch() {
        // Go unmarshals null into a string as a no-op and returns no error.
        let (value, err) = decode::<Sample>(br#"{"num_turns":null,"result":null}"#);
        assert_eq!(value, Sample::default());
        assert!(err.is_none(), "null must not be reported as drift");
    }

    #[test]
    fn the_error_slot_does_not_leak_between_decodes() {
        let (_, err) = decode::<Sample>(br#"{"num_turns":[1]}"#);
        assert!(err.is_some());
        let (_, err) = decode::<Sample>(br#"{"num_turns":1}"#);
        assert!(err.is_none(), "a later clean decode must not inherit it");
    }
}
