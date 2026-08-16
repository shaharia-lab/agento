package github

// This file is the seam `desktop/parity/github_parity_test.go` builds its
// cross-language vectors through, and it exists for exactly one reason: the
// vectors have to come from the **real** server, not from a restatement of it.
//
// `desktop/parity` is a different package, so it can neither reach
// `githubAPIBase` nor stand a server up against a local fake without help. The
// alternative — a generator that rebuilds the tool set from this package's
// source — would freeze someone's reading of the code rather than the code, and
// the whole point of the vectors is that a change here fails the Rust port in
// `desktop/src-tauri/src/native/integrations/github/`.
//
// #310 did the same thing for the local tools server (`tools.FormatCurrentTime`).
// Nothing below changes behavior or wording: `Start` remains the only way the
// app builds this server.

// SetAPIBase points every outgoing GitHub request at base and returns a
// function that restores the previous value.
//
// The variable it writes is the same one this package's own tests redirect, and
// it is a package global in Go for the same reason it is a `RwLock` static on
// the Rust side: `client` is constructed per registration and the base is not
// per-request state. Not safe for concurrent use with a live integration —
// which is fine, because the only callers are tests that own the process.
func SetAPIBase(base string) (restore func()) {
	previous := githubAPIBase
	githubAPIBase = base
	return func() { githubAPIBase = previous }
}
