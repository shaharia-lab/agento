package build

import "fmt"

// These variables are set at build time via -ldflags.
var (
	Version   = "dev"
	CommitSHA = "unknown"
	BuildDate = "unknown"
)

// String returns a single human-readable build info string.
func String() string {
	return fmt.Sprintf("%s (commit %s, built %s)", Version, CommitSHA, BuildDate)
}
