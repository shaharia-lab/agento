package updater

import (
	"runtime"
	"testing"
)

func TestIsHomebrewPath(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string
		path string
		want bool
	}{
		// Positive cases — paths that must be detected as Homebrew.
		{"macos-intel-cellar", "/usr/local/Cellar/agento/1.2.3/bin/agento", true},
		{"macos-intel-opt", "/usr/local/opt/agento/bin/agento", true},
		{"macos-silicon-cellar", "/opt/homebrew/Cellar/agento/1.2.3/bin/agento", true},
		{"macos-silicon-opt", "/opt/homebrew/opt/agento/bin/agento", true},
		{"linuxbrew", "/home/linuxbrew/.linuxbrew/Cellar/agento/1.2.3/bin/agento", true},
		{"linuxbrew-bin-symlink", "/home/linuxbrew/.linuxbrew/bin/agento", true},

		// Negative cases — paths that must NOT be detected as Homebrew.
		{"linux-system", "/usr/local/bin/agento", false},
		{"linux-user-bin", "/home/user/bin/agento", false},
		{"linux-go-install", "/home/user/go/bin/agento", false},
		{"macos-non-brew", "/Applications/agento", false},
		{"empty", "", false},
		// Windows-style paths must never match.
		{"windows-program-files", `C:\Program Files\agento\agento.exe`, false},
		{"windows-user-bin", `C:\Users\alice\bin\agento.exe`, false},
		// Tricky: substring in the middle of a path must not match.
		{"substring-not-prefix", "/var/lib/usr/local/Cellar/foo", false},
	}

	for _, tt := range tests {
		tt := tt
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			if got := isHomebrewPath(tt.path); got != tt.want {
				t.Errorf("isHomebrewPath(%q) = %v, want %v", tt.path, got, tt.want)
			}
		})
	}
}

func TestDetectInstallMethod_WindowsAlwaysSelfUpdate(t *testing.T) {
	if runtime.GOOS != "windows" {
		t.Skip("only runs on windows")
	}
	if got := DetectInstallMethod(); got != InstallMethodSelfUpdate {
		t.Errorf("DetectInstallMethod() on windows = %v, want InstallMethodSelfUpdate", got)
	}
}

func TestDetectInstallMethod_NonHomebrewPath(t *testing.T) {
	// On the typical test environment, the test binary lives under a temp
	// directory chosen by `go test`, which is never a Homebrew prefix.
	// The check should report InstallMethodSelfUpdate.
	if runtime.GOOS == "windows" {
		t.Skip("covered by TestDetectInstallMethod_WindowsAlwaysSelfUpdate")
	}
	if got := DetectInstallMethod(); got != InstallMethodSelfUpdate {
		t.Errorf("DetectInstallMethod() in test env = %v, want InstallMethodSelfUpdate", got)
	}
}
