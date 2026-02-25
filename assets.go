//go:build !dev

package main

import (
	"embed"
	"io/fs"
)

//go:embed frontend/dist
var embeddedFrontend embed.FS

func getFrontendFS() (fs.FS, error) {
	return fs.Sub(embeddedFrontend, "frontend/dist")
}
