package build

import "testing"

func TestIsDevBuild(t *testing.T) {
	tests := []struct {
		name    string
		version string
		want    bool
	}{
		// The case this exists for: `git describe --tags --always --dirty` on a
		// working tree. Valid semver, so a parse-only check lets it through.
		{"dirty tree ahead of tag", "v0.8.0-21-gc325de6-dirty", true},
		{"dirty tree on the tag", "v0.8.0-dirty", true},
		{"clean tree ahead of tag", "v0.8.0-21-gc325de6", true},
		{"clean tree ahead, long sha", "v1.2.3-4-g0123456789abcdef0123456789abcdef01234567", true},
		{"no leading v", "0.8.0-21-gc325de6-dirty", true},

		{"unstamped default", "dev", true},
		{"unknown", "unknown", true},
		{"empty", "", true},
		{"whitespace only", "  ", true},

		// Published releases must keep checking for updates.
		{"release tag", "v0.8.0", false},
		{"release tag without v", "0.8.0", false},
		// A prerelease tag is published — its users should still be offered
		// updates, so it must not be mistaken for a working-tree build.
		{"release candidate", "v1.0.0-rc.1", false},
		{"beta tag", "v1.0.0-beta.2", false},
		// A bare SHA is rejected here too, though the semver parse also catches it.
		{"bare sha", "c325de6", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := IsDevBuild(tt.version); got != tt.want {
				t.Errorf("IsDevBuild(%q) = %v, want %v", tt.version, got, tt.want)
			}
		})
	}
}
