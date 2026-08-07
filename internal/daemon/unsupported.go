package daemon

import (
	"context"
	"fmt"
	"runtime"
)

// unsupported is the Manager returned on platforms without an implementation.
type unsupported struct {
	goos string
}

// NewUnsupported returns a Manager whose every method fails with
// ErrUnsupportedOS — used by tests and as documentation of the fallback.
func NewUnsupported() Manager {
	return &unsupported{goos: runtime.GOOS}
}

func (u *unsupported) err() error {
	return fmt.Errorf("%w: %s", ErrUnsupportedOS, u.goos)
}

// Install always fails with ErrUnsupportedOS.
func (u *unsupported) Install(context.Context, Options) error { return u.err() }

// Uninstall always fails with ErrUnsupportedOS.
func (u *unsupported) Uninstall(context.Context) error { return u.err() }

// Start always fails with ErrUnsupportedOS.
func (u *unsupported) Start(context.Context) error { return u.err() }

// Stop always fails with ErrUnsupportedOS.
func (u *unsupported) Stop(context.Context) error { return u.err() }

// Restart always fails with ErrUnsupportedOS.
func (u *unsupported) Restart(context.Context) error { return u.err() }

// Status always fails with ErrUnsupportedOS.
func (u *unsupported) Status(context.Context) (Status, error) { return Status{}, u.err() }
