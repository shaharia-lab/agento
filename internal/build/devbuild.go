package build

import (
	"regexp"
	"strings"
)

// gitDescribeSuffix matches the "-<commits>-g<sha>" tail `git describe` appends
// when HEAD is ahead of the most recent tag.
var gitDescribeSuffix = regexp.MustCompile(`-\d+-g[0-9a-f]{7,40}$`)

// IsDevBuild reports whether version names a local working tree rather than a
// published release.
//
// The Makefile stamps `git describe --tags --always --dirty`, so a developer
// build reads as "v0.8.0-21-gc325de6-dirty". That is *valid* semver — the tail
// parses as a prerelease identifier — and semver ranks a prerelease below the
// release it precedes. So the published v0.8.0 compares as newer than a build
// 21 commits ahead of it, and the update banner offers a "newer" version the
// developer already has. A plain semver parse cannot catch this; only the shape
// of the version string can.
//
// Both markers are checked because they appear independently: "-dirty" alone
// disappears the moment the tree is clean, while a clean tree several commits
// past the tag is still not a release.
//
// A genuine prerelease tag such as "v1.0.0-rc.1" is deliberately NOT a dev
// build — those are published, and their users should be offered updates.
func IsDevBuild(version string) bool {
	v := strings.TrimPrefix(strings.TrimSpace(version), "v")
	if v == "" || v == "dev" || v == "unknown" {
		return true
	}
	if strings.HasSuffix(v, "-dirty") {
		return true
	}
	return gitDescribeSuffix.MatchString(v)
}
