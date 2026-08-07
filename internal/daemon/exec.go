package daemon

import (
	"bytes"
	"context"
	"fmt"
	"os/exec"
	"strings"
)

// commandRunner abstracts shelling out to launchctl/systemctl/loginctl so
// unit tests can assert command sequencing without touching a real init
// system. args must never be assembled through a shell — each element is
// passed verbatim to exec.CommandContext.
type commandRunner interface {
	Run(ctx context.Context, name string, args ...string) (stdout string, err error)
}

// osRunner is the production commandRunner backed by os/exec.
type osRunner struct{}

// Run executes name with args and returns trimmed stdout. On failure the
// error carries combined output for diagnosis. The nolint is for gosec G204:
// callers only pass fixed platform tools (launchctl/systemctl/loginctl).
func (r *osRunner) Run(ctx context.Context, name string, args ...string) (string, error) {
	cmd := exec.CommandContext(ctx, name, args...) //nolint:gosec
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		detail := strings.TrimSpace(stderr.String())
		if detail == "" {
			detail = err.Error()
		}
		return stdout.String(), fmt.Errorf("%s %s: %s", name, strings.Join(args, " "), detail)
	}
	return strings.TrimSpace(stdout.String()), nil
}
