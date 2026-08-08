// Package daemon installs and manages Agento as a user-level background
// service — launchd on macOS, systemd user units on Linux — so the web server
// survives logout, reboot, and crashes without a terminal staying open.
package daemon

import (
	"context"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

// ErrUnsupportedOS is returned by every Manager method on platforms without a
// service-manager implementation (e.g. Windows).
var ErrUnsupportedOS = errors.New("agento service is not supported on this operating system")

// ErrAlreadyRunning is returned by Install when something already answers on
// the configured port — usually a foreground `agento web`, which would make
// the freshly installed service crash-loop on the same port.
var ErrAlreadyRunning = errors.New("something is already listening on the configured port")

// portInUse is overridable in tests; production performs a real TCP dial.
var portInUse = probePort //nolint:gochecknoglobals

// probePort reports whether a TCP connection to localhost:port can be
// established — i.e. some process is already listening there.
func probePort(port int) bool {
	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()
	dialer := &net.Dialer{}
	conn, err := dialer.DialContext(ctx, "tcp", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)))
	if err != nil {
		return false
	}
	_ = conn.Close() //nolint:errcheck // probe only — close errors are irrelevant
	return true
}

// checkPortFree refuses installation when the service's port already answers.
func checkPortFree(port int) error {
	if portInUse(port) {
		return fmt.Errorf(
			"%w: port %d — stop the foreground `agento web` (or pass a different PORT) before installing the service",
			ErrAlreadyRunning, port)
	}
	return nil
}

// Options carries everything a platform manager needs to render and install
// the unit/plist.
type Options struct {
	// BinaryPath is the absolute, symlink-resolved path to the agento binary.
	BinaryPath string
	// DataDir is the resolved AGENTO_DATA_DIR baked into the unit environment.
	DataDir string
	// LogPath is where the service's stdout/stderr are redirected.
	LogPath string
	// Port is the HTTP port the service listens on.
	Port int
	// ExtraPath is the invoking shell's PATH, baked in so the service's
	// minimal environment can still find the Claude Code CLI and node.
	ExtraPath string
}

// Status describes the installed/enabled/running state of the service. It
// carries only what the platform manager itself knows; presentation fields
// like the URL and log path are derived by the caller.
type Status struct {
	Installed bool
	Enabled   bool
	Running   bool
	// PID is the service process id when Running, 0 otherwise.
	PID int
	// UnitPath is the plist/unit file location (empty when not installed).
	UnitPath string
}

// Manager installs, removes, and controls the OS-level background service.
type Manager interface {
	Install(ctx context.Context, opts Options) error
	Uninstall(ctx context.Context) error
	Start(ctx context.Context) error
	Stop(ctx context.Context) error
	Restart(ctx context.Context) error
	Status(ctx context.Context) (Status, error)
}

// New returns the Manager for the current platform, or an error wrapping
// ErrUnsupportedOS when no implementation exists (e.g. Windows).
func New(_ *config.AppConfig) (Manager, error) {
	runner := &osRunner{}
	switch runtime.GOOS {
	case "darwin":
		return newLaunchd(runner), nil
	case "linux":
		return newSystemd(runner), nil
	default:
		return nil, fmt.Errorf("%w: %s", ErrUnsupportedOS, runtime.GOOS)
	}
}

// NewForTest builds a Manager for an explicit platform with an injected
// command runner and home directory. It exists for unit tests; production
// code should use New.
func NewForTest(goos string, runner commandRunner, homeDir string) (Manager, error) {
	switch goos {
	case "darwin":
		return newLaunchdForHome(runner, homeDir), nil
	case "linux":
		return newSystemdForHome(runner, homeDir), nil
	default:
		return nil, fmt.Errorf("%w: %s", ErrUnsupportedOS, goos)
	}
}

// DefaultOptions builds install Options from the running binary and config.
// The binary path is resolved through symlinks so the unit survives a symlink
// swap, and paths that live under temp/build caches (e.g. `go run`) are
// rejected because they will not exist at the next login.
func DefaultOptions(cfg *config.AppConfig) (Options, error) {
	binary, err := resolveBinaryPath()
	if err != nil {
		return Options{}, err
	}
	return Options{
		BinaryPath: binary,
		DataDir:    cfg.DataDir,
		LogPath:    ServiceLogPath(cfg),
		Port:       cfg.Port,
		ExtraPath:  os.Getenv("PATH"),
	}, nil
}

// ServiceLogPath returns where the service's stdout/stderr are written.
// It is deliberately separate from the lumberjack-managed system.log the app
// writes itself.
func ServiceLogPath(cfg *config.AppConfig) string {
	return filepath.Join(cfg.LogDir(), "service.log")
}

// resolveBinaryPath returns the absolute, symlink-resolved path of the
// running binary, refusing paths that live under temp/build directories.
func resolveBinaryPath() (string, error) {
	exe, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("locating the agento binary: %w", err)
	}
	resolved, err := filepath.EvalSymlinks(exe)
	if err != nil {
		// A missing target means the binary is about to disappear (mid
		// self-update); still, the unresolved path is the better answer.
		resolved = exe
	}
	if isEphemeralPath(resolved) {
		return "", fmt.Errorf(
			"the running binary lives in a temporary build directory (%s) — install a release build first",
			resolved)
	}
	return resolved, nil
}

// isEphemeralPath reports whether path sits under the OS temp dir or a Go
// build cache — both tell-tales of `go run` / `go test` binaries that will
// not exist when the service manager next starts them.
func isEphemeralPath(path string) bool {
	temp := filepath.Clean(os.TempDir())
	if strings.HasPrefix(filepath.Clean(path), temp+string(filepath.Separator)) {
		return true
	}
	if cache, err := os.UserCacheDir(); err == nil {
		if strings.HasPrefix(filepath.Clean(path), filepath.Join(cache, "go-build")+string(filepath.Separator)) {
			return true
		}
	}
	return false
}

// currentUID is a var seam so tests can steer the launchd domain target
// (gui/<uid>) without depending on the test process's real uid. Production
// uses os.Getuid — an inherited UID env var is not trustworthy.
var currentUID = os.Getuid //nolint:gochecknoglobals
