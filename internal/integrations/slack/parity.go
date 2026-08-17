package slack

// This file is the seam `desktop/parity/slack_parity_test.go` builds its
// cross-language vectors through, and it exists for exactly one reason: the
// vectors have to come from the **real** server, not from a restatement of it.
//
// `desktop/parity` is a different package, so it cannot reach `slackAPIBase` to
// stand a server up against a local fake. The alternative — a generator that
// rebuilt the tool set from this package's source — would freeze someone's
// reading of the code rather than the code, and the whole point of the vectors
// is that a change here fails the Rust port in
// `desktop/src-tauri/src/native/integrations/slack/`.
//
// #312 did the same for GitHub, which has the same shape: one API root in a
// package variable. #316 and #317 needed no seam of their own, because an
// Atlassian site URL is per row and a test can simply store one — the difference
// is where the base lives, not how careful the test is being.
//
// Nothing below changes behavior or wording: `Start` remains the only way the
// app builds this server.

// SetAPIBase points every outgoing Slack request at base and returns a function
// that restores the previous value.
//
// Do not call outside tests. What it is is a primitive for pointing every Slack
// request in the process — each one bearing the workspace's bot or user token —
// at an arbitrary host, so a caller anywhere in the running server would be a
// credential-exfiltration seam rather than a misconfiguration. It is exported
// only because `desktop/parity` is a different package; the Rust port gates the
// same seam behind `#[cfg(test)]`, so it does not exist in a shipped desktop
// binary at all.
//
// The variable it writes is the same one this package's own tests redirect. Not
// safe for concurrent use with a live integration — which is fine, because the
// only callers are tests that own the process.
func SetAPIBase(base string) (restore func()) {
	previous := slackAPIBase
	slackAPIBase = base
	return func() { slackAPIBase = previous }
}
