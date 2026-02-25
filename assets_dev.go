//go:build dev

package main

import "io/fs"

// getFrontendFS returns nil in dev mode, signalling the server to proxy to Vite on :5173.
func getFrontendFS() (fs.FS, error) {
	return nil, nil
}
