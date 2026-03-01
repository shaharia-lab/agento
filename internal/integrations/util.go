package integrations

import (
	"context"
	"fmt"
	"net"
)

// FreePort finds an available TCP port on localhost.
func FreePort() (int, error) {
	lc := &net.ListenConfig{}
	ln, err := lc.Listen(context.Background(), "tcp", "localhost:0")
	if err != nil {
		return 0, fmt.Errorf("finding free port: %w", err)
	}
	port := ln.Addr().(*net.TCPAddr).Port
	if err := ln.Close(); err != nil {
		return 0, fmt.Errorf("closing probe listener: %w", err)
	}
	return port, nil
}
