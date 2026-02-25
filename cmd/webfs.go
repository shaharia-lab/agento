package cmd

import "io/fs"

// WebFS is set by main() before Execute() is called.
// It holds the embedded frontend filesystem (nil signals dev proxy mode).
var WebFS fs.FS
