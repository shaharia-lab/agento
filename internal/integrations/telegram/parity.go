package telegram

// This file is the seam `desktop/parity/telegram_parity_test.go` builds its
// cross-language vectors through, and it exists for exactly one reason: the
// vectors have to come from the **real** server, not from a restatement of it.
//
// `desktop/parity` is a different package, so it cannot reach `apiBaseURL` to
// stand a server up against a local fake. The alternative — a generator that
// rebuilt the tool set from this package's source — would freeze someone's
// reading of the code rather than the code, and the whole point of the vectors is
// that a change here fails the Rust port in
// `desktop/src-tauri/src/native/integrations/telegram/`.
//
// #312 and #315 did the same for GitHub and Slack, which have the same shape: one
// API root in a package variable. #316 and #317 needed no seam, because an
// Atlassian site URL is per row and a test can simply store one.
//
// Nothing below changes behavior or wording: `Start` remains the only way the app
// builds this server.

// SetAPIBase points every outgoing Telegram request at base and returns a
// function that restores the previous value.
//
// Do not call outside tests. It is a primitive for pointing every Telegram
// request in the process at an arbitrary host — and Telegram puts the bot token
// in the **URL path**, so a caller anywhere in the running server would hand the
// credential to that host as part of the request line, not merely leak a header.
// It is exported only because `desktop/parity` is a different package; the Rust
// port gates the same seam behind `#[cfg(test)]`, so it does not exist in a
// shipped desktop binary at all.
//
// Not safe for concurrent use with a live integration — which is fine, because
// the only callers are tests that own the process.
func SetAPIBase(base string) (restore func()) {
	previous := apiBaseURL
	apiBaseURL = base
	return func() { apiBaseURL = previous }
}
