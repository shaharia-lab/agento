//! `agent.Interpolate` — the `{{name}}` substitution Agento applies to an
//! agent's system prompt and to a scheduled task's prompt.
//!
//! Ported here rather than left inline because the two callers disagree about
//! what a *failure* means, and that difference is the whole reason it is a
//! `Result`:
//!
//! - the scheduler calls it on `task.Prompt` and a missing variable is a
//!   **recorded failed run** — `prepareTaskRun` writes a `job_history` row with
//!   `prompt interpolation: …` and publishes the failed event, so the user sees
//!   the typo instead of a task that quietly does the wrong thing;
//! - [`crate::native::chat::runner`] calls it on the system prompt and today
//!   swallows the error, which is a pre-existing divergence from Go rather than
//!   a decision (see the note at that call site).
//!
//! One substitution implementation with two policies around it, rather than two
//! implementations — the substitution is the part that must not drift.

/// `agent.MissingVariableError`, formatted as its `Error()` formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingVariable(pub String);

impl std::fmt::Display for MissingVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `fmt.Sprintf("missing required template variable: %q", …)`.
        write!(f, "missing required template variable: {:?}", self.0)
    }
}

/// `agent.Interpolate(template, nil)`.
///
/// Both callers pass no variables of their own, so only the two built-ins
/// resolve and every other name is an error. Three details of the Go loop are
/// reproduced deliberately, because each is observable:
///
/// - **An unterminated `{{` ends the scan rather than failing.** `"a {{b"` is
///   returned unchanged; only a *closed* placeholder is looked up.
/// - **The scan resumes past the substituted value**, not past the placeholder,
///   so a value that itself contains `{{` is not re-scanned.
/// - **The name is trimmed**, so `{{ current_date }}` resolves.
///
/// Go reads the clock once, at entry, so `{{current_date}}{{current_time}}`
/// cannot straddle midnight.
pub fn interpolate(template: &str) -> Result<String, MissingVariable> {
    // Go's `Format("2006-01-02")` / `Format("15:04:05")` on `time.Now()` —
    // local, not UTC.
    let now = chrono::Local::now();
    let current_date = now.format("%Y-%m-%d").to_string();
    let current_time = now.format("%H:%M:%S").to_string();

    let mut result = template.to_string();
    let mut i = 0usize;
    while let Some(start) = result[i..].find("{{") {
        let start = start + i;
        let Some(end) = result[start..].find("}}") else {
            break;
        };
        let end = end + start;

        let name = result[start + 2..end].trim();
        let value = match name {
            "current_date" => current_date.clone(),
            "current_time" => current_time.clone(),
            other => return Err(MissingVariable(other.to_string())),
        };

        result = format!("{}{}{}", &result[..start], value, &result[end + 2..]);
        i = start + value.len();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_builtins_resolve_and_anything_else_is_the_go_error() {
        let out = interpolate("on {{current_date}} at {{current_time}}").expect("builtins resolve");
        assert!(!out.contains("{{"), "{out}");

        assert_eq!(
            interpolate("hello {{name}}").unwrap_err().to_string(),
            r#"missing required template variable: "name""#
        );
    }

    #[test]
    fn an_unterminated_placeholder_ends_the_scan_rather_than_failing() {
        // Go breaks out of the loop when `}}` is absent, so the text survives
        // verbatim — including a *later* placeholder, which is never reached.
        assert_eq!(interpolate("a {{b").expect("no error"), "a {{b");
        assert_eq!(
            interpolate("a {{b {{current_date").expect("no error"),
            "a {{b {{current_date"
        );
    }

    #[test]
    fn the_name_is_trimmed() {
        let out = interpolate("{{  current_date  }}").expect("trimmed name resolves");
        assert_eq!(out.len(), 10, "a bare YYYY-MM-DD: {out}");
    }

    #[test]
    fn a_template_with_no_placeholder_is_returned_unchanged() {
        assert_eq!(interpolate("plain").expect("no error"), "plain");
        assert_eq!(interpolate("").expect("no error"), "");
    }
}
